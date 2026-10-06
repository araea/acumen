use crate::adapters::satori::LockedWriter;
use crate::event::{Context, EventType};
use crate::plugins::{PluginConfig, PluginError, Receipt, get_config_or_default};

/// 统一日志 target
const LOG_TARGET: &str = "Plugin/Recorder";
use chrono::{Datelike, Duration, Local, TimeZone, Timelike};
use futures_util::future::BoxFuture;
use jieba_rs::Jieba;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ConnectionTrait, Schema, Set, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use simd_json::OwnedValue;
use simd_json::base::{ValueAsArray, ValueAsScalar};
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsScalar};
use std::sync::OnceLock;
use toml::Value;

pub mod entity {
    use sea_orm::entity::prelude::*;

    /// 一条消息记录。资源字段照 Satori 协议取名取型：所有 ID 都是字符串，
    /// 同一个 ID 只在它的 `platform` 里唯一；`self_id` 是收到（或发出）这条消息的登录账号。
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "message_records")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        /// `login.platform`
        pub platform: String,
        /// `login.user.id`
        pub self_id: String,
        /// `message.id`
        pub message_id: String,
        /// `channel.id`
        pub channel_id: String,
        /// `channel.type`：0 群聊（TEXT），1 私聊（DIRECT）
        pub channel_type: i32,
        /// `guild.id`；私聊为空串
        pub guild_id: String,
        /// `guild.name`
        pub guild_name: String,
        /// `guild.avatar`
        pub guild_avatar: String,
        /// `user.id`
        pub user_id: String,
        /// `user.name`
        pub user_name: String,
        /// `member.nick`（群名片），没有时同 `user.name`
        pub member_nick: String,
        /// `member.avatar`，没有时同 `user.avatar`
        pub user_avatar: String,
        /// 成员角色（owner / admin / member / self）
        pub member_role: String,

        pub content_rich: String, // 富文本摘要
        pub tokens: String,       // 分词结果（空格分隔）

        pub is_reply: bool,
        pub length: i32,
        pub time: i64,
        pub time_hour: i32,
        pub time_weekday: i32,

        pub has_image: bool,     // 是否包含图片
        pub image_count: i32,    // 图片数量
        pub is_anim_emoji: bool, // 是否包含动画表情/表情包

        pub has_at: bool,  // 是否包含At
        pub at_count: i32, // At数量

        pub face_count: i32, // 小表情(face)数量

        pub is_voice: bool, // 是否是语音
        pub is_video: bool, // 是否是视频
        pub is_music: bool, // 是否是音乐分享

        pub is_rps: bool,  // 是否是猜拳
        pub is_dice: bool, // 是否是骰子
        pub is_poke: bool, // 是否是戳一戳

        pub is_forward: bool, // 是否是合并转发
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

use entity::ActiveModel as RecordActiveModel;
use entity::Entity as RecordEntity;

// 全局 Jieba 实例
static JIEBA: OnceLock<Jieba> = OnceLock::new();

fn get_jieba() -> &'static Jieba {
    JIEBA.get_or_init(Jieba::new)
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct RecorderConfig {
    enabled: bool,
    /// 是否连机器人自己发的消息一起记。搭话的风格分析要用到自己的发言，所以默认开。
    record_self: bool,
    /// 原始消息保留多少天，超期清理；0 表示不清理。清理不影响已算出的统计结果。
    retention_days: i64,
}

impl Default for RecorderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            record_self: true,
            retention_days: 180,
        }
    }
}

impl PluginConfig for RecorderConfig {
    const NAME: &'static str = "recorder";
}


pub fn init(ctx: Context) -> BoxFuture<'static, Result<(), PluginError>> {
    Box::pin(async move {
        let db = &ctx.db;
        let builder = db.get_database_backend();
        let schema = Schema::new(builder);

        // 1. 创建表
        let mut create_table_stmt = schema.create_table_from_entity(RecordEntity);
        create_table_stmt.if_not_exists();

        let stmt = builder.build(&create_table_stmt);
        if let Err(e) = db.execute_raw(stmt).await {
            warn!(target: LOG_TARGET, "Init table error (ignore if exists): {}", e);
        }

        // 2. 创建索引
        let indexes = vec![
            sea_orm::sea_query::Index::create()
                .name("idx_records_guild_time")
                .table(RecordEntity)
                .col(entity::Column::GuildId)
                .col(entity::Column::Time)
                .if_not_exists()
                .to_owned(),
            sea_orm::sea_query::Index::create()
                .name("idx_records_guild_user_time")
                .table(RecordEntity)
                .col(entity::Column::GuildId)
                .col(entity::Column::UserId)
                .col(entity::Column::Time)
                .if_not_exists()
                .to_owned(),
            sea_orm::sea_query::Index::create()
                .name("idx_records_user_time")
                .table(RecordEntity)
                .col(entity::Column::UserId)
                .col(entity::Column::Time)
                .if_not_exists()
                .to_owned(),
            sea_orm::sea_query::Index::create()
                .name("idx_records_time")
                .table(RecordEntity)
                .col(entity::Column::Time)
                .if_not_exists()
                .to_owned(),
        ];

        for idx in indexes {
            let stmt = builder.build(&idx);
            let _ = db.execute_raw(stmt).await;
        }

        // 3. 初始化统计聚合表（建表并自愈近 7 天）
        if let Err(e) = crate::db::stats::init(db).await {
            warn!(target: LOG_TARGET, "统计聚合表初始化失败: {}", e);
        }

        // 4. 注册每日数据清理任务
        let scheduler = ctx.scheduler.clone();
        let db_clone = ctx.db.clone();
        let config_clone = ctx.config.clone();

        scheduler.add_daily_at(4, 0, 0, move || {
            let db = db_clone.clone();
            let cfg = config_clone.clone();
            async move {
                if !cfg.read().unwrap().plugins.get("recorder").and_then(|v| v.get("enabled")).and_then(Value::as_bool).unwrap_or(false) { return; }
                // 统计聚合自愈：重建近 7 天聚合行，修复异常场景下的计数漂移
                // （保留期之外的聚合行是冻结的历史，不会被触碰）
                if let Err(e) = crate::db::stats::self_heal_recent(&db).await {
                    warn!(target: LOG_TARGET, "统计聚合自愈失败: {}", e);
                }

                let retention_days = {
                    let guard = cfg.read().unwrap();
                    if let Some(v) = guard.plugins.get("recorder") {
                        v.get("retention_days").and_then(|x| x.as_integer()).unwrap_or(180)
                    } else {
                        180
                    }
                };

                if retention_days <= 0 {
                    info!(target: LOG_TARGET, "数据保留天数设置为 0 或负数，跳过清理。");
                    return;
                }

                let cutoff_time = Local::now() - Duration::days(retention_days);
                let timestamp = cutoff_time.timestamp();

                info!(target: LOG_TARGET, "开始清理 {} 天前的数据 (Time < {})...", retention_days, timestamp);

                // 注意：只删除原始消息记录，统计聚合表 (message_stats_daily /
                // message_user_stats_daily) 中对应日期的聚合行保留——历史统计
                // （如"去年消息数"）在原始数据过期后依然可查。
                let delete_sql = format!("DELETE FROM message_records WHERE time < {}", timestamp);
                let res = db.execute_raw(Statement::from_string(sea_orm::DatabaseBackend::Sqlite, delete_sql)).await;

                match res {
                    Ok(exec_res) => {
                        let rows = exec_res.rows_affected();
                        info!(target: LOG_TARGET, "已清理 {} 条过期消息记录。", rows);
                        if rows > 0 {
                            // 增量回收 freelist 页(依赖 db.rs 的 auto_vacuum=INCREMENTAL)，
                            // 避免全量 VACUUM 需要约 2× 文件大小的临时空间且长时间锁库。
                            info!(target: LOG_TARGET, "正在增量回收数据库空间 (incremental_vacuum)...");
                            if let Err(e) = db.execute_raw(Statement::from_string(sea_orm::DatabaseBackend::Sqlite, "PRAGMA incremental_vacuum;".to_owned())).await {
                                warn!(target: LOG_TARGET, "incremental_vacuum 执行失败: {}", e);
                            } else {
                                info!(target: LOG_TARGET, "数据库空间回收完成。");
                            }
                        }
                    },
                    Err(e) => {
                        error!(target: LOG_TARGET, "清理数据失败: {}", e);
                    }
                }
            }
        });

        Ok(())
    })
}

pub fn handle(
    ctx: Context,
    _writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        if let EventType::Satori(ev) = &ctx.event
            && ev.get_str("post_type") == Some("message")
        {
            let group = ev.get_str("group_id").unwrap_or("");
            let sender = ev.get("sender");
            let user_name = sender.and_then(|s| s.get_str("nickname")).unwrap_or("");
            let card = sender.and_then(|s| s.get_str("card")).unwrap_or("");
            let record = RecordActiveModel {
                message_id: Set(ev.get_str("message_id").unwrap_or("").to_string()),
                channel_id: Set(ev.get_str("channel_id").unwrap_or("").to_string()),
                channel_type: Set(if group.is_empty() { 1 } else { 0 }),
                guild_id: Set(group.to_string()),
                guild_name: Set(ev.get_str("group_name").unwrap_or("").to_string()),
                guild_avatar: Set(ev.get_str("group_avatar").unwrap_or("").to_string()),
                user_id: Set(ev.get_str("user_id").unwrap_or("").to_string()),
                user_name: Set(user_name.to_string()),
                member_nick: Set(if card.is_empty() { user_name } else { card }.to_string()),
                user_avatar: Set(sender
                    .and_then(|s| s.get_str("avatar"))
                    .unwrap_or("")
                    .to_string()),
                member_role: Set(sender
                    .and_then(|s| s.get_str("role"))
                    .unwrap_or("member")
                    .to_string()),
                ..Default::default()
            };
            let time = ev
                .get_i64("time")
                .or_else(|| ev.get_u64("time").map(|v| v as i64))
                .unwrap_or(0);
            insert(&ctx, &ctx.bot, record, time, ev.get("message")).await;
        }
        Ok(Some(ctx))
    })
}

/// 发出之后的钩子：这时才知道消息 ID 和它真正落在哪个频道（私聊频道要先问实现端）。
pub fn on_sent<'a>(
    ctx: &'a Context,
    _writer: &'a LockedWriter,
    sent: &'a Receipt<'a>,
) -> BoxFuture<'a, ()> {
    Box::pin(record_sent(
        ctx,
        sent.bot,
        sent.packet,
        sent.channel_id,
        sent.message_ids,
    ))
}

/// 记下自己发出的消息。
async fn record_sent(
    ctx: &Context,
    bot: &crate::event::BotStatus,
    packet: &crate::event::SendPacket,
    channel_id: &str,
    message_ids: &[String],
) {
    let config: RecorderConfig = get_config_or_default(ctx);
    if !config.enabled || !config.record_self {
        return;
    }
    let guild = packet.group_id().unwrap_or("");
    let origin = packet
        .original_event
        .as_ref()
        .filter(|origin| origin.get_str("group_id") == Some(guild));
    let origin_str = |key| origin.and_then(|origin| origin.get_str(key)).unwrap_or("");
    let login = bot.login_user.get();
    let user_name = login.name.clone().unwrap_or_default();
    let record = RecordActiveModel {
        message_id: Set(message_ids.first().cloned().unwrap_or_default()),
        channel_id: Set(channel_id.to_string()),
        channel_type: Set(if guild.is_empty() { 1 } else { 0 }),
        guild_id: Set(guild.to_string()),
        guild_name: Set(origin_str("group_name").to_string()),
        guild_avatar: Set(origin_str("group_avatar").to_string()),
        user_id: Set(login.id.clone()),
        user_avatar: Set(login.avatar.clone().unwrap_or_default()),
        member_nick: Set(login.nick.clone().unwrap_or_else(|| user_name.clone())),
        user_name: Set(user_name),
        member_role: Set("self".to_string()),
        ..Default::default()
    };
    insert(ctx, bot, record, Local::now().timestamp(), packet.message()).await;
}

/// 补齐派生字段（时段、长度、分词、消息特征），与当日统计聚合同事务写入。
async fn insert(
    ctx: &Context,
    bot: &crate::event::BotStatus,
    mut record: RecordActiveModel,
    time: i64,
    message: Option<&OwnedValue>,
) {
    let config: RecorderConfig = get_config_or_default(ctx);
    if !config.enabled {
        return;
    }
    record.platform = Set(bot.platform.clone());
    record.self_id = Set(bot.self_id());
    record.time = Set(time);
    if let Some(dt) = Local.timestamp_opt(time, 0).single() {
        record.time_hour = Set(dt.hour() as i32);
        record.time_weekday = Set(dt.weekday().num_days_from_sunday() as i32);
    }
    let (text_len, raw_text) = parse_message_content(message, &mut record);
    record.length = Set(text_len);
    let tokens = if raw_text.is_empty() {
        String::new()
    } else {
        tokio::task::spawn_blocking(move || {
            get_jieba()
                .cut(&raw_text, false)
                .into_iter()
                .map(|t| t.word)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .await
        .unwrap_or_default()
    };
    record.tokens = Set(tokens);

    let delta = crate::db::stats::MessageStatsDelta {
        guild_id: active_str(&record.guild_id),
        guild_name: active_str(&record.guild_name),
        guild_avatar: active_str(&record.guild_avatar),
        user_id: active_str(&record.user_id),
        member_nick: active_str(&record.member_nick),
        user_avatar: active_str(&record.user_avatar),
        time,
        length: text_len,
        image_count: active_i32(&record.image_count),
        is_anim_emoji: active_bool(&record.is_anim_emoji),
        is_voice: active_bool(&record.is_voice),
        is_video: active_bool(&record.is_video),
        face_count: active_i32(&record.face_count),
    };
    // 消息插入 + 聚合 UPSERT 在同一事务内：要么同时生效，要么同时回滚
    let inserted = ctx
        .db
        .transaction(|txn| {
            Box::pin(async move {
                record.insert(txn).await?;
                crate::db::stats::upsert_message_stats(txn, &delta).await?;
                Ok::<(), sea_orm::DbErr>(())
            })
        })
        .await;
    if let Err(e) = inserted {
        error!(target: LOG_TARGET, "消息记录失败: {}", e);
    }
}

/// 读取 ActiveModel 字段当前值（未设置时返回默认值），用于构建统计聚合增量
fn active_i32(v: &ActiveValue<i32>) -> i32 {
    match v {
        ActiveValue::Set(x) | ActiveValue::Unchanged(x) => *x,
        _ => 0,
    }
}

fn active_bool(v: &ActiveValue<bool>) -> bool {
    match v {
        ActiveValue::Set(x) | ActiveValue::Unchanged(x) => *x,
        _ => false,
    }
}

fn active_str(v: &ActiveValue<String>) -> String {
    match v {
        ActiveValue::Set(x) | ActiveValue::Unchanged(x) => x.clone(),
        _ => String::new(),
    }
}

/// 解析消息段数组，提取富文本摘要、特征标记，并返回 (纯文本长度, 拼接后的纯文本)
fn parse_message_content(
    msg_val: Option<&OwnedValue>,
    record: &mut RecordActiveModel,
) -> (i32, String) {
    let mut rich_text = String::new();
    // 存储分段的纯文本，用于最终拼接 tokens
    let mut text_segments: Vec<String> = Vec::new();
    let mut text_char_count = 0;

    // 统计变量
    let mut image_count = 0;
    let mut at_count = 0;
    let mut face_count = 0;

    // 标记变量
    let mut is_anim_emoji = false;
    let mut is_voice = false;
    let mut is_video = false;
    let mut is_music = false;
    let mut is_rps = false;
    let mut is_dice = false;
    let mut is_poke = false;
    let mut is_forward = false;
    let mut is_reply_flag = false;

    if let Some(val) = msg_val {
        // 情况 1: 纯字符串消息
        if let Some(s) = val.as_str() {
            rich_text.push_str(s);
            text_segments.push(s.trim().to_string());
            text_char_count += s.chars().count();
        }
        // 情况 2: 消息段数组
        else if let Some(arr) = val.as_array() {
            for seg in arr {
                let type_ = seg.get_str("type").unwrap_or("unknown");
                let data = seg.get("data");

                match type_ {
                    "text" => {
                        if let Some(t) = data.and_then(|d| d.get_str("text")) {
                            rich_text.push_str(t);
                            let trimmed = t.trim();
                            if !trimmed.is_empty() {
                                text_segments.push(trimmed.to_string());
                            }
                            text_char_count += t.chars().count();
                        }
                    }
                    "at" => {
                        at_count += 1;
                        let qq = data
                            .and_then(|d| {
                                d.get_str("qq")
                                    .map(|s| s.to_string())
                                    .or_else(|| d.get_i64("qq").map(|i| i.to_string()))
                                    .or_else(|| d.get_u64("qq").map(|i| i.to_string()))
                            })
                            .unwrap_or_default();
                        rich_text.push_str(&format!("[@{}]", qq));
                    }
                    "face" => {
                        face_count += 1;
                        rich_text.push_str("[表情]");
                    }
                    "mface" => {
                        image_count += 1;
                        is_anim_emoji = true;
                        rich_text.push_str("[动画表情]");
                    }
                    "image" => {
                        image_count += 1;
                        // 检查是否为动画表情
                        if let Some(d) = data {
                            let summary = d.get_str("summary").unwrap_or("");
                            let sub_type = d
                                .get_i64("sub_type")
                                .or_else(|| d.get_u64("sub_type").map(|v| v as i64))
                                .unwrap_or(0);
                            if summary == "[动画表情]" || sub_type == 1 {
                                is_anim_emoji = true;
                            }
                        }
                        rich_text.push_str("[图片]");
                    }
                    "record" => {
                        is_voice = true;
                        rich_text.push_str("[语音]");
                    }
                    "video" => {
                        is_video = true;
                        rich_text.push_str("[视频]");
                    }
                    "music" => {
                        is_music = true;
                        rich_text.push_str("[音乐]");
                    }
                    "poke" => {
                        is_poke = true;
                        rich_text.push_str("[戳一戳]");
                    }
                    "rps" => {
                        is_rps = true;
                        rich_text.push_str("[猜拳]");
                    }
                    "dice" => {
                        is_dice = true;
                        rich_text.push_str("[骰子]");
                    }
                    "forward" | "node" => {
                        is_forward = true;
                        rich_text.push_str("[合并转发]");
                    }
                    "reply" => {
                        is_reply_flag = true;
                        rich_text.push_str("[回复]");
                    }
                    "json" => rich_text.push_str("[卡片]"),
                    "file" => rich_text.push_str("[文件]"),
                    other => rich_text.push_str(&format!("[{}]", other)),
                }
            }
        }
    }

    record.content_rich = Set(rich_text);

    // 拼接纯文本
    let joined_text = text_segments.join(" ");

    record.has_image = Set(image_count > 0);
    record.has_at = Set(at_count > 0);
    record.is_reply = Set(is_reply_flag);
    record.image_count = Set(image_count);
    record.is_anim_emoji = Set(is_anim_emoji);
    record.at_count = Set(at_count);
    record.face_count = Set(face_count);
    record.is_voice = Set(is_voice);
    record.is_video = Set(is_video);
    record.is_music = Set(is_music);
    record.is_rps = Set(is_rps);
    record.is_dice = Set(is_dice);
    record.is_poke = Set(is_poke);
    record.is_forward = Set(is_forward);

    (text_char_count as i32, joined_text)
}

