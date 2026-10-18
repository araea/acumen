//! AIHOT 模型榜（`/leaderboard`）抓取与解析。
//!
//! AIHOT 的公开 API v1 只覆盖资讯、热点、事件、日报周报月报与精选同步，模型榜没有对应端点
//! （`openapi-v1.json` 里没有 leaderboard），所以这里退而求其次读页面。取数方式仍尽量克制：
//!
//!   1. 页面在 `robots.txt` 的 `User-agent: *` 组里未被 Disallow，`llms.txt` 也把
//!      模型榜列为给 Agent 的页面；
//!   2. 只读一个榜单页（综合或某个能力分类），不遍历模型详情页；
//!   3. 结果按 `leaderboard_cache_minutes` 分榜本地缓存（默认 30 分钟），
//!      榜单每天只更新几次，指令再密也不会变成对站点的轮询；
//!   4. 沿用 `api` 模块的客户端与 UA，保持可识别、可追溯。
//!
//! 页面 2026-10 改版后是服务端直出的普通 HTML 表格（此前是 Next.js 的 RSC 数据流，
//! 要拆引用、拼分片），也不再有 EdgeOne 的 JS 校验页。所以解析简单了：按表头文字认列
//! （列的顺序变了也认得出），再逐行取格。校验页若哪天回来，`fetch` 仍会退到本机已经在跑
//! 的无头 Chromium，按普通访客的方式打开再读最终的 HTML。
//!
//! 站点改版仍可能让解析失效——所有字段一律按可空处理，取不到就如实报错，不猜不编。

use super::api::{ApiError, client};
use crate::render::web::TabGuard;
use cdp_html_shot::Browser;
use regex::Regex;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const SITE: &str = "https://aihot.news";
pub const ATTRIBUTION: &str = "数据来源：AIHOT 模型榜 (aihot.news/leaderboard)";

/// 榜单分类：`(slug, 站点上的名字, 指令里还认的别名)`。综合榜的 slug 为 `overall`。
/// `reasoning` 在站点上叫「科研」（旧名「推理」仍可用），`knowledge` 叫「知识问答」。
pub const CATEGORIES: &[(&str, &str, &[&str])] = &[
    ("overall", "综合", &["综合", "总榜", "全部", "overall"]),
    ("coding", "编程", &["编程", "代码", "coding", "code"]),
    ("reasoning", "科研", &["科研", "推理", "reasoning"]),
    ("professional", "专业办公", &["专业办公", "专业", "办公", "professional"]),
    ("knowledge", "知识问答", &["知识问答", "知识", "问答", "knowledge"]),
];

/// 把指令参数认成分类 slug；空参数是综合榜，认不出返回 None
pub fn parse_category(arg: &str) -> Option<&'static str> {
    let arg = arg.trim().trim_end_matches('榜');
    if arg.is_empty() {
        return Some("overall");
    }
    let lower = arg.to_ascii_lowercase();
    CATEGORIES
        .iter()
        .find(|(slug, _, aliases)| *slug == lower || aliases.iter().any(|a| *a == lower))
        .map(|(slug, _, _)| *slug)
}

pub fn category_label(slug: &str) -> &'static str {
    CATEGORIES
        .iter()
        .find(|(s, _, _)| *s == slug)
        .map_or("综合", |(_, label, _)| *label)
}

/// 榜单页地址：综合榜是 `/leaderboard`，分类榜是 `/leaderboard/category/<slug>`
pub fn page_url(category: &str) -> String {
    match category {
        "" | "overall" => format!("{SITE}/leaderboard"),
        slug => format!("{SITE}/leaderboard/category/{slug}"),
    }
}

// ================= 数据结构 =================

/// 榜单上的一个模型。字段全部可空：站点演进时宁可少显示一项，也不要整榜解析失败。
#[derive(Debug, Clone, Default)]
pub struct ModelEntry {
    pub rank: Option<u32>,
    pub slug: Option<String>,
    pub name: Option<String>,
    /// 厂商，如 Anthropic / OpenAI
    pub provider: Option<String>,
    /// 上线日期（`YYYY-MM-DD`）
    pub released_at: Option<String>,
    /// 参与计分的评测项数
    pub evaluations: Option<u32>,
    /// 评测覆盖度（百分数）：这个模型被各领域的评测覆盖到多少
    pub coverage: Option<u32>,
    /// 官网参考价，人民币／百万 Token，保留站点原样文字（如 `¥26.83`）
    pub cache_price: Option<String>,
    pub input_price: Option<String>,
    pub output_price: Option<String>,
    /// AIHOT 评分：50 分是参照模型的平均水平，每 15 分约一个标准差；不是正确率
    pub score: Option<f64>,
}

impl ModelEntry {
    pub fn display_name(&self) -> &str {
        self.name.as_deref().map_or("（未知模型）", str::trim)
    }

    pub fn provider_name(&self) -> Option<&str> {
        self.provider.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }

    /// 评分保留一位小数
    pub fn score_text(&self) -> Option<String> {
        self.score.map(|s| format!("{s:.1}"))
    }

    /// 「34 项评测」
    pub fn evaluations_text(&self) -> Option<String> {
        self.evaluations.map(|n| format!("{n} 项评测"))
    }

    /// 「覆盖 96%」
    pub fn coverage_text(&self) -> Option<String> {
        self.coverage.map(|n| format!("覆盖 {n}%"))
    }

    pub fn released_date(&self) -> Option<&str> {
        let raw = self.released_at.as_deref()?.trim();
        (raw.len() >= 10).then(|| &raw[..10])
    }

    /// 「输入 ¥x / 输出 ¥y / 缓存 ¥z」；全缺时返回 None（站点显示为「—」）
    pub fn price_text(&self) -> Option<String> {
        let parts: Vec<String> = [
            ("输入", &self.input_price),
            ("输出", &self.output_price),
            ("缓存", &self.cache_price),
        ]
        .into_iter()
        .filter_map(|(label, price)| price.as_deref().map(|p| format!("{label} {p}")))
        .collect();
        (!parts.is_empty()).then(|| parts.join(" / "))
    }

    /// 模型详情页
    pub fn page_url(&self) -> Option<String> {
        let slug = self.slug.as_deref().map(str::trim).filter(|s| !s.is_empty())?;
        Some(format!("{SITE}/leaderboard/{slug}"))
    }
}

/// 一次抓取的榜单快照
#[derive(Debug, Clone, Default)]
pub struct Board {
    /// 分类 slug（`overall` / `coding` / …）
    pub category: String,
    /// 站点给这张榜起的名字，如「综合榜」「编程榜」
    pub title: Option<String>,
    /// 参与计分的评测项数
    pub evaluation_count: Option<u32>,
    /// 评测机构家数
    pub org_count: Option<u32>,
    /// 站点标注的更新时间，如「09/24 15:26」
    pub updated_at: Option<String>,
    /// 「暂不排名」的模型数：已有评分，但评测还不够宽，补齐后自动给名次
    pub pending: Option<u32>,
    pub entries: Vec<ModelEntry>,
}

impl Board {
    pub fn title(&self) -> String {
        self.title
            .clone()
            .unwrap_or_else(|| format!("{}榜", category_label(&self.category)))
    }

    pub fn page_url(&self) -> String {
        page_url(&self.category)
    }
}

// ================= 抓取 =================

type Cache = Mutex<HashMap<String, (Instant, Board)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 同一时刻只让一个抓取在跑：几个人同时查榜时，后来的等前一个抓完直接读缓存，
/// 不会一起去开浏览器
fn fetch_gate() -> &'static tokio::sync::Mutex<()> {
    static GATE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn cached(category: &str, ttl: Duration) -> Option<Board> {
    let guard = cache().lock().ok()?;
    let (fetched_at, board) = guard.get(category)?;
    (fetched_at.elapsed() < ttl).then(|| board.clone())
}

/// 带本地缓存的抓取：`ttl_minutes` 内重复调用直接复用上次结果
pub async fn fetch_cached(
    category: &str,
    timeout_secs: u64,
    ttl_minutes: u64,
) -> Result<Board, ApiError> {
    let ttl = Duration::from_secs(ttl_minutes.clamp(1, 24 * 60) * 60);
    if let Some(board) = cached(category, ttl) {
        return Ok(board);
    }

    let _gate = fetch_gate().lock().await;
    if let Some(board) = cached(category, ttl) {
        return Ok(board);
    }

    let board = fetch(category, timeout_secs).await?;
    if let Ok(mut guard) = cache().lock() {
        guard.insert(category.to_string(), (Instant::now(), board.clone()));
    }
    Ok(board)
}

/// 抓一次榜单页并解析：先直接请求，碰上校验页再用浏览器打开
pub async fn fetch(category: &str, timeout_secs: u64) -> Result<Board, ApiError> {
    let url = page_url(category);
    let html = match fetch_direct(&url, timeout_secs).await? {
        Some(html) => html,
        None => fetch_with_browser(&url, timeout_secs).await?,
    };
    let mut board = parse_board(&html)?;
    board.category = category.to_string();
    Ok(board)
}

/// 直接 GET。拿到的是真页面就返回 HTML；是校验页返回 `Ok(None)`
async fn fetch_direct(url: &str, timeout_secs: u64) -> Result<Option<String>, ApiError> {
    let resp = client(timeout_secs)?
        .get(url)
        .header(reqwest::header::ACCEPT, "text/html")
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("AIHOT 模型榜返回 {}", status.as_u16()).into());
    }
    let html = resp.text().await?;
    Ok((!is_challenge(&html)).then_some(html))
}

/// EdgeOne 的 JS 校验页：一千字节上下的一段脚本，设 cookie 后刷新
fn is_challenge(html: &str) -> bool {
    !html.contains("__next_f") && (html.contains("EO_Bot_Ssid") || html.contains("__tst_status"))
}

/// 用本机的无头 Chromium 打开榜单页，等校验跑完、表格出来后取整页 HTML
async fn fetch_with_browser(url: &str, timeout_secs: u64) -> Result<String, ApiError> {
    // 校验页约 1.2 秒后自己刷新，再加上页面本身的加载，给足余量
    let budget = Duration::from_secs(timeout_secs.clamp(10, 60) + 10);
    let mut page = None;
    let result = tokio::time::timeout(budget, async {
        let browser = Browser::instance().await;
        page = Some(TabGuard::new(browser.new_tab().await?));
        let tab = page.as_ref().expect("刚放进去").tab();
        tab.goto_no_wait(url).await?;

        const READY: &str = "!!document.querySelector('table tbody tr[data-slug]') \
            && document.readyState === 'complete'";
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            // 刷新途中执行上下文会被销毁，这时的报错只说明还没到，继续等
            if let Ok(JsonValue::Bool(true)) = tab.evaluate(READY).await {
                break;
            }
        }
        let html = tab.evaluate_as_string("document.documentElement.outerHTML").await?;
        anyhow::Ok(html)
    })
    .await;

    if let Some(guard) = page {
        guard.close().await;
    }
    match result {
        Ok(Ok(html)) => Ok(html),
        Ok(Err(e)) => Err(format!("用浏览器打开 AIHOT 模型榜失败：{e}").into()),
        Err(_) => Err("用浏览器打开 AIHOT 模型榜超时（站点校验页没有放行）".into()),
    }
}
// ================= 解析 =================

/// 表格的一列是什么，按表头文字认
#[derive(Clone, Copy, PartialEq, Eq)]
enum Column {
    Rank,
    Model,
    Release,
    Coverage,
    CachePrice,
    InputPrice,
    OutputPrice,
    Score,
    Other,
}

impl Column {
    fn from_header(text: &str) -> Self {
        let has = |needle: &str| text.contains(needle);
        if has("名次") {
            Self::Rank
        } else if has("上线") {
            Self::Release
        } else if has("覆盖") {
            Self::Coverage
        } else if has("缓存") {
            Self::CachePrice
        } else if has("输入") {
            Self::InputPrice
        } else if has("输出") {
            Self::OutputPrice
        } else if has("评分") {
            Self::Score
        } else if has("模型") {
            Self::Model
        } else {
            Self::Other
        }
    }
}

pub fn parse_board(html: &str) -> Result<Board, ApiError> {
    let table = table_re()
        .find(html)
        .map(|m| m.as_str())
        .ok_or("未能从 AIHOT 模型榜页面解析出榜单数据（站点结构可能已调整）")?;

    let columns: Vec<Column> = th_re()
        .captures_iter(table)
        .map(|c| Column::from_header(&text_of(&c[1])))
        .collect();

    let entries: Vec<ModelEntry> = row_re()
        .captures_iter(table)
        .map(|row| parse_row(&row[1], &row[2], &columns))
        .collect();
    if entries.is_empty() {
        return Err("AIHOT 模型榜当前没有条目".into());
    }

    let mut board = Board {
        entries,
        ..Default::default()
    };

    // 榜名：「综合榜 30 个模型」里「个模型」前面的那截——标题 h2 的第一段文字
    board.title = capture(title_re(), html);

    // 「35 项评测 · 7 家机构 · 10/18 20:06 更新」
    if let Some(intro) = intro_re()
        .captures_iter(html)
        .map(|c| text_of(&c[1]))
        .find(|text| text.contains("项评测"))
    {
        board.evaluation_count = capture(eval_count_re(), &intro).and_then(|s| s.parse().ok());
        board.org_count = capture(org_count_re(), &intro).and_then(|s| s.parse().ok());
        board.updated_at = capture(updated_at_re(), &intro);
    }
    board.pending = capture(pending_re(), html).and_then(|s| s.parse().ok());

    Ok(board)
}

fn parse_row(slug: &str, row: &str, columns: &[Column]) -> ModelEntry {
    let mut entry = ModelEntry {
        slug: Some(slug.to_string()).filter(|s| !s.is_empty()),
        ..Default::default()
    };
    for (cell, column) in cell_re().captures_iter(row).zip(columns.iter().copied()) {
        let html = &cell[1];
        let text = text_of(html);
        match column {
            Column::Rank => entry.rank = digits(&text),
            Column::Model => {
                entry.name = capture(strong_re(), html).map(|s| text_of(&s));
                entry.provider = capture(small_re(), html).map(|s| text_of(&s));
            }
            Column::Release => {
                entry.released_at = capture(datetime_re(), html).or(Some(text).filter(|t| !t.is_empty()));
            }
            Column::Coverage => {
                entry.evaluations = capture(eval_count_re(), &text).and_then(|s| s.parse().ok());
                entry.coverage = capture(coverage_re(), &text).and_then(|s| s.parse().ok());
            }
            Column::CachePrice => entry.cache_price = price(&text),
            Column::InputPrice => entry.input_price = price(&text),
            Column::OutputPrice => entry.output_price = price(&text),
            // 评分格里还有一行「覆盖 96%」，只认大号数字那个 <strong>
            Column::Score => {
                entry.score = capture(strong_re(), html).and_then(|s| text_of(&s).parse().ok());
            }
            Column::Other => {}
        }
    }
    entry
}

/// 去标签、还原实体、折叠空白。`<!-- -->`（React 拼文本时插的分隔）一并去掉
fn text_of(html: &str) -> String {
    let stripped = tag_re().replace_all(html, "");
    let decoded = stripped
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 文字里的第一个整数（「01」→ 1，「8 项评测」→ 8）
fn digits(text: &str) -> Option<u32> {
    let start = text.find(|c: char| c.is_ascii_digit())?;
    let rest = &text[start..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// 价格格保留站点原文；没有数字（「—」「待核验」）视为缺失
fn price(text: &str) -> Option<String> {
    let text = text.trim();
    text.chars().any(|c| c.is_ascii_digit()).then(|| text.to_string())
}

fn regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("正则字面量合法"))
}

macro_rules! lazy_re {
    ($name:ident, $pattern:expr) => {
        fn $name() -> &'static Regex {
            static RE: OnceLock<Regex> = OnceLock::new();
            regex(&RE, $pattern)
        }
    };
}

lazy_re!(table_re, r"(?s)<table\b.*?</table>");
lazy_re!(th_re, r"(?s)<th\b[^>]*>(.*?)</th>");
lazy_re!(row_re, r#"(?s)<tr data-slug="([^"]*)"[^>]*>(.*?)</tr>"#);
lazy_re!(cell_re, r"(?s)<td\b[^>]*>(.*?)</td>");
lazy_re!(tag_re, r"(?s)<[^>]*>");
lazy_re!(strong_re, r"(?s)<strong\b[^>]*>(.*?)</strong>");
lazy_re!(small_re, r"(?s)<small\b[^>]*>(.*?)</small>");
lazy_re!(datetime_re, r#"dateTime="([0-9]{4}-[0-9]{2}-[0-9]{2})""#);
lazy_re!(title_re, r#"id="lb-board-title"[^>]*>([^<]+)<"#);
lazy_re!(intro_re, r#"(?s)<p class="num\b[^"]*"[^>]*>(.*?)</p>"#);
lazy_re!(pending_re, r#"(?s)id="lb-pending-title"[^>]*>[^<]*<span[^>]*>\s*(\d+)"#);
lazy_re!(eval_count_re, r"(\d{1,4})\s*项评测");
lazy_re!(org_count_re, r"(\d{1,4})\s*家机构");
lazy_re!(coverage_re, r"覆盖\s*(\d{1,3})\s*%");
lazy_re!(updated_at_re, r"([0-9/\-: 年月日]{4,20}?)\s*更新");

fn capture(re: &Regex, source: &str) -> Option<String> {
    re.captures(source)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_server_rendered_table() {
        let html = r#"
<p class="num x">35<!-- --> 项评测<span class="mx-2">·</span>7<!-- --> 家机构<span class="mx-2">·</span>10/18 20:06<!-- --> 更新</p>
<h2 id="lb-board-title" class="t">综合榜<span class="mono">30 个模型</span></h2>
<table class="w-full"><thead><tr>
<th><span>名次</span><button>名次</button></th><th>模型</th><th><button><span>上线日期</span></button></th>
<th><button><span>评测覆盖</span></button></th><th><span>缓存价格<span>人民币 / 百万 Token</span></span></th>
<th><span>输入价格</span></th><th><span>输出价格</span></th><th><button>AIHOT 评分</button></th></tr></thead>
<tbody><tr data-slug="claude-opus-5-5" class="g"><td><span class="mono">01</span></td>
<td><a href="/leaderboard/claude-opus-5-5"><strong class="x">Claude Opus 5.5</strong></a><small class="hidden">Anthropic</small><small>Anthropic<!-- --> · 9月22日上线</small></td>
<td><time dateTime="2026-09-22">2026-09-22</time></td>
<td><span>34<!-- --> 项评测</span><span>覆盖 <!-- -->96<!-- -->%</span></td>
<td>¥1.34</td><td>¥26.74</td><td>—</td>
<td><span><strong class="mono" aria-label="a">73.7</strong><small>覆盖 <!-- -->96<!-- -->%</small></span></td></tr></tbody></table>
<h2 id="lb-pending-title">暂不排名<span class="mono">5 个模型</span></h2>"#;
        let board = parse_board(html).expect("能解析");
        assert_eq!(board.title(), "综合榜");
        assert_eq!((board.evaluation_count, board.org_count), (Some(35), Some(7)));
        assert_eq!(board.updated_at.as_deref(), Some("10/18 20:06"));
        assert_eq!(board.pending, Some(5));
        let model = &board.entries[0];
        assert_eq!((model.rank, model.slug.as_deref()), (Some(1), Some("claude-opus-5-5")));
        assert_eq!((model.display_name(), model.provider_name()), ("Claude Opus 5.5", Some("Anthropic")));
        assert_eq!(model.released_date(), Some("2026-09-22"));
        assert_eq!((model.evaluations, model.coverage), (Some(34), Some(96)));
        assert_eq!(model.price_text().as_deref(), Some("输入 ¥26.74 / 缓存 ¥1.34"), "「—」不算价格");
        assert_eq!(model.score_text().as_deref(), Some("73.7"));
    }
}
