use crate::adapters::satori::api;
use crate::adapters::satori::{LockedWriter, send_msg};
use crate::command::match_command;
use crate::config::build_config;
use crate::event::{Context, EventType};
use crate::plugins::PluginError;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsScalar};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};
use toml::Value;

#[derive(Serialize, Deserialize)]
struct Config {
    enabled: bool,
}

pub fn default_config() -> Value {
    build_config(Config { enabled: true })
}

/// 每个群上次「打开展示成员群头衔开关」的时间，给这个写操作单独限一道频。
/// QQ 对群设置的写有过一天写太多就限流的先例（2026-09-19，code=1010），这个开关
/// 一旦查到是开的就不会再写，正常情况下这道频根本用不上；只在开关被反复关掉、
/// 或者短时间内很多人同时触发指令时兜底，避免对同一个群短时间内连续发起写请求。
static SWITCH_ENABLE_COOLDOWN_UNTIL: OnceLock<Mutex<HashMap<i64, Instant>>> = OnceLock::new();
const SWITCH_ENABLE_COOLDOWN: Duration = Duration::from_secs(300);

fn switch_enable_cooldowns() -> MutexGuard<'static, HashMap<i64, Instant>> {
    SWITCH_ENABLE_COOLDOWN_UNTIL
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// 这个群最近是否已经尝试过打开开关；没有就登记本次尝试，返回 true 放行。
fn should_try_enable_switch(group_id: i64) -> bool {
    let mut cooldowns = switch_enable_cooldowns();
    let now = Instant::now();
    let recently_tried = cooldowns
        .get(&group_id)
        .is_some_and(|last| now.duration_since(*last) < SWITCH_ENABLE_COOLDOWN);
    if recently_tried {
        return false;
    }
    cooldowns.insert(group_id, now);
    true
}

pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        if let Some(cmd) = match_command(&ctx, "我要头衔") {
            // 1. 确认是群消息
            let msg = match ctx.as_message() {
                Some(m) => m,
                None => return Ok(Some(ctx)),
            };

            let group_id = match msg.group_id() {
                Some(gid) => gid,
                None => return Ok(Some(ctx)),
            };

            let user_id = msg.user_id();

            // 2. 获取 Bot 自身的 ID (self_id)
            let self_id = if let EventType::Satori(ev) = &ctx.event {
                ev.get_i64("self_id")
                    .or_else(|| ev.get_u64("self_id").map(|v| v as i64))
                    .unwrap_or(0)
            } else {
                0
            };

            // 3. 检查 Bot 是否为群主
            // 只有群主才有权限设置群头衔
            let bot_info =
                match api::get_group_member_info(&ctx, writer.clone(), group_id, self_id, true)
                    .await
                {
                    Ok(info) => info,
                    Err(e) => {
                        error!(
                            target: "Plugin/GroupTitle",
                            "[Group({})] 获取 Bot 成员信息失败: {}",
                            group_id, e
                        );
                        send_msg(
                            &ctx,
                            writer,
                            Some(group_id),
                            Some(user_id),
                            "暂时无法查询机器人群权限。请稍后重试；详细原因已记录在日志中。",
                        )
                        .await?;
                        return Ok(None);
                    }
                };

            if bot_info.role != "owner" {
                // 如果不是群主，忽略指令（或者可以回复提示）
                warn!(
                    target: "Plugin/GroupTitle",
                    "[Group({})] Bot 不是群主，无法设置头衔",
                    group_id
                );
                send_msg(
                    &ctx,
                    writer,
                    Some(group_id),
                    Some(user_id),
                    "无法设置头衔：机器人需要群主权限。请由群主调整权限后重试。",
                )
                .await?;
                return Ok(None);
            }

            // 4. 拼接头衔内容
            let mut title = String::new();
            for seg in cmd.args {
                // 提取文本段内容
                if let Some(text) = seg.get("data").and_then(|d| d.get_str("text")) {
                    title.push_str(text);
                }
            }
            let title = title.trim();

            // 5. 设置头衔。
            // 写头衔本身（OIDB 0x8FC_2）和群管理里「展示成员群头衔」的显示开关是两回事：
            // 写头衔这次调用哪怕不报错，开关没开也照样不显示——所以不能只看这次调用
            // 返不返回错误。不管这次写头衔成不成功，都顺手把显示开关的状态确认一遍，
            // 没开就打开；开关原本关着又正好写头衔失败了，打开开关后再补一次。
            // 两边都试过仍然不行，就静默放弃，不打扰群聊。
            let mut result = api::set_group_special_title(
                &ctx,
                writer.clone(),
                group_id,
                user_id,
                title.to_string(),
                -1,
            )
            .await;

            match api::get_group_title_display(&ctx, writer.clone(), group_id).await {
                Ok(false) if !should_try_enable_switch(group_id) => {
                    warn!(
                        target: "Plugin/GroupTitle",
                        "[Group({})] 「展示成员群头衔」开关最近刚尝试打开过，冷却中，本次跳过",
                        group_id
                    );
                }
                Ok(false) => {
                    if let Err(e) =
                        api::set_group_title_display(&ctx, writer.clone(), group_id, true).await
                    {
                        error!(
                            target: "Plugin/GroupTitle",
                            "[Group({})] 打开「展示成员群头衔」开关失败: {}",
                            group_id, e
                        );
                    } else if result.is_err() {
                        result = api::set_group_special_title(
                            &ctx,
                            writer.clone(),
                            group_id,
                            user_id,
                            title.to_string(),
                            -1,
                        )
                        .await;
                    }
                }
                Ok(true) => {}
                Err(qe) => {
                    warn!(
                        target: "Plugin/GroupTitle",
                        "[Group({})] 查询「展示成员群头衔」开关状态失败: {}",
                        group_id, qe
                    );
                }
            }

            if let Err(e) = result {
                error!(
                    target: "Plugin/GroupTitle",
                    "[Group({})] 设置头衔失败: {}",
                    group_id, e
                );
                send_msg(
                    &ctx,
                    writer,
                    Some(group_id),
                    Some(user_id),
                    "头衔未设置成功。请检查群权限与头衔长度后重试；详细原因已记录在日志中。",
                )
                .await?;
                return Ok(None);
            }

            return Ok(None);
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
