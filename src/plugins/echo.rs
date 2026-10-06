use crate::adapters::satori::{LockedWriter, send_msg};
use crate::plugins::PluginConfig;
use crate::command::match_command;
use crate::event::Context;
use crate::plugins::PluginError;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl PluginConfig for Config {
    const NAME: &'static str = "echo";
}


pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        if let Some(cmd) = match_command(&ctx, "echo") {
            if cmd.args.is_empty() {
                return Ok(Some(ctx));
            }

            let Some(msg) = ctx.as_message() else { return Ok(Some(ctx)) };

            let group_id = msg.group_id();
            let user_id = msg.user_id();

            // 直接将解析出的参数部分（消息段列表）作为内容发送，实现富文本回显
            send_msg(&ctx, writer, group_id, Some(user_id), cmd.args).await?;

            return Ok(None);
        }

        Ok(Some(ctx))
    })
}

