//! AIHOT 模型榜（`/leaderboard.md`）抓取与解析。
//!
//! AIHOT 2.0.0 起，每个榜单页都有官方维护的 Markdown 版：`llms.txt` 与
//! `/api/v1/agent` 都把它列为「给 Agent」的地址——综合榜 `/leaderboard.md`，
//! 分类榜 `/leaderboard/category/<slug>.md`。它是匿名可读的纯文本表格，
//! 不经过网页的反爬校验层，结构比服务端渲染的 HTML 稳定得多。
//!
//! 取数方式仍尽量克制：
//!   1. 只读一个榜单页（综合或某个能力分类），不遍历模型详情页；
//!   2. 结果按 `leaderboard_cache_minutes` 分榜本地缓存（默认 30 分钟），
//!      榜单每天只更新几次，指令再密也不会变成对站点的轮询；
//!   3. 沿用 `api` 模块的客户端与 UA，保持可识别、可追溯。
//!
//! 2.0.0 之前这里读的是 HTML 表格：按表头文字认列、十来个正则拆格，
//! 校验页回来时还得开无头 Chromium 兜底。现在网页路径对非白名单 UA
//! 直接 403，而 `.md` 是官方给脚本的通道，故整体切换过来。
//! 所有字段一律按可空处理，取不到就如实少显示一项，不猜不编。

use super::api::{ApiError, client};
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

/// 给用户点的网页地址：综合榜 `/leaderboard`，分类榜 `/leaderboard/category/<slug>`
pub fn page_url(category: &str) -> String {
    match category {
        "" | "overall" => format!("{SITE}/leaderboard"),
        slug => format!("{SITE}/leaderboard/category/{slug}"),
    }
}

/// 抓取用的 Markdown 地址：综合榜 `/leaderboard.md`，分类榜 `…/<slug>.md`
fn markdown_url(category: &str) -> String {
    match category {
        "" | "overall" => format!("{SITE}/leaderboard.md"),
        slug => format!("{SITE}/leaderboard/category/{slug}.md"),
    }
}

// ================= 数据结构 =================

/// 榜单上的一个模型。字段全部可空：站点演进时宁可少显示一项，也不要整榜解析失败。
/// Markdown 版只给到名次、模型、厂商、评分、上线日期与输入／输出参考价；
/// `evaluations` / `coverage` / `cache_price` 是 HTML 时代的列，这里保持可空。
#[derive(Debug, Clone, Default)]
pub struct ModelEntry {
    pub rank: Option<u32>,
    pub slug: Option<String>,
    pub name: Option<String>,
    /// 厂商，如 Anthropic / OpenAI
    pub provider: Option<String>,
    /// 上线日期（`YYYY-MM-DD`）
    pub released_at: Option<String>,
    /// 参与计分的评测项数（Markdown 版不提供）
    pub evaluations: Option<u32>,
    /// 评测覆盖度（百分数，Markdown 版不提供）
    pub coverage: Option<u32>,
    /// 缓存命中参考价（Markdown 版不提供）
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
    /// 站点标注的更新时间，如「2026-11-21 20:05（北京时间）」
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

/// 同一时刻只让一个抓取在跑：几个人同时查榜时，后来的等前一个抓完直接读缓存
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

/// 抓一次榜单 Markdown 并解析
pub async fn fetch(category: &str, timeout_secs: u64) -> Result<Board, ApiError> {
    let url = markdown_url(category);
    let resp = client(timeout_secs)?
        .get(&url)
        .header(reqwest::header::ACCEPT, "text/markdown, text/plain")
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("AIHOT 模型榜返回 {}", status.as_u16()).into());
    }
    let md = resp.text().await?;
    let mut board = parse_board(&md)?;
    board.category = category.to_string();
    Ok(board)
}

// ================= 解析 =================

/// 表格的一列是什么，按表头文字认（列顺序变了也认得出）
#[derive(Clone, Copy, PartialEq, Eq)]
enum Column {
    Rank,
    Model,
    Provider,
    Release,
    Score,
    Price,
    Other,
}

impl Column {
    fn from_header(text: &str) -> Self {
        if text.contains("名次") {
            Self::Rank
        } else if text.contains("模型") {
            Self::Model
        } else if text.contains("厂商") {
            Self::Provider
        } else if text.contains("上线") {
            Self::Release
        } else if text.contains("评分") {
            Self::Score
        } else if text.contains("价格") {
            Self::Price
        } else {
            Self::Other
        }
    }
}

/// 解析一份榜单 Markdown。
///
/// 排名表在 `## 排名` 区块里，表头之后逐行取格；`## 有成绩、暂不排名`
/// 区块的行数就是 pending。其它区块（其他榜单、回答提示）不参与解析。
pub fn parse_board(md: &str) -> Result<Board, ApiError> {
    let mut board = Board::default();

    for line in md.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("- 评测：") {
            board.evaluation_count = int_before_unit(rest, "项评测");
            board.org_count = int_before_unit(rest, "家机构");
        } else if let Some(rest) = line.strip_prefix("- 更新：") {
            board.updated_at = Some(rest.trim().to_string()).filter(|s| !s.is_empty());
        }
    }

    let mut in_ranked = false;
    let mut columns: Vec<Column> = Vec::new();
    for raw in md.lines() {
        let line = raw.trim();
        if let Some(heading) = line.strip_prefix("## ") {
            in_ranked = heading.starts_with("排名");
            if in_ranked {
                columns.clear();
            }
            continue;
        }
        if !in_ranked || !line.starts_with('|') || is_separator_row(line) {
            continue;
        }
        let cells = split_row(line);
        if columns.is_empty() {
            columns = cells.iter().map(|c| Column::from_header(c)).collect();
            continue;
        }
        if let Some(entry) = parse_row(&cells, &columns) {
            board.entries.push(entry);
        }
    }
    if board.entries.is_empty() {
        return Err("AIHOT 模型榜当前没有可解析的条目（Markdown 结构可能已调整）".into());
    }

    // 「暂不排名」区块：跳过表头与分隔行，剩下的数据行就是待排名的模型数
    let mut in_pending = false;
    let mut seen_header = false;
    let mut pending = 0u32;
    for raw in md.lines() {
        let line = raw.trim();
        if let Some(heading) = line.strip_prefix("## ") {
            in_pending = heading.starts_with("有成绩、暂不排名");
            seen_header = false;
            continue;
        }
        if !in_pending || !line.starts_with('|') || is_separator_row(line) {
            continue;
        }
        if !seen_header {
            seen_header = true;
            continue;
        }
        pending += 1;
    }
    if pending > 0 {
        board.pending = Some(pending);
    }

    Ok(board)
}

/// 一行 Markdown 表格拆成格子；单元格内容里不含 `|`
fn split_row(line: &str) -> Vec<&str> {
    line.trim()
        .trim_start_matches('|')
        .trim_end_matches('|')
        .split('|')
        .map(str::trim)
        .collect()
}

/// `| --- | :---: |` 这类分隔行
fn is_separator_row(line: &str) -> bool {
    let inner = line.trim().trim_start_matches('|').trim_end_matches('|');
    !inner.trim().is_empty()
        && inner.split('|').all(|cell| {
            cell.trim()
                .trim_matches(|c: char| c == '-' || c == ':' || c.is_whitespace())
                .is_empty()
        })
}

fn parse_row(cells: &[&str], columns: &[Column]) -> Option<ModelEntry> {
    let mut entry = ModelEntry::default();
    for (cell, column) in cells.iter().zip(columns.iter().copied()) {
        let cell = cell.trim();
        match column {
            Column::Rank => entry.rank = first_int(cell),
            Column::Model => {
                // `[名称](https://aihot.news/leaderboard/<slug>)`
                if let Some(rest) = cell.strip_prefix('[')
                    && let Some((name, tail)) = rest.split_once("](")
                {
                    entry.name = Some(name.trim().to_string()).filter(|s| !s.is_empty());
                    let url = tail.trim().trim_end_matches(')');
                    entry.slug = url
                        .rsplit('/')
                        .next()
                        .map(str::to_string)
                        .filter(|s| !s.is_empty());
                }
            }
            Column::Provider => {
                entry.provider = Some(cell.to_string()).filter(|s| !s.is_empty() && s != "—");
            }
            Column::Release => {
                entry.released_at = Some(cell.to_string()).filter(|s| s.len() >= 10);
            }
            Column::Score => entry.score = cell.parse::<f64>().ok(),
            Column::Price => {
                // 「¥53.42 / ¥267.11（$8 / $40）」：丢掉括号里的美元原价，
                // 按 `/` 拆成输入价与输出价；「—」视为无价格。
                let head = cell.split('（').next().unwrap_or(cell);
                let mut parts = head.split('/').map(str::trim);
                let clean = |s: &str| {
                    Some(s.to_string()).filter(|v: &String| !v.is_empty() && v != "—")
                };
                entry.input_price = parts.next().and_then(clean);
                entry.output_price = parts.next().and_then(clean);
            }
            Column::Other => {}
        }
    }
    (entry.name.is_some() || entry.slug.is_some()).then_some(entry)
}

/// 文字里的第一个整数（「01」→ 1）
fn first_int(text: &str) -> Option<u32> {
    let start = text.find(|c: char| c.is_ascii_digit())?;
    let rest = &text[start..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// 某个单位前的整数：「50 项评测、15 家机构」取「家机构」前的 15
fn int_before_unit(text: &str, unit: &str) -> Option<u32> {
    let head = text.split_once(unit)?.0.trim_end();
    let mut start = head.len();
    let bytes = head.as_bytes();
    while start > 0 && bytes[start - 1].is_ascii_digit() {
        start -= 1;
    }
    head[start..].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r"# 大模型排行榜（2026年11月）：最新 AI 模型实测排名

2026年11月大模型排行榜，排在前面的是 Claude Fable 5.2、Gemini 4 Neon。

- 网页版：https://aihot.news/leaderboard
- 更新：2026-11-21 20:05（北京时间）
- 评测：50 项评测、15 家机构

只用第三方评测机构自己实测的成绩。

## 排名（共 94 个模型）

| 名次 | 模型 | 厂商 | AIHOT 评分 | 上线日期 | 官方 API 价格（每百万 Token，输入 / 输出） |
| --- | --- | --- | --- | --- | --- |
| 1 | [Claude Fable 5.2](https://aihot.news/leaderboard/claude-fable-5-2) | Anthropic | 79.4 | 2026-11-09 | ¥53.42 / ¥267.11（$8 / $40） |
| 2 | [Qwen4 Max](https://aihot.news/leaderboard/qwen-4-max) | Alibaba | 69.4 | 2026-11-16 | ¥10 / ¥30 |
| 3 | [Some Model](https://aihot.news/leaderboard/some-model) | — | 60.0 | — | — |

## 有成绩、暂不排名

| 模型 | 厂商 | AIHOT 评分 | 原因 |
| --- | --- | --- | --- |
| [cobalt-c](https://aihot.news/leaderboard/cobalt-c) | Google | 76.4 | 评测数量还不够 |
| [ember-1](https://aihot.news/leaderboard/ember-1) | Fireworks AI | 63.7 | 评测数量还不够 |

## 其他榜单

- 编程榜：https://aihot.news/leaderboard/category/coding （Markdown 版：https://aihot.news/leaderboard/category/coding.md ）

## 回答提示

- AIHOT 评分不是正确率。
";

    #[test]
    fn parses_current_markdown_board() {
        let mut board = parse_board(SAMPLE).unwrap();
        board.category = "overall".to_string();
        assert_eq!(board.title(), "综合榜");
        assert_eq!(board.evaluation_count, Some(50));
        assert_eq!(board.org_count, Some(15));
        assert_eq!(board.updated_at.as_deref(), Some("2026-11-21 20:05（北京时间）"));
        assert_eq!(board.pending, Some(2));
        assert_eq!(board.entries.len(), 3);

        let first = &board.entries[0];
        assert_eq!(first.rank, Some(1));
        assert_eq!(first.name.as_deref(), Some("Claude Fable 5.2"));
        assert_eq!(first.slug.as_deref(), Some("claude-fable-5-2"));
        assert_eq!(first.provider_name(), Some("Anthropic"));
        assert_eq!(first.score, Some(79.4));
        assert_eq!(first.released_date(), Some("2026-11-09"));
        assert_eq!(first.input_price.as_deref(), Some("¥53.42"));
        assert_eq!(first.output_price.as_deref(), Some("¥267.11"));
        assert_eq!(first.price_text().as_deref(), Some("输入 ¥53.42 / 输出 ¥267.11"));
        assert_eq!(first.page_url().as_deref(), Some("https://aihot.news/leaderboard/claude-fable-5-2"));

        let second = &board.entries[1];
        assert_eq!(second.input_price.as_deref(), Some("¥10"));
        assert_eq!(second.output_price.as_deref(), Some("¥30"));

        let third = &board.entries[2];
        assert_eq!(third.provider_name(), None);
        assert_eq!(third.released_date(), None);
        assert_eq!(third.price_text(), None);
        assert_eq!(third.evaluations_text(), None);
        assert_eq!(third.coverage_text(), None);
    }

    #[test]
    fn markdown_urls_cover_all_categories() {
        assert_eq!(markdown_url("overall"), "https://aihot.news/leaderboard.md");
        assert_eq!(
            markdown_url("coding"),
            "https://aihot.news/leaderboard/category/coding.md"
        );
        // 给人点的仍是网页地址
        assert_eq!(page_url("coding"), "https://aihot.news/leaderboard/category/coding");
    }

    /// 真实榜单快照钉住契约：站点把表格列或价格格式改了，这里立刻变红。
    const SNAPSHOT: &str = include_str!("fixtures/leaderboard-2026-11-21.md");

    #[test]
    fn parses_real_markdown_snapshot() {
        let mut board = parse_board(SNAPSHOT).unwrap();
        board.category = "overall".to_string();
        assert_eq!(board.entries.len(), 94);
        assert_eq!(board.evaluation_count, Some(50));
        assert_eq!(board.org_count, Some(15));
        assert_eq!(board.title(), "综合榜");
        assert!(board.pending.is_some_and(|n| n > 0));

        let first = &board.entries[0];
        assert_eq!(first.rank, Some(1));
        assert_eq!(first.name.as_deref(), Some("Claude Fable 5.2"));
        assert_eq!(first.slug.as_deref(), Some("claude-fable-5-2"));
        assert_eq!(first.provider_name(), Some("Anthropic"));
        assert_eq!(first.score, Some(79.4));
        assert_eq!(first.released_date(), Some("2026-11-09"));
        assert_eq!(first.input_price.as_deref(), Some("¥53.42"));
        assert_eq!(first.output_price.as_deref(), Some("¥267.11"));

        // 末尾几名常缺价格，要当作可空而不是整行解析失败
        let last = board.entries.last().unwrap();
        assert_eq!(last.rank, Some(94));
        assert_eq!(last.price_text(), None);
        assert!(board.entries.iter().all(|e| e.name.is_some()));
    }
}
