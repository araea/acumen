//! AIHOT v1 API 客户端（匿名只读，无需 API Key）。
//!
//! 之所以选 API 而不是 RSS：
//!   1. 返回结构化 JSON，字段稳定（`schemaVersion`），无需引入 XML/HTML 解析依赖；
//!   2. 支持服务端筛选（`mode` / `window` / `category` / `q` / `limit`），
//!      推送多少条、推什么分类都交给服务端，客户端不必拉全量再本地过滤；
//!   3. 每条带稳定 `id`，可做跨次推送去重，RSS 只能靠链接猜；
//!   4. 额外提供热点榜、事件详情与日报、周报、月报端点，RSS 只有几个固定 feed；
//!   5. 支持 `ETag` / `If-None-Match` 条件请求，轮询成本低。
//!
//! 契约要点（来自 AIHOT 官方 `openapi-v1.json`——2.0.0 起路径仍是 `/api/v1`，
//! 1.x 的写法原样可用——与 `llms.txt`）：
//!   - Base URL `https://aihot.news`（2026-09 从 `aihot.virxact.com` 迁来，旧域名整站 301，
//!     2026-10-31 起只做跳转），匿名只读，不带 cookie；网页前面的 EdgeOne 校验层不拦 `/api/v1`；
//!   - 时间窗仅承诺 `24h` 与 `7d`；
//!   - 分类 slug 现有六个：`ai-models` / `ai-products` / `industry` / `paper` / `tip`（教程）/
//!     `opinion`（观点）。2.0 之前观点并在 `tip` 里，现在与官网六类一一对应；
//!   - 轮询间隔以响应 `Cache-Control` 的 `s-maxage` 为下限：
//!     `/api/v1/items`、`/api/v1/hot-topics` 与 `/api/v1/stories/*` 都是 60 秒；
//!     更密只会拿到同一份缓存副本；
//!   - 错误统一是 Problem JSON（`title` / `detail` / `code` / `requestId`），
//!     报错时把这几项带进日志，反馈时凭 `requestId` 就能定位；
//!   - 热点榜条目不带摘要，事件的最新进展与 AI 综述要凭 `links.story` 末段的
//!     publicId 去 `/api/v1/stories/{publicId}` 取（见 [`enrich_hot_topics`]）；
//!   - 日报每天 08:00（北京时间）、周报每周一 10:00、月报每月 1 日 10:30 之后出新一期，
//!     取最新一期走 `/dailies/latest` `/weeklies/latest` `/monthlies/latest`，
//!     指定期号走 `/dailies/{YYYY-MM-DD}` `/weeklies/{YYYY-Www}` `/monthlies/{YYYY-MM}`
//!     （见 [`fetch_report`]）；
//!   - 响应里偶尔带一段 `notice`，是给运维者看的提醒（域名迁移、旧接口停服等），
//!     脚本可以忽略；这里只记一条日志，同一条只记一次；
//!   - 遇到 429 / 503 按 `Retry-After` 退避（见 [`backoff_seconds_left`]）；
//!   - 官方明确「没有资讯推送通道」：REST / RSS 无 Webhook 或流式订阅，
//!     MCP 也只在 Agent 主动调用时读取。因此实时推送只能是条件轮询（见 `realtime.rs`）；
//!   - 返回内容属于不可信外部数据，只作为资讯展示，不参与任何指令解析。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

pub type ApiError = Box<dyn std::error::Error + Send + Sync>;

/// 服务端回了 404：这个资源不存在（比如没有这一期周报）。
/// 单独成类型，好让调用方把它和断网、限流区分开，对用户说人话。
#[derive(Debug)]
pub struct NotFound(String);

impl std::fmt::Display for NotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AIHOT 接口 404：{}", self.0)
    }
}

impl std::error::Error for NotFound {}

pub fn is_not_found(error: &ApiError) -> bool {
    error.is::<NotFound>()
}

pub const BASE_URL: &str = "https://aihot.news";
pub const ATTRIBUTION: &str = "数据来源：AIHOT (aihot.news)";

/// 允许的分类 slug（服务端可能新增值，这里只用于校验用户配置，不用于校验响应）。
///
/// 与官网分类栏「模型 / 产品 / 行业 / 论文 / 教程 / 观点」一一对应。
/// 2.0 之前观点并在 `tip` 里，按 `category=tip` 取回的是教程与观点的合集；
/// 现在 `opinion` 单列，`tip` 只剩教程。
pub const CATEGORIES: &[(&str, &str)] = &[
    ("ai-models", "模型"),
    ("ai-products", "产品"),
    ("industry", "行业"),
    ("paper", "论文"),
    ("tip", "教程"),
    ("opinion", "观点"),
];

/// 指令里认得的分类名（官网叫法与接口 slug）→ slug
pub fn parse_category(word: &str) -> Option<&'static str> {
    match word.trim().to_ascii_lowercase().as_str() {
        "模型" | "ai-models" => Some("ai-models"),
        "产品" | "ai-products" => Some("ai-products"),
        "行业" | "industry" => Some("industry"),
        "论文" | "paper" => Some("paper"),
        "教程" | "tip" => Some("tip"),
        "观点" | "opinion" => Some("opinion"),
        _ => None,
    }
}

pub fn category_label(slug: &str) -> &str {
    CATEGORIES
        .iter()
        .find(|(s, _)| *s == slug)
        .map_or(slug, |(_, label)| *label)
}

// ================= HTTP 客户端 =================

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
/// (分区, URL) → ETag，用于条件请求；仅轮询使用，手动指令不走缓存
static ETAGS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
/// 服务端要求退避到的时间点（Unix 秒）；收到 429 / 503 时按 `Retry-After` 设置
static BACKOFF_UNTIL: AtomicI64 = AtomicI64::new(0);

/// 轮询的缓存策略。
///
/// 条件请求靠 `ETag` 省流量，但不同用途的轮询必须各记各的 ETag：
/// 实时轮询每几分钟就把最新的 ETag 刷一遍，若与定时档共用同一份，
/// 定时档到点时只会收到一个 304，永远等不到内容。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Poll {
    /// 不带条件请求，每次都取回完整响应（手动指令走这条）
    Fresh,
    /// 带 `If-None-Match`，ETag 记在给定分区下（定时档 / 实时轮询各一个分区）
    Cached(&'static str),
}

impl Poll {
    fn scope(&self) -> Option<&'static str> {
        match self {
            Poll::Fresh => None,
            Poll::Cached(scope) => Some(scope),
        }
    }
}

/// 距离服务端要求的退避结束还剩几秒（0 表示当前可以正常请求）。
///
/// 轮询任务应在发起请求前检查这里；用户指令不受影响——
/// 手动查询本来就是低频的，没必要因为一次限流把功能整个关掉。
pub fn backoff_seconds_left() -> i64 {
    (BACKOFF_UNTIL.load(Ordering::Relaxed) - Utc::now().timestamp()).max(0)
}

/// 记录一次服务端要求的退避；`Retry-After` 缺失或不是秒数时按 60 秒处理
fn note_backoff(headers: &reqwest::header::HeaderMap) -> u64 {
    let seconds = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(60)
        .clamp(1, 3600);
    BACKOFF_UNTIL.store(Utc::now().timestamp() + seconds as i64, Ordering::Relaxed);
    seconds
}

/// 全局复用一个客户端以复用连接池；超时取首次调用时的配置值，
/// 改动 `request_timeout_seconds` 需重启后生效。
pub(super) fn client(timeout_secs: u64) -> Result<&'static reqwest::Client, ApiError> {
    if let Some(c) = CLIENT.get() {
        return Ok(c);
    }
    let built = crate::http::builder()
        .timeout(Duration::from_secs(timeout_secs.clamp(3, 60)))
        .user_agent(concat!(
            "acumen-ai-news/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/araea/acumen)"
        ))
        .build()?;
    Ok(CLIENT.get_or_init(|| built))
}

fn etags() -> &'static Mutex<HashMap<String, String>> {
    ETAGS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// ETag 的存储键：同一个 URL 在不同分区下各记一份
fn etag_key(scope: &str, url: &str) -> String {
    format!("{scope}\u{1}{url}")
}

/// 发起一次 GET 请求并解析 JSON。
///
/// [`Poll::Cached`] 会携带上次的 `If-None-Match`；服务端回 304 表示内容未变化，
/// 返回 `Ok(None)`，调用方据此跳过本轮推送。
async fn get_json(
    path_and_query: &str,
    timeout_secs: u64,
    poll: Poll,
) -> Result<Option<JsonValue>, ApiError> {
    // 退避期内不再打扰服务端；用户指令（Fresh）照常放行
    let left = backoff_seconds_left();
    if left > 0 && poll.scope().is_some() {
        return Err(format!("AIHOT 要求退避，还剩 {left} 秒").into());
    }

    let url = format!("{BASE_URL}{path_and_query}");
    let mut req = client(timeout_secs)?.get(&url);

    if let Some(scope) = poll.scope()
        && let Ok(guard) = etags().lock()
        && let Some(tag) = guard.get(&etag_key(scope, &url))
    {
        req = req.header(reqwest::header::IF_NONE_MATCH, tag.clone());
    }

    let resp = req.send().await?;
    let status = resp.status();

    if status == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(Box::new(NotFound(path_and_query.to_string())));
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
    {
        let seconds = note_backoff(resp.headers());
        return Err(format!(
            "AIHOT 接口返回 {}，按 Retry-After 退避 {} 秒：{}",
            status.as_u16(),
            seconds,
            path_and_query
        )
        .into());
    }
    if !status.is_success() {
        let problem = problem_text(&resp.text().await.unwrap_or_default());
        return Err(format!(
            "AIHOT 接口返回 {}：{}{}",
            status.as_u16(),
            path_and_query,
            problem
        )
        .into());
    }

    if let Some(scope) = poll.scope()
        && let Some(tag) = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
        && let Ok(mut guard) = etags().lock()
    {
        guard.insert(etag_key(scope, &url), tag);
    }

    let value = resp.json::<JsonValue>().await?;
    note_notice(&value);
    Ok(Some(value))
}

/// 响应里的 `notice` 是给运维者的提醒（域名迁移、旧接口停服、Skill 升级），
/// 不是资讯内容。记一条日志让人看得见，同一条只记一次，免得每分钟刷一遍。
fn note_notice(value: &JsonValue) {
    static SEEN: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    let Some(notice) = value.get("notice") else {
        return;
    };
    let Some(message) = pick_str(notice, &["message"]) else {
        return;
    };
    let fresh = SEEN
        .get_or_init(Default::default)
        .lock()
        .is_ok_and(|mut seen| seen.insert(message.clone()));
    if fresh {
        warn!(
            target: super::LOG_TARGET,
            "AIHOT 通知（{}）：{}{}",
            pick_str(notice, &["type"]).unwrap_or_default(),
            message,
            pick_str(notice, &["url"]).map(|url| format!(" {url}")).unwrap_or_default()
        );
    }
}

/// 从 Problem JSON 里摘出能说明问题的几项，拼成「（…）」；不是 JSON 就返回空串
fn problem_text(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<JsonValue>(body) else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    for key in ["detail", "title", "code"] {
        if let Some(text) = value.get(key).and_then(JsonValue::as_str).map(str::trim)
            && !text.is_empty()
            && !parts.iter().any(|p| p == text)
        {
            parts.push(text.to_string());
            if key == "detail" {
                // 有 detail 就不必再重复笼统的 title
                break;
            }
        }
    }
    if let Some(id) = value.get("requestId").and_then(JsonValue::as_str) {
        parts.push(format!("requestId {id}"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("（{}）", parts.join("，"))
    }
}

// ================= 数据结构 =================

/// `id` 等字段的类型服务端未承诺一定是字符串，这里统一收敛成字符串
fn flex_string<'de, D>(de: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = JsonValue::deserialize(de)?;
    Ok(match v {
        JsonValue::String(s) if !s.is_empty() => Some(s),
        JsonValue::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Links {
    /// AIHOT 站内中文阅读页，默认主链接
    #[serde(default)]
    pub aihot: Option<String>,
    /// 第三方原文
    #[serde(default)]
    pub original: Option<String>,
    /// 事件页（仅热点榜可能有），是给人读的 HTML 页面
    #[serde(default)]
    pub story: Option<String>,
}

impl Links {
    /// 展示用主链接：优先站内页，其次原文
    pub fn primary(&self) -> Option<&str> {
        self.aihot
            .as_deref()
            .or(self.original.as_deref())
            .filter(|s| !s.is_empty())
    }

    /// 事件页的 publicId：`links.story` 的最后一段路径。官方要求只从这里取，不要自己拼
    pub fn story_id(&self) -> Option<&str> {
        let url = self.story.as_deref()?.trim();
        let path = url.split(['?', '#']).next().unwrap_or(url);
        let (prefix, id) = path.trim_end_matches('/').rsplit_once('/')?;
        (prefix.ends_with("/story") && !id.is_empty() && id.len() <= 128).then_some(id)
    }

    /// 给人点的事件页链接。服务端目前还会把它写成旧域名（整站 301 到新域名），
    /// 这里直接换成新域名，省一次跳转
    pub fn story_url(&self) -> Option<String> {
        let url = self.story.as_deref()?.trim();
        if url.is_empty() {
            return None;
        }
        Some(match url.strip_prefix("https://aihot.virxact.com") {
            Some(rest) => format!("{BASE_URL}{rest}"),
            None => url.to_string(),
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Source {
    #[serde(default)]
    pub name: Option<String>,
}

/// 一条资讯。文档承诺 `id` / `title` / `links` / `discoveredAt` 非空，
/// 但客户端仍全部按可空处理，避免服务端演进时整批解析失败。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    #[serde(default, deserialize_with = "flex_string")]
    pub id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    /// 网页「推荐理由」，未精选或无理由时为 null
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub source: Option<Source>,
    #[serde(default)]
    pub links: Links,
    /// 第三方原文发布时间
    #[serde(default)]
    pub published_at: Option<String>,
    /// AIHOT 首次收录时间
    #[serde(default)]
    pub discovered_at: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    /// AIHOT 的 AI 评分（0—100），官网卡片上的「AI 评分」
    #[serde(default)]
    pub score: Option<f64>,
}

impl Item {
    /// 去重键：优先服务端 id，缺失时退回站内链接或标题
    pub fn dedupe_key(&self) -> Option<String> {
        if let Some(id) = self.id.as_deref().filter(|s| !s.is_empty()) {
            return Some(format!("id:{id}"));
        }
        if let Some(url) = self.links.primary() {
            return Some(format!("url:{url}"));
        }
        self.title
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|t| format!("title:{t}"))
    }

    /// 是否有足够信息可以展示
    pub fn is_displayable(&self) -> bool {
        self.title.as_deref().is_some_and(|t| !t.trim().is_empty())
    }

    /// 「这条在官网时间轴上排哪儿」的时间戳（Unix 秒），口径与接口默认的 `by=timeline`
    /// 一致：补录超过 72 小时的旧文章仍按原文发布时间排，其余按 AIHOT 首次收录时间。
    ///
    /// 用收录时间而非发布时间，是因为实时推送关心的是「刚刚出现在 AIHOT 上」——
    /// 一篇昨天发布、今天才被收录的文章，对群里同样是新消息。但补录不是：
    /// 几年前的老文章被信源慢推、今天才收进来，官网把它留在原来的日期上，
    /// 群里也不该把它当成刚发生的事。缺一个时间时用另一个；两个都没有返回 `None`。
    pub fn timeline_ts(&self) -> Option<i64> {
        let parse = |s: &Option<String>| {
            s.as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.timestamp())
        };
        let discovered = parse(&self.discovered_at);
        let published = parse(&self.published_at);
        match (discovered, published) {
            (Some(d), Some(p)) if d - p > BACKFILL_SECONDS => Some(p),
            (Some(d), _) => Some(d),
            (None, p) => p,
        }
    }

    /// 谁先报的：原文发布时间优先，缺失时用收录时间。同一事件里取最早的当代表。
    pub fn first_reported_ts(&self) -> Option<i64> {
        [&self.published_at, &self.discovered_at]
            .into_iter()
            .find_map(|s| {
                s.as_deref()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.timestamp())
            })
    }

    /// 展示用的信源名：见 [`clean_source_name`]
    pub fn source_label(&self) -> Option<String> {
        self.source
            .as_ref()
            .and_then(|s| s.name.as_deref())
            .map(clean_source_name)
            .filter(|name| !name.is_empty())
    }
}

/// 收录时间比发布时间晚过这么久，就算补录（官网时间轴同一口径）
const BACKFILL_SECONDS: i64 = 72 * 3600;

/// 信源名去掉技术说明，与官网展示一致：`The Verge：AI（RSS）` → `The Verge：AI`，
/// `OpenAI：官网动态（RSS · 排除企业/客户案例）` → `OpenAI：官网动态`，
/// `X：Sam Altman (@sama)` → `X：Sam Altman`。
///
/// 去的是「怎么抓的」那一类括号——以 RSS、网页、API 打头的；
/// `X：Lydia Hallie（Anthropic，Claude Code）` 里的括号是在说这个人是谁，留着。
pub fn clean_source_name(raw: &str) -> String {
    let mut name = raw.trim();
    for (open, close) in [('（', '）'), ('(', ')')] {
        if let Some(body) = name.strip_suffix(close)
            && let Some((head, inner)) = body.rsplit_once(open)
            && ["RSS", "网页", "API"].iter().any(|tag| inner.trim().starts_with(tag))
        {
            name = head.trim_end();
        }
    }
    // 末尾的 `(@handle)`：X 信源的账号名，名字本身已经够认了
    if let Some((head, tail)) = name.rsplit_once(" (@")
        && tail.ends_with(')')
    {
        name = head.trim_end();
    }
    name.to_string()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ItemsResponse {
    #[serde(default)]
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotTopic {
    #[serde(default)]
    pub rank: Option<u32>,
    #[serde(default)]
    pub title: Option<String>,
    /// 旧版接口带的摘要；v1 1.3 起热点榜不再返回，改由 [`enrich_hot_topics`]
    /// 用事件详情的 AI 综述填上
    #[serde(default)]
    pub summary: Option<String>,
    /// 报道来源数
    #[serde(default)]
    pub source_count: Option<u32>,
    /// 讨论参与者数（编辑信源与社交信号去重合并后的独立参与者）
    #[serde(default)]
    pub participant_count: Option<u32>,
    #[serde(default)]
    pub source_names: Vec<String>,
    #[serde(default)]
    pub latest_at: Option<String>,
    #[serde(default)]
    pub links: Links,
    /// 事件的最新进展（事件详情里的 `latest`），与标题、综述不同时才展示
    #[serde(skip)]
    pub latest: Option<String>,
}

impl HotTopic {
    /// 综述的开头一段。事件综述是随演化改写的多段长文，榜单上只放得下头一段——
    /// 它总是先把最要紧的结论说完。
    pub fn digest_lead(&self) -> Option<&str> {
        self.summary
            .as_deref()
            .and_then(|text| text.split("\n\n").map(str::trim).find(|p| !p.is_empty()))
    }

    /// 去重、去掉「（RSS）」这类技术后缀后的报道来源名，与站点展示一致
    pub fn display_sources(&self, limit: usize) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for name in &self.source_names {
            let name = clean_source_name(name);
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
            if out.len() >= limit {
                break;
            }
        }
        out
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HotTopicsResponse {
    #[serde(default)]
    pub items: Vec<HotTopic>,
}

/// 定期报告的三种刊期。三种的正文结构一致——一段导语加按栏目分好的条目——
/// 所以共用一套解析、渲染与推送，差别只在端点、期号写法和出刊节奏。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportKind {
    Daily,
    Weekly,
    Monthly,
}

impl ReportKind {
    /// 指令与文案里的短名：「日报」
    pub fn noun(self) -> &'static str {
        match self {
            ReportKind::Daily => "日报",
            ReportKind::Weekly => "周报",
            ReportKind::Monthly => "月报",
        }
    }

    /// 卡片与消息的抬头：「AI 日报」
    pub fn title(self) -> &'static str {
        match self {
            ReportKind::Daily => "AI 日报",
            ReportKind::Weekly => "AI 周报",
            ReportKind::Monthly => "AI 月报",
        }
    }

    /// 卡片眉标上的英文简写
    pub fn kicker_en(self) -> &'static str {
        match self {
            ReportKind::Daily => "AI DAILY",
            ReportKind::Weekly => "AI WEEKLY",
            ReportKind::Monthly => "AI MONTHLY",
        }
    }

    /// 状态文件里记「推过哪一期」用的键
    pub fn state_key(self) -> &'static str {
        match self {
            ReportKind::Daily => "daily",
            ReportKind::Weekly => "weekly",
            ReportKind::Monthly => "monthly",
        }
    }

    fn collection(self) -> &'static str {
        match self {
            ReportKind::Daily => "dailies",
            ReportKind::Weekly => "weeklies",
            ReportKind::Monthly => "monthlies",
        }
    }

    /// 用户眼里「那一期」叫什么：日报按日期，周报按期号，月报按月份
    pub fn key_label(self) -> &'static str {
        match self {
            ReportKind::Daily => "日期",
            ReportKind::Weekly => "期号",
            ReportKind::Monthly => "月份",
        }
    }

    /// 期号的写法示例，用在用法提示里
    pub fn key_example(self) -> &'static str {
        match self {
            ReportKind::Daily => "2026-10-17",
            ReportKind::Weekly => "2026-W41",
            ReportKind::Monthly => "2026-09",
        }
    }

    /// 把用户输入的期号规整成接口认的写法；认不出返回 `None`。
    ///
    /// 只做书写上的宽容（周报的 `w` 小写、月报的 `2026-9`、分隔符写成 `/` 或 `.`），
    /// 不做「昨天」「上周」这类要本地推算日期的解释——官方明确要求期号只能来自
    /// 接口返回或用户明写，不要自己拼。
    pub fn normalize_key(self, raw: &str) -> Option<String> {
        let raw = raw.trim().replace(['/', '.'], "-");
        let (year, rest) = raw.split_once('-')?;
        if year.len() != 4 || !year.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let number = |text: &str, max: u32| {
            let n: u32 = text.parse().ok()?;
            (text.bytes().all(|b| b.is_ascii_digit()) && (1..=max).contains(&n)).then_some(n)
        };
        match self {
            ReportKind::Daily => {
                let (month, day) = rest.split_once('-')?;
                Some(format!("{year}-{:02}-{:02}", number(month, 12)?, number(day, 31)?))
            }
            ReportKind::Weekly => {
                let week = rest.strip_prefix(['W', 'w'])?;
                Some(format!("{year}-W{:02}", number(week, 53)?))
            }
            ReportKind::Monthly => Some(format!("{year}-{:02}", number(rest, 12)?)),
        }
    }

    /// 新刊多久之内还算「刚出」，超过就不再主动推送（秒）。
    ///
    /// 周报、月报的定时任务每天都会来看一眼，靠这个把推送限定在出刊后的几天里：
    /// 周一 10:00 出刊、10:40 来看，没赶上（官方晚了、Bot 刚好离线）的话，
    /// 之后几天还有机会补上，但过了窗口就不拿隔了一周的旧刊当新闻发。
    /// 日报不设限——每天只有一期，推没推过由期号记录判断。
    pub fn fresh_for(self) -> Option<i64> {
        match self {
            ReportKind::Daily => None,
            ReportKind::Weekly | ReportKind::Monthly => Some(3 * 24 * 3600),
        }
    }
}

/// 报告里的一条：日报与周报月报的条目、日报的快讯都是它。
#[derive(Debug, Clone, Default)]
pub struct ReportItem {
    pub title: String,
    /// 快讯没有摘要
    pub summary: Option<String>,
    /// 信源名，已去掉技术后缀
    pub source: Option<String>,
    /// AIHOT 站内阅读页
    pub aihot: Option<String>,
    /// 第三方原文
    pub original: Option<String>,
    /// 原文发布时间（ISO8601）；日报的条目没有
    pub published_at: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ReportSection {
    /// 栏目名，如「模型发布/更新」
    pub label: Option<String>,
    /// 栏目导语（周报、月报才有，一两句话）
    pub summary: Option<String>,
    /// 按重要程度排好的条目
    pub items: Vec<ReportItem>,
}

/// 一期日报、周报或月报。
#[derive(Debug, Clone)]
pub struct Report {
    pub kind: ReportKind,
    /// 期号：日报 `2026-10-18`、周报 `2026-W41`、月报 `2026-09`
    pub key: String,
    /// 周报、月报覆盖的起止日（`YYYY-MM-DD`，含两端）
    pub period: Option<(String, String)>,
    /// 出刊时间（Unix 秒）
    pub generated_at: Option<i64>,
    /// 头条标题：日报的 `lead.title`，周报、月报的 `headline`
    pub headline: Option<String>,
    /// 导语：日报的 `lead.leadParagraph`，周报、月报的 `overview`
    pub lead: Option<String>,
    /// 这一期在官网的阅读页
    pub link: Option<String>,
    pub sections: Vec<ReportSection>,
    /// 日报末尾的快讯（只有标题、信源与时间）
    pub flashes: Vec<ReportItem>,
}

impl Report {
    /// 没有任何可展示的内容
    pub fn is_empty(&self) -> bool {
        self.headline.is_none()
            && self.lead.is_none()
            && self.flashes.is_empty()
            && self.sections.iter().all(|s| s.items.is_empty())
    }

    /// 第几期，读起来的写法：`2026-10-18` / `2026 年第 41 周` / `2026 年 9 月`
    pub fn edition(&self) -> String {
        let key = self.key.as_str();
        match self.kind {
            ReportKind::Daily => key.to_string(),
            ReportKind::Weekly => match key.split_once("-W") {
                Some((year, week)) => format!("{year} 年第 {} 周", week.trim_start_matches('0')),
                None => key.to_string(),
            },
            ReportKind::Monthly => match key.split_once('-') {
                Some((year, month)) => format!("{year} 年 {} 月", month.trim_start_matches('0')),
                None => key.to_string(),
            },
        }
    }

    /// 周报、月报覆盖的日期范围：`10.05—10.11`
    pub fn span(&self) -> Option<String> {
        let (from, to) = self.period.as_ref()?;
        let short = |date: &str| date.get(5..).map(|md| md.replace('-', "."));
        Some(format!("{}—{}", short(from)?, short(to)?))
    }

    /// 副标题：期号加覆盖范围
    pub fn subtitle(&self) -> String {
        match self.span() {
            Some(span) => format!("{} · {}", self.edition(), span),
            None => self.edition(),
        }
    }
}

fn pick_str(v: &JsonValue, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(JsonValue::as_str) {
            let s = s.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn parse_item(v: &JsonValue) -> Option<ReportItem> {
    let title = pick_str(v, &["title"])?;
    let links = v.get("links").unwrap_or(&JsonValue::Null);
    Some(ReportItem {
        title,
        summary: pick_str(v, &["summary"]),
        source: v
            .get("source")
            .and_then(|s| pick_str(s, &["name"]))
            .map(|name| clean_source_name(&name))
            .filter(|name| !name.is_empty()),
        aihot: pick_str(links, &["aihot"]),
        original: pick_str(links, &["original"]),
        published_at: pick_str(v, &["publishedAt"]),
    })
}

fn parse_items(v: &JsonValue, key: &str) -> Vec<ReportItem> {
    v.get(key)
        .and_then(JsonValue::as_array)
        .map(|arr| arr.iter().filter_map(parse_item).collect())
        .unwrap_or_default()
}

/// 解析一期报告。
///
/// 三种刊期的外层略有不同：日报的头条与导语在 `lead` 对象里（`{title, leadParagraph}`，
/// 可能为 null），周报、月报直接给 `headline` 与 `overview`；日报有快讯，
/// 周报、月报的栏目多一句 `summary`。条目结构一样。字段缺了只影响该处的展示。
fn parse_report(kind: ReportKind, value: &JsonValue, fallback_key: &str) -> Report {
    let report = value.get("report").unwrap_or(value);
    let lead_obj = report.get("lead").unwrap_or(&JsonValue::Null);

    let key = pick_str(report, &["date", "week", "month"]).unwrap_or_else(|| fallback_key.to_string());
    let period = pick_str(report, &["periodStart"]).zip(pick_str(report, &["periodEnd"]));
    let generated_at = pick_str(report, &["generatedAt"])
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|dt| dt.timestamp());

    let sections = report
        .get("sections")
        .and_then(JsonValue::as_array)
        .map(|arr| {
            arr.iter()
                .map(|section| ReportSection {
                    label: pick_str(section, &["label"]),
                    summary: pick_str(section, &["summary"]),
                    items: parse_items(section, "items"),
                })
                .filter(|section| !section.items.is_empty())
                .collect()
        })
        .unwrap_or_default();

    Report {
        kind,
        key,
        period,
        generated_at,
        headline: pick_str(report, &["headline"]).or_else(|| pick_str(lead_obj, &["title"])),
        lead: pick_str(report, &["overview"]).or_else(|| pick_str(lead_obj, &["leadParagraph"])),
        link: report
            .get("links")
            .and_then(|links| pick_str(links, &["aihot"])),
        sections,
        flashes: parse_items(report, "flashes"),
    }
}

// ================= 端点 =================

/// 最近资讯 / 分类 / 搜索：`GET /api/v1/items`
///
/// - `mode`：`selected`（精选，默认）或 `all`（全部公开动态）
/// - `window`：仅支持 `24h` 与 `7d`
/// - `category`：可选分类 slug
/// - `query`：可选关键词（服务端搜索，2—200 字）
pub async fn fetch_items(
    mode: &str,
    window: &str,
    category: Option<&str>,
    query: Option<&str>,
    limit: u32,
    timeout_secs: u64,
    poll: Poll,
) -> Result<Option<Vec<Item>>, ApiError> {
    let mut path = format!(
        "/api/v1/items?mode={}&window={}&limit={}",
        encode(mode),
        encode(window),
        limit.clamp(1, 100)
    );
    if let Some(c) = category.filter(|c| !c.is_empty()) {
        path.push_str(&format!("&category={}", encode(c)));
    }
    if let Some(q) = query.filter(|q| !q.is_empty()) {
        path.push_str(&format!("&q={}", encode(q)));
    }

    let Some(value) = get_json(&path, timeout_secs, poll).await? else {
        return Ok(None);
    };
    let parsed: ItemsResponse = serde_json::from_value(value)?;
    Ok(Some(
        parsed
            .items
            .into_iter()
            .filter(Item::is_displayable)
            .collect(),
    ))
}

/// 当前热点榜：`GET /api/v1/hot-topics`（最多 Top 10，按 rank 升序展示）
pub async fn fetch_hot_topics(
    timeout_secs: u64,
    poll: Poll,
) -> Result<Option<Vec<HotTopic>>, ApiError> {
    let Some(value) = get_json("/api/v1/hot-topics", timeout_secs, poll).await? else {
        return Ok(None);
    };
    let parsed: HotTopicsResponse = serde_json::from_value(value)?;
    let mut items: Vec<HotTopic> = parsed
        .items
        .into_iter()
        .filter(|t| t.title.as_deref().is_some_and(|s| !s.trim().is_empty()))
        .collect();
    // 保持服务端顺序，仅在 rank 齐全时按 rank 兜底排序
    if items.iter().all(|t| t.rank.is_some()) {
        items.sort_by_key(|t| t.rank.unwrap_or(u32::MAX));
    }
    Ok(Some(items))
}

/// 事件详情里用得上的两句话
#[derive(Debug, Clone, Default)]
pub struct StoryBrief {
    /// 随事件演化增量改写的 AI 综述，可能为 null
    pub digest: Option<String>,
    /// 最新一条报道的标题
    pub latest: Option<String>,
}

/// 事件时间线上的一篇报道
#[derive(Debug, Clone, Default)]
pub struct StoryReport {
    pub item: ReportItem,
    /// 是否一手信源（官方发布）
    pub first_party: bool,
}

/// 一个事件的完整详情：`/api/v1/stories/{publicId}`
#[derive(Debug, Clone, Default)]
pub struct Story {
    pub title: String,
    /// `settled`：已平息，不再有新进展；否则仍在发展
    pub settled: bool,
    pub source_count: u32,
    pub report_count: u32,
    /// 首篇报道的时间（ISO8601）
    pub first_report_at: Option<String>,
    pub latest: Option<String>,
    pub latest_at: Option<String>,
    /// AI 综述，多段，段与段之间空一行
    pub digest: Option<String>,
    /// 事件在官网的阅读页
    pub link: Option<String>,
    /// 报道时间线，接口给的是新的在前
    pub reports: Vec<StoryReport>,
}

impl Story {
    /// 综述按段拆开
    pub fn digest_paragraphs(&self) -> Vec<&str> {
        self.digest
            .as_deref()
            .map(|text| text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()).collect())
            .unwrap_or_default()
    }

    /// 挑 `limit` 篇按时间从早到晚排好，读起来是「起因 → 发展 → 最新」。
    /// 报道多于 `limit` 时留下最早的一篇（多半是官方首发）和最近的 `limit - 1` 篇，
    /// 中间的省掉——头尾交代了来龙去脉，中段多是重复转述。
    pub fn timeline(&self, limit: usize) -> Vec<&StoryReport> {
        let limit = limit.max(2);
        let mut ordered: Vec<&StoryReport> = self.reports.iter().collect();
        ordered.sort_by_key(|r| r.item.published_at.clone());
        if ordered.len() > limit {
            let recent = ordered.split_off(ordered.len() - (limit - 1));
            ordered.truncate(1);
            ordered.extend(recent);
        }
        ordered
    }
}

/// 事件详情：`GET /api/v1/stories/{publicId}`
pub async fn fetch_story(public_id: &str, timeout_secs: u64) -> Result<Story, ApiError> {
    let path = format!("/api/v1/stories/{}", encode(public_id));
    let value = get_json(&path, timeout_secs, Poll::Fresh)
        .await?
        .ok_or("AIHOT 事件详情无响应体")?;
    let story = value.get("story").unwrap_or(&value);
    let count = |key: &str| {
        story
            .get(key)
            .and_then(JsonValue::as_u64)
            .map_or(0, |n| n.min(u32::MAX as u64) as u32)
    };
    Ok(Story {
        title: pick_str(story, &["title"]).unwrap_or_default(),
        settled: pick_str(story, &["status"]).as_deref() == Some("settled"),
        source_count: count("sourceCount"),
        report_count: count("reportCount"),
        first_report_at: pick_str(story, &["firstReportAt"]),
        latest: pick_str(story, &["latest"]),
        latest_at: pick_str(story, &["latestAt"]),
        digest: pick_str(story, &["digest"]),
        link: story.get("links").and_then(|links| pick_str(links, &["aihot"])),
        reports: story
            .get("reports")
            .and_then(JsonValue::as_array)
            .map(|reports| {
                reports
                    .iter()
                    .filter_map(|report| {
                        Some(StoryReport {
                            item: parse_item(report)?,
                            first_party: report
                                .get("source")
                                .and_then(|s| s.get("firstParty"))
                                .and_then(JsonValue::as_bool)
                                .unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// 事件详情的短期缓存：publicId → (取回时刻, 内容)。热点榜一次要查十个事件，
/// 连着查两次榜不该再打十次接口；10 分钟足够覆盖一轮讨论，又不至于让「最新进展」太旧
type StoryCache = Mutex<HashMap<String, (std::time::Instant, StoryBrief)>>;

fn story_cache() -> &'static StoryCache {
    static CACHE: OnceLock<StoryCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

const STORY_TTL: Duration = Duration::from_mins(10);

async fn story_brief(public_id: &str, timeout_secs: u64) -> Option<StoryBrief> {
    if let Ok(guard) = story_cache().lock()
        && let Some((at, brief)) = guard.get(public_id)
        && at.elapsed() < STORY_TTL
    {
        return Some(brief.clone());
    }
    match fetch_story(public_id, timeout_secs).await {
        Ok(story) => {
            let brief = StoryBrief {
                digest: story.digest,
                latest: story.latest,
            };
            if let Ok(mut guard) = story_cache().lock() {
                guard.retain(|_, (at, _)| at.elapsed() < STORY_TTL);
                guard.insert(public_id.to_string(), (std::time::Instant::now(), brief.clone()));
            }
            Some(brief)
        }
        Err(e) => {
            debug!(target: super::LOG_TARGET, "事件详情 {} 取不到，跳过：{}", public_id, e);
            None
        }
    }
}

/// 给热点榜补上事件的 AI 综述与最新进展。
///
/// 热点榜本身已不带摘要，只剩标题和计数，读起来不知道「这件事现在怎样了」。
/// 这里按 `links.story` 并发查事件详情，单条失败只少一行说明，不影响整榜。
/// 请求数最多等于榜单条数（Top 10），且有 10 分钟缓存。
pub async fn enrich_hot_topics(topics: &mut [HotTopic], timeout_secs: u64) {
    // 事件详情只是锦上添花，超时收紧一些，别让整张榜等它
    let timeout_secs = timeout_secs.clamp(3, 8);
    let briefs = futures_util::future::join_all(topics.iter().map(|topic| async move {
        match topic.links.story_id() {
            Some(id) => story_brief(id, timeout_secs).await,
            None => None,
        }
    }))
    .await;

    for (topic, brief) in topics.iter_mut().zip(briefs) {
        let Some(brief) = brief else { continue };
        let has_summary = topic
            .summary
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty());
        if !has_summary {
            topic.summary = brief.digest.clone();
        }
        // 最新进展与标题、综述重复时不再多占一行
        topic.latest = brief.latest.filter(|latest| {
            topic.title.as_deref().map(str::trim) != Some(latest.as_str())
                && topic.summary.as_deref() != Some(latest.as_str())
        });
    }
}

/// 取一期报告：`key` 为空取最新一期（`/…/latest`），否则取指定期号。
///
/// 期号按 [`ReportKind::normalize_key`] 规整后才会发出去；写法不对返回错误而不是
/// 白白请求一次。指定期号不存在时服务端回 404，错误可用 [`is_not_found`] 认出。
/// 正文一个字都没有的返回 `Ok(None)`。
pub async fn fetch_report(
    kind: ReportKind,
    key: Option<&str>,
    timeout_secs: u64,
) -> Result<Option<Report>, ApiError> {
    let (path, fallback_key) = match key {
        Some(raw) => {
            let key = kind
                .normalize_key(raw)
                .ok_or_else(|| format!("{}写法不对：{raw}（比如 {}）", kind.key_label(), kind.key_example()))?;
            (format!("/api/v1/{}/{}", kind.collection(), encode(&key)), key)
        }
        None => (format!("/api/v1/{}/latest", kind.collection()), String::new()),
    };
    let value = get_json(&path, timeout_secs, Poll::Fresh)
        .await?
        .ok_or("AIHOT 报告无响应体")?;
    let report = parse_report(kind, &value, &fallback_key);
    Ok((!report.is_empty()).then_some(report))
}

// ================= 工具 =================

/// 最小 URL 查询值编码（关键词可能含中文与空格）
fn encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
