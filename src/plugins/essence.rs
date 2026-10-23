//! 群精华：引用一条消息，发「加精」把它收进群精华，「取消加精」移出来。
//!
//! 只用群里已有的权限：按的人得是群主或管理员（机器人管理员也可以），机器人在这个群
//! 也得是群主或管理员，否则 QQ 会回绝。与智能对话里的精华动作同一条实现端接口
//! （`internal/essence`），不额外扩协议。
//!
//! 成功只回一句短话，不 @ 任何人；失败把原因说清楚，细节写日志。被加精的消息要在
//! 群历史里找得到：刚收到的、或模块能向内核补取的旧消息都行；太久远、内核也翻不到
//! 时，服务端拿不到 `msgSeq`，会直接拒绝。

use crate::adapters::satori::{LockedWriter, api, send_msg};
use crate::command::match_word_command;
use crate::event::Context;
use crate::plugins::{PluginConfig, PluginError};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

const LOG_TARGET: &str = "Plugin/Essence";

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
    const NAME: &'static str = "essence";
}

const ADD: &[&str] = &["加精", "设精华"];
const REMOVE: &[&str] = &["取消加精", "撤精"];

pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        // 用「整词」匹配：「加精了」「加精是什么意思」这类闲聊不该被当成指令接走。
        let action = ADD
            .iter()
            .find_map(|word| match_word_command(&ctx, word))
            .map(|match_| (match_, true))
            .or_else(|| {
                REMOVE
                    .iter()
                    .find_map(|word| match_word_command(&ctx, word))
                    .map(|match_| (match_, false))
            });
        let Some((matched, add)) = action else {
            return Ok(Some(ctx));
        };
        let Some(msg) = ctx.as_message() else {
            return Ok(Some(ctx));
        };
        let user_id = msg.user_id();
        let Some(group_id) = msg.group_id() else {
            send_msg(
                &ctx,
                writer,
                None,
                Some(user_id),
                "加精是群里的动作：在群里引用一条消息再发「加精」。",
            )
            .await?;
            return Ok(None);
        };

        // 按的人：机器人管理员，或本群群主、管理员。
        let allowed = crate::access::is_manager(&ctx)
            || msg.sender_role().is_some_and(api::is_staff);
        if !allowed {
            send_msg(
                &ctx,
                writer,
                Some(group_id),
                Some(user_id),
                "加精要群主或管理员来按。",
            )
            .await?;
            return Ok(None);
        }

        let Some(message_id) = matched.reply_id else {
            send_msg(
                &ctx,
                writer,
                Some(group_id),
                Some(user_id),
                "引用一条要处理的消息，再发「加精」或「取消加精」。",
            )
            .await?;
            return Ok(None);
        };

        // 机器人自己得管得动，否则这次调用只会换来一个服务端拒绝。
        let role = api::own_role(&ctx, writer.clone(), group_id).await;
        if !api::is_staff(&role) {
            send_msg(
                &ctx,
                writer,
                Some(group_id),
                Some(user_id),
                "机器人还不是本群群主或管理员，设不了精华。",
            )
            .await?;
            return Ok(None);
        }

        match api::set_group_essence(&ctx, writer.clone(), group_id, &message_id, add).await {
            Ok(()) => {
                info!(
                    target: LOG_TARGET,
                    "[Group({group_id})] {}精华消息 {message_id}（操作人 {user_id}）",
                    if add { "设置" } else { "取消" }
                );
                send_msg(
                    &ctx,
                    writer,
                    Some(group_id),
                    Some(user_id),
                    if add { "已设为精华。" } else { "已移出精华。" },
                )
                .await?;
            }
            Err(error) => {
                error!(
                    target: LOG_TARGET,
                    "[Group({group_id})] 设置精华消息 {message_id} 失败: {error}"
                );
                send_msg(
                    &ctx,
                    writer,
                    Some(group_id),
                    Some(user_id),
                    "没有设上：机器人需要是群主或管理员，且这条消息还在记录里。原因已写进日志。",
                )
                .await?;
            }
        }

        Ok(None)
    })
}
