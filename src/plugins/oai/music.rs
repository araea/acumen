//! Suno 文生歌（中转站的 `/suno/*` 接口）。
//!
//! 房间模型关键字命中 `[oai] music_models`（默认 `suno`）的房间，提示词不再交给聊天
//! 补全，而是走 Suno：先 `POST /suno/submit/music` 拿任务号，再 `GET /suno/fetch/{id}`
//! 轮询到出歌。一次提交会返回两个版本，两个都发——封面走图片段，音频按
//! `[oai] music_send` 决定发语音气泡还是群文件。
//!
//! 房间的系统提示词仍然当风格前缀用（和图像房间同一套写法），用户那句话接在后面；
//! 提示词是歌词体裁（带 `[Verse]` 之类的段落标记）时改走自定义模式，否则走「灵感模式」
//! ——把整段文字当作创作描述交给 Suno，歌词由它自己写。

use super::logic::{Media, MediaMessage, Reply};
use super::types::{Agent, ChatMessage};
use anyhow::{Context as _, anyhow};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use std::time::Duration;

/// 默认走 Suno 的模型关键字（不区分大小写、子串匹配）。
/// 站点上加别的音乐服务商时改 `[oai] music_models` 即可。
pub(super) const DEFAULT_MUSIC_MODELS: &[&str] = &["suno"];

/// 兜底模型 id：模型列表还没拉回来（或列表里没有 Suno）时用它。
/// 预设房间与群聊搭话的写歌工具都靠它兜底。
pub(crate) const FALLBACK_MODEL: &str = "suno_music";

/// 兜底版本号。中转站实际把 `chirp-v5` 映射到当前最新模型（返回里是 `v6` / `chirp-hawk`），
/// 所以写 `chirp-v5` 拿到的就是最"新"的那一档；站点接上更新的版本时改 `[oai] music_version`。
pub(super) const DEFAULT_VERSION: &str = "chirp-v5";

const POLL_INTERVAL: Duration = Duration::from_secs(6);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);
/// 一首歌最多等多久；真正的总预算由 `[oai] media_timeout_seconds` 兜底。
const TASK_TIMEOUT: Duration = Duration::from_secs(8 * 60);
/// 一次提交最多发几首（Suno 一次给两个版本）。
const MAX_CLIPS: usize = 2;
/// 回复里最多带几行歌词当引子。
const LYRICS_LINES: usize = 4;
/// 风格那一行最多留几个字。
const STYLE_MAX_CHARS: usize = 60;

/// 音频怎么发进群。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendMode {
    /// 只发群文件。
    File,
    /// 只发语音气泡。
    Voice,
    /// 先发语音气泡（点开就听），再补一条群文件（能存能下载）。
    Both,
}

impl SendMode {
    pub(super) fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "voice" | "语音" => Self::Voice,
            "file" | "文件" | "群文件" => Self::File,
            _ => Self::Both,
        }
    }

    /// 提示词里的开关。`None` 表示这句话没说怎么发，照 `[oai] music_send` 走。
    fn flag(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "--语音" | "--voice" => Some(Self::Voice),
            "--文件" | "--群文件" | "--file" => Some(Self::File),
            "--都发" | "--both" => Some(Self::Both),
            _ => None,
        }
    }

    /// 卡片上标一句这一单是怎么发的，换个发法不用去猜配置里写的是什么。
    fn label(self) -> &'static str {
        match self {
            Self::File => "群文件",
            Self::Voice => "语音",
            Self::Both => "群文件 + 语音",
        }
    }

    fn file(self) -> bool {
        matches!(self, Self::File | Self::Both)
    }

    fn voice(self) -> bool {
        matches!(self, Self::Voice | Self::Both)
    }
}

/// 模型是否走 Suno 接口。`keywords` 为空时视为不启用。
pub(crate) fn is_music_model(model: &str, keywords: &[String]) -> bool {
    let lower = model.trim().to_lowercase();
    keywords
        .iter()
        .filter(|keyword| !keyword.trim().is_empty())
        .any(|keyword| lower.contains(&keyword.trim().to_lowercase()))
}

/// 一次生成的产物：若干版本的歌。
pub(crate) struct Generated {
    pub(crate) lyrics: String,
    pub(crate) tags: String,
    pub(crate) version: String,
    /// 站点给的计费数字（单位与站点标价一致）。
    pub(crate) cost: f64,
    pub(crate) clips: Vec<Clip>,
}

/// 去掉参数后剩下的正文，以及几个可选项。
///
/// 房间那条路径从一段文本里剥出它（[`Options::parse`]），群聊搭话那条路径拿到的是
/// 结构化的工具参数，直接填字段——两条路最后都落到同一份选项与同一个 [`generate`]。
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Options {
    pub(crate) prompt: String,
    pub(crate) title: String,
    pub(crate) tags: String,
    /// 留空表示用 [`DEFAULT_VERSION`]（房间那边会用 `[oai] music_version` 先兜一层）。
    pub(crate) version: String,
    pub(crate) instrumental: bool,
    /// 这一句自己指定的发法（`--语音` / `--文件` / `--都发`），没写就照配置走。
    pub(crate) send: Option<SendMode>,
}

impl Options {
    /// 从提示词里剥离 `--标题/--title`、`--风格/--tags`、`--版本/--mv`、`--纯音乐/--instrumental`
    /// 与 `--语音/--文件/--都发`。参数缺值时原样留在正文里，避免把用户想写的东西
    /// 悄悄吃掉。
    pub(crate) fn parse(input: &str) -> Self {
        let mut words: Vec<&str> = Vec::new();
        let mut options = Self::default();
        let mut tokens = input.split_whitespace().peekable();

        while let Some(token) = tokens.next() {
            let lower = token.to_ascii_lowercase();
            match lower.as_str() {
                "--标题" | "--title" | "--歌名" => match tokens.peek() {
                    Some(value) => {
                        options.title = (*value).to_string();
                        tokens.next();
                    }
                    None => words.push(token),
                },
                "--风格" | "--tags" | "--tag" => match tokens.peek() {
                    Some(value) => {
                        options.tags = (*value).to_string();
                        tokens.next();
                    }
                    None => words.push(token),
                },
                "--版本" | "--mv" => match tokens.peek() {
                    Some(value) => {
                        options.version = (*value).to_string();
                        tokens.next();
                    }
                    None => words.push(token),
                },
                "--纯音乐" | "--instrumental" | "--instrument" => options.instrumental = true,
                _ => match SendMode::flag(token) {
                    Some(mode) => options.send = Some(mode),
                    None => words.push(token),
                },
            }
        }

        options.prompt = words.join(" ");
        options
    }
}

/// 正文是不是一段写好的歌词。Suno 的段落标记是 `[Verse]` / `[Chorus]` 这一族；
/// 中文写法（`[主歌]` / `[副歌]`）一并认。
fn looks_like_lyrics(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "[verse", "[chorus", "[bridge", "[intro", "[outro", "[pre-chorus", "[hook", "[主歌",
        "[副歌", "[间奏", "[尾声",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

/// 最后一条用户消息就是本轮提示词；房间系统提示词是风格前缀。
fn last_user(hist: &[ChatMessage]) -> Option<&ChatMessage> {
    hist.iter().rev().find(|message| message.role == "user")
}

/// 调用 Suno，把结果包装成与聊天补全一致的 `Reply`（正文是歌名/时长/歌词引子，
/// 封面与音频放进 `media` 由发送阶段按段发出去）。
pub(super) async fn generate_reply(
    api_base: &str,
    api_key: &str,
    agent: &Agent,
    hist: &[ChatMessage],
    config: &super::OaiConfig,
) -> anyhow::Result<Reply> {
    let last = last_user(hist);
    let mut options = Options::parse(
        &match (agent.system_prompt.trim(), last.map(|m| m.content.as_str()).unwrap_or("")) {
            ("", user) => user.to_string(),
            (system, "") => system.to_string(),
            (system, user) => format!("{system}\n{user}"),
        },
    );
    if options.prompt.trim().is_empty() {
        return Ok(super::logic::guidance(
            "💡 没说想写什么歌\n例如：唱一首关于秋天的民谣",
        ));
    }
    if options.version.trim().is_empty() {
        options.version = config.music_version();
    }
    let generated = generate(api_base, api_key, &options, config.media_timeout()).await?;

    // 这一句带 `--语音` / `--文件` 就听它的，没带才照 `[oai] music_send` 走。
    let send = options.send.unwrap_or_else(|| config.music_send());

    let title = title_of(&generated);
    let text = summary(&generated, &title, send, options.send.is_some());

    Ok(Reply {
        text,
        sources: Vec::new(),
        trace: Vec::new(),
        trace_overflow: 0,
        model: Some(agent.model.clone()),
        plain: true,
        media: media_messages(&generated.clips, &title, send),
    })
}

/// 卡片上写哪个歌名：Suno 两个版本一般共用同一个标题，取第一个非空的。
fn title_of(generated: &Generated) -> String {
    generated
        .clips
        .iter()
        .map(|clip| clip.title.trim())
        .find(|title| !title.is_empty())
        .unwrap_or("生成完成")
        .to_string()
}

/// 正文。这一轮是纯文本（不渲染卡片），所以一个 markdown 记号都不能用——`**` 会原样
/// 出现在群里。一行一件事：第一行歌名，第二行两个版本各自的时长与这一单的账，第三行
/// 风格，然后隔一行点明这是歌词。`show_send` 只在用户自己写了发法时打开。
fn summary(generated: &Generated, title: &str, send: SendMode, show_send: bool) -> String {
    let durations: Vec<String> = generated
        .clips
        .iter()
        .map(|clip| mmss(clip.duration))
        .collect();
    let mut text = title.to_string();
    let mut meta = if generated.clips.len() > 1 {
        format!("两个版本：{}", durations.join(" / "))
    } else {
        durations.join(" / ")
    };
    if !generated.version.trim().is_empty() {
        meta.push_str(&format!(" · Suno {}", generated.version.trim()));
    }
    if generated.cost > 0.0 {
        meta.push_str(&format!(" · ${:.2}", generated.cost));
    }
    // 照配置走是常态，不必每张卡片都念一遍发法。
    if show_send {
        meta.push_str(&format!(" · {}", send.label()));
    }
    text.push('\n');
    text.push_str(&meta);
    let style = style_line(&generated.tags);
    if !style.is_empty() {
        text.push_str("\n风格：");
        text.push_str(&style);
    }
    let lyrics = lyrics_excerpt(&generated.lyrics);
    if !lyrics.is_empty() {
        text.push_str("\n\n歌词\n");
        text.push_str(&lyrics);
    }
    text
}

/// 每个版本两条消息：`both` 时先一条语音气泡、再一条群文件；只发一种就只出一条。
/// 封面跟着语音那条走，到了实现端会按「顺媒体单独成条」拆开——群里先看到封面那张图，
/// 再是语音（见 satori-qq 的 `docs/SATORI_SUPPORT.md`）。
///
/// 顺序是刻意的：有些群不让普通成员发群文件，那条腿会失败并在实现端重试到超预算
/// （默认 2 次 / 45 秒）。语音排在前面，点开就听的那条先落地，坏掉的文件腿拖不住它
/// （调用方只 warn，不发第二条错）。
///
/// 两个版本的文件名带上序号——Suno 给同一单两首曲子的是同一个标题，照原样发出去
/// 群里会出现两个同名文件，谁是谁分不出来。
fn media_messages(clips: &[Clip], title: &str, mode: SendMode) -> Vec<MediaMessage> {
    let base = super::utils::safe_file_name(title);
    let mut messages = Vec::new();
    for (index, clip) in clips.iter().enumerate() {
        let audio = clip.audio_url.trim();
        if audio.is_empty() {
            continue;
        }
        let name = if clips.len() > 1 {
            format!("{} {}.mp3", base, index + 1)
        } else {
            format!("{base}.mp3")
        };
        // 封面只挂在这一版的第一条上，两条都挂就是同一个封面进群两次。
        let mut cover = (!clip.image_url.trim().is_empty()).then(|| Media::Image {
            url: clip.image_url.trim().to_string(),
        });
        if mode.voice() {
            let mut segments = Vec::new();
            if let Some(cover) = cover.take() {
                segments.push(cover);
            }
            segments.push(Media::Audio {
                url: audio.to_string(),
            });
            messages.push(MediaMessage { segments });
        }
        if mode.file() {
            let mut segments = Vec::new();
            if let Some(cover) = cover {
                segments.push(cover);
            }
            segments.push(Media::File {
                url: audio.to_string(),
                name,
            });
            messages.push(MediaMessage { segments });
        }
    }
    messages
}

/// 风格那一行。Suno 回的 tags 是一整段风格描述（v6 起尤其长），整段贴进群没人读；
/// 截到 [`STYLE_MAX_CHARS`] 内最后一个逗号或分号为止，让句子断在能读懂的地方——
/// 从前是硬截到 80 字再挂个省略号，常断在半个词上。
fn style_line(tags: &str) -> String {
    let tags = tags.trim();
    if tags.chars().count() <= STYLE_MAX_CHARS {
        return tags.to_string();
    }
    let head: String = tags.chars().take(STYLE_MAX_CHARS).collect();
    match head.rfind(|c| matches!(c, ',' | ';' | '，' | '；' | '/')) {
        Some(at) if !head[..at].trim().is_empty() => head[..at].trim_end().to_string(),
        _ => head.trim_end().to_string(),
    }
}

/// 歌词引子：最多几行，末尾空行与段落标记丢掉，让回复读起来像人写的摘要。
fn lyrics_excerpt(lyrics: &str) -> String {
    let lines: Vec<&str> = lyrics
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !(line.starts_with('[') && line.ends_with(']')))
        .take(LYRICS_LINES)
        .collect();
    lines.join("\n")
}

fn mmss(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

/// 文件名里的路径分隔符与引号一律换掉，QQ 群文件列表才不会看着乱。
fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\n' | '\r' => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() {
        "suno".to_string()
    } else {
        super::utils::truncate_str(&cleaned, 60)
    }
}

/// 提交一次生成并等到出结果。
///
/// 与房间的回复构造解耦，供普通房间（[`generate_reply`]）与群聊搭话的写歌工具共用；
/// 搭话那条路径拿不到 `Reply`，要的是落到本地、能随后用 `satori_action` 发出去的成品。
pub(crate) async fn generate(
    api_base: &str,
    api_key: &str,
    options: &Options,
    deadline: Duration,
) -> anyhow::Result<Generated> {
    let prompt = options.prompt.trim();
    if prompt.is_empty() {
        return Err(anyhow!("没说想写什么歌"));
    }
    let version = if options.version.trim().is_empty() {
        DEFAULT_VERSION
    } else {
        options.version.trim()
    };
    let base = service_base(api_base);
    let custom = looks_like_lyrics(prompt);
    let body = json!({
        "prompt": if custom { prompt } else { "" },
        "tags": options.tags.trim(),
        "title": options.title.trim(),
        "mv": version,
        "make_instrumental": options.instrumental,
        "gpt_description_prompt": if custom { "" } else { prompt },
        "task_id": "",
        "continue_at": 0,
        "continue_clip_id": "",
        "notify_hook": "",
    });

    let task_id = submit(&base, api_key, &body).await?;
    info!(
        target: "Plugin/OAI/Music",
        "Suno 任务 {} 已提交（{}，{}模式）",
        task_id,
        version,
        if custom { "自定义" } else { "灵感" }
    );
    let task = poll(&base, api_key, &task_id, deadline).await?;
    if task.clips.is_empty() {
        return Err(anyhow!("Suno 任务已完成，但没有返回任何音频"));
    }
    Ok(Generated {
        lyrics: task
            .clips
            .first()
            .map(|clip| clip.prompt.clone())
            .unwrap_or_default(),
        tags: task
            .clips
            .first()
            .map(|clip| clip.tags.clone())
            .unwrap_or_default(),
        version: task
            .clips
            .first()
            .map(|clip| clip.model_version())
            .filter(|version| !version.trim().is_empty())
            .unwrap_or_else(|| version.to_string()),
        cost: task.cost,
        clips: task.clips.into_iter().take(MAX_CLIPS).collect(),
    })
}

/// Suno 的接口在服务根路径下（`/suno/...`），而房间配的是 OpenAI 兼容的 `.../v1`。
fn service_base(configured: &str) -> String {
    let base = configured.trim().trim_end_matches('/');
    base.strip_suffix("/v1").unwrap_or(base).trim_end_matches('/').to_string()
}

async fn submit(base: &str, key: &str, body: &Value) -> anyhow::Result<String> {
    let endpoint = format!("{base}/suno/submit/music");
    let response = crate::http::client()
        .post(&endpoint)
        .bearer_auth(key)
        .json(body)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .with_context(|| format!("提交 {endpoint} 失败"))?;
    let status = response.status();
    let bytes = response.bytes().await.context("读取提交响应失败")?;
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let code = value
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if code.eq_ignore_ascii_case("success") {
        return scalar_string(value.get("data"))
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow!("提交成功但没有返回任务 ID"));
    }
    if !status.is_success() || !code.is_empty() {
        let detail = if message.trim().is_empty() {
            excerpt(&bytes)
        } else {
            message.to_string()
        };
        return Err(anyhow!("Suno 提交失败（{}）：{}", status.as_u16(), detail));
    }
    Err(anyhow!("Suno 提交失败：{}", excerpt(&bytes)))
}

#[derive(Debug, Default, Deserialize)]
struct Task {
    #[serde(default, deserialize_with = "null_default")]
    status: String,
    #[serde(default, deserialize_with = "null_default")]
    fail_reason: String,
    #[serde(default, deserialize_with = "null_default")]
    progress: String,
    /// 任务产物：`MUSIC` 是曲子数组，生成歌词时是单个对象；这里只取数组。
    #[serde(default, rename = "data", deserialize_with = "null_default")]
    clips: Vec<Clip>,
    #[serde(default)]
    cost: f64,
}

impl Task {
    fn done(&self) -> Option<Result<(), String>> {
        match self.status.to_ascii_uppercase().as_str() {
            "SUCCESS" => Some(Ok(())),
            "FAILURE" => Some(Err(if self.fail_reason.trim().is_empty() {
                "Suno 任务执行失败".to_string()
            } else {
                self.fail_reason.trim().to_string()
            })),
            _ if self.progress.trim() == "100%" => Some(Ok(())),
            _ => None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct Clip {
    /// 音频直链（`suno.day`，带签名）。
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) audio_url: String,
    /// 封面直链（`cdn2.suno.ai`）。
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) image_url: String,
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) title: String,
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) tags: String,
    /// 歌词。
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) prompt: String,
    #[serde(default, deserialize_with = "null_default")]
    major_model_version: String,
    #[serde(default, deserialize_with = "null_default")]
    model_name: String,
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) duration: f64,
}

impl Clip {
    /// 回复里写的版本：优先人类看的 `v6`，没有再退到渠道名 `chirp-hawk`。
    fn model_version(&self) -> String {
        if !self.major_model_version.trim().is_empty() {
            self.major_model_version.trim().to_string()
        } else {
            self.model_name.trim().to_string()
        }
    }
}

async fn poll(base: &str, key: &str, task_id: &str, deadline: Duration) -> anyhow::Result<Task> {
    let endpoint = format!("{base}/suno/fetch/{task_id}");
    let stop = tokio::time::Instant::now() + TASK_TIMEOUT.min(deadline);
    loop {
        let response = crate::http::client()
            .get(&endpoint)
            .bearer_auth(key)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .context("查询 Suno 任务失败")?;
        let status = response.status();
        let bytes = response.bytes().await.context("读取 Suno 任务响应失败")?;
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let code = value
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !status.is_success() || !code.eq_ignore_ascii_case("success") {
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .filter(|message| !message.trim().is_empty())
                .unwrap_or("");
            return Err(anyhow!(
                "查询 Suno 任务失败（{}）：{}",
                status.as_u16(),
                if message.is_empty() {
                    excerpt(&bytes)
                } else {
                    message.to_string()
                }
            ));
        }
        let task: Task = serde_json::from_value(value.get("data").cloned().unwrap_or(Value::Null))
            .context("无法解析 Suno 任务响应")?;
        if let Some(result) = task.done() {
            return match result {
                Ok(()) => Ok(task),
                Err(reason) => Err(anyhow!(reason)),
            };
        }
        if tokio::time::Instant::now() >= stop {
            return Err(anyhow!("等了太久还没出歌（任务 {task_id}）"));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Option::<T>::deserialize(deserializer).map(Option::unwrap_or_default)
}

fn scalar_string(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(value) = value.as_str() {
        Some(value.to_string())
    } else if let Some(value) = value.as_i64() {
        Some(value.to_string())
    } else {
        value.as_u64().map(|value| value.to_string())
    }
}

fn excerpt(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(300)]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_configured_keywords_case_insensitively() {
        let keywords = vec!["suno".to_string()];
        assert!(is_music_model("suno_music", &keywords));
        assert!(is_music_model("SUNO-lyrics", &keywords));
        assert!(!is_music_model("gpt-image-2.5-flare", &keywords));
        assert!(!is_music_model("suno_music", &[]));
    }

    #[test]
    fn parses_and_strips_music_flags() {
        let options = Options::parse("一首秋天的歌 --标题 落叶 --风格 folk,acoustic --版本 chirp-v5");
        assert_eq!(options.prompt, "一首秋天的歌");
        assert_eq!(options.title, "落叶");
        assert_eq!(options.tags, "folk,acoustic");
        assert_eq!(options.version, "chirp-v5");
        assert!(!options.instrumental);
    }

    #[test]
    fn keeps_valueless_flags_and_reads_instrumental() {
        let options = Options::parse("钢琴小品 --纯音乐 --title");
        assert_eq!(options.prompt, "钢琴小品 --title");
        assert!(options.instrumental);
        assert!(options.title.is_empty());
    }

    #[test]
    fn reads_the_send_flag_out_of_the_prompt() {
        let voice = Options::parse("唱一首秋天的民谣 --语音");
        assert_eq!(voice.prompt, "唱一首秋天的民谣");
        assert_eq!(voice.send, Some(SendMode::Voice));

        let file = Options::parse("唱一首秋天的民谣 --文件");
        assert_eq!(file.prompt, "唱一首秋天的民谣");
        assert_eq!(file.send, Some(SendMode::File));

        assert_eq!(
            Options::parse("秋天的民谣 --both").send,
            Some(SendMode::Both)
        );
        // 没写就不替用户决定，留给 `[oai] music_send`。
        assert_eq!(Options::parse("唱一首秋天的民谣").send, None);
        // 别把 `--语音` 当歌词或描述留下来。
        assert_eq!(Options::parse("--语音 秋天的民谣").prompt, "秋天的民谣");
    }

    #[test]
    fn send_modes_are_labelled_on_the_card() {
        assert_eq!(SendMode::File.label(), "群文件");
        assert_eq!(SendMode::Voice.label(), "语音");
        assert_eq!(SendMode::Both.label(), "群文件 + 语音");
    }

    #[test]
    fn recognizes_written_lyrics() {
        assert!(looks_like_lyrics("[Verse 1]\nSun on the table"));
        assert!(looks_like_lyrics("[副歌]\n啦啦啦"));
        assert!(!looks_like_lyrics("唱一首关于秋天的民谣"));
    }

    #[test]
    fn stems_the_voice_and_file_socket_from_the_url() {
        assert_eq!(service_base("https://api.apilio.ai/v1/"), "https://api.apilio.ai");
        assert_eq!(service_base("https://api.apilio.ai/v1"), "https://api.apilio.ai");
        assert_eq!(service_base("https://api.apilio.ai"), "https://api.apilio.ai");
    }

    #[test]
    fn send_mode_parses_with_a_permissive_default() {
        assert_eq!(SendMode::parse("voice"), SendMode::Voice);
        assert_eq!(SendMode::parse("文件"), SendMode::File);
        assert_eq!(SendMode::parse("both"), SendMode::Both);
        // 写错的值当成 both：宁可多发一条，也别把歌吞掉。
        assert_eq!(SendMode::parse("whatever"), SendMode::Both);
    }

    #[test]
    fn builds_one_message_per_clip_with_the_cover_up_front() {
        let clips = vec![
            Clip {
                audio_url: "https://a/1.mp3".into(),
                image_url: "https://a/1.jpg".into(),
                title: "落叶".into(),
                duration: 143.2,
                ..Default::default()
            },
            Clip {
                audio_url: "https://a/2.mp3".into(),
                image_url: "https://a/2.jpg".into(),
                title: "落叶".into(),
                duration: 61.0,
                ..Default::default()
            },
        ];
        let messages = media_messages(&clips, "落叶", SendMode::File);
        assert_eq!(messages.len(), 2);
        assert!(matches!(messages[0].segments[0], Media::Image { .. }));
        assert!(matches!(
            &messages[0].segments[1],
            Media::File { name, .. } if name == "落叶 1.mp3"
        ));
        // 两版各带序号，群里不会出现两个同名文件。
        assert!(matches!(
            &messages[1].segments[1],
            Media::File { name, .. } if name == "落叶 2.mp3"
        ));
        // both：每版先一条语音（封面跟着它走，发出时被实现端拆成封面 + 语音两条），
        // 再一条群文件。语音排在前面，文件那条腿失败（有些群不让发群文件）拖不住它。
        let both = media_messages(&clips, "落叶", SendMode::Both);
        assert_eq!(both.len(), 4);
        assert!(matches!(both[0].segments[0], Media::Image { .. }));
        assert!(matches!(both[0].segments[1], Media::Audio { .. }));
        assert!(matches!(
            &both[1].segments[0],
            Media::File { name, .. } if name == "落叶 1.mp3"
        ));
        assert!(matches!(both[2].segments[0], Media::Image { .. }));
        assert!(matches!(both[2].segments[1], Media::Audio { .. }));
        assert!(matches!(
            &both[3].segments[0],
            Media::File { name, .. } if name == "落叶 2.mp3"
        ));
        // 封面只挂一次，两条腿都挂就是同一个封面进群两遍。
        assert!(
            both.iter()
                .all(|message| message.segments.len() == 1 || matches!(message.segments[0], Media::Image { .. }))
        );
        // voice：不重复发文件。
        let voice = media_messages(&clips, "落叶", SendMode::Voice);
        assert_eq!(voice.len(), 2);
        assert!(matches!(voice[0].segments[1], Media::Audio { .. }));
    }

    #[test]
    fn a_single_clip_keeps_a_plain_file_name() {
        let clips = vec![Clip {
            audio_url: "https://a/1.mp3".into(),
            title: "落叶".into(),
            duration: 143.2,
            ..Default::default()
        }];
        let messages = media_messages(&clips, "落叶", SendMode::File);
        assert!(matches!(
            &messages[0].segments[0],
            Media::File { name, .. } if name == "落叶.mp3"
        ));
    }

    #[test]
    fn cuts_the_style_line_at_a_clause_not_a_word() {
        // 短的开头原样留着。
        assert_eq!(style_line("folk, acoustic"), "folk, acoustic");
        assert_eq!(style_line("   "), "");
        // 长的断在最后一个逗号上，不带省略号，也不断在半个词里。
        let long = "Future bass / kawaii J-pop with a bouncy 140 BPM half-step feel, \
                    glittery synths, sweet music box, sparkling effects";
        let cut = style_line(long);
        assert!(cut.chars().count() <= STYLE_MAX_CHARS);
        assert!(cut.starts_with("Future bass"));
        assert!(!cut.ends_with(','));
        assert!(!cut.contains("glittery"));
    }

    #[test]
    fn formats_durations_and_lyrics_as_a_short_lead_in() {
        assert_eq!(mmss(143.2), "2:23");
        assert_eq!(mmss(0.0), "0:00");
        assert_eq!(
            lyrics_excerpt("[Verse 1]\nSun on the table\n\nSteam in my mug\n[Chorus]"),
            "Sun on the table\nSteam in my mug"
        );
    }

    #[test]
    fn lays_the_summary_out_one_thing_per_line() {
        let generated = Generated {
            lyrics: "[Verse]\nSun on the table\nSteam in my mug".into(),
            tags: "folk, acoustic, close-mic vocal".into(),
            version: "v6".into(),
            cost: 0.5,
            clips: vec![
                Clip {
                    title: "落叶".into(),
                    duration: 191.0,
                    ..Default::default()
                },
                Clip {
                    title: "落叶".into(),
                    duration: 176.0,
                    ..Default::default()
                },
            ],
        };
        assert_eq!(title_of(&generated), "落叶");
        // 照配置走时不提发法：卡片上只有歌名、时长、账与风格。
        assert_eq!(
            summary(&generated, "落叶", SendMode::File, false),
            "落叶\n两个版本：3:11 / 2:56 · Suno v6 · $0.50\n风格：folk, acoustic, \
             close-mic vocal\n\n歌词\nSun on the table\nSteam in my mug"
        );
        // 自己指定了发法就标出来，省得回头猜这一单是怎么发的。
        assert!(
            summary(&generated, "落叶", SendMode::Voice, true)
                .contains("· Suno v6 · $0.50 · 语音\n")
        );
        // 正文是纯文本，一个 markdown 记号都不能有。
        assert!(!summary(&generated, "落叶", SendMode::File, false).contains("**"));
    }

    #[test]
    fn parses_a_finished_task_and_its_status_states() {
        let task: Task = serde_json::from_value(json!({
            "status": "SUCCESS",
            "fail_reason": "",
            "progress": "100%",
            "cost": 0.5,
            "data": [
                {"audio_url": "https://a/1.mp3", "image_url": null, "title": "落叶",
                 "duration": 143.2, "prompt": "[Verse]\nSun", "major_model_version": "v6"},
                {"audio_url": "https://a/2.mp3", "title": null, "duration": 134.4}
            ]
        }))
        .unwrap();
        assert!(task.done().unwrap().is_ok());
        assert_eq!(task.clips.len(), 2);
        assert_eq!(task.clips[0].model_version(), "v6");
        assert!(task.clips[1].title.is_empty());
        assert!((task.cost - 0.5).abs() < f64::EPSILON);

        let running: Task = serde_json::from_value(json!({"status": "IN_PROGRESS"})).unwrap();
        assert!(running.done().is_none());
        let failed: Task =
            serde_json::from_value(json!({"status": "FAILURE", "fail_reason": "上游超时"})).unwrap();
        assert_eq!(failed.done().unwrap().unwrap_err(), "上游超时");
    }
}