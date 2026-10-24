//! 假头衔：把「群主 / 管理员」之类的群身份头衔戴到自己头上。
//!
//! QQ 的群身份头衔（群主、管理员、群等级、互动标签）由客户端向服务器「上报」一枚
//! titleId，服务器再把它广播给群成员。这个上报入口只认 groupId + titleId，不核
//! 对佩戴者是不是真的群主或管理员，于是普通成员也能戴上一会儿——这就是群友说的
//! 假头衔。它通常撑不了多久，服务器下一次同步就会收回去，所以插件提供自动续报：
//!
//! - 手动：`假头衔 群主` / `假头衔 管理员` / `假头衔 等级` / `假头衔 摘`
//! - 自动：`假头衔 自动 8` 每隔几分钟重报一次，默认在群主与管理员之间轮换
//! - 花样：`假头衔 波动 群主,管理员,等级 5`、`假头衔 随机 6`
//! - 自定义文字：`假头衔 文字 群主的爹`，走专属头衔或名片的兜底通道
//!
//! 只改机器人自己的头衔；群管理类的动作一律不碰。自动模式默认只有群主/管理员
//! 能开，免得群里任何人把机器人的身份改来改去。

use crate::adapters::satori::{LockedWriter, api, logins, send_msg};
use crate::command::{extract_text_arg, match_command};
use crate::event::Context;
use crate::plugins::{PluginConfig, PluginError, get_config_or_default};
use crate::storage::JsonState;
use futures_util::future::BoxFuture;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const LOG_TARGET: &str = "Plugin/FakeTitle";
const OWNER: &str = "群主";
const ADMIN: &str = "管理员";
const LEVEL: &str = "等级";

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    /// 手动指令的适用人群：`all`（所有人）或 `admin`（群主/管理员）。
    pub permission: String,
    /// 同一个群两次手动上报的最小间隔（秒）。
    pub cooldown_secs: u64,
    /// 自动模式的默认间隔（分钟）。
    pub default_interval_minutes: u64,
    /// 自动间隔的随机抖动（分钟）；0 表示卡着整点报。
    pub jitter_minutes: u64,
    /// 自动模式只有群主/管理员能开。
    pub auto_admin_only: bool,
    /// 自动续报时顺带说一句的概率（0~1）。
    pub announce_chance: f64,
    /// 允许 `假头衔 文字 …` 的自定义头衔。
    pub allow_custom_text: bool,
    /// 自定义头衔最多几个字。
    pub max_text_chars: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            permission: "all".into(),
            cooldown_secs: 30,
            default_interval_minutes: 8,
            jitter_minutes: 3,
            auto_admin_only: true,
            announce_chance: 0.3,
            allow_custom_text: true,
            max_text_chars: 6,
        }
    }
}

impl PluginConfig for Config {
    const NAME: &'static str = "fake_title";
}

/// 一个群的自动续报计划。
#[derive(Serialize, Deserialize, Clone, Default)]
struct Plan {
    /// `off` / `fixed` / `cycle` / `random`
    mode: String,
    /// 轮换用的头衔：`群主`、`管理员`、`等级`，或群身份目录里的任意名字。
    titles: Vec<String>,
    interval_secs: u64,
    /// 下一轮的时间戳（秒）；0 表示没排期。
    next_at: i64,
    index: usize,
    /// 谁开的自动模式，出问题时好 @ 他。
    started_by: String,
    last_at: i64,
    last_title: String,
    last_ok: bool,
    failures: u32,
}

static PLANS: JsonState<HashMap<String, Plan>> =
    JsonState::new(LOG_TARGET, "fake_title", "plans.json", "自动头衔计划");

/// 手动指令的冷却：按群算，不按人算。一个群的人轮流点也刷不出节拍。
static COOLDOWN: OnceLock<std::sync::Mutex<HashMap<String, Instant>>> = OnceLock::new();
static LOOP_STARTED: AtomicBool = AtomicBool::new(false);

fn cooldowns() -> std::sync::MutexGuard<'static, HashMap<String, Instant>> {
    COOLDOWN
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn cooldown_left(group_id: &str, secs: u64) -> Option<u64> {
    let mut map = cooldowns();
    let now = Instant::now();
    if let Some(last) = map.get(group_id) {
        let elapsed = now.duration_since(*last).as_secs();
        if elapsed < secs {
            return Some(secs - elapsed);
        }
    }
    map.insert(group_id.to_string(), now);
    None
}

pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let Some(cmd) = match_command(&ctx, "假头衔") else {
            return Ok(Some(ctx));
        };
        let Some(msg) = ctx.as_message() else {
            return Ok(Some(ctx));
        };
        let Some(group_id) = msg.group_id().map(str::to_owned) else {
            return Ok(Some(ctx));
        };
        let user_id = msg.user_id().to_owned();
        let config: Config = get_config_or_default(&ctx);
        if !config.enabled {
            return Ok(Some(ctx));
        }

        let text = extract_text_arg(&cmd.args);
        let mut words = text.split_whitespace();
        let head = words.next().unwrap_or("").trim();
        let rest: Vec<String> = words.map(str::to_owned).collect();

        // 权限：手动可以是所有人；改自动计划的门槛单独算。
        // 只有确实要看角色时才问一次群成员信息，普通的一次性换头衔不多跑一趟。
        let known = matches!(
            head,
            "" | "开" | "一次" | "群主" | "主人" | "owner" | "管理员" | "管理" | "admin"
                | "等级" | "level" | "摘" | "取消" | "off" | "下掉" | "列表" | "有哪些" | "清单"
                | "状态" | "计划" | "停" | "关闭" | "关" | "自动" | "续报" | "波动" | "随机"
        );
        let needs_admin = config.permission == "admin"
            || (matches!(head, "自动" | "波动" | "随机" | "停") && config.auto_admin_only)
            || matches!(head, "文字" | "自定义")
            || !known;
        let admin = if needs_admin {
            is_admin(&ctx, writer.clone(), &group_id, &user_id)
                .await
                .unwrap_or(false)
        } else {
            false
        };
        if !admin && config.permission == "admin" {
            reply(&ctx, writer, &group_id, "这个群的假头衔只对群主和管理员开放。").await;
            return Ok(None);
        }
        let wants_auto = matches!(head, "自动" | "波动" | "随机" | "停");
        if wants_auto && config.auto_admin_only && !admin {
            reply(&ctx, writer, &group_id, "自动续报会一直改机器人的头衔，只让群主或管理员开。").await;
            return Ok(None);
        }

        // 一次性的换头衔共用一个按群的冷却；查询、停计划和自动设置不占。
        let one_shot = !matches!(head, "列表" | "有哪些" | "清单" | "状态" | "计划" | "停" | "关闭" | "关" | "自动" | "续报" | "波动" | "随机");
        if one_shot && let Some(left) = cooldown_left(&group_id, config.cooldown_secs) {
            reply(&ctx, writer, &group_id, format!("刚报过，{left} 秒后再来。")).await;
            return Ok(None);
        }

        match head {
            "" | "开" | "一次" => {
                wear(&ctx, writer, &group_id, OWNER, "").await;
            }
            "群主" | "主人" | "owner" => {
                wear(&ctx, writer, &group_id, OWNER, "").await;
            }
            "管理员" | "管理" | "admin" => {
                wear(&ctx, writer, &group_id, ADMIN, "").await;
            }
            "等级" | "level" => {
                wear(&ctx, writer, &group_id, LEVEL, "").await;
            }
            "摘" | "取消" | "off" | "下掉" => {
                wear(&ctx, writer, &group_id, "off", "").await;
            }
            "文字" | "自定义" => {
                if !config.allow_custom_text {
                    reply(&ctx, writer, &group_id, "这个群没开自定义头衔。").await;
                    return Ok(None);
                }
                let custom = rest.join(" ");
                let custom: String = custom.trim().chars().take(config.max_text_chars).collect();
                if custom.is_empty() {
                    reply(&ctx, writer, &group_id, "用法：假头衔 文字 <头衔>，最多六字。").await;
                    return Ok(None);
                }
                custom_wear(&ctx, writer, &group_id, &custom, admin).await;
            }
            "列表" | "有哪些" | "清单" => {
                list(&ctx, writer, &group_id).await;
            }
            "状态" | "计划" => {
                status(&ctx, writer, &group_id).await;
            }
            "停" | "关闭" | "关" => {
                let had = PLANS
                    .with(|plans| plans.remove(&group_id))
                    .await
                    .is_some();
                // 顺手把手上的假头衔摘掉；摘不掉也不影响停计划。
                let _ = api::title_wear(
                    &ctx,
                    writer.clone(),
                    &group_id,
                    serde_json::json!({"guild_id": group_id, "role": "off", "verify": false}),
                )
                .await;
                reply(
                    &ctx,
                    writer,
                    &group_id,
                    if had { "自动续报停了，头衔也摘了。" } else { "本来就没在自动报，头衔已摘。" },
                )
                .await;
            }
            "自动" | "续报" | "波动" | "随机" => {
                let minutes = rest
                    .iter()
                    .find_map(|word| word.trim_end_matches('分').parse::<u64>().ok())
                    .unwrap_or(config.default_interval_minutes)
                    .clamp(1, 180);
                let mut titles: Vec<String> = rest
                    .iter()
                    .filter(|word| !word.trim_end_matches('分').chars().all(|c| c.is_ascii_digit()))
                    .flat_map(|word| word.split([',', '，', '、', '/']).map(str::to_owned))
                    .map(|word| word.trim().to_owned())
                    .filter(|word| !word.is_empty())
                    .collect();
                if titles.is_empty() {
                    titles = match head {
                        "随机" => vec![OWNER.into(), ADMIN.into(), LEVEL.into()],
                        "波动" => vec![OWNER.into(), ADMIN.into()],
                        _ => vec![OWNER.into()],
                    };
                }
                let mode = match head {
                    "随机" => "random",
                    "自动" | "续报" => "fixed",
                    _ => "cycle",
                };
                let interval = Duration::from_secs(minutes * 60);
                let plan = Plan {
                    mode: mode.into(),
                    titles: titles.clone(),
                    interval_secs: interval.as_secs(),
                    next_at: chrono::Utc::now().timestamp() + interval.as_secs() as i64,
                    index: 0,
                    started_by: user_id.clone(),
                    ..Default::default()
                };
                PLANS.with(|plans| {
                    plans.insert(group_id.clone(), plan);
                })
                .await;
                let every = if config.jitter_minutes > 0 {
                    format!("约 {minutes} 分钟（±{} 分钟）", config.jitter_minutes)
                } else {
                    format!("{minutes} 分钟")
                };
                reply(
                    &ctx,
                    writer,
                    &group_id,
                    format!(
                        "自动续报开了：每{every}在「{}」之间切换。发「假头衔 停」随时收手。",
                        titles.join(" / ")
                    ),
                )
                .await;
            }
            _ => {
                // 没认出来的词当成群身份目录里的名字，或自定义文字。
                let name = head.to_owned();
                if config.allow_custom_text && name.chars().count() <= config.max_text_chars {
                    custom_wear(&ctx, writer, &group_id, &name, admin).await;
                } else if config.allow_custom_text {
                    let name: String = name.chars().take(config.max_text_chars).collect();
                    custom_wear(&ctx, writer, &group_id, &name, admin).await;
                } else {
                    reply(&ctx, writer, &group_id, "没认出来。试试：假头衔 群主 / 管理员 / 等级 / 列表 / 自动 8 / 停").await;
                }
            }
        }
        Ok(None)
    })
}

pub fn init(ctx: Context) -> BoxFuture<'static, Result<(), PluginError>> {
    Box::pin(async move {
        PLANS.preload().await;
        if !LOOP_STARTED.swap(true, Ordering::SeqCst) {
            tokio::spawn(auto_loop(ctx));
        }
        Ok(())
    })
}

async fn is_admin(
    ctx: &Context,
    writer: LockedWriter,
    group_id: &str,
    user_id: &str,
) -> Result<bool, PluginError> {
    let info = api::get_group_member_info(ctx, writer, group_id, user_id, true).await?;
    Ok(api::is_staff(&info.role))
}

async fn reply(ctx: &Context, writer: LockedWriter, group_id: &str, text: impl Into<String>) {
    let _ = send_msg(ctx, writer, Some(group_id), None, text.into()).await;
}

/// 戴一次头衔，并把结果说成人话。
async fn wear(ctx: &Context, writer: LockedWriter, group_id: &str, role: &str, custom: &str) {
    let value = api::title_wear(
        ctx,
        writer.clone(),
        group_id,
        serde_json::json!({
            "guild_id": group_id,
            "role": role,
            "title": custom,
            "strategy": "auto",
            "verify": true,
        }),
    )
    .await;

    match value {
        Ok(value) => {
            let label = value
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(role);
            let worn = value
                .get("worn")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let accepted = value
                .get("accepted")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let message = if worn {
                format!("已戴上「{label}」。趁服务器还没反应过来，多看两眼。")
            } else if accepted {
                format!("「{label}」已上报。服务器收了，但没立刻认——过一会儿再看，可能只撑一小会儿。")
            } else {
                let detail = attempts_text(&value);
                format!("没戴成「{label}」。{detail}")
            };
            reply(ctx, writer, group_id, message).await;
        }
        Err(error) => {
            reply(ctx, writer, group_id, format!("没戴成：{error}")).await;
        }
    }
}

/// 自定义文字：机器人是群主/管理员时走专属头衔（稳），否则走名片兜底通道。
async fn custom_wear(
    ctx: &Context,
    writer: LockedWriter,
    group_id: &str,
    text: &str,
    admin: bool,
) {
    if admin {
        match api::set_group_special_title(ctx, writer.clone(), group_id, &ctx.bot.self_id(), text.to_string(), -1).await {
            Ok(()) => {
                // 显示开关没开的话，写进去也不显示；顺手确认一遍。
                if let Ok(false) = api::get_group_title_display(ctx, writer.clone(), group_id).await {
                    let _ = api::set_group_title_display(ctx, writer.clone(), group_id, true).await;
                }
                reply(
                    ctx,
                    writer,
                    group_id,
                    format!("专属头衔已写成「{text}」。这个是真的，不用假装。"),
                )
                .await;
                return;
            }
            Err(error) => {
                debug!(target: LOG_TARGET, "专属头衔通道失败，改用名片兜底: {error}");
            }
        }
    }
    let value = api::title_wear(
        ctx,
        writer.clone(),
        group_id,
        serde_json::json!({
            "guild_id": group_id,
            "title": text,
            "strategy": "card",
            "verify": false,
        }),
    )
    .await;
    match value {
        Ok(value) => {
            let accepted = value
                .get("accepted")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let message = if accepted {
                format!("头衔「{text}」已上报。名片通道收下了，能不能显示看服务器心情。")
            } else {
                format!("「{text}」没写进去。{}", attempts_text(&value))
            };
            reply(ctx, writer, group_id, message).await;
        }
        Err(error) => {
            reply(ctx, writer, group_id, format!("没写成：{error}")).await;
        }
    }
}

fn attempts_text(value: &serde_json::Value) -> String {
    let Some(attempts) = value.get("attempts").and_then(|v| v.as_array()) else {
        return String::new();
    };
    let parts: Vec<String> = attempts
        .iter()
        .filter_map(|attempt| {
            let strategy = attempt.get("strategy")?.as_str()?;
            let ok = attempt.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            let msg = attempt.get("msg").and_then(|v| v.as_str()).unwrap_or("");
            let mark = if ok { " ✓".to_string() } else { format!(" ✗ {msg}") };
            Some(format!("{strategy}{mark}"))
        })
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("（{}）", parts.join("；"))
    }
}

async fn list(ctx: &Context, writer: LockedWriter, group_id: &str) {
    match api::title_list(ctx, writer.clone(), group_id).await {
        Ok(value) => {
            let mut lines = vec!["本群身份头衔：".to_string()];
            if let Some(titles) = value.get("titles").and_then(|v| v.as_array()) {
                for title in titles {
                    let name = title.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let id = title.get("title_id").and_then(|v| v.as_i64()).unwrap_or(0);
                    let worn = title.get("worn").and_then(|v| v.as_bool()).unwrap_or(false);
                    let hint = match id {
                        300 => "（群主）",
                        301 => "（管理员）",
                        _ => "",
                    };
                    lines.push(format!(
                        "· {name}{hint} · {id}{}",
                        if worn { " ← 当前戴着" } else { "" }
                    ));
                }
            }
            lines.push("发「假头衔 群主 / 管理员 / 等级」直接换。".to_string());
            reply(ctx, writer, group_id, lines.join("\n")).await;
        }
        Err(error) => reply(ctx, writer, group_id, format!("读不到头衔目录：{error}")).await,
    }
}

async fn status(ctx: &Context, writer: LockedWriter, group_id: &str) {
    let plan = PLANS.read(|plans| plans.get(group_id).cloned()).await;
    let Some(plan) = plan else {
        reply(ctx, writer, group_id, "现在没有自动续报。发「假头衔 自动 8」开一个。").await;
        return;
    };
    let now = chrono::Utc::now().timestamp();
    let next = if plan.next_at == 0 {
        "已停（连续失败）".to_string()
    } else if plan.next_at > now {
        format!("{} 分钟后", (plan.next_at - now + 59) / 60)
    } else {
        "马上".to_string()
    };
    let mode = match plan.mode.as_str() {
        "random" => "随机",
        "cycle" => "轮换",
        "fixed" => "固定",
        _ => "已停",
    };
    let last = if plan.last_title.is_empty() {
        "还没报过".to_string()
    } else {
        format!("{} · {}", plan.last_title, if plan.last_ok { "成功" } else { "失败" })
    };
    reply(
        ctx,
        writer,
        group_id,
        format!(
            "自动模式：{mode}\n候选：{}\n间隔：{} 分钟 · 下一轮：{next}\n上次：{last}",
            plan.titles.join(" / "),
            plan.interval_secs / 60,
        ),
    )
    .await;
}

async fn auto_loop(ctx: Context) {
    let mut tick = tokio::time::interval(Duration::from_secs(15));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let config: Config = get_config_or_default(&ctx);
        if !config.enabled {
            continue;
        }
        let now = chrono::Utc::now().timestamp();
        let due: Vec<(String, Plan)> = PLANS
            .read(|plans| {
                plans
                    .iter()
                    .filter(|(_, plan)| plan.next_at > 0 && plan.next_at <= now)
                    .map(|(group, plan)| (group.clone(), plan.clone()))
                    .collect()
            })
            .await;
        if due.is_empty() {
            continue;
        }
        let Some((_bot, writer)) = logins()
            .into_iter()
            .find(|(bot, _)| bot.self_id() == ctx.bot.self_id())
            .or_else(|| logins().into_iter().next())
        else {
            continue;
        };
        for (group_id, plan) in due {
            if !ctx.group_allowed(&group_id) {
                PLANS.with(|plans| {
                    if let Some(plan) = plans.get_mut(&group_id) {
                        plan.next_at = now + 600;
                    }
                })
                .await;
                continue;
            }
            let title = pick_title(&plan);
            let value = api::title_wear(
                &ctx,
                writer.clone(),
                &group_id,
                serde_json::json!({
                    "guild_id": group_id,
                    "role": title,
                    "strategy": "auto",
                    "verify": true,
                }),
            )
            .await;
            let (ok, label) = match &value {
                Ok(value) => {
                    let worn = value.get("worn").and_then(|v| v.as_bool()).unwrap_or(false)
                        || value.get("accepted").and_then(|v| v.as_bool()).unwrap_or(false);
                    let label = value
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&title)
                        .to_owned();
                    (worn, label)
                }
                Err(_) => (false, title.clone()),
            };
            let jitter = if config.jitter_minutes > 0 {
                rand::rng().random_range(0..=config.jitter_minutes * 60) as i64
            } else {
                0
            };
            let failures = if ok { 0 } else { plan.failures.saturating_add(1) };
            let next_at = if failures >= 5 {
                warn!(
                    target: LOG_TARGET,
                    "[Group({group_id})] 连续 {failures} 次戴不上，自动续报先停了"
                );
                0
            } else {
                now + plan.interval_secs as i64 + jitter
            };
            let mode = if failures >= 5 { String::new() } else { plan.mode.clone() };
            PLANS.with(|plans| {
                if let Some(plan) = plans.get_mut(&group_id) {
                    plan.last_at = now;
                    plan.last_title = label.clone();
                    plan.last_ok = ok;
                    plan.failures = failures;
                    plan.next_at = next_at;
                    plan.mode = mode;
                    plan.index = plan.index.wrapping_add(1);
                }
            })
            .await;
            if ok && rand::random::<f64>() < config.announce_chance.clamp(0.0, 1.0) {
                let line = match label.as_str() {
                    OWNER => "本群头衔续费成功：群主。真的假的自己判断。",
                    ADMIN => "本群头衔续费成功：管理员。管不了人，但看着像。",
                    _ => "群头衔已换，趁热看。",
                };
                let _ = send_msg(&ctx, writer.clone(), Some(&group_id), None, line).await;
            }
        }
    }
}

fn pick_title(plan: &Plan) -> String {
    if plan.titles.is_empty() {
        return OWNER.to_string();
    }
    match plan.mode.as_str() {
        "random" => plan.titles[rand::rng().random_range(0..plan.titles.len())].clone(),
        "fixed" => plan.titles[0].clone(),
        _ => plan.titles[plan.index % plan.titles.len()].clone(),
    }
}
