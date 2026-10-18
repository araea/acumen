//! 消息统计聚合表：`message_stats_daily`（群×日）与 `message_user_stats_daily`（群×用户×日）。
//!
//! ## 为什么需要聚合表
//! 基准测试（300 万行原始消息、最活跃群 73 万行/180 天）表明：
//! - 纯计数（`COUNT(*)`）可走 `idx_records_guild_time` 覆盖索引，只数索引条目，不读消息行；
//! - 但消息类型统计、每日走势、各类排行榜等聚合查询必须**扫描区间内全部原始消息行**，
//!   "今年/总"范围下单次可达 0.5~1 秒。
//!
//! 聚合表按"群×日"预聚合（100 群 × 180 天仅约 1.8 万行），查询某群今年消息数
//! 只需读取该群 ≤366 行聚合记录求和，不再扫描原始消息。
//!
//! ## 数据同步机制（与 message_records 保持一致）
//! 1. **写入**：recorder 在消息入库的**同一事务**内 UPSERT 当日聚合行（计数 +1、分类计数累加），
//!    两者要么同时成功要么同时回滚，不存在中间态；
//! 2. **删除**：项目中对 message_records 的唯一删除路径是 recorder 每日保留期批量清理
//!    （无逐条删除接口；消息撤回走 Satori API，不删除本地记录）。聚合行**不随原始数据删除**，
//!    历史统计（如"去年消息数"）在原始消息过期后依然可查；
//! 3. **自愈**：每日 4:00 清理任务开始前、以及每次进程启动时，重建近 7 天的聚合行，
//!    修复异常场景（手工改库、旧版本数据、时钟回拨）导致的漂移。
//!
//! ## 时间口径
//! `stat_date` 为本地时区自然日（'YYYY-MM-DD'）。聚合行按消息自身的 `time` 字段归属日期，
//! 晚到的历史消息会正确累计到其所属日期的聚合行上。

use chrono::{Duration, Local, NaiveDate, TimeZone};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement};

use crate::{info, warn};

const LOG_TARGET: &str = "Database/Stats";

/// 自愈重建的天数窗口
pub const SELF_HEAL_DAYS: i64 = 7;

/// 纯文本消息的口径：有文字，且不含图片、动画表情、语音、视频、音乐与合并转发。
///
/// 与 [`MessageStatsDelta::is_text_message`] 必须一致：这里算历史聚合，那里算实时增量，
/// 两处口径一分岔，同一张榜在「今天」和「昨天」就会对不上。
/// 小表情（face）与 @ 是文字消息的一部分，不妨碍它算一条文字消息。
const PURE_TEXT_PREDICATE: &str = "length > 0 AND image_count = 0 AND is_anim_emoji = 0 \
     AND is_voice = 0 AND is_video = 0 AND is_music = 0 AND is_forward = 0";

// ================= 初始化与维护 =================

/// 初始化聚合表：建表 + 启动自愈。
/// 依赖 message_records 表已存在（由 recorder::init 先行创建）。
pub async fn init(db: &DatabaseConnection) -> Result<(), DbErr> {
    let migrated = create_tables(db).await?;
    if migrated {
        // 刚给老库补上文字两列，历史聚合行里这两列还是 0：趁原始消息还在，
        // 把仍被完整覆盖的那些日子一次性重算，免得字数榜一上来就缺一段。
        match backfill_text_columns(db).await {
            Ok(()) => {
                info!(target: LOG_TARGET, "文字统计补列完成，已回填历史聚合行。");
                return Ok(());
            }
            Err(e) => warn!(target: LOG_TARGET, "文字统计回填失败，改为只自愈近 7 天: {}", e),
        }
    }
    self_heal_recent(db).await
}

/// 建表并补列；返回是否给已有的表补过列。
async fn create_tables(db: &DatabaseConnection) -> Result<bool, DbErr> {
    let backend = db.get_database_backend();
    let stmts = [
        // 群 × 日 聚合（含 6 类消息类型计数，口径与原实时查询完全一致：
        // image = SUM(image_count) - SUM(is_anim_emoji)，此处直接存净额）
        r"CREATE TABLE IF NOT EXISTS message_stats_daily (
            guild_id      TEXT    NOT NULL,
            stat_date     TEXT    NOT NULL,
            guild_name    TEXT    NOT NULL DEFAULT '',
            guild_avatar  TEXT    NOT NULL DEFAULT '',
            msg_count     INTEGER NOT NULL DEFAULT 0,
            text_count    INTEGER NOT NULL DEFAULT 0,
            image_count   INTEGER NOT NULL DEFAULT 0,
            voice_count   INTEGER NOT NULL DEFAULT 0,
            video_count   INTEGER NOT NULL DEFAULT 0,
            anim_emoji_count INTEGER NOT NULL DEFAULT 0,
            face_count    INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (guild_id, stat_date)
        )",
        // 群 × 用户 × 日 聚合（服务发言榜/表情包榜/我的群排行等）
        r"CREATE TABLE IF NOT EXISTS message_user_stats_daily (
            guild_id      TEXT    NOT NULL,
            stat_date     TEXT    NOT NULL,
            user_id       TEXT    NOT NULL,
            member_nick   TEXT    NOT NULL DEFAULT '',
            user_avatar   TEXT    NOT NULL DEFAULT '',
            guild_name    TEXT    NOT NULL DEFAULT '',
            msg_count     INTEGER NOT NULL DEFAULT 0,
            anim_emoji_count INTEGER NOT NULL DEFAULT 0,
            text_chars    INTEGER NOT NULL DEFAULT 0,
            text_msg_count INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (guild_id, stat_date, user_id)
        )",
        // 覆盖"按用户查日期范围"的查询（我的群组参与排行）
        "CREATE INDEX IF NOT EXISTS idx_user_stats_user_date \
         ON message_user_stats_daily (user_id, stat_date, guild_id)",
    ];

    for sql in stmts {
        db.execute_raw(Statement::from_string(backend, sql.to_owned()))
            .await?;
    }

    // CREATE TABLE IF NOT EXISTS 不会给已有的表加列：老库要逐列补。
    // 两列都是「有就跳过」，多跑几次也无害。
    let existing = table_columns(db, "message_user_stats_daily").await?;
    let mut migrated = false;
    for (name, ddl) in [
        ("text_chars", "INTEGER NOT NULL DEFAULT 0"),
        ("text_msg_count", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !existing.iter().any(|c| c == name) {
            db.execute_raw(Statement::from_string(
                backend,
                format!("ALTER TABLE message_user_stats_daily ADD COLUMN {name} {ddl}"),
            ))
            .await?;
            migrated = true;
        }
    }
    Ok(migrated)
}

/// 表的列名。用 PRAGMA 而不是硬猜：补列前先看它在不在。
async fn table_columns(db: &DatabaseConnection, table: &str) -> Result<Vec<String>, DbErr> {
    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            format!("PRAGMA table_info({table})"),
        ))
        .await?;
    rows.iter()
        .map(|row| row.try_get::<String>("", "name"))
        .collect()
}

/// 补列后的回填：重建原始消息仍完整覆盖的所有日期。
/// 保留期之外的聚合行是冻结的历史，`rebuild_range` 自己会避开。
async fn backfill_text_columns(db: &DatabaseConnection) -> Result<(), DbErr> {
    let raw_rows = scalar_i64(db, "SELECT COUNT(*) AS c FROM message_records").await?;
    if raw_rows == 0 {
        return Ok(());
    }
    let min_time = scalar_i64(db, "SELECT MIN(time) AS c FROM message_records").await?;
    rebuild_range(db, date_of_ts(min_time), Local::now().date_naive()).await?;
    Ok(())
}

/// 自愈：重建近 SELF_HEAL_DAYS 天的聚合行。
/// 由每日 4:00 保留期清理任务与进程启动时调用。
pub async fn self_heal_recent(db: &DatabaseConnection) -> Result<(), DbErr> {
    let today = Local::now().date_naive();
    let from = today - Duration::days(SELF_HEAL_DAYS - 1);
    match rebuild_range(db, from, today).await {
        Ok(n) => {
            if n > 0 {
                info!(target: LOG_TARGET, "统计聚合自愈完成，重建 {} 天聚合行。", n);
            }
            Ok(())
        }
        Err(e) => {
            warn!(target: LOG_TARGET, "统计聚合自愈失败: {}", e);
            Err(e)
        }
    }
}

/// 重建 [from, to] 闭区间内的聚合行（返回重建的天数）。
/// 只重建原始数据仍完整覆盖的日期：保留期之外的聚合行是"冻结的历史"，
/// 原始消息已删除，不能被重建，也不能被误删。
async fn rebuild_range<C>(db: &C, from: NaiveDate, to: NaiveDate) -> Result<i64, DbErr>
where
    C: ConnectionTrait,
{
    let raw_rows = scalar_i64(db, "SELECT COUNT(*) AS c FROM message_records").await?;
    if raw_rows == 0 {
        return Ok(0);
    }
    let min_time = scalar_i64(db, "SELECT MIN(time) AS c FROM message_records").await?;

    // 原始数据最早只能覆盖到 min_time 所在日期（且当日可能不完整），
    // 从能被原始数据完整覆盖的第一天开始重建，避免误删冻结行。
    let min_date = date_of_ts(min_time);
    let min_cover_from = if min_time <= midnight_of(min_date) {
        min_date
    } else {
        min_date + Duration::days(1)
    };
    let eff_from = min_cover_from.max(from);
    if eff_from > to {
        return Ok(0);
    }

    let backend = db.get_database_backend();
    let f = eff_from.format("%Y-%m-%d").to_string();
    let t = to.format("%Y-%m-%d").to_string();
    let t_from = midnight_of(eff_from);
    let t_to = midnight_of(to) + 86_400;

    db.execute_raw(Statement::from_string(
        backend,
        format!("DELETE FROM message_stats_daily WHERE stat_date >= '{f}' AND stat_date <= '{t}'"),
    ))
    .await?;
    db.execute_raw(Statement::from_string(
        backend,
        format!(
            "DELETE FROM message_user_stats_daily WHERE stat_date >= '{f}' AND stat_date <= '{t}'"
        ),
    ))
    .await?;

    db.execute_raw(Statement::from_string(
        backend,
        format!(
            r"INSERT INTO message_stats_daily
               (guild_id, stat_date, guild_name, guild_avatar, msg_count, text_count, image_count,
                voice_count, video_count, anim_emoji_count, face_count)
               SELECT guild_id,
                      strftime('%Y-%m-%d', datetime(time, 'unixepoch', 'localtime')),
                      MAX(guild_name), MAX(guild_avatar), COUNT(*),
                      SUM(CASE WHEN length > 0 THEN 1 ELSE 0 END),
                      SUM(image_count) - SUM(is_anim_emoji),
                      SUM(is_voice), SUM(is_video), SUM(is_anim_emoji), SUM(face_count)
               FROM message_records
               WHERE time >= {t_from} AND time < {t_to}
               GROUP BY guild_id, 2"
        ),
    ))
    .await?;

    db.execute_raw(Statement::from_string(
        backend,
        format!(
            r"INSERT INTO message_user_stats_daily
               (guild_id, stat_date, user_id, member_nick, user_avatar, guild_name, msg_count, anim_emoji_count,
                text_chars, text_msg_count)
               SELECT guild_id,
                      strftime('%Y-%m-%d', datetime(time, 'unixepoch', 'localtime')),
                      user_id, MAX(member_nick), MAX(user_avatar), MAX(guild_name), COUNT(*), SUM(is_anim_emoji),
                      SUM(length), SUM(CASE WHEN {PURE_TEXT_PREDICATE} THEN 1 ELSE 0 END)
               FROM message_records
               WHERE time >= {t_from} AND time < {t_to}
               GROUP BY guild_id, 2, user_id"
        ),
    ))
    .await?;

    Ok((to - eff_from).num_days() + 1)
}

// ================= 写入同步 =================

/// 单条消息的聚合增量
pub struct MessageStatsDelta {
    pub guild_id: String,
    pub guild_name: String,
    pub guild_avatar: String,
    pub user_id: String,
    pub member_nick: String,
    pub user_avatar: String,
    pub time: i64,
    pub length: i32,
    pub image_count: i32,
    pub is_anim_emoji: bool,
    pub is_voice: bool,
    pub is_video: bool,
    pub is_music: bool,
    pub is_forward: bool,
    pub face_count: i32,
}

impl MessageStatsDelta {
    /// 这一条算不算纯文本消息（口径见 [`PURE_TEXT_PREDICATE`]）。
    pub fn is_text_message(&self) -> bool {
        self.length > 0
            && self.image_count == 0
            && !self.is_anim_emoji
            && !self.is_voice
            && !self.is_video
            && !self.is_music
            && !self.is_forward
    }
}

/// 消息入库后在**同一事务内**调用：将计数累加到当日聚合行。
/// 接受 `ConnectionTrait` 以便复用外层事务（与消息插入原子绑定）。
pub async fn upsert_message_stats<C>(db: &C, d: &MessageStatsDelta) -> Result<(), DbErr>
where
    C: ConnectionTrait,
{
    let backend = db.get_database_backend();
    let date = date_str_of(d.time);

    // 口径与原实时查询保持一致
    let text = i64::from(d.length > 0);
    let anim = i64::from(d.is_anim_emoji);
    let image = d.image_count as i64 - anim;
    let voice = i64::from(d.is_voice);
    let video = i64::from(d.is_video);
    let face = d.face_count as i64;

    let stmt = Statement::from_sql_and_values(
        backend,
        r"INSERT INTO message_stats_daily
           (guild_id, stat_date, guild_name, guild_avatar, msg_count, text_count, image_count,
            voice_count, video_count, anim_emoji_count, face_count)
           VALUES (?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?)
           ON CONFLICT (guild_id, stat_date) DO UPDATE SET
             guild_name = excluded.guild_name,
             guild_avatar = excluded.guild_avatar,
             msg_count = message_stats_daily.msg_count + 1,
             text_count = message_stats_daily.text_count + excluded.text_count,
             image_count = message_stats_daily.image_count + excluded.image_count,
             voice_count = message_stats_daily.voice_count + excluded.voice_count,
             video_count = message_stats_daily.video_count + excluded.video_count,
             anim_emoji_count = message_stats_daily.anim_emoji_count + excluded.anim_emoji_count,
             face_count = message_stats_daily.face_count + excluded.face_count",
        vec![
            d.guild_id.clone().into(),
            date.clone().into(),
            d.guild_name.clone().into(),
            d.guild_avatar.clone().into(),
            text.into(),
            image.into(),
            voice.into(),
            video.into(),
            anim.into(),
            face.into(),
        ],
    );
    db.execute_raw(stmt).await?;

    let chars = i64::from(d.length.max(0));
    let text_msg = i64::from(d.is_text_message());

    let stmt = Statement::from_sql_and_values(
        backend,
        r"INSERT INTO message_user_stats_daily
           (guild_id, stat_date, user_id, member_nick, user_avatar, guild_name, msg_count, anim_emoji_count,
            text_chars, text_msg_count)
           VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, ?)
           ON CONFLICT (guild_id, stat_date, user_id) DO UPDATE SET
             member_nick = excluded.member_nick,
             user_avatar = excluded.user_avatar,
             guild_name = excluded.guild_name,
             msg_count = message_user_stats_daily.msg_count + 1,
             anim_emoji_count = message_user_stats_daily.anim_emoji_count + excluded.anim_emoji_count,
             text_chars = message_user_stats_daily.text_chars + excluded.text_chars,
             text_msg_count = message_user_stats_daily.text_msg_count + excluded.text_msg_count",
        vec![
            d.guild_id.clone().into(),
            date.into(),
            d.user_id.clone().into(),
            d.member_nick.clone().into(),
            d.user_avatar.clone().into(),
            d.guild_name.clone().into(),
            anim.into(),
            chars.into(),
            text_msg.into(),
        ],
    );
    db.execute_raw(stmt).await?;

    Ok(())
}

// ================= 时间范围切分 =================

/// 将时间戳区间切分为"完整自然日 + 非整日边界片段"。
///
/// 完整自然日部分走聚合表（O(天数) 行）；非整日边界（如"近7天"的起止时刻落在
/// 一天中间）回退原始表做小范围查询。所有查询区间均由 `db::utils::get_time_range`
/// 生成，起点为本地零点或当前时刻，因此边界片段最多 2 个（首日头、末日尾）。
pub struct SplitRange {
    /// 完整覆盖的自然日范围（闭区间，'YYYY-MM-DD'）；None 表示无完整自然日
    pub full_days: Option<(String, String)>,
    /// 非整日边界片段（时间戳区间，左闭右开），最多 2 个
    pub partials: Vec<(i64, i64)>,
}

pub fn split_range(start: i64, end: i64) -> SplitRange {
    if end <= start {
        return SplitRange {
            full_days: None,
            partials: Vec::new(),
        };
    }

    let d0 = date_of_ts(start);
    let d1 = date_of_ts(end - 1);
    let m0 = midnight_of(d0);

    // start >= m0 恒成立；相等表示起点恰为零点，首日完整
    let full_from = if start <= m0 {
        d0
    } else {
        d0 + Duration::days(1)
    };
    // end 为右开边界；末日 d1 完整当且仅当 end 不早于次日零点
    let full_to = if end >= midnight_of(d1) + 86_400 {
        d1
    } else {
        d1 - Duration::days(1)
    };

    if full_from > full_to {
        // 区间不足两个零点边界（如"今日"），整体回退原始表
        return SplitRange {
            full_days: None,
            partials: vec![(start, end)],
        };
    }

    let mut partials = Vec::new();
    let head_end = midnight_of(full_from);
    if start < head_end {
        partials.push((start, head_end));
    }
    let tail_start = midnight_of(full_to) + 86_400;
    if end > tail_start {
        partials.push((tail_start, end));
    }

    SplitRange {
        full_days: Some((
            full_from.format("%Y-%m-%d").to_string(),
            full_to.format("%Y-%m-%d").to_string(),
        )),
        partials,
    }
}

// ================= 内部工具 =================

async fn scalar_i64<C>(db: &C, sql: &str) -> Result<i64, DbErr>
where
    C: ConnectionTrait,
{
    let stmt = Statement::from_string(db.get_database_backend(), sql.to_owned());
    let row = db.query_one_raw(stmt).await?;
    match row {
        Some(r) => Ok(r.try_get::<i64>("", "c")?),
        None => Ok(0),
    }
}

fn date_of_ts(ts: i64) -> NaiveDate {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| dt.date_naive())
        .unwrap_or_default()
}

fn midnight_of(d: NaiveDate) -> i64 {
    d.and_hms_opt(0, 0, 0)
        .and_then(|ndt| Local.from_local_datetime(&ndt).single())
        .map_or(0, |dt| dt.timestamp())
}

fn date_str_of(ts: i64) -> String {
    date_of_ts(ts).format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(length: i32, image_count: i32, anim: bool, media: Media) -> MessageStatsDelta {
        MessageStatsDelta {
            guild_id: String::new(),
            guild_name: String::new(),
            guild_avatar: String::new(),
            user_id: String::new(),
            member_nick: String::new(),
            user_avatar: String::new(),
            time: 0,
            length,
            image_count,
            is_anim_emoji: anim,
            is_voice: media.voice,
            is_video: media.video,
            is_music: media.music,
            is_forward: media.forward,
            face_count: 0,
        }
    }

    #[derive(Default)]
    struct Media {
        voice: bool,
        video: bool,
        music: bool,
        forward: bool,
    }

    #[test]
    fn text_message_excludes_media() {
        let plain = |length| delta(length, 0, false, Media::default());
        assert!(plain(5).is_text_message());
        // 空白消息（长度 0）不算
        assert!(!plain(0).is_text_message());
        // 只有图片、表情包、语音、视频、音乐、合并转发，都不算
        assert!(!delta(0, 1, false, Media::default()).is_text_message());
        assert!(!delta(0, 1, true, Media::default()).is_text_message());
        for media in [
            Media {
                voice: true,
                ..Media::default()
            },
            Media {
                video: true,
                ..Media::default()
            },
            Media {
                music: true,
                ..Media::default()
            },
            Media {
                forward: true,
                ..Media::default()
            },
        ] {
            assert!(!delta(0, 0, false, media).is_text_message());
        }
        // 文字带图：算字数，但不算一条纯文本消息
        assert!(!delta(3, 1, false, Media::default()).is_text_message());
    }
}
