//! 消息记录表（`message_records`）的实体。
//!
//! 放在数据层而不是 recorder 插件里：统计、词云、群聊记录与 `db/queries` 都直接读这张表，
//! 数据层不应反过来依赖某个插件的模块。表结构与索引仍由 recorder 在初始化时创建。
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
