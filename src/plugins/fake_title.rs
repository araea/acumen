//! 假头衔：把「群主 / 管理员」之类的字写在群头衔上，或戴上手里已有的群身份头衔。
//!
//! QQ 里「头衔」有三层，能做什么完全由账号在本群的角色决定：
//!
//! - **身份头衔**：群主 `300`、管理员 `301`、群等级 `315`、群主授予的专属头衔 `302`。
//!   只有目录里真的出现的那几枚才戴得上；普通成员硬报 `300/301`，服务器回
//!   `20000 medal not available`。
//! - **专属头衔**：群主给成员写的六个字以内的文字，走 `0x8FC_2`。群主可以给自己
//!   或别人写，写完后身份目录里会多出一枚 `302`，任何成员都能戴上它。普通成员自己
//!   写不了（`oidb=1013`），管理员也写不了。
//! - **群等级头衔**：群主 / 管理员配置的等级名，普通成员只能戴已经获得的那一枚。
//!
//! 所以「假头衔」真正能成立的两条路是：
//!
//! 1. 机器人是群主：用 `假头衔 文字 管理员`（或 `假头衔 管理员`）写一枚专属头衔，
//!    内容可以是「群主」「管理员」；这是持久的，不需要自动续报。
//! 2. 群主已经给机器人写了专属头衔：`假头衔 列表` 里能看到 `302`，戴上它即可。
//!
//! 机器人只是普通成员时，没有任何接口能凭空造出群主 / 管理员身份。插件会直接说清
//! 原因，而不是把服务器的拒绝误报成「模块没更新」。
//!
//! - 手动：`假头衔 群主` / `管理员` / `等级` / `摘`
//! - 文字：`假头衔 文字 群主的爹`（需要机器人是群主；最多六字）
//! - 自动：`假头衔 自动 8` 每隔几分钟重报一次；`波动` / `随机` 在候选里挑
//! - 查询：`假头衔 列表` / `状态`
//!
//! 自动模式默认仍然只让群主 / 管理员开。开之前会先真戴一次，戴不上就不排期，
//! 避免每轮都拿一次注定失败的请求去撞实现端的出站熔断。

use crate::adapters::satori::{LockedWriter, api, logins, send_msg};
use crate::command::{extract_text_arg, match_command};
use crate::event::Context;
use crate::plugins::{PluginConfig, PluginError, get_config_or_default};
use crate::storage::JsonState;
use futures_util::future::BoxFuture;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const LOG_TARGET: &str = "Plugin/FakeTitle";
const OWNER: &str = "群主";
const ADMIN: &str = "管理员";
const LEVEL: &str = "等级";
/// 这个插件只跟知弦 0.45.0 起的头衔接口对齐；低于它的实现端没有 `role` /
/// `reason` 这些字段，也没法区分「普通成员戴不上」和「接口没加载」。
const MIN_MODULE: &str = "0.45.0";

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    /// 手动指令的适用人群：`all`（所有人）或 `admin`（群主/管理员）。
    pub permission: String,
    /// 同一个群两次手动写入的最小间隔（秒）。查询与停计划不占。
    pub cooldown_secs: u64,
    /// 自动模式的默认间隔（分钟）。
    pub default_interval_minutes: u64,
    /// 自动间隔的随机抖动（分钟）；0 表示卡着整点报。
    pub jitter_minutes: u64,
    /// 自动模式只有群主/管理员能开。
    pub auto_admin_only: bool,
    /// 自动续报成功时顺带说一句的概率（0~1）。
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
            cooldown_secs: 15,
            default_interval_minutes: 8,
            jitter_minutes: 3,
            auto_admin_only: true,
            announce_chance: 0.25,
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
    /// `fixed` / `cycle` / `random`
    mode: String,
    /// 轮换用的候选：`群主`、`管理员`、`等级`，或目录里的任意名字。
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
    last_error: String,
    /// 上一次成功时账号在本群的角色，展示用。
    role: String,
}

static PLANS: JsonState<HashMap<String, Plan>> =
    JsonState::new(LOG_TARGET, "fake_title", "plans.json", "自动头衔计划");

/// 手动写入的冷却：按群算，不按人算。
static COOLDOWN: OnceLock<std::sync::Mutex<HashMap<String, Instant>>> = OnceLock::new();
static LOOP_STARTED: AtomicBool = AtomicBool::new(false);

fn cooldowns() -> std::sync::MutexGuard<'static, HashMap<String, Instant>> {
    COOLDOWN
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn cooldown_left(group_id: &str, secs: u64) -> Option<u64> {
    if secs == 0 {
        return None;
    }
    let map = cooldowns();
    let now = Instant::now();
    map.get(group_id).and_then(|last| {
        let elapsed = now.duration_since(*last).as_secs();
        (elapsed < secs).then(|| secs - elapsed)
    })
}

fn mark_cooldown(group_id: &str) {
    cooldowns().insert(group_id.to_string(), Instant::now());
}

/// 一次佩戴的结果。`ok` 表示实现端处理完了，`accepted` 表示 QQ 收下了，
/// `worn` 表示回读身份目录时确实戴着——三者不能混用。
#[derive(Default, Clone)]
struct Outcome {
    accepted: bool,
    worn: bool,
    label: String,
    role: String,
    source: String,
    reason: String,
    hint: String,
    error: Option<String>,
}

impl Outcome {
    fn success(&self) -> bool {
        self.worn || self.accepted
    }

    fn retryable(&self) -> bool {
        !matches!(
            self.reason.as_str(),
            "owner_required" | "not_available" | "title_unavailable" | "not_reflected"
        )
    }
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
        let head = words.next().unwrap_or("").trim().to_owned();
        let rest: Vec<String> = words.map(str::to_owned).collect();
        let lower = head.to_ascii_lowercase();
        let is = |names: &[&str]| names.iter().any(|name| lower == *name);

        let is_query = is(&["", "状态", "计划", "列表", "有哪些", "清单", "帮助", "help", "用法"]);
        let is_auto = is(&["自动", "续报", "波动", "随机"]);
        let is_stop = is(&["停", "关闭", "关", "stop"]);
        let is_off = is(&["摘", "取消", "off", "none", "clear", "下掉"]);
        let is_text = is(&["文字", "自定义", "text"]);
        let is_role = !is_query
            && !is_auto
            && !is_stop
            && !is_off
            && !is_text
            && is(&[
                "群主", "主人", "owner", "管理员", "管理", "admin", "等级", "level",
            ]);

        // 只有真的要看角色时才问一次；查询与一次性的普通换头衔不多跑一趟。
        let needs_admin = config.permission == "admin"
            || (is_auto && config.auto_admin_only)
            || is_stop && config.auto_admin_only;
        let role = if needs_admin {
            api::own_role(&ctx, writer.clone(), &group_id).await
        } else {
            String::new()
        };
        if needs_admin && !api::is_staff(&role) {
            let why = if is_auto || is_stop {
                "自动续报会一直改机器人的头衔，只让群主或管理员开。"
            } else {
                "这个群的假头衔只对群主和管理员开放。"
            };
            reply(&ctx, writer, &group_id, why).await;
            return Ok(None);
        }

        // 查询、停计划不占冷却；写入才占。冷却在写完之后才登记，失败也能马上再试一次
        // 真正可用的候选。
        if !is_query && !is_stop
            && let Some(left) = cooldown_left(&group_id, config.cooldown_secs)
        {
            reply(&ctx, writer, &group_id, format!("刚动过头衔，{left} 秒后再来。")).await;
            return Ok(None);
        }

        let result = match head.as_str() {
            "" | "状态" | "计划" => {
                status(&ctx, writer.clone(), &group_id).await;
                return Ok(None);
            }
            "列表" | "有哪些" | "清单" => {
                list(&ctx, writer.clone(), &group_id).await;
                return Ok(None);
            }
            "帮助" | "help" | "用法" => {
                reply(&ctx, writer, &group_id, help_text()).await;
                return Ok(None);
            }
            "停" | "关闭" | "关" | "stop" => {
                stop(&ctx, writer.clone(), &group_id).await;
                return Ok(None);
            }
            "摘" | "取消" | "off" | "none" | "clear" | "下掉" => {
                wear_target(&ctx, writer.clone(), &group_id, Target::role("off")).await
            }
            "文字" | "自定义" | "text" => {
                if !config.allow_custom_text {
                    reply(&ctx, writer, &group_id, "这个群没开自定义头衔。").await;
                    return Ok(None);
                }
                let custom: String = rest.join(" ").trim().chars().take(config.max_text_chars).collect();
                if custom.is_empty() {
                    reply(
                        &ctx,
                        writer,
                        &group_id,
                        format!(
                            "用法：假头衔 文字 <头衔>，最多{}字。需要机器人是本群群主。",
                            config.max_text_chars
                        ),
                    )
                    .await;
                    return Ok(None);
                }
                wear_target(&ctx, writer.clone(), &group_id, Target::text(&custom)).await
            }
            "群主" | "主人" | "owner" => {
                wear_target(&ctx, writer.clone(), &group_id, Target::role("owner")).await
            }
            "管理员" | "管理" | "admin" => {
                wear_target(&ctx, writer.clone(), &group_id, Target::role("admin")).await
            }
            "等级" | "level" => {
                wear_target(&ctx, writer.clone(), &group_id, Target::role("level")).await
            }
            "自动" | "续报" | "波动" | "随机" => {
                start_auto(
                    &ctx,
                    writer.clone(),
                    &group_id,
                    &user_id,
                    &head,
                    &rest,
                    &config,
                )
                .await;
                return Ok(None);
            }
            _ => {
                if !is_role && !config.allow_custom_text {
                    reply(
                        &ctx,
                        writer,
                        &group_id,
                        "没认出来。试试：假头衔 群主 / 管理员 / 等级 / 列表 / 文字 <文字> / 自动 8 / 停",
                    )
                    .await;
                    return Ok(None);
                }
                // 先当成目录里的名字；目录里没有、而机器人是群主时，再当成自定义文字。
                wear_word(
                    &ctx,
                    writer.clone(),
                    &group_id,
                    &head,
                    config.allow_custom_text,
                    config.max_text_chars,
                )
                .await
            }
        };

        match &result {
            Ok(outcome) => {
                reply(&ctx, writer, &group_id, describe(outcome)).await;
                mark_cooldown(&group_id);
            }
            Err(error) => {
                reply(&ctx, writer, &group_id, error_text(error)).await;
                mark_cooldown(&group_id);
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

async fn reply(ctx: &Context, writer: LockedWriter, group_id: &str, text: impl Into<String>) {
    let _ = send_msg(ctx, writer, Some(group_id), None, text.into()).await;
}

/// 一次佩戴要传给实现端的目标。
#[derive(Default, Clone)]
struct Target {
    role: String,
    name: String,
    text: String,
}

impl Target {
    fn role(role: &str) -> Self {
        Self {
            role: role.into(),
            ..Default::default()
        }
    }

    fn name(name: &str) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    fn text(text: &str) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }

    fn params(&self, group_id: &str) -> Value {
        let mut params = json!({
            "guild_id": group_id,
            "strategy": "auto",
            "verify": true,
            "settle_ms": 350,
        });
        if !self.role.is_empty() {
            params["role"] = json!(self.role);
        }
        if !self.name.is_empty() {
            params["name"] = json!(self.name);
        }
        if !self.text.is_empty() {
            params["title"] = json!(self.text);
        }
        params
    }
}

async fn wear_target(
    ctx: &Context,
    writer: LockedWriter,
    group_id: &str,
    target: Target,
) -> Result<Outcome, String> {
    let value = api::title_wear(ctx, writer, group_id, target.params(group_id))
        .await
        .map_err(|error| error.to_string())?;
    Ok(outcome_of(&value))
}

/// 先按目录里的名字戴；名字不存在、机器人又是群主时，退到自定义文字。
async fn wear_word(
    ctx: &Context,
    writer: LockedWriter,
    group_id: &str,
    word: &str,
    allow_custom: bool,
    max_chars: usize,
) -> Result<Outcome, String> {
    let first = wear_target(ctx, writer.clone(), group_id, Target::name(word)).await?;
    if first.success() || first.reason != "not_available" || !allow_custom || first.role != "owner" {
        return Ok(first);
    }
    let text: String = word.chars().take(max_chars).collect();
    wear_target(ctx, writer, group_id, Target::text(&text)).await
}

fn outcome_of(value: &Value) -> Outcome {
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let flag = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    Outcome {
        accepted: flag("accepted"),
        worn: flag("worn"),
        label: {
            let worn = text("worn_name");
            if worn.is_empty() { text("title") } else { worn }
        },
        role: text("role"),
        source: text("source"),
        reason: text("reason"),
        hint: text("hint"),
        error: None,
    }
}

/// 把结果说成人话。先区分「服务器收下了」「回读确认戴上了」「根本没接受」。
fn describe(outcome: &Outcome) -> String {
    if outcome.worn {
        return match outcome.source.as_str() {
            "special" => format!(
                "已把群头衔写成「{}」。这是群主写的专属头衔，能一直显示，不用自动续报。",
                outcome.label
            ),
            "default" => format!("已经换回默认头衔「{}」。", outcome.label),
            _ => format!("已戴上「{}」。", outcome.label),
        };
    }
    if outcome.accepted {
        return format!(
            "「{}」已上报。服务器收了，但回读身份目录还没看到；可能只显示一会儿，也可能要等客户端刷新。",
            outcome.label
        );
    }
    if !outcome.hint.is_empty() {
        return outcome.hint.clone();
    }
    if !outcome.reason.is_empty() {
        return reason_text(&outcome.reason, &outcome.label);
    }
    "这次没有戴上。发「假头衔 状态」看看本群有哪些头衔。".into()
}

fn reason_text(reason: &str, label: &str) -> String {
    match reason {
        "owner_required" => format!(
            "本群没有接受「{label}」：QQ 只让群主写专属头衔。普通成员和管理员都不能自己造群主/管理员身份；让群主给你写「{label}」，写好后用「假头衔 列表」戴上它。"
        ),
        "not_available" => format!(
            "本群身份目录里没有「{label}」。普通成员只能戴已经获得的身份头衔（群等级、群主授予的专属头衔）。"
        ),
        "not_reflected" => format!(
            "「{label}」已经上报，但回读目录还没看到；过一会儿再发「假头衔 状态」确认。"
        ),
        "timeout" => "QQ 内核没有及时回话，稍后再试。".into(),
        "kernel_refused" => format!("QQ 拒绝了「{label}」这次上报。"),
        _ => format!("这次没有戴上「{label}」。"),
    }
}

fn is_unsupported(error: &str) -> bool {
    error.contains("unknown_action")
        || error.contains("removed_action")
        || error.contains("no such action")
        || error.contains("API not found")
        || error.contains("实现端没有声明")
}

/// 只把「实现端确实没有这个动作」翻译成更新提示。以前只要错误文案里带上
/// `title_wear` 就提示更新，结果服务器回一句 500 也会被说成模块没更新。
fn error_text(error: &str) -> String {
    if is_unsupported(error) {
        return format!(
            "知弦没有提供头衔接口。把知弦模块更新到 {MIN_MODULE} 或更高，重启手机后再试（只重启 QQ 不算）。"
        );
    }
    let cleaned = error
        .split_once("): ")
        .map(|(_, rest)| rest)
        .unwrap_or(error)
        .trim();
    if cleaned.is_empty() {
        format!("这次没成功：{error}")
    } else {
        format!("这次没成功：{cleaned}")
    }
}

/// 按角色名 / 目录名把实现端返回的 title 列表说成几行。
fn title_lines(value: &Value) -> (Vec<String>, String, String, String, bool) {
    let role = value
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("member")
        .to_string();
    let worn = value
        .get("worn")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let special = value
        .get("special_title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let can_special = value
        .get("can_set_special")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut lines = Vec::new();
    if let Some(titles) = value.get("titles").and_then(Value::as_array) {
        for title in titles {
            let name = title.get("name").and_then(Value::as_str).unwrap_or("?");
            let id = title.get("title_id").and_then(Value::as_i64).unwrap_or(0);
            let kind = title.get("kind").and_then(Value::as_str).unwrap_or("");
            let worn = title.get("worn").and_then(Value::as_bool).unwrap_or(false);
            let kind = match kind {
                "role" => "身份",
                "custom" => "专属",
                "level" => "等级",
                _ => "其它",
            };
            lines.push(format!(
                "· {name} · {kind} · {id}{}",
                if worn { " ← 当前戴着" } else { "" }
            ));
        }
    }
    (lines, role, worn, special, can_special)
}

async fn list(ctx: &Context, writer: LockedWriter, group_id: &str) {
    match api::title_list(ctx, writer.clone(), group_id).await {
        Ok(value) => {
            let (titles, role, worn, special, can_special) = title_lines(&value);
            let mut lines = vec![format!("本群身份：{}", role_label(&role))];
            lines.push(format!(
                "当前头衔：{}",
                if worn.is_empty() { "（没戴）" } else { &worn }
            ));
            if !special.is_empty() {
                lines.push(format!("群主写的专属头衔：「{special}」"));
            }
            lines.push("可戴的头衔：".into());
            if titles.is_empty() {
                lines.push("· 目录里没有可戴的头衔".into());
            } else {
                lines.extend(titles);
            }
            if can_special {
                lines.push("你是群主，可以用「假头衔 文字 <文字>」自己写一枚（最多六字）。".into());
            } else if role == "admin" {
                lines.push("你是管理员：能戴「管理员」，但不能写专属头衔；要假「群主」得让群主写。".into());
            } else {
                lines.push("你是普通成员：只能戴上面已有的头衔，不能自己造群主/管理员。".into());
            }
            reply(ctx, writer, group_id, lines.join("\n")).await;
        }
        Err(error) => {
            reply(ctx, writer, group_id, format!("读不到头衔目录：{}", error_text(&error.to_string()))).await
        }
    }
}

async fn status(ctx: &Context, writer: LockedWriter, group_id: &str) {
    let plan = PLANS.read(|plans| plans.get(group_id).cloned()).await;
    match api::title_list(ctx, writer.clone(), group_id).await {
        Ok(value) => {
            let (titles, role, worn, special, can_special) = title_lines(&value);
            let mut lines = vec![format!("本群身份：{}", role_label(&role))];
            lines.push(format!(
                "当前头衔：{}",
                if worn.is_empty() { "（没戴）" } else { &worn }
            ));
            if !special.is_empty() {
                lines.push(format!("群主写的专属头衔：「{special}」"));
            }
            if !titles.is_empty() {
                lines.push(format!("目录：{}", titles.join("；")));
            }
            if can_special {
                lines.push("可以写自定义头衔（群主专属）。".into());
            }
            if let Some(plan) = plan {
                let now = chrono::Utc::now().timestamp();
                let next = if plan.next_at == 0 {
                    "已停".to_string()
                } else if plan.next_at > now {
                    format!("{} 分钟后", (plan.next_at - now + 59) / 60)
                } else {
                    "马上".to_string()
                };
                lines.push(format!(
                    "自动续报：{} · 每 {} 分钟 · 候选 {} · 下一轮 {next}",
                    if plan.last_ok { "上次成功" } else { "上次失败" },
                    plan.interval_secs / 60,
                    plan.titles.join(" / "),
                ));
                if !plan.last_error.is_empty() {
                    lines.push(format!("上次原因：{}", plan.last_error));
                }
            } else {
                lines.push("现在没有自动续报。发「假头衔 自动 8」开一个。".into());
            }
            reply(ctx, writer, group_id, lines.join("\n")).await;
        }
        Err(error) => {
            let plan_note = plan
                .map(|plan| format!("\n自动续报仍在，下一轮 {} 分钟后。", (plan.next_at - chrono::Utc::now().timestamp()).max(0) / 60))
                .unwrap_or_default();
            reply(
                ctx,
                writer,
                group_id,
                format!("读不到头衔目录：{}{plan_note}", error_text(&error.to_string())),
            )
            .await
        }
    }
}

fn role_label(role: &str) -> &str {
    match role {
        "owner" => "群主",
        "admin" => "管理员",
        "member" => "普通成员",
        "" => "未知",
        other => other,
    }
}

fn help_text() -> String {
    [
        "假头衔：",
        "· 假头衔 群主 / 管理员 / 等级 · 戴一枚身份头衔",
        "· 假头衔 文字 <文字> · 群主写专属头衔（最多六字）",
        "· 假头衔 列表 / 状态 · 看本群有哪些头衔、当前戴着什么",
        "· 假头衔 自动 [分钟] · 每隔几分钟重报一次",
        "· 假头衔 波动 [头衔,头衔] [分钟] · 在候选里轮换",
        "· 假头衔 随机 [分钟] · 每轮随机挑一枚",
        "· 假头衔 摘 · 换回默认身份头衔",
        "· 假头衔 停 · 停掉自动续报",
        "普通成员戴不上群主 / 管理员身份；这是 QQ 的限制，不是模块没更新。",
    ]
    .join("\n")
}

async fn stop(ctx: &Context, writer: LockedWriter, group_id: &str) {
    let had = PLANS.with(|plans| plans.remove(group_id)).await.is_some();
    let quit = wear_target(ctx, writer.clone(), group_id, Target::role("off")).await;
    let tail = match quit {
        Ok(outcome) if outcome.success() => "头衔已换回默认。".to_string(),
        Ok(outcome) => describe(&outcome),
        Err(error) => error_text(&error),
    };
    reply(
        ctx,
        writer,
        group_id,
        if had {
            format!("自动续报停了。{tail}")
        } else {
            format!("本来就没在自动报。{tail}")
        },
    )
    .await;
}

async fn start_auto(
    ctx: &Context,
    writer: LockedWriter,
    group_id: &str,
    user_id: &str,
    head: &str,
    rest: &[String],
    config: &Config,
) {
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
        "波动" => "cycle",
        _ => "fixed",
    };
    let interval = Duration::from_secs(minutes * 60);

    // 先真戴一次。戴不上就不排期，也别让自动循环每轮都去撞同一个拒绝。
    let mut chosen = None;
    let mut last = None;
    for title in &titles {
        match wear_word(
            ctx,
            writer.clone(),
            group_id,
            title,
            config.allow_custom_text,
            config.max_text_chars,
        )
        .await
        {
            Ok(outcome) if outcome.success() => {
                chosen = Some((title.clone(), outcome));
                break;
            }
            Ok(outcome) => {
                // 这枚戴不上不代表别的候选也不行（例如会员群里的「等级」）。
                // 继续试下一枚，最后再把最相关的那条原因带回去。
                last = Some(outcome);
            }
            Err(error) => {
                last = Some(Outcome {
                    error: Some(error),
                    ..Default::default()
                });
            }
        }
    }

    let Some((first_title, outcome)) = chosen else {
        let reason = last.map(|outcome| {
            outcome
                .error
                .as_deref()
                .map(error_text)
                .unwrap_or_else(|| describe(&outcome))
        });
        let reason = reason.unwrap_or_else(|| "候选头衔都戴不上。".into());
        reply(
            ctx,
            writer,
            group_id,
            format!("自动续报没有开起来：{reason}"),
        )
        .await;
        return;
    };

    let plan = Plan {
        mode: mode.into(),
        titles: titles.clone(),
        interval_secs: interval.as_secs(),
        next_at: chrono::Utc::now().timestamp() + interval.as_secs() as i64,
        index: 1,
        started_by: user_id.to_string(),
        last_at: chrono::Utc::now().timestamp(),
        last_title: first_title,
        last_ok: true,
        role: outcome.role.clone(),
        ..Default::default()
    };
    PLANS.with(|plans| {
        plans.insert(group_id.to_string(), plan);
    })
    .await;
    let every = if config.jitter_minutes > 0 {
        format!("约 {minutes} 分钟（±{} 分钟）", config.jitter_minutes)
    } else {
        format!("{minutes} 分钟")
    };
    reply(
        ctx,
        writer,
        group_id,
        format!(
            "自动续报开了：每{every}在「{}」之间切换。{}\n发「假头衔 停」随时收手。",
            titles.join(" / "),
            describe(&outcome)
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
            let candidates = ordered_candidates(&plan);
            let mut outcome = Outcome::default();
            let mut used_index = plan.index;
            for (index, title) in &candidates {
                let current = wear_word(
                    &ctx,
                    writer.clone(),
                    &group_id,
                    title,
                    config.allow_custom_text,
                    config.max_text_chars,
                )
                .await;
                outcome = match current {
                    Ok(outcome) => outcome,
                    Err(error) => Outcome {
                        error: Some(error),
                        ..Default::default()
                    },
                };
                if outcome.success() {
                    used_index = *index;
                    break;
                }
            }
            let jitter = if config.jitter_minutes > 0 {
                rand::rng().random_range(0..=config.jitter_minutes * 60) as i64
            } else {
                0
            };
            if outcome.success() {
                PLANS.with(|plans| {
                    if let Some(plan) = plans.get_mut(&group_id) {
                        plan.last_at = now;
                        plan.last_title = outcome.label.clone();
                        plan.last_ok = true;
                        plan.failures = 0;
                        plan.next_at = now + plan.interval_secs as i64 + jitter;
                        plan.index = used_index.wrapping_add(1);
                        plan.last_error.clear();
                        plan.role = outcome.role.clone();
                    }
                })
                .await;
                if rand::random::<f64>() < config.announce_chance.clamp(0.0, 1.0) {
                    let line = match outcome.source.as_str() {
                        "special" => format!(
                            "本群头衔写成「{}」了。真的假的自己判断。",
                            outcome.label
                        ),
                        _ => format!("本群头衔续上了：「{}」。", outcome.label),
                    };
                    let _ = send_msg(&ctx, writer.clone(), Some(&group_id), None, line).await;
                }
                continue;
            }

            let reason = outcome
                .error
                .as_deref()
                .map(error_text)
                .unwrap_or_else(|| describe(&outcome));
            let retryable = outcome.error.is_none() && outcome.retryable();
            let failures = plan.failures.saturating_add(1);
            let stop_plan = !retryable || failures >= 3;
            PLANS.with(|plans| {
                if stop_plan {
                    plans.remove(&group_id);
                } else if let Some(plan) = plans.get_mut(&group_id) {
                    plan.last_at = now;
                    plan.last_title = outcome.label.clone();
                    plan.last_ok = false;
                    plan.failures = failures;
                    // 指数退避，最多半小时；别每 15 秒去试一次。
                    let backoff = (plan.interval_secs as i64)
                        .saturating_mul(1 << failures.min(4))
                        .min(1800);
                    plan.next_at = now + backoff;
                    plan.last_error = reason.clone();
                    plan.role = outcome.role.clone();
                }
            })
            .await;
            if stop_plan {
                warn!(
                    target: LOG_TARGET,
                    "[Group({group_id})] 自动续报停下：{reason}"
                );
                let _ = send_msg(
                    &ctx,
                    writer.clone(),
                    Some(&group_id),
                    None,
                    format!("自动续报停了：{reason}"),
                )
                .await;
            }
        }
    }
}

fn ordered_candidates(plan: &Plan) -> Vec<(usize, String)> {
    let len = plan.titles.len();
    if len == 0 {
        return vec![(0, OWNER.to_string())];
    }
    let start = match plan.mode.as_str() {
        "fixed" => 0,
        "random" => rand::rng().random_range(0..len),
        _ => plan.index % len,
    };
    (0..len)
        .map(|offset| {
            let index = (start + offset) % len;
            (index, plan.titles[index].clone())
        })
        .collect()
}
