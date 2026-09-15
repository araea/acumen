//! 文生视频（中转站的 OpenAI 视频任务接口 `/v1/video/generations`）。
//!
//! 房间模型关键字命中 `[oai] video_models` 的房间走这里：`POST {base}/video/generations`
//! 拿任务号，再 `GET {base}/video/generations/{id}` 轮询到 `video_url`，最后把整段视频
//! 当一个视频消息发出去。接口是异步任务制的，一次生成通常一两分钟，所以这类房间用
//! `[oai] media_timeout_seconds` 而不是普通的 `request_timeout_seconds`。
//!
//! 默认预设房间用的是 veo3.1-fast：实测能在同一套接口上跑通、自带同步音效、出片约
//! 一分钟、一次约 $1.2，是这批顶级模型里性价比最高的一档。别的模型（`veo3.1-pro`、
//! `wan3.0-video-prime`、`sora-2` …）用 `房间%模型` 换过来即可——接口格式一致，但
//! 各家渠道的可用分组、单次价格与所需参数并不一样，换之前先确认能调通。

use super::logic::{Media, MediaMessage, Reply};
use super::types::{Agent, ChatMessage};
use anyhow::{Context as _, anyhow};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

/// 默认走视频接口的模型关键字（不区分大小写、子串匹配）。
///
/// 站点上新的视频模型时改 `[oai] video_models` 即可，不必改代码。刻意不写成
/// `wan` / `kling` 这种宽泛的词：同一家还有一堆图片模型，混进来会把画图和聊天
/// 房间一起打歪。
pub(super) const DEFAULT_VIDEO_MODELS: &[&str] = &[
    "veo3.1",
    "veo3.",
    "sora-2",
    "doubao-seedance",
    "kling-video",
    "wan3.0-video",
    "wan2.6-t2v",
    "viduq3",
    "grok-video",
    "MiniMax-Hailuo",
    "pixverse-video",
    "luma_video",
];

/// 兜底模型 id：模型列表还没拉回来时用它。
/// 预设房间与群聊搭话的拍片工具都靠它兜底。
pub(crate) const FALLBACK_MODEL: &str = "veo3.1-fast";

/// 预设房间优先挑的那一档。刻意挑 fast：同一族的 pro 贵出好几倍（实测 $7 对 $1.2），
/// 预设房间不该一上来就把账单顶上去。
pub(super) const PREFERRED_MODEL_KEYWORD: &str = "veo3.1-fast";

const POLL_INTERVAL: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// 一次生成最多等多久；真正的总预算由 `[oai] media_timeout_seconds` 兜底。
const TASK_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// 默认时长与分辨率。短一点更便宜，长一点更好看，改 `[oai] video_seconds` 即可。
const DEFAULT_SECONDS: &str = "5";
pub(crate) const LANDSCAPE: &str = "1280x720";
pub(crate) const PORTRAIT: &str = "720x1280";

/// 模型是否走视频任务接口。
pub(crate) fn is_video_model(model: &str, keywords: &[String]) -> bool {
    let lower = model.trim().to_lowercase();
    keywords
        .iter()
        .filter(|keyword| !keyword.trim().is_empty())
        .any(|keyword| lower.contains(&keyword.trim().to_lowercase()))
}

/// 拍好的片子怎么发进群。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendMode {
    /// 只发群文件：能存能下载，QQ 里是个文件卡片。
    File,
    /// 只发视频气泡：点开就播。
    Bubble,
    /// 先发群文件，再补一条视频气泡：一个能存能转，一个点开就看。
    Both,
}

impl SendMode {
    /// 与 `[oai] video_send` 同一套写法；视频解析插件也读这里，两处的
    /// 「文件 / 气泡 / 都发」是同一个意思。
    pub(crate) fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "file" | "文件" | "群文件" => Self::File,
            "video" | "bubble" | "气泡" | "视频" => Self::Bubble,
            _ => Self::Both,
        }
    }

    /// 提示词里的开关。`None` 表示这句没说怎么发，照 `[oai] video_send` 走。
    fn flag(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "--文件" | "--群文件" | "--file" => Some(Self::File),
            "--视频" | "--气泡" | "--video" | "--bubble" => Some(Self::Bubble),
            "--都发" | "--both" => Some(Self::Both),
            _ => None,
        }
    }

    /// 卡片上标一句这一单是怎么发的。
    fn label(self) -> &'static str {
        match self {
            Self::File => "群文件",
            Self::Bubble => "视频",
            Self::Both => "群文件 + 视频",
        }
    }
}

/// 去掉参数后剩下的正文，以及可选的时长与画面比例。
#[derive(Debug, Default, PartialEq)]
struct Options {
    prompt: String,
    seconds: Option<String>,
    size: Option<String>,
    /// 这一句自己指定的发法，没写就照配置走。
    send: Option<SendMode>,
}

/// 从提示词里剥离 `--秒数 8` / `--seconds`、`--竖屏` / `--横屏`、`--尺寸 1280x720`
/// 与 `--文件` / `--视频` / `--都发`。
fn parse_options(input: &str) -> Options {
    let mut options = Options::default();
    let mut words: Vec<&str> = Vec::new();
    let mut tokens = input.split_whitespace().peekable();

    while let Some(token) = tokens.next() {
        let lower = token.to_ascii_lowercase();
        match lower.as_str() {
            "--秒数" | "--时长" | "--seconds" | "--duration" | "-t" => match tokens
                .peek()
                .filter(|value| is_seconds(value))
            {
                Some(value) => {
                    options.seconds = Some((*value).to_string());
                    tokens.next();
                }
                None => words.push(token),
            },
            "--尺寸" | "--分辨率" | "--size" | "-s" => match tokens
                .peek()
                .filter(|value| is_size(value) || is_ratio(value))
            {
                Some(value) => {
                    options.size = Some(normalize_size(value));
                    tokens.next();
                }
                None => words.push(token),
            },
            "--竖屏" | "--portrait" | "--vertical" => options.size = Some(PORTRAIT.to_string()),
            "--横屏" | "--landscape" | "--horizontal" => options.size = Some(LANDSCAPE.to_string()),
            _ => match SendMode::flag(token) {
                Some(mode) => options.send = Some(mode),
                None => words.push(token),
            },
        }
    }

    options.prompt = words.join(" ");
    options
}

/// 合法时长：正整数秒。中转站按字符串收，但写成数字也照发。
fn is_seconds(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii_digit())
}

fn is_size(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    match lower.split_once('x') {
        Some((width, height)) => {
            !width.is_empty()
                && !height.is_empty()
                && width.chars().all(|c| c.is_ascii_digit())
                && height.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// `16:9` 这样的比例也收：站点的 OpenAI 视频格式里 `size` 两种写法都认。
fn is_ratio(value: &str) -> bool {
    match value.split_once(':') {
        Some((width, height)) => {
            !width.is_empty()
                && !height.is_empty()
                && width.chars().all(|c| c.is_ascii_digit())
                && height.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

fn normalize_size(value: &str) -> String {
    if is_ratio(value) {
        match value.trim() {
            "16:9" => LANDSCAPE.to_string(),
            "9:16" => PORTRAIT.to_string(),
            other => other.to_string(),
        }
    } else {
        value.to_ascii_lowercase()
    }
}

fn last_user(hist: &[ChatMessage]) -> Option<&ChatMessage> {
    hist.iter().rev().find(|message| message.role == "user")
}

/// 发起一次视频生成并等出结果，包装成与聊天补全一致的 `Reply`。
pub(super) async fn generate_reply(
    api_base: &str,
    api_key: &str,
    agent: &Agent,
    hist: &[ChatMessage],
    config: &super::OaiConfig,
) -> anyhow::Result<Reply> {
    let last = last_user(hist);
    // 标题写用户自己那句话，而不是拼上风格前缀后的整段（预设房间的前缀是英文，
    // 整段截 80 字只会看到一串风格描述）。
    let user_text = last
        .map(|message| message.content.trim().to_string())
        .unwrap_or_default();
    let options = parse_options(
        &match (agent.system_prompt.trim(), last.map(|m| m.content.as_str()).unwrap_or("")) {
            ("", user) => user.to_string(),
            (system, "") => system.to_string(),
            (system, user) => format!("{system}\n{user}"),
        },
    );
    let prompt = options.prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(anyhow!("请描述想拍什么，例如：拍一段雪山日出延时"));
    }

    let seconds = options
        .seconds
        .unwrap_or_else(|| config.video_seconds().to_string());
    let size = options.size.unwrap_or_else(|| LANDSCAPE.to_string());
    let generated = generate(
        api_base,
        api_key,
        &agent.model,
        &prompt,
        &seconds,
        &size,
        config.media_timeout(),
    )
    .await?;

    let caption = super::utils::truncate_str(
        &one_line(if user_text.is_empty() { &prompt } else { &user_text }),
        80,
    );
    // 完成态里 `seconds` 常常是空串，只有提交回执才带着实际时长；两处都空就写请求值。
    let shown_seconds = generated
        .seconds
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(seconds);
    // 这一句带 `--视频` / `--文件` / `--都发` 就听它的，没带才照 `[oai] video_send` 走。
    let send = options.send.unwrap_or_else(|| config.video_send());
    // 正文是纯文本（不渲染卡片），所以这里一个 markdown 记号都不能用——`**` 会原样
    // 出现在群里。第一行是用户自己那句话，第二行是时长、模型与这一单的账。
    let mut text = format!("🎬 {caption}");
    let mut meta = format!("时长：{} 秒", shown_seconds.trim());
    if !generated.model.trim().is_empty() {
        meta.push_str(&format!(" · {}", generated.model.trim()));
    }
    if generated.cost > 0.0 {
        meta.push_str(&format!(" · ${:.2}", generated.cost));
    }
    // 照配置走是常态，不必每张卡片都念一遍发法。
    if options.send.is_some() {
        meta.push_str(&format!(" · {}", send.label()));
    }
    text.push('\n');
    text.push_str(&meta);

    Ok(Reply {
        text,
        sources: Vec::new(),
        trace: Vec::new(),
        trace_overflow: 0,
        model: Some(agent.model.clone()),
        plain: true,
        media: media_messages(&generated.video_url, &caption, send),
    })
}

/// 拍好的片子按发法发出去：文件名拿用户那句话来取，跟音乐房间同一个规矩。
fn media_messages(url: &str, caption: &str, mode: SendMode) -> Vec<MediaMessage> {
    let url = url.trim();
    if url.is_empty() {
        return Vec::new();
    }
    let mut messages = Vec::new();
    if mode != SendMode::Bubble {
        messages.push(MediaMessage {
            segments: vec![Media::File {
                url: url.to_string(),
                name: format!("{}.mp4", super::utils::safe_file_name(caption)),
            }],
        });
    }
    if mode != SendMode::File {
        messages.push(MediaMessage {
            segments: vec![Media::Video {
                url: url.to_string(),
            }],
        });
    }
    messages
}

/// 一次生成的产物。
pub(crate) struct Generated {
    pub(crate) video_url: String,
    pub(crate) model: String,
    pub(crate) seconds: Option<String>,
    /// 站点给的计费数字（单位与站点标价一致）。
    pub(crate) cost: f64,
}

/// 提交一次生成并等到出片。
///
/// 与房间的回复构造解耦，供普通房间（[`generate_reply`]）与群聊搭话的拍片工具共用。
pub(crate) async fn generate(
    api_base: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    seconds: &str,
    size: &str,
    deadline: Duration,
) -> anyhow::Result<Generated> {
    let base = api_base.trim().trim_end_matches('/').to_string();
    let endpoint = format!("{base}/video/generations");
    let body = json!({
        "model": model,
        "prompt": prompt,
        "seconds": seconds,
        "size": size,
    });

    let response = crate::http::client()
        .post(&endpoint)
        .bearer_auth(api_key)
        .json(&body)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .with_context(|| format!("提交 {endpoint} 失败"))?;
    let status = response.status();
    let bytes = response.bytes().await.context("读取视频提交响应失败")?;
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let task_id = value
        .get("task_id")
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string);
    let Some(task_id) = task_id else {
        return Err(anyhow!(
            "提交视频任务失败（{}）：{}",
            status.as_u16(),
            api_error(&value, &bytes)
        ));
    };
    // 提交回执里的时长是接口实际采用的那一档（不写 `--秒数` 时它会自己定）；
    // 完成态里这个字段常常是空串，所以以提交回执为准。
    let submit_seconds = value
        .get("seconds")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty());
    info!(
        target: "Plugin/OAI/Video",
        "视频任务 {} 已提交（{}，{}秒 {}）",
        task_id,
        model,
        submit_seconds.as_deref().unwrap_or(seconds),
        size
    );

    let mut generated = poll(&base, api_key, &task_id, deadline).await?;
    if generated.seconds.is_none() {
        generated.seconds = submit_seconds;
    }
    Ok(generated)
}

#[derive(Debug, Default, Deserialize)]
struct TaskData {
    #[serde(default)]
    video_url: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    seconds: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Envelope {
    #[serde(default)]
    status: String,
    #[serde(default)]
    fail_reason: String,
    #[serde(default)]
    progress: String,
    #[serde(default)]
    cost: f64,
    #[serde(default)]
    data: Option<TaskData>,
}

async fn poll(
    base: &str,
    key: &str,
    task_id: &str,
    deadline: Duration,
) -> anyhow::Result<Generated> {
    let endpoint = format!("{base}/video/generations/{task_id}");
    let stop = tokio::time::Instant::now() + TASK_TIMEOUT.min(deadline);
    loop {
        let response = crate::http::client()
            .get(&endpoint)
            .bearer_auth(key)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .context("查询视频任务失败")?;
        let status = response.status();
        let bytes = response.bytes().await.context("读取视频任务响应失败")?;
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(anyhow!(
                "查询视频任务失败（{}）：{}",
                status.as_u16(),
                api_error(&value, &bytes)
            ));
        }
        let task: Envelope =
            serde_json::from_value(value.get("data").cloned().unwrap_or(Value::Null))
                .context("无法解析视频任务响应")?;
        match task.status.to_ascii_uppercase().as_str() {
            "SUCCESS" => {
                let inner = task.data.unwrap_or_default();
                let video_url = inner
                    .video_url
                    .or(inner.url)
                    .map(|url| url.trim().to_string())
                    .filter(|url| !url.is_empty())
                    .ok_or_else(|| anyhow!("视频任务完成了，但接口没给下载地址"))?;
                return Ok(Generated {
                    video_url,
                    model: inner.model.unwrap_or_default(),
                    seconds: inner.seconds.filter(|value| !value.trim().is_empty()),
                    cost: task.cost,
                });
            }
            "FAILURE" => {
                return Err(anyhow!(if task.fail_reason.trim().is_empty() {
                    "视频生成失败".to_string()
                } else {
                    task.fail_reason.trim().to_string()
                }));
            }
            _ if task.progress.trim() == "100%" => {
                // 有的渠道不写 status，只把进度推到 100%：当作完成再看一次 data。
                let inner = task.data.unwrap_or_default();
                if let Some(url) = inner
                    .video_url
                    .or(inner.url)
                    .filter(|url| !url.trim().is_empty())
                {
                    return Ok(Generated {
                        video_url: url.trim().to_string(),
                        model: inner.model.unwrap_or_default(),
                        seconds: inner.seconds.filter(|value| !value.trim().is_empty()),
                        cost: task.cost,
                    });
                }
            }
            _ => {}
        }
        if tokio::time::Instant::now() >= stop {
            return Err(anyhow!("等了太久还没出片（任务 {task_id}）"));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// 从错误响应里挑一句有用的话：`message` 优先，其次 `error.message`，最后原文摘录。
fn api_error(value: &Value, bytes: &[u8]) -> String {
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| value.get("error").and_then(|error| error.get("message")).and_then(Value::as_str))
        .unwrap_or_default();
    if !message.trim().is_empty() {
        return message.trim().to_string();
    }
    String::from_utf8_lossy(&bytes[..bytes.len().min(300)]).into_owned()
}

/// 折叠空白成单行，用于把多行提示词放进加粗标题。
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_configured_keywords_case_insensitively() {
        let keywords = vec!["veo3.1".to_string(), "sora-2".to_string()];
        assert!(is_video_model("veo3.1-fast", &keywords));
        assert!(is_video_model("VEO3.1-PRO-4K", &keywords));
        assert!(is_video_model("sora-2-pro", &keywords));
        assert!(!is_video_model("gpt-5.6-luna", &keywords));
        assert!(!is_video_model("gpt-image-2.5-flare", &keywords));
        assert!(!is_video_model("veo3.1-fast", &[]));
    }

    #[test]
    fn parses_and_strips_video_flags() {
        let options = parse_options("雪山日出 --秒数 8 --尺寸 1280x720");
        assert_eq!(options.prompt, "雪山日出");
        assert_eq!(options.seconds.as_deref(), Some("8"));
        assert_eq!(options.size.as_deref(), Some("1280x720"));

        let options = parse_options("猫在打字 --竖屏");
        assert_eq!(options.prompt, "猫在打字");
        assert_eq!(options.size.as_deref(), Some(PORTRAIT));

        let options = parse_options("猫在打字 --尺寸 9:16");
        assert_eq!(options.size.as_deref(), Some(PORTRAIT));
        let options = parse_options("猫在打字 --尺寸 16:9");
        assert_eq!(options.size.as_deref(), Some(LANDSCAPE));
    }

    #[test]
    fn keeps_invalid_or_valueless_flags_in_the_prompt() {
        let options = parse_options("一张画 --秒数 很久 --size");
        assert_eq!(options.prompt, "一张画 --秒数 很久 --size");
        assert!(options.seconds.is_none());
        assert!(options.size.is_none());
    }

    #[test]
    fn reads_the_send_flag_out_of_the_prompt() {
        let file = parse_options("雪山日出 --文件");
        assert_eq!(file.prompt, "雪山日出");
        assert_eq!(file.send, Some(SendMode::File));

        let bubble = parse_options("雪山日出 --视频");
        assert_eq!(bubble.prompt, "雪山日出");
        assert_eq!(bubble.send, Some(SendMode::Bubble));

        assert_eq!(parse_options("雪山日出 --都发").send, Some(SendMode::Both));
        // 没写就不替用户决定，留给 `[oai] video_send`。
        assert_eq!(parse_options("雪山日出").send, None);
    }

    #[test]
    fn send_modes_parse_and_are_labelled() {
        assert_eq!(SendMode::parse("file"), SendMode::File);
        assert_eq!(SendMode::parse("群文件"), SendMode::File);
        assert_eq!(SendMode::parse("video"), SendMode::Bubble);
        assert_eq!(SendMode::parse("气泡"), SendMode::Bubble);
        assert_eq!(SendMode::parse("both"), SendMode::Both);
        // 写错的值当成 both：宁可多发一条，也别把片子吞掉。
        assert_eq!(SendMode::parse("whatever"), SendMode::Both);
        assert_eq!(SendMode::File.label(), "群文件");
        assert_eq!(SendMode::Bubble.label(), "视频");
        assert_eq!(SendMode::Both.label(), "群文件 + 视频");
    }

    #[test]
    fn both_sends_the_file_first_then_the_bubble() {
        let messages = media_messages("https://cdn/v.mp4", "一只橘猫坐在窗台上看雨", SendMode::Both);
        assert_eq!(messages.len(), 2);
        // 文件名拿用户那句话来取，跟音乐房间同一个规矩。
        assert!(matches!(
            &messages[0].segments[0],
            Media::File { name, .. } if name == "一只橘猫坐在窗台上看雨.mp4"
        ));
        assert!(matches!(messages[1].segments[0], Media::Video { .. }));

        assert_eq!(
            media_messages("https://cdn/v.mp4", "猫", SendMode::File).len(),
            1
        );
        assert!(matches!(
            media_messages("https://cdn/v.mp4", "猫", SendMode::Bubble)[0].segments[0],
            Media::Video { .. }
        ));
        // 没有地址就什么都不发，别发出一个空消息。
        assert!(media_messages("  ", "猫", SendMode::Both).is_empty());
    }

    #[test]
    fn recognizes_seconds_and_sizes() {
        assert!(is_seconds("8"));
        assert!(!is_seconds("8s"));
        assert!(is_size("1280x720"));
        assert!(!is_size("1280"));
        assert!(is_ratio("16:9"));
        assert!(!is_ratio("16"));
    }

    /// 中转站的完成态是 `data.status = SUCCESS`，视频地址在 `data.data.video_url`。
    #[test]
    fn reads_the_video_url_out_of_a_finished_task() {
        let task: Envelope = serde_json::from_value(json!({
            "status": "SUCCESS",
            "fail_reason": "",
            "progress": "100%",
            "cost": 1.2,
            "data": {
                "id": "task-1",
                "object": "video",
                "model": "veo3.1-fast",
                "status": "completed",
                "video_url": "https://cdn.example/v.mp4"
            }
        }))
        .unwrap();
        let inner = task.data.unwrap();
        assert_eq!(inner.video_url.as_deref(), Some("https://cdn.example/v.mp4"));
        assert_eq!(inner.model.as_deref(), Some("veo3.1-fast"));
        assert!((task.cost - 1.2).abs() < f64::EPSILON);
    }

    #[test]
    fn surfaces_the_most_specific_error_message() {
        let value = json!({"error": {"message": "所有分组不支持此 API 路径"}});
        assert_eq!(api_error(&value, b""), "所有分组不支持此 API 路径");
        let value = json!({"code": "upstream_error", "message": "上游抖动"});
        assert_eq!(api_error(&value, b""), "上游抖动");
        assert_eq!(api_error(&json!({}), b"raw body"), "raw body");
    }
}
