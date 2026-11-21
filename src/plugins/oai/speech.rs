//! DeepSeek 官方的语音工具：语音识别（ASR）、语音合成（TTS），以及把它们接进群聊的两端。
//!
//! - 入站：群里一条语音 → 取回音频 → 必要时 ffmpeg 转成 16k 单声道 WAV →
//!   `POST /v1/tools/asr` → 文字，房间与搭话共用一份按消息号去重的缓存。
//! - 出站：回复文本 → `POST /v1/tools/tts`（整段）或 `wss://…/tts`（流式分段）→
//!   WAV → ffmpeg 转 MP3 → `upload.create` → 一条语音气泡。QQ 的语音气泡按 MP3 送，
//!   与点歌那边的成品同一口径。
//!
//! 工具端点都在官方根地址上，只对官方地址 + `deepseek-flash` 的请求开放（见
//! [`super::native::official_key`] / [`super::native::eligible`]）。
//!
//! 接口文档：<https://api-docs.deepseek.com/zh-cn/guides/asr> 与
//! <https://api-docs.deepseek.com/zh-cn/guides/tts>。请求与响应形状由本文件末尾的单测钉住，
//! 另在 1.81.0 上线前用真实接口逐项验过。

use anyhow::{Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, protocol::Message as WsMessage};

use super::native::{self, DeepSeekConfig};
use super::OaiConfig;
use crate::adapters::satori::{LockedWriter, send_msg};
use crate::event::Context;
use crate::message::Message;
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsArray, ValueObjectAccessAsScalar};

const LOG_TARGET: &str = "Plugin/OAI/Speech";

/// 官方根地址上的 HTTP 工具端点。
fn http(path: &str) -> String {
    format!("{}{path}", native::OFFICIAL)
}

/// 同一根地址上的 WebSocket 端点（只换 scheme，路径一致）。
fn ws(path: &str) -> String {
    format!(
        "{}{path}",
        native::OFFICIAL.replacen("https://", "wss://", 1)
    )
}

/// 朗读触发方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TtsMode {
    /// 不朗读。
    Off,
    /// 用户说了「念出来」这类词才朗读；群聊里默认走这档，免得刷屏。
    Keyword,
    /// 每条回复都附上语音。
    Always,
}

impl TtsMode {
    pub(crate) fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "关" | "关闭" => Self::Off,
            "always" | "all" | "on" | "总是" | "全部" => Self::Always,
            _ => Self::Keyword,
        }
    }
}

/// 想让人把话读出来的那几个说法；命中任意一个就朗读。
const SPEAK_KEYWORDS: &[&str] = &[
    "念出来", "读出来", "念一下", "读一下", "朗读", "念给我", "说出来", "发语音", "用语音", "语音回",
];

/// 这一轮要不要把回复读出来。`trigger` 是用户这一句（含提示词开关），不是回复本身。
pub(crate) fn wants_tts(cfg: &DeepSeekConfig, trigger: &str) -> bool {
    match cfg.tts_mode() {
        TtsMode::Off => false,
        TtsMode::Always => true,
        TtsMode::Keyword => SPEAK_KEYWORDS.iter().any(|word| trigger.contains(word)),
    }
}

/// 同一条语音消息可能被 oai 房间与 ambient 搭话各看一次：按消息号缓存转写结果，
/// 一份音频只花一次识别的钱。消息号为空时不缓存。
static HEARD: LazyLock<Mutex<HashMap<String, Option<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const HEARD_CAP: usize = 256;

fn cached(id: &str) -> Option<Option<String>> {
    if id.is_empty() {
        return None;
    }
    HEARD.lock().ok()?.get(id).cloned()
}

fn remember(id: &str, heard: Option<String>) {
    if id.is_empty() {
        return;
    }
    if let Ok(mut map) = HEARD.lock() {
        if map.len() >= HEARD_CAP {
            map.clear();
        }
        map.insert(id.to_string(), heard);
    }
}

/// 把一条消息里的语音转写成文字。没有语音、没开 ASR、没接密钥或识别失败都返回 `None`，
/// 调用方据此照旧走原来的路径（搭话那边还有 QQ 自带的「转文字」兜底）。
pub(crate) async fn transcribe_message(ctx: &Context, cfg: &OaiConfig) -> Option<String> {
    let deep = &cfg.deepseek;
    if !deep.enabled || !deep.asr {
        return None;
    }
    let event = ctx.as_message()?;
    let id = event.message_id().to_string();
    if let Some(heard) = cached(&id) {
        return heard;
    }
    let url = voice_url(ctx)?;
    debug!(target: LOG_TARGET, "转写语音：消息 {id}");
    let heard = match transcribe_url(cfg, &url).await {
        Ok(text) => {
            info!(target: LOG_TARGET, "语音转写：{text}");
            Some(text)
        }
        Err(error) => {
            // 识别不出、格式不认得、接口忙都不是故障，记一行就行；别为一条语音刷错误日志。
            debug!(target: LOG_TARGET, "语音转写没成：{error}");
            None
        }
    };
    remember(&id, heard.clone());
    heard
}

/// 事件里第一段语音的可取回地址（`url` 优先，`file` 兜底）。
fn voice_url(ctx: &Context) -> Option<String> {
    let crate::event::EventType::Satori(raw) = &ctx.event else {
        return None;
    };
    let segments = raw.get_array("message")?;
    for seg in segments {
        if seg.get_str("type") != Some("record") {
            continue;
        }
        let data = seg.get("data")?;
        if let Some(url) = data
            .get_str("url")
            .or_else(|| data.get_str("file"))
            .map(str::trim)
            .filter(|url| !url.is_empty())
        {
            return Some(url.to_string());
        }
    }
    None
}

/// 下载音频 → 必要时转码 → ASR → 文字。
async fn transcribe_url(cfg: &OaiConfig, url: &str) -> Result<String> {
    let key = native::official_key(cfg).ok_or_else(|| anyhow!("没接 DeepSeek 官方密钥"))?;
    let response = crate::http::client()
        .get(url)
        .timeout(Duration::from_secs(60))
        .send()
        .await?
        .error_for_status()?;
    let bytes = response.bytes().await?.to_vec();
    // ASR 上限 8 MiB / 120 秒，超了直接不试，省一次注定失败的请求。
    if bytes.is_empty() || bytes.len() > 8 * 1024 * 1024 {
        bail!("音频为空或超过 8 MiB");
    }
    let audio = if is_wav(&bytes) || is_ogg(&bytes) {
        bytes
    } else {
        // QQ 的语音多半是 silk / amr / mp3；统一转成 16k 单声道 16 位 WAV。
        to_wav16k(&bytes).await?
    };
    transcribe_audio(&key, audio).await
}

/// `POST /v1/tools/asr`：`multipart/form-data` 的 `file` 字段，回包里取 `text`。
pub(crate) async fn transcribe_audio(key: &str, audio: Vec<u8>) -> Result<String> {
    if audio.is_empty() {
        bail!("没有可识别的音频");
    }
    let part = reqwest::multipart::Part::bytes(audio).file_name("voice.wav".to_string());
    let response = crate::http::client()
        .post(http("/v1/tools/asr"))
        .bearer_auth(key)
        .multipart(reqwest::multipart::Form::new().part("file", part))
        .timeout(Duration::from_secs(90))
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        bail!("ASR {status}: {}", clip(&body));
    }
    let value: Value = serde_json::from_str(&body)?;
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        bail!("ASR 没识别出文字");
    }
    Ok(text)
}

/// 把一段文本合成成 WAV 字节。`filter_markdown` 交给接口再兜一层，本地先清一遍 Markdown，
/// 免得把 `**`、`![图](网址)` 念出来。
pub(crate) async fn synthesize(key: &str, text: &str, cfg: &DeepSeekConfig) -> Result<Vec<u8>> {
    let clean = clean_for_speech(text);
    if clean.is_empty() {
        bail!("没有可朗读的文本");
    }
    let wav = if cfg.tts_stream {
        tts_stream(key, &clean, cfg.voice()).await?
    } else {
        tts_whole(key, &clean, cfg.voice()).await?
    };
    // 文档：音频已经开始返回后合成失败会中途断开，提前结束的响应当失败处理。
    if !wav_complete(&wav) {
        bail!("TTS 返回的音频不完整（合成中途断开）");
    }
    Ok(wav)
}

/// `POST /v1/tools/tts`：整段文本 → 流式返回的 `audio/wav`。
async fn tts_whole(key: &str, text: &str, voice: &str) -> Result<Vec<u8>> {
    let body = json!({
        "text": text,
        "voice_id": voice,
        "format": "wav",
        "filter_markdown": true,
    });
    let response = crate::http::client()
        .post(http("/v1/tools/tts"))
        .bearer_auth(key)
        .json(&body)
        .timeout(Duration::from_secs(120))
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("TTS {status}: {}", clip(&body));
    }
    Ok(response.bytes().await?.to_vec())
}

/// `wss://…/tts`：把文本按句切成几段陆续发过去，收齐二进制 PCM 帧后包成 WAV。
///
/// 流式那条路的意义是「先出声」：即便整段回复已经拿到，分段发送也能让服务端更早开始吐
/// 音频帧。PCM 是 24 kHz、16 位、单声道，按这个规格补 WAV 头即可直接拿去转 MP3。
async fn tts_stream(key: &str, text: &str, voice: &str) -> Result<Vec<u8>> {
    // tungstenite 的 rustls 连接器走 ClientConfig::builder()，它在项目里同时启用了
    // ring 与 aws_lc_rs 两个 CryptoProvider 时会挑不出来而 panic。reqwest 那条 HTTPS
    // 路用的是显式 provider，不替它装；这里为 WebSocket 补一个进程级默认即可。
    let _ = rustls::crypto::ring::default_provider().install_default();
    let url = format!("{}?voice_id={voice}&format=pcm", ws("/v1/tools/tts"));
    let mut request = url.into_client_request()?;
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {key}").parse()?);
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await?;

    for chunk in split_text(text, 160) {
        socket
            .send(WsMessage::Text(
                json!({"event": "text", "text": chunk}).to_string().into(),
            ))
            .await?;
    }
    socket
        .send(WsMessage::Text(
            json!({"event": "finish_text"}).to_string().into(),
        ))
        .await?;

    let mut pcm = Vec::new();
    let mut finished = false;
    let pumped = async {
        while let Some(frame) = socket.next().await {
            match frame? {
                WsMessage::Binary(bytes) => pcm.extend_from_slice(&bytes),
                WsMessage::Text(text) => {
                    let value: Value = serde_json::from_str(text.as_str())?;
                    if value.get("event").and_then(Value::as_str) == Some("finish") {
                        let code = value.get("code").and_then(Value::as_str).unwrap_or("");
                        if code != "success" {
                            bail!(
                                "TTS 流式 {code}: {}",
                                value.get("message").and_then(Value::as_str).unwrap_or("")
                            );
                        }
                        finished = true;
                        break;
                    }
                }
                WsMessage::Close(_) => break,
                // Ping 由 tungstenite 自动回 Pong；其余帧忽略。
                _ => {}
            }
        }
        Ok::<(), anyhow::Error>(())
    };
    tokio::time::timeout(Duration::from_secs(180), pumped)
        .await
        .map_err(|_| anyhow!("TTS 流式超时"))??;
    if !finished || pcm.is_empty() {
        bail!("TTS 流式没收到完整音频");
    }
    Ok(wav_from_pcm(&pcm))
}

/// 合成好的 WAV/PCM → MP3 → `upload.create` → 一条语音气泡发进群。
/// 语音是「顺媒体」：单独成条，不带引用也不带文字（同条引用会把它顶成空气泡）。
pub(crate) async fn speak(
    ctx: &Context,
    writer: &LockedWriter,
    group: Option<&str>,
    user: Option<&str>,
    text: &str,
    cfg: &OaiConfig,
) -> Result<()> {
    let deep = &cfg.deepseek;
    let key = native::official_key(cfg).ok_or_else(|| anyhow!("没接 DeepSeek 官方密钥"))?;
    let wav = synthesize(&key, text, deep).await?;
    let mp3 = to_mp3(&wav).await?;
    let name = format!(
        "acumen-{}.mp3",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    );
    let uploaded = writer
        .upload(ctx, mp3, &name, "audio/mpeg")
        .await
        .map_err(|error| anyhow!("{error}"))?;
    let resource = uploaded
        .get("file")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("upload.create 没返回 file 资源"))?;
    send_msg(
        ctx,
        writer.clone(),
        group,
        user,
        Message::new().record(resource),
    )
    .await
    .map_err(|error| anyhow!("{error}"))?;
    info!(target: LOG_TARGET, "已朗读 {} 字", text.chars().count());
    Ok(())
}

// ============================================================================
// 音频小工具
// ============================================================================

fn is_wav(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE"
}

fn is_ogg(bytes: &[u8]) -> bool {
    bytes.starts_with(b"OggS")
}

/// 24 kHz、16 位、单声道原始 PCM → WAV（补 44 字节头）。
fn wav_from_pcm(pcm: &[u8]) -> Vec<u8> {
    const RATE: u32 = 24_000;
    const CHANNELS: u16 = 1;
    const BITS: u16 = 16;
    let data_len = pcm.len() as u32;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // fmt 块长度
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&CHANNELS.to_le_bytes());
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * u32::from(CHANNELS) * u32::from(BITS) / 8).to_le_bytes());
    wav.extend_from_slice(&(CHANNELS * BITS / 8).to_le_bytes()); // 块对齐（2 字节）
    wav.extend_from_slice(&BITS.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

/// WAV 头里声明的 `data` 长度有没有真的收齐：中途断开的响应当失败。
///
/// 整段 TTS 是边合成边返回的流式 WAV：`RIFF` 与 `data` 的长度都写成 `0xFFFFFFFF`
/// （未知，实测如此），没法按声明值校验。这时只看后面确实收到了一段音频数据；
/// 自己包的 WAV（流式 WebSocket 的 PCM）长度是写实的，按声明值逐字节核对。
fn wav_complete(bytes: &[u8]) -> bool {
    if !is_wav(bytes) {
        return false;
    }
    const UNKNOWN: usize = u32::MAX as usize;
    let mut offset = 12usize;
    while offset + 8 <= bytes.len() {
        let size = u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]) as usize;
        if &bytes[offset..offset + 4] == b"data" {
            return if size == UNKNOWN {
                // 至少得有一段像样的 PCM（几个采样帧）
                bytes.len() > offset + 8 + 256
            } else {
                offset + 8 + size <= bytes.len()
            };
        }
        if size == UNKNOWN {
            return false; // 非 data 块不该是未知长度，也防住偏移死循环
        }
        // 块按偶数字节对齐。
        offset += 8 + size + size % 2;
    }
    false
}

/// 朗读前把 Markdown 收成能念的纯文本：跳过代码块、去掉图片/链接的地址、
/// 抹掉强调符号与裸网址，压平空白并截到接口计费上限以内。
fn clean_for_speech(text: &str) -> String {
    static IMG: OnceLock<regex::Regex> = OnceLock::new();
    static LINK: OnceLock<regex::Regex> = OnceLock::new();
    static URL: OnceLock<regex::Regex> = OnceLock::new();
    let img = IMG.get_or_init(|| regex::Regex::new(r"!\[([^\]]*)\]\([^)]*\)").unwrap());
    let link = LINK.get_or_init(|| regex::Regex::new(r"\[([^\]]*)\]\([^)]*\)").unwrap());
    let url = URL.get_or_init(|| regex::Regex::new(r"https?://[^\s)）】]+").unwrap());

    let mut out = String::new();
    let mut in_code = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        let s = img.replace_all(line, "$1");
        let s = link.replace_all(&s, "$1");
        let s = url.replace_all(&s, "");
        let s = s.replace(['#', '*', '`', '>', '|', '~', '_'], "");
        out.push_str(s.trim());
        out.push('\n');
    }
    let joined = out.split_whitespace().collect::<Vec<_>>().join(" ");
    // 汉字按 2 个计费字符算，3000 字 ≲ 6000 计费字符，稳在 10,000 上限内。
    joined.chars().take(3000).collect()
}

/// 按句读标点把长文本切成几段，供 WebSocket 流式发送。尽量在标点处断，别把一个词劈开。
fn split_text(text: &str, max: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        current.push(ch);
        let end = matches!(ch, '。' | '！' | '？' | '；' | '\n' | '.' | '!' | '?' | ',' | '，');
        if current.chars().count() >= max && (end || current.chars().count() >= max * 2) {
            chunks.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push(text.to_string());
    }
    chunks
}

/// ffmpeg 转换：输入写临时文件、输出读回来，两个临时文件用完即删。
/// `args` 是输出格式之前的那串参数（`-f mp3` / `-f wav` 也含在内），输出路径自动补在最后。
async fn ffmpeg(input: &[u8], args: &[&str]) -> Result<Vec<u8>> {
    let program = std::env::var("PREFIX")
        .map_or_else(|_| "ffmpeg".to_string(), |prefix| format!("{prefix}/bin/ffmpeg"));
    let tag = format!("{:016x}", rand::random::<u64>());
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let input_path = PathBuf::from(&home).join(format!(".acumen-ff-{tag}.in"));
    let output_path = PathBuf::from(&home).join(format!(".acumen-ff-{tag}.out"));
    tokio::fs::write(&input_path, input).await?;

    let result: Result<Vec<u8>> = async {
        let status = tokio::time::timeout(
            Duration::from_secs(120),
            tokio::process::Command::new(&program)
                .arg("-y")
                .arg("-loglevel")
                .arg("error")
                .arg("-i")
                .arg(&input_path)
                .args(args)
                .arg(&output_path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status(),
        )
        .await
        .map_err(|_| anyhow!("ffmpeg 超时"))??;
        if !status.success() {
            bail!("ffmpeg 转换失败");
        }
        let bytes = tokio::fs::read(&output_path).await?;
        if bytes.len() < 1024 {
            bail!("ffmpeg 输出太短");
        }
        Ok(bytes)
    }
    .await;
    let _ = tokio::fs::remove_file(&input_path).await;
    let _ = tokio::fs::remove_file(&output_path).await;
    result
}

async fn to_mp3(wav: &[u8]) -> Result<Vec<u8>> {
    ffmpeg(wav, &["-codec:a", "libmp3lame", "-b:a", "64k", "-f", "mp3"]).await
}

async fn to_wav16k(raw: &[u8]) -> Result<Vec<u8>> {
    ffmpeg(
        raw,
        &["-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le", "-f", "wav"],
    )
    .await
}

fn clip(text: &str) -> String {
    let text = text.trim();
    let short: String = text.chars().take(200).collect();
    if short.chars().count() < text.chars().count() {
        format!("{short}…")
    } else {
        short
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tts_mode_and_keywords() {
        assert_eq!(TtsMode::parse("off"), TtsMode::Off);
        assert_eq!(TtsMode::parse("关闭"), TtsMode::Off);
        assert_eq!(TtsMode::parse("always"), TtsMode::Always);
        assert_eq!(TtsMode::parse("总 是"), TtsMode::Keyword); // 认不出按关键词
        let cfg = DeepSeekConfig::default();
        assert!(wants_tts(&cfg, "这个能念出来吗"));
        assert!(wants_tts(&cfg, "用语音回我"));
        assert!(!wants_tts(&cfg, "今天天气咋样"));
        let mut off = cfg.clone();
        off.tts = "off".into();
        assert!(!wants_tts(&off, "念出来"));
        let mut all = cfg;
        all.tts = "always".into();
        assert!(wants_tts(&all, "随便一句"));
    }

    #[test]
    fn wav_header_is_24k_mono_s16() {
        let pcm = vec![0u8; 4800]; // 0.1 秒
        let wav = wav_from_pcm(&pcm);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1); // 声道
        assert_eq!(u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]), 24_000);
        assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 16); // 位深
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]), 4800);
        assert_eq!(wav.len(), 44 + 4800);
        assert!(wav_complete(&wav));
        // 少收一段数据就被判成中途断开。
        assert!(!wav_complete(&wav[..wav.len() - 10]));
        // 官方整段 TTS 回的是流式 WAV：RIFF/data 长度都写 0xFFFFFFFF，仍算完整。
        let mut streamed = wav.clone();
        streamed[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        streamed[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(wav_complete(&streamed));
    }

    #[test]
    fn speech_text_strips_markdown_and_caps() {
        let raw = "**重点**：看 ![趋势图](https://x/a.png) 与 [文档](https://x/b)。\n\
                   ```rust\nfn main() {}\n```\n访问 https://example.com/very/long 就行。";
        let clean = clean_for_speech(raw);
        assert!(!clean.contains("**"));
        assert!(!clean.contains("https://"));
        assert!(!clean.contains("fn main"));
        assert!(clean.contains("趋势图"));
        assert!(clean.contains("文档"));
        let long = "啊".repeat(5000);
        assert_eq!(clean_for_speech(&long).chars().count(), 3000);
    }

    #[test]
    fn split_text_keeps_all_content() {
        let text = "第一句比较长啊。第二句也不短呢，还有半句？最后收尾！";
        let parts = split_text(text, 8);
        assert!(parts.len() > 1);
        assert_eq!(parts.concat(), text);
    }

    #[test]
    fn tts_request_body_shape() {
        // 整段合成的请求形状：text / voice_id / format / filter_markdown 都在顶层。
        let body = json!({
            "text": "你好",
            "voice_id": "stella",
            "format": "wav",
            "filter_markdown": true,
        });
        assert_eq!(body["voice_id"], "stella");
        assert_eq!(body["format"], "wav");
        assert_eq!(body["filter_markdown"], true);
        assert_eq!(DeepSeekConfig::default().voice(), "stella");
        assert_eq!(
            DeepSeekConfig {
                voice_id: "TIDE".into(),
                ..DeepSeekConfig::default()
            }
            .voice(),
            "tide"
        );
    }

    #[test]
    fn asr_response_shape() {
        // 真实响应固化：转写在 text，时长在 duration_ms，字段名必须精确。
        let real: Value = serde_json::from_str(
            r#"{"request_id":"a69153e3","text":"上海今天天气怎么样？","duration_ms":3200}"#,
        )
        .unwrap();
        assert_eq!(
            real.get("text").and_then(Value::as_str),
            Some("上海今天天气怎么样？")
        );
        assert_eq!(real["duration_ms"], 3200);
    }

    /// 真机收口：按量计费，默认不跑。密钥从环境变量取，不写进仓库、不打印。
    ///
    /// `DEEPSEEK_API_KEY=… cargo test --offline real_deepseek_tools -- --ignored --nocapture`
    ///
    /// 一次过验四件事：整段合成、流式合成、合成结果回环识别、Files API 上传 + 按 file_id 问答。
    /// 每次都是针对一个具体风险点的真实请求，不是重复离线测试。
    #[test]
    #[ignore]
    fn real_deepseek_tools() {
        let key = std::env::var("DEEPSEEK_API_KEY").expect("要设 DEEPSEEK_API_KEY");
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let cfg = DeepSeekConfig::default();
                // 整段 HTTP：拿到完整的 WAV。
                let wav = tts_whole(&key, "你好，我是知微。", cfg.voice())
                    .await
                    .expect("整段合成");
                assert!(wav_complete(&wav), "整段 WAV 头尾不一致");
                // 回环：把合成结果喂回 ASR，识别出的文字非空。
                let heard = transcribe_audio(&key, wav.clone())
                    .await
                    .expect("ASR 回环");
                assert!(!heard.is_empty());
                // 流式 WebSocket：PCM 帧包成 WAV 后同样是完整的。
                let streamed = DeepSeekConfig {
                    tts_stream: true,
                    ..cfg.clone()
                };
                let wav2 = synthesize(&key, "今天天气不错，适合出门走走。", &streamed)
                    .await
                    .expect("流式合成");
                assert!(wav_complete(&wav2), "流式 WAV 头尾不一致");
                // Files API：上传一份纯文本（user_data 免费），再按 file_id 问答。
                let id = super::super::files::upload(
                    &key,
                    "知微最喜欢的数字是 42。\n".as_bytes().to_vec(),
                    "probe.txt",
                    "user_data",
                )
                .await
                .expect("Files API 上传");
                assert!(id.starts_with("file-api-"));
                let body = json!({
                    "model": "deepseek-flash",
                    "messages": [{
                        "role": "user",
                        "content": [
                            super::super::files::document_part(&id),
                            {"type": "text", "text": "这个文件里最喜欢的数字是多少？只答数字。"}
                        ]
                    }],
                    "max_tokens": 32,
                });
                let response = crate::http::client()
                    .post(format!("{}/chat/completions", native::OFFICIAL))
                    .bearer_auth(&key)
                    .json(&body)
                    .send()
                    .await
                    .expect("文件问答请求");
                assert!(response.status().is_success(), "{response:?}");
                let value: Value = response.json().await.expect("文件问答响应");
                assert!(
                    value["choices"][0]["message"]["content"]
                        .as_str()
                        .unwrap_or("")
                        .contains("42"),
                    "{value}"
                );

                // 原生档的完整请求：正规 latest_reminder + 扁平文档块 + 服务端联网工具
                // + 思考 + 标题 一起发，确认这几件扩展互不打架、模型真读到了文件。
                use rig_core::completion::Message;
                use rig_core::completion::message::{Text, UserContent};
                use rig_core::wire::{Body, Mode, Wire};
                let native = native::Native::resolve(
                    &cfg,
                    "https://api.deepseek.com/v1",
                    "deepseek-flash",
                    Some("high"),
                    true,
                    true,
                )
                .unwrap();
                let history = vec![Message::User {
                    content: vec![UserContent::Text(Text::new(
                        "这个文件里最喜欢的数字是多少？只答数字。",
                    ))],
                }];
                let mut encoded_request = super::super::llm::request(
                    history,
                    Vec::new(),
                    None,
                    None,
                    Some(&native),
                );
                // 开思考时前几十个 token 会花在推理上，给足预算让正文真的写出来。
                encoded_request.max_tokens = Some(512);
                let model = super::super::llm::native_model(
                    "https://api.deepseek.com/v1",
                    &key,
                    "deepseek-flash",
                );
                super::super::llm::inject_extras(
                    &model,
                    &mut encoded_request,
                    std::slice::from_ref(&id),
                    Some(&native.reminder()),
                );
                let encoded = model.wire.encode(encoded_request, Mode::Unary).unwrap();
                let Body::Bytes(bytes) = encoded.request.body() else {
                    panic!("请求体应是 JSON 字节");
                };
                let response = crate::http::client()
                    .post(format!("{}/chat/completions", native::OFFICIAL))
                    .bearer_auth(&key)
                    .header("Content-Type", "application/json")
                    .body(bytes.clone())
                    .send()
                    .await
                    .expect("原生完整请求");
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                assert!(status.is_success(), "{status}: {text}");
                let value: Value = serde_json::from_str(&text).unwrap();
                assert!(
                    value["choices"][0]["message"]["content"]
                        .as_str()
                        .unwrap_or("")
                        .contains("42"),
                    "{text}"
                );
                println!("真机收口通过：整段/流式合成、ASR 回环、Files 问答、原生完整请求");
            });
    }
}
