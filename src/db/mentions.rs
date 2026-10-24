//! 艾特记录表（`message_mentions`）：谁在什么时候 @ 了谁。
//!
//! `message_records` 只存一条消息里有没有 @、有几个（`has_at` / `at_count`），
//! 存不下「@ 的是谁」——而「被艾特排行榜」问的恰恰是这个。本表由 recorder 在消息入库的
//! 同一事务里写入，一条 @ 记一行；@全体成员不算「别人」，不入表。
//!
//! 与 `member_bans` 一样：低频、按时间查询，直接扫表足够，不进聚合表。
//! 机器人自己（`self_id`）不入榜的过滤在查询里做：机器人被 @ 是家常便饭，
//! 混进「被艾特排行榜」会常年霸榜；机器人回复时 @ 群友也一样。

use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement};

/// 建表与索引。recorder 初始化时调用。
pub async fn init(db: &DatabaseConnection) -> Result<(), DbErr> {
    let backend = db.get_database_backend();
    let stmts = [
        r"CREATE TABLE IF NOT EXISTS message_mentions (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            platform    TEXT    NOT NULL DEFAULT '',
            self_id     TEXT    NOT NULL DEFAULT '',
            guild_id    TEXT    NOT NULL DEFAULT '',
            guild_name  TEXT    NOT NULL DEFAULT '',
            message_id  TEXT    NOT NULL DEFAULT '',
            atter_id    TEXT    NOT NULL DEFAULT '',
            atte_id     TEXT    NOT NULL DEFAULT '',
            atte_name   TEXT    NOT NULL DEFAULT '',
            time        INTEGER NOT NULL DEFAULT 0
        )",
        "CREATE INDEX IF NOT EXISTS idx_mentions_guild_time ON message_mentions (guild_id, time)",
        "CREATE INDEX IF NOT EXISTS idx_mentions_atte ON message_mentions (atte_id, time)",
        "CREATE INDEX IF NOT EXISTS idx_mentions_atter ON message_mentions (atter_id, time)",
    ];
    for sql in stmts {
        db.execute_raw(Statement::from_string(backend, sql.to_owned()))
            .await?;
    }
    Ok(())
}

/// 一次入库的一条 @。recorder 从消息段里解析出来，事务里写入。
pub struct MentionRow {
    pub guild_id: String,
    pub guild_name: String,
    pub message_id: String,
    pub atter_id: String,
    pub atte_id: String,
    pub atte_name: String,
    pub time: i64,
}

/// 与消息记录同事务写入几条 @。目标认不出（`atte_id` 空）就不记。
pub async fn insert<C>(
    txn: &C,
    platform: &str,
    self_id: &str,
    rows: &[MentionRow],
) -> Result<(), DbErr>
where
    C: ConnectionTrait,
{
    for row in rows {
        if row.atte_id.is_empty() {
            continue;
        }
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            r"INSERT INTO message_mentions
              (platform, self_id, guild_id, guild_name, message_id, atter_id, atte_id, atte_name, time)
              VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                platform.into(),
                self_id.into(),
                row.guild_id.clone().into(),
                row.guild_name.clone().into(),
                row.message_id.clone().into(),
                row.atter_id.clone().into(),
                row.atte_id.clone().into(),
                row.atte_name.clone().into(),
                row.time.into(),
            ],
        ))
        .await?;
    }
    Ok(())
}

/// 排行榜的一行：一个人（或一个群）与它的次数。`name` 可能是空的，交给调用方补。
pub struct MentionRank {
    pub id: String,
    pub name: String,
    pub count: i64,
}

/// 榜排谁：动手 @ 的人，还是被 @ 的人。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// 艾特别人：按发起者合计。
    Given,
    /// 被艾特：按目标合计。
    Received,
}

impl Direction {
    /// 主角是哪一列。
    fn person(self) -> &'static str {
        match self {
            Self::Given => "atter_id",
            Self::Received => "atte_id",
        }
    }
    /// 要排除机器人那一侧的哪一列。
    fn not_bot(self) -> &'static str {
        match self {
            Self::Given => "atte_id",
            Self::Received => "atter_id",
        }
    }
}

/// 本群（或跨群）的艾特榜。机器人两侧都不进榜。
pub async fn ranking(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    start_time: i64,
    end_time: i64,
    limit: u64,
    direction: Direction,
) -> Result<Vec<MentionRank>, DbErr> {
    let guild = guild_id
        .map(|g| format!(" AND guild_id = '{}'", g.replace('\'', "''")))
        .unwrap_or_default();
    let sql = format!(
        "SELECT {person} AS id, MAX(atte_name) AS name, COUNT(*) AS count \
         FROM message_mentions \
         WHERE time >= {start_time} AND time < {end_time} \
           AND {person} != '' AND {person} != self_id AND {not_bot} != self_id{guild} \
         GROUP BY {person} ORDER BY count DESC, id LIMIT {limit}",
        person = direction.person(),
        not_bot = direction.not_bot(),
    );
    query_ranks(db, sql).await
}

/// 「我的」视角：一个人名下（发起或被 @）按群合计，看自己在哪些群被 @ 得多。
pub async fn ranking_in_guilds(
    db: &DatabaseConnection,
    user_id: &str,
    start_time: i64,
    end_time: i64,
    limit: u64,
    direction: Direction,
) -> Result<Vec<MentionRank>, DbErr> {
    let user = user_id.replace('\'', "''");
    let sql = format!(
        "SELECT guild_id AS id, MAX(guild_name) AS name, COUNT(*) AS count \
         FROM message_mentions \
         WHERE time >= {start_time} AND time < {end_time} \
           AND {person} = '{user}' AND {person} != self_id AND {not_bot} != self_id \
         GROUP BY guild_id ORDER BY count DESC, id LIMIT {limit}",
        person = direction.person(),
        not_bot = direction.not_bot(),
    );
    query_ranks(db, sql).await
}

async fn query_ranks(db: &DatabaseConnection, sql: String) -> Result<Vec<MentionRank>, DbErr> {
    let rows = db
        .query_all_raw(Statement::from_string(db.get_database_backend(), sql))
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(MentionRank {
            id: row.try_get("", "id")?,
            name: row.try_get("", "name").unwrap_or_default(),
            count: row.try_get("", "count")?,
        });
    }
    Ok(out)
}

/// 榜上这个人的昵称与头像：取他在本群（或全局）最近一条发言里的；
/// 从没发过言就退回空，由调用方回 QQ 号与 QQ 头像。
pub async fn latest_identity(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    user_id: &str,
) -> Option<(String, String)> {
    let guild = guild_id
        .map(|g| format!(" AND guild_id = '{}'", g.replace('\'', "''")))
        .unwrap_or_default();
    let sql = format!(
        "SELECT member_nick, user_avatar FROM message_records \
         WHERE user_id = '{}'{} ORDER BY time DESC LIMIT 1",
        user_id.replace('\'', "''"),
        guild
    );
    let rows = db
        .query_all_raw(Statement::from_string(db.get_database_backend(), sql))
        .await
        .ok()?;
    let row = rows.first()?;
    let nick: String = row.try_get("", "member_nick").unwrap_or_default();
    let avatar: String = row.try_get("", "user_avatar").unwrap_or_default();
    Some((nick, avatar))
}

/// 一个人名下的总次数（榜外补位用）。
pub async fn total_for(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    user_id: &str,
    start_time: i64,
    end_time: i64,
    direction: Direction,
) -> Result<i64, DbErr> {
    let guild = guild_id
        .map(|g| format!(" AND guild_id = '{}'", g.replace('\'', "''")))
        .unwrap_or_default();
    let sql = format!(
        "SELECT COUNT(*) AS count FROM message_mentions \
         WHERE time >= {start_time} AND time < {end_time} \
           AND {person} = '{user}' AND {person} != self_id AND {not_bot} != self_id{guild}",
        person = direction.person(),
        not_bot = direction.not_bot(),
        user = user_id.replace('\'', "''"),
    );
    let rows = db
        .query_all_raw(Statement::from_string(db.get_database_backend(), sql))
        .await?;
    Ok(rows
        .first()
        .and_then(|row| row.try_get::<i64>("", "count").ok())
        .unwrap_or(0))
}
