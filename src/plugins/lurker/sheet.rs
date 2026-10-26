//! 把名单排成长图（或文字），装进一条合并转发。
//!
//! 一张图放 [`super::ListConfig::rows_per_image`] 行，长了就切成几张：分节跟着走，接着上一张的节
//! 在下一张开头写「（续）」。第一张有总账（多少人、各类几个、数据全不全），最后一张有怎么移出。
//! 合并转发里另附一份纯文字名单：能搜、能复制号码，出图失败时它就是名单本身。

use super::roster::{Category, Entry, Scan};
use super::{LOG_TARGET, date};
use crate::render::web::esc;
use base64::Engine;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

const CSS: &str = include_str!("../../../res/cards/lurker.css");
const WIDTH: u32 = 680;

/// 一节：标题、这节有几个人、说明。
struct Part {
    title: String,
    hint: &'static str,
    rows: Vec<Entry>,
}

/// 按沉默时长分节：一年、半年、三个月、两个月、一个月、两周、一周、几天；说得太少、待观察单列。
fn parts(scan: &Scan) -> Vec<Part> {
    const STEPS: [(i64, &str); 8] = [
        (365, "一年以上"),
        (180, "半年以上"),
        (90, "三个月以上"),
        (60, "两个月以上"),
        (30, "一个月以上"),
        (14, "两周以上"),
        (7, "一周以上"),
        (0, "不到一周"),
    ];
    let now = scan.at;
    let mut out: Vec<Part> = Vec::new();
    for entry in scan.entries.iter().filter(|e| e.kind != Category::Quiet) {
        let days = (now - entry.since) / 86_400;
        let (_, label) = STEPS.iter().find(|(min, _)| days >= *min).copied().unwrap_or((0, "不到一周"));
        let title = format!("{label}没说话");
        match out.last_mut() {
            Some(part) if part.title == title => part.rows.push(entry.clone()),
            _ => out.push(Part { title, hint: "", rows: vec![entry.clone()] }),
        }
    }
    let quiet: Vec<Entry> = scan.entries.iter().filter(|e| e.kind == Category::Quiet).cloned().collect();
    if !quiet.is_empty() {
        out.push(Part {
            title: format!("{} 天里说不到 {} 句", scan.rule.days, scan.rule.min_messages),
            hint: "",
            rows: quiet,
        });
    }
    if !scan.unsure.is_empty() {
        out.push(Part { title: "待观察".into(), hint: "记录不全，不编号、不会被移出", rows: scan.unsure.clone() });
    }
    out
}

/// 「93 天」「进群 210 天」这一列。
fn gone(entry: &Entry, now: i64) -> String {
    let days = ((now - entry.since) / 86_400).max(0);
    match entry.kind {
        Category::Never => format!("{days}<small>天</small>"),
        Category::Quiet => format!("{}<small>句</small>", entry.said),
        Category::Unsure => format!("{days}+<small>天</small>"),
        _ if entry.last == 0 => format!("≥{days}<small>天</small>"),
        _ => format!("{days}<small>天</small>"),
    }
}

/// 名字下面那一行：昵称（与群名片不同时）· 号码 · 等级 · 进群。
fn meta(entry: &Entry) -> String {
    let mut bits = Vec::new();
    if !entry.nick.is_empty() && entry.nick != entry.name {
        bits.push(esc(&entry.nick));
    }
    bits.push(esc(&entry.id));
    if entry.level > 0 {
        bits.push(format!("LV{}", entry.level));
    }
    if !entry.title.trim().is_empty() {
        bits.push(format!("「{}」", esc(entry.title.trim())));
    }
    if entry.joined > 0 {
        bits.push(format!("{}进群", date(entry.joined)));
    }
    bits.join(" · ")
}

/// 右栏第二行：最后说话那天，或「没有记录」。
fn last_line(entry: &Entry, now: i64) -> String {
    let mut out = String::new();
    if entry.verify_failed > 0 {
        out.push_str(r#"<span class="tag verify">验证未通过</span>"#);
    }
    if entry.warned > 0 {
        let _ = write!(out, r#"<span class="tag warned">{}提醒过</span>"#, ago(now, entry.warned));
    }
    match entry.kind {
        Category::Never => out.push_str(r#"<span class="tag never">进群后没说过话</span>"#),
        Category::Quiet => {
            let _ = write!(out, "最后 {}", date(entry.last));
        }
        Category::Unsure if entry.last > 0 => {
            let _ = write!(out, r#"<span class="tag">记录不全</span>见过 {}"#, date(entry.last));
        }
        Category::Unsure => out.push_str(r#"<span class="tag">记录不全</span>没见过他说话"#),
        _ if entry.last > 0 => {
            let _ = write!(out, "最后 {}", date(entry.last));
        }
        _ => out.push_str("没有发言记录"),
    }
    out
}

fn ago(now: i64, at: i64) -> String {
    match (now - at) / 86_400 {
        0 => "今天".into(),
        1 => "昨天".into(),
        n => format!("{n} 天前"),
    }
}

fn row(entry: &Entry, now: i64, avatars: &HashMap<String, String>) -> String {
    let face = match avatars.get(&entry.id) {
        Some(data) => format!(r#"<img class="av" src="data:image/jpeg;base64,{data}" alt="">"#),
        None => format!(r#"<span class="av">{}</span>"#, esc(&initial(&entry.name))),
    };
    let no = if entry.no > 0 { entry.no.to_string() } else { "—".into() };
    format!(
        r#"<div class="row{unsure}"><span class="no">{no}</span>{face}<div class="who"><div class="name">{name}</div><div class="meta">{meta}</div></div><div class="when"><div class="gone">{gone}</div><div class="meta">{last}</div></div></div>"#,
        unsure = if entry.no == 0 { " unsure" } else { "" },
        name = esc(&entry.name),
        meta = meta(entry),
        gone = gone(entry, now),
        last = last_line(entry, now),
    )
}

/// 名字的头一个字（emoji、符号开头的跳过），没有就是「?」。
fn initial(name: &str) -> String {
    name.chars().find(|c| c.is_alphanumeric()).map_or_else(|| "?".into(), |c| c.to_uppercase().collect())
}

fn header(scan: &Scan) -> String {
    let never = scan.entries.iter().filter(|e| e.kind == Category::Never).count();
    let quiet = scan.entries.iter().filter(|e| e.kind == Category::Quiet).count();
    let silent = scan.entries.len() - never - quiet;
    let mut tally = String::new();
    for (label, n) in [("很久没说话", silent), ("进群后没说过话", never), ("说得太少", quiet), ("待观察", scan.unsure.len())] {
        if n > 0 {
            let _ = write!(tally, "<span>{label}<b>{n}</b></span>");
        }
    }
    let caveat = super::roster::source_note(scan)
        .map(|note| format!(r#"<div class="caveat"><b>注意</b><span>{}</span></div>"#, esc(&note)))
        .unwrap_or_default();
    let verification = if scan.verification_records > 0 {
        let overlap = scan.entries.iter().filter(|entry| entry.verify_failed > 0).count();
        format!(
            r#"<div class="caveat verification-note"><b>验证留档</b><span>当前成员有 {} 条入群验证留档，其中 {} 人符合本次潜水口径，已标「验证未通过」。未通过验证不等于潜水，其余留档不会进入清理队列。</span></div>"#,
            scan.verification_records, overlap,
        )
    } else {
        String::new()
    };
    format!(
        r#"<div class="md-eyebrow"><div class="md-kicker"><span class="md-dot"></span>潜水名单<span class="md-kicker-en">INACTIVE</span></div><span class="md-stamp">{stamp}</span></div>
<h1 class="sheet-title md-type-headline-small md-balance">{name}</h1>
<div class="sheet-sub">全群 <b>{total}</b> 人，按「{rule}」查到 <b>{count}</b> 人{extra}</div>
<div class="tally">{tally}</div>{caveat}{verification}"#,
        stamp = stamp(scan.at),
        name = esc(&scan.group_name),
        total = scan.total,
        rule = esc(&scan.rule.text),
        count = scan.entries.len(),
        extra = if scan.entries.is_empty() { "，没有要清理的" } else { "" },
    )
}

fn footer(scan: &Scan, prefix: &str) -> String {
    let mut kept: Vec<String> = scan.protected.iter().map(|(label, n)| format!("{} {n}", esc(label))).collect();
    if scan.newcomers > 0 {
        kept.push(format!("进群不满 {} 天的新人 {}", scan.rule.days, scan.newcomers));
    }
    let mut out = String::from(r#"<div class="foot">"#);
    if !kept.is_empty() {
        let _ = write!(out, "<p><b>不在名单上的：</b>{}</p>", kept.join(" · "));
    }
    if scan.entries.is_empty() {
        out.push_str("<p>没有要清理的人。</p>");
    } else if !scan.can_kick {
        out.push_str("<p>机器人在这个群里不是管理员，只能看，移不出人。</p>");
    } else {
        let last = scan.entries.len();
        let _ = write!(
            out,
            r#"<p><b>怎么移出：</b>发「<span class="cmd">{prefix}潜水 踢 1-{last}</span>」，也可以挑：「<span class="cmd">{prefix}潜水 踢 1-20 25</span>」「<span class="cmd">{prefix}潜水 踢 全部 除 3 7</span>」；确认后分批慢慢移出。先打个招呼：「<span class="cmd">{prefix}潜水 提醒 1-{last}</span>」。</p>"#
        );
        out.push_str("<p>序号 24 小时内有效；移出前会再核一遍，期间冒过泡的自动留下。</p>");
    }
    out.push_str("</div>");
    out
}

fn stamp(at: i64) -> String {
    use chrono::TimeZone;
    crate::clock::beijing()
        .timestamp_opt(at, 0)
        .single()
        .map_or_else(String::new, |t| t.format("%m-%d %H:%M").to_string())
}

/// 排成几页 HTML。
pub fn pages(scan: &Scan, per_page: usize, prefix: &str, avatars: &HashMap<String, String>) -> Vec<String> {
    let per_page = per_page.max(10);
    let mut pages: Vec<String> = Vec::new();
    let mut body = String::new();
    let mut used = 0;
    for part in parts(scan) {
        let mut rows = part.rows.as_slice();
        let mut continued = false;
        while !rows.is_empty() {
            if used >= per_page {
                pages.push(std::mem::take(&mut body));
                used = 0;
            }
            let take = rows.len().min(per_page - used);
            let (now, rest) = rows.split_at(take);
            let _ = write!(
                body,
                r#"<div class="part"><h2>{}{}</h2><span class="n">{} 人</span>{}</div><div class="rows">"#,
                esc(&part.title),
                if continued { "（续）" } else { "" },
                part.rows.len(),
                if part.hint.is_empty() { String::new() } else { format!(r#"<span class="hint">{}</span>"#, part.hint) },
            );
            for entry in now {
                body.push_str(&row(entry, scan.at, avatars));
            }
            body.push_str("</div>");
            used += take;
            rows = rest;
            continued = true;
        }
    }
    pages.push(body);
    let count = pages.len();
    pages
        .into_iter()
        .enumerate()
        .map(|(index, body)| {
            let head = if index == 0 {
                header(scan)
            } else {
                format!(
                    r#"<div class="md-eyebrow"><div class="md-kicker"><span class="md-dot"></span>潜水名单 · 第 {} / {count} 张</div><span class="md-stamp">{}</span></div><h1 class="sheet-title md-type-title-large">{}</h1>"#,
                    index + 1,
                    stamp(scan.at),
                    esc(&scan.group_name)
                )
            };
            let foot = if index + 1 == count { footer(scan, prefix) } else { String::new() };
            document(&format!(r#"{head}<hr class="rule md-rule">{body}{foot}"#))
        })
        .collect()
}

fn document(inner: &str) -> String {
    format!(
        r#"<!DOCTYPE html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:"><style>{}{CSS}</style></head>
<body class="md-text"><div class="shot"><div class="card md-card sheet">{inner}</div></div></body></html>"#,
        crate::render::web::DESIGN_SYSTEM
    )
}

/// 出图。任何一张没出成就整体返回错误，由调用方改发文字。
pub async fn render(pages: &[String], browser: Option<&str>) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::with_capacity(pages.len());
    for html in pages {
        out.push(
            crate::render::web::shoot(
                crate::render::web::Shot::new(html, WIDTH + 24)
                    .scale(2.5)
                    .jpeg(90)
                    .max_height(15_000.0)
                    .browser(browser),
            )
            .await?,
        );
    }
    Ok(out)
}

/// 文字版：一行一人，能搜、能复制号码。分成几段，每段不超过 `per_chunk` 行。
pub fn text(scan: &Scan, per_chunk: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for part in parts(scan) {
        lines.push(format!("【{}】{} 人", part.title, part.rows.len()));
        for entry in &part.rows {
            let no = if entry.no > 0 { format!("{}.", entry.no) } else { "·".into() };
            let days = ((scan.at - entry.since) / 86_400).max(0);
            let gone = match entry.kind {
                Category::Never => format!("进群 {days} 天没说过话"),
                Category::Quiet => format!("{} 天里 {} 句", scan.rule.days, entry.said),
                Category::Unsure => format!("机器人记录里 {days} 天没说话（之前的不知道）"),
                _ if entry.last > 0 => format!("{days} 天没说话（最后 {}）", date(entry.last)),
                _ => format!("至少 {days} 天没说话"),
            };
            let verification = if entry.verify_failed > 0 { "｜验证未通过（仅作复核标签）" } else { "" };
            lines.push(format!("{no} {}（{}）{gone}{verification}", entry.name, entry.id));
        }
    }
    lines.chunks(per_chunk.max(10)).map(|chunk| chunk.join("\n")).collect()
}

// ==================== 头像 ====================

/// 头像缓存多久算新鲜。
const AVATAR_FRESH: Duration = Duration::from_hours(7 * 24);
/// 整个头像阶段最多等多久：到点没拿到的用首字。
const AVATAR_DEADLINE: Duration = Duration::from_secs(12);

/// 一批人的头像（base64 JPEG）。只有 QQ 号能拼出公开头像；缓存在 `data/lurker/avatars/`。
pub async fn avatars(ids: &[String], qq: bool) -> HashMap<String, String> {
    use futures_util::StreamExt;
    let mut out = HashMap::new();
    if !qq {
        return out;
    }
    let Ok(dir) = crate::plugins::get_data_dir("lurker").await.map(|d| d.join("avatars")) else {
        return out;
    };
    let _ = tokio::fs::create_dir_all(&dir).await;
    let deadline = tokio::time::Instant::now() + AVATAR_DEADLINE;
    let wanted: Vec<(String, PathBuf)> = ids
        .iter()
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
        .map(|id| (id.clone(), dir.join(format!("{id}.jpg"))))
        .collect();
    let jobs = wanted.into_iter().map(|(id, path)| async move {
        let bytes = avatar(&id, path, deadline).await;
        (id, bytes)
    });
    let mut stream = futures_util::stream::iter(jobs).buffer_unordered(8);
    while let Some((id, bytes)) = stream.next().await {
        if let Some(bytes) = bytes {
            out.insert(id, base64::engine::general_purpose::STANDARD.encode(bytes));
        }
    }
    out
}

async fn avatar(id: &str, path: PathBuf, deadline: tokio::time::Instant) -> Option<Vec<u8>> {
    let cached = tokio::fs::read(&path).await.ok();
    let age = tokio::fs::metadata(&path)
        .await
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|at| SystemTime::now().duration_since(at).ok());
    if let Some(bytes) = &cached
        && age.is_some_and(|age| age < AVATAR_FRESH)
    {
        return Some(bytes.clone());
    }
    let url = format!("https://q1.qlogo.cn/g?b=qq&nk={id}&s=100");
    let fetched = tokio::time::timeout_at(deadline, async {
        let response = crate::http::client().get(&url).timeout(Duration::from_secs(6)).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.bytes().await.ok().map(|b| b.to_vec())
    })
    .await
    .ok()
    .flatten()
    .filter(|bytes| bytes.len() > 100);
    match fetched {
        Some(bytes) => {
            if let Err(error) = crate::storage::write_atomic_async(path, bytes.clone()).await {
                debug!(target: LOG_TARGET, "头像缓存写不进去：{error}");
            }
            Some(bytes)
        }
        // 网不通时旧的也比首字强。
        None => cached,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::roster::RuleView;

    fn entry(no: u32, days: i64, kind: Category) -> Entry {
        Entry {
            no,
            id: format!("{}", 10_000 + no),
            name: format!("成员{no}"),
            nick: String::new(),
            level: 3,
            title: String::new(),
            joined: 1_000,
            last: if kind == Category::Never { 0 } else { 1_800_000_000 - days * 86_400 },
            since: 1_800_000_000 - days * 86_400,
            kind,
            said: 0,
            warned: 0,
            verify_failed: 0,
        }
    }

    fn scan(entries: Vec<Entry>, unsure: Vec<Entry>) -> Scan {
        Scan {
            group: "1".into(),
            group_name: "测试群".into(),
            at: 1_800_000_000,
            rule: RuleView { days: 30, min_messages: 1, newcomer_days: 0, text: "30 天没说话".into() },
            total: 100,
            entries,
            unsure,
            newcomers: 0,
            protected: Default::default(),
            source: "trusted".into(),
            heard_from: 0,
            can_kick: true,
            verification_records: 0,
        }
    }

    #[test]
    fn parts_follow_silence_and_split_pages_with_continuation() {
        let mut entries: Vec<Entry> = (1..=30).map(|n| entry(n, 400, Category::Silent)).collect();
        entries.extend((31..=35).map(|n| entry(n, 40, Category::Never)));
        let s = scan(entries, vec![entry(0, 20, Category::Unsure)]);
        let titles: Vec<String> = parts(&s).into_iter().map(|p| p.title).collect();
        assert_eq!(titles, ["一年以上没说话", "一个月以上没说话", "待观察"]);
        let pages = pages(&s, 20, "!", &HashMap::new());
        assert_eq!(pages.len(), 2);
        assert!(pages[1].contains("一年以上没说话（续）"));
        assert!(pages[1].contains("潜水 踢 1-35"));
        assert!(!pages[0].contains("怎么移出"));
    }

    #[test]
    fn text_list_keeps_numbers_and_ids() {
        let s = scan(vec![entry(1, 45, Category::Silent), entry(2, 50, Category::Never)], Vec::new());
        let text = text(&s, 100).join("\n");
        assert!(text.contains("1. 成员1（10001）45 天没说话"), "{text}");
        assert!(text.contains("2. 成员2（10002）进群"), "{text}");
    }
}
