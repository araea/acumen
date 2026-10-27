use crate::db::entity::{self, Entity as MessageLogs};
use crate::db::stats;
use sea_orm::sea_query::{Alias, Expr, Func};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, FromQueryResult,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Statement,
};

// ================= 常量定义 =================

const MAX_TEXT_CORPUS_LIMIT: u64 = 20000;
const MAX_RANKING_LIMIT: u64 = 50;
const MAX_TREND_LIMIT: u64 = 2000;
const MAX_GROUP_TREND_LIMIT: u64 = 10000;

// ================= 榜的口径 =================

/// 用户榜按什么数。每加一种，聚合表列名与原始表表达式两处要一起补：
/// 整日走聚合表、边界片段走原始表，两边口径必须一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserMetric {
    /// 发言条数。
    Messages,
    /// 动画表情（表情包）的条数。
    AnimEmoji,
    /// 纯文本消息条数：有文字，不含图片、表情包、语音、视频等。
    TextMessages,
    /// 文本字数：每条消息里文字段的字符数相加。
    TextChars,
}

impl UserMetric {
    /// 聚合表里的列名。
    fn agg_column(self) -> &'static str {
        match self {
            Self::Messages => "msg_count",
            Self::AnimEmoji => "anim_emoji_count",
            Self::TextMessages => "text_msg_count",
            Self::TextChars => "text_chars",
        }
    }

    /// 原始表上的取值表达式。
    fn raw_expr(self) -> &'static str {
        match self {
            Self::Messages => "COUNT(*)",
            Self::AnimEmoji => "SUM(CAST(is_anim_emoji AS integer))",
            // 与 db::stats::PURE_TEXT_PREDICATE 同一口径
            Self::TextMessages => {
                "SUM(CASE WHEN length > 0 AND image_count = 0 AND is_anim_emoji = 0 \
                 AND is_voice = 0 AND is_video = 0 AND is_music = 0 AND is_forward = 0 THEN 1 ELSE 0 END)"
            }
            Self::TextChars => "SUM(length)",
        }
    }

    /// 与主口径成对出现的另一面：字数榜上顺带写条数，文本消息榜上顺带写字数。
    /// 一张榜只回答「谁的数大」，两张摆在一起才看得出「一条顶几条」和「分几段发」。
    pub fn companion(self) -> Option<Self> {
        match self {
            Self::TextChars => Some(Self::TextMessages),
            Self::TextMessages => Some(Self::TextChars),
            _ => None,
        }
    }

    fn agg_companion(self) -> &'static str {
        self.companion().map_or("0", Self::agg_column)
    }

    fn raw_companion(self) -> &'static str {
        self.companion().map_or("0", Self::raw_expr)
    }
}

// ================= 数据结构 =================

/// 纯文本数据（用于生成词云）
#[derive(Debug, FromQueryResult)]
pub struct TextData {
    pub content_text: String,
}

/// 用户活跃排行（龙王榜）
#[derive(Debug, FromQueryResult)]
pub struct UserRanking {
    pub user_id: String,
    pub nickname: String,
    /// `user.avatar`，实现端没给时为空
    pub avatar: String,
    pub count: i64,
    /// 成对口径的另一面（字数↔条数）；不适用时为 0。
    pub side: i64,
}

/// 群组活跃排行
#[derive(Debug, FromQueryResult)]
pub struct GuildRanking {
    pub guild_id: String,
    pub guild_name: String,
    /// `guild.avatar`，实现端没给时为空
    pub avatar: String,
    pub count: i64,
}

/// 「我的」榜的一行：某个群里我的那个数，外加成对口径的另一面。
#[derive(Debug, FromQueryResult)]
pub struct GuildMetricRanking {
    pub guild_id: String,
    pub guild_name: String,
    pub avatar: String,
    pub count: i64,
    pub side: i64,
}

/// 每日消息量走势
#[derive(Debug, FromQueryResult)]
pub struct DailyTrend {
    pub date: String,
    pub count: i64,
}

/// 群组每日消息量走势 (用于多线图)
#[derive(Debug, FromQueryResult)]
pub struct GuildTrend {
    pub date: String,
    pub guild_name: String,
    pub count: i64,
}

/// 消息类型走势数据点
#[derive(Debug, FromQueryResult)]
pub struct MessageTypeTrend {
    pub date: String,
    pub text: i64,
    pub image: i64,
    pub voice: i64,
    pub video: i64,
    pub anim_emoji: i64,
    pub face: i64,
}

/// 小时活跃分布
#[derive(Debug, FromQueryResult)]
pub struct HourlyActivity {
    pub hour: i32,
    pub count: i64,
}

/// 星期活跃分布
#[derive(Debug, FromQueryResult)]
pub struct WeekdayActivity {
    pub weekday: i32,
    pub count: i64,
}

/// 星期×小时 热力分布数据
#[derive(Debug, FromQueryResult)]
pub struct HeatmapData {
    pub weekday: i32,
    pub hour: i32,
    pub count: i64,
}

/// 消息类型统计
#[derive(Debug, FromQueryResult)]
pub struct MessageTypeStats {
    pub text: i64,
    pub image: i64,
    pub voice: i64,
    pub video: i64,
    pub anim_emoji: i64,
    pub face: i64,
}

// ================= 查询路由辅助 =================

// 查询策略说明：
// - 群/全局维度的跨多日统计 → 聚合表 message_stats_daily / message_user_stats_daily
//   （每群每日一行，查询"今年"仅读 ≤366 行，不再扫描原始消息）；
// - 非整日边界片段（近7天/近30天的起止时刻落在一天中间）→ 回退原始表小范围查询后合并；
// - 用户维度单用户的统计 → 原始表（idx_records_user_time 覆盖索引本身高效，
//   且单用户消息量远小于全群）；
// - 需要消息内容的查询（词云语料）→ 原始表（聚合表不含内容，无法替代）。

/// 构建非整日边界片段的 UNION ALL 子查询列表。
///
/// 每个边界片段独立成段（而非 OR 条件合并）：实测 OR 条件会让查询计划退化为
/// 按 guild_id 前缀扫描全群索引条目，而分段后每段各自走 (guild_id, time)
/// 索引范围扫描，边界片段通常只有一个残日，开销可忽略。
///
/// `tmpl` 接收片段的 (start, end) 时间戳，返回一段完整的
/// `UNION ALL SELECT ... FROM message_records WHERE time >= ? AND time < ? ...` 子查询。
fn partial_union_subqueries(
    partials: &[(i64, i64)],
    mut tmpl: impl FnMut(i64, i64) -> String,
) -> String {
    partials
        .iter()
        .map(|(s, e)| tmpl(*s, *e))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 聚合 SQL 的绑定参数。ID 是字符串，只能绑定、不能拼进 SQL：
/// 片段里写 `?`，值按它在 SQL 文本里出现的先后收集。
#[derive(Default)]
struct Params(Vec<sea_orm::Value>);

impl Params {
    /// 群过滤条件片段；没指定群时为空。
    fn guild(&mut self, guild_id: Option<&str>) -> &'static str {
        match guild_id {
            Some(id) => {
                self.0.push(id.into());
                " AND guild_id = ?"
            }
            None => "",
        }
    }

    /// 绑定一个值，返回占位符。
    fn bind(&mut self, value: &str) -> &'static str {
        self.0.push(value.into());
        "?"
    }

    fn statement(self, db: &DatabaseConnection, sql: String) -> Statement {
        Statement::from_sql_and_values(db.get_database_backend(), sql, self.0)
    }
}

async fn scalar_from_sql(db: &DatabaseConnection, stmt: Statement) -> Result<i64, DbErr> {
    let row = db.query_one_raw(stmt).await?;
    match row {
        Some(r) => Ok(r.try_get::<i64>("", "c")?),
        None => Ok(0),
    }
}

// ================= 查询函数（公共接口） =================

/// 获取指定时间范围内的纯文本内容列表
///
/// 始终查询原始表：词云需要消息原文（分词结果），聚合表无法替代；
/// 上限 MAX_TEXT_CORPUS_LIMIT 条，实测极端规模（最活跃群 73 万行）下 ~25ms。
pub async fn get_text_corpus(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    user_id: Option<&str>,
    start_time: i64,
    end_time: i64,
) -> Result<Vec<String>, DbErr> {
    raw_text_corpus(db, guild_id, user_id, start_time, end_time).await
}

/// 获取活跃用户排行（龙王榜）
///
/// 整日部分走 message_user_stats_daily（群×用户×日聚合），
/// 非整日边界回退原始表，SQL 内 UNION ALL 合并后统一排序。
pub async fn get_user_ranking(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    start_time: i64,
    end_time: i64,
    limit: u64,
    metric: UserMetric,
) -> Result<Vec<UserRanking>, DbErr> {
    let sr = stats::split_range(start_time, end_time);
    let Some((from, to)) = sr.full_days else {
        return raw_user_ranking(db, guild_id, start_time, end_time, limit, metric).await;
    };

    let n = limit.min(MAX_RANKING_LIMIT);
    let mut params = Params::default();
    let g = params.guild(guild_id);
    let (raw_count, raw_side) = (metric.raw_expr(), metric.raw_companion());
    let partials = partial_union_subqueries(&sr.partials, |s, e| {
        let g = params.guild(guild_id);
        format!(
            "UNION ALL SELECT user_id, MAX(member_nick) AS nickname, MAX(user_avatar) AS avatar, \
             {raw_count} AS cnt, {raw_side} AS side \
             FROM message_records WHERE time >= {s} AND time < {e}{g} GROUP BY user_id"
        )
    });
    let (col, side) = (metric.agg_column(), metric.agg_companion());
    let sql = format!(
        "SELECT user_id, MAX(nickname) AS nickname, MAX(avatar) AS avatar, SUM(cnt) AS count, \
           SUM(side) AS side FROM (\
           SELECT user_id, member_nick AS nickname, user_avatar AS avatar, {col} AS cnt, {side} AS side \
           FROM message_user_stats_daily \
           WHERE stat_date >= '{from}' AND stat_date <= '{to}'{g} \
           {partials} \
         ) GROUP BY user_id ORDER BY count DESC LIMIT {n}"
    );
    UserRanking::find_by_statement(params.statement(db, sql))
        .all(db)
        .await
}

/// 某个人在区间内某一口径的合计。
///
/// 榜外补位用：没进前 N 名也要给个数，不然「★」那一行只能写个零。
pub async fn get_user_metric_total(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    user_id: &str,
    start_time: i64,
    end_time: i64,
    metric: UserMetric,
) -> Result<i64, DbErr> {
    let mut query = MessageLogs::find()
        .select_only()
        .column_as(
            Expr::cust(format!("COALESCE({}, 0)", metric.raw_expr())),
            "count",
        )
        .filter(entity::Column::Time.gte(start_time))
        .filter(entity::Column::Time.lt(end_time))
        .filter(entity::Column::UserId.eq(user_id));

    if let Some(gid) = guild_id {
        query = query.filter(entity::Column::GuildId.eq(gid));
    }

    let row: Option<i64> = query.into_tuple().one(db).await?;
    Ok(row.unwrap_or(0))
}

/// 获取群组活跃排行
///
/// 整日部分走 message_stats_daily，非整日边界回退原始表。
pub async fn get_guild_ranking(
    db: &DatabaseConnection,
    start_time: i64,
    end_time: i64,
    limit: u64,
) -> Result<Vec<GuildRanking>, DbErr> {
    let sr = stats::split_range(start_time, end_time);
    let Some((from, to)) = sr.full_days else {
        return raw_guild_ranking(db, start_time, end_time, limit).await;
    };

    let n = limit.min(MAX_RANKING_LIMIT);
    let partials = partial_union_subqueries(&sr.partials, |s, e| {
        format!(
            "UNION ALL SELECT guild_id, MAX(guild_name) AS guild_name, COUNT(*) AS cnt \
             FROM message_records WHERE time >= {s} AND time < {e} AND guild_id != '' GROUP BY guild_id"
        )
    });
    let sql = format!(
        "SELECT guild_id, MAX(guild_name) AS guild_name, \
           (SELECT MAX(guild_avatar) FROM message_stats_daily a WHERE a.guild_id = t.guild_id) AS avatar, \
           SUM(cnt) AS count FROM (\
           SELECT guild_id, guild_name, msg_count AS cnt \
           FROM message_stats_daily \
           WHERE guild_id != '' AND stat_date >= '{from}' AND stat_date <= '{to}' \
           {partials} \
         ) t GROUP BY guild_id ORDER BY count DESC LIMIT {n}"
    );
    GuildRanking::find_by_statement(Statement::from_string(db.get_database_backend(), sql))
        .all(db)
        .await
}

/// 获取用户参与的群组排行 ("我的...排行")
///
/// 整日部分走 message_user_stats_daily（idx_user_stats_user_date 索引），
/// 非整日边界回退原始表。
pub async fn get_user_guild_participation_ranking(
    db: &DatabaseConnection,
    user_id: &str,
    start_time: i64,
    end_time: i64,
    limit: u64,
    metric: UserMetric,
) -> Result<Vec<GuildMetricRanking>, DbErr> {
    let sr = stats::split_range(start_time, end_time);
    let Some((from, to)) = sr.full_days else {
        return raw_user_guild_participation_ranking(
            db, user_id, start_time, end_time, limit, metric,
        )
        .await;
    };

    let n = limit.min(MAX_RANKING_LIMIT);
    let mut params = Params::default();
    let user = params.bind(user_id);
    let (raw_count, raw_side) = (metric.raw_expr(), metric.raw_companion());
    let partials = partial_union_subqueries(&sr.partials, |s, e| {
        let user = params.bind(user_id);
        format!(
            "UNION ALL SELECT guild_id, MAX(guild_name) AS guild_name, {raw_count} AS cnt, {raw_side} AS side \
             FROM message_records WHERE time >= {s} AND time < {e} \
             AND user_id = {user} AND guild_id != '' GROUP BY guild_id"
        )
    });
    let (col, side) = (metric.agg_column(), metric.agg_companion());
    let sql = format!(
        "SELECT guild_id, MAX(guild_name) AS guild_name, \
           (SELECT MAX(guild_avatar) FROM message_stats_daily a WHERE a.guild_id = t.guild_id) AS avatar, \
           SUM(cnt) AS count, SUM(side) AS side FROM (\
           SELECT guild_id, guild_name, {col} AS cnt, {side} AS side \
           FROM message_user_stats_daily \
           WHERE user_id = {user} AND guild_id != '' \
             AND stat_date >= '{from}' AND stat_date <= '{to}' \
           {partials} \
         ) t GROUP BY guild_id ORDER BY count DESC LIMIT {n}"
    );
    GuildMetricRanking::find_by_statement(params.statement(db, sql))
        .all(db)
        .await
}

/// 获取每日消息量走势 (总)
///
/// 用户维度走原始表（单用户消息量小，idx_records_user_time 高效）；
/// 群/全局维度整日部分走 message_stats_daily，边界片段回退原始表。
pub async fn get_daily_trend(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    user_id: Option<&str>,
    start_time: i64,
    end_time: i64,
) -> Result<Vec<DailyTrend>, DbErr> {
    if user_id.is_some() {
        return raw_daily_trend(db, guild_id, user_id, start_time, end_time).await;
    }

    let sr = stats::split_range(start_time, end_time);
    let Some((from, to)) = sr.full_days else {
        return raw_daily_trend(db, guild_id, None, start_time, end_time).await;
    };

    let mut merged: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();

    // 整日部分：每群每日一行的聚合表，按日求和
    let mut params = Params::default();
    let g = params.guild(guild_id);
    let sql = format!(
        "SELECT stat_date AS date, SUM(msg_count) AS count FROM message_stats_daily \
         WHERE stat_date >= '{from}' AND stat_date <= '{to}'{g} \
         GROUP BY stat_date ORDER BY stat_date"
    );
    let rows: Vec<DailyTrend> = DailyTrend::find_by_statement(params.statement(db, sql))
        .all(db)
        .await?;
    for r in rows {
        merged.insert(r.date, r.count);
    }

    // 边界片段：回退原始表
    for (s, e) in &sr.partials {
        for r in raw_daily_trend(db, guild_id, None, *s, *e).await? {
            *merged.entry(r.date).or_insert(0) += r.count;
        }
    }

    Ok(merged
        .into_iter()
        .take(MAX_TREND_LIMIT as usize)
        .map(|(date, count)| DailyTrend { date, count })
        .collect())
}

/// 获取各群每日消息量走势 (用于"所有群...走势")
///
/// 按小时聚合（by_hour）时区间必为单日（≤24h），直接走原始表；
/// 按日聚合时整日部分走 message_stats_daily（自带最新群名）。
pub async fn get_daily_trend_by_guild(
    db: &DatabaseConnection,
    start_time: i64,
    end_time: i64,
    by_hour: bool,
) -> Result<Vec<GuildTrend>, DbErr> {
    if by_hour {
        return raw_daily_trend_by_guild(db, start_time, end_time, true).await;
    }

    let sr = stats::split_range(start_time, end_time);
    let Some((from, to)) = sr.full_days else {
        return raw_daily_trend_by_guild(db, start_time, end_time, false).await;
    };

    // (群名, 日期) → 消息数
    let mut merged: std::collections::HashMap<(String, String), i64> =
        std::collections::HashMap::new();

    let sql = format!(
        "SELECT stat_date AS date, guild_name, msg_count AS count \
         FROM message_stats_daily \
         WHERE guild_id != '' AND stat_date >= '{from}' AND stat_date <= '{to}' \
         ORDER BY stat_date"
    );
    let rows: Vec<GuildTrend> =
        GuildTrend::find_by_statement(Statement::from_string(db.get_database_backend(), sql))
            .all(db)
            .await?;
    for r in rows {
        merged.insert((r.guild_name, r.date), r.count);
    }

    for (s, e) in &sr.partials {
        for r in raw_daily_trend_by_guild(db, *s, *e, false).await? {
            *merged.entry((r.guild_name, r.date)).or_insert(0) += r.count;
        }
    }

    let mut result: Vec<GuildTrend> = merged
        .into_iter()
        .map(|((guild_name, date), count)| GuildTrend {
            date,
            guild_name,
            count,
        })
        .collect();
    result.sort_by(|a, b| a.date.cmp(&b.date));
    result.truncate(MAX_GROUP_TREND_LIMIT as usize);
    Ok(result)
}

/// 获取消息类型走势 (多维度)
///
/// 用户维度走原始表；群/全局维度整日部分走聚合表（按日/按小时），
/// 按小时聚合仅用于 ≤24h 区间（无完整自然日），自动回退原始表。
pub async fn get_message_type_trend(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    user_id: Option<&str>,
    start_time: i64,
    end_time: i64,
    by_hour: bool,
) -> Result<Vec<MessageTypeTrend>, DbErr> {
    // 按小时聚合需要小时粒度，聚合表为日粒度，直接走原始表
    // （调用方仅在区间 ≤24h 时启用 by_hour，原始表扫描量很小）
    if by_hour || user_id.is_some() {
        return raw_message_type_trend(db, guild_id, user_id, start_time, end_time, by_hour).await;
    }

    let sr = stats::split_range(start_time, end_time);
    let Some((from, to)) = sr.full_days else {
        return raw_message_type_trend(db, guild_id, None, start_time, end_time, false).await;
    };

    let mut merged: std::collections::BTreeMap<String, MessageTypeTrend> =
        std::collections::BTreeMap::new();

    // 整日部分：聚合表逐日累计 6 类计数
    let mut params = Params::default();
    let g = params.guild(guild_id);
    let sql = format!(
        "SELECT stat_date AS date, SUM(text_count) AS text, SUM(image_count) AS image, \
         SUM(voice_count) AS voice, SUM(video_count) AS video, \
         SUM(anim_emoji_count) AS anim_emoji, SUM(face_count) AS face \
         FROM message_stats_daily \
         WHERE stat_date >= '{from}' AND stat_date <= '{to}'{g} \
         GROUP BY stat_date ORDER BY stat_date"
    );
    let rows: Vec<MessageTypeTrend> =
        MessageTypeTrend::find_by_statement(params.statement(db, sql))
            .all(db)
            .await?;
    for r in rows {
        merged.insert(
            r.date.clone(),
            MessageTypeTrend {
                date: r.date,
                text: r.text,
                image: r.image,
                voice: r.voice,
                video: r.video,
                anim_emoji: r.anim_emoji,
                face: r.face,
            },
        );
    }

    // 边界片段：回退原始表并逐字段累加
    for (s, e) in &sr.partials {
        for r in raw_message_type_trend(db, guild_id, None, *s, *e, false).await? {
            let entry = merged.entry(r.date.clone()).or_insert(MessageTypeTrend {
                date: r.date.clone(),
                text: 0,
                image: 0,
                voice: 0,
                video: 0,
                anim_emoji: 0,
                face: 0,
            });
            entry.text += r.text;
            entry.image += r.image;
            entry.voice += r.voice;
            entry.video += r.video;
            entry.anim_emoji += r.anim_emoji;
            entry.face += r.face;
        }
    }

    Ok(merged
        .into_values()
        .take(MAX_TREND_LIMIT as usize)
        .collect())
}

/// 获取24小时活跃时段分布
///
/// 始终走原始表：仅用于 ≤24h 区间（"今日/昨日"走势图），
/// time_hour 已物化为列，扫描量小。
pub async fn get_hourly_activity(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    user_id: Option<&str>,
    start_time: i64,
    end_time: i64,
) -> Result<Vec<HourlyActivity>, DbErr> {
    raw_hourly_activity(db, guild_id, user_id, start_time, end_time).await
}

/// 获取星期活跃分布
///
/// 始终走原始表：星期维度无聚合表，且该查询当前未被调用方使用。
pub async fn get_weekday_activity(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    start_time: i64,
    end_time: i64,
) -> Result<Vec<WeekdayActivity>, DbErr> {
    raw_weekday_activity(db, guild_id, start_time, end_time).await
}

/// 获取 星期×小时 的热力分布数据
///
/// 始终走原始表：二维时间维度无聚合表，且该查询当前未被调用方使用。
pub async fn get_heatmap_data(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    start_time: i64,
    end_time: i64,
) -> Result<Vec<HeatmapData>, DbErr> {
    raw_heatmap_data(db, guild_id, start_time, end_time).await
}

/// 获取消息类型统计 (总计)
///
/// 用户维度走原始表；群/全局维度整日部分走聚合表（6 类计数已预聚合），
/// 边界片段回退原始表后逐字段相加。
pub async fn get_message_type_stats(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    user_id: Option<&str>,
    start_time: i64,
    end_time: i64,
) -> Result<MessageTypeStats, DbErr> {
    if user_id.is_some() {
        return raw_message_type_stats(db, guild_id, user_id, start_time, end_time).await;
    }

    let sr = stats::split_range(start_time, end_time);
    let Some((from, to)) = sr.full_days else {
        return raw_message_type_stats(db, guild_id, None, start_time, end_time).await;
    };

    let mut params = Params::default();
    let g = params.guild(guild_id);
    let sql = format!(
        "SELECT COALESCE(SUM(text_count), 0) AS text, COALESCE(SUM(image_count), 0) AS image, \
         COALESCE(SUM(voice_count), 0) AS voice, COALESCE(SUM(video_count), 0) AS video, \
         COALESCE(SUM(anim_emoji_count), 0) AS anim_emoji, \
         COALESCE(SUM(face_count), 0) AS face \
         FROM message_stats_daily \
         WHERE stat_date >= '{from}' AND stat_date <= '{to}'{g}"
    );
    let mut result = MessageTypeStats::find_by_statement(params.statement(db, sql))
        .one(db)
        .await?
        .unwrap_or(MessageTypeStats {
            text: 0,
            image: 0,
            voice: 0,
            video: 0,
            anim_emoji: 0,
            face: 0,
        });

    for (s, e) in &sr.partials {
        let p = raw_message_type_stats(db, guild_id, None, *s, *e).await?;
        result.text += p.text;
        result.image += p.image;
        result.voice += p.voice;
        result.video += p.video;
        result.anim_emoji += p.anim_emoji;
        result.face += p.face;
    }

    Ok(result)
}

/// 获取指定条件下的活跃用户数量 (DISTINCT user_id)
///
/// 始终走原始表：DISTINCT 语义跨日不去重（用户多日活跃只算一次），
/// 无法由按日聚合行直接求和；调用方（定时推送）均使用 今日/昨日/上周/上月
/// 等小范围区间，idx_records_group_time 索引下耗时可忽略。
pub async fn get_active_user_count(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    start_time: i64,
    end_time: i64,
) -> Result<u64, DbErr> {
    raw_active_user_count(db, guild_id, start_time, end_time).await
}

/// 「我在所有群」的某个数：与 get_user_metric_total 同一口径，只是不含私聊
/// （guild_id 为空字符串的那些行）。「我的」走势与它旁边的「群覆盖」说的是同一批群。
pub async fn get_user_metric_total_in_groups(
    db: &DatabaseConnection,
    user_id: &str,
    start_time: i64,
    end_time: i64,
    metric: UserMetric,
) -> Result<i64, DbErr> {
    let row: Option<i64> = MessageLogs::find()
        .select_only()
        .column_as(
            Expr::cust(format!("COALESCE({}, 0)", metric.raw_expr())),
            "count",
        )
        .filter(entity::Column::Time.gte(start_time))
        .filter(entity::Column::Time.lt(end_time))
        .filter(entity::Column::UserId.eq(user_id))
        .filter(entity::Column::GuildId.ne(""))
        .into_tuple()
        .one(db)
        .await?;
    Ok(row.unwrap_or(0))
}

/// 获取指定条件下的消息数量
///
/// 用户维度走原始表（覆盖索引计数）；群/全局维度整日部分走聚合表
/// （查询某群"今年"消息数只需读 ≤366 行聚合记录求和），
/// 边界片段回退原始表小范围计数。
pub async fn get_message_count(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    user_id: Option<&str>,
    start_time: i64,
    end_time: i64,
) -> Result<u64, DbErr> {
    if user_id.is_some() {
        return raw_message_count(db, guild_id, user_id, start_time, end_time).await;
    }

    let sr = stats::split_range(start_time, end_time);
    let Some((from, to)) = sr.full_days else {
        return raw_message_count(db, guild_id, None, start_time, end_time).await;
    };

    let mut params = Params::default();
    let g = params.guild(guild_id);
    let sql = format!(
        "SELECT COALESCE(SUM(msg_count), 0) AS c FROM message_stats_daily \
         WHERE stat_date >= '{from}' AND stat_date <= '{to}'{g}"
    );
    let agg = scalar_from_sql(db, params.statement(db, sql)).await?.max(0) as u64;

    let mut total = agg;
    for (s, e) in &sr.partials {
        total += raw_message_count(db, guild_id, None, *s, *e).await?;
    }
    Ok(total)
}

/// 个人走势的时间粒度。按小时用于单日（今日/昨日），按月用于跨度很长的「总」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrendGranularity {
    Hour,
    Day,
    Month,
}

impl TrendGranularity {
    /// SQL 里日期键的写法；与图表侧生成桶标签的格式必须逐字相同。
    fn sql_format(self) -> &'static str {
        match self {
            // 按小时分桶：分钟恒为 00。写成 %H:%M 会按分钟分组，图上一个点
            // 一小时，落格只剩整点那一条，其余全被当成「没说话」。
            Self::Hour => "%H:00",
            Self::Day => "%Y-%m-%d",
            Self::Month => "%Y-%m",
        }
    }
}

/// 某个人的数值走势：按小时 / 天 / 月聚合。
///
/// 单个人的消息量远小于全群，直接走原始表（idx_records_user_time），不必碰聚合表；
/// 口径与排行榜共用 [`UserMetric`]，一张走势图和一个榜说的是同一件事。
pub async fn get_user_metric_trend(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    user_id: &str,
    start_time: i64,
    end_time: i64,
    granularity: TrendGranularity,
    metric: UserMetric,
) -> Result<Vec<DailyTrend>, DbErr> {
    raw_metric_trend(db, guild_id, Some(user_id), start_time, end_time, granularity, metric).await
}

/// 一个群（或全部）的数值走势，口径与 [`get_user_metric_trend`] 相同，只是不含「谁」。
///
/// 「我的」报表拿它当对比线。整群的数据量比一个人大得多，所以按天、按月时整日部分
/// 走聚合表（`message_user_stats_daily` 按日把所有人摞起来就是全群），只有边界那两天
/// 回原始表；按小时只可能是一天之内，直接查原始表。
pub async fn get_group_metric_trend(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    start_time: i64,
    end_time: i64,
    granularity: TrendGranularity,
    metric: UserMetric,
) -> Result<Vec<DailyTrend>, DbErr> {
    if granularity == TrendGranularity::Hour {
        return raw_metric_trend(db, guild_id, None, start_time, end_time, granularity, metric).await;
    }

    let sr = stats::split_range(start_time, end_time);
    let Some((from, to)) = sr.full_days else {
        return raw_metric_trend(db, guild_id, None, start_time, end_time, granularity, metric).await;
    };

    let date_expr = if granularity == TrendGranularity::Month {
        "substr(stat_date, 1, 7)"
    } else {
        "stat_date"
    };
    let mut params = Params::default();
    let g = params.guild(guild_id);
    let sql = format!(
        "SELECT {date_expr} AS date, SUM({}) AS count FROM message_user_stats_daily WHERE stat_date >= '{from}' AND stat_date <= '{to}'{g} GROUP BY date ORDER BY date",
        metric.agg_column()
    );
    let rows: Vec<DailyTrend> = DailyTrend::find_by_statement(params.statement(db, sql))
        .all(db)
        .await?;

    // 边界片段回原始表；它们与整日部分不重叠，按日期键累加即可。
    let mut merged: std::collections::BTreeMap<String, i64> =
        rows.into_iter().map(|r| (r.date, r.count)).collect();
    for (s, e) in &sr.partials {
        for r in raw_metric_trend(db, guild_id, None, *s, *e, granularity, metric).await? {
            *merged.entry(r.date).or_insert(0) += r.count;
        }
    }

    Ok(merged
        .into_iter()
        .take(MAX_TREND_LIMIT as usize)
        .map(|(date, count)| DailyTrend { date, count })
        .collect())
}

/// 原始表上的数值走势：`user_id` 给了就只算这个人。按小时只用于单日，
/// 按天/按月用于没有聚合表可用的时候。
async fn raw_metric_trend(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    user_id: Option<&str>,
    start_time: i64,
    end_time: i64,
    granularity: TrendGranularity,
    metric: UserMetric,
) -> Result<Vec<DailyTrend>, DbErr> {
    let time_expr = Expr::cust(format!(
        "strftime('{}', datetime(time, 'unixepoch', 'localtime'))",
        granularity.sql_format()
    ));

    let mut query = MessageLogs::find()
        .select_only()
        .column_as(time_expr.clone(), "date")
        .column_as(Expr::cust(metric.raw_expr()), "count")
        .filter(entity::Column::Time.gte(start_time))
        .filter(entity::Column::Time.lt(end_time));

    if let Some(gid) = guild_id {
        query = query.filter(entity::Column::GuildId.eq(gid));
    }
    if let Some(uid) = user_id {
        query = query.filter(entity::Column::UserId.eq(uid));
    }
    // 「我的」跨群走势只数群里的发言：私聊记录也在这个表里（guild_id 为空），
    // 混进来会让「跨群」的合计比旁边「群覆盖」里那几个群的和还大。
    if user_id.is_some() && guild_id.is_none() {
        query = query.filter(entity::Column::GuildId.ne(""));
    }

    query
        .group_by(time_expr)
        .order_by_asc(Expr::custom_keyword(Alias::new("date")))
        .limit(MAX_TREND_LIMIT)
        .into_model::<DailyTrend>()
        .all(db)
        .await
}

/// 我在一个群里的名次：返回（名次，同榜人数，全群合计）。
///
/// 名次用「比我多的人数 + 1」：数一样的并列，不做跳号；合计用来算「占全群多少」。
/// 只数在区间里说过话的人——没说话的人没有名次，也不该被算进分母。
pub async fn get_user_rank_in_guild(
    db: &DatabaseConnection,
    guild_id: &str,
    user_id: &str,
    start_time: i64,
    end_time: i64,
    metric: UserMetric,
) -> Result<(i64, i64, i64), DbErr> {
    let mine =
        get_user_metric_total(db, Some(guild_id), user_id, start_time, end_time, metric).await?;

    // 绑定顺序按 SQL 文本里占位符出现的先后：SELECT 里的比较值在前，WHERE 里的群号在后。
    let mut params = Params::default();
    params.0.push(mine.into());
    let g = params.guild(Some(guild_id));
    let sql = format!(
        "SELECT COALESCE(SUM(CASE WHEN v > ? THEN 1 ELSE 0 END), 0) + 1 AS pos, \
                COUNT(*) AS n, COALESCE(SUM(v), 0) AS total \
         FROM (SELECT user_id, {} AS v FROM message_records \
               WHERE time >= {start_time} AND time < {end_time}{g} \
               GROUP BY user_id HAVING v > 0)",
        metric.raw_expr()
    );

    let row = db.query_one_raw(params.statement(db, sql)).await?;
    match row {
        Some(r) => Ok((
            r.try_get::<i64>("", "pos")?,
            r.try_get::<i64>("", "n")?,
            r.try_get::<i64>("", "total")?,
        )),
        None => Ok((1, 0, 0)),
    }
}

/// 某个人第一条消息的时间；从没说过话返回 None。
///
/// 「我的总发言走势」按自然月分桶，起点得是这个人的第一条消息所在的月，
/// 不能从库里第一行算起——那会把别人的历史算进我的横轴。
///
/// 给了群号就只在这个群里找（本群我的总），没给则看所有群、不含私聊。
pub async fn get_user_first_message_time(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    user_id: &str,
) -> Result<Option<i64>, DbErr> {
    let mut query = MessageLogs::find()
        .select_only()
        .column_as(Expr::cust("MIN(time)"), "first")
        .filter(entity::Column::UserId.eq(user_id));
    query = match guild_id {
        Some(gid) => query.filter(entity::Column::GuildId.eq(gid)),
        None => query.filter(entity::Column::GuildId.ne("")),
    };
    let row: Option<(Option<i64>,)> = query.into_tuple().one(db).await?;
    Ok(row.and_then(|(first,)| first))
}

// ================= 原始表查询（回退路径 / 原有实现） =================

async fn raw_text_corpus(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    user_id: Option<&str>,
    start_time: i64,
    end_time: i64,
) -> Result<Vec<String>, DbErr> {
    let mut query = MessageLogs::find()
        .select_only()
        .column_as(entity::Column::Tokens, "content_text")
        .filter(entity::Column::Time.gte(start_time))
        .filter(entity::Column::Time.lt(end_time))
        .filter(entity::Column::Tokens.ne(""));

    if let Some(gid) = guild_id {
        query = query.filter(entity::Column::GuildId.eq(gid));
    }
    if let Some(uid) = user_id {
        query = query.filter(entity::Column::UserId.eq(uid));
    }

    let results: Vec<TextData> = query
        .limit(MAX_TEXT_CORPUS_LIMIT)
        .into_model()
        .all(db)
        .await?;
    Ok(results.into_iter().map(|d| d.content_text).collect())
}

async fn raw_user_ranking(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    start_time: i64,
    end_time: i64,
    limit: u64,
    metric: UserMetric,
) -> Result<Vec<UserRanking>, DbErr> {
    let mut query = MessageLogs::find()
        .select_only()
        .column(entity::Column::UserId)
        .column_as(
            Expr::from(Func::max(Expr::col(entity::Column::MemberNick))),
            "nickname",
        )
        .column_as(
            Expr::from(Func::max(Expr::col(entity::Column::UserAvatar))),
            "avatar",
        )
        .column_as(Expr::cust(metric.raw_expr()), "count")
        .column_as(Expr::cust(metric.raw_companion()), "side")
        .filter(entity::Column::Time.gte(start_time))
        .filter(entity::Column::Time.lt(end_time));

    if let Some(gid) = guild_id {
        query = query.filter(entity::Column::GuildId.eq(gid));
    }

    query
        .group_by(entity::Column::UserId)
        .order_by_desc(Expr::custom_keyword(Alias::new("count")))
        .limit(limit.min(MAX_RANKING_LIMIT))
        .into_model::<UserRanking>()
        .all(db)
        .await
}

async fn raw_guild_ranking(
    db: &DatabaseConnection,
    start_time: i64,
    end_time: i64,
    limit: u64,
) -> Result<Vec<GuildRanking>, DbErr> {
    MessageLogs::find()
        .select_only()
        .column(entity::Column::GuildId)
        .column_as(
            Expr::from(Func::max(Expr::col(entity::Column::GuildName))),
            "guild_name",
        )
        .column_as(
            Expr::from(Func::max(Expr::col(entity::Column::GuildAvatar))),
            "avatar",
        )
        .column_as(
            Expr::from(Func::count(Expr::col(entity::Column::Id))),
            "count",
        )
        .filter(entity::Column::Time.gte(start_time))
        .filter(entity::Column::Time.lt(end_time))
        .filter(entity::Column::GuildId.ne(""))
        .group_by(entity::Column::GuildId)
        .order_by_desc(Expr::custom_keyword(Alias::new("count")))
        .limit(limit.min(MAX_RANKING_LIMIT))
        .into_model::<GuildRanking>()
        .all(db)
        .await
}

async fn raw_user_guild_participation_ranking(
    db: &DatabaseConnection,
    user_id: &str,
    start_time: i64,
    end_time: i64,
    limit: u64,
    metric: UserMetric,
) -> Result<Vec<GuildMetricRanking>, DbErr> {
    MessageLogs::find()
        .select_only()
        .column(entity::Column::GuildId)
        .column_as(
            Expr::from(Func::max(Expr::col(entity::Column::GuildName))),
            "guild_name",
        )
        .column_as(
            Expr::from(Func::max(Expr::col(entity::Column::GuildAvatar))),
            "avatar",
        )
        .column_as(Expr::cust(metric.raw_expr()), "count")
        .column_as(Expr::cust(metric.raw_companion()), "side")
        .filter(entity::Column::Time.gte(start_time))
        .filter(entity::Column::Time.lt(end_time))
        .filter(entity::Column::UserId.eq(user_id))
        .filter(entity::Column::GuildId.ne(""))
        .group_by(entity::Column::GuildId)
        .order_by_desc(Expr::custom_keyword(Alias::new("count")))
        .limit(limit.min(MAX_RANKING_LIMIT))
        .into_model::<GuildMetricRanking>()
        .all(db)
        .await
}

async fn raw_daily_trend(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    user_id: Option<&str>,
    start_time: i64,
    end_time: i64,
) -> Result<Vec<DailyTrend>, DbErr> {
    let date_expr = Expr::cust("strftime('%Y-%m-%d', datetime(time, 'unixepoch', 'localtime'))");

    let mut query = MessageLogs::find()
        .select_only()
        .column_as(date_expr.clone(), "date")
        .column_as(
            Expr::from(Func::count(Expr::col(entity::Column::Id))),
            "count",
        )
        .filter(entity::Column::Time.gte(start_time))
        .filter(entity::Column::Time.lt(end_time));

    if let Some(gid) = guild_id {
        query = query.filter(entity::Column::GuildId.eq(gid));
    }
    if let Some(uid) = user_id {
        query = query.filter(entity::Column::UserId.eq(uid));
    }

    query
        .group_by(date_expr)
        .order_by_asc(Expr::custom_keyword(Alias::new("date")))
        .limit(MAX_TREND_LIMIT)
        .into_model::<DailyTrend>()
        .all(db)
        .await
}

async fn raw_daily_trend_by_guild(
    db: &DatabaseConnection,
    start_time: i64,
    end_time: i64,
    by_hour: bool,
) -> Result<Vec<GuildTrend>, DbErr> {
    let time_expr = if by_hour {
        Expr::cust("strftime('%H:00', datetime(time, 'unixepoch', 'localtime'))")
    } else {
        Expr::cust("strftime('%Y-%m-%d', datetime(time, 'unixepoch', 'localtime'))")
    };

    MessageLogs::find()
        .select_only()
        .column_as(time_expr.clone(), "date")
        .column(entity::Column::GuildName)
        .column_as(
            Expr::from(Func::count(Expr::col(entity::Column::Id))),
            "count",
        )
        .filter(entity::Column::Time.gte(start_time))
        .filter(entity::Column::Time.lt(end_time))
        .filter(entity::Column::GuildId.ne(""))
        .group_by(time_expr)
        .group_by(entity::Column::GuildId)
        .order_by_asc(Expr::custom_keyword(Alias::new("date")))
        .limit(MAX_GROUP_TREND_LIMIT)
        .into_model::<GuildTrend>()
        .all(db)
        .await
}

async fn raw_message_type_trend(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    user_id: Option<&str>,
    start_time: i64,
    end_time: i64,
    by_hour: bool,
) -> Result<Vec<MessageTypeTrend>, DbErr> {
    // 聚合时间键 (按小时 或 按天)
    let time_expr = if by_hour {
        Expr::cust("strftime('%H:00', datetime(time, 'unixepoch', 'localtime'))")
    } else {
        Expr::cust("strftime('%Y-%m-%d', datetime(time, 'unixepoch', 'localtime'))")
    };

    // 统计逻辑定义：
    // 1. 文本: length > 0
    let text_expr = Expr::cust("SUM(CASE WHEN length > 0 THEN 1 ELSE 0 END)");
    // 2. 动画表情: is_anim_emoji = true
    let anim_expr = Expr::from(Func::sum(Expr::col(entity::Column::IsAnimEmoji)));
    // 3. 图片: image_count - is_anim_emoji
    let image_expr = Expr::cust("SUM(image_count) - SUM(is_anim_emoji)");
    // 4. 语音: is_voice
    let voice_expr = Expr::from(Func::sum(Expr::col(entity::Column::IsVoice)));
    // 5. 视频: is_video
    let video_expr = Expr::from(Func::sum(Expr::col(entity::Column::IsVideo)));
    // 6. 表情 (Face): face_count
    let face_expr = Expr::from(Func::sum(Expr::col(entity::Column::FaceCount)));

    let mut query = MessageLogs::find()
        .select_only()
        .column_as(time_expr.clone(), "date")
        .column_as(text_expr, "text")
        .column_as(image_expr, "image")
        .column_as(voice_expr, "voice")
        .column_as(video_expr, "video")
        .column_as(anim_expr, "anim_emoji")
        .column_as(face_expr, "face")
        .filter(entity::Column::Time.gte(start_time))
        .filter(entity::Column::Time.lt(end_time));

    if let Some(gid) = guild_id {
        query = query.filter(entity::Column::GuildId.eq(gid));
    }
    if let Some(uid) = user_id {
        query = query.filter(entity::Column::UserId.eq(uid));
    }

    query
        .group_by(time_expr)
        .order_by_asc(Expr::custom_keyword(Alias::new("date")))
        .limit(MAX_TREND_LIMIT)
        .into_model::<MessageTypeTrend>()
        .all(db)
        .await
}

async fn raw_hourly_activity(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    user_id: Option<&str>,
    start_time: i64,
    end_time: i64,
) -> Result<Vec<HourlyActivity>, DbErr> {
    let mut query = MessageLogs::find()
        .select_only()
        .column_as(entity::Column::TimeHour, "hour")
        .column_as(
            Expr::from(Func::count(Expr::col(entity::Column::Id))),
            "count",
        )
        .filter(entity::Column::Time.gte(start_time))
        .filter(entity::Column::Time.lt(end_time));

    if let Some(gid) = guild_id {
        query = query.filter(entity::Column::GuildId.eq(gid));
    }
    if let Some(uid) = user_id {
        query = query.filter(entity::Column::UserId.eq(uid));
    }

    query
        .group_by(entity::Column::TimeHour)
        .order_by_asc(entity::Column::TimeHour)
        .into_model::<HourlyActivity>()
        .all(db)
        .await
}

async fn raw_weekday_activity(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    start_time: i64,
    end_time: i64,
) -> Result<Vec<WeekdayActivity>, DbErr> {
    let mut query = MessageLogs::find()
        .select_only()
        .column_as(entity::Column::TimeWeekday, "weekday")
        .column_as(
            Expr::from(Func::count(Expr::col(entity::Column::Id))),
            "count",
        )
        .filter(entity::Column::Time.gte(start_time))
        .filter(entity::Column::Time.lt(end_time));

    if let Some(gid) = guild_id {
        query = query.filter(entity::Column::GuildId.eq(gid));
    }

    query
        .group_by(entity::Column::TimeWeekday)
        .order_by_asc(entity::Column::TimeWeekday)
        .into_model::<WeekdayActivity>()
        .all(db)
        .await
}

async fn raw_heatmap_data(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    start_time: i64,
    end_time: i64,
) -> Result<Vec<HeatmapData>, DbErr> {
    let mut query = MessageLogs::find()
        .select_only()
        .column_as(entity::Column::TimeWeekday, "weekday")
        .column_as(entity::Column::TimeHour, "hour")
        .column_as(
            Expr::from(Func::count(Expr::col(entity::Column::Id))),
            "count",
        )
        .filter(entity::Column::Time.gte(start_time))
        .filter(entity::Column::Time.lt(end_time));

    if let Some(gid) = guild_id {
        query = query.filter(entity::Column::GuildId.eq(gid));
    }

    query
        .group_by(entity::Column::TimeWeekday)
        .group_by(entity::Column::TimeHour)
        .into_model::<HeatmapData>()
        .all(db)
        .await
}

async fn raw_message_type_stats(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    user_id: Option<&str>,
    start_time: i64,
    end_time: i64,
) -> Result<MessageTypeStats, DbErr> {
    let text_expr = Expr::cust("SUM(CASE WHEN length > 0 THEN 1 ELSE 0 END)");
    let anim_expr = Expr::from(Func::sum(Expr::col(entity::Column::IsAnimEmoji)));
    let image_expr = Expr::cust("SUM(image_count) - SUM(is_anim_emoji)");
    let voice_expr = Expr::from(Func::sum(Expr::col(entity::Column::IsVoice)));
    let video_expr = Expr::from(Func::sum(Expr::col(entity::Column::IsVideo)));
    let face_expr = Expr::from(Func::sum(Expr::col(entity::Column::FaceCount)));

    let mut query = MessageLogs::find()
        .select_only()
        .column_as(text_expr, "text")
        .column_as(image_expr, "image")
        .column_as(voice_expr, "voice")
        .column_as(video_expr, "video")
        .column_as(anim_expr, "anim_emoji")
        .column_as(face_expr, "face")
        .filter(entity::Column::Time.gte(start_time))
        .filter(entity::Column::Time.lt(end_time));

    if let Some(gid) = guild_id {
        query = query.filter(entity::Column::GuildId.eq(gid));
    }
    if let Some(uid) = user_id {
        query = query.filter(entity::Column::UserId.eq(uid));
    }

    let result = query.into_model::<MessageTypeStats>().one(db).await?;

    Ok(result.unwrap_or(MessageTypeStats {
        text: 0,
        image: 0,
        voice: 0,
        video: 0,
        anim_emoji: 0,
        face: 0,
    }))
}

async fn raw_active_user_count(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    start_time: i64,
    end_time: i64,
) -> Result<u64, DbErr> {
    let mut query = MessageLogs::find()
        .select_only()
        .column_as(Expr::cust("COUNT(DISTINCT user_id)"), "count")
        .filter(entity::Column::Time.gte(start_time))
        .filter(entity::Column::Time.lt(end_time));

    if let Some(gid) = guild_id {
        query = query.filter(entity::Column::GuildId.eq(gid));
    }

    let result: Option<i64> = query.into_tuple().one(db).await?;
    Ok(result.unwrap_or(0) as u64)
}

async fn raw_message_count(
    db: &DatabaseConnection,
    guild_id: Option<&str>,
    user_id: Option<&str>,
    start_time: i64,
    end_time: i64,
) -> Result<u64, DbErr> {
    let mut query = MessageLogs::find()
        .filter(entity::Column::Time.gte(start_time))
        .filter(entity::Column::Time.lt(end_time));

    if let Some(gid) = guild_id {
        query = query.filter(entity::Column::GuildId.eq(gid));
    }
    if let Some(uid) = user_id {
        query = query.filter(entity::Column::UserId.eq(uid));
    }

    query.count(db).await
}
