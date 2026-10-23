//! 群成员的活跃度：谁在本群最后一次说话是哪天、一段时间里说了几句，以及机器人哪些时候在收消息。
//!
//! 读的是按「群 × 用户 × 日」预聚合的 `message_user_stats_daily`：它不随原始消息的保留期清理，
//! 一个群几百人、半年的记录也只是几万行，一次查询就够。口径是自然日（本地时区），清理潜水按天算，
//! 不需要精确到秒。
//!
//! 这些数字只覆盖**机器人收到了**的消息：机器人离线那几天群里说过的话这里没有。所以
//! [`heard_slots`] 另给一份「哪些十分钟里收到过消息」，调用方据此判断记录在哪几段是断的。

use chrono::{Local, NaiveDate, TimeZone};
use sea_orm::{DatabaseConnection, DbErr, FromQueryResult, Statement};

/// 一个人在一个群里留下的记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    /// 最后说话那天结束的时刻（Unix 秒）：按天记的，宁可往晚里算，不把那天傍晚说过话的人算早。
    pub last: i64,
    /// `since` 那天起说了几条。
    pub recent: u32,
}

#[derive(FromQueryResult)]
struct Row {
    user_id: String,
    last_day: String,
    recent: i64,
}

/// 本群每个说过话的人：最后说话的那天，以及 `since`（Unix 秒，按所在那天算起）以来的条数。
pub async fn seen_in_guild(
    db: &DatabaseConnection,
    guild_id: &str,
    since: i64,
) -> Result<std::collections::HashMap<String, Seen>, DbErr> {
    let since_day = day_of(since);
    let rows = Row::find_by_statement(Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT user_id, MAX(stat_date) AS last_day, \
         COALESCE(SUM(CASE WHEN stat_date >= ? THEN msg_count ELSE 0 END), 0) AS recent \
         FROM message_user_stats_daily WHERE guild_id = ? AND msg_count > 0 GROUP BY user_id",
        vec![since_day.into(), guild_id.into()],
    ))
    .all(db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let last = day_end(&row.last_day)?;
            Some((
                row.user_id,
                Seen {
                    last,
                    recent: u32::try_from(row.recent.max(0)).unwrap_or(u32::MAX),
                },
            ))
        })
        .collect())
}

/// 本群最早有记录的那天开始的时刻：在那之前机器人没替这个群记过任何一句。没有记录给 `None`。
pub async fn first_heard(db: &DatabaseConnection, guild_id: &str) -> Result<Option<i64>, DbErr> {
    #[derive(FromQueryResult)]
    struct First {
        day: Option<String>,
    }
    let first = First::find_by_statement(Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT MIN(stat_date) AS day FROM message_stats_daily WHERE guild_id = ? AND msg_count > 0",
        vec![guild_id.into()],
    ))
    .one(db)
    .await?;
    Ok(first.and_then(|f| f.day).and_then(|day| day_start(&day)))
}

/// 从 `since` 起，哪些十分钟里收到过（任何群、任何人的）消息：返回各段的序号（Unix 秒 / 600），升序。
///
/// 机器人在很多群里，平时每十分钟总有人说话；连着几个小时一条都没收到，多半是机器人没在线。
/// 原始消息有保留期，更早的就查不到了。
pub async fn heard_slots(db: &DatabaseConnection, since: i64) -> Result<Vec<i64>, DbErr> {
    #[derive(FromQueryResult)]
    struct Slot {
        slot: i64,
    }
    let slots = Slot::find_by_statement(Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT DISTINCT time / 600 AS slot FROM message_records WHERE time >= ? ORDER BY slot",
        vec![since.into()],
    ))
    .all(db)
    .await?;
    Ok(slots.into_iter().map(|s| s.slot).collect())
}

fn day_of(at: i64) -> String {
    Local.timestamp_opt(at, 0).single().map_or_else(
        || "1970-01-01".to_string(),
        |t| t.format("%Y-%m-%d").to_string(),
    )
}

fn day_start(day: &str) -> Option<i64> {
    let date = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
        .earliest()
        .map(|t| t.timestamp())
}

fn day_end(day: &str) -> Option<i64> {
    let date = NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .ok()?
        .succ_opt()?;
    Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
        .earliest()
        .map(|t| t.timestamp() - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_bounds_wrap_the_whole_day() {
        let start = day_start("2026-10-01").unwrap();
        let end = day_end("2026-10-01").unwrap();
        assert_eq!(end - start, 86399);
        assert_eq!(day_of(start), "2026-10-01");
        assert_eq!(day_of(end), "2026-10-01");
        assert_eq!(day_of(end + 1), "2026-10-02");
    }
}
