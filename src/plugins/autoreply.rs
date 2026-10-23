//! 自动回复：群里有人问到设好的关键词，自动回一句（可以带图、表情，也可以是一整条合并转发）。
//!
//! 新人进群总是那几个问题——下载地址、群规、怎么用——管理员不必一遍遍手打。规则在
//! `data/autoreply/rules.json`（见 [`book`]），群主、管理员在群里用「自动回复 …」指令增删改，
//! 号主在控制台的规则面板里改，两边是同一份。
//!
//! 一条规则：
//!
//! - **触发词**：几个都行，任一对上就算；**怎么比**有精确 / 包含 / 开头 / 正则四种。比之前
//!   消息与触发词都去掉空格和标点、全角转半角、不分大小写，「怎么下载？」与「怎么 下载」是一句话。
//! - **回复**：几种说法随机一种；文字里可以插图（`{image:…}`）、表情与 `{at}` 这类占位符；
//!   也可以是群里引用来的一整条合并转发。长内容可以折成合并转发发，不刷屏。
//! - **范围**：只在某几个群，或所有群共用；共用的规则可以在某个群单独停用。
//!
//! 防误触是这个插件的重点，几道闸按顺序：
//!
//! 1. 包含匹配的触发词至少两个字，且只看 [`Config::contains_max_chars`] 字以内的消息——长消息
//!    顺口提到关键词多半不是在问；
//! 2. 以指令前缀开头的消息不回（交给指令），机器人自己和 `ignore_users` 里的账号不回；
//! 3. 可以设成只在 @ 机器人时回；
//! 4. 冷却：同一条规则在同一个群里隔 `cooldown_seconds` 才回第二次，同一个人隔
//!    `user_cooldown_seconds`；整个群每分钟最多回 `per_minute` 次；
//! 5. 几条规则同时对上只回一条（越精确越先，本群专用的先于共用的，触发词长的先于短的）。
//!
//! 还在冷却里的那句不算处理过，照常交给后面的插件。

mod book;
mod command;
mod panel;
mod reply;

pub(crate) use panel::{delete as panel_delete, overview as panel_overview, restore as panel_restore};
pub(crate) use panel::{live, save as panel_save, test as panel_test};

use crate::adapters::satori::LockedWriter;
use crate::command::match_word_command;
use crate::event::Context;
use crate::plugins::{ChannelConfig, PluginConfig, PluginError, get_config_or_default};
use book::{Asked, BOOK, Mode};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const LOG_TARGET: &str = "Plugin/AutoReply";

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    enabled: bool,
    /// 在哪些群生效；两边都空 = 账号所在的全部群。
    channel: ChannelConfig,
    /// 谁能在群里用指令增删改：staff = 群主、群管理员与机器人管理员；admin = 只有机器人管理员。
    managers: String,
    /// 指令「添加」的新规则默认怎么比：contains / exact / prefix。
    default_mode: String,
    /// 包含匹配只看这么多字以内的消息（去掉空格标点后数）；0 = 不限。
    contains_max_chars: u32,
    /// 同一条规则在同一个群里两次回复至少隔这么多秒；规则自己可以另设。
    cooldown_seconds: u64,
    /// 同一个人反复问同一个问题，隔这么多秒才再回他。
    user_cooldown_seconds: u64,
    /// 每个群每分钟最多自动回复几次；0 = 不限。
    per_minute: u32,
    /// 引用提问的那条消息来回（合并转发没法引用，照常发）。
    quote: bool,
    /// 以指令前缀开头的消息不回，交给指令。
    skip_commands: bool,
    /// 这些账号说话不回（比如群里别的机器人）。
    ignore_users: Vec<String>,
    /// 群里任何人发「问答」，列出本群能问的触发词。
    menu: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            channel: ChannelConfig::default(),
            managers: "staff".into(),
            default_mode: "contains".into(),
            contains_max_chars: 40,
            cooldown_seconds: 30,
            user_cooldown_seconds: 120,
            per_minute: 6,
            quote: true,
            skip_commands: true,
            ignore_users: Vec::new(),
            menu: true,
        }
    }
}

impl PluginConfig for Config {
    const NAME: &'static str = "autoreply";

    fn check(&self) -> Result<(), String> {
        if !matches!(self.managers.as_str(), "staff" | "admin") {
            return Err("managers 只能是 staff 或 admin".into());
        }
        if !matches!(self.default_mode.as_str(), "contains" | "exact" | "prefix") {
            return Err("default_mode 只能是 contains、exact 或 prefix".into());
        }
        if self.contains_max_chars > 500 {
            return Err("contains_max_chars 最多 500（0 = 不限）".into());
        }
        if self.cooldown_seconds > 86400 || self.user_cooldown_seconds > 86400 {
            return Err("冷却最多 86400 秒（一天）".into());
        }
        if self.per_minute > 60 {
            return Err("per_minute 最多 60（0 = 不限）".into());
        }
        Ok(())
    }
}

impl Config {
    fn default_mode(&self) -> Mode {
        Mode::parse(&self.default_mode).unwrap_or_default()
    }
}

pub fn init(_ctx: Context) -> BoxFuture<'static, Result<(), PluginError>> {
    Box::pin(async move {
        BOOK.preload().await;
        Ok(())
    })
}

pub fn handle(ctx: Context, writer: LockedWriter) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let Some(message) = ctx.as_message() else {
            return Ok(Some(ctx));
        };
        let config = get_config_or_default::<Config>(&ctx);
        let group = message.group_id().map(str::to_owned);
        let allowed = match &group {
            Some(group) => config.channel.allows_group(group),
            None => config.channel.allows_direct(&[message.channel_id(), message.user_id()]),
        };
        // 自己发出去的回声不理（只认号主亲手打的指令），免得回复触发回复。
        let me = ctx.bot.self_id();
        if !allowed || (message.user_id() == me && !message.is_manual_self()) {
            return Ok(Some(ctx));
        }
        if let Some(matched) = match_word_command(&ctx, "自动回复")
            && command::run(&ctx, &writer, &config, group.as_deref(), matched).await?
        {
            return Ok(None);
        }
        if config.menu
            && let Some(matched) = match_word_command(&ctx, "问答")
            && crate::command::extract_text_arg(&matched.args).is_empty()
        {
            command::menu(&ctx, &writer, group.as_deref()).await?;
            return Ok(None);
        }
        if message.user_id() != me && answer(&ctx, &writer, &config, group.as_deref()).await? {
            return Ok(None);
        }
        Ok(Some(ctx))
    })
}

/// 这条消息有规则能回就回。返回 true 表示回了（这条消息就算处理过了）。
async fn answer(ctx: &Context, writer: &LockedWriter, config: &Config, group: Option<&str>) -> Result<bool, PluginError> {
    let Some(message) = ctx.as_message() else {
        return Ok(false);
    };
    let user = message.user_id();
    let text = message.text();
    if user.is_empty() || text.trim().is_empty() || config.ignore_users.iter().any(|id| id == user) {
        return Ok(false);
    }
    let bodies = crate::command::spoken_bodies(text);
    if config.skip_commands {
        let prefixes = crate::command::get_prefixes(ctx);
        if bodies.iter().any(|body| prefixes.iter().any(|p| !p.is_empty() && body.starts_with(p.as_str()))) {
            return Ok(false);
        }
    }
    let at_bot = message.mentions().names(&ctx.bot.self_id());
    let max = config.contains_max_chars;
    let found = BOOK
        .read(|book| {
            bodies.iter().find_map(|body| {
                book::best(&book.rules, &Asked::new(body, at_bot, group), max).map(|(rule, keyword)| (rule.clone(), keyword))
            })
        })
        .await;
    let Some((rule, keyword)) = found else {
        return Ok(false);
    };
    let place = group.map_or_else(|| format!("private:{user}"), str::to_owned);
    if let Some(why) = throttle(&place, user, &rule, config) {
        debug!(target: LOG_TARGET, "{place} 的「{keyword}」对上了 #{}，但{why}，这次不回", rule.id);
        return Ok(false);
    }
    let choices: Vec<&book::Reply> = rule.replies.iter().collect();
    let Some(chosen) = choices.get(rand::random_range(0..choices.len().max(1))).copied() else {
        return Ok(false);
    };
    let asker = reply::Asker { user_id: user, name: message.sender_name(), group, keyword: &keyword };
    let quote = config.quote.then(|| message.message_id()).filter(|id| !id.is_empty());
    let sent = reply::send(ctx, writer, &rule, chosen, &asker, quote).await?;
    if sent {
        info!(target: LOG_TARGET, "{place} 的 {}（{user}）问到「{keyword}」，按 #{} 回了", asker.name, rule.id);
        BOOK.with(|book| {
            if let Some(found) = book.rules.iter_mut().find(|r| r.id == rule.id) {
                found.hits += 1;
                found.last_hit = book::now();
            }
        })
        .await;
    }
    Ok(sent)
}

/// 冷却与限流。放行时顺手记下这一次；拦下时说为什么（只进调试日志）。
#[derive(Default)]
struct Gate {
    rules: HashMap<(String, u32), Instant>,
    users: HashMap<(String, String, u32), Instant>,
    minute: HashMap<String, VecDeque<Instant>>,
}

fn throttle(place: &str, user: &str, rule: &book::Rule, config: &Config) -> Option<&'static str> {
    static GATE: OnceLock<Mutex<Gate>> = OnceLock::new();
    let mut gate = GATE.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let longest = Duration::from_secs(config.cooldown_seconds.max(config.user_cooldown_seconds).max(rule.cooldown_seconds.unwrap_or(0)).max(60));
    if gate.rules.len() + gate.users.len() > 4096 {
        gate.rules.retain(|_, at| at.elapsed() < longest);
        gate.users.retain(|_, at| at.elapsed() < longest);
    }
    let cooldown = Duration::from_secs(rule.cooldown_seconds.unwrap_or(config.cooldown_seconds));
    let rule_key = (place.to_string(), rule.id);
    if gate.rules.get(&rule_key).is_some_and(|at| at.elapsed() < cooldown) {
        return Some("这条规则在本群还在冷却");
    }
    let user_key = (place.to_string(), user.to_string(), rule.id);
    if gate.users.get(&user_key).is_some_and(|at| at.elapsed() < Duration::from_secs(config.user_cooldown_seconds)) {
        return Some("同一个人刚问过");
    }
    let window = gate.minute.entry(place.to_string()).or_default();
    window.retain(|at| at.elapsed() < Duration::from_mins(1));
    if config.per_minute > 0 && window.len() >= config.per_minute as usize {
        return Some("这一分钟回得够多了");
    }
    window.push_back(Instant::now());
    gate.rules.insert(rule_key, Instant::now());
    gate.users.insert(user_key, Instant::now());
    None
}

/// 控制台表单里字段的中文名。
pub(crate) const LABELS: &[(&str, &str)] = &[
    ("channel", "生效范围"),
    ("managers", "谁能用指令管理"),
    ("default_mode", "指令添加时默认怎么比"),
    ("contains_max_chars", "包含匹配的字数上限"),
    ("cooldown_seconds", "同一条规则的冷却（秒）"),
    ("user_cooldown_seconds", "同一个人的冷却（秒）"),
    ("per_minute", "每群每分钟最多回几次"),
    ("quote", "引用提问的消息"),
    ("skip_commands", "指令不回"),
    ("ignore_users", "不回这些账号"),
    ("menu", "「问答」列出常见问题"),
];

pub(crate) fn option_labels() -> serde_json::Value {
    serde_json::json!({
        "managers": { "staff": "群主、群管理员与机器人管理员", "admin": "只有机器人管理员" },
        "default_mode": { "contains": "包含（句子里有触发词就回）", "exact": "精确（整句一样才回）", "prefix": "开头（以触发词开头就回）" },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_pass_check() {
        assert!(Config::default().check().is_ok());
    }

    #[test]
    fn throttle_holds_repeats_and_caps_a_minute() {
        let config = Config { cooldown_seconds: 30, user_cooldown_seconds: 120, per_minute: 2, ..Default::default() };
        let rule = |id| book::Rule { id, ..Default::default() };
        let place = "throttle-test";
        assert!(throttle(place, "a", &rule(1), &config).is_none());
        assert!(throttle(place, "b", &rule(1), &config).is_some(), "同一条规则冷却中");
        assert!(throttle(place, "a", &rule(2), &config).is_none());
        assert!(throttle(place, "c", &rule(3), &config).is_some(), "一分钟两次用完了");
        let free = book::Rule { id: 9, cooldown_seconds: Some(0), ..Default::default() };
        let loose = Config { per_minute: 0, user_cooldown_seconds: 0, ..config };
        assert!(throttle("throttle-free", "a", &free, &loose).is_none());
        assert!(throttle("throttle-free", "a", &free, &loose).is_none(), "规则自己写 0 = 不冷却");
    }
}
