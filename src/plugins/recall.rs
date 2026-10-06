use crate::adapters::satori::{LockedWriter, api};
use crate::command::match_command;
use crate::event::{Context, Event, EventType, SendPacket};
use crate::plugins::{PluginConfig, PluginError, Receipt, get_config_or_default};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use simd_json::derived::ValueObjectAccessAsScalar;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const LOG_TARGET: &str = "Plugin/Recall";

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    enabled: bool,
    follow_recall: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            follow_recall: true,
        }
    }
}

impl PluginConfig for Config {
    const NAME: &'static str = "recall";
}


// Only keep recent trigger -> response relations. Nothing here survives a restart.
const RETENTION: Duration = Duration::from_mins(30);
const MAX_TRIGGERS: usize = 4096;

type Key = (String, String, String); // bot identity, channel, trigger message ID

struct Entry {
    at: Instant,
    recalled: bool,
    replies: Vec<String>,
}

#[derive(Default)]
struct Pending(HashMap<Key, Entry>);

impl Pending {
    fn entry(&mut self, key: Key, now: Instant) -> &mut Entry {
        self.0
            .retain(|_, entry| now.duration_since(entry.at) < RETENTION);
        if !self.0.contains_key(&key) && self.0.len() >= MAX_TRIGGERS
            && let Some(oldest) = self
                .0
                .iter()
                .min_by_key(|(_, entry)| entry.at)
                .map(|(key, _)| key.clone())
            {
                self.0.remove(&oldest);
            }
        self.0.entry(key).or_insert_with(|| Entry {
            at: now,
            recalled: false,
            replies: Vec::new(),
        })
    }

    fn sent(&mut self, key: Key, ids: &[String], now: Instant) -> Vec<String> {
        let entry = self.entry(key, now);
        if entry.recalled {
            ids.to_vec()
        } else {
            for id in ids {
                if !entry.replies.contains(id) {
                    entry.replies.push(id.clone());
                }
            }
            Vec::new()
        }
    }

    fn recalled(&mut self, key: Key, now: Instant) -> Vec<String> {
        let entry = self.entry(key, now);
        entry.recalled = true;
        std::mem::take(&mut entry.replies)
    }
}

static PENDING: OnceLock<Mutex<Pending>> = OnceLock::new();
fn pending() -> &'static Mutex<Pending> {
    PENDING.get_or_init(|| Mutex::new(Pending::default()))
}

fn enabled(ctx: &Context) -> bool {
    let config: Config = get_config_or_default(ctx);
    config.enabled && config.follow_recall
}

fn id(event: &Event) -> Option<String> {
    event
        .get_str("message_id")
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

fn key(ctx: &Context, event: &Event) -> Option<Key> {
    let channel = event
        .get_str("channel_id")
        .filter(|id| !id.is_empty())?
        .to_string();
    Some((
        format!(
            "{}:{}:{}",
            ctx.bot.adapter,
            ctx.bot.platform,
            ctx.bot.login_user.get().id
        ),
        channel,
        id(event)?,
    ))
}

fn same_channel(source: &Event, packet: &SendPacket) -> bool {
    match source.get_str("group_id").filter(|id| !id.is_empty()) {
        Some(group) => packet.group_id() == Some(group),
        None => packet.group_id().is_none() && packet.user_id() == source.get_str("user_id"),
    }
}

fn user_trigger(ctx: &Context, event: &Event) -> bool {
    event.get_str("satori_type") == Some("message-created")
        && event
            .get_str("user_id")
            .is_some_and(|id| !id.is_empty() && id != ctx.bot.self_id())
        && !event.get_bool("manual_self").unwrap_or(false)
}

async fn delete_replies(ctx: &Context, writer: LockedWriter, channel: &str, ids: Vec<String>) {
    for id in ids {
        if let Err(error) = api::delete_msg_in(ctx, writer.clone(), channel, &id).await {
            warn!(target: LOG_TARGET, "跟随撤回消息 {id} 失败: {error}");
        }
    }
}

/// 发出之后的钩子：`message.create` 已返回全部 ID（含拆开发的媒体消息）。
/// 回复还在生成、发送途中就被撤回的情形也在这里处理。
pub fn on_sent<'a>(
    ctx: &'a Context,
    writer: &'a LockedWriter,
    sent: &'a Receipt<'a>,
) -> BoxFuture<'a, ()> {
    Box::pin(record_sent(ctx, writer.clone(), sent.packet, sent.message_ids))
}

async fn record_sent(ctx: &Context, writer: LockedWriter, packet: &SendPacket, ids: &[String]) {
    if ids.is_empty() || !enabled(ctx) {
        return;
    }
    let Some(source) = packet
        .original_event
        .as_ref()
        .filter(|event| user_trigger(ctx, event) && same_channel(event, packet))
    else {
        return;
    };
    let Some(key) = key(ctx, source) else {
        return;
    };
    let channel = key.1.clone();
    let to_delete = pending().lock().unwrap().sent(key, ids, Instant::now());
    delete_replies(ctx, writer, &channel, to_delete).await;
}

pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        if enabled(&ctx)
            && let EventType::Satori(event) = &ctx.event
            && event.get_str("satori_type") == Some("message-deleted")
            && event
                .get_str("user_id")
                .is_none_or(|id| id != ctx.bot.self_id())
            && let Some(key) = key(&ctx, event)
        {
            let channel = key.1.clone();
            let ids = pending().lock().unwrap().recalled(key, Instant::now());
            delete_replies(&ctx, writer.clone(), &channel, ids).await;
        }

        if let Some(cmd) = match_command(&ctx, "撤回") {
            let Some(reply_id_str) = cmd.reply_id else {
                if let Some(msg) = ctx.as_message() {
                    crate::adapters::satori::send_msg(
                        &ctx,
                        writer,
                        msg.group_id(),
                        Some(msg.user_id()),
                        "请先引用要撤回的消息，再发送撤回指令。",
                    )
                    .await?;
                }
                return Ok(None);
            };
            let Some(msg) = ctx.as_message() else { return Ok(Some(ctx)) };
            let command_msg_id = msg.message_id();

            {
                let target_id = reply_id_str.as_str();
                if let Err(error) = api::delete_msg(&ctx, writer.clone(), target_id).await {
                    warn!(target: LOG_TARGET, "撤回引用消息 {target_id} 失败: {error}");
                    crate::adapters::satori::send_msg(
                        &ctx,
                        writer,
                        msg.group_id(),
                        Some(msg.user_id()),
                        "未能撤回引用消息。请检查机器人权限、消息归属和平台撤回时限后重试。",
                    )
                    .await?;
                    return Ok(None);
                }
                if let Err(error) = api::delete_msg(&ctx, writer, command_msg_id).await {
                    warn!(target: LOG_TARGET, "撤回指令消息 {command_msg_id} 失败: {error}");
                }
                return Ok(None);
            }
        }
        Ok(Some(ctx))
    })
}

