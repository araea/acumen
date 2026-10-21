//! 入群欢迎：进群 @ 一句欢迎（可配图），退群说一声；可选的入群验证与加群申请自动审批。
//!
//! 三件事各有各的开关，按群名单（`channel`）与每群覆盖（`groups."群号"`）收窄：
//!
//! - **欢迎 / 送别**：模板里的 `{at}` 换成真 @，其余占位符见 [`Vars`]。同一时间涌进来
//!   好几个人时可以合并成一条（`join.merge_seconds`），不刷屏。
//! - **入群验证**：新人要在时限内发出指定内容（或任意一句话、或随机验证码），期间按
//!   `remind_seconds` 的剩余时间点 @ 他倒计时；超时按 `action` 移出或禁言。账号不是
//!   群主或管理员时罚不了人，验证整个跳过、只发欢迎。待验证名单落盘，重启后接着计时。
//! - **申请审批**：附言里含关键词的自动同意；对不上的看 `reject` 决定拒绝还是留给人
//!   （智能体的管理栏也看得到，见 [`crate::requests`]）。
//!
//! 默认文案的取舍：一句欢迎、一句规矩、一句后果，短到在手机上一屏读完；口吻是「有人
//! 管、但不凶」——后果写明白（多久、会怎样），不加感叹号堆砌与「警告」字样。

use crate::adapters::satori::{LockedWriter, api, send_msg};
use crate::clock::duration_text;
use crate::event::{Context, EventType};
use crate::message::Message;
use crate::plugins::{ChannelConfig, PluginConfig, PluginError, get_config_or_default};
use crate::storage::JsonState;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsScalar};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const LOG_TARGET: &str = "Plugin/Welcome";

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Config {
    enabled: bool,
    /// 在哪些群生效；两边都空 = 账号所在的全部群。
    channel: ChannelConfig,
    join: JoinConfig,
    leave: LeaveConfig,
    verify: VerifyConfig,
    approve: ApproveConfig,
    /// 每个群单独的文案与开关，键是群号；没写的项沿用上面的。
    groups: HashMap<String, GroupOverride>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct JoinConfig {
    enabled: bool,
    /// 占位符：{at} {name} {user_id} {group} {group_id}
    text: String,
    /// 配图：图片直链，或数据目录 data/welcome/ 下的文件名（也可写绝对路径）。多张时每次随机一张。
    images: Vec<String>,
    /// 这么多秒内进群的人合并成一条欢迎；0 = 来一个欢迎一个。
    merge_seconds: u64,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct LeaveConfig {
    enabled: bool,
    /// 自己退群。占位符：{name} {user_id} {group}
    text: String,
    /// 被移出。再多一个 {operator}；留空则同 text。
    kicked_text: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct VerifyConfig {
    enabled: bool,
    /// 时限（秒）。
    timeout_seconds: u64,
    /// 回答里含其中任一句即通过（不分大小写）；留空且不用验证码 = 说任意一句话就行。
    answers: Vec<String>,
    /// 每人发一个随机 4 位数验证码，回答须含这串数字；优先于 answers。
    code: bool,
    /// 剩余这么多秒时 @ 他提醒一次（每个时间点一次）。
    remind_seconds: Vec<u64>,
    /// 超时怎么办：kick（移出）、mute（禁言）、none（只记一笔）。
    action: String,
    /// action = mute 时禁言多久（秒）。
    mute_seconds: u64,
    /// 移出时拒绝再次申请。
    kick_permanent: bool,
    /// 验证通过前他发的其他消息撤回掉（防新号进群就发广告）。
    recall_unverified: bool,
    /// 接在欢迎后面的验证说明。占位符：{at} {timeout} {answer} {punish}
    prompt_text: String,
    /// 倒计时提醒。占位符：{at} {remaining} {answer} {punish}
    remind_text: String,
    /// 通过时说的话；留空不说。占位符：{at} {name}
    pass_text: String,
    /// 超时处理后说的话；留空不说。占位符：{name} {user_id} {punish}
    fail_text: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct ApproveConfig {
    enabled: bool,
    /// 附言里含其中任一个（不分大小写）就同意；留空 = 全部同意。
    keywords: Vec<String>,
    /// 对不上关键词时拒绝；否则留给管理员（智能体的管理栏也看得到）。
    reject: bool,
    /// 拒绝时给申请人看的话。
    reject_message: String,
}

/// 每群覆盖：只写想改的项。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
struct GroupOverride {
    join_enabled: Option<bool>,
    join_text: Option<String>,
    images: Option<Vec<String>>,
    leave_enabled: Option<bool>,
    verify_enabled: Option<bool>,
    answers: Option<Vec<String>>,
    approve_enabled: Option<bool>,
    keywords: Option<Vec<String>>,
}

impl Default for JoinConfig {
    fn default() -> Self {
        Self {
        enabled: true,
        text: "欢迎 {at} 加入{group}～\n先看看群公告，有问题直接问。".into(),
        images: Vec::new(),
        merge_seconds: 0,
        }
    }
}

impl Default for LeaveConfig {
    fn default() -> Self {
        Self {
        enabled: false,
        text: "{name}（{user_id}）离开了本群。".into(),
        kicked_text: "{name}（{user_id}）已被移出本群。".into(),
        }
    }
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self {
        enabled: false,
        timeout_seconds: 300,
        answers: Vec::new(),
        code: false,
        remind_seconds: vec![120, 30],
        action: "kick".into(),
        mute_seconds: 86400,
        kick_permanent: false,
        recall_unverified: false,
        prompt_text: "请在 {timeout} 内发送「{answer}」完成验证，超时将{punish}。".into(),
        remind_text: "{at} 验证还剩 {remaining}：发送「{answer}」即可，超时将{punish}。".into(),
        pass_text: "{at} 验证通过，欢迎正式入群！".into(),
        fail_text: "{name} 未在时限内完成入群验证，已{punish}。".into(),
        }
    }
}

impl Default for ApproveConfig {
    fn default() -> Self {
        Self {
        enabled: false,
        keywords: Vec::new(),
        reject: false,
        reject_message: "答案不对，看清入群问题再来～".into(),
        }
    }
}

impl PluginConfig for Config {
    const NAME: &'static str = "welcome";

    fn check(&self) -> Result<(), String> {
        if !matches!(self.verify.action.as_str(), "kick" | "mute" | "none") {
            return Err("verify.action 只能是 kick、mute 或 none".into());
        }
        if !(30..=86400).contains(&self.verify.timeout_seconds) {
            return Err("verify.timeout_seconds 须在 30–86400 秒之间".into());
        }
        if self.verify.action == "mute" && !(60..=30 * 86400).contains(&self.verify.mute_seconds) {
            return Err("verify.mute_seconds 须在 60 秒到 30 天之间".into());
        }
        if self.join.merge_seconds > 120 {
            return Err("join.merge_seconds 最多 120 秒".into());
        }
        if self.groups.keys().any(|id| id.parse::<u64>().is_err()) {
            return Err("groups 的键写群号".into());
        }
        Ok(())
    }
}

/// 这个群实际生效的那一份：全局配置叠上每群覆盖。
struct Effective {
    join: bool,
    join_text: String,
    images: Vec<String>,
    leave: bool,
    verify: bool,
    answers: Vec<String>,
    approve: bool,
    keywords: Vec<String>,
}

impl Config {
    fn for_group(&self, group: &str) -> Effective {
        let o = self.groups.get(group).cloned().unwrap_or_default();
        Effective {
            join: o.join_enabled.unwrap_or(self.join.enabled),
            join_text: o.join_text.unwrap_or_else(|| self.join.text.clone()),
            images: o.images.unwrap_or_else(|| self.join.images.clone()),
            leave: o.leave_enabled.unwrap_or(self.leave.enabled),
            verify: o.verify_enabled.unwrap_or(self.verify.enabled),
            answers: o.answers.unwrap_or_else(|| self.verify.answers.clone()),
            approve: o.approve_enabled.unwrap_or(self.approve.enabled),
            keywords: o.keywords.unwrap_or_else(|| self.approve.keywords.clone()),
        }
    }
}

// ==================== 文案 ====================

/// 模板里能用的占位符。`{at}` 不在这里：它是一个真 @，由 [`render`] 单独切开。
#[derive(Default)]
struct Vars<'a> {
    name: &'a str,
    user_id: &'a str,
    group: &'a str,
    group_id: &'a str,
    timeout: String,
    remaining: String,
    answer: String,
    punish: String,
    operator: &'a str,
}

fn fill(text: &str, vars: &Vars) -> String {
    text.replace("{name}", vars.name)
        .replace("{user_id}", vars.user_id)
        .replace("{group}", vars.group)
        .replace("{group_id}", vars.group_id)
        .replace("{timeout}", &vars.timeout)
        .replace("{remaining}", &vars.remaining)
        .replace("{answer}", &vars.answer)
        .replace("{punish}", &vars.punish)
        .replace("{operator}", vars.operator)
}

/// 模板 → 消息：`{at}` 处换成对这些人的 @（合并欢迎时一次 @ 好几个）。
fn render(template: &str, at: &[String], vars: &Vars) -> Message {
    let mut message = Message::new();
    let mut pieces = template.split("{at}").peekable();
    while let Some(piece) = pieces.next() {
        let text = fill(piece, vars);
        if !text.is_empty() {
            message = message.text(text);
        }
        if pieces.peek().is_some() {
            for (index, user) in at.iter().enumerate() {
                if index > 0 {
                    message = message.text(" ");
                }
                message = message.at(user);
            }
        }
    }
    message
}

fn punish_text(config: &VerifyConfig) -> String {
    match config.action.as_str() {
        "kick" => "移出群聊".into(),
        "mute" => format!("禁言 {}", duration_text(config.mute_seconds)),
        _ => "记录在案".into(),
    }
}

// ==================== 待验证名单 ====================

#[derive(Serialize, Deserialize, Clone)]
struct Pending {
    self_id: String,
    group: String,
    user_id: String,
    name: String,
    /// 到期时刻（Unix 秒）。
    deadline: i64,
    /// 本人要发的内容；空 = 任意一句。
    answers: Vec<String>,
    /// 已经提醒过的剩余秒数点。
    reminded: Vec<u64>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct State {
    pending: Vec<Pending>,
}

static STATE: JsonState<State> = JsonState::new(LOG_TARGET, "welcome", "pending.json", "入群验证名单");

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn answer_hint(answers: &[String]) -> String {
    match answers {
        [] => "任意一句话".into(),
        [one, ..] => one.clone(),
    }
}

fn passes(answers: &[String], text: &str) -> bool {
    let text = text.trim().to_lowercase();
    if answers.is_empty() {
        return !text.is_empty();
    }
    answers
        .iter()
        .map(|answer| answer.trim().to_lowercase())
        .any(|answer| !answer.is_empty() && text.contains(&answer))
}

// ==================== 事件 ====================

pub fn handle(ctx: Context, writer: LockedWriter) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let EventType::Satori(raw) = &ctx.event else {
            return Ok(Some(ctx));
        };
        let Some(group) = raw.get_str("group_id").filter(|id| !id.is_empty()).map(str::to_owned) else {
            return Ok(Some(ctx));
        };
        let config = get_config_or_default::<Config>(&ctx);
        if !config.channel.allows_group(&group) {
            return Ok(Some(ctx));
        }
        let user = raw.get_str("user_id").unwrap_or("").to_string();
        let me = ctx.bot.self_id();
        let consumed = match raw.get_str("satori_type").unwrap_or("") {
            "guild-member-added" if user != me => {
                joined(&ctx, &writer, &config, &group, &user).await?;
                false
            }
            "guild-member-removed" if user != me => {
                left(&ctx, &writer, &config, &group, &user).await?;
                false
            }
            "guild-member-request" => {
                request(&ctx, &writer, &config, &group).await?;
                false
            }
            "message-created" if !user.is_empty() && user != me => {
                verify_message(&ctx, &writer, &config, &group, &user).await?
            }
            _ => false,
        };
        Ok((!consumed).then_some(ctx))
    })
}

/// 事件里带的名字；没有就问一次平台，再没有用号码。
async fn display_name(ctx: &Context, writer: &LockedWriter, group: &str, user: &str) -> String {
    if let EventType::Satori(raw) = &ctx.event {
        for path in [["_satori", "member", "nick"], ["_satori", "user", "nick"], ["_satori", "user", "name"]] {
            if let Some(name) = raw
                .get(path[0])
                .and_then(|v| v.get(path[1]))
                .and_then(|v| v.get_str(path[2]))
                .filter(|name| !name.trim().is_empty())
            {
                return name.trim().to_string();
            }
        }
    }
    if let Ok(info) = api::get_group_member_info(ctx, writer.clone(), group, user, false).await {
        let name = if info.card.trim().is_empty() { info.nickname } else { info.card };
        if !name.trim().is_empty() {
            return name.trim().to_string();
        }
    }
    if let Ok(value) = writer.call::<_, Value>(ctx, "user.get", json!({"user_id": user})).await
        && let Some(name) = ["/nick", "/name"]
            .iter()
            .find_map(|path| value.pointer(path).and_then(Value::as_str).filter(|n| !n.trim().is_empty()))
    {
        return name.trim().to_string();
    }
    user.to_string()
}

async fn group_name(ctx: &Context, writer: &LockedWriter, group: &str) -> String {
    if let EventType::Satori(raw) = &ctx.event
        && let Some(name) = raw
            .get("_satori")
            .and_then(|v| v.get("guild"))
            .and_then(|v| v.get_str("name"))
            .filter(|name| !name.is_empty())
    {
        return name.to_string();
    }
    api::get_guild_info(ctx, writer.clone(), group)
        .await
        .map(|info| info.group_name)
        .unwrap_or_default()
}

/// 合并欢迎的缓冲：群 → 这一批要 @ 的人。
fn batch() -> &'static Mutex<HashMap<String, Vec<String>>> {
    static BATCH: OnceLock<Mutex<HashMap<String, Vec<String>>>> = OnceLock::new();
    BATCH.get_or_init(Default::default)
}

async fn joined(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str, user: &str) -> Result<(), PluginError> {
    let effective = config.for_group(group);
    let mut verify = false;
    let mut answers = Vec::new();
    if effective.verify {
        let exempt = crate::plugins::ctl::admin_ids(ctx).iter().any(|id| id == user);
        if !exempt && api::is_staff(&api::own_role(ctx, writer.clone(), group).await) {
            verify = true;
            answers = if config.verify.code {
                vec![format!("{:04}", rand::random_range(0..10000))]
            } else {
                effective.answers.clone()
            };
        } else if !exempt {
            debug!(target: LOG_TARGET, "群 {group} 账号不是管理员，入群验证跳过");
        }
    }
    if !effective.join && !verify {
        return Ok(());
    }
    let name = display_name(ctx, writer, group, user).await;
    let group_title = group_name(ctx, writer, group).await;
    if verify {
        let pending = Pending {
            self_id: ctx.bot.self_id(),
            group: group.to_string(),
            user_id: user.to_string(),
            name: name.clone(),
            deadline: now() + i64::try_from(config.verify.timeout_seconds).unwrap_or(300),
            answers: answers.clone(),
            reminded: Vec::new(),
        };
        STATE
            .with(|state| {
                state.pending.retain(|p| !(p.group == pending.group && p.user_id == pending.user_id));
                state.pending.push(pending);
            })
            .await;
        info!(target: LOG_TARGET, "群 {group} 新成员 {name}（{user}）开始入群验证");
    }
    // 要验证的人单独欢迎：验证说明里有他自己的验证码与倒计时，合不进别人的那条。
    if !verify && config.join.merge_seconds > 0 {
        let first = {
            let mut batch = batch().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let list = batch.entry(group.to_string()).or_default();
            list.push(user.to_string());
            list.len() == 1
        };
        if first {
            let (ctx, writer, group) = (ctx.clone(), writer.clone(), group.to_string());
            let delay = Duration::from_secs(config.join.merge_seconds);
            let (text, images, group_title) = (effective.join_text.clone(), effective.images.clone(), group_title.clone());
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let users = batch()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&group)
                    .unwrap_or_default();
                let names = if users.len() == 1 { name } else { format!("{} 位新朋友", users.len()) };
                let vars = Vars { name: &names, user_id: &users.join("、"), group: &group_title, group_id: &group, ..Vars::default() };
                let message = with_image(&ctx, &writer, render(&text, &users, &vars), &images).await;
                if let Err(error) = send_msg(&ctx, writer, Some(&group), None, message).await {
                    warn!(target: LOG_TARGET, "群 {group} 欢迎没发出去：{error}");
                }
            });
        }
        return Ok(());
    }
    let mut vars = Vars { name: &name, user_id: user, group: &group_title, group_id: group, ..Vars::default() };
    let mut template = if effective.join { effective.join_text.clone() } else { String::new() };
    if verify {
        vars.timeout = duration_text(config.verify.timeout_seconds);
        vars.answer = answer_hint(&answers);
        vars.punish = punish_text(&config.verify);
        let prompt = &config.verify.prompt_text;
        if template.is_empty() {
            template = if prompt.contains("{at}") { prompt.clone() } else { format!("{{at}} {prompt}") };
        } else {
            template = format!("{template}\n{prompt}");
        }
    }
    let images = if effective.join { effective.images } else { Vec::new() };
    let message = with_image(ctx, writer, render(&template, &[user.to_string()], &vars), &images).await;
    send_msg(ctx, writer.clone(), Some(group), None, message).await?;
    Ok(())
}

/// 配图随机挑一张接在文字后面；本地文件先上传（QQ 读不到 Termux 的私有目录）。
async fn with_image(ctx: &Context, writer: &LockedWriter, message: Message, images: &[String]) -> Message {
    if images.is_empty() {
        return message;
    }
    let pick = &images[rand::random_range(0..images.len())];
    let pick = pick.trim();
    if pick.starts_with("http://") || pick.starts_with("https://") {
        return message.image(pick);
    }
    let path = if std::path::Path::new(pick).is_absolute() {
        std::path::PathBuf::from(pick)
    } else {
        match crate::storage::data_path("welcome") {
            Ok(dir) => dir.join(pick),
            Err(_) => return message,
        }
    };
    let uploaded = match tokio::fs::read(&path).await {
        Ok(bytes) => writer.upload(ctx, bytes, pick, "application/octet-stream").await,
        Err(error) => {
            warn!(target: LOG_TARGET, "欢迎配图 {} 读不出来：{error}", path.display());
            return message;
        }
    };
    match uploaded.ok().and_then(|value| value.get("file").and_then(Value::as_str).map(str::to_owned)) {
        Some(file) => message.image(file),
        None => message,
    }
}

async fn left(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str, user: &str) -> Result<(), PluginError> {
    let was_pending = STATE
        .with(|state| {
            let before = state.pending.len();
            state.pending.retain(|p| !(p.group == group && p.user_id == user));
            before != state.pending.len()
        })
        .await;
    if was_pending {
        info!(target: LOG_TARGET, "群 {group} 待验证的 {user} 已离开，验证作废");
    }
    if !config.for_group(group).leave {
        return Ok(());
    }
    let EventType::Satori(raw) = &ctx.event else {
        return Ok(());
    };
    let operator = raw.get_str("operator_id").unwrap_or("").to_string();
    let kicked = !operator.is_empty() && operator != user;
    // 自己按超时踢掉的已经说过一句了，别再报一遍。
    if kicked && operator == ctx.bot.self_id() && recently_failed(group, user) {
        return Ok(());
    }
    let name = display_name(ctx, writer, group, user).await;
    let operator_name = if kicked { display_name(ctx, writer, group, &operator).await } else { String::new() };
    let group_title = group_name(ctx, writer, group).await;
    let template = if kicked && !config.leave.kicked_text.trim().is_empty() {
        &config.leave.kicked_text
    } else {
        &config.leave.text
    };
    let vars = Vars { name: &name, user_id: user, group: &group_title, group_id: group, operator: &operator_name, ..Vars::default() };
    send_msg(ctx, writer.clone(), Some(group), None, render(template, &[user.to_string()], &vars)).await?;
    Ok(())
}

/// 刚因验证超时被我们移出的人（群, QQ），记两分钟：退群事件回来时不再说第二遍。
fn failed() -> &'static Mutex<Vec<(String, String, Instant)>> {
    static FAILED: OnceLock<Mutex<Vec<(String, String, Instant)>>> = OnceLock::new();
    FAILED.get_or_init(Default::default)
}

fn recently_failed(group: &str, user: &str) -> bool {
    let mut list = failed().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    list.retain(|(_, _, at)| at.elapsed() < Duration::from_mins(2));
    list.iter().any(|(g, u, _)| g == group && u == user)
}

async fn request(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str) -> Result<(), PluginError> {
    let effective = config.for_group(group);
    if !effective.approve {
        return Ok(());
    }
    let EventType::Satori(raw) = &ctx.event else {
        return Ok(());
    };
    if raw.get_str("sub_type") != Some("add") {
        return Ok(());
    }
    let flag = raw.get_str("flag").unwrap_or("").to_string();
    let user = raw.get_str("user_id").unwrap_or("").to_string();
    let comment = raw.get_str("comment").unwrap_or("").to_lowercase();
    if flag.is_empty() {
        return Ok(());
    }
    let matched = effective.keywords.is_empty()
        || effective
            .keywords
            .iter()
            .map(|keyword| keyword.trim().to_lowercase())
            .any(|keyword| !keyword.is_empty() && comment.contains(&keyword));
    if !matched && !config.approve.reject {
        info!(target: LOG_TARGET, "群 {group} 的加群申请（{user}）附言对不上，留给管理员");
        return Ok(());
    }
    let params = json!({"message_id": flag, "approve": matched, "comment": if matched { "" } else { config.approve.reject_message.as_str() }});
    match writer.call::<_, Value>(ctx, "guild.member.approve", params).await {
        Ok(_) => {
            crate::requests::resolve(&flag);
            info!(target: LOG_TARGET, "群 {group} 的加群申请（{user}）已自动{}", if matched { "同意" } else { "拒绝" });
        }
        Err(error) => warn!(target: LOG_TARGET, "群 {group} 的加群申请（{user}）没能自动处理：{error}"),
    }
    Ok(())
}

/// 待验证的人说话了：答对就放行。返回 true 表示这条消息已被处理（撤回了），不再往下传。
async fn verify_message(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str, user: &str) -> Result<bool, PluginError> {
    let Some(message) = ctx.as_message() else {
        return Ok(false);
    };
    let text = message.text().to_string();
    let message_id = message.message_id().to_string();
    let found = STATE
        .with(|state| {
            let index = state.pending.iter().position(|p| p.group == group && p.user_id == user)?;
            if passes(&state.pending[index].answers, &text) {
                Some(Some(state.pending.remove(index)))
            } else {
                Some(None)
            }
        })
        .await;
    match found {
        None => Ok(false),
        Some(Some(passed)) => {
            info!(target: LOG_TARGET, "群 {group} 的 {}（{user}）通过入群验证", passed.name);
            if !config.verify.pass_text.trim().is_empty() {
                let vars = Vars { name: &passed.name, user_id: user, group_id: group, ..Vars::default() };
                send_msg(ctx, writer.clone(), Some(group), None, render(&config.verify.pass_text, &[user.to_string()], &vars)).await?;
            }
            Ok(false)
        }
        Some(None) if config.verify.recall_unverified && !message_id.is_empty() => {
            if let Err(error) = api::delete_msg_in(ctx, writer.clone(), group, &message_id).await {
                debug!(target: LOG_TARGET, "撤回待验证成员的消息失败：{error}");
            }
            Ok(true)
        }
        Some(None) => Ok(false),
    }
}

// ==================== 计时 ====================

/// 每条连接起一个计时器：到点提醒、超时处理。只管自己这个账号名下的待验证成员。
pub fn on_connected(ctx: Context, writer: LockedWriter) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        STATE.preload().await;
        let (tick_ctx, tick_writer) = (ctx.clone(), writer.clone());
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                tick(&tick_ctx, &tick_writer).await;
            }
        });
        Ok(Some(ctx))
    })
}

enum Due {
    Remind(Pending, u64),
    Expire(Pending),
}

async fn tick(ctx: &Context, writer: &LockedWriter) {
    let config = get_config_or_default::<Config>(ctx);
    let me = ctx.bot.self_id();
    let now = now();
    let mut remind_points = config.verify.remind_seconds.clone();
    remind_points.sort_unstable_by(|a, b| b.cmp(a));
    let due: Vec<Due> = STATE
        .with(|state| {
            let mut due = Vec::new();
            state.pending.retain_mut(|p| {
                if p.self_id != me {
                    return true;
                }
                // 插件关了、或这个群不再验证：名单作废，不罚。
                if !config.enabled || !config.channel.allows_group(&p.group) || !config.for_group(&p.group).verify {
                    return false;
                }
                let remaining = p.deadline - now;
                if remaining <= 0 {
                    due.push(Due::Expire(p.clone()));
                    return false;
                }
                let remaining = remaining.unsigned_abs();
                if let Some(point) = remind_points
                    .iter()
                    .copied()
                    .filter(|point| remaining <= *point && !p.reminded.contains(point))
                    .min()
                {
                    // 一次跨过好几个点（比如重启后）只提醒最近的那个，其余记作已提醒。
                    for skipped in remind_points.iter().filter(|q| **q >= point) {
                        if !p.reminded.contains(skipped) {
                            p.reminded.push(*skipped);
                        }
                    }
                    due.push(Due::Remind(p.clone(), remaining));
                }
                true
            });
            due
        })
        .await;
    for item in due {
        match item {
            Due::Remind(p, remaining) => {
                let vars = Vars {
                    name: &p.name,
                    user_id: &p.user_id,
                    group_id: &p.group,
                    remaining: duration_text(remaining.div_ceil(10) * 10),
                    answer: answer_hint(&p.answers),
                    punish: punish_text(&config.verify),
                    ..Vars::default()
                };
                let message = render(&config.verify.remind_text, std::slice::from_ref(&p.user_id), &vars);
                if let Err(error) = send_msg(ctx, writer.clone(), Some(&p.group), None, message).await {
                    warn!(target: LOG_TARGET, "群 {} 验证提醒没发出去：{error}", p.group);
                }
            }
            Due::Expire(p) => expire(ctx, writer, &config, p).await,
        }
    }
}

async fn expire(ctx: &Context, writer: &LockedWriter, config: &Config, p: Pending) {
    let verify = &config.verify;
    let result = match verify.action.as_str() {
        "kick" => {
            failed()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((p.group.clone(), p.user_id.clone(), Instant::now()));
            writer
                .call::<_, Value>(
                    ctx,
                    "guild.member.kick",
                    json!({"guild_id": p.group, "user_id": p.user_id, "permanent": verify.kick_permanent}),
                )
                .await
                .map(|_| ())
        }
        "mute" => writer
            .call::<_, Value>(
                ctx,
                "guild.member.mute",
                json!({"guild_id": p.group, "user_id": p.user_id, "duration": verify.mute_seconds * 1000}),
            )
            .await
            .map(|_| ()),
        _ => Ok(()),
    };
    if let Err(error) = result {
        warn!(target: LOG_TARGET, "群 {} 的 {}（{}）验证超时，处理失败：{error}", p.group, p.name, p.user_id);
        return;
    }
    info!(target: LOG_TARGET, "群 {} 的 {}（{}）验证超时，已按 {} 处理", p.group, p.name, p.user_id, verify.action);
    if verify.fail_text.trim().is_empty() {
        return;
    }
    let vars = Vars { name: &p.name, user_id: &p.user_id, group_id: &p.group, punish: punish_text(verify), ..Vars::default() };
    let message = render(&verify.fail_text, std::slice::from_ref(&p.user_id), &vars);
    if let Err(error) = send_msg(ctx, writer.clone(), Some(&p.group), None, message).await {
        warn!(target: LOG_TARGET, "群 {} 验证结果没发出去：{error}", p.group);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_match_loosely_and_empty_means_anything() {
        assert!(passes(&[], "你好"));
        assert!(!passes(&[], "   "));
        let answers = vec!["我已阅读群规".to_string(), "OK".to_string()];
        assert!(passes(&answers, "好的，我已阅读群规！"));
        assert!(passes(&answers, "ok"));
        assert!(!passes(&answers, "收到"));
    }

    #[test]
    fn at_placeholders_become_mentions() {
        let vars = Vars { name: "小明", group: "测试群", ..Vars::default() };
        let message = render("欢迎 {at} 加入{group}，{name}", &["1".into(), "2".into()], &vars);
        let kinds: Vec<_> = message.0.iter().map(|s| s.type_.as_str()).collect();
        assert_eq!(kinds, ["text", "at", "text", "at", "text"]);
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration_text(300), "5 分钟");
        assert_eq!(duration_text(90), "1 分 30 秒");
        assert_eq!(duration_text(86400), "1 天");
        assert_eq!(duration_text(30), "30 秒");
    }

    #[test]
    fn defaults_pass_their_own_check() {
        assert!(Config::default().check().is_ok());
    }
}
