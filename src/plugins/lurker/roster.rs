//! 查一遍：拉全群名单、对上消息记录、逐个判定，排成带序号的名单。
//!
//! 名单按沉默时长排：最久没说话的排第一。序号只给确定潜水的人；拿不准的单列、不编号。
//! 每次查完存一份（`data/lurker/lists.json`），「潜水 踢 1-20」认的就是最近这一份——
//! 管理员看到的是哪几个人，移出的就是哪几个人，中间有人冒泡了执行时再剔掉。

use super::judge::{self, Basis, Kind, Member, Role, Rule, Shield, Trust, Verdict};
use super::{Config, LOG_TARGET, watch};
use crate::adapters::satori::{LockedWriter, api};
use crate::event::Context;
use crate::storage::JsonState;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

/// 一份名单多久以内能拿来按序号移出。
pub const VALID_FOR: i64 = 24 * 3600;

/// 名单上的一行。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Entry {
    /// 序号；拿不准的人是 0。
    pub no: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub nick: String,
    #[serde(default)]
    pub level: u32,
    /// 群头衔。
    #[serde(default)]
    pub title: String,
    /// 进群时刻，0 = 不知道。
    pub joined: i64,
    /// 已知的最后一次说话，0 = 没有记录。
    pub last: i64,
    /// 从这一刻起再没说过话（判定的口径）。
    pub since: i64,
    pub kind: Category,
    /// 这段时间里说了几条（只数机器人收到的）。
    #[serde(default)]
    pub said: u32,
    /// 被提醒过的时刻。
    #[serde(default)]
    pub warned: i64,
    /// 入群验证未通过的留档时刻；0 = 没有。仅作复核标签，不参与潜水判定。
    #[serde(default)]
    pub verify_failed: i64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Never,
    Silent,
    Quiet,
    Unsure,
}

impl From<Kind> for Category {
    fn from(kind: Kind) -> Self {
        match kind {
            Kind::Never => Self::Never,
            Kind::Silent => Self::Silent,
            Kind::Quiet => Self::Quiet,
        }
    }
}

/// 一次查询的结果。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Scan {
    pub group: String,
    pub group_name: String,
    pub at: i64,
    pub rule: RuleView,
    /// 群里一共多少人。
    pub total: usize,
    /// 确定潜水的，带序号。
    pub entries: Vec<Entry>,
    /// 拿不准的，不带序号。
    pub unsure: Vec<Entry>,
    /// 进群还不满观察期的人数。
    pub newcomers: usize,
    /// 不动的人，按原因数（顺序固定：机器人自己、群主与管理员……）。
    pub protected: Vec<(String, usize)>,
    /// QQ 那份「最后发言」的情况：missing / stale / trusted。
    pub source: String,
    /// 机器人的记录从这一刻起没断过。
    pub heard_from: i64,
    /// 机器人在这个群里是群主或管理员（能不能移出人）。
    pub can_kick: bool,
    /// 当前群成员里有多少条验证留档（包括活跃、受保护、证据不足而不在本名单里的人）。
    #[serde(default)]
    pub verification_records: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RuleView {
    pub days: u32,
    pub min_messages: u32,
    pub newcomer_days: u32,
    pub text: String,
}

impl Scan {
    /// 按序号找人。
    pub fn numbered(&self, no: u32) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.no == no)
    }

    pub fn fresh(&self, now: i64) -> bool {
        now - self.at < VALID_FOR
    }
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct Lists {
    groups: HashMap<String, Scan>,
}

static LISTS: JsonState<Lists> = JsonState::new(LOG_TARGET, "lurker", "lists.json", "潜水名单");

/// 最近一次查这个群的结果。
pub async fn latest(group: &str) -> Option<Scan> {
    LISTS.read(|lists| lists.groups.get(group).cloned()).await
}

/// 查过的各群，最近查的在前。
pub async fn recent() -> Vec<Scan> {
    let mut scans: Vec<Scan> = LISTS.read(|lists| lists.groups.values().cloned().collect()).await;
    scans.sort_by_key(|scan| std::cmp::Reverse(scan.at));
    scans
}

/// 同上，不等锁。
pub fn peek_latest(group: &str) -> Option<Scan> {
    LISTS.peek().and_then(|mut lists| lists.groups.remove(group))
}

async fn remember(scan: &Scan) {
    let now = chrono::Utc::now().timestamp();
    LISTS
        .with(|lists| {
            lists.groups.retain(|_, old| now - old.at < 30 * judge::DAY);
            lists.groups.insert(scan.group.clone(), scan.clone());
        })
        .await;
}

/// 全群名单。`refresh`：让实现端先向服务器重拉（最后发言才是新的）。
pub async fn members(ctx: &Context, writer: &LockedWriter, group: &str, refresh: bool) -> Result<Vec<Member>, String> {
    const MAX_PAGES: usize = 40;
    let mut out = Vec::new();
    let mut next: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let params = match &next {
            Some(cursor) => json!({"guild_id": group, "next": cursor}),
            None => json!({"guild_id": group, "refresh": refresh}),
        };
        let page: Value = writer
            .call(ctx, "guild.member.list", params)
            .await
            .map_err(|error| format!("群成员名单读不出来：{error}"))?;
        out.extend(page.get("data").and_then(Value::as_array).into_iter().flatten().filter_map(member));
        next = page.get("next").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_owned);
        if next.is_none() {
            return Ok(out);
        }
    }
    Err(format!("群成员名单超过 {MAX_PAGES} 页，没读完，这次先不判"))
}

fn member(value: &Value) -> Option<Member> {
    let user = value.get("user")?;
    let id = user.get("id").and_then(Value::as_str).filter(|id| !id.is_empty())?.to_string();
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::trim).filter(|s| visible(s)).map(str::to_owned);
    let nick = text(user.get("name")).or_else(|| text(user.get("nick"))).unwrap_or_default();
    let card = text(value.get("nick")).or_else(|| text(value.get("name")));
    let millis = |key: &str| value.get(key).and_then(Value::as_i64).map(|ms| ms.max(0) / 1000);
    let role = value
        .get("roles")
        .and_then(Value::as_array)
        .and_then(|roles| roles.first())
        .and_then(|role| role.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("member");
    Some(Member {
        name: card.clone().unwrap_or_else(|| if nick.is_empty() { id.clone() } else { nick.clone() }),
        nick: if card.is_some() { nick } else { String::new() },
        id,
        role: Role::parse(role),
        joined: millis("joined_at").unwrap_or(0),
        spoke: millis("last_sent_at"),
        level: value.get("level").and_then(Value::as_u64).and_then(|l| u32::try_from(l).ok()).unwrap_or(0),
        title: text(value.get("title")).unwrap_or_default(),
    })
}

/// 名字里有没有看得见的字。QQ 昵称常见「ㅤ」（韩文填充字）、零宽字符这类占位，画出来是一片空白。
fn visible(name: &str) -> bool {
    name.chars().any(|c| {
        !c.is_whitespace()
            && !c.is_control()
            && !matches!(c, '\u{115F}' | '\u{1160}' | '\u{3164}' | '\u{FFA0}' | '\u{2800}' | '\u{FEFF}' | '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{206F}' | '\u{202A}'..='\u{202E}')
    })
}

/// 这个群用的口径：每群覆盖项盖过全局。
pub fn rule_for(config: &Config, group: &str) -> Rule {
    let own = config.groups.get(group);
    Rule {
        days: own.and_then(|g| g.days).unwrap_or(config.days),
        min_messages: own.and_then(|g| g.min_messages).unwrap_or(config.min_messages),
        newcomer_days: own.and_then(|g| g.newcomer_days).unwrap_or(config.newcomer_days),
        protect_titled: config.protect_titled,
        protect_level: config.protect_level,
    }
}

/// 全局与本群的豁免名单。
pub fn exempt_for(config: &Config, group: &str) -> Vec<String> {
    let mut ids = config.exempt.clone();
    if let Some(own) = config.groups.get(group).and_then(|g| g.exempt.as_ref()) {
        ids.extend(own.iter().cloned());
    }
    ids
}

/// 查一遍这个群。`days`：只这一次换个天数看（「潜水 60」）。
pub async fn scan(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str, days: Option<u32>) -> Result<Scan, String> {
    let now = chrono::Utc::now().timestamp();
    let mut rule = rule_for(config, group);
    if let Some(days) = days {
        rule.days = days;
    }
    let members = members(ctx, writer, group, true).await?;
    if members.is_empty() {
        return Err("群成员名单是空的，实现端可能还没准备好，过一会儿再试".into());
    }
    let since = rule.cutoff(now).min(now - i64::from(rule.newcomer_days) * judge::DAY);
    let seen = crate::db::activity::seen_in_guild(&ctx.db, group, since).await.map_err(|e| format!("消息记录读不出来：{e}"))?;
    let me = ctx.bot.self_id();
    let mine = members.iter().find(|m| m.id == me);
    let trust = judge::trust(&members, &seen, now);
    let first = crate::db::activity::first_heard(&ctx.db, group).await.ok().flatten().unwrap_or(now);
    let floor = first.max(mine.map_or(0, |m| m.joined));
    let spans = watch::spans(ctx, now).await;
    let heard_from = judge::heard_from(&spans, floor, now, watch::TOLERANCE);
    let keepers = crate::access::admin_ids(ctx);
    let exempt = exempt_for(config, group);
    let basis = Basis { now, rule: &rule, trust, heard_from, me: &me, keepers: &keepers, exempt: &exempt };
    let warned = super::warn::warned(group).await;
    let verification = crate::plugins::welcome::records::verification_marks(ctx, group).await;
    let verification_records = members.iter().filter(|member| verification.contains_key(&member.id)).count();

    let mut entries = Vec::new();
    let mut unsure = Vec::new();
    let mut newcomers = 0;
    let mut protected: BTreeMap<Shield, usize> = BTreeMap::new();
    for member in &members {
        let seen = seen.get(&member.id);
        let last = seen.map_or(0, |s| s.last).max(if trust == Trust::Trusted { member.spoke.unwrap_or(0) } else { 0 });
        let row = |since: i64, kind: Category| Entry {
            no: 0,
            id: member.id.clone(),
            name: member.name.clone(),
            nick: member.nick.clone(),
            level: member.level,
            title: member.title.clone(),
            joined: member.joined,
            last,
            since,
            kind,
            said: seen.map_or(0, |s| s.recent),
            warned: warned.get(&member.id).copied().unwrap_or(0),
            verify_failed: verification.get(&member.id).copied().unwrap_or(0),
        };
        match judge::judge(member, seen, &basis) {
            Verdict::Active => {}
            Verdict::Protected(shield) => *protected.entry(shield).or_default() += 1,
            Verdict::Newcomer => newcomers += 1,
            Verdict::Lurking { since, kind } => entries.push(row(since, kind.into())),
            Verdict::Unsure { since } => unsure.push(row(since, Category::Unsure)),
        }
    }
    // 说得太少的排在没说话的后面；同类里沉默越久越靠前，一样久的按号码，结果稳定。
    entries.sort_by(|a, b| (a.kind == Category::Quiet, a.since, &a.id).cmp(&(b.kind == Category::Quiet, b.since, &b.id)));
    for (index, entry) in entries.iter_mut().enumerate() {
        entry.no = u32::try_from(index + 1).unwrap_or(u32::MAX);
    }
    // 拿不准的按「最后一次见他说话」排，没见过的在前、早进群的在前。
    unsure.sort_by(|a, b| (a.last, a.joined, &a.id).cmp(&(b.last, b.joined, &b.id)));

    let group_name = api::get_guild_info(ctx, writer.clone(), group)
        .await
        .ok()
        .map(|info| info.group_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| group.to_string());
    let scan = Scan {
        group: group.to_string(),
        group_name,
        at: now,
        rule: RuleView { days: rule.days, min_messages: rule.min_messages, newcomer_days: rule.newcomer_days, text: rule.describe() },
        total: members.len(),
        entries,
        unsure,
        newcomers,
        protected: protected.into_iter().map(|(shield, n)| (shield.label().to_string(), n)).collect(),
        source: match trust {
            Trust::Missing => "missing",
            Trust::Stale => "stale",
            Trust::Trusted => "trusted",
        }
        .into(),
        heard_from,
        can_kick: mine.is_some_and(|m| m.role != Role::Member),
        verification_records,
    };
    info!(
        target: LOG_TARGET,
        "群 {group}：{} 人里 {} 人潜水、{} 人拿不准（{}；QQ 记录 {}）",
        scan.total,
        scan.entries.len(),
        scan.unsure.len(),
        scan.rule.text,
        scan.source
    );
    remember(&scan).await;
    Ok(scan)
}

/// 名单底下那句「数据从哪来、哪里不全」。
pub fn source_note(scan: &Scan) -> Option<String> {
    let since = super::date;
    match scan.source.as_str() {
        "trusted" => None,
        _ if scan.unsure.is_empty() => None,
        "stale" => Some(format!(
            "QQ 给的最后发言时间像是旧的，没拿它下结论；机器人自己的记录从 {} 起才没断过，那之前说没说过话不知道，所以 {} 人列在「待观察」，不编号、不会被移出。过几天记录够长了再查。",
            since(scan.heard_from),
            scan.unsure.len()
        )),
        _ => Some(format!(
            "QQ 端没给最后发言时间（知弦 0.39 起才有，升级后重启手机生效），只能看机器人收到的消息；记录从 {} 起才没断过，那之前说没说过话不知道，所以 {} 人列在「待观察」，不编号、不会被移出。",
            since(scan.heard_from),
            scan.unsure.len()
        )),
    }
}
