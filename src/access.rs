//! 谁可以管理机器人：号主、全局管理员与本机控制台。
//!
//! 判据原先挂在 ctl 插件里，重启、入群欢迎、奶龙提醒、智能体设置都要去 `ctl::` 借。
//! 「是不是管理员」是框架级的问题，配置键只是它当前的落点，因此放在这里；ctl 仍负责
//! 改配置与插件开关，不再兼任身份判定。

use crate::adapters::satori::{LockedWriter, api};
use crate::event::Context;
use toml::Value;

/// 拒绝的固定说法：号主与 ctl.admins 之外的人不能做管理操作。
pub const DENIED: &str = "此操作仅限号主（登录账号本人在 QQ 里亲手发）与 ctl.admins 中的全局管理员；其他人要由号主在控制台或 /ctl 里加进 admins";

/// `[ctl].admins`：号主与他信任的其他全局管理员。
pub fn admin_ids(ctx: &Context) -> Vec<String> {
    let config = ctx.config.read().unwrap();
    config
        .plugins
        .get("ctl")
        .and_then(|value| value.get("admins"))
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// 这个用户是不是号主：`ctl.admins` 里的人，或账号自己**亲手**打的字（`manual`）。
///
/// 号主与机器人共用同一个 QQ 号，机器人发出去的话也会作为「自己发的消息」回声进来——
/// 那一份不是号主说的，而是任何能让机器人开口的人（`echo` 之类）借机器人的嘴说的，
/// 所以账号自己只有在 `manual_self` 标着「亲手打的」时才算号主；管理员名单里恰好写着
/// 登录号（线上就是这样）也一样。
pub fn is_owner(ctx: &Context, user_id: &str, manual: bool) -> bool {
    if user_id.is_empty() {
        return false;
    }
    if ctx.bot.self_id() == user_id {
        return manual;
    }
    admin_ids(ctx).iter().any(|id| id == user_id)
}

/// 能不能管：本机控制台、号主，以及 `ctl.admins` 里的人。
pub fn is_manager(ctx: &Context) -> bool {
    if ctx.bot.adapter == "console" && ctx.bot.platform == "console" {
        return true;
    }
    ctx.as_message()
        .is_some_and(|msg| is_owner(ctx, msg.user_id(), msg.is_manual_self()))
}

/// 这条群消息的发送者在本群管不管事：[`is_manager`]，或本群群主、管理员。
///
/// 消息上带的身份是实现端顺手给的，说不准时写成普通成员（知弦 0.50 起干脆不带，
/// 适配器补成 member）——只认它，真管理员会被当成群友，管理指令静默不理。所以消息上
/// 没说是管理时按成员身份正经问一次（有缓存，同一个人十分钟内只问一回，见 `roles`）。
/// 机器人自己发出去的回声不算；号主在账号上亲手发的已经由 [`is_manager`] 认下。
pub async fn is_group_staff(ctx: &Context, writer: &LockedWriter, group: &str) -> bool {
    if is_manager(ctx) {
        return true;
    }
    let Some(message) = ctx.as_message() else {
        return false;
    };
    // 机器人自己的回声带着账号的真实身份（往往就是管理员），先挡掉，免得谁都能借它的嘴下指令。
    let user = message.user_id();
    if user.is_empty() || group.is_empty() || user == ctx.bot.self_id() {
        return false;
    }
    if message.sender_role().is_some_and(api::is_staff) {
        return true;
    }
    api::member_role(ctx, writer.clone(), group, user)
        .await
        .is_some_and(|role| api::is_staff(&role))
}
