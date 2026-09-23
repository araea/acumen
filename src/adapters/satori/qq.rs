//! QQ extensions; standard reactions still use reaction.create/delete/list.
use super::{BotError, LockedWriter};
use crate::event::Context;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
pub struct Capabilities {
    pub version: String,
    pub actions: Vec<String>,
    #[serde(default)]
    pub params: std::collections::HashMap<String, String>,
}
impl Capabilities {
    pub fn supports(&self, action: &str) -> bool {
        self.actions.iter().any(|a| a == action)
    }
}
#[derive(Debug, Deserialize)]
pub struct Reaction {
    pub emoji_id: String,
    pub count: u64,
    #[serde(rename = "self")]
    pub by_self: bool,
}
#[derive(Debug, Deserialize)]
pub struct ReactionSummary {
    pub message_id: String,
    pub data: Vec<Reaction>,
    pub source: String,
    pub observed_at: u64,
}

pub async fn call<P: serde::Serialize, R: serde::de::DeserializeOwned>(
    ctx: &Context,
    writer: &LockedWriter,
    action: &str,
    params: P,
) -> Result<R, BotError> {
    if ctx.bot.adapter != "satori-qq" {
        return Err("当前适配器未声明 satori-qq 扩展".into());
    }
    if action.is_empty()
        || !action
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err("无效的 QQ 扩展动作名称".into());
    }
    writer
        .call(ctx, &format!("internal/{action}"), params)
        .await
}
pub async fn capabilities(ctx: &Context, writer: &LockedWriter) -> Result<Capabilities, BotError> {
    call(ctx, writer, "capabilities", json!({})).await
}
pub async fn poke(
    ctx: &Context,
    writer: &LockedWriter,
    channel: &str,
    user: &str,
) -> Result<Value, BotError> {
    call(
        ctx,
        writer,
        "poke",
        json!({"channel_id":channel,"user_id":user}),
    )
    .await
}
pub async fn reactions(
    ctx: &Context,
    writer: &LockedWriter,
    channel: &str,
    message: &str,
) -> Result<ReactionSummary, BotError> {
    call(
        ctx,
        writer,
        "reaction_summary",
        json!({"channel_id":channel,"message_id":message}),
    )
    .await
}
pub async fn clear_reactions(
    ctx: &Context,
    writer: &LockedWriter,
    channel: &str,
    message: &str,
) -> Result<Value, BotError> {
    call(
        ctx,
        writer,
        "reaction_clear",
        json!({"channel_id":channel,"message_id":message}),
    )
    .await
}
