//! 违禁词：群里出现设定的词（也能写正则）就按规则提醒、撤回、禁言或撤回并禁言。
//!
//! 群管理的刚需功能，重点在「别误伤、别吵到聊天」：
//!
//! - 动作四选一：仅提醒、撤回、禁言、撤回并禁言；禁言时长可以每条规则单独设。
//! - 提示可静默：`notice` 留空就一个字都不发；要发的话可以设 `notice_seconds`，到期自己撤回，
//!   写 0 就是永久留着。
//! - 管理员与号主永远不动手；`exempt` 里的账号也不动手。`exempt_staff` 关掉才管群管理。
//! - 刷屏只处理一次：同一个人在同一群里已经被禁言时，后面那几条照常撤回，但不再重复禁言、
//!   也不再刷提示；`repeat_seconds` 之内同一个人只提示一次。
//! - 机器人自己、号主手打的话、以指令前缀开头的消息都不进检测；图片与卡片不扫（没有 OCR）。
//!
//! 规则是内容、不是配置：群主管理员在群里用「违禁词 添加 …」维护本群的规则，号主用
//! 「违禁词 全局添加 …」维护所有群共用的规则。两边都落在 `data/keyword_guard/rules.json`
//! （见 [`book`]），改词不会重写 `config.toml`，也碰不到别的群。
//!
//! 比对之前两边都做归一化（见 [`crate::text::fold`]）：全角转半角、不分大小写、去掉空白
//! 标点与零宽字符，所以「加 微 信」「加，微，信」「Ｖ信」都认。正则对保留空格标点的原文比，
//! 只统一宽度与大小写，写 `加\\s*微信` 也照常好使。

mod book;
mod command;

use crate::adapters::satori::{LockedWriter, api, send_msg_id};
use crate::command::match_word_command;
use crate::event::Context;
use crate::message::Message;
use crate::plugins::{ChannelConfig, PluginConfig, PluginError, get_config_or_default};
use book::{Action, BOOK, Rule};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use simd_json::derived::ValueObjectAccessAsScalar;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

const LOG_TARGET: &str = "Plugin/KeywordGuard";

/// 提示里 `{at}` 的临时记号：名字里不会出现这个私用区字符。
const AT_MARK: char = '\u{E000}';

/// 提示语：一句，或几句随机挑一句。
///
/// 写成一句时配置里是字符串；控制台的文案编辑器点「再加一种说法」会存成数组。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(untagged)]
pub enum Notice {
    One(String),
    Many(Vec<String>),
}

impl Default for Notice {
    fn default() -> Self {
        Self::One("{at} 本群禁止广告、引流等违规内容，{action}。".into())
    }
}

impl Notice {
    pub fn list(&self) -> Vec<&str> {
        match self {
            Self::One(text) => vec![text.as_str()],
            Self::Many(texts) => texts.iter().map(String::as_str).collect(),
        }
    }

    /// 一句能用的都没有 = 静默。
    pub fn is_silent(&self) -> bool {
        self.list().iter().all(|text| text.trim().is_empty())
    }

    /// 随机挑一句非空的；全空时没有。
    pub fn pick(&self) -> Option<&str> {
        let texts: Vec<&str> = self
            .list()
            .into_iter()
            .filter(|text| !text.trim().is_empty())
            .collect();
        if texts.is_empty() {
            return None;
        }
        texts.get(rand::random_range(0..texts.len())).copied()
    }

    /// 第一句非空的，给「试一句」预览用（预览不随机跳）。
    pub fn first(&self) -> Option<&str> {
        self.list().into_iter().find(|text| !text.trim().is_empty())
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    enabled: bool,
    /// 在哪些群生效；两边都空 = 账号所在的全部群。
    channel: ChannelConfig,
    /// 新规则没写动作时用这个。
    default_action: Action,
    /// 新规则没写禁言时长、或旧规则没单独设时用这个。
    mute_seconds: u64,
    /// 提示语；空 = 静默，什么都不发。可用 {at} {name} {keyword} {action} {duration} {group}。
    /// 写成数组就是几种说法随机一种（控制台的文案编辑器会这么存）。
    notice: Notice,
    /// 提示留多久：0 = 永久保留，其余秒数到点自动撤回。
    notice_seconds: u64,
    /// 群主、管理员也说违禁词时不动手（默认开）。关掉就连群管一起处理。
    exempt_staff: bool,
    /// 另外不动手的账号。
    exempt: Vec<String>,
    /// 这些账号说话不检测（比如群里别的机器人）。
    ignore_users: Vec<String>,
    /// 同一个人在同一群里两次提示至少隔这么多秒；0 = 不隔。
    repeat_seconds: u64,
    /// 谁能改本群规则：staff = 群主、管理员与机器人管理员；admin = 只有机器人管理员。
    managers: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            channel: ChannelConfig::default(),
            default_action: Action::Recall,
            mute_seconds: 600,
            notice: Notice::One("{at} 本群禁止广告、引流等违规内容，{action}。".into()),
            notice_seconds: 60,
            exempt_staff: true,
            exempt: Vec::new(),
            ignore_users: Vec::new(),
            repeat_seconds: 300,
            managers: "staff".into(),
        }
    }
}

impl PluginConfig for Config {
    const NAME: &'static str = "keyword_guard";

    fn check(&self) -> Result<(), String> {
        if !matches!(self.managers.as_str(), "staff" | "admin") {
            return Err("managers 只能是 staff 或 admin".into());
        }
        if !(book::MUTE_MIN..=book::MUTE_MAX).contains(&self.mute_seconds) {
            return Err(format!(
                "mute_seconds 要在 {} 到 {} 之间",
                book::MUTE_MIN,
                book::MUTE_MAX
            ));
        }
        if self.notice_seconds != 0
            && !(5..=book::NOTICE_MAX_SECONDS).contains(&self.notice_seconds)
        {
            return Err(format!(
                "notice_seconds 写 0 表示永久保留，否则要在 5 秒到 {} 之间",
                crate::clock::duration_text(book::NOTICE_MAX_SECONDS)
            ));
        }
        let mut total = 0;
        for text in self.notice.list() {
            let chars = text.chars().count();
            if chars > 1000 {
                return Err("提示语最多 1000 字一种".into());
            }
            total += chars;
        }
        if total > 4000 {
            return Err("提示语合计最多 4000 字".into());
        }
        if self.repeat_seconds > 86400 {
            return Err("repeat_seconds 最多 86400 秒（一天）".into());
        }
        for id in self.exempt.iter().chain(&self.ignore_users) {
            if id.trim().is_empty() || id.len() > 128 || id.chars().any(char::is_whitespace) {
                return Err(format!("「{id}」不像一个账号"));
            }
        }
        Ok(())
    }
}

pub fn init(ctx: Context) -> BoxFuture<'static, Result<(), PluginError>> {
    Box::pin(async move {
        BOOK.preload().await;
        let default_mute = get_config_or_default::<Config>(&ctx).mute_seconds;
        let (rules, globals, invalid) = BOOK
            .read(|book| {
                let invalid = book
                    .rules
                    .iter()
                    .filter_map(|rule| {
                        rule.problem(default_mute)
                            .map(|why| format!("#{} {why}", rule.id))
                    })
                    .collect::<Vec<_>>();
                (book.rules.len(), book.global().len(), invalid)
            })
            .await;
        info!(
            target: LOG_TARGET,
            "就绪：{rules} 条规则（{globals} 条全局）"
        );
        for why in invalid {
            warn!(target: LOG_TARGET, "规则有问题，可能不会按预期触发：{why}");
        }
        Ok(())
    })
}

pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let Some(message) = ctx.as_message() else {
            return Ok(Some(ctx));
        };
        let me = ctx.bot.self_id();
        let user = message.user_id();
        // 机器人自己发出去的回声不理；号主手打的照常走（可能是指令）。
        if !user.is_empty() && user == me && !message.is_manual_self() {
            return Ok(Some(ctx));
        }
        let group = message.group_id().map(str::to_owned);
        let config = get_config_or_default::<Config>(&ctx);
        let allowed = match &group {
            Some(group) => config.channel.allows_group(group),
            None => config.channel.allows_direct(&[message.channel_id(), user]),
        };
        if !allowed {
            return Ok(Some(ctx));
        }

        // 管理指令先认：指令本身可能带着违禁词，不能先被自己的规则处理掉。
        if let Some(matched) = ["违禁词", "敏感词"]
            .iter()
            .find_map(|word| match_word_command(&ctx, word))
        {
            command::run(&ctx, &writer, &config, group.as_deref(), matched).await?;
            return Ok(None);
        }

        let Some(group) = group else {
            return Ok(Some(ctx));
        };
        let text = message.text().trim();
        if text.is_empty() {
            return Ok(Some(ctx));
        }
        // 号主、全局管理员、机器人自己永远不动手。
        if user.is_empty()
            || user == me
            || message.is_manual_self()
            || crate::access::is_manager(&ctx)
        {
            return Ok(Some(ctx));
        }
        if config.ignore_users.iter().any(|id| id == user)
            || config.exempt.iter().any(|id| id == user)
        {
            return Ok(Some(ctx));
        }
        if config.exempt_staff && message.sender_role().is_some_and(api::is_staff) {
            return Ok(Some(ctx));
        }
        // 以指令前缀开头的消息交给指令，不当成聊天内容。
        let prefixes = crate::command::get_prefixes(&ctx);
        if prefixes
            .iter()
            .any(|prefix| !prefix.is_empty() && text.starts_with(prefix))
        {
            return Ok(Some(ctx));
        }

        let body = book::Text::new(text);
        if body.is_empty() {
            return Ok(Some(ctx));
        }
        let found = BOOK
            .read(|book| {
                book.effective(&group).into_iter().find_map(|rule| {
                    rule.hit(body.raw, &body.normalized, &body.folded)
                        .map(|keyword| (rule.clone(), keyword))
                })
            })
            .await;
        let Some((rule, keyword)) = found else {
            return Ok(Some(ctx));
        };

        let name = message.sender_name().to_string();
        let message_id = message.message_id().to_string();
        let group_name = message.0.get_str("group_name").unwrap_or("").to_string();
        let outcome = act(
            &ctx,
            &writer,
            &config,
            &rule,
            &keyword,
            &group,
            user,
            &name,
            &message_id,
            &group_name,
        )
        .await;
        BOOK.update(|book| {
            let changed = book.count_hit(rule.id, chrono::Utc::now().timestamp());
            ((), changed)
        })
        .await;
        // 真撤了或真禁了才算处理过：后面接话、复读、智能体都不必再看到这条。
        if outcome.handled {
            Ok(None)
        } else {
            Ok(Some(ctx))
        }
    })
}

/// 一次动手的结果。
#[derive(Default)]
struct Outcome {
    handled: bool,
}

/// 按规则动手，再按提示规则说一句。
#[allow(clippy::too_many_arguments)]
async fn act(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    rule: &Rule,
    keyword: &str,
    group: &str,
    user: &str,
    name: &str,
    message_id: &str,
    group_name: &str,
) -> Outcome {
    let action = rule.action;
    // 手改 rules.json 写了离谱的时长也不至于发出一个超长的禁言。
    let mute_seconds = rule
        .mute_seconds
        .unwrap_or(config.mute_seconds)
        .min(book::MUTE_MAX);
    let mut outcome = Outcome::default();

    // 撤回与禁言都得机器人是本群管理员；不是就先别白跑一趟接口。
    if (action.recalls() || action.mutes())
        && !api::is_staff(&api::own_role(ctx, writer.clone(), group).await)
    {
        warn!(
            target: LOG_TARGET,
            "群 {group} 触发了 #{}，但机器人不是本群管理员，撤不了也禁不了；在群里发「违禁词」可看状态",
            rule.id
        );
        return outcome;
    }

    let seen = gate().seen(group, user);
    let now = Instant::now();
    let already_muted = seen.muted_until.is_some_and(|until| until > now);
    let notice_gap = Duration::from_secs(config.repeat_seconds);
    let notice_quiet = config.repeat_seconds > 0
        && seen
            .last_notice
            .is_some_and(|at| now.duration_since(at) < notice_gap);

    let mut recalled = false;
    if action.recalls() && !message_id.is_empty() {
        match api::delete_msg_in(ctx, writer.clone(), group, message_id).await {
            Ok(()) => recalled = true,
            Err(error) => {
                warn!(
                    target: LOG_TARGET,
                    "群 {group} 撤回 {name}（{user}）的消息 {message_id} 失败：{error}"
                );
            }
        }
    }

    let mut muted = 0;
    if action.mutes() && !already_muted && mute_seconds > 0 {
        match writer
            .call::<_, serde_json::Value>(
                ctx,
                "guild.member.mute",
                serde_json::json!({
                    "guild_id": group,
                    "user_id": user,
                    "duration": mute_seconds.saturating_mul(1000),
                }),
            )
            .await
        {
            Ok(_) => {
                muted = mute_seconds;
                gate().mark_muted(group, user, now + Duration::from_secs(mute_seconds));
            }
            Err(error) => {
                warn!(
                    target: LOG_TARGET,
                    "群 {group} 禁言 {name}（{user}）失败：{error}"
                );
            }
        }
    }

    let done = match action {
        Action::Alert => "请注意".to_string(),
        Action::Recall if recalled => "消息已撤回".to_string(),
        Action::Mute if muted > 0 => format!("已禁言 {}", crate::clock::duration_text(muted)),
        Action::RecallMute => match (recalled, muted) {
            (true, 0) => "消息已撤回".to_string(),
            (false, muted) if muted > 0 => {
                format!("已禁言 {}", crate::clock::duration_text(muted))
            }
            (true, muted) if muted > 0 => {
                format!("消息已撤回，并禁言 {}", crate::clock::duration_text(muted))
            }
            _ => String::new(),
        },
        _ => String::new(),
    };
    outcome.handled = recalled || muted > 0;

    if done.is_empty() {
        info!(
            target: LOG_TARGET,
            "群 {group} 的 {name}（{user}）触发 #{}「{keyword}」，但动作没成功",
            rule.id
        );
        return outcome;
    }
    info!(
        target: LOG_TARGET,
        "群 {group} 的 {name}（{user}）触发 #{}「{keyword}」，{done}", rule.id
    );

    // 已经禁言着：后面那几条照撤，但别再刷提示。
    if already_muted || notice_quiet {
        return outcome;
    }
    if config.notice.is_silent() {
        return outcome;
    }
    let message = notice_message(config, user, name, keyword, &done, mute_seconds, group_name);
    if message.0.is_empty() {
        return outcome;
    }
    match send_msg_id(ctx, writer.clone(), Some(group), Some(user), message).await {
        Ok(id) => {
            gate().mark_notice(group, user);
            let seconds = config.notice_seconds.min(book::NOTICE_MAX_SECONDS);
            if seconds > 0 {
                match id {
                    Some(id) => {
                        schedule_recall(
                            ctx.clone(),
                            writer.clone(),
                            group.to_string(),
                            id,
                            seconds,
                        );
                    }
                    None => warn!(
                        target: LOG_TARGET,
                        "群 {group} 的提示发出去了，但没拿到消息 ID，这条不会自动撤回"
                    ),
                }
            }
        }
        Err(error) => warn!(target: LOG_TARGET, "群 {group} 的提示没发出去：{error}"),
    }
    outcome
}

/// 到点自己撤回一条提示。
fn schedule_recall(
    ctx: Context,
    writer: LockedWriter,
    group: String,
    message_id: String,
    seconds: u64,
) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(seconds)).await;
        if let Err(error) = api::delete_msg_in(&ctx, writer, &group, &message_id).await {
            debug!(
                target: LOG_TARGET,
                "群 {group} 的提示 {message_id} 到点没撤掉：{error}（可能已经被手动撤回）"
            );
        }
    });
}

/// 把提示模板变成一条消息：`{at}` 是真 @，其余占位符换成文字。
fn notice_message(
    config: &Config,
    user: &str,
    name: &str,
    keyword: &str,
    done: &str,
    mute_seconds: u64,
    group_name: &str,
) -> Message {
    let Some(template) = config.notice.pick() else {
        return Message::new();
    };
    let mut text = template.replace("{at}", &AT_MARK.to_string());
    text = expand(&text, name, keyword, done, mute_seconds, group_name);
    if user.is_empty() {
        text = text.replace(AT_MARK, &format!("@{name} "));
    }
    let mut message = Message::new();
    for (index, part) in text.split(AT_MARK).enumerate() {
        if index > 0 {
            message = message.at(user);
        }
        if !part.is_empty() {
            message = message.text(part.to_string());
        }
    }
    message
}

/// 提示的纯文字样子（「试一句」与列表里预览用）。
pub(crate) fn notice_preview(
    config: &Config,
    name: &str,
    keyword: &str,
    done: &str,
    mute_seconds: u64,
    group_name: &str,
) -> String {
    let Some(template) = config.notice.first() else {
        return String::new();
    };
    let at = if name.is_empty() {
        String::new()
    } else {
        format!("@{name} ")
    };
    let text = template.replace("{at}", &at);
    expand(&text, name, keyword, done, mute_seconds, group_name)
        .trim()
        .to_string()
}

/// 一次扫完占位符：换进去的值里如果也带着 `{name}` 之类，不会被再换一遍。
fn expand(
    text: &str,
    name: &str,
    keyword: &str,
    done: &str,
    mute_seconds: u64,
    group_name: &str,
) -> String {
    let duration = if mute_seconds > 0 {
        crate::clock::duration_text(mute_seconds)
    } else {
        String::new()
    };
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}').map(|at| open + at) else {
            out.push_str(&rest[open..]);
            return out;
        };
        match &rest[open + 1..close] {
            "name" => out.push_str(name),
            "keyword" => out.push_str(keyword),
            "action" => out.push_str(done),
            "duration" => out.push_str(&duration),
            "group" => out.push_str(group_name),
            _ => out.push_str(&rest[open..=close]),
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// 提示留多久的人话：静默 / 永久 / X 后自动撤回。
pub(crate) fn notice_lifetime(config: &Config) -> String {
    if config.notice.is_silent() {
        "静默，群里不发提示（只写日志）".into()
    } else if config.notice_seconds == 0 {
        "永久保留".into()
    } else {
        format!(
            "{}后自动撤回",
            crate::clock::duration_text(config.notice_seconds.min(book::NOTICE_MAX_SECONDS))
        )
    }
}

/// 「假设动作都成功」时的 {action} 说法，给「试一句」预览用。
pub(crate) fn done_preview(action: Action, mute_seconds: u64) -> String {
    match action {
        Action::Alert => "请注意".to_string(),
        Action::Recall => "消息已撤回".to_string(),
        Action::Mute => format!("已禁言 {}", crate::clock::duration_text(mute_seconds)),
        Action::RecallMute => format!(
            "消息已撤回，并禁言 {}",
            crate::clock::duration_text(mute_seconds)
        ),
    }
}

// ==================== 谁刚被处理过 ====================

#[derive(Clone, Copy, Default)]
struct Seen {
    /// 禁言到什么时候；到点之后才允许再禁一次。
    muted_until: Option<Instant>,
    /// 上一次提示是什么时候。
    last_notice: Option<Instant>,
}

#[derive(Default)]
struct Gate {
    seen: HashMap<(String, String), Seen>,
}

fn gate() -> MutexGuard<'static, Gate> {
    static GATE: OnceLock<Mutex<Gate>> = OnceLock::new();
    GATE.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Gate {
    fn seen(&self, group: &str, user: &str) -> Seen {
        self.seen
            .get(&(group.to_string(), user.to_string()))
            .copied()
            .unwrap_or_default()
    }

    fn mark_muted(&mut self, group: &str, user: &str, until: Instant) {
        self.cleanup();
        self.seen
            .entry((group.to_string(), user.to_string()))
            .or_default()
            .muted_until = Some(until);
    }

    fn mark_notice(&mut self, group: &str, user: &str) {
        self.cleanup();
        self.seen
            .entry((group.to_string(), user.to_string()))
            .or_default()
            .last_notice = Some(Instant::now());
    }

    fn cleanup(&mut self) {
        if self.seen.len() <= 4096 {
            return;
        }
        let now = Instant::now();
        self.seen.retain(|_, seen| {
            seen.muted_until.is_some_and(|until| until > now)
                || seen
                    .last_notice
                    .is_some_and(|at| now.duration_since(at) < Duration::from_hours(24))
        });
    }
}

/// 控制台配置表单里的中文名与先后。
pub const LABELS: &[(&str, &str)] = &[
    ("default_action", "默认动作"),
    ("mute_seconds", "默认禁言时长（秒）"),
    ("notice", "提示语"),
    ("notice_seconds", "提示留多久（秒，0 = 永久）"),
    ("exempt_staff", "群主、管理员不动手"),
    ("exempt", "豁免名单"),
    ("ignore_users", "不检测这些账号"),
    ("repeat_seconds", "同一个人的提示间隔（秒）"),
    ("managers", "谁能改本群规则"),
    ("channel", "生效范围"),
];

/// 控制台插件页上方「现在的情况」：只报数量，词表不在网页上展开。
pub fn live(ctx: &Context) -> serde_json::Value {
    let config = ctx.config.read().unwrap();
    if !crate::plugins::enabled(&config, Config::NAME) {
        return serde_json::json!({
            "lines": ["插件停着，现在不检测；规则仍在 data/keyword_guard/rules.json。"],
        });
    }
    let Some(book) = BOOK.peek() else {
        return serde_json::json!({ "lines": ["正在读取规则…"] });
    };
    let enabled = book.rules.iter().filter(|rule| rule.enabled).count();
    serde_json::json!({
        "lines": [
            format!(
                "{} 条规则，{} 条在用；其中全局 {} 条。",
                book.rules.len(),
                enabled,
                book.global().len()
            ),
            "规则在群里用「违禁词」维护；这一页改的是提示语、默认动作与豁免名单。".to_string(),
        ],
    })
}

pub fn option_labels() -> serde_json::Value {
    serde_json::json!({
        "default_action": {
            "alert": "仅提醒",
            "recall": "撤回消息",
            "mute": "禁言",
            "recall_mute": "撤回并禁言",
        },
        "managers": {
            "staff": "群主、群管理员与机器人管理员",
            "admin": "只有机器人管理员",
        },
    })
}

/// 提示语的文案编辑器：可用占位符与一段样例。
pub fn templates() -> serde_json::Value {
    serde_json::json!({
        "fields": {
            "notice": {
                "kind": "notice",
                "about": "检测到之后在群里说的话；留空就静默执行",
                "silent": "留空：群里一个字都不发，只写日志。",
            },
        },
        "placeholders": [
            { "key": "at", "label": "@ 违规的人", "hint": "写成 {at} 才会真的 @；不想要 @ 就删掉", "kinds": ["notice"] },
            { "key": "name", "label": "名字", "hint": "对方的群名片或昵称", "kinds": ["notice"] },
            { "key": "keyword", "label": "命中的词", "hint": "规则里对上的那个词；不想公开词表就别放", "kinds": ["notice"] },
            { "key": "action", "label": "处理结果", "hint": "如「消息已撤回，并禁言 10 分钟」", "kinds": ["notice"] },
            { "key": "duration", "label": "禁言时长", "hint": "只对禁言类动作用得到，如「10 分钟」", "kinds": ["notice"] },
            { "key": "group", "label": "群名", "hint": "这个群的名称", "kinds": ["notice"] },
        ],
        "samples": {
            "at": "@小林 ",
            "name": "小林",
            "keyword": "加微信",
            "action": "消息已撤回，并禁言 10 分钟",
            "duration": "10 分钟",
            "group": "手机交流群",
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use simd_json::prelude::ValueAsScalar;

    #[test]
    fn defaults_pass_check() {
        assert!(Config::default().check().is_ok());
    }

    #[test]
    fn notice_renders_mentions_and_placeholders() {
        let config = Config {
            notice: Notice::One("{at}请勿发{keyword}，{action}；{duration}；{group}".into()),
            ..Default::default()
        };
        let message = notice_message(&config, "42", "小明", "加微信", "消息已撤回", 600, "测试群");
        assert_eq!(message.0[0].type_, "at");
        assert_eq!(message.0[1].type_, "text");
        let text = message.0[1]
            .data
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(text, "请勿发加微信，消息已撤回；10 分钟；测试群");
    }

    #[test]
    fn notice_accepts_one_line_or_several() {
        let one: Config = toml::from_str("notice = \"已处理\"").unwrap();
        assert_eq!(one.notice.first(), Some("已处理"));
        let many: Config = toml::from_str("notice = [\"一\", \"二\"]").unwrap();
        assert_eq!(many.notice.list(), ["一", "二"]);
        assert!(!many.notice.is_silent());
        let silent: Config = toml::from_str("notice = []").unwrap();
        assert!(silent.notice.is_silent());
    }

    #[test]
    fn notice_without_at_has_no_mention() {
        let config = Config {
            notice: Notice::One("已处理".into()),
            ..Default::default()
        };
        let message = notice_message(&config, "42", "小明", "加微信", "已撤回", 0, "");
        assert_eq!(message.0.len(), 1);
        assert_eq!(message.0[0].type_, "text");
    }

    #[test]
    fn silent_and_permanent_lifetimes_read_back() {
        let silent = Config {
            notice: Notice::One(String::new()),
            ..Default::default()
        };
        assert!(notice_lifetime(&silent).contains("静默"));
        let forever = Config {
            notice_seconds: 0,
            ..Default::default()
        };
        assert_eq!(notice_lifetime(&forever), "永久保留");
    }
}
