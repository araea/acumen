//! 禁言记录：`member_bans` 一行是一次「禁言」或「解除」通知（QQ 的灰条）。
//!
//! ## 来源与口径
//! - satori-qq 把群里的禁言灰条转成 `guild-member-updated`（`_type = satori-qq/mute`），
//!   适配器再换成 `notice_type = group_ban`；普通群友也看得到灰条，所以机器人哪怕只是
//!   个旁观成员，也能记下别人被禁言。**只记机器人在线之后发生的**，没有历史可补。
//! - `duration`（秒）：`> 0` 是禁言时长，`0` 是解除，`-1`（[`UNKNOWN`]）是灰条里没读出时长。
//!
//! ## 实际禁言了多久
//! 通知里的时长是「打算禁多久」，不是「禁了多久」：禁言 30 天、一分钟后被解除，
//! 榜上就不该记 30 天。[`tally`] 因此拿同一个人的**下一条事件**截断——后面跟着解除或新的
//! 禁言，这次就只算到那个时刻；时长没读出来的，只有后面紧跟解除时才能知道它到底多长。
//! 自然到期没有通知，所以按通知里的时长算满。
//!
//! ## 排谁
//! [`tally`] 可以按**被禁言的人**排（谁被禁得多），也可以按**动手的管理员**排（谁禁得多）。
//! 灰条里只有一个人（没写操作者）的禁言，不进管理员榜，只在榜外交代有几次。

use std::collections::BTreeMap;

use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, FromQueryResult, Statement, Value};

/// 灰条里没读出禁言时长。
pub const UNKNOWN: i64 = -1;

/// 同一人、同一时长的通知相隔多少秒以内算同一次（内核可能先后推两遍同一条灰条）。
const DUPLICATE_WINDOW_SECS: i64 = 3;

/// satori-qq 0.32.1 及更早读不出时长时会填 1 秒，不是真的禁言了 1 秒。
/// QQ 客户端的最短禁言是 1 分钟，所以 1 秒按「没读出」处理。
const LEGACY_UNREAD_SECS: i64 = 1;

pub async fn init(db: &DatabaseConnection) -> Result<(), DbErr> {
    let backend = db.get_database_backend();
    for sql in [
        r"CREATE TABLE IF NOT EXISTS member_bans (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            guild_id    TEXT    NOT NULL,
            user_id     TEXT    NOT NULL,
            operator_id TEXT    NOT NULL DEFAULT '',
            duration    INTEGER NOT NULL,
            time        INTEGER NOT NULL
        )",
        "CREATE INDEX IF NOT EXISTS idx_bans_guild_time ON member_bans (guild_id, time)",
    ] {
        db.execute_raw(Statement::from_string(backend, sql.to_owned()))
            .await?;
    }
    Ok(())
}

/// 把通知里的时长整理成表里的口径：解除 0、没读出 [`UNKNOWN`]、其余原样。
pub fn normalize_duration(lifted: bool, reported: Option<i64>) -> i64 {
    if lifted {
        return 0;
    }
    match reported {
        Some(secs) if secs > LEGACY_UNREAD_SECS => secs,
        _ => UNKNOWN,
    }
}

/// 记一条通知；相邻几秒内的同一条重复通知只留第一条。
pub async fn record(
    db: &DatabaseConnection,
    guild_id: &str,
    user_id: &str,
    operator_id: &str,
    duration: i64,
    time: i64,
) -> Result<(), DbErr> {
    let sql = "INSERT INTO member_bans (guild_id, user_id, operator_id, duration, time) \
               SELECT ?, ?, ?, ?, ? WHERE NOT EXISTS (\
                 SELECT 1 FROM member_bans \
                 WHERE guild_id = ? AND user_id = ? AND duration = ? AND time BETWEEN ? AND ?)";
    let values: Vec<Value> = vec![
        guild_id.into(),
        user_id.into(),
        operator_id.into(),
        duration.into(),
        time.into(),
        guild_id.into(),
        user_id.into(),
        duration.into(),
        (time - DUPLICATE_WINDOW_SECS).into(),
        (time + DUPLICATE_WINDOW_SECS).into(),
    ];
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        sql,
        values,
    ))
    .await?;
    Ok(())
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct BanEvent {
    pub user_id: String,
    pub operator_id: String,
    pub time: i64,
    pub duration: i64,
}

impl BanEvent {
    /// 动手的管理员；通知没写、或写成被禁言的人自己（实现端没读出操作者时的兜底）都算不知道。
    fn operator(&self) -> Option<&str> {
        Some(self.operator_id.as_str())
            .filter(|id| !id.is_empty() && *id != "0" && *id != self.user_id)
    }
}

/// 某个群从 `start` 起的全部通知，按人、按时间排好。
/// 不设上限：范围内最后一次禁言还要看它后面有没有解除。
pub async fn events_since(
    db: &DatabaseConnection,
    guild_id: &str,
    start: i64,
) -> Result<Vec<BanEvent>, DbErr> {
    BanEvent::find_by_statement(Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT user_id, operator_id, time, duration FROM member_bans \
         WHERE guild_id = ? AND time >= ? ORDER BY user_id, time",
        vec![guild_id.into(), start.into()],
    ))
    .all(db)
    .await
}

/// 榜上排的是谁。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    /// 被禁言的人。
    Member,
    /// 动手禁言的管理员（含群主）。
    Operator,
}

/// 一个人在区间内的禁言汇总：被禁言的，或动手禁言的，看 [`By`]。
#[derive(Debug, PartialEq, Eq)]
pub struct Tally {
    pub person_id: String,
    /// 禁言的次数（解除不算）。
    pub times: i64,
    /// 实际禁言的秒数，只含时长已知的那些次。
    pub secs: i64,
    /// 时长没读出、事后也没能从解除时刻推出的次数。
    pub unknown: i64,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Board {
    /// 按人 id 排好的汇总。
    pub rows: Vec<Tally>,
    /// 按管理员排时，不知道是谁动的手的次数；按成员排恒为 0。
    pub unattributed: i64,
    /// 榜上提前结束的禁言次数：被解除或改判，实际禁言的时间比通知里说的短。
    /// 这些次数只按实际时长累计，榜上要交代一声，否则「禁言 1 天」的人只算了几秒看着像记错了。
    pub cut_short: i64,
}

/// 区间内一次禁言的结果。
struct Term<'a> {
    member: &'a str,
    operator: Option<&'a str>,
    /// 实际禁言的秒数；`None` 是时长没读出、事后也没能推出。
    served: Option<i64>,
    /// 通知里说的时长比实际禁言的长：中途被解除，或被新的禁言顶替了。
    cut_short: bool,
}

/// 区间内的每一次禁言实际生效了多久。`events` 必须按 (人, 时间) 排好，即 [`events_since`] 的顺序。
fn terms(events: &[BanEvent], start: i64, end: i64) -> Vec<Term<'_>> {
    let mut out = Vec::new();
    for person in events.chunk_by(|a, b| a.user_id == b.user_id) {
        for (i, event) in person.iter().enumerate() {
            if event.duration == 0 || event.time < start || event.time >= end {
                continue;
            }
            let next = person.get(i + 1);
            let gap = next.map(|next| next.time - event.time);
            let served = if event.duration == UNKNOWN {
                // 后面紧跟解除，这次的长度就是两者的间隔；否则只能认「不知道」。
                match (next, gap) {
                    (Some(next), Some(gap)) if next.duration == 0 => Some(gap),
                    _ => None,
                }
            } else {
                Some(gap.map_or(event.duration, |gap| event.duration.min(gap)))
            };
            out.push(Term {
                member: &event.user_id,
                operator: event.operator(),
                served,
                cut_short: served.is_some_and(|served| event.duration > served),
            });
        }
    }
    out
}

/// 汇总 `[start, end)` 内每个人的禁言。`events` 必须按 (人, 时间) 排好，即 [`events_since`] 的顺序。
pub fn tally(events: &[BanEvent], start: i64, end: i64, by: By) -> Board {
    let mut rows: BTreeMap<&str, Tally> = BTreeMap::new();
    let mut unattributed = 0;
    let mut cut_short = 0;
    for term in terms(events, start, end) {
        let person = match by {
            By::Member => term.member,
            By::Operator => match term.operator {
                Some(operator) => operator,
                None => {
                    unattributed += 1;
                    continue;
                }
            },
        };
        let row = rows.entry(person).or_insert_with(|| Tally {
            person_id: person.to_owned(),
            times: 0,
            secs: 0,
            unknown: 0,
        });
        row.times += 1;
        cut_short += i64::from(term.cut_short);
        match term.served {
            Some(secs) => row.secs += secs,
            None => row.unknown += 1,
        }
    }
    Board {
        rows: rows.into_values().collect(),
        unattributed,
        cut_short,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(user: &str, time: i64, duration: i64) -> BanEvent {
        by(user, "", time, duration)
    }

    fn by(user: &str, operator: &str, time: i64, duration: i64) -> BanEvent {
        BanEvent {
            user_id: user.into(),
            operator_id: operator.into(),
            time,
            duration,
        }
    }

    fn members(events: &[BanEvent], start: i64, end: i64) -> Vec<Tally> {
        tally(events, start, end, By::Member).rows
    }

    #[test]
    fn normalizes_what_the_notice_carries() {
        assert_eq!(normalize_duration(true, Some(600)), 0, "解除一律是 0");
        assert_eq!(normalize_duration(false, Some(600)), 600);
        assert_eq!(normalize_duration(false, None), UNKNOWN);
        assert_eq!(
            normalize_duration(false, Some(1)),
            UNKNOWN,
            "旧版 satori-qq 读不出时长时填的 1 秒不是真时长"
        );
        assert_eq!(normalize_duration(false, Some(60)), 60);
    }

    #[test]
    fn counts_times_and_sums_full_durations() {
        let events = [ev("a", 100, 600), ev("a", 5_000, 3_600), ev("b", 200, 60)];
        let t = members(&events, 0, 10_000);
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].times, t[0].secs, t[0].unknown), (2, 4_200, 0));
        assert_eq!((t[1].times, t[1].secs, t[1].unknown), (1, 60, 0));
    }

    #[test]
    fn early_lift_cuts_the_term_short() {
        // 禁言 30 天，一分钟后被解除：只算一分钟。
        let events = [ev("a", 1_000, 30 * 86_400), ev("a", 1_060, 0)];
        let t = members(&events, 0, 10_000);
        assert_eq!((t[0].times, t[0].secs), (1, 60));
    }

    #[test]
    fn counts_the_terms_that_ended_early() {
        let events = [
            ev("a", 1_000, 86_400), // 禁言 1 天，十秒后解除：提前结束
            ev("a", 1_010, 0),
            ev("a", 2_000, 600),   // 十分钟满额
            ev("b", 3_000, 3_600), // 一小时，五分钟后被改判成十分钟：前一次提前结束
            ev("b", 3_300, 600),
            ev("c", 4_000, UNKNOWN), // 时长没读出，后面也没有解除：不算提前
        ];
        let board = tally(&events, 0, 10_000, By::Member);
        assert_eq!(board.cut_short, 2);
        let a = &board.rows[0];
        assert_eq!(
            (a.times, a.secs),
            (2, 10 + 600),
            "实际时长累计，不是声明的 1 天"
        );

        // 区间只含满额那次时，没有提前结束
        assert_eq!(tally(&events, 1_500, 2_500, By::Member).cut_short, 0);
    }

    #[test]
    fn a_new_ban_replaces_the_old_one() {
        let events = [ev("a", 1_000, 3_600), ev("a", 1_300, 600)];
        let t = members(&events, 0, 10_000);
        assert_eq!((t[0].times, t[0].secs), (2, 300 + 600));
    }

    #[test]
    fn lift_after_the_term_changes_nothing() {
        let events = [ev("a", 1_000, 600), ev("a", 9_000, 0)];
        let t = members(&events, 0, 10_000);
        assert_eq!(t[0].secs, 600, "自然到期后的解除通知不该把时长拉长");
    }

    #[test]
    fn unknown_length_is_recovered_only_by_a_following_lift() {
        let lifted = [ev("a", 1_000, UNKNOWN), ev("a", 1_900, 0)];
        let t = members(&lifted, 0, 10_000);
        assert_eq!((t[0].times, t[0].secs, t[0].unknown), (1, 900, 0));

        let alone = [ev("a", 1_000, UNKNOWN)];
        let t = members(&alone, 0, 10_000);
        assert_eq!((t[0].times, t[0].secs, t[0].unknown), (1, 0, 1));
    }

    #[test]
    fn only_bans_inside_the_range_count_but_later_events_still_clip() {
        let events = [
            ev("a", 50, 600), // 区间之前
            ev("a", 1_000, 3_600),
            ev("a", 1_100, 0), // 区间之后的解除也要截断上一次
        ];
        let t = members(&events, 900, 1_050);
        assert_eq!((t[0].times, t[0].secs), (1, 100));
        assert!(members(&events, 2_000, 3_000).is_empty());
    }

    #[test]
    fn lifts_alone_make_no_entry() {
        assert!(members(&[ev("a", 100, 0)], 0, 1_000).is_empty());
    }

    #[test]
    fn operators_are_ranked_by_what_they_did() {
        // 管理员 m1 禁了 a（两次）与 b；m2 禁了 c。按人排好：a、a、b、c。
        let events = [
            by("a", "m1", 100, 600),
            by("a", "m1", 5_000, 3_600),
            by("b", "m1", 200, 60),
            by("c", "m2", 300, 120),
        ];
        let board = tally(&events, 0, 10_000, By::Operator);
        assert_eq!(board.unattributed, 0);
        let rows: Vec<_> = board
            .rows
            .iter()
            .map(|t| (t.person_id.as_str(), t.times, t.secs))
            .collect();
        assert_eq!(rows, [("m1", 3, 4_260), ("m2", 1, 120)]);
    }

    #[test]
    fn operator_term_is_also_cut_short_by_a_lift() {
        // 管理员 m1 禁言 30 天，一分钟后（谁解除的不论）解除：m1 名下只算一分钟。
        let events = [by("a", "m1", 1_000, 30 * 86_400), by("a", "m2", 1_060, 0)];
        let board = tally(&events, 0, 10_000, By::Operator);
        assert_eq!(board.rows.len(), 1, "解除不算动手禁言，m2 不上榜");
        assert_eq!(
            (
                board.rows[0].person_id.as_str(),
                board.rows[0].times,
                board.rows[0].secs
            ),
            ("m1", 1, 60)
        );
    }

    #[test]
    fn unknown_operators_stay_off_the_operator_board() {
        let events = [
            ev("a", 100, 600),      // 没写操作者
            by("b", "b", 200, 600), // 操作者写成自己：实现端的兜底，不是真的自己禁自己
            by("c", "0", 300, 600), // 0 也是没有
            by("d", "m1", 400, 600),
        ];
        let board = tally(&events, 0, 10_000, By::Operator);
        assert_eq!(board.unattributed, 3);
        assert_eq!(board.rows.len(), 1);
        assert_eq!(board.rows[0].person_id, "m1");
        // 按成员排时这四次都在榜上
        let by_member = tally(&events, 0, 10_000, By::Member);
        assert_eq!(by_member.rows.len(), 4);
        assert_eq!(by_member.unattributed, 0);
    }

    #[test]
    fn unread_durations_are_counted_per_operator_too() {
        let events = [
            by("a", "m1", 1_000, UNKNOWN),
            by("b", "m1", 2_000, UNKNOWN),
            by("b", "m1", 2_500, 0),
        ];
        let board = tally(&events, 0, 10_000, By::Operator);
        assert_eq!(
            (
                board.rows[0].times,
                board.rows[0].secs,
                board.rows[0].unknown
            ),
            (2, 500, 1),
            "b 那次后面紧跟解除，算得出 500 秒；a 那次算不出"
        );
    }

    #[tokio::test]
    async fn stores_dedupes_and_reads_back_in_order() {
        let db = sea_orm::Database::connect("sqlite::memory:").await.unwrap();
        init(&db).await.unwrap();
        init(&db).await.unwrap(); // 重复建表不报错

        record(&db, "g1", "u2", "", 600, 1_000).await.unwrap();
        record(&db, "g1", "u2", "", 600, 1_002).await.unwrap(); // 同一条灰条被推了两遍
        record(&db, "g1", "u2", "", 600, 1_100).await.unwrap(); // 间隔够远：另一次
        record(&db, "g1", "u1", "op", UNKNOWN, 900).await.unwrap();
        record(&db, "g1", "u1", "op", 0, 950).await.unwrap(); // 解除与禁言同人但时长不同，不算重复
        record(&db, "g2", "u1", "", 600, 1_000).await.unwrap(); // 别的群
        record(&db, "g1", "u3", "", 600, 10).await.unwrap(); // 早于起点

        let events = events_since(&db, "g1", 500).await.unwrap();
        let seen: Vec<_> = events
            .iter()
            .map(|e| (e.user_id.as_str(), e.time, e.duration))
            .collect();
        assert_eq!(events[0].operator_id, "op", "操作者随通知一起存下");
        assert_eq!(events[2].operator_id, "");
        assert_eq!(
            seen,
            [
                ("u1", 900, UNKNOWN),
                ("u1", 950, 0),
                ("u2", 1_000, 600),
                ("u2", 1_100, 600)
            ]
        );

        let t = members(&events, 0, 10_000);
        assert_eq!(
            (t[0].person_id.as_str(), t[0].times, t[0].secs),
            ("u1", 1, 50)
        );
        assert_eq!(
            (t[1].person_id.as_str(), t[1].times, t[1].secs),
            ("u2", 2, 100 + 600)
        );
    }
}
