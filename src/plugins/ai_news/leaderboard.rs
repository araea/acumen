//! AIHOT 模型榜（`/leaderboard`）抓取与解析。
//!
//! AIHOT 的公开 API v1 只覆盖资讯、热点、事件、日报与精选同步，模型榜没有对应端点
//! （`openapi-v1.json` 里没有 leaderboard，`/api/v1/leaderboard` 回 404），
//! 所以这里退而求其次读页面。取数方式仍尽量克制：
//!
//!   1. 页面在 `robots.txt` 的 `User-agent: *` 组里未被 Disallow，`llms.txt` 也把
//!      模型榜列为给 Agent 的页面；
//!   2. 只读一个榜单页（综合或某个能力分类），不遍历模型详情页；
//!   3. 结果按 `leaderboard_cache_minutes` 分榜本地缓存（默认 30 分钟），
//!      榜单每天只更新几次，指令再密也不会变成对站点的轮询；
//!   4. 沿用 `api` 模块的客户端与 UA，保持可识别、可追溯。
//!
//! 站点迁到 `aihot.news` 后，网页（不含 `/api/v1`）前面多了一层 EdgeOne 的
//! JS 校验页：先回一段设 cookie 再刷新的脚本，真正的页面要执行完才拿得到。
//! 这里不去复刻那段脚本，而是先照常请求一次（校验层撤掉时就是最省的路），
//! 拿到校验页就交给本机已经在跑的无头 Chromium 按普通访客的方式打开页面，
//! 再读它最终的 HTML——和群友点开链接是同一条路。
//!
//! 解析取的是页面里 Next.js 的 RSC 数据段而非渲染后的 DOM：数据段是结构化 JSON，
//! 比起追着 class 名解析 HTML 更不容易被样式调整打断。2026-09 改版后榜单不再
//! 附带一份 `entries` 数组，而是直接下发表格元素树（行还可能拆成 `$L` 懒引用
//! 散落在别的数据行里），所以这里先把数据行解析成 JSON、解开引用，再按表头文字
//! 认列——列的顺序变了也认得出。站点改版仍可能让解析失效——所有字段一律按可空
//! 处理，取不到就如实报错，不猜不编。

use super::api::{ApiError, client};
use crate::render::web::TabGuard;
use cdp_html_shot::Browser;
use regex::Regex;
use serde_json::{Map, Value as JsonValue};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const SITE: &str = "https://aihot.news";
pub const ATTRIBUTION: &str = "数据来源：AIHOT 模型榜 (aihot.news/leaderboard)";

/// 榜单分类：`(slug, 站点上的名字, 指令里还认的别名)`。综合榜的 slug 为 `overall`。
pub const CATEGORIES: &[(&str, &str, &[&str])] = &[
    ("overall", "综合", &["综合", "总榜", "全部", "overall"]),
    ("coding", "编程", &["编程", "代码", "coding", "code"]),
    ("reasoning", "推理", &["推理", "reasoning"]),
    ("knowledge", "知识", &["知识", "knowledge"]),
    ("professional", "专业办公", &["专业办公", "专业", "办公", "professional"]),
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
        .map(|(_, label, _)| *label)
        .unwrap_or("综合")
}

/// 榜单页地址：综合榜是 `/leaderboard`，分类榜是 `/leaderboard/category/<slug>`
pub fn page_url(category: &str) -> String {
    match category {
        "" | "overall" => format!("{}/leaderboard", SITE),
        slug => format!("{}/leaderboard/category/{}", SITE, slug),
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
    /// 证据状态代码：HIGH / MEDIUM / LOW
    pub confidence: Option<String>,
    /// 站点给证据状态的名字，如「较充分」「持续积累」「证据敏感」
    pub confidence_text: Option<String>,
    /// 证据状态的说明（站点悬停提示），含名次的情景范围
    pub confidence_note: Option<String>,
    /// 官网参考价，人民币／百万 Token，保留站点原样文字（如 `¥26.83`）
    pub cache_price: Option<String>,
    pub input_price: Option<String>,
    pub output_price: Option<String>,
    /// AIHOT 共识指数（0—100）
    pub score: Option<f64>,
}

impl ModelEntry {
    pub fn display_name(&self) -> &str {
        self.name.as_deref().map(str::trim).unwrap_or("（未知模型）")
    }

    pub fn provider_name(&self) -> Option<&str> {
        self.provider.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }

    /// 共识指数保留一位小数
    pub fn score_text(&self) -> Option<String> {
        self.score.map(|s| format!("{:.1}", s))
    }

    /// 「8 项评测」
    pub fn evaluations_text(&self) -> Option<String> {
        self.evaluations.map(|n| format!("{} 项评测", n))
    }

    /// 证据状态的中文名：优先站点原文，缺失时按代码兜底
    pub fn confidence_label(&self) -> Option<&str> {
        if let Some(text) = self.confidence_text.as_deref().map(str::trim).filter(|s| !s.is_empty())
        {
            return Some(text);
        }
        match self.confidence.as_deref()?.trim().to_ascii_uppercase().as_str() {
            "HIGH" => Some("较充分"),
            "MEDIUM" => Some("持续积累"),
            "LOW" => Some("证据敏感"),
            _ => None,
        }
    }

    /// 「证据较充分」「证据持续积累」「证据敏感」：站点的名字本身带「证据」时不再重复
    pub fn evidence_text(&self) -> Option<String> {
        let label = self.confidence_label()?;
        Some(if label.starts_with("证据") {
            label.to_string()
        } else {
            format!("证据{}", label)
        })
    }

    /// 名次的情景范围，如「1—3」；从证据说明里摘出来
    pub fn rank_range(&self) -> Option<String> {
        let note = self.confidence_note.as_deref()?;
        let caps = rank_range_re().captures(note)?;
        let from = caps.get(1)?.as_str();
        let to = caps.get(2).map_or(from, |m| m.as_str());
        Some(if from == to {
            from.to_string()
        } else {
            format!("{}—{}", from, to)
        })
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
        .filter_map(|(label, price)| price.as_deref().map(|p| format!("{} {}", label, p)))
        .collect();
        (!parts.is_empty()).then(|| parts.join(" / "))
    }

    /// 模型详情页
    pub fn page_url(&self) -> Option<String> {
        let slug = self.slug.as_deref().map(str::trim).filter(|s| !s.is_empty())?;
        Some(format!("{}/leaderboard/{}", SITE, slug))
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

        const READY: &str = "!!document.querySelector('table.lb-ranking-table tbody tr') \
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
        Ok(Err(e)) => Err(format!("用浏览器打开 AIHOT 模型榜失败：{}", e).into()),
        Err(_) => Err("用浏览器打开 AIHOT 模型榜超时（站点校验页没有放行）".into()),
    }
}

/// 仅用于测试与离线排查：清空缓存
#[cfg(test)]
pub fn clear_cache() {
    if let Ok(mut guard) = cache().lock() {
        guard.clear();
    }
}

// ================= 解析 =================

pub fn parse_board(html: &str) -> Result<Board, ApiError> {
    let payload = flight_payload(html);
    let rows = flight_rows(&payload);
    let tree = Tree { rows: &rows };

    let table = rows
        .values()
        .find_map(|root| tree.find(root, &|el| el.kind == "table" && el.has_class("lb-ranking-table")))
        .ok_or("未能从 AIHOT 模型榜页面解析出榜单数据（站点结构可能已调整）")?;

    let entries = tree.parse_table(table);
    if entries.is_empty() {
        return Err("AIHOT 模型榜当前没有条目".into());
    }

    let mut board = Board {
        entries,
        ..Default::default()
    };

    // 榜名：「综合榜 TOP 30」里 TOP 之前那截
    if let Some(heading) = rows
        .values()
        .find_map(|root| tree.find(root, &|el| el.has_class("lb-ranking-heading")))
        .and_then(|el| tree.find_all(&el, &|h| h.kind == "h2").into_iter().next())
    {
        let text = tree.text(heading);
        let name = text.split("TOP").next().unwrap_or("").trim();
        if !name.is_empty() {
            board.title = Some(name.to_string());
        }
    }

    // 「20 项评测 · 9 家机构 · 09/24 15:26 更新」
    if let Some(intro) = rows
        .values()
        .find_map(|root| tree.find(root, &|el| el.has_class("lb-board-intro")))
    {
        let text = tree.text(intro);
        board.evaluation_count = capture(eval_count_re(), &text).and_then(|s| s.parse().ok());
        board.org_count = capture(org_count_re(), &text).and_then(|s| s.parse().ok());
        board.updated_at = capture(updated_at_re(), &text);
    }

    Ok(board)
}

/// 把页面里 `self.__next_f.push([1,"..."])` 的分片拼回一整段 RSC 数据。
///
/// 分片是 JSON 字符串字面量，内容本身又是 JSON——数组可能横跨两个分片，
/// 所以先整体解码拼接，再在完整文本里找数据。
fn flight_payload(html: &str) -> String {
    const MARKER: &str = "self.__next_f.push([1,";
    let mut out = String::new();
    let mut rest = html;

    while let Some(pos) = rest.find(MARKER) {
        rest = &rest[pos + MARKER.len()..];
        let Some(quote) = rest.find('"') else { break };
        let chunk = &rest[quote..];
        let Some(end) = string_literal_end(chunk) else {
            break;
        };
        if let Ok(decoded) = serde_json::from_str::<String>(&chunk[..=end]) {
            out.push_str(&decoded);
        }
        rest = &chunk[end + 1..];
    }
    out
}

/// 给定以 `"` 开头的切片，返回该 JSON 字符串字面量结束引号的下标
fn string_literal_end(slice: &str) -> Option<usize> {
    let bytes = slice.as_bytes();
    let mut escaped = false;
    for (idx, byte) in bytes.iter().enumerate().skip(1) {
        if escaped {
            escaped = false;
            continue;
        }
        match byte {
            b'\\' => escaped = true,
            b'"' => return Some(idx),
            _ => {}
        }
    }
    None
}

/// 把 RSC 数据段拆成 `行号 → JSON`。
///
/// 每行形如 `1a:["$","div",...]`；`I[...]`（模块）与 `T<长度>,`（长文本，可能跨行）
/// 这类行不以 JSON 开头，直接跳过。一行的 JSON 解析完就停，不依赖换行切分。
fn flight_rows(payload: &str) -> HashMap<String, JsonValue> {
    let mut rows = HashMap::new();
    for caps in row_start_re().captures_iter(payload) {
        let (Some(id), Some(body)) = (caps.get(1), caps.get(2)) else {
            continue;
        };
        let mut stream =
            serde_json::Deserializer::from_str(&payload[body.start()..]).into_iter::<JsonValue>();
        if let Some(Ok(value)) = stream.next() {
            rows.insert(id.as_str().to_string(), value);
        }
    }
    rows
}

/// RSC 元素 `["$", 类型, key, props]` 的视图
struct Element<'a> {
    kind: &'a str,
    key: Option<&'a str>,
    props: &'a Map<String, JsonValue>,
}

impl<'a> Element<'a> {
    fn of(value: &'a JsonValue) -> Option<Self> {
        let arr = value.as_array()?;
        if arr.len() < 4 || arr[0].as_str() != Some("$") {
            return None;
        }
        Some(Self {
            kind: arr[1].as_str()?,
            key: arr[2].as_str(),
            props: arr[3].as_object()?,
        })
    }

    fn prop(&self, name: &str) -> Option<&'a str> {
        self.props.get(name).and_then(JsonValue::as_str)
    }

    fn has_class(&self, class: &str) -> bool {
        self.prop("className")
            .is_some_and(|c| c.split_whitespace().any(|c| c == class))
    }

    fn children(&self) -> Option<&'a JsonValue> {
        self.props.get("children")
    }
}

/// 带引用解析的元素树遍历
struct Tree<'a> {
    rows: &'a HashMap<String, JsonValue>,
}

impl<'a> Tree<'a> {
    /// `"$L27"` / `"$27"` / `"$@27"` 这类引用指向别的数据行；其余 `$` 开头的
    /// （`$undefined`、`$Sreact.fragment` 等）不是内容
    fn deref(&self, value: &'a JsonValue) -> Option<&'a JsonValue> {
        let JsonValue::String(s) = value else {
            return Some(value);
        };
        let Some(rest) = s.strip_prefix('$') else {
            return Some(value);
        };
        let id = rest
            .strip_prefix('L')
            .or_else(|| rest.strip_prefix('@'))
            .unwrap_or(rest);
        if !id.is_empty() && id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return self.rows.get(id);
        }
        None
    }

    /// 深度优先找第一个满足条件的元素
    fn find(
        &self,
        value: &'a JsonValue,
        pred: &dyn Fn(&Element<'a>) -> bool,
    ) -> Option<Element<'a>> {
        self.find_depth(value, pred, 0)
    }

    fn find_depth(
        &self,
        value: &'a JsonValue,
        pred: &dyn Fn(&Element<'a>) -> bool,
        depth: usize,
    ) -> Option<Element<'a>> {
        if depth > 200 {
            return None;
        }
        let value = self.deref(value)?;
        if let Some(el) = Element::of(value) {
            if pred(&el) {
                return Some(el);
            }
            return el
                .children()
                .and_then(|c| self.find_depth(c, pred, depth + 1));
        }
        value
            .as_array()?
            .iter()
            .find_map(|child| self.find_depth(child, pred, depth + 1))
    }

    /// 某元素下所有满足条件的元素（不再深入命中的元素内部）
    fn find_all(&self, el: &Element<'a>, pred: &dyn Fn(&Element<'a>) -> bool) -> Vec<Element<'a>> {
        let mut out = Vec::new();
        if let Some(children) = el.children() {
            self.collect(children, pred, &mut out, 0);
        }
        out
    }

    fn collect(
        &self,
        value: &'a JsonValue,
        pred: &dyn Fn(&Element<'a>) -> bool,
        out: &mut Vec<Element<'a>>,
        depth: usize,
    ) {
        if depth > 200 {
            return;
        }
        let Some(value) = self.deref(value) else {
            return;
        };
        if let Some(el) = Element::of(value) {
            if pred(&el) {
                out.push(el);
            } else if let Some(children) = el.children() {
                self.collect(children, pred, out, depth + 1);
            }
            return;
        }
        if let Some(arr) = value.as_array() {
            for child in arr {
                self.collect(child, pred, out, depth + 1);
            }
        }
    }

    /// 元素里的全部文字，按出现顺序直接拼接（站点自己在文字里带了空格）
    fn text(&self, el: Element<'a>) -> String {
        let mut out = String::new();
        if let Some(children) = el.children() {
            self.text_into(children, &mut out, 0);
        }
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn text_into(&self, value: &'a JsonValue, out: &mut String, depth: usize) {
        if depth > 200 {
            return;
        }
        match value {
            JsonValue::String(s) if s.starts_with('$') => {
                if let Some(target) = self.deref(value).filter(|t| !t.is_string()) {
                    self.text_into(target, out, depth + 1);
                }
            }
            JsonValue::String(s) => out.push_str(s),
            JsonValue::Number(n) => out.push_str(&n.to_string()),
            JsonValue::Array(arr) => match Element::of(value) {
                Some(el) => {
                    if let Some(children) = el.children() {
                        self.text_into(children, out, depth + 1);
                    }
                }
                None => arr.iter().for_each(|v| self.text_into(v, out, depth + 1)),
            },
            _ => {}
        }
    }

    /// 按表头认列，逐行取数
    fn parse_table(&self, table: Element<'a>) -> Vec<ModelEntry> {
        let headers: Vec<Column> = self
            .find_all(&table, &|el| el.kind == "th")
            .into_iter()
            .map(|th| Column::from_header(&self.text(th)))
            .collect();

        let Some(tbody) = self.find_all(&table, &|el| el.kind == "tbody").into_iter().next() else {
            return Vec::new();
        };

        self.find_all(&tbody, &|el| el.kind == "tr")
            .into_iter()
            .filter_map(|tr| {
                let cells = self.find_all(&tr, &|el| el.kind == "td");
                let mut entry = ModelEntry {
                    slug: tr.key.map(str::to_string),
                    ..Default::default()
                };
                for (idx, cell) in cells.into_iter().enumerate() {
                    let column = headers.get(idx).copied().unwrap_or(Column::Other);
                    self.fill(&mut entry, column, cell);
                }
                entry.name.is_some().then_some(entry)
            })
            .collect()
    }

    fn fill(&self, entry: &mut ModelEntry, column: Column, cell: Element<'a>) {
        let first = |kind: &'static str| self.find_all(&cell, &move |el| el.kind == kind).into_iter().next();
        match column {
            Column::Rank => entry.rank = digits(&self.text(cell)),
            Column::Model => {
                entry.name = first("strong").map(|el| self.text(el)).filter(|s| !s.is_empty());
                entry.provider = first("small").map(|el| self.text(el)).filter(|s| !s.is_empty());
                // 详情页链接里的 slug 比行 key 更可靠（可能带 `?from=coding`）
                if let Some(slug) = self
                    .find_all(&cell, &|el| el.prop("href").is_some())
                    .into_iter()
                    .next()
                    .and_then(|el| el.prop("href"))
                    .and_then(|href| href.strip_prefix("/leaderboard/"))
                    .map(|rest| rest.split(['?', '#', '/']).next().unwrap_or(""))
                    .filter(|s| !s.is_empty())
                {
                    entry.slug = Some(slug.to_string());
                }
            }
            Column::Release => {
                entry.released_at = first("time")
                    .and_then(|el| el.prop("dateTime").map(str::to_string))
                    .or_else(|| Some(self.text(cell)))
                    .filter(|s| s.chars().any(|c| c.is_ascii_digit()));
            }
            Column::Evidence => {
                entry.evaluations = first("span").and_then(|el| digits(&self.text(el)));
                if let Some(small) = first("small") {
                    entry.confidence = small.prop("data-confidence").map(str::to_string);
                    entry.confidence_note = small.prop("title").map(str::to_string);
                    entry.confidence_text = Some(self.text(small)).filter(|s| !s.is_empty());
                }
            }
            Column::CachePrice => entry.cache_price = price(&self.text(cell)),
            Column::InputPrice => entry.input_price = price(&self.text(cell)),
            Column::OutputPrice => entry.output_price = price(&self.text(cell)),
            Column::Score => {
                entry.score = first("strong")
                    .and_then(|el| self.text(el).trim().parse().ok())
                    .or_else(|| {
                        first("meter").and_then(|el| el.props.get("value").and_then(JsonValue::as_f64))
                    });
            }
            Column::Other => {}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Column {
    Rank,
    Model,
    Release,
    Evidence,
    CachePrice,
    InputPrice,
    OutputPrice,
    Score,
    Other,
}

impl Column {
    fn from_header(text: &str) -> Self {
        let has = |needle: &str| text.contains(needle);
        if has("缓存") {
            Self::CachePrice
        } else if has("输入") {
            Self::InputPrice
        } else if has("输出") {
            Self::OutputPrice
        } else if has("排名") || has("名次") {
            Self::Rank
        } else if has("模型") {
            Self::Model
        } else if has("上线") || has("发布") {
            Self::Release
        } else if has("证据") || has("评测") {
            Self::Evidence
        } else if has("共识") || has("分") || has("指数") {
            Self::Score
        } else {
            Self::Other
        }
    }
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

fn row_start_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^([0-9a-f]{1,8}):([\[{])").expect("正则字面量合法"))
}

fn eval_count_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(\d{1,4})\s*项评测").expect("正则字面量合法"))
}

fn org_count_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(\d{1,4})\s*家机构").expect("正则字面量合法"))
}

fn updated_at_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"([0-9/\-: 年月日]{4,20}?)\s*更新").expect("正则字面量合法"))
}

fn rank_range_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"名次为\s*(\d{1,3})\s*(?:[—–\-~～至到]\s*(\d{1,3}))?").expect("正则字面量合法"))
}

fn capture(re: &Regex, source: &str) -> Option<String> {
    re.captures(source)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 2026-09 改版后的真实页面同构的最小样本：
    /// 表格元素树 + 第二行拆成 `$L` 懒引用 + 数据段横跨多个分片
    fn sample_html() -> String {
        let flight_a = concat!(
            "1:\"$Sreact.fragment\"\n",
            "2:T12,多行\n文本不是 JSON\n",
            r#"1a:["$","div",null,{"className":"lb-page","children":[["$","div",null,{"className":"lb-board-intro","children":[["$","p",null,{"children":"汇集多种能力的真实评测。"}],["$","span",null,{"children":[20," 项评测 ",["$","i",null,{"children":"·"}]," ",9," 家机构 ",["$","i",null,{"children":"·"}]," ","09/24 15:26"," 更新"]}]]}],["$","section",null,{"className":"lb-ranking","children":[["$","div",null,{"className":"lb-ranking-heading","children":[["$","h2",null,{"children":["综合","榜 ",["$","span",null,{"children":["TOP ",30]}]]}]]}],["$","table",null,{"className":"lb-ranking-table","children":[["$","thead",null,{"children":["$","tr",null,{"children":[["$","th",null,{"children":"排名"}],["$","th",null,{"children":"模型"}],["$","th",null,{"children":"上线日期"}],["$","th",null,{"children":"评测证据"}],["$","th",null,{"children":["缓存价格",["$","small",null,{"children":"人民币 / 百万 Token"}]]}],["$","th",null,{"children":["输入价格",["$","small",null,{"children":"人民币 / 百万 Token"}]]}],["$","th",null,{"children":["输出价格",["$","small",null,{"children":"人民币 / 百万 Token"}]]}],["$","th",null,{"children":["共识指数"," ",["$","$L17",null,{"href":"/leaderboard/rules","children":"ⓘ"}]]}]]}]}],["$","tbody",null,{"children":[["$","tr","model-a",{"className":"lb-leading-row","children":[["$","td",null,{"className":"lb-rank-number","children":["$","span",null,{"children":"01"}]}],["$","td",null,{"className":"lb-name-cell","children":["$","$L17",null,{"href":"/leaderboard/model-a","children":[["$","span",null,{"children":[["$","strong",null,{"children":"Model A"}],["$","small",null,{"children":"Vendor"}],["$","span",null,{"className":"lb-mobile-meta","children":[["$","span",null,{"children":["上线 ","2026-09-17",""]}]]}]]}]]}]}],["$","td",null,{"className":"lb-release-cell","children":["$","time",null,{"dateTime":"2026-09-17","children":"2026-09-17"}]}],["$","td",null,{"className":"lb-evidence-cell","children":[["$","span",null,{"children":[8," 项评测"]}],["$","small",null,{"data-confidence":"MEDIUM","title":"在已完成的预设删源、权重和误差对照中，名次为 1—3。这是情景范围，不是置信区间。","children":"持续积累"}]]}],["$","td",null,{"className":"lb-price-cell","children":["$","span",null,{"children":"¥1.34"}]}],["$","td",null,{"className":"lb-price-cell","children":[["$","span",null,{"children":"¥26.83"}],null]}],["$","td",null,{"className":"lb-price-cell","children":["$","span",null,{"children":"¥134.15"}]}],["$","td",null,{"className":"lb-score-cell","children":[["$","strong",null,{"children":"94.3"}],["$","meter",null,{"min":"0","max":"100","value":94.3}]]}]]}],["$","tr","model-b",{"className":"lb-leading-row","children":[["$","td",null,{"className":"lb-rank-number","children":"$L27"}],"$L28","$L29","$L2a","$L2b","$L2c","$L2d","$L2e"]}]]}]]}]]}]]}]"#,
            "\n27:[\"$\",\"span\",null,{\"children\":\"02\"}]\n",
        );
        let flight_b = concat!(
            r#"28:["$","td",null,{"className":"lb-name-cell","children":["$","$L17",null,{"href":"/leaderboard/model-b?from=coding","children":[["$","span",null,{"children":[["$","strong",null,{"children":"Model B"}],["$","small",null,{"children":"Vendor B"}]]}]]}]}]"#,
            "\n29:[\"$\",\"td\",null,{\"children\":[\"$\",\"time\",null,{\"dateTime\":\"2026-07-24\",\"children\":\"2026-07-24\"}]}]\n",
            r#"2a:["$","td",null,{"children":[["$","span",null,{"children":[2," 项评测"]}],["$","small",null,{"data-confidence":"LOW","title":"名次为 5—12。","children":"证据敏感"}]]}]"#,
            "\n2b:[\"$\",\"td\",null,{\"children\":[\"$\",\"span\",null,{\"children\":\"—\"}]}]\n",
            "2c:[\"$\",\"td\",null,{\"children\":[\"$\",\"span\",null,{\"children\":\"¥12\"}]}]\n",
            "2d:[\"$\",\"td\",null,{\"children\":[\"$\",\"span\",null,{\"children\":\"¥36\"}]}]\n",
            "2e:[\"$\",\"td\",null,{\"children\":[[\"$\",\"strong\",null,{\"children\":\"86.2\"}]]}]\n",
        );

        // 第一段从中间切开，验证分片拼接
        let split = flight_a.len() / 2;
        let split = (split..).find(|i| flight_a.is_char_boundary(*i)).unwrap();
        format!(
            "<html><body><div>页面正文</div>\
<script>self.__next_f.push([1,{}])</script>\
<script>self.__next_f.push([1,{}])</script>\
<script>self.__next_f.push([1,{}])</script></body></html>",
            serde_json::to_string(&flight_a[..split]).unwrap(),
            serde_json::to_string(&flight_a[split..]).unwrap(),
            serde_json::to_string(flight_b).unwrap()
        )
    }

    #[test]
    fn parses_table_rows_and_board_meta() {
        let board = parse_board(&sample_html()).expect("样本应能解析");
        assert_eq!(board.entries.len(), 2);
        assert_eq!(board.title.as_deref(), Some("综合榜"));
        assert_eq!(board.evaluation_count, Some(20));
        assert_eq!(board.org_count, Some(9));
        assert_eq!(board.updated_at.as_deref(), Some("09/24 15:26"));

        let first = &board.entries[0];
        assert_eq!(first.rank, Some(1));
        assert_eq!(first.display_name(), "Model A");
        assert_eq!(first.provider_name(), Some("Vendor"));
        assert_eq!(first.released_date(), Some("2026-09-17"));
        assert_eq!(first.evaluations_text().as_deref(), Some("8 项评测"));
        assert_eq!(first.confidence_label(), Some("持续积累"));
        assert_eq!(first.evidence_text().as_deref(), Some("证据持续积累"));
        assert_eq!(first.rank_range().as_deref(), Some("1—3"));
        assert_eq!(first.score_text().as_deref(), Some("94.3"));
        assert_eq!(
            first.price_text().as_deref(),
            Some("输入 ¥26.83 / 输出 ¥134.15 / 缓存 ¥1.34")
        );
        assert_eq!(
            first.page_url().as_deref(),
            Some("https://aihot.news/leaderboard/model-a")
        );
    }

    #[test]
    fn resolves_lazy_row_references() {
        let board = parse_board(&sample_html()).unwrap();
        let second = &board.entries[1];
        assert_eq!(second.rank, Some(2));
        assert_eq!(second.display_name(), "Model B");
        // 分类榜的详情链接带 `?from=`，slug 不能把它带进来
        assert_eq!(second.slug.as_deref(), Some("model-b"));
        assert_eq!(second.released_date(), Some("2026-07-24"));
        assert_eq!(second.evaluations, Some(2));
        assert_eq!(second.confidence.as_deref(), Some("LOW"));
        assert_eq!(second.evidence_text().as_deref(), Some("证据敏感"));
        assert_eq!(second.rank_range().as_deref(), Some("5—12"));
        // 「—」是缺价，不编造
        assert_eq!(second.cache_price, None);
        assert_eq!(second.price_text().as_deref(), Some("输入 ¥12 / 输出 ¥36"));
        assert_eq!(second.score, Some(86.2));
    }

    #[test]
    fn reports_a_clear_error_when_the_page_changes() {
        let err = parse_board("<html><body>什么都没有</body></html>").unwrap_err();
        assert!(err.to_string().contains("站点结构"), "{}", err);
    }

    #[test]
    fn recognises_the_edge_challenge_page() {
        let challenge = r##"<script>function a(a){var t="";t+="EO_Bot_Ssid=";}document.cookie="__tst_status="+a(0)+"#;";</script>"##;
        assert!(is_challenge(challenge));
        assert!(!is_challenge(&sample_html()));
    }

    #[test]
    fn category_arguments_map_to_site_slugs() {
        assert_eq!(parse_category(""), Some("overall"));
        assert_eq!(parse_category("编程"), Some("coding"));
        assert_eq!(parse_category("编程榜"), Some("coding"));
        assert_eq!(parse_category("Coding"), Some("coding"));
        assert_eq!(parse_category("办公"), Some("professional"));
        assert_eq!(parse_category("绘画"), None);
        assert_eq!(page_url("overall"), "https://aihot.news/leaderboard");
        assert_eq!(
            page_url("reasoning"),
            "https://aihot.news/leaderboard/category/reasoning"
        );
    }

    /// 联网冒烟测试：确认线上页面仍能解析出榜单（校验页要走浏览器）
    ///   CHROME_BIN=$PREFIX/bin/chromium-browser \
    ///   cargo test plugins::ai_news::leaderboard::tests::live -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "需要访问 aihot.news 与本机 Chromium"]
    async fn live_page_is_parseable() {
        // 线上由 main 按 `browser_path` 先起好全局浏览器；测试里照样先起一次
        if let Ok(path) = std::env::var("CHROME_BIN") {
            let _ = Browser::instance_with_path(path).await;
        }
        for (slug, _, _) in CATEGORIES {
            let board = fetch(slug, 20).await.expect("榜单页应可访问并解析");
            assert!(!board.entries.is_empty());
            assert!(board.entries.iter().all(|e| e.score.is_some() && e.rank.is_some()));
            println!(
                "{} · {:?} 项评测 · {:?} 家机构 · {:?} 更新 · {} 个模型",
                board.title(),
                board.evaluation_count,
                board.org_count,
                board.updated_at,
                board.entries.len()
            );
        }
        let board = fetch("overall", 20).await.unwrap();
        println!(
            "{}",
            crate::plugins::ai_news::render::render_models(&board, 12).to_text()
        );

        // 顺带把真实数据的卡片 HTML 落盘，方便肉眼校版
        if let Ok(dir) = std::env::var("AI_NEWS_CARD_DUMP") {
            std::fs::create_dir_all(&dir).unwrap();
            let html = crate::plugins::ai_news::card::models_card(
                &board,
                12,
                crate::plugins::ai_news::card::CardTheme::Dark,
            );
            std::fs::write(format!("{}/models_live.html", dir), html).unwrap();
        }
        Browser::shutdown_global().await;
    }
}
