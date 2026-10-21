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

use super::api::{HotTopic, Item, Report, ReportItem, ReportSection, Story, category_label};
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

/// ISO8601 → `MM-DD HH:MM`（北京时间）。
///
/// 整整 00:00:00 的只写 `MM-DD`：不少信源只给到「日」，接口把它补成当天零点，
/// 照写「00:00」会让人以为是半夜发的，是假的精确。
pub(super) fn fmt_time(iso: &str) -> Option<String> {
    use chrono::Timelike;
    DateTime::parse_from_rfc3339(iso).ok().map(|dt| {
        let local = dt.with_timezone(&crate::clock::beijing());
        if local.num_seconds_from_midnight() == 0 {
            local.format("%m-%d").to_string()
        } else {
            local.format("%m-%d %H:%M").to_string()
        }
    })
}

/// 按句子收尾地截断，让读者读到的是完整的一句话，而不是半句加省略号。
///
/// 超长时依次尝试：
///   1. 退到 `max_chars` 以内最后一个句末标点（只在后 60% 里找，退得太多就没意思了）；
///   2. 往后多读一点，到 `max_chars` 的 1.4 倍内的第一个句末——多出来的几个字换一整句，值；
///   3. 退到最后一个逗号、顿号前，补「…」；
///   4. 都没有才硬截并补「…」。
///
/// 摘要、导语、综述都是一两句成段的文字，在句中切断最影响阅读。
pub(super) fn clip(text: &str, max_chars: usize) -> String {
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = cleaned.chars().collect();
    if chars.len() <= max_chars {
        return cleaned;
    }
    let ends_sentence = |i: usize| match chars[i] {
        '。' | '！' | '？' | '；' | '!' | '?' | ';' => true,
        // 英文句号：后面是空白才算句末，免得把 `4.1`、`aihot.news` 切开
        '.' => chars.get(i + 1).is_none_or(|next| next.is_whitespace()),
        _ => false,
    };
    // 停在分号上读着像没说完，收成句号；其余句末标点原样保留
    let take = |end: usize| {
        let text: String = chars[..=end].iter().collect();
        match text.trim_end().strip_suffix(['；', ';']) {
            Some(head) => format!("{head}。"),
            None => text.trim_end().to_string(),
        }
    };

    let floor = max_chars * 2 / 5;
    if let Some(end) = (floor..max_chars).rev().find(|&i| ends_sentence(i)) {
        return take(end);
    }
    let ceiling = (max_chars * 7 / 5).min(chars.len());
    if let Some(end) = (max_chars..ceiling).find(|&i| ends_sentence(i)) {
        return take(end);
    }
    let cut = (floor..max_chars)
        .rev()
        .find(|&i| matches!(chars[i], '，' | '、' | ',' | '：'))
        .unwrap_or(max_chars);
    let head: String = chars[..cut].iter().collect();
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
            out.push_str(&format!("\n   {}", clip(summary, opts.summary_max_chars)));
        }
        let others = cluster.other_reports();
        if !others.is_empty() {
            out.push_str(&format!("\n   {}", also_line(&others)));
        }
        if opts.show_reason
            && let Some(reason) = item.reason.as_deref().map(str::trim).filter(|s| !s.is_empty())
        {
            out.push_str(&format!("\n   💡 {}", clip(reason, opts.summary_max_chars)));
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

        if let Some(summary) = topic.digest_lead() {
            out.push_str(&format!("\n   {}", clip(summary, 100)));
        }
        if let Some(latest) = topic.latest.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            out.push_str(&format!("\n   最新进展：{}", clip(latest, 80)));
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

/// 模型榜：一条一段，先名次与模型名，再 AIHOT 评分与评测覆盖，最后上线日期与价格
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
            score_line.push(format!("AIHOT 评分 {score}"));
        }
        if let Some(count) = model.evaluations_text() {
            score_line.push(count);
        }
        if let Some(coverage) = model.coverage_text() {
            score_line.push(coverage);
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
    if let Some(pending) = board.pending.filter(|n| *n > 0) {
        footer.push_str(&format!("\n另有 {pending} 个模型评测还不够宽，暂不排名。"));
    }
    footer.push_str(
        "\n评分 50 分是参照模型的平均水平，每 15 分约一个标准差，不是正确率；价格为厂商官网参考价，人民币／百万 Token。",
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

/// 一期报告里挑出来展示的那部分。
pub(super) struct Digest<'a> {
    /// 有条目的栏目，连同各自入选的条目
    pub sections: Vec<(&'a ReportSection, Vec<&'a ReportItem>)>,
    /// 日报末尾的快讯（只在栏目条目没把名额占满时才有）
    pub flashes: Vec<&'a ReportItem>,
    /// 这一期一共多少条
    pub total: usize,
}

impl Digest<'_> {
    pub fn shown(&self) -> usize {
        self.sections.iter().map(|(_, items)| items.len()).sum::<usize>() + self.flashes.len()
    }
}

/// 在 `budget` 条的名额里挑条目：各栏目轮流各取一条，一圈一圈地取。
///
/// 栏目内的条目已经按重要程度排好，所以每栏取最前面的几条；不能从第一栏顺着取到底——
/// 周报、月报动辄五六个栏目，名额被头两栏占光，后面几栏连标题都见不着。
pub(super) fn digest(report: &Report, budget: usize) -> Digest<'_> {
    let mut taken = vec![0usize; report.sections.len()];
    let mut left = budget.max(1);
    'rounds: loop {
        let mut progressed = false;
        for (taken, section) in taken.iter_mut().zip(&report.sections) {
            if left == 0 {
                break 'rounds;
            }
            if *taken < section.items.len() {
                *taken += 1;
                left -= 1;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }

    Digest {
        sections: report
            .sections
            .iter()
            .zip(&taken)
            .filter(|(_, n)| **n > 0)
            .map(|(section, n)| (section, section.items.iter().take(*n).collect()))
            .collect(),
        flashes: report.flashes.iter().take(left).collect(),
        total: report.sections.iter().map(|s| s.items.len()).sum::<usize>() + report.flashes.len(),
    }
}

/// 条目的「信源 · 发布时间」一行；两样都没有返回 `None`
pub(super) fn report_item_meta(item: &ReportItem) -> Option<String> {
    let parts: Vec<String> = [
        item.source.clone(),
        item.published_at.as_deref().and_then(fmt_time),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// 一期报告的抬头：`AI 周报 · 2026 年第 41 周 · 10.05—10.11`
pub(super) fn report_heading(report: &Report) -> String {
    format!("{} · {}", report.kind.title(), report.subtitle())
}

/// 日报、周报、月报：每条一个序号，引用后回序号取这一条的 AIHOT 页与原文，
/// 最后一个序号是整期的阅读页。栏目名写在该栏第一条的上面。
pub fn render_report(report: &Report, budget: usize, opts: &RenderOptions) -> Rendered {
    let digest = digest(report, budget);

    let mut header = report_heading(report);
    if let Some(headline) = report.headline.as_deref() {
        header.push_str(&format!("\n{headline}"));
    }
    if let Some(lead) = report.lead.as_deref() {
        header.push_str(&format!("\n{}", clip(lead, 300)));
    }

    let mut entries: Vec<String> = Vec::new();
    let mut links: Vec<EntryLinks> = Vec::new();
    let mut push_item = |item: &ReportItem, banner: Option<String>| {
        let mut out = banner.unwrap_or_default();
        out.push_str(&format!("{}. {}", entries.len() + 1, item.title));
        if let Some(meta) = report_item_meta(item) {
            out.push_str(&format!("\n   {meta}"));
        }
        if let Some(summary) = item.summary.as_deref() {
            out.push_str(&format!("\n   {}", clip(summary, opts.summary_max_chars)));
        }
        let mut entry_links = EntryLinks {
            title: item.title.clone(),
            links: Vec::new(),
        };
        if let Some(url) = item.aihot.as_deref() {
            out.push_str(&format!("\n   🔗 {url}"));
            entry_links.links.push(EntryLink {
                label: "AIHOT".to_string(),
                url: url.to_string(),
            });
        }
        if opts.show_original_link
            && let Some(url) = item.original.as_deref()
        {
            out.push_str(&format!("\n   📄 {url}"));
            entry_links.links.push(EntryLink {
                label: "原文".to_string(),
                url: url.to_string(),
            });
        }
        entries.push(out);
        links.push(entry_links);
    };

    for (section, items) in &digest.sections {
        for (index, item) in items.iter().enumerate() {
            let banner = (index == 0).then(|| {
                let mut banner = format!("▍{}\n", section.label.as_deref().unwrap_or("其他"));
                if let Some(summary) = section.summary.as_deref() {
                    banner.push_str(&format!("   {}\n", clip(summary, 160)));
                }
                banner
            });
            push_item(item, banner);
        }
    }
    if !digest.flashes.is_empty() {
        for (index, item) in digest.flashes.iter().enumerate() {
            push_item(item, (index == 0).then(|| "▍快讯\n".to_string()));
        }
    }

    if let Some(url) = report.link.as_deref() {
        entries.push(format!("{}. 阅读完整{}\n   🔗 {url}", entries.len() + 1, report.kind.noun()));
        links.push(EntryLinks {
            title: format!("完整{}（{}）", report.kind.noun(), report.edition()),
            links: vec![EntryLink {
                label: "AIHOT".to_string(),
                url: url.to_string(),
            }],
        });
    }

    let footer = if digest.shown() < digest.total {
        format!("{} · 本期共 {} 条，这里选了 {} 条", super::api::ATTRIBUTION, digest.total, digest.shown())
    } else {
        format!("{} · 共 {} 条", super::api::ATTRIBUTION, digest.total)
    };

    Rendered {
        header,
        entries,
        footer,
        links,
    }
}

/// 事件详情的时间线最多放几篇
pub(super) const STORY_TIMELINE: usize = 10;

/// 事件的概况一行：`17 家信源 · 19 篇报道 · 进行中 · 首报 10-17`
pub(super) fn story_meta(story: &Story) -> String {
    let mut parts: Vec<String> = Vec::new();
    if story.source_count > 0 {
        parts.push(format!("{} 家信源", story.source_count));
    }
    if story.report_count > 0 {
        parts.push(format!("{} 篇报道", story.report_count));
    }
    parts.push(if story.settled { "已平息" } else { "仍在发展" }.to_string());
    if let Some(first) = story.first_report_at.as_deref().and_then(fmt_time) {
        parts.push(format!("首报 {first}"));
    }
    parts.join(" · ")
}

/// 时间线上一篇报道的「信源 · 时间」，一手信源标出来
pub(super) fn story_report_meta(report: &super::api::StoryReport) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(meta) = report_item_meta(&report.item) {
        parts.push(meta);
    }
    if report.first_party {
        parts.push("官方首发".to_string());
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// 事件详情：综述、最新进展，再是按时间排好的报道，每篇一个序号，最后一个序号是事件页
pub fn render_story(story: &Story, rank: Option<u32>, opts: &RenderOptions) -> Rendered {
    let mut header = match rank {
        Some(rank) => format!("AI 热点 · 第 {rank} 名\n{}", story.title),
        None => format!("AI 热点\n{}", story.title),
    };
    header.push_str(&format!("\n{}", story_meta(story)));
    for paragraph in story.digest_paragraphs() {
        header.push_str(&format!("\n\n{}", clip(paragraph, 420)));
    }
    if let Some(latest) = story.latest.as_deref() {
        header.push_str(&format!("\n\n最新进展：{latest}"));
    }

    let mut entries: Vec<String> = Vec::new();
    let mut links: Vec<EntryLinks> = Vec::new();
    for report in story.timeline(STORY_TIMELINE) {
        let item = &report.item;
        let mut out = format!("{}. {}", entries.len() + 1, item.title);
        if let Some(meta) = story_report_meta(report) {
            out.push_str(&format!("\n   {meta}"));
        }
        let mut entry_links = EntryLinks {
            title: item.title.clone(),
            links: Vec::new(),
        };
        if let Some(url) = item.aihot.as_deref() {
            out.push_str(&format!("\n   🔗 {url}"));
            entry_links.links.push(EntryLink {
                label: "AIHOT".to_string(),
                url: url.to_string(),
            });
        }
        if opts.show_original_link
            && let Some(url) = item.original.as_deref()
        {
            out.push_str(&format!("\n   📄 {url}"));
            entry_links.links.push(EntryLink {
                label: "原文".to_string(),
                url: url.to_string(),
            });
        }
        entries.push(out);
        links.push(entry_links);
    }
    if let Some(url) = story.link.as_deref() {
        entries.push(format!("{}. 阅读事件页\n   🔗 {url}", entries.len() + 1));
        links.push(EntryLinks {
            title: format!("事件页：{}", story.title),
            links: vec![EntryLink {
                label: "AIHOT".to_string(),
                url: url.to_string(),
            }],
        });
    }

    Rendered {
        header,
        entries,
        footer: format!("{} · 时间线选了 {} / {} 篇", super::api::ATTRIBUTION, story.timeline(STORY_TIMELINE).len(), story.reports.len()),
        links,
    }
}
