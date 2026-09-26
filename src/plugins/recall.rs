use crate::adapters::satori::{LockedWriter, api};
use crate::command::match_command;
use crate::config::build_config;
use crate::event::{Context, Event, EventType, SendPacket};
use crate::plugins::{PluginError, get_config_or_default};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use simd_json::derived::ValueObjectAccessAsScalar;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use toml::Value;

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Config {
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

pub fn default_config() -> Value {
    build_config(Config::default())
}

// Only keep recent trigger -> response relations. Nothing here survives a restart.
const RETENTION: Duration = Duration::from_secs(30 * 60);
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
    let config: Config = get_config_or_default(ctx, "recall");
    config.enabled && config.follow_recall
}

fn id(event: &Event) -> Option<String> {
    event
        .get_str("message_id_str")
        .filter(|id| !id.is_empty() && *id != "0")
        .map(str::to_owned)
        .or_else(|| {
            event
                .get_i64("message_id")
                .filter(|id| *id != 0)
                .map(|id| id.to_string())
        })
}

fn key(ctx: &Context, event: &Event) -> Option<Key> {
    let channel = event
        .get_i64("group_id")
        .filter(|id| *id != 0)
        .map(|id| id.to_string())
        .or_else(|| {
            event
                .get_i64("user_id")
                .filter(|id| *id != 0)
                .map(|id| format!("private:{id}"))
        })?;
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
    if let Some(group) = source.get_i64("group_id").filter(|id| *id != 0) {
        packet.group_id() == Some(group)
    } else {
        packet.group_id().is_none_or(|id| id == 0) && packet.user_id() == source.get_i64("user_id")
    }
}

fn user_trigger(ctx: &Context, event: &Event) -> bool {
    event.get_str("satori_type") == Some("message-created")
        && event
            .get_i64("user_id")
            .is_some_and(|id| id != 0 && id.to_string() != ctx.bot.login_user.get().id)
        && !event.get_bool("manual_self").unwrap_or(false)
}

async fn delete_replies(ctx: &Context, writer: LockedWriter, channel: &str, ids: Vec<String>) {
    for id in ids {
        let result: Result<serde_json::Value, _> = writer
            .call(
                ctx,
                "message.delete",
                serde_json::json!({
                    "channel_id": channel, "message_id": id
                }),
            )
            .await;
        if let Err(error) = result {
            warn!(target: "Plugin/Recall", "跟随撤回消息 {id} 失败: {error}");
        }
    }
}

/// Called after message.create has returned all IDs (including split media messages).
/// A withdrawal while the reply was still being generated/sent is handled here too.
pub async fn record_sent(ctx: &Context, writer: LockedWriter, packet: &SendPacket, ids: &[String]) {
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
                .get_i64("user_id")
                .is_none_or(|id| id.to_string() != ctx.bot.login_user.get().id)
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
            let msg = match ctx.as_message() {
                Some(m) => m,
                None => return Ok(Some(ctx)),
            };
            let command_msg_id = msg.message_id();

            if let Ok(target_id) = reply_id_str.parse::<i64>() {
                if let Err(error) = api::delete_msg(&ctx, writer.clone(), target_id).await {
                    warn!(target: "Plugin/Recall", "撤回引用消息 {target_id} 失败: {error}");
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
                    warn!(target: "Plugin/Recall", "撤回指令消息 {command_msg_id} 失败: {error}");
                }
                return Ok(None);
            }
        }
        Ok(Some(ctx))
    })
}

/// Validate control edits against the plugin's actual configuration type.
pub fn validate_config(value: &toml::Value) -> Result<(), String> {
    <Config as serde::Deserialize>::deserialize(value.clone())
        .map(|_| ())
        .map_err(|_| "配置类型不匹配（请检查数组元素、字段类型及整数范围）".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: &str) -> Key {
        ("bot".into(), "group".into(), id.into())
    }

    #[test]
    fn follow_recall_tracks_multiple_responses_once() {
        let mut state = Pending::default();
        let now = Instant::now();
        assert!(
            state
                .sent(key("1"), &["a".into(), "b".into()], now)
                .is_empty()
        );
        assert!(
            state
                .sent(key("1"), &["b".into(), "c".into()], now)
                .is_empty()
        );
        assert_eq!(state.recalled(key("1"), now), vec!["a", "b", "c"]);
        assert!(state.recalled(key("1"), now).is_empty());
        assert_eq!(state.sent(key("1"), &["late".into()], now), vec!["late"]);
        assert!(state.recalled(key("2"), now).is_empty());
        assert_eq!(state.sent(key("2"), &["after".into()], now), vec!["after"]);
    }

    #[test]
    fn follow_recall_is_scoped_and_bounded() {
        let mut state = Pending::default();
        let now = Instant::now();
        state.sent(key("expired"), &["old".into()], now - RETENTION);
        state.sent(key("1"), &["reply".into()], now);
        assert!(!state.0.contains_key(&key("expired")));
        assert!(
            state
                .recalled(("other-bot".into(), "group".into(), "1".into()), now)
                .is_empty()
        );
        assert_eq!(state.recalled(key("1"), now), vec!["reply"]);
        for i in 0..=MAX_TRIGGERS {
            state.recalled(key(&i.to_string()), now);
        }
        assert!(state.0.len() <= MAX_TRIGGERS);
    }

    #[test]
    fn configuration_defaults_on_upgrade() {
        let config: Config = toml::from_str("enabled = true").unwrap();
        assert!(config.follow_recall);
        let disabled: Config = toml::from_str("enabled = true\nfollow_recall = false").unwrap();
        assert!(!disabled.follow_recall);
        assert!(toml::from_str::<Config>("enabled = true\nfollow_recall = 'yes'").is_err());
    }

    #[test]
    fn only_track_responses_in_the_trigger_channel() {
        let event = |value| simd_json::serde::to_owned_value(value).unwrap();
        let packet = |group_id: Option<i64>, user_id: Option<i64>| SendPacket {
            action: "message.create".into(),
            repeat_guard: None,
            freshness: None,
            params: event(serde_json::json!({"group_id": group_id, "user_id": user_id})),
            original_event: None,
            receipt_message_ids: Default::default(),
        };
        let group = event(serde_json::json!({"group_id": 42, "user_id": 7}));
        assert!(same_channel(&group, &packet(Some(42), None)));
        assert!(!same_channel(&group, &packet(Some(43), None)));
        assert!(!same_channel(&group, &packet(None, Some(7))));
        let private = event(serde_json::json!({"user_id": 7}));
        assert!(same_channel(&private, &packet(None, Some(7))));
        assert!(!same_channel(&private, &packet(None, Some(8))));
    }
}
