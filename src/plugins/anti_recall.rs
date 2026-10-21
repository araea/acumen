//! 防撤回：有人撤回消息时，把原消息转存到指定的地方（私聊或一个群）。
//!
//! 实现端的撤回事件只带消息号，原文得自己记着：每条收到的消息在进流水线之前抄一份进
//! 内存（[`on_receive`]，按收到顺序，插件消费与否都不影响），留 `retention_minutes`。
//! 缓存里没有的（进程重启之前的）再去消息记录库里找一行摘要文字，图就没有了。
//!
//! 一口气撤一串（管理员清屏）不该刷爆存储频道：同一个去处两秒内的撤回攒成一批，
//! 一条就直接转，多条合成一条合并转发，每个节点署原作者。
//!
//! 去处留空就发到号主自己的私聊（QQ 里自己和自己的那个会话）：开了就能看到，不必先填什么。
//! 控制台的插件页会显示去处、这次运行转存了几条，也能先发一条试试（[`live`]、[`test`]）。
//!
//! 原样再发有几处要改写：@ 到了别的会话就是 @ 一个不在场的人，改成文字「@名字」；
//! 引用指向原会话的消息，摘掉、标一句「回复了一条消息」；语音、视频、文件不能和文字
//! 拼成一条，跟在后面单独发。

use crate::adapters::satori::{LockedWriter, send_msg};
use crate::event::{BotStatus, Context, EventType};
use crate::message::{Message, Segment};
use crate::plugins::{ChannelConfig, PluginConfig, PluginError, get_config_or_default};
use futures_util::future::BoxFuture;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde::{Deserialize, Serialize};
use simd_json::base::ValueAsScalar;
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsScalar};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

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
}

const CACHE_LIMIT: usize = 6000;

fn cache() -> &'static Mutex<VecDeque<Kept>> {
    static CACHE: OnceLock<Mutex<VecDeque<Kept>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 进流水线之前：消息抄一份；撤回就把原文找出来排进待转。
pub fn on_receive(ctx: &mut Context, writer: &LockedWriter) -> Option<BoxFuture<'static, Result<(), PluginError>>> {
    let EventType::Satori(raw) = &ctx.event else {
        return None;
    };
    remember(&ctx.bot, writer);
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
            if message_id.is_empty() || user.is_empty() || user == ctx.bot.self_id() {
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
            };
            let horizon = now() - i64::try_from(config.retention_minutes * 60).unwrap_or(7200);
            let mut cache = cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            while cache.front().is_some_and(|old| old.at < horizon) || cache.len() >= CACHE_LIMIT {
                cache.pop_front();
            }
            cache.push_back(kept);
            None
        }
        "message-deleted" => {
            let message_id = raw.get_str("message_id").unwrap_or("").to_string();
            let user = raw.get_str("user_id").unwrap_or("").to_string();
            let operator = raw.get_str("operator_id").unwrap_or("").to_string();
            let me = ctx.bot.self_id();
            if message_id.is_empty() || user == me || operator == me {
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
            let kept = cache()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .rev()
                .find(|kept| kept.message_id == message_id && kept.channel_id == channel && kept.self_id == me)
                .cloned();
            let (ctx, writer) = (ctx.clone(), writer.clone());
            Some(Box::pin(async move {
                let kept = if let Some(kept) = kept {
                    kept
                } else if let Some(kept) = from_records(&ctx, &message_id, &channel).await {
                    kept
                } else {
                    debug!(target: LOG_TARGET, "撤回的消息 {message_id} 没记着原文");
                    tally().missed += 1;
                    return Ok(());
                };
                let operator = if by_other { operator } else { String::new() };
                enqueue(ctx, writer, &config, Recalled { kept, operator, recalled_at: now() });
                Ok(())
            }))
        }
        _ => None,
    }
}

/// 防撤回不处理流水线里的事：活都在 [`on_receive`] 里干完了。
pub fn handle(ctx: Context, _writer: LockedWriter) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move { Ok(Some(ctx)) })
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
    })
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
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let batch = queue()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key)
            .unwrap_or_default();
        let count = batch.len() as u64;
        let last = batch.last().map(|item| headline(item).lines().next().unwrap_or("").to_string());
        let result = deliver(&ctx, &writer, group.as_deref(), user.as_deref(), batch).await;
        let mut tally = tally();
        match result {
            Ok(()) => {
                tally.forwarded += count;
                tally.last = last.map(|line| (now(), line));
                tally.error = None;
            }
            Err(error) => {
                warn!(target: LOG_TARGET, "撤回的消息没转存出去：{error}");
                tally.error = Some((now(), error.to_string()));
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
    format!(
        "🗑 {who} 在{place}撤回了一条消息{by}\n{} 发出 · {} 撤回",
        clock(kept.at),
        clock(item.recalled_at)
    )
}

/// 原样再发要改写的几类元素：返回能拼在一起发的那部分，与要单独发的媒体。
fn adapt(content: &Message) -> (Message, Vec<Segment>) {
    let mut inline = Message::new();
    let mut separate = Vec::new();
    for segment in &content.0 {
        let text = |key: &str| segment.data.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
        match segment.type_.as_str() {
            "text" | "image" | "face" | "mface" => inline.0.push(segment.clone()),
            "at" => {
                let name = text("name");
                let label = if name.is_empty() { text("id") } else { name };
                let label = if label.is_empty() { text("qq") } else { label };
                inline = inline.text(if text("type") == "all" { "@全体成员 ".to_string() } else { format!("@{label} ") });
            }
            "reply" | "quote" => inline = inline.text("（回复了一条消息）"),
            "record" | "audio" | "video" | "file" => separate.push(segment.clone()),
            "forward" => inline = inline.text("[合并转发]"),
            "poke" => inline = inline.text("[戳一戳]"),
            "dice" => inline = inline.text("[骰子]"),
            "rps" => inline = inline.text("[猜拳]"),
            "json" | "lightapp" | "xml" | "ark" => inline = inline.text("[卡片消息]"),
            other => inline = inline.text(format!("[{other}]")),
        }
    }
    (inline, separate)
}

async fn deliver(
    ctx: &Context,
    writer: &LockedWriter,
    group: Option<&str>,
    user: Option<&str>,
    batch: Vec<Recalled>,
) -> Result<(), PluginError> {
    match batch.as_slice() {
        [] => Ok(()),
        [item] => {
            let (inline, separate) = adapt(&item.kept.content);
            let mut message = Message::new().text(format!("{}\n", headline(item)));
            message.0.extend(inline.0);
            send_msg(ctx, writer.clone(), group, user, message).await?;
            for segment in separate {
                send_msg(ctx, writer.clone(), group, user, Message(vec![segment])).await?;
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
            for item in items {
                let (inline, separate) = adapt(&item.kept.content);
                let mut body = Message::new().text(format!("{}\n", headline(item)));
                body.0.extend(inline.0);
                for segment in &separate {
                    body = body.text(format!("[{}]", segment.type_));
                }
                let name = if item.kept.name.is_empty() { &item.kept.user_id } else { &item.kept.name };
                message = message.node_custom(&item.kept.user_id, name, body);
            }
            send_msg(ctx, writer.clone(), group, user, message).await?;
            Ok(())
        }
    }
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

/// 每个登录账号最近的那条连接：控制台要知道去处是谁、试发走哪条线。
type Link = (Arc<BotStatus>, LockedWriter);

fn links() -> std::sync::MutexGuard<'static, Vec<Link>> {
    static LINKS: OnceLock<Mutex<Vec<Link>>> = OnceLock::new();
    LINKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn remember(bot: &Arc<BotStatus>, writer: &LockedWriter) {
    let me = bot.self_id();
    if me.is_empty() {
        return;
    }
    let mut links = links();
    match links.iter_mut().find(|(known, _)| known.self_id() == me) {
        Some(link) if Arc::ptr_eq(&link.1, writer) => {}
        Some(link) => *link = (bot.clone(), writer.clone()),
        None => links.push((bot.clone(), writer.clone())),
    }
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
    let links = links().clone();
    if links.is_empty() {
        lines.push("还没收到过消息，连上账号后这里显示转存的去处。".to_string());
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
            "有 {} 次撤回没找到原文：撤的是机器人启动前或 {} 分钟以前的消息；QQ 端还得是知弦 0.37.1 以上（升级后重启手机才生效）。",
            tally.missed, config.retention_minutes
        ));
    }
    if let Some((at, error)) = &tally.error {
        lines.push(format!("{} 转存失败：{error}", clock(*at)));
    }
    serde_json::json!({ "lines": lines, "test": !links.is_empty() })
}

/// 往去处发一条说明，看看那边收不收得到。
pub(crate) async fn test(ctx: &Context) -> Result<String, String> {
    let config = get_config_or_default::<Config>(ctx);
    let links = links().clone();
    if links.is_empty() {
        return Err("还没连上账号，等机器人收到第一条消息再试".into());
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
