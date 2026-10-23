//! 提醒：先在群里或私聊打个招呼，说清几天后还没冒泡会被请出去。
//!
//! 记下谁在什么时候被提醒过（`data/lurker/warnings.json`）：名单上标「几天前提醒过」，
//! 自动清理按它数宽限期。提醒一个月后作废——那时候该重新提醒一遍。
//!
//! 提醒有三种说法，按 `warn.mode` 挑，也可以组合：
//!
//! - **mention**：在群里 @ 每个人，一条最多 `warn.batch` 人；被 @ 的人手机会亮。
//! - **digest**：群里只发一条，把名字列出来、不 @ 任何人；大群里不刷屏，也不逼人当场表态。
//! - **private**：挨个私聊，群里一句话不说；不得罪人，但对方不一定是好友，送不到只记日志。
//!
//! `warn.escalate` 打开时，提醒过的人会在宽限期满后被移出——名单重新查一遍，
//! 冒泡的人自然不在名单上，只动当初被提醒过、也确实还没说话的那几个。

use super::{Config, LOG_TARGET, date};
use crate::adapters::satori::{LockedWriter, send_msg};
use crate::event::Context;
use crate::message::Message;
use crate::plugins::PluginError;
use crate::storage::JsonState;
use crate::template::{Piece, pieces};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const EXPIRE: i64 = 30 * 86_400;
/// 提醒之后隔这么久还没处理，就当这次提醒没发生过（与提醒记录的寿命一致）。
const PENDING_EXPIRE: i64 = 30 * 86_400;
/// 送不到时多久之后再试一次（只对私聊；群里的提醒不重试）。
const RETRY_AFTER: Duration = Duration::from_mins(10);

#[derive(Serialize, Deserialize, Default, Clone)]
struct Book {
    /// 群 → 人 → 提醒的时刻。
    groups: HashMap<String, HashMap<String, i64>>,
    /// 群 → 人 → 该在什么时候被移出（`warn.escalate` 打开时才有）。
    #[serde(default)]
    pending: HashMap<String, HashMap<String, i64>>,
}

static BOOK: JsonState<Book> = JsonState::new(LOG_TARGET, "lurker", "warnings.json", "提醒记录");

/// 上一次因为没查成而放弃的群：十分钟内不再重试，免得每分钟敲一次平台。
static LAST_TRY: std::sync::Mutex<Option<HashMap<String, Instant>>> = std::sync::Mutex::new(None);

/// 这一批提醒里，群里怎么发。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Where {
    /// 群里 @ 每个人。
    At,
    /// 群里一条，列出名字、不 @。
    Digest,
    /// 群里不说。
    None,
}

pub(crate) struct Style {
    pub group: Where,
    pub private: bool,
}

/// `mention` / `digest` / `private` / `mention_private` / `digest_private`。
pub(crate) fn style(mode: &str) -> Style {
    match mode {
        "digest" => Style { group: Where::Digest, private: false },
        "private" => Style { group: Where::None, private: true },
        "mention_private" => Style { group: Where::At, private: true },
        "digest_private" => Style { group: Where::Digest, private: true },
        _ => Style { group: Where::At, private: false },
    }
}

/// 说给人听的模式名。
pub fn mode_name(mode: &str) -> &'static str {
    match mode {
        "digest" => "群里一条、不 @",
        "private" => "只私聊",
        "mention_private" => "群里 @ 并私聊",
        "digest_private" => "群里一条并私聊",
        _ => "群里 @ 每个人",
    }
}

/// 命令里能写的说法：返回模式名与剩下的参数。
pub(crate) fn parse_mode(rest: &str) -> (Option<String>, String) {
    let mut words: Vec<&str> = rest.split_whitespace().collect();
    let mut group: Option<&str> = None;
    let mut private = false;
    while let Some(last) = words.last().copied() {
        match last {
            "群里" | "群内" | "at" | "@" => group = Some("mention"),
            "一条" | "安静" | "不@" | "digest" => group = Some("digest"),
            "私聊" | "private" => private = true,
            "都要" | "两者" | "both" => {
                group = Some("mention");
                private = true;
            }
            _ => break,
        }
        words.pop();
    }
    let mode = match (group, private) {
        (Some("digest"), true) => Some("digest_private"),
        (Some("digest"), false) => Some("digest"),
        (Some(_), true) => Some("mention_private"),
        (Some(group), false) => Some(group),
        (None, true) => Some("private"),
        (None, false) => None,
    };
    (mode.map(str::to_owned), words.join(" "))
}

/// 这个群里谁被提醒过、什么时候（一个月内的）。
pub async fn warned(group: &str) -> HashMap<String, i64> {
    let now = chrono::Utc::now().timestamp();
    BOOK.read(|book| {
        book.groups
            .get(group)
            .map(|people| people.iter().filter(|(_, at)| now - **at < EXPIRE).map(|(id, at)| (id.clone(), *at)).collect())
            .unwrap_or_default()
    })
    .await
}

/// 排着队等宽限期满的人有几个（控制台用）。
pub fn pending_count() -> usize {
    BOOK.peek().map(|book| book.pending.values().map(HashMap::len).sum()).unwrap_or(0)
}

/// 各群排着队等宽限期满的人（控制台在名单上方标出来）。
pub fn pending_by_group() -> HashMap<String, usize> {
    BOOK.peek()
        .map(|book| book.pending.iter().filter(|(_, people)| !people.is_empty()).map(|(group, people)| (group.clone(), people.len())).collect())
        .unwrap_or_default()
}

/// 移出了、或不必再记的人划掉。
pub async fn forget(group: &str, ids: &[String]) {
    if ids.is_empty() {
        return;
    }
    BOOK.with(|book| {
        if let Some(people) = book.groups.get_mut(group) {
            people.retain(|id, _| !ids.contains(id));
        }
        if let Some(people) = book.pending.get_mut(group) {
            people.retain(|id, _| !ids.contains(id));
        }
    })
    .await;
}

/// 发出去的一批提醒，回来报个数。
pub(crate) struct Report {
    pub total: usize,
    pub group_messages: usize,
    pub private: usize,
    pub mode: String,
}

impl Report {
    /// 一行日志用的简报（自动清理的结果栏地方小）。
    pub fn brief(&self) -> String {
        match (self.group_messages, self.private) {
            (0, 0) => "提醒没发出去".into(),
            (0, n) => format!("私聊提醒 {n} 人"),
            (m, 0) => format!("提醒了 {} 人（{m} 条消息）", self.total),
            (m, n) => format!("提醒了 {} 人（{m} 条消息 + 私聊 {n} 人）", self.total),
        }
    }

    /// 说给人听的一句话。
    pub fn describe(&self, config: &Config) -> String {
        let mut text = match (self.group_messages, self.private) {
            (0, 0) => format!("没提醒到人：{} 的话是空的，在控制台写一句再来。", mode_name(&self.mode)),
            (0, n) => format!("正在私聊提醒 {n} 人（一条一条发，别急）"),
            (1, 0) => format!("在群里说了一声，提醒了 {} 人", self.total),
            (m, 0) => format!("在群里提醒了 {} 人，分 {m} 条消息发", self.total),
            (m, n) => format!("在群里提醒了 {} 人（{m} 条消息），另给 {n} 人发了私聊", self.total),
        };
        text.push('，');
        let tail = if config.warn.escalate {
            format!("{} 天后还没冒泡的会自动移出", config.warn.grace_days)
        } else {
            "到时候重新查一遍名单，再按序号移出".to_string()
        };
        text.push_str(&tail);
        text.push('。');
        text
    }
}

/// 提醒这些人。群里的那条发完才回来，私聊在后台一条一条发。
pub async fn send(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    group: &str,
    people: &[(String, String)],
) -> Result<Report, PluginError> {
    let now = chrono::Utc::now().timestamp();
    let grace = i64::from(config.warn.grace_days) * 86_400;
    let deadline = date(now + grace);
    let days = super::roster::rule_for(config, group).days;
    let group_name = super::roster::peek_latest(group)
        .map(|scan| scan.group_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| group.to_string());
    let style = style(&config.warn.mode);
    let values = HashMap::from([
        ("days", days.to_string()),
        ("deadline", deadline),
        ("grace", config.warn.grace_days.to_string()),
        ("count", people.len().to_string()),
        ("group", group_name.clone()),
    ]);
    let names = names_of(people);

    // 先把要说的话捡出来：一条都没有就别发，也不占一次的额。
    let at_text = (style.group == Where::At).then(|| pick(&config.warn.text)).flatten();
    let digest_text = (style.group == Where::Digest).then(|| pick(&config.warn.digest_text)).flatten();
    let private_texts: Vec<String> = if style.private {
        config.warn.private_text.iter().filter(|text| !text.trim().is_empty()).cloned().collect()
    } else {
        Vec::new()
    };
    if style.group == Where::At && at_text.is_none() {
        return Err(PluginError::from("提醒的话（warn.text）是空的，在控制台写一句再来"));
    }
    if style.group == Where::Digest && digest_text.is_none() {
        return Err(PluginError::from("群里那条提醒（warn.digest_text）是空的，在控制台写一句再来"));
    }
    if style.private && private_texts.is_empty() {
        return Err(PluginError::from("私聊说的话（warn.private_text）是空的，在控制台写一句再来"));
    }

    let mut reached: Vec<String> = Vec::new();
    let mut group_messages = 0;
    // 群里 @：分批，条与条之间歇一会儿，别一屏蓝字砸下来。
    if let Some(text) = at_text {
        let batch = config.warn.batch.clamp(1, 50) as usize;
        let chunks = people.len().div_ceil(batch);
        for (index, chunk) in people.chunks(batch).enumerate() {
            let mut message = Message::new();
            for piece in pieces(&text, &values) {
                message = match piece {
                    Piece::Text(text) => message.text(text),
                    Piece::At => chunk.iter().fold(message, |m, (id, _)| m.at(id).text(" ")),
                    Piece::Face(id) => message.face(id),
                    Piece::Image(_) | Piece::Avatar => message,
                };
            }
            if let Err(error) = send_msg(ctx, writer.clone(), Some(group), None, message).await {
                // 前几条已经发出去了：把它们记下，别让下一轮再提醒一遍。
                if !reached.is_empty() {
                    mark(config, group, &reached, now, grace).await;
                }
                return Err(error);
            }
            group_messages += 1;
            reached.extend(chunk.iter().map(|(id, _)| id.clone()));
            if index + 1 < chunks {
                tokio::time::sleep(Duration::from_secs(u64::from(config.warn.pause_seconds))).await;
            }
        }
    }
    // 群里一条、不 @：名字列出来，谁看见算谁的。
    if let Some(text) = digest_text {
        let mut values = values.clone();
        values.insert("count", people.len().to_string());
        values.insert("names", names.clone());
        let mut message = Message::new();
        for piece in pieces(&text, &values) {
            if let Piece::Text(text) = piece {
                message = message.text(text);
            }
        }
        send_msg(ctx, writer.clone(), Some(group), None, message).await?;
        group_messages += 1;
        reached.extend(people.iter().map(|(id, _)| id.clone()));
    }

    if !reached.is_empty() {
        mark(config, group, &reached, now, grace).await;
    }

    // 私聊：一个人在后台慢慢发，不挡群里的下一条消息。
    if style.private {
        let (ctx, writer, people, texts, group, values) = (
            ctx.clone(),
            writer.clone(),
            people.to_vec(),
            private_texts,
            group.to_string(),
            values.clone(),
        );
        let escalate = config.warn.escalate;
        let grace_days = config.warn.grace_days;
        let pause = Duration::from_secs(u64::from(config.warn.pause_seconds));
        tokio::spawn(async move {
            let mut ok = Vec::new();
            let mut failed = 0usize;
            for (id, _) in &people {
                let Some(text) = pick(&texts) else {
                    break;
                };
                let mut message = Message::new();
                for piece in pieces(&text, &values) {
                    if let Piece::Text(text) = piece {
                        message = message.text(text);
                    }
                }
                match send_msg(&ctx, writer.clone(), None, Some(id), message).await {
                    Ok(()) => ok.push(id.clone()),
                    Err(error) => {
                        failed += 1;
                        debug!(target: LOG_TARGET, "私聊提醒 {id} 没送到：{error}");
                    }
                }
                tokio::time::sleep(pause).await;
            }
            if !ok.is_empty() {
                let now = chrono::Utc::now().timestamp();
                let grace = i64::from(grace_days) * 86_400;
                BOOK.with(|book| {
                    let people = book.groups.entry(group.clone()).or_default();
                    people.retain(|_, at| now - *at < EXPIRE);
                    for id in &ok {
                        people.insert(id.clone(), now);
                    }
                    if escalate {
                        let pending = book.pending.entry(group.clone()).or_default();
                        pending.retain(|_, at| *at > now - PENDING_EXPIRE);
                        for id in &ok {
                            pending.insert(id.clone(), now + grace);
                        }
                    }
                })
                .await;
            }
            info!(target: LOG_TARGET, "群 {group} 私聊提醒：送到 {} 人，{failed} 人没送到（多半不是好友）", ok.len());
        });
    }

    let report = Report {
        total: people.len(),
        group_messages,
        private: if style.private { people.len() } else { 0 },
        mode: config.warn.mode.clone(),
    };
    info!(
        target: LOG_TARGET,
        "群 {group} 提醒 {} 人（{}）：群里 {} 条，私聊 {} 人",
        report.total,
        mode_name(&report.mode),
        report.group_messages,
        report.private
    );
    Ok(report)
}

/// 记下「这些人什么时候被提醒过」，以及到期该移出谁。
async fn mark(config: &Config, group: &str, ids: &[String], now: i64, grace: i64) {
    let escalate = config.warn.escalate;
    BOOK.with(|book| {
        let people = book.groups.entry(group.to_string()).or_default();
        people.retain(|_, at| now - *at < EXPIRE);
        for id in ids {
            people.insert(id.clone(), now);
        }
        let pending = book.pending.entry(group.to_string()).or_default();
        pending.retain(|_, at| *at > now - PENDING_EXPIRE);
        if escalate {
            for id in ids {
                pending.entry(id.clone()).or_insert(now + grace);
            }
        } else {
            pending.clear();
        }
    })
    .await;
}

/// 名字列成一行：重名的不重复写，太多就写「等 N 人」。
fn names_of(people: &[(String, String)]) -> String {
    let mut names: Vec<&str> = Vec::new();
    for (id, name) in people {
        let name = if name.trim().is_empty() { id.as_str() } else { name.as_str() };
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let mut text = names.iter().take(30).copied().collect::<Vec<_>>().join("、");
    if names.len() > 30 {
        text.push_str(&format!(" 等 {} 人", people.len()));
    }
    text
}

/// 宽限期满的那批：重新查一遍名单，只动当初被提醒过、现在还没说话的人。
///
/// 每分钟被叫一次；没有到期的就只读一次内存。查名单要走平台，所以一个群一天最多
/// 也就查那几次；查不成十分钟内不再试。
pub async fn tick(ctx: &Context, writer: &LockedWriter) {
    let config = crate::plugins::get_config_or_default::<Config>(ctx);
    if !config.enabled || !config.warn.escalate {
        return;
    }
    let now = chrono::Utc::now().timestamp();
    // 到期太久（机器人离线、或这一批一直没轮到）就不再补：当初说的是「三天后」，
    // 隔了半个月再来踢人，比不踢更伤人。这一批直接作废，下次重新提醒。
    const LATE: i64 = 7 * 86_400;
    let due: Vec<(String, Vec<String>, Vec<String>)> = BOOK
        .read(|book| {
            book.pending
                .iter()
                .filter_map(|(group, people)| {
                    let mut ready = Vec::new();
                    let mut stale = Vec::new();
                    for (id, at) in people {
                        if *at > now {
                            continue;
                        }
                        if now - *at > LATE {
                            stale.push(id.clone());
                        } else {
                            ready.push(id.clone());
                        }
                    }
                    (!ready.is_empty() || !stale.is_empty()).then(|| (group.clone(), ready, stale))
                })
                .collect()
        })
        .await;
    for (group, ids, stale) in due {
        if !stale.is_empty() {
            info!(target: LOG_TARGET, "群 {group} 有 {} 人的到期清理过了头（机器人不在线），作废，下次重新提醒", stale.len());
            remove(&group, &stale).await;
        }
        if ids.is_empty() {
            continue;
        }
        if !config.channel.allows_group(&group) || !ctx.group_allowed(&group) {
            clear(&group).await;
            continue;
        }
        let cooling = LAST_TRY
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().and_then(|tries| tries.get(&group)).copied())
            .is_some_and(|at| at.elapsed() < RETRY_AFTER);
        if cooling {
            continue;
        }
        if super::purge::active(&group).await.is_some() {
            continue;
        }
        let scan = match super::roster::scan(ctx, writer, &config, &group, None).await {
            Ok(scan) => scan,
            Err(error) => {
                warn!(target: LOG_TARGET, "群 {group} 到期清理没查成：{error}");
                if let Ok(mut guard) = LAST_TRY.lock() {
                    guard.get_or_insert_with(HashMap::new).insert(group.clone(), Instant::now());
                }
                continue;
            }
        };
        if !scan.can_kick {
            super::tell_keepers(ctx, writer, &format!("到期清理潜水：我在「{}」（{group}）不是管理员，这次跳过了。", scan.group_name)).await;
            clear(&group).await;
            continue;
        }
        let targets: Vec<&super::roster::Entry> = scan.entries.iter().filter(|entry| ids.contains(&entry.id)).collect();
        if targets.is_empty() {
            info!(target: LOG_TARGET, "群 {group} 到期清理：当初提醒的人都已经冒泡，没有要移出的");
            clear(&group).await;
            continue;
        }
        let ratio = targets.len() * 100 / scan.total.max(1);
        if ratio > config.auto.max_ratio as usize {
            super::tell_keepers(
                ctx,
                writer,
                &format!(
                    "到期清理潜水：「{}」（{group}）当初提醒的人里有 {} 人还没冒泡，占全群 {ratio}%，超过上限 {}%，这次没动手。\n请在群里发「潜水」看一眼名单，确认无误再手动移出。",
                    scan.group_name,
                    targets.len(),
                    config.auto.max_ratio
                ),
            )
            .await;
            clear(&group).await;
            continue;
        }
        let cap = config.pace.max_per_run as usize;
        let (now_targets, rest): (Vec<_>, Vec<_>) = targets.into_iter().enumerate().partition(|(index, _)| *index < cap);
        let kicked: Vec<String> = now_targets.iter().map(|(_, entry)| entry.id.clone()).collect();
        let queue: Vec<super::purge::Target> = now_targets
            .iter()
            .map(|(_, entry)| super::purge::Target {
                id: entry.id.clone(),
                name: entry.name.clone(),
                no: entry.no,
                since: entry.since,
                kind: entry.kind,
            })
            .collect();
        let count = queue.len();
        let job = super::purge::Job {
            id: chrono::Utc::now().timestamp_millis(),
            group: group.clone(),
            group_name: scan.group_name.clone(),
            operator: String::new(),
            operator_name: "到期清理".into(),
            source: "warn".into(),
            reply_to: String::new(),
            permanent: config.permanent,
            rule: scan.rule.text.clone(),
            created: now,
            queue,
            done: Vec::new(),
            state: super::purge::State::Running,
            finished: 0,
            note: String::new(),
        };
        match super::purge::submit(ctx, writer, job).await {
            Ok(()) => {
                info!(target: LOG_TARGET, "群 {group} 到期清理：开始移出 {count} 人，另有 {} 人下一轮再看", rest.len());
                remove(&group, &kicked).await;
            }
            Err(error) => warn!(target: LOG_TARGET, "群 {group} 到期清理没排上：{error}"),
        }
    }
}

async fn remove(group: &str, ids: &[String]) {
    BOOK.with(|book| {
        if let Some(people) = book.pending.get_mut(group) {
            people.retain(|id, _| !ids.contains(id));
        }
    })
    .await;
}

async fn clear(group: &str) {
    BOOK.with(|book| book.pending.remove(group)).await;
}

/// 提醒之后谁冒泡了、还剩多少——群里「潜水 提醒 状态」。
pub async fn status(group: &str) -> String {
    let warned = warned(group).await;
    if warned.is_empty() {
        return "本群最近一个月没有提醒过谁。发「潜水 提醒」可以把名单上的人一次性提醒一遍。".into();
    }
    let guard = BOOK.read(|book| book.pending.get(group).cloned()).await.unwrap_or_default();
    let now = chrono::Utc::now().timestamp();
    let scan = super::roster::latest(group).await;
    let (mut here, mut returned) = (0, 0);
    if let Some(scan) = &scan {
        let listed: Vec<&str> = scan.entries.iter().chain(scan.unsure.iter()).map(|entry| entry.id.as_str()).collect();
        for id in warned.keys() {
            if listed.contains(&id.as_str()) {
                here += 1;
            } else {
                returned += 1;
            }
        }
    }
    let mut text = format!("提醒过 {} 人（最近一个月）", warned.len());
    if scan.is_some() {
        text.push_str(&format!("：{returned} 人已经冒泡或不在名单上，{here} 人还没说话"));
    }
    text.push('。');
    let overdue = guard.values().filter(|at| **at <= now).count();
    let due = guard.values().copied().filter(|at| *at > now).min();
    if overdue > 0 {
        text.push_str(&format!("有 {overdue} 人已经到期，等下一次检查（每分钟看一眼）。"));
    } else if let Some(due) = due {
        text.push_str(&format!("下一批在 {} 到期移出。", date(due)));
    } else if here > 0 {
        text.push_str("没有排在后面的自动移出；到时候重新查一遍名单，再按序号移出。");
    }
    text
}

/// 几种说法随机一种；全空 = 不发。
pub fn pick(variants: &[String]) -> Option<String> {
    let usable: Vec<&String> = variants.iter().filter(|text| !text.trim().is_empty()).collect();
    (!usable.is_empty()).then(|| usable[rand::random_range(0..usable.len())].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_combine() {
        assert_eq!(style("mention").group, Where::At);
        assert!(!style("mention").private);
        assert!(style("mention_private").private);
        assert_eq!(style("digest_private").group, Where::Digest);
        assert_eq!(style("private").group, Where::None);
    }

    #[test]
    fn command_accepts_trailing_mode() {
        assert_eq!(parse_mode("1-20 私聊"), (Some("private".into()), "1-20".into()));
        assert_eq!(parse_mode("1-20 一条 私聊"), (Some("digest_private".into()), "1-20".into()));
        assert_eq!(parse_mode("全部"), (None, "全部".into()));
    }

    #[test]
    fn names_stay_short() {
        let people: Vec<(String, String)> = (0..40).map(|i| (i.to_string(), format!("人{i}"))).collect();
        assert!(names_of(&people).ends_with("等 40 人"));
    }
}
