use crate::adapters::satori::{LockedWriter, api, send_msg};
use crate::command::first_command_match;
use crate::event::Context;
use crate::message::Message;
use crate::plugins::{PluginConfig, PluginError};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use simd_json::base::ValueAsScalar;

const LOG_TARGET: &str = "Plugin/Sticker";

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

// 收藏指令（内置，无需配置）
const COMMANDS: &[&str] = &["表情转图片", "收", "偷", "存表情"];

impl PluginConfig for Config {
    const NAME: &'static str = "sticker";
}

pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let Some(msg) = ctx.as_message() else {
            return Ok(Some(ctx));
        };
        let Some(matched) = first_command_match(&ctx, COMMANDS) else {
            return Ok(Some(ctx));
        };

        // 回复都引用触发指令的那条消息
        let reply = |text: &str| Message::new().reply(msg.message_id()).text(text);
        let say = |message: Message| {
            send_msg(&ctx, writer.clone(), msg.group_id(), Some(msg.user_id()), message)
        };

        // 必须通过引用回复
        let Some(reply_id) = matched.reply_id else {
            say(reply("❌ 请引用你要保存的表情或图片，然后重发这条指令")).await?;
            return Ok(None);
        };

        let original = match api::get_msg(&ctx, writer.clone(), &reply_id).await {
            Ok(original) => original,
            Err(error) => {
                warn!(target: LOG_TARGET, "取被引用的消息 {reply_id} 失败: {error}");
                say(reply("❌ 获取原消息失败，消息可能已过期")).await?;
                return Ok(None);
            }
        };
        let urls: Vec<&str> = original
            .message
            .0
            .iter()
            .filter(|seg| seg.type_ == "image")
            .filter_map(|seg| seg.data.get("url").and_then(|v| v.as_str()))
            .collect();

        if urls.is_empty() {
            say(reply("❌ 检测不到图片或表情\n商城表情这类特殊格式暂时读不出来")).await?;
        } else {
            let message = urls
                .into_iter()
                .fold(reply("✅ 图片提取成功：\n"), |message, url| message.image(url));
            say(message).await?;
        }
        Ok(None)
    })
}
