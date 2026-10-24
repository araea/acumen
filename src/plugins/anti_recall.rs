//! 防撤回：有人撤回消息时，把原消息转存到指定的地方（私聊或一个群）。
//!
//! 实现端的撤回事件只带消息号，原文得自己记着：每条收到的消息在进流水线之前抄一份进
//! 内存（[`on_receive`]，按收到顺序，插件消费与否都不影响），留 `retention_minutes`。
//! 缓存里没有的（进程重启之前的）再去消息记录库里找一行摘要文字，图就没有了。
//!
//! 图和语音收到时就取到本地（`data/anti_recall/media`，留半小时、至多 100 MB）：实现端给的
//! `internal:` 链接是照原消息现取的，消息一撤回多半就取不到了，而一个取不开的链接会让
//! 整条转存连文字一起发不出去。转存时把本地那份内联上去（发送层会 `upload.create`）；
//! 还是没取到的换成一句「[图片（没取到）]」。真发不出去，再退一步只发文字。
//!
//! 一口气撤一串（管理员清屏）不该刷爆存储频道：同一个去处两秒内的撤回攒成一批，
//! 一条就直接转，多条合成一条合并转发，每个节点署原作者。
//!
//! 合并转发与引用回复这两类消息，光靠「撤回时再取」是取不到的：合并转发要读父消息，
//! 父消息一撤，内核缓存与 resId 两条路都可能断；引用只带一个消息号，而那条消息未必还
//! 在。所以收到时就各取一步（[`prefetch_forward`]、[`prefetch_quote`]），结果挂在缓存
//! 条目上，撤回时直接用。合并转发按「再发一张合并转发」还原（[`forward_card`]），取不到
//! 或实现端不收就退成文字聊天记录；引用回复在正文前加一行「↪ 回复 谁：原话」。
//!
//! 去处留空就发到号主自己的私聊（QQ 里自己和自己的那个会话）：开了就能看到，不必先填什么。
//! 控制台的插件页会显示去处、这次运行转存了几条，也能先发一条试试（[`live`]、[`test`]）；
//! 试发走框架登记的连接，不必等插件先收到一条消息。
//!
//! 原样再发有几处要改写：@ 到了别的会话就是 @ 一个不在场的人，改成文字「@名字」；
//! 语音、视频、文件不能和文字拼成一条，跟在后面单独发；合并转发里的二次转发展不平，
//! 写成一句占位。

use crate::adapters::satori::{self, LockedWriter, delivery_uncertain, forward, message, send_msg};
use crate::event::{BotStatus, Context, EventType};
use crate::message::{Message, Segment};
use crate::plugins::{ChannelConfig, PluginConfig, PluginError, get_config_or_default};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::future::BoxFuture;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde::{Deserialize, Serialize};
use simd_json::OwnedValue;
use simd_json::base::ValueAsScalar;
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsScalar};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::OnceCell;

const LOG_TARGET: &str = "Plugin/AntiRecall";

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    enabled: bool,
    /// 存到哪：群号；或 `private:QQ号`；留空 = 号主自己的私聊。
    target: String,
    /// 在哪些群生效；两边都空 = 全部群。
    channel: ChannelConfig,
    /// 私聊里的撤回也存。
    private: bool,
    /// 管理员撤回别人的消息也存（清广告时会多一些）。
    operator_recalls: bool,
    /// 号主在 QQ 里亲手发、又亲手撤回的也存（拿自己的消息试的时候打开）。机器人自己发的不存。
    self_recalls: bool,
    /// 合并转发照原样再发一张合并转发；关掉只留文字聊天记录。
    forward_records: bool,
    /// 这些人的撤回不管（QQ 号）。
    ignore_users: Vec<String>,
    /// 原文在内存里留多久（分钟）。普通成员只能撤回两分钟内的，管理员能撤更早的。
    retention_minutes: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            target: String::new(),
            channel: ChannelConfig::default(),
            private: true,
            operator_recalls: true,
            self_recalls: false,
            forward_records: true,
            ignore_users: Vec::new(),
            retention_minutes: 120,
        }
    }
}

impl PluginConfig for Config {
    const NAME: &'static str = "anti_recall";

    fn check(&self) -> Result<(), String> {
        let target = self.target.trim();
        let id = target.strip_prefix("private:").unwrap_or(target);
        if !target.is_empty() && (id.is_empty() || id.parse::<u64>().is_err()) {
            return Err("target 写群号、private:QQ号，或留空（发到号主自己的私聊）".into());
        }
        if !(1..=1440).contains(&self.retention_minutes) {
            return Err("retention_minutes 须在 1–1440 之间".into());
        }
        Ok(())
    }
}

/// 控制台上的字段名，按这个先后排。
pub(crate) const LABELS: &[(&str, &str)] = &[
    ("enabled", "启用"),
    ("target", "转存到"),
    ("channel", "生效的群"),
    ("private", "私聊撤回也存"),
    ("operator_recalls", "管理员撤回的也存"),
    ("self_recalls", "自己撤回的也存"),
    ("forward_records", "合并转发连内容一起存"),
    ("ignore_users", "不管这些人"),
    ("retention_minutes", "原文留多久（分钟）"),
];

/// 记着的一条消息。
#[derive(Clone)]
struct Kept {
    self_id: String,
    message_id: String,
    channel_id: String,
    group: Option<String>,
    group_name: String,
    user_id: String,
    name: String,
    at: i64,
    content: Message,
    /// 收到时顺手取回来的两样东西：合并转发的聊天记录、引用回复的原话。
    extra: Extra,
}

/// 撤回时才去取就晚了的那部分：收到时在后台填进来，撤回时直接读。
///
/// 每个槽只填一次，填的时候撤回任务还在等也看得见（[`tokio::sync::OnceCell::get`]
/// 在填完之前给 `None`）；两样都没有就是空着。
#[derive(Clone, Default)]
struct Extra {
    /// 这条消息是一张合并转发时，里面那份聊天记录。
    forward: Arc<OnceCell<forward::View>>,
    /// 这条消息引用回复了谁、说了什么。
    quote: Arc<OnceCell<Option<Quote>>>,
}

/// 被引用的那条消息。
#[derive(Clone)]
struct Quote {
    user_id: String,
    name: String,
    content: Message,
}

const CACHE_LIMIT: usize = 6000;
/// 撤回比原文事件先到时，留给事件乱序、数据库落库与平台回读的时间。
/// 这是恢复窗口，不是给每次正常撤回增加的延迟：缓存命中仍会立刻转存。
const RECOVER_DELAYS: [Duration; 6] = [
    Duration::ZERO,
    Duration::from_millis(150),
    Duration::from_millis(400),
    Duration::from_millis(900),
    Duration::from_millis(1_800),
    Duration::from_millis(3_000),
];
const PENDING_LIMIT: usize = 256;

fn cache() -> &'static Mutex<VecDeque<Kept>> {
    static CACHE: OnceLock<Mutex<VecDeque<Kept>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// 一条已经收到、但暂时还没找到原文的撤回。
/// 配置与操作者按撤回当时保存；随后配置改动不会把同一条转去另一个地方。
#[derive(Clone)]
struct PendingRecall {
    config: Config,
    operator: String,
    recalled_at: i64,
    inserted_at: std::time::Instant,
}

fn pending() -> &'static Mutex<HashMap<String, PendingRecall>> {
    static PENDING: OnceLock<Mutex<HashMap<String, PendingRecall>>> = OnceLock::new();
    PENDING.get_or_init(Default::default)
}

fn recall_key(self_id: &str, channel: &str, message_id: &str) -> String {
    format!("{self_id}|{channel}|{message_id}")
}

fn cached(self_id: &str, channel: &str, message_id: &str) -> Option<Kept> {
    cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .rev()
        .find(|kept| {
            kept.message_id == message_id && kept.channel_id == channel && kept.self_id == self_id
        })
        .cloned()
}

fn take_pending(key: &str) -> Option<PendingRecall> {
    pending()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(key)
}

fn pending_exists(key: &str) -> bool {
    pending()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains_key(key)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 进流水线之前：消息抄一份；撤回就把原文找出来排进待转。
pub fn on_receive(ctx: &mut Context, writer: &LockedWriter) -> Option<BoxFuture<'static, Result<(), PluginError>>> {
    let EventType::Satori(raw) = &ctx.event else {
        return None;
    };
    let config = get_config_or_default::<Config>(ctx);
    let group = raw.get_str("group_id").filter(|id| !id.is_empty()).map(str::to_owned);
    match &group {
        Some(id) if !config.channel.allows_group(id) || target_group(&config) == Some(id.as_str()) => return None,
        None if !config.private => return None,
        _ => {}
    }
    match raw.get_str("satori_type").unwrap_or("") {
        "message-created" => {
            let user = raw.get_str("user_id").unwrap_or("");
            let message_id = raw.get_str("message_id").unwrap_or("");
            if message_id.is_empty() || user.is_empty() {
                return None;
            }
            // 号主亲手发的，实现端挂的作者是 `qq-client:<号>`；机器人自己发的就是登录号本身。
            if user == ctx.bot.self_id() && !(config.self_recalls && by_hand(raw)) {
                return None;
            }
            let content = raw
                .get("message")
                .and_then(|value| simd_json::serde::from_owned_value::<Message>(value.clone()).ok())
                .unwrap_or_default();
            if content.0.is_empty() {
                return None;
            }
            let message = ctx.as_message()?;
            let kept = Kept {
                self_id: ctx.bot.self_id(),
                message_id: message_id.to_string(),
                channel_id: raw.get_str("channel_id").unwrap_or("").to_string(),
                group,
                group_name: raw.get_str("group_name").unwrap_or("").to_string(),
                user_id: user.to_string(),
                name: message.sender_name().to_string(),
                at: raw.get_i64("time").filter(|t| *t > 0).unwrap_or_else(now),
                content,
                extra: Extra::default(),
            };
            prefetch(ctx, writer, &kept);
            let horizon = now() - i64::try_from(config.retention_minutes * 60).unwrap_or(7200);
            let mut expired = Vec::new();
            {
                let mut cache = cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                while cache.front().is_some_and(|old| old.at < horizon) || cache.len() >= CACHE_LIMIT {
                    expired.extend(cache.pop_front());
                }
                cache.push_back(kept.clone());
            }
            for kept in &expired {
                forget(kept);
            }
            // 快速撤回时，删除事件可能先一步到达。原文现在到了，直接唤醒那条
            // 待恢复撤回；后台恢复任务随后会发现占位已被取走，不会重复转存。
            let key = recall_key(&kept.self_id, &kept.channel_id, &kept.message_id);
            if let Some(pending) = take_pending(&key) {
                enqueue(
                    ctx.clone(),
                    writer.clone(),
                    &pending.config,
                    Recalled {
                        kept,
                        operator: pending.operator,
                        recalled_at: pending.recalled_at,
                    },
                );
            }
            None
        }
        "message-deleted" => {
            let message_id = raw.get_str("message_id").unwrap_or("").to_string();
            let user = raw.get_str("user_id").unwrap_or("").to_string();
            let operator = raw.get_str("operator_id").unwrap_or("").to_string();
            let me = ctx.bot.self_id();
            // 机器人（或号主）撤别人的：那是在管群，不是要藏什么。
            if message_id.is_empty() || (operator == me && user != me) || (user == me && !config.self_recalls) {
                return None;
            }
            if config.ignore_users.iter().any(|id| *id == user || *id == operator) {
                return None;
            }
            let by_other = !operator.is_empty() && operator != user;
            if by_other && !config.operator_recalls {
                return None;
            }
            let channel = raw.get_str("channel_id").unwrap_or("").to_string();
            let kept = cached(&me, &channel, &message_id);
            // 自己的消息只认缓存里那些亲手发的：机器人发的不进缓存，也不该去记录库里翻。
            if user == me && kept.is_none() {
                return None;
            }
            let operator = if by_other { operator } else { String::new() };
            let (ctx, writer) = (ctx.clone(), writer.clone());
            if let Some(kept) = kept {
                return Some(Box::pin(async move {
                    enqueue(
                        ctx,
                        writer,
                        &config,
                        Recalled {
                            kept,
                            operator,
                            recalled_at: now(),
                        },
                    );
                    Ok(())
                }));
            }

            // 撤回先到的占一个位，等原文事件、记录库或平台回读把它补上。
            // 重复撤回共享同一个恢复任务，操作者与撤回时间以第一次为准。
            let key = recall_key(&me, &channel, &message_id);
            {
                let mut all = pending()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                all.retain(|_, item| item.inserted_at.elapsed() < Duration::from_secs(20));
                if all.contains_key(&key) {
                    return None;
                }
                if all.len() >= PENDING_LIMIT {
                    warn!(target: LOG_TARGET, "待恢复撤回过多，丢弃最早无法恢复的一条");
                    if let Some(oldest) = all
                        .iter()
                        .min_by_key(|(_, item)| item.inserted_at)
                        .map(|(key, _)| key.clone())
                    {
                        all.remove(&oldest);
                        tally().missed += 1;
                    }
                }
                all.insert(
                    key.clone(),
                    PendingRecall {
                        config,
                        operator,
                        recalled_at: now(),
                        inserted_at: std::time::Instant::now(),
                    },
                );
            }
            let group_name = raw.get_str("group_name").unwrap_or("").to_string();
            // 这个钩子的 future 会在流水线之前被逐个 await：恢复要退避几秒，必须自己
            // 起任务，不能把整个事件循环堵在这里。
            tokio::spawn(async move {
                recover(
                    ctx, writer, key, message_id, channel, group, group_name, user,
                )
                .await;
            });
            None
        }
        _ => None,
    }
}

/// 防撤回不处理流水线里的事：活都在 [`on_receive`] 里干完了。
pub fn handle(ctx: Context, _writer: LockedWriter) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move { Ok(Some(ctx)) })
}

/// 号主在 QQ 客户端里亲手发的（不是机器人经实现端发的）。
fn by_hand(raw: &crate::event::Event) -> bool {
    raw.get("_satori")
        .and_then(|body| body.get("user"))
        .and_then(|user| user.get_str("id"))
        .is_some_and(|id| id.starts_with("qq-client:"))
}

/// 缓存里没有：去消息记录库找那一行摘要（重启之前收的消息只剩文字）。
async fn from_records(ctx: &Context, message_id: &str, channel: &str) -> Option<Kept> {
    let row = ctx
        .db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT self_id, guild_id, guild_name, user_id, user_name, member_nick, content_rich, time \
             FROM message_records WHERE message_id = ? AND channel_id = ? ORDER BY id DESC LIMIT 1",
            [message_id.into(), channel.into()],
        ))
        .await
        .ok()??;
    let text: String = row.try_get("", "content_rich").ok()?;
    let group: String = row.try_get("", "guild_id").unwrap_or_default();
    let nick: String = row.try_get("", "member_nick").unwrap_or_default();
    let name: String = row.try_get("", "user_name").unwrap_or_default();
    Some(Kept {
        self_id: row.try_get("", "self_id").unwrap_or_default(),
        message_id: message_id.to_string(),
        channel_id: channel.to_string(),
        group: (!group.is_empty()).then_some(group),
        group_name: row.try_get("", "guild_name").unwrap_or_default(),
        user_id: row.try_get("", "user_id").unwrap_or_default(),
        name: if nick.is_empty() { name } else { nick },
        at: row.try_get("", "time").unwrap_or_default(),
        content: Message::new().text(format!("{text}（只留下了文字摘要）")),
        extra: Extra::default(),
    })
}

/// 恢复一条先于原文到达的撤回。原消息事件到达时会直接取走占位（见
/// `message-created` 分支），这里同时用短退避查缓存、记录库和平台，覆盖
/// 「实现端已经投递、框架记录还没落库」与「框架重启后只剩平台历史」两种情形。
#[allow(clippy::too_many_arguments)]
async fn recover(
    ctx: Context,
    writer: LockedWriter,
    key: String,
    message_id: String,
    channel: String,
    group: Option<String>,
    group_name: String,
    user: String,
) {
    for (attempt, delay) in RECOVER_DELAYS.iter().enumerate() {
        tokio::time::sleep(*delay).await;
        if !pending_exists(&key) {
            return;
        }
        let me = ctx.bot.self_id();
        let mut found = cached(&me, &channel, &message_id);
        if found.is_none() {
            found = from_records(&ctx, &message_id, &channel).await;
        }
        if found.is_none() && attempt % 2 == 0 {
            found = from_platform(
                &ctx,
                &writer,
                &message_id,
                &channel,
                group.as_deref(),
                &group_name,
                &user,
            )
            .await;
            if let Some(kept) = &found {
                // 平台回读不会经过 message-created 的接收钩子，媒体与引用要在
                // 这里补做预取，否则快速撤回的图仍然可能只剩占位。
                prefetch(&ctx, &writer, kept);
            }
        }
        if let Some(kept) = found {
            if let Some(pending) = take_pending(&key) {
                debug!(target: LOG_TARGET, "撤回的消息 {message_id} 已在乱序宽限期内找回原文");
                enqueue(
                    ctx,
                    writer,
                    &pending.config,
                    Recalled {
                        kept,
                        operator: pending.operator,
                        recalled_at: pending.recalled_at,
                    },
                );
            }
            return;
        }
    }
    if take_pending(&key).is_some() {
        debug!(target: LOG_TARGET, "撤回的消息 {message_id} 在恢复窗口后仍没有原文");
        tally().missed += 1;
    }
}

/// 平台还留着原消息时，直接取完整内容，而不是只剩记录库的一行文字摘要。
/// 对 satori-qq 来说，即使事件因进程重启没进本机缓存，实现端的 MsgStore
/// 通常仍能给出原始元素；失败只代表这条恢复路径不可用，不影响后续重试。
async fn from_platform(
    ctx: &Context,
    writer: &LockedWriter,
    message_id: &str,
    channel: &str,
    group: Option<&str>,
    group_name: &str,
    fallback_user: &str,
) -> Option<Kept> {
    let raw: serde_json::Value = tokio::time::timeout(
        Duration::from_secs(3),
        writer.call(
            ctx,
            "message.get",
            serde_json::json!({"channel_id": channel, "message_id": message_id}),
        ),
    )
    .await
    .ok()?
    .ok()?;
    let content = raw.get("content")?.as_str().unwrap_or("");
    if content.is_empty() {
        return None;
    }
    let content = message::from_content_with(content, &writer.resources());
    if content.0.is_empty() {
        return None;
    }
    let scalar = |value: Option<&serde_json::Value>| -> String {
        match value {
            Some(serde_json::Value::String(text)) => text.clone(),
            Some(serde_json::Value::Number(number)) => number.to_string(),
            _ => String::new(),
        }
    };
    let user = raw.get("user").cloned().unwrap_or(serde_json::Value::Null);
    let member = raw
        .get("member")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let user_id = {
        let id = scalar(user.get("id"));
        if id.is_empty() {
            fallback_user.to_string()
        } else {
            id
        }
    };
    let name = [
        member.get("nick"),
        member.get("name"),
        user.get("name"),
        user.get("nick"),
    ]
    .into_iter()
    .find_map(|value| value.and_then(serde_json::Value::as_str))
    .filter(|name| !name.is_empty())
    .unwrap_or("")
    .to_string();
    let at = raw
        .get("created_at")
        .and_then(serde_json::Value::as_i64)
        .map(|time| {
            if time > 10_000_000_000 {
                time / 1000
            } else {
                time
            }
        })
        .filter(|time| *time > 0)
        .unwrap_or_else(now);
    let recovered_id = scalar(raw.get("id"));
    Some(Kept {
        self_id: ctx.bot.self_id(),
        message_id: if recovered_id.is_empty() {
            message_id.to_string()
        } else {
            recovered_id
        },
        channel_id: channel.to_string(),
        group: group.map(str::to_owned),
        group_name: group_name.to_string(),
        user_id,
        name,
        at,
        content,
        extra: Extra::default(),
    })
}

// ==================== 媒体 ====================

/// 能内联转存的媒体段；收到时就取的只有图和语音（视频、文件可能很大，撤回时才现取）。
const MEDIA: [&str; 4] = ["image", "record", "video", "file"];
const PREFETCH: [&str; 2] = ["image", "record"];
const PREFETCH_LIMIT: u64 = 16 << 20;
const LATE_LIMIT: u64 = 64 << 20;
/// 本地那份最多留多久、目录最多多大：普通成员只能撤两分钟内的，半小时足够；
/// 刷图高峰也不让它涨过这个数，超了先删最旧的。
const MEDIA_MINUTES: u64 = 30;
const MEDIA_BUDGET: u64 = 100 << 20;

/// 资源链接 → 取到本地的那份。同一个链接只取一次：撤回时预取还没完就等它。
type Fetched = Arc<OnceCell<Option<PathBuf>>>;

fn stash() -> &'static Mutex<HashMap<String, Fetched>> {
    static STASH: OnceLock<Mutex<HashMap<String, Fetched>>> = OnceLock::new();
    STASH.get_or_init(Default::default)
}

/// 存放目录。上次运行留下的先清掉：内存里的索引随进程没了，那些文件再也用不上。
fn media_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = crate::storage::data_path("anti_recall").ok()?.join("media");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    })
    .as_ref()
}

/// 媒体段的 (键, 下载地址)：键是实现端给的资源链接，下载地址是框架换好的代理 URL。
fn media_source(segment: &Segment) -> Option<(String, String)> {
    if !MEDIA.contains(&segment.type_.as_str()) {
        return None;
    }
    let field = |key: &str| segment.data.get(key).and_then(|v| v.as_str()).filter(|v| !v.is_empty());
    let url = field("url")?;
    Some((field("file").unwrap_or(url).to_string(), url.to_string()))
}

fn slot(key: &str) -> Fetched {
    stash()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(key.to_string())
        .or_default()
        .clone()
}

/// 收到就取：图和语音在后台落盘。
fn prefetch_media(content: &Message) {
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    for segment in &content.0 {
        if !PREFETCH.contains(&segment.type_.as_str()) {
            continue;
        }
        let Some((key, url)) = media_source(segment) else {
            continue;
        };
        let slot = slot(&key);
        if slot.initialized() {
            continue;
        }
        tokio::spawn(async move {
            slot.get_or_init(|| fetch(key, url, PREFETCH_LIMIT)).await;
        });
    }
}

/// 等收到时那一步在后台取完：撤回来得太快时，别把「还在取」写成「没取到」。
///
/// 最多等 `wait`；没等到就照没取到处理，转存不能一直卡着。
async fn settled<T>(cell: &OnceCell<T>, wait: Duration) -> Option<&T> {
    settled_until(cell, tokio::time::Instant::now() + wait).await
}

async fn settled_until<T>(cell: &OnceCell<T>, deadline: tokio::time::Instant) -> Option<&T> {
    loop {
        if let Some(value) = cell.get() {
            return Some(value);
        }
        if cell.initialized() || tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 收到一条消息时顺手做的事：媒体落盘、合并转发展开、引用原话取回。
///
/// 后面两样都得趁现在：撤回之后再要，父消息与引用目标都可能已经不在了。
fn prefetch(ctx: &Context, writer: &LockedWriter, kept: &Kept) {
    prefetch_media(&kept.content);
    prefetch_forward(ctx, writer, kept);
    prefetch_quote(ctx, writer, kept);
}

/// 展开一条合并转发，连同里面的图一起留下。
///
/// 一次最多两张同时在读：群里连发几张转发时，别把实现端的取消息通道占满。
fn prefetch_forward(ctx: &Context, writer: &LockedWriter, kept: &Kept) {
    let Some(source) = forward::source_of(&kept.content, Some(kept.message_id.clone())) else {
        return;
    };
    if kept.extra.forward.initialized() || tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    let (ctx, writer, slot, channel) = (
        ctx.clone(),
        writer.clone(),
        kept.extra.forward.clone(),
        kept.channel_id.clone(),
    );
    tokio::spawn(async move {
        static GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
        let Ok(_permit) = GATE.acquire().await else {
            return;
        };
        let view = forward::expand(&ctx, &writer, source.in_channel(channel)).await;
        for node in &view.nodes {
            prefetch_media(&node.message);
        }
        debug!(
            target: LOG_TARGET,
            "合并转发读回 {} 条{}",
            view.nodes.len(),
            if view.nodes.is_empty() { "（空）" } else { "" }
        );
        let _ = slot.set(view);
    });
}

/// 引用回复只带一个消息号：趁被引的那条还在，把原话取回来。
fn prefetch_quote(ctx: &Context, writer: &LockedWriter, kept: &Kept) {
    let Some(id) = reply_id(&kept.content) else {
        return;
    };
    if kept.extra.quote.initialized() || tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    let (ctx, writer, slot, channel) = (
        ctx.clone(),
        writer.clone(),
        kept.extra.quote.clone(),
        kept.channel_id.clone(),
    );
    tokio::spawn(async move {
        let quote = quote_of(&ctx, &writer, &id, &channel).await;
        if quote.is_none() {
            debug!(target: LOG_TARGET, "引用的消息 {id} 没取到原文");
        }
        let _ = slot.set(quote);
    });
}

/// 这条消息引用的是哪一条（Satori 的 `<quote>`，实现端给的是 `reply`）。
fn reply_id(content: &Message) -> Option<String> {
    let segment = content
        .0
        .iter()
        .find(|segment| matches!(segment.type_.as_str(), "reply" | "quote"))?;
    let value = segment.data.get("id")?;
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_i64().map(|id| id.to_string()))
        .filter(|id| !id.is_empty())
}

/// 取回被引用的那条消息：先看自己记着的（刚说过的话多半在），再问平台。
async fn quote_of(ctx: &Context, writer: &LockedWriter, id: &str, channel: &str) -> Option<Quote> {
    let kept = {
        let cache = cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        cache
            .iter()
            .rev()
            .find(|kept| kept.message_id == id && kept.channel_id == channel)
            .cloned()
    };
    if let Some(kept) = kept {
        return Some(Quote {
            user_id: kept.user_id,
            name: kept.name,
            content: kept.content,
        });
    }
    let raw: serde_json::Value = tokio::time::timeout(
        Duration::from_secs(3),
        writer.call(
            ctx,
            "message.get",
            serde_json::json!({"channel_id": channel, "message_id": id}),
        ),
    )
    .await
    .ok()?
    .ok()?;
    let content = message::from_content_with(
        raw.get("content").and_then(|value| value.as_str()).unwrap_or(""),
        &writer.resources(),
    );
    if content.0.is_empty() {
        return None;
    }
    let name = raw
        .get("member")
        .and_then(|member| member.get("nick"))
        .and_then(|nick| nick.as_str())
        .filter(|nick| !nick.is_empty())
        .or_else(|| {
            raw.get("user")
                .and_then(|user| user.get("name"))
                .and_then(|name| name.as_str())
        })
        .unwrap_or("")
        .to_string();
    Some(Quote {
        user_id: raw
            .get("user")
            .and_then(|user| user.get("id"))
            .and_then(|id| id.as_str())
            .unwrap_or("")
            .to_string(),
        name,
        content,
    })
}

/// 缓存里的消息过期了：它的媒体一起删掉（合并转发里、引用里的也算）。
fn forget(kept: &Kept) {
    forget_media(&kept.content);
    if let Some(view) = kept.extra.forward.get() {
        for node in &view.nodes {
            forget_media(&node.message);
        }
    }
    if let Some(Some(quote)) = kept.extra.quote.get() {
        forget_media(&quote.content);
    }
}

fn forget_media(content: &Message) {
    for segment in &content.0 {
        let Some((key, _)) = media_source(segment) else {
            continue;
        };
        let slot = stash()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        if let Some(Some(path)) = slot.as_deref().and_then(OnceCell::get) {
            let _ = std::fs::remove_file(path);
        }
    }
}

async fn fetch(key: String, url: String, limit: u64) -> Option<PathBuf> {
    // 别一口气把实现端的下载器挤满：群里刷图时排着来。
    static GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(3);
    let _permit = GATE.acquire().await.ok()?;
    let dir = media_dir()?;
    let download = async {
        let response = crate::http::get(&url).await?.error_for_status()?;
        if response.content_length().is_some_and(|size| size > limit) {
            return Ok(None);
        }
        let bytes = response.bytes().await?;
        Ok::<_, reqwest::Error>((!bytes.is_empty() && bytes.len() as u64 <= limit).then_some(bytes))
    };
    let bytes = match tokio::time::timeout(Duration::from_secs(30), download).await {
        Ok(Ok(Some(bytes))) => bytes,
        Ok(Ok(None)) => {
            debug!(target: LOG_TARGET, "{key} 太大，不留");
            return None;
        }
        Ok(Err(error)) => {
            debug!(target: LOG_TARGET, "{key} 没取到：{error}");
            return None;
        }
        Err(_) => {
            debug!(target: LOG_TARGET, "{key} 取了 30 秒没取完");
            return None;
        }
    };
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    let path = dir.join(format!("{:016x}", hasher.finish()));
    tokio::fs::write(&path, &bytes).await.ok()?;
    let _ = tokio::task::spawn_blocking(|| trim(dir)).await;
    Some(path)
}

/// 删掉过了 [`MEDIA_MINUTES`] 的，再从最旧的删起直到不超过 [`MEDIA_BUDGET`]。
/// 删掉的那份转存时会再现取一次，取不到就写「没取到」。
fn trim(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let horizon = std::time::SystemTime::now() - Duration::from_secs(MEDIA_MINUTES * 60);
    let mut files: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            Some((meta.modified().ok()?, meta.len(), entry.path()))
        })
        .collect();
    files.sort();
    let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
    for (modified, size, path) in files {
        if modified >= horizon && total <= MEDIA_BUDGET {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= size;
        }
    }
}

fn media_label(kind: &str) -> &'static str {
    match kind {
        "image" => "图片",
        "record" | "audio" => "语音",
        "video" => "视频",
        _ => "文件",
    }
}

/// 转存前把媒体换成本地那份的内联字节；预取没成的再现取一次，还不行就留一句说明。
async fn localize(content: &Message) -> Message {
    let mut out = Message::new();
    for segment in &content.0 {
        let Some((key, url)) = media_source(segment) else {
            out.0.push(segment.clone());
            continue;
        };
        let limit = if PREFETCH.contains(&segment.type_.as_str()) { PREFETCH_LIMIT } else { LATE_LIMIT };
        let slot = slot(&key);
        let waited = tokio::time::timeout(Duration::from_secs(40), async {
            slot.get_or_init(|| fetch(key.clone(), url.clone(), limit)).await.clone()
        })
        .await
        .ok()
        .flatten();
        let path = match waited {
            Some(path) => Some(path),
            None => fetch(key, url, limit).await,
        };
        let bytes = match path {
            Some(path) => tokio::fs::read(path).await.ok(),
            None => None,
        };
        match bytes {
            Some(bytes) => {
                let mut segment = segment.clone();
                segment.data.remove("url");
                segment
                    .data
                    .insert("file".into(), OwnedValue::from(format!("base64://{}", STANDARD.encode(bytes))));
                out.0.push(segment);
            }
            None => out = out.text(format!("[{}（没取到）]", media_label(&segment.type_))),
        }
    }
    out
}

// ==================== 转存 ====================

struct Recalled {
    kept: Kept,
    operator: String,
    recalled_at: i64,
}

/// 去处 → 这一批要转的。第一条进来时起一个两秒的计时，到点一并发走。
fn queue() -> &'static Mutex<HashMap<String, Vec<Recalled>>> {
    static QUEUE: OnceLock<Mutex<HashMap<String, Vec<Recalled>>>> = OnceLock::new();
    QUEUE.get_or_init(Default::default)
}

fn target_group(config: &Config) -> Option<&str> {
    let target = config.target.trim();
    (!target.is_empty() && !target.starts_with("private:")).then_some(target)
}

/// 去处：(群号, 私聊对象)。留空就是号主自己的私聊（登录账号）。
fn destination(bot: &BotStatus, config: &Config) -> Option<(Option<String>, Option<String>)> {
    let target = config.target.trim();
    if let Some(user) = target.strip_prefix("private:") {
        return Some((None, Some(user.to_string())));
    }
    if !target.is_empty() {
        return Some((Some(target.to_string()), None));
    }
    let me = bot.self_id();
    (!me.is_empty()).then_some((None, Some(me)))
}

fn enqueue(ctx: Context, writer: LockedWriter, config: &Config, item: Recalled) {
    let Some((group, user)) = destination(&ctx.bot, config) else {
        warn!(target: LOG_TARGET, "没有转存去处：还没拿到登录账号，或设置 anti_recall.target");
        return;
    };
    let key = format!("{}|{}", group.as_deref().unwrap_or(""), user.as_deref().unwrap_or(""));
    let first = {
        let mut queue = queue().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let list = queue.entry(key.clone()).or_default();
        list.push(item);
        list.len() == 1
    };
    if !first {
        return;
    }
    let records = config.forward_records;
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let mut batch = queue()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key)
            .unwrap_or_default();
        for item in &mut batch {
            item.kept.content = localize(&item.kept.content).await;
        }
        let count = batch.len() as u64;
        let last = batch.last().map(|item| headline(item).lines().next().unwrap_or("").to_string());
        let (group, user) = (group.as_deref(), user.as_deref());
        let result = match deliver(&ctx, &writer, group, user, &batch, records).await {
            Ok(()) => Ok(None),
            // 带媒体的那条发不出去（实现端拒收某个资源、合并转发太大……）：退一步只发文字，
            // 总比整条丢了强。
            Err(error) => {
                warn!(target: LOG_TARGET, "撤回的消息没转存出去，改发文字：{error}");
                send_msg(&ctx, writer.clone(), group, user, plain(&batch, &error.to_string()))
                    .await
                    .map(|()| Some(error.to_string()))
                    .map_err(|second| format!("{error}；只发文字也失败：{second}"))
            }
        };
        let mut tally = tally();
        match result {
            Ok(degraded) => {
                tally.forwarded += count;
                tally.last = last.map(|line| (now(), line));
                tally.error = degraded.map(|error| (now(), format!("{error}（已改发文字）")));
            }
            Err(error) => {
                warn!(target: LOG_TARGET, "撤回的消息没转存出去：{error}");
                tally.error = Some((now(), error));
            }
        }
    });
}

fn clock(at: i64) -> String {
    chrono::DateTime::from_timestamp(at, 0)
        .map(|at| at.with_timezone(&crate::clock::beijing()).format("%H:%M:%S").to_string())
        .unwrap_or_default()
}

fn headline(item: &Recalled) -> String {
    let kept = &item.kept;
    let place = match &kept.group {
        Some(group) if !kept.group_name.is_empty() => format!("「{}」（{group}）", kept.group_name),
        Some(group) => format!("群 {group}"),
        None => "私聊".to_string(),
    };
    let who = if kept.name.is_empty() || kept.name == kept.user_id {
        kept.user_id.clone()
    } else {
        format!("{}（{}）", kept.name, kept.user_id)
    };
    let by = if item.operator.is_empty() {
        String::new()
    } else {
        format!("，由 {} 撤回", item.operator)
    };
    // 合并转发说清是什么：不然只看到一句「撤回了一条消息」加一张卡片。
    let what = if kept.content.0.iter().any(|segment| segment.type_ == "forward") {
        "一条合并转发"
    } else {
        "一条消息"
    };
    format!(
        "🗑 {who} 在{place}撤回了{what}{by}\n{} 发出 · {} 撤回",
        clock(kept.at),
        clock(item.recalled_at)
    )
}

/// 原样再发要改写的几类元素：返回能拼在一起发的那部分，与要单独发的媒体。
///
/// `forward_sent` 表示这条消息里的合并转发会另外发一张卡片，正文里就不必再留占位。
fn adapt(content: &Message, forward_sent: bool) -> (Message, Vec<Segment>) {
    let mut inline = Message::new();
    let mut separate = Vec::new();
    for segment in &content.0 {
        match segment.type_.as_str() {
            "text" | "image" | "face" | "mface" => inline.0.push(segment.clone()),
            "at" => inline = inline.text(at_label(segment)),
            // 引用由 [`quote_line`] 统一交代，这里不再另写一句。
            "reply" | "quote" => {}
            "record" | "audio" | "video" | "file" => separate.push(segment.clone()),
            "forward" if forward_sent => {}
            "forward" => inline = inline.text("[合并转发（内容没取到）]"),
            "poke" => inline = inline.text("[戳一戳]"),
            "dice" => inline = inline.text("[骰子]"),
            "rps" => inline = inline.text("[猜拳]"),
            "json" | "lightapp" | "xml" | "ark" => inline = inline.text("[卡片消息]"),
            other => inline = inline.text(format!("[{other}]")),
        }
    }
    (inline, separate)
}

/// @ 到了别的会话就是 @ 一个不在场的人：写成文字。
fn at_label(segment: &Segment) -> String {
    let text = |key: &str| segment.data.get(key).and_then(|value| value.as_str()).unwrap_or("");
    if text("type") == "all" {
        return "@全体成员 ".to_string();
    }
    let label = [text("name"), text("id"), text("qq")]
        .into_iter()
        .find(|value| !value.is_empty())
        .unwrap_or("");
    if label.is_empty() {
        "@某人 ".to_string()
    } else {
        format!("@{label} ")
    }
}

/// 转存一条消息要发的东西。
struct Prepared {
    /// 标题、引用、正文合在一条里。
    body: Message,
    /// 原消息是合并转发时，还原成的那张卡片。
    forward: Option<Message>,
    /// 没法跟文字拼成一条的媒体（语音、视频、文件），跟在后面单独发。
    separate: Vec<Segment>,
}

/// 把一条撤回的消息整理成要发的样子。
async fn prepare(item: &Recalled, records: bool) -> Prepared {
    let kept = &item.kept;
    // 撤回来得比预取还快时等一会儿：合并转发要展开、引用要问平台，都只在这一刻还来得及。
    // 没有这两样的消息不等——那个槽永远不会有人填。
    let view = match forward::source_of(&kept.content, None) {
        Some(_) => settled(&kept.extra.forward, FORWARD_WAIT).await,
        None => None,
    }
    .filter(|view| !view.nodes.is_empty());
    if reply_id(&kept.content).is_some() {
        let _ = settled(&kept.extra.quote, QUOTE_WAIT).await;
    }
    let mut body = Message::new().text(format!("{}\n", headline(item)));
    if let Some(line) = quote_line(item) {
        body = body.text(format!("{line}\n"));
    }
    let (inline, separate) = adapt(&kept.content, view.is_some());
    body.0.extend(inline.0);
    let mut card = None;
    if let Some(view) = view {
        if records {
            card = forward_card(view).await;
        }
        if card.is_none() {
            body = body.text(format!("\n{}", forward_text(view)));
        }
    }
    Prepared {
        body,
        forward: card,
        separate,
    }
}

/// 引用回复的那一行：`↪ 回复 张三：原话`。没有引用就没有这一行。
///
/// 被引的那条取不到时也留一句，让人知道原文里还有一段引用。
fn quote_line(item: &Recalled) -> Option<String> {
    reply_id(&item.kept.content)?;
    let Some(quote) = item.kept.extra.quote.get().and_then(Option::as_ref) else {
        return Some("↪ 回复了一条消息（原文没取到）".to_string());
    };
    let who = match (quote.name.as_str(), quote.user_id.as_str()) {
        ("", "") => "某人".to_string(),
        ("", id) => id.to_string(),
        (name, "") => name.to_string(),
        (name, id) => format!("{name}（{id}）"),
    };
    // 引用的是合并转发时说「[合并转发]」，别用节点内部那套「嵌套」的说法。
    let body = forward::describe(&quote.content).replace("[嵌套合并转发]", "[合并转发]");
    if body.trim().is_empty() {
        return Some(format!("↪ 回复了 {who} 的一条消息"));
    }
    Some(format!("↪ 回复 {who}：{body}"))
}

/// 撤回之后等预取最多多久：合并转发要展开，引用要问平台。
const FORWARD_WAIT: Duration = Duration::from_secs(8);
const QUOTE_WAIT: Duration = Duration::from_secs(2);

/// 一张合并转发最多还原多少条：再多发得慢，也容易撞上实现端的超时。
const CARD_NODES: usize = 40;
/// 退成文字时，一条合并转发最多留多少字。
const FORWARD_TEXT_CHARS: usize = 600;

/// 把展开出来的聊天记录重新做成一张合并转发。
///
/// 图与语音用收到时存下的本地那份（[`localize`]），撤回之后也发得出去；视频、文件
/// 在节点里只留一句占位——它们没有预取，撤回后十个有九个取不回来，等下去只会拖住整张卡。
async fn forward_card(view: &forward::View) -> Option<Message> {
    let mut card = Message::new();
    let mut count = 0;
    for node in view.nodes.iter().take(CARD_NODES) {
        let content = localize(&flatten_node(&node.message)).await;
        if content.0.is_empty() {
            continue;
        }
        let name = if node.name.is_empty() {
            node.user_id.clone()
        } else {
            node.name.clone()
        };
        let user = if node.user_id.is_empty() {
            "0"
        } else {
            node.user_id.as_str()
        };
        card = card.node_custom(user, name, content);
        count += 1;
    }
    if count == 0 {
        return None;
    }
    let rest = view.nodes.len().saturating_sub(count);
    let note = if rest > 0 {
        Some(format!("（还有 {rest} 条记录没有转存）"))
    } else if view.truncated {
        Some("（内容较多，后面还有没读进来的）".to_string())
    } else {
        None
    };
    if let Some(note) = note {
        card = card.node_custom("0", "防撤回", Message::new().text(note));
    }
    Some(card)
}

/// 合并转发的文字版：展开出来的聊天记录，一行一条。
fn forward_text(view: &forward::View) -> String {
    let transcript = view.transcript();
    let transcript = transcript.trim();
    if transcript.is_empty() {
        return "（合并转发的内容没取到）".to_string();
    }
    format!(
        "合并转发里的聊天记录：\n{}",
        crate::storage::truncate_chars(transcript, FORWARD_TEXT_CHARS)
    )
}

/// 节点正文里能发的东西：@ 变文字、二次转发变占位，其余照旧。
fn flatten_node(content: &Message) -> Message {
    let mut out = Message::new();
    for segment in &content.0 {
        match segment.type_.as_str() {
            "text" | "image" | "face" | "mface" => out.0.push(segment.clone()),
            "record" | "audio" => {
                let mut segment = segment.clone();
                segment.type_ = "record".to_string();
                out.0.push(segment);
            }
            "at" => out = out.text(at_label(segment)),
            "reply" | "quote" => out = out.text("（回复了一条消息）"),
            "forward" | "node" => out = out.text("[合并转发里的转发]"),
            "video" => out = out.text("[视频]"),
            "file" => {
                let name = segment
                    .data
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                out = out.text(if name.is_empty() {
                    "[文件]".to_string()
                } else {
                    format!("[文件：{name}]")
                });
            }
            "poke" => out = out.text("[戳一戳]"),
            "dice" => out = out.text("[骰子]"),
            "rps" => out = out.text("[猜拳]"),
            "json" | "lightapp" | "xml" | "ark" => out = out.text("[卡片消息]"),
            other => out = out.text(format!("[{other}]")),
        }
    }
    out
}

async fn deliver(
    ctx: &Context,
    writer: &LockedWriter,
    group: Option<&str>,
    user: Option<&str>,
    batch: &[Recalled],
    records: bool,
) -> Result<(), PluginError> {
    match batch {
        [] => Ok(()),
        [item] => {
            let prepared = prepare(item, records).await;
            // 标题正文发不出去才算整条失败，交给外层退成纯文字。
            send_msg(ctx, writer.clone(), group, user, prepared.body).await?;
            if let Some(card) = prepared.forward
                && let Err(error) = send_msg(ctx, writer.clone(), group, user, card).await
            {
                // 实现端不收这张卡片（超大、native 打包失败……）：把聊天记录写成文字补上，
                // 至少别让这次撤回只剩一句「撤回了一条合并转发」。没收到回执时不再补发，
                // 那张卡可能已经落地了，再发一遍就是重复。
                if delivery_uncertain(error.as_ref()) {
                    warn!(target: LOG_TARGET, "合并转发的发送结果未知，不再补发文字：{error}");
                } else {
                    warn!(target: LOG_TARGET, "合并转发没发出去，改发文字：{error}");
                    if let Some(view) = item
                        .kept
                        .extra
                        .forward
                        .get()
                        .filter(|view| !view.nodes.is_empty())
                    {
                        let fallback = Message::new().text(forward_text(view));
                        send_msg(ctx, writer.clone(), group, user, fallback).await?;
                    }
                }
            }
            for segment in prepared.separate {
                if let Err(error) = send_msg(ctx, writer.clone(), group, user, Message(vec![segment])).await {
                    // 主内容已经发出去了，缺一段媒体不值得把整条再退成文字。
                    warn!(target: LOG_TARGET, "随原文另发的媒体没发出去：{error}");
                }
            }
            Ok(())
        }
        items => {
            let login = ctx.bot.login_user.get();
            let mut message = Message::new().node_custom(
                &login.id,
                login.name.as_deref().unwrap_or("防撤回"),
                Message::new().text(format!("一口气撤回了 {} 条消息", items.len())),
            );
            // 一批里几张转发都还在展开时，总共只等一个 FORWARD_WAIT；引用同理。
            let deadline = tokio::time::Instant::now() + FORWARD_WAIT;
            let quote_deadline = tokio::time::Instant::now() + QUOTE_WAIT;
            for item in items {
                let view = match forward::source_of(&item.kept.content, None) {
                    Some(_) => settled_until(&item.kept.extra.forward, deadline).await,
                    None => None,
                }
                .filter(|view| !view.nodes.is_empty());
                if reply_id(&item.kept.content).is_some() {
                    let _ = settled_until(&item.kept.extra.quote, quote_deadline).await;
                }
                let mut body = Message::new().text(format!("{}\n", headline(item)));
                if let Some(line) = quote_line(item) {
                    body = body.text(format!("{line}\n"));
                }
                let (inline, separate) = adapt(&item.kept.content, view.is_some());
                body.0.extend(inline.0);
                // 合并转发在节点里做不成一张卡片：退成文字聊天记录，跟着这条一起转。
                if let Some(view) = view {
                    body = body.text(format!("\n{}", forward_text(view)));
                }
                for segment in &separate {
                    body = body.text(format!("[{}]", media_label(&segment.type_)));
                }
                let name = if item.kept.name.is_empty() { &item.kept.user_id } else { &item.kept.name };
                message = message.node_custom(&item.kept.user_id, name, body);
            }
            send_msg(ctx, writer.clone(), group, user, message).await?;
            Ok(())
        }
    }
}

/// 退路：整批只用文字说一遍，媒体写成「[图片]」，其余元素照 [`adapt`] 的写法。
fn plain(batch: &[Recalled], error: &str) -> Message {
    let mut text = String::new();
    for item in batch {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&headline(item));
        text.push('\n');
        if let Some(line) = quote_line(item) {
            text.push_str(&line);
            text.push('\n');
        }
        let view = item
            .kept
            .extra
            .forward
            .get()
            .filter(|view| !view.nodes.is_empty());
        for segment in &item.kept.content.0 {
            let kind = segment.type_.as_str();
            if MEDIA.contains(&kind) || kind == "audio" {
                text.push_str(&format!("[{}]", media_label(kind)));
                continue;
            }
            // 合并转发按展开出来的聊天记录另外写在后面；没展开出来就留一句占位。
            if kind == "forward" && view.is_some() {
                continue;
            }
            let (inline, _) = adapt(&Message(vec![segment.clone()]), false);
            for part in inline.0.iter().filter(|part| part.type_ == "text") {
                text.push_str(part.data.get("text").and_then(|v| v.as_str()).unwrap_or(""));
            }
            if kind == "face" || kind == "mface" {
                text.push_str("[表情]");
            }
        }
        if let Some(view) = view {
            text.push('\n');
            text.push_str(&forward_text(view));
        }
    }
    let reason = crate::storage::truncate_chars(error, 80);
    Message::new().text(format!("{text}\n（原样转存没发出去，只留了文字：{reason}）"))
}

// ==================== 控制台 ====================

/// 这次运行的账：转存了几条、几次没找到原文、最近一条与最近一次失败。
#[derive(Default)]
struct Tally {
    forwarded: u64,
    missed: u64,
    last: Option<(i64, String)>,
    error: Option<(i64, String)>,
}

fn tally() -> std::sync::MutexGuard<'static, Tally> {
    static TALLY: OnceLock<Mutex<Tally>> = OnceLock::new();
    TALLY
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn describe(bot: &BotStatus, config: &Config) -> String {
    match destination(bot, config) {
        Some((Some(group), _)) => format!("群 {group}"),
        Some((None, Some(user))) if config.target.trim().is_empty() => {
            let login = bot.login_user.get();
            match login.name.as_deref().filter(|name| !name.is_empty()) {
                Some(name) => format!("号主自己的私聊（{name} · {user}），在 QQ 里打开和自己的会话就能看到"),
                None => format!("号主自己的私聊（{user}），在 QQ 里打开和自己的会话就能看到"),
            }
        }
        Some((None, Some(user))) => format!("私聊 {user}"),
        _ => "还没有去处".to_string(),
    }
}

/// 控制台插件页上的那张「现在的情况」：几行字，加一个试发按钮。
pub(crate) fn live(ctx: &Context) -> serde_json::Value {
    let config = get_config_or_default::<Config>(ctx);
    let mut lines = Vec::new();
    let links = satori::logins();
    if links.is_empty() {
        lines.push("还没连上账号，连上后这里显示转存的去处。".to_string());
    }
    for (bot, _) in &links {
        let lead = if config.enabled { "转存到" } else { "打开后转存到" };
        lines.push(format!("{lead}：{}", describe(bot, &config)));
    }
    let tally = tally();
    if tally.forwarded > 0 {
        let last = tally
            .last
            .as_ref()
            .map(|(at, line)| format!("，最近一条 {}：{}", clock(*at), line.trim_start_matches("🗑 ")))
            .unwrap_or_default();
        lines.push(format!("这次运行转存了 {} 条{last}", tally.forwarded));
    } else if config.enabled {
        lines.push("这次运行还没有人撤回过消息。".to_string());
    }
    if tally.missed > 0 {
        lines.push(format!(
            "有 {} 次撤回没找到原文：撤的是机器人启动、插件打开之前，或 {} 分钟以前的消息；QQ 端还得是知弦 0.37.1 以上（升级后重启手机才生效）。",
            tally.missed, config.retention_minutes
        ));
    }
    if config.enabled && config.forward_records {
        lines.push("合并转发会连里面的聊天记录一起转存成一张合并转发；引用回复会带上被引用的原话。".to_string());
    }
    if !config.self_recalls {
        lines.push("自己撤回自己的消息不转存；想拿自己的消息试，打开「自己撤回的也存」。".to_string());
    }
    if let Some((at, error)) = &tally.error {
        lines.push(format!("{} 转存失败：{error}", clock(*at)));
    }
    serde_json::json!({ "lines": lines, "test": !links.is_empty() })
}

/// 往去处发一条说明，看看那边收不收得到。
pub(crate) async fn test(ctx: &Context) -> Result<String, String> {
    let config = get_config_or_default::<Config>(ctx);
    let links = satori::logins();
    if links.is_empty() {
        return Err("还没连上账号，等机器人连上再试".into());
    }
    let mut sent = Vec::new();
    for (bot, writer) in links {
        let Some((group, user)) = destination(&bot, &config) else {
            continue;
        };
        let mut ctx = ctx.clone();
        ctx.bot = bot.clone();
        let text = format!(
            "🗑 防撤回试发：{}有人撤回消息时，原文会像这样转存到这里。",
            if config.enabled { "" } else { "插件还没打开；打开后" }
        );
        send_msg(&ctx, writer, group.as_deref(), user.as_deref(), Message::new().text(text))
            .await
            .map_err(|error| format!("发不出去：{error}"))?;
        sent.push(describe(&bot, &config));
    }
    match sent.as_slice() {
        [] => Err("没有去处：设置 target，或等登录账号就绪".into()),
        [one] => Ok(format!("已发到{}", one.split('，').next().unwrap_or(one))),
        many => Ok(format!("已发到 {} 处", many.len())),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use simd_json::base::ValueAsArray;
    use simd_json::owned::Object;

    fn segment(kind: &str, pairs: &[(&str, &str)]) -> Segment {
        let mut data = Object::new();
        for (key, value) in pairs {
            data.insert((*key).into(), OwnedValue::from(*value));
        }
        Segment::new(kind, data)
    }

    fn kept(content: Message) -> Kept {
        Kept {
            self_id: "1".into(),
            message_id: "9001".into(),
            channel_id: "280183116".into(),
            group: Some("280183116".into()),
            group_name: "测试群".into(),
            user_id: "42".into(),
            name: "张三".into(),
            at: 0,
            content,
            extra: Extra::default(),
        }
    }

    fn recalled(content: Message) -> Recalled {
        Recalled {
            kept: kept(content),
            operator: String::new(),
            recalled_at: 0,
        }
    }

    fn view(nodes: Vec<forward::Node>, truncated: bool) -> forward::View {
        forward::View {
            nodes,
            truncated,
            notes: Vec::new(),
        }
    }

    fn node(user: &str, name: &str, message: Message) -> forward::Node {
        forward::Node {
            depth: 0,
            message_id: None,
            user_id: user.into(),
            name: name.into(),
            time: 0,
            message,
        }
    }

    #[test]
    fn quote_line_names_the_quoted_author() {
        let item = recalled(Message(vec![segment("reply", &[("id", "123")])]));
        item.kept
            .extra
            .quote
            .set(Some(Quote {
                user_id: "7".into(),
                name: "李四".into(),
                content: Message::new().text("这是原话"),
            }))
            .ok();
        let line = quote_line(&item).unwrap_or_default();
        assert_eq!(line, "↪ 回复 李四（7）：这是原话");
    }

    #[test]
    fn quote_line_says_so_when_the_original_is_gone() {
        let item = recalled(Message(vec![segment("reply", &[("id", "123")])]));
        assert_eq!(
            quote_line(&item).as_deref(),
            Some("↪ 回复了一条消息（原文没取到）")
        );
    }

    #[test]
    fn quote_line_is_absent_without_a_reply() {
        assert_eq!(quote_line(&recalled(Message::new().text("普通消息"))), None);
    }

    #[test]
    fn adapt_turns_at_into_text_and_leaves_quotes_to_quote_line() {
        let content = Message(vec![
            segment("reply", &[("id", "1")]),
            segment("at", &[("qq", "7"), ("name", "李四")]),
            segment("text", &[("text", "看这个")]),
        ]);
        let (inline, separate) = adapt(&content, false);
        assert!(separate.is_empty());
        let text: String = inline
            .0
            .iter()
            .filter(|part| part.type_ == "text")
            .map(|part| part.data.get("text").and_then(|v| v.as_str()).unwrap_or(""))
            .collect();
        assert_eq!(text, "@李四 看这个");
    }

    #[test]
    fn adapt_only_mentions_the_forward_when_no_card_is_sent() {
        let content = Message(vec![segment("forward", &[("id", "res")])]);
        let (with_card, _) = adapt(&content, true);
        assert!(with_card.0.is_empty());
        let (without, _) = adapt(&content, false);
        assert_eq!(without.0.len(), 1);
        assert_eq!(
            without.0[0].data.get("text").and_then(|v| v.as_str()),
            Some("[合并转发（内容没取到）]")
        );
    }

    #[tokio::test]
    async fn forward_card_keeps_authors_and_order() {
        let view = view(
            vec![
                node("7", "李四", Message::new().text("第一句")),
                node("", "", Message(vec![segment("forward", &[("id", "n")])])),
                node("8", "王五", Message::new().text("第三句")),
            ],
            false,
        );
        let card = forward_card(&view).await.expect("一张卡");
        let nodes: Vec<_> = card.0.iter().filter(|seg| seg.type_ == "node").collect();
        assert_eq!(nodes.len(), 3);
        assert_eq!(
            nodes[0].data.get("nickname").and_then(|v| v.as_str()),
            Some("李四")
        );
        // 没有作者的节点用 0 占位，名字空着也不报错。
        assert_eq!(
            nodes[1].data.get("user_id").and_then(|v| v.as_str()),
            Some("0")
        );
        // 节点里的二次转发变成占位文字，不会带出一张打不开的子卡片。
        let empty = Vec::new();
        let body = nodes[1]
            .data
            .get("content")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty);
        assert!(body.iter().any(|part| {
            part.get("data")
                .and_then(|data| data.get("text"))
                .and_then(|text| text.as_str())
                .is_some_and(|text| text.contains("合并转发里的转发"))
        }));
    }

    #[tokio::test]
    async fn forward_card_notes_what_did_not_fit() {
        let view = view(vec![node("7", "李四", Message::new().text("一"))], true);
        let card = forward_card(&view).await.expect("一张卡");
        let nodes: Vec<_> = card.0.iter().filter(|seg| seg.type_ == "node").collect();
        assert_eq!(nodes.len(), 2);
        assert_eq!(
            nodes[1].data.get("nickname").and_then(|v| v.as_str()),
            Some("防撤回")
        );
    }

    #[test]
    fn forward_text_reads_like_a_transcript() {
        let view = view(vec![node("7", "李四", Message::new().text("第一句"))], false);
        let text = forward_text(&view);
        assert!(text.starts_with("合并转发里的聊天记录："));
        assert!(text.contains("李四(7)"));
        assert!(text.contains("第一句"));
    }

    #[tokio::test]
    async fn prepare_sends_a_card_for_a_recalled_forward() {
        let item = recalled(Message(vec![segment("forward", &[("id", "res")])]));
        item.kept
            .extra
            .forward
            .set(view(vec![node("7", "李四", Message::new().text("第一句"))], false))
            .ok();
        let prepared = prepare(&item, true).await;
        assert!(prepared.forward.is_some());
        let body: String = prepared
            .body
            .0
            .iter()
            .filter(|part| part.type_ == "text")
            .map(|part| part.data.get("text").and_then(|v| v.as_str()).unwrap_or(""))
            .collect();
        assert!(body.contains("一条合并转发"));
        // 卡片另外发，正文里不再重复一遍聊天记录。
        assert!(!body.contains("合并转发里的聊天记录"));
    }

    #[tokio::test]
    async fn prepare_writes_the_transcript_when_records_are_off() {
        let item = recalled(Message(vec![segment("forward", &[("id", "res")])]));
        item.kept
            .extra
            .forward
            .set(view(vec![node("7", "李四", Message::new().text("第一句"))], false))
            .ok();
        let prepared = prepare(&item, false).await;
        assert!(prepared.forward.is_none());
        let body: String = prepared
            .body
            .0
            .iter()
            .filter(|part| part.type_ == "text")
            .map(|part| part.data.get("text").and_then(|v| v.as_str()).unwrap_or(""))
            .collect();
        assert!(body.contains("合并转发里的聊天记录"));
    }
}

