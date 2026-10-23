//! 判定：一个人算不算潜水。只有数据，没有网络与存储，便于单测。
//!
//! 「多久没说话」有两份来源：
//!
//! - **QQ 记的最后发言**（知弦 0.39 起的 `last_sent_at`）：服务器那边记的，机器人离线那几天的
//!   发言它也知道。可它也可能是旧的（实现端读的是缓存），所以 [`trust`] 先拿机器人自己收到的
//!   消息核一遍：最近说过话的人里，QQ 的记录大多跟得上，才信它。
//! - **机器人收到的消息**：只覆盖机器人在线、在记录的那些时候。[`Basis::heard_from`] 是从此刻
//!   往回、记录一直没断的最早时刻——比它更早的沉默，机器人没有资格下结论。
//!
//! 两份取较晚的那个（谁说过话就算谁说过）。只有「确实知道这段时间里他一句没说」才列进名单；
//! 记录不全、拿不准的人单列成「待观察」，不编号，也就不可能被一个序号区间顺手移出去。

use std::collections::HashMap;

use crate::db::activity::Seen;

pub const DAY: i64 = 86_400;

/// 群里的身份。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Owner,
    Admin,
    Member,
}

impl Role {
    pub fn parse(role: &str) -> Self {
        match role {
            "owner" => Self::Owner,
            "admin" | "administrator" => Self::Admin,
            _ => Self::Member,
        }
    }
}

/// 名单上的一个成员（实现端 `guild.member.list` 给的）。
#[derive(Clone, Debug)]
pub struct Member {
    pub id: String,
    /// 群名片；没有就是昵称。
    pub name: String,
    /// QQ 昵称（与群名片不同时显示在旁边）。
    pub nick: String,
    pub role: Role,
    /// 进群时刻（Unix 秒），0 = 不知道。
    pub joined: i64,
    /// QQ 记的最后发言（Unix 秒）：`None` = 实现端没给这一项，`Some(0)` = 进群以来没说过话。
    pub spoke: Option<i64>,
    pub level: u32,
    pub title: String,
}

/// 判定口径。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// 多少天没说话算潜水。
    pub days: u32,
    /// 这段时间里至少说几条才算活跃；≤ 1 就是「说过话就行」。
    pub min_messages: u32,
    /// 新人进群满这么多天还一句没说也算（0 = 不单独看新人）。
    pub newcomer_days: u32,
    pub protect_titled: bool,
    pub protect_level: u32,
}

impl Rule {
    pub fn cutoff(&self, now: i64) -> i64 {
        now - i64::from(self.days) * DAY
    }

    /// 一句话的口径：「30 天没说话」「30 天里说不到 3 句」，新人另说。
    pub fn describe(&self) -> String {
        let mut text = if self.min_messages > 1 {
            format!("{} 天里说不到 {} 句", self.days, self.min_messages)
        } else {
            format!("{} 天没说话", self.days)
        };
        if self.newcomer_days > 0 && self.newcomer_days < self.days {
            text.push_str(&format!("，或进群 {} 天一句没说", self.newcomer_days));
        }
        text
    }
}

/// QQ 那份「最后发言」可不可信。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// 实现端不给这一项（知弦 0.39 以前、微信）。
    Missing,
    /// 给了，但跟机器人亲眼看到的对不上（读的多半是缓存），不拿它下结论。
    Stale,
    Trusted,
}

/// 拿机器人收到的消息核对 QQ 的记录。
///
/// 看最近三天机器人见过说话的人：QQ 的记录应当不早于机器人见到的那天（按天记，再宽一天）。
/// 这样的人够五个、且八成对得上才信；人太少核不了时，只要 QQ 给的不是几乎全为 0，就照 QQ 的。
pub fn trust(members: &[Member], seen: &HashMap<String, Seen>, now: i64) -> Trust {
    let given: Vec<&Member> = members.iter().filter(|m| m.spoke.is_some()).collect();
    if given.is_empty() {
        return Trust::Missing;
    }
    let mut checked = 0;
    let mut agreed = 0;
    for member in &given {
        let Some(seen) = seen.get(&member.id).filter(|s| s.last >= now - 3 * DAY) else {
            continue;
        };
        checked += 1;
        if member.spoke.unwrap_or(0) >= seen.last - 2 * DAY {
            agreed += 1;
        }
    }
    if checked >= 5 {
        return if agreed * 5 >= checked * 4 { Trust::Trusted } else { Trust::Stale };
    }
    let zeros = given.iter().filter(|m| m.spoke == Some(0)).count();
    if given.len() >= 10 && zeros * 20 >= given.len() * 19 {
        return Trust::Stale;
    }
    Trust::Trusted
}

/// 为什么不动这个人。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Shield {
    /// 机器人自己。
    Myself,
    /// 群主、管理员。
    Staff,
    /// 号主与机器人管理员（`ctl.admins`）。
    Keeper,
    /// 豁免名单。
    Exempt,
    /// 有群头衔。
    Titled,
    /// 群等级够高。
    Level,
}

impl Shield {
    pub fn label(self) -> &'static str {
        match self {
            Self::Myself => "机器人自己",
            Self::Staff => "群主与管理员",
            Self::Keeper => "号主与机器人管理员",
            Self::Exempt => "豁免名单",
            Self::Titled => "有群头衔",
            Self::Level => "群等级够高",
        }
    }
}

/// 判下来是什么情况。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Active,
    Protected(Shield),
    /// 进群还不满观察期，现在不判。
    Newcomer,
    /// 潜水：`since` 起再没说过话（最后一次说话，或进群那一刻）。
    Lurking { since: i64, kind: Kind },
    /// 记录不全，拿不准：`since` 是能确定的最晚一次动静（可能是 0）。
    Unsure { since: i64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// 进群以来一句没说过。
    Never,
    /// 很久没说话。
    Silent,
    /// 说过，但说得太少（只在 `min_messages > 1` 时有）。
    Quiet,
}

/// 下结论需要的背景。
pub struct Basis<'a> {
    pub now: i64,
    pub rule: &'a Rule,
    pub trust: Trust,
    /// 从此刻往回、机器人的记录一直没断的最早时刻（再早就没资格说「这期间没说话」）。
    pub heard_from: i64,
    pub me: &'a str,
    pub keepers: &'a [String],
    pub exempt: &'a [String],
}

pub fn judge(member: &Member, seen: Option<&Seen>, basis: &Basis) -> Verdict {
    if let Some(shield) = shield(member, basis) {
        return Verdict::Protected(shield);
    }
    let rule = basis.rule;
    let now = basis.now;
    let cutoff = rule.cutoff(now);
    let trusted = basis.trust == Trust::Trusted;
    let last_db = seen.map_or(0, |s| s.last);
    let last_qq = if trusted { member.spoke.unwrap_or(0) } else { 0 };
    let last = last_db.max(last_qq);
    // 记录从 `at` 起一直没断，才能说「`at` 之后没说过话」。
    let heard_since = |at: i64| trusted || basis.heard_from <= at;

    if last >= cutoff {
        // 说过话；要求条数时，再看说够了没有（条数只有机器人自己的记录，记录不全就不判）。
        if rule.min_messages > 1 && member.joined <= cutoff && basis.heard_from <= cutoff {
            let said = seen.map_or(0, |s| s.recent);
            if said < rule.min_messages {
                return Verdict::Lurking { since: last, kind: Kind::Quiet };
            }
        }
        return Verdict::Active;
    }
    // 到这里：在已知的记录里，cutoff 之后他一句没说。
    let joined = member.joined;
    let never = last == 0 || (joined > 0 && last < joined);
    if joined > cutoff {
        // 进群还不满 days 天：只有开了新人规则、且进群满 newcomer_days 天一句没说才算。
        let newcomer = i64::from(rule.newcomer_days) * DAY;
        if rule.newcomer_days == 0 || now - joined < newcomer {
            return Verdict::Newcomer;
        }
        return if heard_since(joined) {
            Verdict::Lurking { since: joined, kind: Kind::Never }
        } else {
            Verdict::Unsure { since: last }
        };
    }
    if !heard_since(cutoff) {
        return Verdict::Unsure { since: last.max(joined) };
    }
    // 「从没说过话」要整段在册才说得出口：QQ 可信，或机器人从他进群起就在记。
    let never_known = never && joined > 0 && heard_since(joined);
    if never_known {
        return Verdict::Lurking { since: joined, kind: Kind::Never };
    }
    // 最后一次动静不在册时，只能说「至少从记录开始那时起没说话」。
    let floor = if trusted { 0 } else { basis.heard_from.min(cutoff) };
    Verdict::Lurking { since: last.max(joined).max(floor), kind: Kind::Silent }
}

fn shield(member: &Member, basis: &Basis) -> Option<Shield> {
    if member.id == basis.me {
        return Some(Shield::Myself);
    }
    if member.role != Role::Member {
        return Some(Shield::Staff);
    }
    if basis.keepers.contains(&member.id) {
        return Some(Shield::Keeper);
    }
    if basis.exempt.contains(&member.id) {
        return Some(Shield::Exempt);
    }
    if basis.rule.protect_titled && !member.title.trim().is_empty() {
        return Some(Shield::Titled);
    }
    if basis.rule.protect_level > 0 && member.level >= basis.rule.protect_level {
        return Some(Shield::Level);
    }
    None
}

/// 机器人的记录：从此刻往回一直没断的最早时刻。
///
/// `spans` 是记录在的各段（升序、可能重叠）；`floor` 是这个群最早可能有记录的时刻
/// （机器人进群、开始替这个群记消息，两者取晚）。短于 `tolerance` 的缺口不算断——
/// 重启只要几十秒，那几十秒里恰好说了一辈子唯一一句话的概率可以不计。
pub fn heard_from(spans: &[(i64, i64)], floor: i64, now: i64, tolerance: i64) -> i64 {
    let mut cursor = now;
    for &(start, end) in spans.iter().rev() {
        if end < floor {
            break;
        }
        if end + tolerance < cursor {
            // 这一段的结尾到 cursor 之间断过。
            return cursor.max(floor);
        }
        cursor = cursor.min(start);
        if cursor <= floor {
            return floor;
        }
    }
    cursor.max(floor)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn rule() -> Rule {
        Rule { days: 30, min_messages: 1, newcomer_days: 0, protect_titled: true, protect_level: 0 }
    }

    fn member(id: &str, joined_days_ago: i64, spoke: Option<i64>) -> Member {
        Member {
            id: id.into(),
            name: id.into(),
            nick: String::new(),
            role: Role::Member,
            joined: NOW - joined_days_ago * DAY,
            spoke,
            level: 1,
            title: String::new(),
        }
    }

    fn basis<'a>(rule: &'a Rule, trust: Trust, heard_from: i64) -> Basis<'a> {
        Basis { now: NOW, rule, trust, heard_from, me: "bot", keepers: &[], exempt: &[] }
    }

    fn seen(days_ago: i64, recent: u32) -> Seen {
        Seen { last: NOW - days_ago * DAY, recent }
    }

    #[test]
    fn offline_gap_makes_db_only_silence_unsure() {
        let rule = rule();
        // 机器人十天前才连续在线：三十天的沉默它没资格判。
        let b = basis(&rule, Trust::Missing, NOW - 10 * DAY);
        let m = member("1", 200, None);
        assert_eq!(judge(&m, None, &b), Verdict::Unsure { since: m.joined });
        // 但十天内说过话的照样算活跃。
        assert_eq!(judge(&m, Some(&seen(3, 4)), &b), Verdict::Active);
    }

    #[test]
    fn full_record_or_trusted_qq_makes_it_certain() {
        let rule = rule();
        let m = member("1", 200, None);
        let full = basis(&rule, Trust::Missing, NOW - 400 * DAY);
        assert_eq!(judge(&m, Some(&seen(45, 0)), &full), Verdict::Lurking { since: NOW - 45 * DAY, kind: Kind::Silent });
        assert_eq!(judge(&m, None, &full), Verdict::Lurking { since: m.joined, kind: Kind::Never });

        // QQ 说他十天前说过话（机器人那会儿离线）：活跃。
        let qq = basis(&rule, Trust::Trusted, NOW - DAY);
        let spoke = member("2", 200, Some(NOW - 10 * DAY));
        assert_eq!(judge(&spoke, None, &qq), Verdict::Active);
        let silent = member("3", 200, Some(NOW - 90 * DAY));
        assert_eq!(judge(&silent, None, &qq), Verdict::Lurking { since: NOW - 90 * DAY, kind: Kind::Silent });
        let never = member("4", 200, Some(0));
        assert_eq!(judge(&never, None, &qq), Verdict::Lurking { since: never.joined, kind: Kind::Never });
    }

    #[test]
    fn stale_qq_is_not_used_to_condemn() {
        let rule = rule();
        let b = basis(&rule, Trust::Stale, NOW - 5 * DAY);
        let m = member("1", 200, Some(NOW - 90 * DAY));
        assert!(matches!(judge(&m, None, &b), Verdict::Unsure { .. }));
    }

    #[test]
    fn newcomers_wait_unless_the_newcomer_rule_is_on() {
        let mut rule = rule();
        let m = member("1", 10, Some(0));
        let b = basis(&rule, Trust::Trusted, NOW);
        assert_eq!(judge(&m, None, &b), Verdict::Newcomer);
        rule.newcomer_days = 7;
        let b = basis(&rule, Trust::Trusted, NOW);
        assert_eq!(judge(&m, None, &b), Verdict::Lurking { since: m.joined, kind: Kind::Never });
        let fresh = member("2", 3, Some(0));
        assert_eq!(judge(&fresh, None, &b), Verdict::Newcomer);
    }

    #[test]
    fn protections_come_first() {
        let rule = rule();
        let keepers = vec!["k".to_string()];
        let exempt = vec!["e".to_string()];
        let b = Basis { keepers: &keepers, exempt: &exempt, ..basis(&rule, Trust::Trusted, NOW) };
        let mut m = member("bot", 300, Some(0));
        assert_eq!(judge(&m, None, &b), Verdict::Protected(Shield::Myself));
        m.id = "a".into();
        m.role = Role::Admin;
        assert_eq!(judge(&m, None, &b), Verdict::Protected(Shield::Staff));
        m.role = Role::Member;
        m.id = "k".into();
        assert_eq!(judge(&m, None, &b), Verdict::Protected(Shield::Keeper));
        m.id = "e".into();
        assert_eq!(judge(&m, None, &b), Verdict::Protected(Shield::Exempt));
        m.id = "t".into();
        m.title = "元老".into();
        assert_eq!(judge(&m, None, &b), Verdict::Protected(Shield::Titled));
    }

    #[test]
    fn quiet_needs_a_full_record() {
        let mut rule = rule();
        rule.min_messages = 3;
        let m = member("1", 200, Some(NOW - DAY));
        let full = basis(&rule, Trust::Trusted, NOW - 100 * DAY);
        assert_eq!(judge(&m, Some(&seen(1, 2)), &full), Verdict::Lurking { since: NOW - DAY, kind: Kind::Quiet });
        assert_eq!(judge(&m, Some(&seen(1, 3)), &full), Verdict::Active);
        // 记录不全时条数不可信，不判。
        let partial = basis(&rule, Trust::Trusted, NOW - 5 * DAY);
        assert_eq!(judge(&m, Some(&seen(1, 2)), &partial), Verdict::Active);
    }

    #[test]
    fn trust_checks_qq_against_what_we_saw() {
        let seen: HashMap<String, Seen> = (0..6).map(|i| (i.to_string(), Seen { last: NOW - DAY, recent: 5 })).collect();
        let fresh: Vec<Member> = (0..6).map(|i| member(&i.to_string(), 100, Some(NOW - DAY))).collect();
        assert_eq!(trust(&fresh, &seen, NOW), Trust::Trusted);
        let stale: Vec<Member> = (0..6).map(|i| member(&i.to_string(), 100, Some(NOW - 40 * DAY))).collect();
        assert_eq!(trust(&stale, &seen, NOW), Trust::Stale);
        let missing: Vec<Member> = (0..6).map(|i| member(&i.to_string(), 100, None)).collect();
        assert_eq!(trust(&missing, &seen, NOW), Trust::Missing);
        let zeros: Vec<Member> = (0..20).map(|i| member(&format!("z{i}"), 100, Some(0))).collect();
        assert_eq!(trust(&zeros, &HashMap::new(), NOW), Trust::Stale);
    }

    #[test]
    fn heard_from_stops_at_the_latest_real_gap() {
        let h = |x: i64| NOW - x * 3600;
        // 连续在线，只有一处 30 秒的重启：一直追到 floor。
        let spans = [(h(100), h(50)), (h(50) + 30, NOW)];
        assert_eq!(heard_from(&spans, h(90), NOW, 1200), h(90));
        // 五小时前断过三小时：只能追到断开的那头。
        let spans = [(h(100), h(8)), (h(5), NOW)];
        assert_eq!(heard_from(&spans, h(90), NOW, 1200), h(5));
        // 此刻本身不在线（最后一段在两小时前结束）：从此刻起就是断的。
        let spans = [(h(100), h(2))];
        assert_eq!(heard_from(&spans, h(90), NOW, 1200), NOW);
        // 没有任何记录。
        assert_eq!(heard_from(&[], h(90), NOW, 1200), NOW);
    }
}
