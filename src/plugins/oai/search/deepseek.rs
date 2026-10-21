//! DeepSeek 官方的网页搜索与抓取接口：`POST /v1/tools/search`、`POST /v1/tools/fetch`。
//!
//! 实测下来它比抓取式后端强在四处，所以排在 `auto` 链的最前：
//!
//! - **快且干净**：搜索 0.4 秒内返回结构化 JSON，不用扒 HTML；每条带发布日期、站点名、
//!   0～4 的来源权威度，以及专门写给模型读的 `summary`（比 `snippet` 长得多）。
//! - **搜索语法**：`site:` / `-site:` / `after:` / `before:` / `lang:` 由接口自己落实
//!   （见 [`super::filters`]），不像抓取式后端那样只能靠关键词碰运气。
//! - **可顺带取正文**：`need_content` 让一次搜索直接带回几篇的正文，省掉一次 `web_fetch`。
//! - **抓取出 Markdown**：`fetch` 返回保留标题、列表、表格、链接的 Markdown，而不是剥光标签的
//!   纯文本；脚本渲染的页面它也常常取得到。但它**不是万能的**：维基、知乎、HN、Reddit、
//!   PDF、`raw.githubusercontent.com` 实测直接 404，所以调用方必须留着本地抓取兜底。
//!
//! 接口约束（官方文档）：搜索与抓取各自 5 QPS（按账号，与密钥个数无关，抓取另有按目标域名的限速），
//! 超了回 429；失败与零结果不计费；每次成功调用 0.005 元。所以这里做了三件事：
//! 全进程共用一个出口节拍器（[`Pacer`]）免得多个房间同时搜索时互相撞 429；429 与瞬时 5xx
//! 按文档建议等一等重试；密钥失效或余额不足时把这份密钥暂时摘下，别让每次搜索都先白挨一次 401/402。

use super::Hit;
use crate::plugins::oai::LOG_TARGET;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tokio::time::Instant;

pub(super) const DEFAULT_BASE: &str = "https://api.deepseek.com";

/// 两次请求之间的最小间隔。官方上限是 5 QPS，留一点余量给同账号上的其他用户。
const PACE: Duration = Duration::from_millis(260);

/// 单次 429 重试的等待上限，与最多尝试几次。
const RETRY_WAIT_MAX: Duration = Duration::from_secs(4);
const ATTEMPTS: u32 = 3;

/// 响应体上限。带正文时一次几篇、单篇动辄几十万字，所以比普通搜索宽得多。
const MAX_BYTES: usize = 8 * 1024 * 1024;

/// `summary` 留给模型的字符上限。接口自称 600～800 字，实测 700～1400。
const SUMMARY_CHARS: usize = 600;

/// 全进程共用的出口节拍：每次请求占一个时间槽，槽与槽之间至少隔 [`PACE`]。
struct Pacer {
    next: tokio::sync::Mutex<Instant>,
}

impl Pacer {
    fn new() -> Self {
        Self {
            next: tokio::sync::Mutex::new(Instant::now()),
        }
    }

    async fn wait(&self) {
        let slot = {
            let mut next = self.next.lock().await;
            let slot = (*next).max(Instant::now());
            *next = slot + PACE;
            slot
        };
        tokio::time::sleep_until(slot).await;
    }
}

/// 搜索与抓取各有各的额度，节拍也各排各的。
static SEARCH_PACER: LazyLock<Pacer> = LazyLock::new(Pacer::new);
static FETCH_PACER: LazyLock<Pacer> = LazyLock::new(Pacer::new);

/// 被摘下的密钥 → 恢复时刻（unix 秒）。只放内存，重启即清；密钥本身从不写进日志。
static HALTED: LazyLock<Mutex<HashMap<String, i64>>> = LazyLock::new(Mutex::default);

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 这份密钥此刻是否被摘下（401 无效 / 402 欠费之后的一段冷静期）。
pub(super) fn halted(key: &str) -> bool {
    HALTED
        .lock()
        .map(|map| map.get(key).is_some_and(|until| *until > now()))
        .unwrap_or(false)
}

fn halt(key: &str, seconds: i64, reason: &str) {
    if let Ok(mut map) = HALTED.lock() {
        let fresh = map.get(key).is_none_or(|until| *until <= now());
        map.insert(key.to_string(), now() + seconds);
        if fresh {
            warn!(
                target: LOG_TARGET,
                "DeepSeek 搜索接口{reason}，{} 分钟内这份密钥不再尝试（搜索退回链上的下一个后端）",
                seconds / 60
            );
        }
    }
}

/// 一次调用要用到的接入点：客户端、接口根、密钥与单次请求的超时。
pub(super) struct Api<'a> {
    pub client: &'a reqwest::Client,
    pub base: &'a str,
    pub key: &'a str,
    pub timeout: Duration,
}

/// 一次 POST，带节拍、429 与瞬时故障的重试、以及按状态码的统一报错。
///
/// `retry_server` 为假时，5xx 不重试直接报错：抓取遇到打不开的站点会拖几秒再回 500，
/// 再来一遍只是多等几秒，而本地抓取兜底就在后面。
async fn post(
    api: &Api<'_>,
    path: &str,
    body: &Value,
    pacer: &Pacer,
    retry_server: bool,
) -> anyhow::Result<Value> {
    let Api { client, base, key, timeout } = *api;
    let url = format!("{}/v1/tools/{path}", base.trim_end_matches('/'));
    let mut attempt = 0;
    loop {
        attempt += 1;
        pacer.wait().await;
        let response = client
            .post(&url)
            .bearer_auth(key)
            .timeout(timeout)
            .json(body)
            .send()
            .await
            .map_err(|error| anyhow::anyhow!("请求失败：{error}"))?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let bytes = super::read_capped(response, MAX_BYTES).await?;
        if status.is_success() {
            return Ok(serde_json::from_slice(&bytes)?);
        }
        let message = error_message(&bytes);
        match status.as_u16() {
            401 => {
                halt(key, 3600, "密钥无效（401）");
                anyhow::bail!("密钥无效（401）");
            }
            402 => {
                halt(key, 1800, "余额不足（402）");
                anyhow::bail!("余额不足（402）");
            }
            429 if attempt < ATTEMPTS => {
                let wait = retry_after
                    .unwrap_or(Duration::from_millis(700) * attempt)
                    .min(RETRY_WAIT_MAX);
                tokio::time::sleep(wait).await;
            }
            408 | 500 | 502 | 503 | 504 if retry_server && attempt < 2 => {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            429 => anyhow::bail!("触发限流（429），稍后再试"),
            code => anyhow::bail!("HTTP {code}{}", if message.is_empty() { String::new() } else { format!("：{message}") }),
        }
    }
}

/// 接口的错误体是 `{"error":{"message":"… (request_id: …)"}}`；request_id 对读的人没用。
fn error_message(bytes: &[u8]) -> String {
    let value: Value = serde_json::from_slice(bytes).unwrap_or(Value::Null);
    let message = value["error"]["message"].as_str().unwrap_or("");
    message
        .split(" (request_id")
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 搜一次。`query` 已经带上了搜索语法；`count` 是向接口要的条数（去重前）。
pub(super) async fn search(
    api: &Api<'_>,
    query: &str,
    count: usize,
    question: Option<&str>,
    content: bool,
) -> anyhow::Result<Vec<Hit>> {
    let mut body = json!({
        "query": query,
        "count": count.clamp(1, 50),
        "need_content": content,
    });
    if let Some(question) = question {
        body["ori_question"] = json!(question);
    }
    let value = post(api, "search", &body, &SEARCH_PACER, true).await?;
    Ok(parse_hits(&value))
}

/// 抓一页，返回 `(标题, Markdown 正文)`。
pub(super) async fn fetch(api: &Api<'_>, url: &str) -> anyhow::Result<(String, String)> {
    let body = json!({ "url": url });
    let value = post(api, "fetch", &body, &FETCH_PACER, false).await?;
    let title = value["title"].as_str().unwrap_or("").trim().to_string();
    let text = unescape_markdown(value["content"].as_str().unwrap_or(""));
    if text.trim().is_empty() {
        anyhow::bail!("没有取到正文");
    }
    Ok((title, text))
}

fn parse_hits(value: &Value) -> Vec<Hit> {
    let mut seen = std::collections::HashSet::new();
    value["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let url = clean_url(item["url"].as_str()?.trim());
            let title = item["title"].as_str().unwrap_or("").trim().to_string();
            if url.is_empty() || !seen.insert(url_key(&url)) {
                return None;
            }
            // `summary` 是写给模型读的长摘要，`snippet` 是给界面看的短碎片；前者缺失才退回后者。
            let summary = item["summary"].as_str().unwrap_or("").trim();
            let excerpt = if summary.is_empty() {
                item["snippet"].as_str().unwrap_or("")
            } else {
                summary
            };
            Some(Hit {
                title: if title.is_empty() { url.clone() } else { title },
                url,
                snippet: squash(excerpt),
                site: item["site_name"].as_str().unwrap_or("").trim().to_string(),
                date: item["date"].as_str().map(beijing_date).unwrap_or_default(),
                // 接口用 -1 表示未知。
                authority: item["authority"]
                    .as_i64()
                    .and_then(|level| u8::try_from(level).ok()),
                low_relevance: item["rerank_score"].as_f64().is_some_and(|score| score < 0.8),
                body: item["content"].as_str().unwrap_or("").trim().to_string(),
                excerpt_chars: SUMMARY_CHARS,
            })
        })
        .collect()
}

/// 摘要里的换行是片段分隔（`\n...\n`），读起来压成一行更省 token。
fn squash(text: &str) -> String {
    static SPACE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let space = SPACE.get_or_init(|| regex::Regex::new(r"\s+").unwrap());
    space.replace_all(text.trim(), " ").into_owned()
}

/// 接口给的是 UTC 的 RFC 3339；国内页面的「当天零点」落在前一天的 16:00，
/// 直接截前十位会差一天，所以先换成北京时间再取日期。
fn beijing_date(raw: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|moment| {
            moment
                .with_timezone(&crate::clock::beijing())
                .date_naive()
                .to_string()
        })
        .unwrap_or_default()
}

/// 整理结果链接：去掉后端自己追加的 `#1`、`#1#1` 占位片段，以及只用来统计来源的追踪参数
/// （`utm_*`、`spm`、`scm`、`fbclid`……动辄两三百字符，白占 token，也让来源页脚难看）。
/// 真正的锚点与功能性参数原样保留。
fn clean_url(raw: &str) -> String {
    let url = match raw.find('#') {
        Some(position) if raw[position..].chars().all(|c| c.is_ascii_digit() || c == '#') => &raw[..position],
        _ => raw,
    };
    let Ok(mut parsed) = url::Url::parse(url) else {
        return url.to_string();
    };
    if parsed.query().is_none() {
        return url.to_string();
    }
    let kept: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(key, _)| !is_tracking(key))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    if kept.len() == parsed.query_pairs().count() {
        return url.to_string();
    }
    if kept.is_empty() {
        parsed.set_query(None);
    } else {
        parsed.query_pairs_mut().clear().extend_pairs(kept);
    }
    parsed.to_string()
}

fn is_tracking(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.starts_with("utm_")
        || matches!(
            key.as_str(),
            "spm" | "scm" | "fbclid" | "gclid" | "igshid" | "mc_cid" | "mc_eid" | "email_hash" | "trk" | "cnl" | "share_source"
        )
}

/// 去重用的键：忽略协议、`www.`、片段与末尾斜杠。
fn url_key(url: &str) -> String {
    let lower = url.to_lowercase();
    let rest = lower.split_once("://").map_or(lower.as_str(), |(_, rest)| rest);
    let rest = rest.split('#').next().unwrap_or(rest);
    rest.trim_start_matches("www.").trim_end_matches('/').to_string()
}

/// 抓取接口返回的 Markdown 把所有 ASCII 标点都加了反斜杠（`\#\#`、`\.`、`\(`），
/// 代码块里也一样，直接交给模型既占 token 又难读。还原这一层转义，并压掉连续空行。
pub(super) fn unescape_markdown(text: &str) -> String {
    static ESCAPE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static BLANKS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let escape = ESCAPE.get_or_init(|| regex::Regex::new(r"\\([!-/:-@\[-`{-~])").unwrap());
    let blanks = BLANKS.get_or_init(|| regex::Regex::new(r"\n{3,}").unwrap());
    let plain = escape.replace_all(text, "$1");
    blanks.replace_all(plain.trim(), "\n\n").into_owned()
}
