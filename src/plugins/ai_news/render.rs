//! 把 AIHOT 的接口数据渲染成适合群聊阅读的纯文本。
//!
//! 原则：
//!   - 时间统一换算为北京时间（UTC+8）后再展示；
//!   - `publishedAt` 为空时回退 `discoveredAt`，并明确标注为「收录」，不冒充原文发布时间；
//!   - `summary` / `reason` 可能为 null，判空后再展示，绝不编造；
//!   - 主链接用站内页 `links.aihot`，仅在配置开启时附第三方原文；
//!   - 保持服务端返回顺序，不按 `score` 自行重排。
//!
//! 渲染结果统一用 [`Rendered`] 表示，拆成「头 / 逐条 / 尾」三段：
//! 内容短时合成一条纯文本发送；超过阈值时按条目打包成合并转发的节点，
//! 群里只占一个折叠卡片，不会刷屏。每条正文同时保留结构化的标题与关键链接
//! （[`Rendered::links`]），供引用卡片后回复序号只回链接、不再复述图片上的正文。

use super::api::{DailyBlock, DailyReport, HotTopic, Item, category_label};
use super::cluster::Cluster;
use super::leaderboard::{self, Board};
use chrono::DateTime;
use serde::{Deserialize, Serialize};


/// 一条可提取的链接：标签 + 地址。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryLink {
    /// 展示标签，如 `AIHOT` / `原文` / `链接`
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub url: String,
}

/// 与某一条正文对应的可提取信息：标题 + 关键链接。
///
/// 引用卡片后回复序号只回这些内容，不再复述图片上的正文，避免刷屏。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryLinks {
    /// 条目标题（不含序号）
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub links: Vec<EntryLink>,
}

impl EntryLinks {
    pub fn has_link(&self) -> bool {
        self.links.iter().any(|link| !link.url.trim().is_empty())
    }
}

/// 一次渲染的产物。分段保存，以便按需要合成纯文本或拆成转发节点。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Rendered {
    /// 标题行
    pub header: String,
    /// 逐条正文，每段自身不带首尾空行
    pub entries: Vec<String>,
    /// 落款（数据来源等）
    pub footer: String,
    /// 与 `entries` 同序的标题与关键链接，供引用卡片后回复序号只回链接。
    /// 升级前落盘的旧记录没有该字段，读取时会回退到从正文里解析。
    #[serde(default)]
    pub links: Vec<EntryLinks>,
}

impl Rendered {
    /// 取某一条的标题与链接：优先用渲染时留下的结构化数据；
    /// 旧记录（升级前落盘、`links` 为空）则从条目正文里解析。
    pub fn entry_links(&self, index: usize) -> EntryLinks {
        if let Some(links) = self.links.get(index)
            && (!links.title.is_empty() || links.has_link())
        {
            return links.clone();
        }
        self.entries
            .get(index)
            .map(|entry| parse_entry_links(entry))
            .unwrap_or_default()
    }

    /// 这批内容是否带有可提取的链接（决定提取走链接视图还是正文视图）
    pub fn has_links(&self) -> bool {
        (0..self.entries.len()).any(|index| self.entry_links(index).has_link())
    }

    /// 纯提示文本（错误、空结果等），永远按单条纯文本发送
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            header: text.into(),
            entries: Vec::new(),
            footer: String::new(),
            links: Vec::new(),
        }
    }

    /// 合成单条纯文本
    pub fn to_text(&self) -> String {
        if self.entries.is_empty() {
            return self.header.clone();
        }
        // 条目之间、正文与页脚之间各留一个空行——分组靠空行与序号，
        // 不再用横线（分隔线不用）。
        let mut out = String::with_capacity(self.char_count() * 3);
        out.push_str(&self.header);
        for entry in &self.entries {
            out.push('\n');
            out.push('\n');
            out.push_str(entry);
        }
        if !self.footer.is_empty() {
            out.push('\n');
            out.push('\n');
            out.push_str(&self.footer);
        }
        out
    }

    /// 按字符数估算整条消息的体量（判断是否该转成合并转发）
    pub fn char_count(&self) -> usize {
        self.header.chars().count()
            + self.footer.chars().count()
            + self
                .entries
                .iter()
                .map(|e| e.chars().count() + 2)
                .sum::<usize>()
    }

    /// 把条目贪心打包成若干节点正文：标题并入首个节点，落款单独成节。
    ///
    /// `node_max_chars` 只是软上限——单条超长的条目不会被切断，
    /// 宁可让某个节点长一点，也不在句子中间断开。
    pub fn nodes(&self, node_max_chars: usize) -> Vec<String> {
        if self.entries.is_empty() {
            return vec![self.to_text()];
        }

        let budget = node_max_chars.max(60);
        let mut nodes: Vec<String> = Vec::new();
        let mut current = String::new();

        if !self.header.is_empty() {
            current.push_str(&self.header);
        }

        for entry in &self.entries {
            let entry = entry.trim_end();
            if entry.is_empty() {
                continue;
            }
            let would_be = current.chars().count() + entry.chars().count();
            if !current.is_empty() && would_be > budget {
                nodes.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(entry);
        }

        if !current.is_empty() {
            nodes.push(current);
        }
        if !self.footer.is_empty() {
            nodes.push(self.footer.clone());
        }
        nodes
    }
}

/// ISO8601 → `MM-DD HH:MM`（北京时间）
pub(super) fn fmt_time(iso: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|dt| dt.with_timezone(&crate::clock::beijing()).format("%m-%d %H:%M").to_string())
}

/// 按字符（而非字节）截断，避免切坏中文
pub(super) fn truncate(text: &str, max_chars: usize) -> String {
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.chars().count() <= max_chars {
        return cleaned;
    }
    let head: String = cleaned.chars().take(max_chars).collect();
    format!("{}…", head.trim_end())
}

/// 从渲染好的条目正文里还原标题与链接。
///
/// 条目由本模块生成，格式稳定：首行是 `序号. 标题` 或 `第 N 名 标题`，
/// 之后是缩进正文，链接行以 `🔗 ` / `📄 ` 开头。仅用于升级前的旧记录，
/// 新记录直接读结构化的 [`Rendered::links`]。
fn parse_entry_links(entry: &str) -> EntryLinks {
    let mut title = String::new();
    let mut links: Vec<EntryLink> = Vec::new();

    for (idx, raw) in entry.lines().enumerate() {
        let line = raw.trim();
        if idx == 0 {
            title = strip_entry_prefix(line).to_string();
            continue;
        }
        let (label, url) = if let Some(rest) = line.strip_prefix("🔗 ") {
            ("AIHOT", rest.trim())
        } else if let Some(rest) = line.strip_prefix("📄 ") {
            ("原文", rest.trim())
        } else {
            continue;
        };
        if url.is_empty() || links.iter().any(|link| link.url == url) {
            continue;
        }
        links.push(EntryLink {
            label: label.to_string(),
            url: url.to_string(),
        });
    }

    EntryLinks { title, links }
}

/// 去掉 `1. ` / `第 3 名 ` 这类行首序号
fn strip_entry_prefix(line: &str) -> &str {
    if let Some((head, rest)) = line.split_once(". ")
        && head.chars().all(|ch| ch.is_ascii_digit())
    {
        return rest.trim();
    }
    if let Some(rest) = line.strip_prefix("第 ")
        && let Some((_, rest)) = rest.split_once('名')
    {
        return rest.trim();
    }
    line.trim()
}

/// 一条资讯的时间行：来源 · 时间（无法取得原文时间时标注为收录时间）
fn meta_line(item: &Item) -> Option<String> {
    let source = item.source_label();

    let time = item
        .published_at
        .as_deref()
        .and_then(fmt_time)
        .map(|t| format!("{t} 发布"))
        .or_else(|| {
            item.discovered_at
                .as_deref()
                .and_then(fmt_time)
                .map(|t| format!("{t} 收录"))
        });

    let category = item.category.as_deref().map(category_label);

    let parts: Vec<String> = [source, category.map(str::to_string), time]
        .into_iter()
        .flatten()
        .collect();

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

pub struct RenderOptions {
    pub summary_max_chars: usize,
    pub show_reason: bool,
    pub show_original_link: bool,
}

/// 资讯列表（速递 / 搜索结果共用）。一条是一个事件：代表报道的正文，
/// 后面跟「另有 N 家信源报道」，那几家的链接也随提取一并给出。
pub fn render_items(header: &str, clusters: &[Cluster], opts: &RenderOptions) -> Rendered {
    let mut entries = Vec::with_capacity(clusters.len());
    let mut links = Vec::with_capacity(clusters.len());

    for (idx, cluster) in clusters.iter().enumerate() {
        let item = &cluster.lead;
        let mut out = String::new();
        let title = item.title.as_deref().unwrap_or("（无标题）").trim();
        let mut entry_links = EntryLinks {
            title: title.to_string(),
            links: Vec::new(),
        };
        out.push_str(&format!("{}. {}", idx + 1, title));

        if let Some(meta) = meta_line(item) {
            out.push_str(&format!("\n   {meta}"));
        }
        if let Some(summary) = item.summary.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            out.push_str(&format!("\n   {}", truncate(summary, opts.summary_max_chars)));
        }
        let others = cluster.other_reports();
        if !others.is_empty() {
            out.push_str(&format!("\n   {}", also_line(&others)));
        }
        if opts.show_reason
            && let Some(reason) = item.reason.as_deref().map(str::trim).filter(|s| !s.is_empty())
        {
            out.push_str(&format!("\n   💡 {}", truncate(reason, opts.summary_max_chars)));
        }
        if let Some(link) = item.links.aihot.as_deref().filter(|s| !s.is_empty()) {
            out.push_str(&format!("\n   🔗 {link}"));
            entry_links.links.push(EntryLink {
                label: "AIHOT".to_string(),
                url: link.to_string(),
            });
        }
        if opts.show_original_link
            && let Some(orig) = item.links.original.as_deref().filter(|s| !s.is_empty())
        {
            out.push_str(&format!("\n   📄 {orig}"));
            entry_links.links.push(EntryLink {
                label: "原文".to_string(),
                url: orig.to_string(),
            });
        }
        // 其它信源只进提取的链接，不进正文：正文里已经写了它们的名字，再列一串网址会把这一条撑得很长
        for (name, other) in &others {
            if let Some(url) = other.links.primary() {
                entry_links.links.push(EntryLink {
                    label: name.clone(),
                    url: url.to_string(),
                });
            }
        }
        entries.push(out);
        links.push(entry_links);
    }

    Rendered {
        header: header.to_string(),
        entries,
        footer: format!("{} · 共 {} 条", super::api::ATTRIBUTION, clusters.len()),
        links,
    }
}

/// 「另有 3 家信源报道：A、B、C」；名字最多列四家，其余写「等」
pub(super) fn also_line(others: &[(String, &Item)]) -> String {
    const SHOWN: usize = 4;
    let names: Vec<&str> = others.iter().take(SHOWN).map(|(name, _)| name.as_str()).collect();
    let etc = if others.len() > SHOWN { " 等" } else { "" };
    format!("另有 {} 家信源报道：{}{}", others.len(), names.join("、"), etc)
}

/// 热点榜：按 rank 展示「第 N 名」，不展示或推算热度值
pub fn render_hot_topics(topics: &[HotTopic]) -> Rendered {
    let mut entries = Vec::with_capacity(topics.len());
    let mut links = Vec::with_capacity(topics.len());

    for (idx, topic) in topics.iter().enumerate() {
        let mut out = String::new();
        let rank = topic.rank.unwrap_or((idx + 1) as u32);
        let title = topic.title.as_deref().unwrap_or("（无标题）").trim();
        let mut entry_links = EntryLinks {
            title: title.to_string(),
            links: Vec::new(),
        };
        out.push_str(&format!("第 {rank} 名 {title}"));

        let mut meta: Vec<String> = Vec::new();
        if let Some(count) = topic.source_count.filter(|c| *c > 0) {
            meta.push(format!("{count} 个报道来源"));
        }
        if let Some(count) = topic.participant_count.filter(|c| *c > 0) {
            meta.push(format!("{count} 人讨论"));
        }
        let names = topic.display_sources(3);
        if !names.is_empty() {
            meta.push(names.join("、"));
        }
        if let Some(t) = topic.latest_at.as_deref().and_then(fmt_time) {
            meta.push(format!("最新 {t}"));
        }
        if !meta.is_empty() {
            out.push_str(&format!("\n   {}", meta.join(" · ")));
        }

        if let Some(summary) = topic.summary.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            out.push_str(&format!("\n   {}", truncate(summary, 80)));
        }
        if let Some(latest) = topic.latest.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            out.push_str(&format!("\n   最新进展：{}", truncate(latest, 80)));
        }
        // 事件页汇总了全部报道与时间线，比单篇报道更适合当热点的主链接
        if let Some(story) = topic.links.story_url() {
            out.push_str(&format!("\n   🔗 {story}"));
            entry_links.links.push(EntryLink {
                label: "事件页".to_string(),
                url: story,
            });
        }
        if let Some(link) = topic.links.primary() {
            if entry_links.links.is_empty() {
                out.push_str(&format!("\n   🔗 {link}"));
            }
            entry_links.links.push(EntryLink {
                label: "AIHOT".to_string(),
                url: link.to_string(),
            });
        }
        entries.push(out);
        links.push(entry_links);
    }

    Rendered {
        header: "AI 当前热点榜".to_string(),
        entries,
        footer: super::api::ATTRIBUTION.to_string(),
        links,
    }
}

/// 模型榜：一条一段，先名次与模型名，再共识指数与证据情况，最后上线日期与价格
pub fn render_models(board: &Board, max_items: usize) -> Rendered {
    let shown = &board.entries[..board.entries.len().min(max_items.max(1))];
    let mut entries = Vec::with_capacity(shown.len());
    let mut links = Vec::with_capacity(shown.len());

    for (idx, model) in shown.iter().enumerate() {
        let rank = model.rank.unwrap_or((idx + 1) as u32);
        let mut out = match model.provider_name() {
            Some(provider) => format!("第 {} 名 {}（{}）", rank, model.display_name(), provider),
            None => format!("第 {} 名 {}", rank, model.display_name()),
        };

        let mut score_line: Vec<String> = Vec::new();
        if let Some(score) = model.score_text() {
            score_line.push(format!("共识指数 {score}"));
        }
        if let Some(count) = model.evaluations_text() {
            score_line.push(count);
        }
        if let Some(evidence) = model.evidence_text() {
            score_line.push(evidence);
        }
        if let Some(range) = model.rank_range().filter(|r| r.contains('—')) {
            score_line.push(format!("名次范围 {range}"));
        }
        if !score_line.is_empty() {
            out.push_str(&format!("\n   {}", score_line.join(" · ")));
        }

        let mut meta: Vec<String> = Vec::new();
        if let Some(date) = model.released_date() {
            meta.push(format!("上线 {date}"));
        }
        if let Some(price) = model.price_text() {
            meta.push(price);
        }
        if !meta.is_empty() {
            out.push_str(&format!("\n   {}", meta.join(" · ")));
        }

        let mut entry_links = EntryLinks {
            title: model.display_name().to_string(),
            links: Vec::new(),
        };
        if let Some(url) = model.page_url() {
            out.push_str(&format!("\n   🔗 {url}"));
            entry_links.links.push(EntryLink {
                label: "模型详情".to_string(),
                url,
            });
        }

        entries.push(out);
        links.push(entry_links);
    }

    let mut footer = String::from(leaderboard::ATTRIBUTION);
    footer.push_str(&format!("\n🔗 {}", board.page_url()));
    footer.push_str(
        "\n价格为厂商官网参考价，人民币／百万 Token；共识指数只反映公开评测的汇总结果。",
    );

    Rendered {
        header: models_header(board),
        entries,
        footer,
        links,
    }
}

/// 模型榜标题行：带上榜名、评测规模与站点标注的更新时间
pub(super) fn models_header(board: &Board) -> String {
    let mut header = format!("AIHOT 大模型排行榜 · {}", board.title());
    let meta = models_meta(board);
    if !meta.is_empty() {
        header.push_str(&format!("\n{}", meta.join(" · ")));
    }
    header
}

/// 「20 项评测 · 9 家机构 · 09/24 15:26 更新」，缺哪项就略过哪项
pub(super) fn models_meta(board: &Board) -> Vec<String> {
    let mut meta: Vec<String> = Vec::new();
    if let Some(count) = board.evaluation_count {
        meta.push(format!("{count} 项评测"));
    }
    if let Some(count) = board.org_count {
        meta.push(format!("{count} 家机构"));
    }
    if let Some(updated) = board.updated_at.as_deref().filter(|s| !s.is_empty()) {
        meta.push(format!("{updated} 更新"));
    }
    meta
}

fn render_block(out: &mut String, block: &DailyBlock, depth: usize, budget: &mut usize) {
    if *budget == 0 {
        return;
    }
    let indent = "  ".repeat(depth);

    if let Some(title) = block.title.as_deref() {
        let marker = if depth == 0 { "▍" } else { "· " };
        out.push_str(&format!("{indent}{marker}{title}\n"));
        *budget -= 1;
    }
    if let Some(text) = block.text.as_deref() {
        out.push_str(&format!("{}  {}\n", indent, truncate(text, 100)));
    }
    if let Some(url) = block.url.as_deref() {
        out.push_str(&format!("{indent}  🔗 {url}\n"));
    }

    for child in &block.children {
        render_block(out, child, depth + 1, budget);
    }
}

/// AI 日报：保留 lead / sections / flashes 的原有结构，不重排成普通列表
pub fn render_daily(report: &DailyReport, max_blocks: usize) -> Rendered {
    let header = match (report.date.as_deref(), report.title.as_deref()) {
        (Some(date), Some(title)) => format!("AI 日报 · {date}\n{title}"),
        (Some(date), None) => format!("AI 日报 · {date}"),
        (None, Some(title)) => format!("AI 日报 · {title}"),
        (None, None) => "AI 日报".to_string(),
    };

    let mut entries: Vec<String> = Vec::new();

    if let Some(lead) = report.lead.as_deref() {
        entries.push(truncate(lead, 220));
    }

    let mut budget = max_blocks.max(1);
    for section in &report.sections {
        if budget == 0 {
            break;
        }
        let mut block = String::new();
        render_block(&mut block, section, 0, &mut budget);
        let block = block.trim_end();
        if !block.is_empty() {
            entries.push(block.to_string());
        }
    }

    if budget > 0 && !report.flashes.is_empty() {
        let mut block = String::from("快讯\n");
        for flash in &report.flashes {
            if budget == 0 {
                break;
            }
            render_block(&mut block, flash, 1, &mut budget);
        }
        let block = block.trim_end();
        if !block.is_empty() {
            entries.push(block.to_string());
        }
    }

    if let Some(link) = report.links.primary() {
        entries.push(format!("完整日报：{link}"));
    }

    Rendered {
        header,
        entries,
        footer: super::api::ATTRIBUTION.to_string(),
        links: Vec::new(),
    }
}
