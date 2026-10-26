//! 「只记一笔，不处理」的留档：独立保存、限期保留，并与潜水名单做一次只读联动。
//!
//! 留档只回答「谁没完成入群验证」，不回答「谁潜水」。展示时重新跑一遍潜水判定，
//! 把交集及原来的潜水序号标出来；留档本身从不改变 [`crate::plugins::lurker`] 的判定与清理队列。

use super::{Config, LOG_TARGET, Pending, VerifyConfig};
use crate::adapters::satori::{LockedWriter, send_msg};
use crate::event::Context;
use crate::message::Message;
use crate::plugins::{PluginError, get_config_or_default};
use crate::render::web::esc;
use crate::storage::JsonState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Write as _;

const DAY: i64 = 86_400;
const PER_PAGE: usize = 24;
const WIDTH: u32 = 704;
/// 除了每群的可配上限，再给整份文件一个硬上限，避免群很多时总量失控。
const MAX_TOTAL: usize = 5_000;
const CSS: &str = include_str!("../../../res/cards/lurker.css");
const RECORD_CSS: &str = include_str!("../../../res/cards/welcome-records.css");

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(super) struct Record {
    self_id: String,
    group: String,
    pub(super) user_id: String,
    pub(super) name: String,
    /// 进群时刻；旧数据或实现端没给时为 0。
    joined_at: i64,
    failed_at: i64,
    mode: String,
    /// timeout / attempts。
    reason: String,
    attempts: u32,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct Archive {
    entries: Vec<Record>,
}

static ARCHIVE: JsonState<Archive> = JsonState::new(
    LOG_TARGET,
    "welcome",
    "verification-records.json",
    "入群验证留档",
);

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 时间、每群人数与总量三重收口。全局按新到旧排，超过任何一条线都先丢最旧的。
fn prune(archive: &mut Archive, verify: &VerifyConfig, at: i64) -> bool {
    let before = archive.entries.len();
    let oldest = at - i64::from(verify.record_days) * DAY;
    archive.entries.retain(|record| record.failed_at >= oldest);
    archive.entries.sort_by(|a, b| {
        (b.failed_at, &a.group, &a.self_id, &a.user_id).cmp(&(
            a.failed_at,
            &b.group,
            &b.self_id,
            &b.user_id,
        ))
    });
    let limit = verify.record_limit as usize;
    let mut counts: HashMap<(String, String), usize> = HashMap::new();
    archive.entries.retain(|record| {
        let count = counts
            .entry((record.self_id.clone(), record.group.clone()))
            .or_default();
        *count += 1;
        *count <= limit
    });
    archive.entries.truncate(MAX_TOTAL);
    archive.entries.len() != before
}

/// 一次未通过：同一个人只留最新一笔，重复入群不会把目录越攒越长。
pub(super) async fn remember(p: &Pending, reason: &str, verify: &VerifyConfig) {
    let record = Record {
        self_id: p.self_id.clone(),
        group: p.group.clone(),
        user_id: p.user_id.clone(),
        name: p.name.clone(),
        joined_at: p.joined_at,
        failed_at: now(),
        mode: p.mode.clone(),
        reason: reason.to_string(),
        attempts: p.wrong,
    };
    ARCHIVE
        .with(|archive| {
            archive.entries.retain(|old| {
                !(old.self_id == record.self_id
                    && old.group == record.group
                    && old.user_id == record.user_id)
            });
            archive.entries.push(record);
            prune(archive, verify, now());
        })
        .await;
}

/// 通过、退群或被移出后，留档不再代表一个待复核的在群成员。
pub(super) async fn forget(self_id: &str, group: &str, user_id: &str) {
    ARCHIVE
        .update(|archive| {
            let before = archive.entries.len();
            archive.entries.retain(|old| {
                !(old.self_id == self_id && old.group == group && old.user_id == user_id)
            });
            ((), before != archive.entries.len())
        })
        .await;
}

pub(super) async fn take(self_id: &str, group: &str, user_id: &str) -> Option<Record> {
    ARCHIVE
        .update(|archive| {
            let index = archive.entries.iter().position(|old| {
                old.self_id == self_id && old.group == group && old.user_id == user_id
            });
            let changed = index.is_some();
            let record = index.map(|index| archive.entries.remove(index));
            (record, changed)
        })
        .await
}

pub(super) async fn list(config: &Config, self_id: &str, group: &str) -> Vec<Record> {
    ARCHIVE
        .update(|archive| {
            let changed = prune(archive, &config.verify, now());
            let records = archive
                .entries
                .iter()
                .filter(|record| record.self_id == self_id && record.group == group)
                .cloned()
                .collect();
            (records, changed)
        })
        .await
}

/// 给潜水判定贴标签用的只读视图。即使欢迎插件后来关了，仍在保留期内的留档也不会突然消失。
pub(crate) async fn verification_marks(ctx: &Context, group: &str) -> HashMap<String, i64> {
    let config = get_config_or_default::<Config>(ctx);
    list(&config, &ctx.bot.self_id(), group)
        .await
        .into_iter()
        .map(|record| (record.user_id, record.failed_at))
        .collect()
}

pub(super) async fn preload(ctx: &Context) {
    ARCHIVE.preload().await;
    let config = get_config_or_default::<Config>(ctx);
    ARCHIVE
        .update(|archive| ((), prune(archive, &config.verify, now())))
        .await;
}

#[derive(Clone, Debug)]
enum Mark {
    Lurking(u32),
    Unsure,
    NotListed,
}

struct Review {
    group_name: String,
    marks: HashMap<String, Mark>,
    note: Option<String>,
}

/// 查目录时顺便刷新潜水名单。这样目录里的「潜水 #N」就是紧接着可用的原序号，
/// 不会拿一天前的旧名单诱导管理员操作。
async fn review(ctx: &Context, writer: &LockedWriter, group: &str, records: &[Record]) -> Review {
    match crate::plugins::lurker::review_scan(ctx, writer, group).await {
        Ok(Some(scan)) => {
            let mut marks = HashMap::new();
            for record in records {
                let mark = if let Some(entry) =
                    scan.entries.iter().find(|entry| entry.id == record.user_id)
                {
                    Mark::Lurking(entry.no)
                } else if scan.unsure.iter().any(|entry| entry.id == record.user_id) {
                    Mark::Unsure
                } else {
                    Mark::NotListed
                };
                marks.insert(record.user_id.clone(), mark);
            }
            Review {
                group_name: scan.group_name,
                marks,
                note: None,
            }
        }
        Ok(None) => Review {
            group_name: super::group_info(ctx, writer, group).await.name,
            marks: HashMap::new(),
            note: Some("潜水清理未启用或不对本群生效，本页只展示验证留档。".into()),
        },
        Err(error) => Review {
            group_name: super::group_info(ctx, writer, group).await.name,
            marks: HashMap::new(),
            note: Some(format!(
                "这次没能核对潜水名单：{error}。验证留档仍可查看，但不要据此判断潜水。"
            )),
        },
    }
}

fn mark_label(mark: Option<&Mark>) -> Option<(&'static str, String)> {
    match mark {
        Some(Mark::Lurking(no)) => Some(("needs-review", format!("符合潜水口径 · 序号 {no}"))),
        Some(Mark::Unsure) => Some(("watch", "潜水待观察 · 不可移出".into())),
        Some(Mark::NotListed) => Some(("not-listed", "未列入潜水名单".into())),
        None => None,
    }
}

fn mode_label(mode: &str) -> &str {
    match mode {
        "code" => "验证码",
        "math" => "加法题",
        "answer" => "口令",
        _ => "任意消息",
    }
}

fn reason_label(record: &Record) -> String {
    if record.reason == "attempts" {
        format!("答错 {} 次", record.attempts)
    } else {
        "超时未完成".into()
    }
}

fn date(at: i64) -> String {
    use chrono::{Datelike, TimeZone};
    crate::clock::beijing()
        .timestamp_opt(at, 0)
        .single()
        .map_or_else(String::new, |time| {
            if time.year() == crate::clock::beijing_now().year() {
                time.format("%m 月 %d 日").to_string()
            } else {
                time.format("%Y 年 %m 月 %d 日").to_string()
            }
        })
}

fn stamp(at: i64) -> String {
    use chrono::TimeZone;
    crate::clock::beijing()
        .timestamp_opt(at, 0)
        .single()
        .map_or_else(String::new, |time| time.format("%m-%d %H:%M").to_string())
}

fn initial(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map_or_else(|| "?".into(), |c| c.to_uppercase().collect())
}

fn row(record: &Record, review: &Review) -> String {
    let mut meta = vec![esc(&record.user_id), mode_label(&record.mode).to_string()];
    if record.joined_at > 0 {
        meta.push(format!("{}进群", date(record.joined_at)));
    }
    let review_tag = mark_label(review.marks.get(&record.user_id))
        .map_or_else(String::new, |(class, label)| {
            format!(r#"<span class="tag review {class}">{}</span>"#, esc(&label))
        });
    format!(
        r#"<div class="row record-row"><span class="av">{}</span><div class="who"><div class="name">{}</div><div class="meta">{}</div></div><div class="when"><div class="gone">{}</div><div class="meta">{review_tag}{}</div></div></div>"#,
        esc(&initial(&record.name)),
        esc(&record.name),
        meta.join(" · "),
        stamp(record.failed_at),
        esc(&reason_label(record)),
    )
}

#[allow(clippy::too_many_arguments)]
fn page_html(
    group_name: &str,
    records: &[Record],
    page: usize,
    pages: usize,
    total: usize,
    review: &Review,
    verify: &VerifyConfig,
    prefix: &str,
) -> String {
    let lurking = review
        .marks
        .values()
        .filter(|mark| matches!(mark, Mark::Lurking(_)))
        .count();
    let unsure = review
        .marks
        .values()
        .filter(|mark| matches!(mark, Mark::Unsure))
        .count();
    let reviewed = !review.marks.is_empty();
    let start = (page - 1) * PER_PAGE;
    let rows = records
        .iter()
        .map(|record| row(record, review))
        .collect::<String>();
    let mut tally = format!("<span>当前留档<b>{total}</b></span>");
    if reviewed {
        let _ = write!(
            tally,
            "<span>符合潜水口径<b>{lurking}</b></span><span>待观察<b>{unsure}</b></span>"
        );
    }
    let note = review
        .note
        .as_ref()
        .map(|note| {
            format!(
                r#"<div class="caveat"><b>本次说明</b><span>{}</span></div>"#,
                esc(note)
            )
        })
        .unwrap_or_default();
    let mut footer = format!(
        "<p><b>留存边界：</b>未通过后保留 {} 天，每群最多 {} 人；超期、超额都自动移除最旧记录。已复核的人可发「<span class=\"cmd\">{}欢迎 记录 清除 @某人</span>」移除留档。</p>",
        verify.record_days,
        verify.record_limit,
        esc(prefix),
    );
    if reviewed {
        footer.push_str("<p><b>联动管理：</b>只有标「符合潜水口径」的人才有潜水序号，可继续用「潜水 提醒 / 踢」；验证留档本身不会让人进入清理队列。</p>");
    }
    if pages > 1 {
        let _ = write!(
            footer,
            "<p><b>第 {page} / {pages} 页：</b>显示第 {}–{} 人。翻页发「<span class=\"cmd\">{}欢迎 记录 {}</span>」。</p>",
            start + 1,
            start + records.len(),
            esc(prefix),
            if page < pages { page + 1 } else { 1 },
        );
    }
    let body = format!(
        r#"<div class="md-eyebrow"><div class="md-kicker"><span class="md-dot"></span>入群验证留档<span class="md-kicker-en">VERIFICATION</span></div><span class="md-stamp">{}</span></div>
<h1 class="sheet-title md-type-headline-small md-balance">{}</h1>
<div class="sheet-sub">「不处理，仅记录在案」的待复核成员；按最近留档排序。</div>
<div class="tally">{tally}</div>
<div class="caveat record-distinction"><b>验证 ≠ 潜水</b><span>没完成验证只说明这一项待复核。是否潜水仍按发言记录、进群时间与保护规则独立判定。</span></div>{note}
<hr class="rule md-rule"><div class="part"><h2>复核目录</h2><span class="n">第 {page} / {pages} 页</span><span class="hint">最近未通过在前</span></div><div class="rows">{rows}</div>
<div class="foot">{footer}</div>"#,
        stamp(now()),
        esc(group_name),
    );
    format!(
        r#"<!DOCTYPE html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:"><style>{}{CSS}{RECORD_CSS}</style></head><body class="md-text"><div class="shot"><div class="card md-card sheet records">{body}</div></div></body></html>"#,
        crate::render::web::DESIGN_SYSTEM,
    )
}

fn text_page(
    group_name: &str,
    records: &[Record],
    page: usize,
    pages: usize,
    total: usize,
    review: &Review,
) -> String {
    let mut lines = vec![
        format!("入群验证留档｜{group_name}（共 {total} 人，第 {page}/{pages} 页）"),
        "未通过验证不等于潜水；只有标「符合潜水口径」的人才有可操作的潜水序号。".into(),
    ];
    if let Some(note) = &review.note {
        lines.push(note.clone());
    }
    for (index, record) in records.iter().enumerate() {
        let mark = mark_label(review.marks.get(&record.user_id))
            .map(|(_, label)| label)
            .unwrap_or_else(|| "未核对潜水状态".into());
        lines.push(format!(
            "{}. {}（{}）｜{}｜{}｜{}｜{}",
            (page - 1) * PER_PAGE + index + 1,
            record.name,
            record.user_id,
            mode_label(&record.mode),
            reason_label(record),
            stamp(record.failed_at),
            mark,
        ));
    }
    lines.join("\n")
}

/// 发一页留档。出图失败时退回内容完全等价的文字，不多发一张摘要造成刷屏。
pub(super) async fn show(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    group: &str,
    page: usize,
    reply_to: &str,
) -> Result<(), PluginError> {
    let me = ctx.bot.self_id();
    let all = list(config, &me, group).await;
    if all.is_empty() {
        send_msg(
            ctx,
            writer.clone(),
            Some(group),
            None,
            Message::new()
                .reply(reply_to)
                .text("本群没有仍在保留期内的入群验证留档。"),
        )
        .await?;
        return Ok(());
    }
    let pages = all.len().div_ceil(PER_PAGE);
    if page == 0 || page > pages {
        send_msg(
            ctx,
            writer.clone(),
            Some(group),
            None,
            Message::new().reply(reply_to).text(format!(
                "页码写 1 到 {pages}；本群共 {} 条留档。",
                all.len()
            )),
        )
        .await?;
        return Ok(());
    }
    let start = (page - 1) * PER_PAGE;
    let records = &all[start..(start + PER_PAGE).min(all.len())];
    let review = review(ctx, writer, group, &all).await;
    let prefix = crate::command::get_prefixes(ctx)
        .into_iter()
        .next()
        .unwrap_or_default();
    let html = page_html(
        &review.group_name,
        records,
        page,
        pages,
        all.len(),
        &review,
        &config.verify,
        &prefix,
    );
    let description = format!(
        "入群验证留档：共 {} 人，第 {page}/{pages} 页；未通过验证不等于潜水",
        all.len()
    );
    let shot = crate::render::web::shoot(
        crate::render::web::Shot::new(&html, WIDTH + 24)
            .scale(2.5)
            .jpeg(91)
            .max_height(15_000.0)
            .browser(ctx.browser_path().as_deref()),
    )
    .await;
    match shot {
        Ok(image) => {
            send_msg(
                ctx,
                writer.clone(),
                Some(group),
                None,
                Message::new()
                    .reply(reply_to)
                    .image_described(format!("base64://{image}"), description),
            )
            .await?;
        }
        Err(error) => {
            warn!(target: LOG_TARGET, "群 {group} 的验证留档没出成图，改发文字：{error}");
            let text = text_page(&review.group_name, records, page, pages, all.len(), &review);
            send_msg(
                ctx,
                writer.clone(),
                Some(group),
                None,
                Message::new().reply(reply_to).text(text),
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(group: &str, user: &str, failed_at: i64) -> Record {
        Record {
            self_id: "bot".into(),
            group: group.into(),
            user_id: user.into(),
            name: user.into(),
            joined_at: 0,
            failed_at,
            mode: "code".into(),
            reason: "timeout".into(),
            attempts: 0,
        }
    }

    #[test]
    fn archive_is_bounded_by_age_group_and_total() {
        let verify = VerifyConfig {
            record_days: 30,
            record_limit: 10,
            ..Default::default()
        };
        let at = 1_800_000_000;
        let mut archive = Archive {
            entries: (0..30)
                .map(|n| record("a", &n.to_string(), at - n))
                .chain([
                    record("a", "expired", at - 31 * DAY),
                    record("b", "other-group", at),
                ])
                .collect(),
        };
        assert!(prune(&mut archive, &verify, at));
        assert_eq!(
            archive
                .entries
                .iter()
                .filter(|record| record.group == "a")
                .count(),
            10
        );
        assert_eq!(archive.entries.len(), 11);
        assert!(
            archive
                .entries
                .iter()
                .all(|record| record.failed_at >= at - 30 * DAY)
        );

        let mut archive = Archive {
            entries: (0..MAX_TOTAL + 7)
                .map(|n| record(&format!("group-{n}"), "member", at - n as i64))
                .collect(),
        };
        assert!(prune(&mut archive, &verify, at));
        assert_eq!(archive.entries.len(), MAX_TOTAL);
        assert_eq!(archive.entries.first().map(|record| record.failed_at), Some(at));
        assert!(archive.entries.iter().all(|record| record.failed_at >= at - (MAX_TOTAL - 1) as i64));
    }
}
