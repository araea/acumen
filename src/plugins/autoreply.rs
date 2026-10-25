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
//! 1. 包含匹配的触发词至少两个字，且只看 [`Limits::contains_max_chars`] 字以内的消息——长消息
//!    顺口提到关键词多半不是在问；
//! 2. 以指令前缀开头的消息交给指令，除非对上的触发词自己就带着这个前缀（「/菜单」）；机器人
//!    回声与 `ignore_users` 里的账号不回；号主手打的话照常回（`owner_triggers` 可关）；
//! 3. 可以设成只在 @ 机器人时回；
//! 4. 冷却：同一条规则在同一个群里隔 `cooldown_seconds` 才回第二次，同一个人隔
//!    `user_cooldown_seconds`；整个群每分钟最多回 `per_minute` 次；
//! 5. 几条规则同时对上只回一条（越精确越先，本群专用的先于共用的，触发词长的先于短的）。
//!
//! 还在冷却里的那句不算处理过，照常交给后面的插件。「试一句」（[`trial`]）把这几道闸连同
//! 插件开关、生效范围一起过一遍，说清楚一句话为什么回、为什么不回。

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
use book::{Asked, BOOK, Rule};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

const LOG_TARGET: &str = "Plugin/AutoReply";

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    enabled: bool,
    /// 在哪些群生效；两边都空 = 账号所在的全部群。
    channel: ChannelConfig,
    /// 引用提问的那条消息来回（合并转发没法引用，照常发）。
    quote: bool,
    /// 号主（机器人登录的就是这个号）亲手发的话也回：自己在群里试一句就能看到效果。
    owner_triggers: bool,
    /// 群里任何人发「问答」，列出本群能问的触发词。
    menu: bool,
    /// 谁能在群里用指令增删改：staff = 群主、群管理员与机器人管理员；admin = 只有机器人管理员。
    managers: String,
    /// 这些账号说话不回（比如群里别的机器人）。
    ignore_users: Vec<String>,
    /// 防误触与限流。
    limits: Limits,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            channel: ChannelConfig::default(),
            quote: true,
            owner_triggers: true,
            menu: true,
            managers: "staff".into(),
            ignore_users: Vec::new(),
            limits: Limits::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Limits {
    /// 包含匹配只看这么多字以内的消息（去掉空格标点后数）；0 = 不限。
    contains_max_chars: u32,
    /// 同一条规则在同一个群里两次回复至少隔这么多秒；规则自己可以另设。
    cooldown_seconds: u64,
    /// 同一个人反复问同一个问题，隔这么多秒才再回他。
    user_cooldown_seconds: u64,
    /// 每个群每分钟最多自动回复几次；0 = 不限。
    per_minute: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self { contains_max_chars: 40, cooldown_seconds: 30, user_cooldown_seconds: 120, per_minute: 6 }
    }
}

/// 1.25.0 之前限流几项摊在顶层、另有两项后来删了：挪进 `[autoreply.limits]`，删掉的去掉。
const MOVED: [&str; 4] = ["contains_max_chars", "cooldown_seconds", "user_cooldown_seconds", "per_minute"];
const DROPPED: [&str; 2] = ["default_mode", "skip_commands"];

impl PluginConfig for Config {
    const NAME: &'static str = "autoreply";

    fn check(&self) -> Result<(), String> {
        if !matches!(self.managers.as_str(), "staff" | "admin") {
            return Err("managers 只能是 staff 或 admin".into());
        }
        let limits = &self.limits;
        if limits.contains_max_chars > 500 {
            return Err("contains_max_chars 最多 500（0 = 不限）".into());
        }
        if limits.cooldown_seconds > 86400 || limits.user_cooldown_seconds > 86400 {
            return Err("冷却最多 86400 秒（一天）".into());
        }
        if limits.per_minute > 60 {
            return Err("per_minute 最多 60（0 = 不限）".into());
        }
        Ok(())
    }

    fn migrate(own: &mut toml::Table) -> bool {
        let mut changed = false;
        for key in MOVED {
            if let Some(value) = own.remove(key) {
                let limits = own.entry("limits").or_insert_with(|| toml::Value::Table(toml::Table::new()));
                if let toml::Value::Table(limits) = limits {
                    limits.entry(key).or_insert(value);
                }
                changed = true;
            }
        }
        for key in DROPPED {
            changed |= own.remove(key).is_some();
        }
        changed
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
        // 自己发出去的回声不理（只认号主亲手打的），免得回复触发回复。
        let me = ctx.bot.self_id();
        let owner = message.user_id() == me;
        if !allowed || (owner && !message.is_manual_self()) {
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
        if (!owner || config.owner_triggers) && answer(&ctx, &writer, &config, group.as_deref()).await? {
            return Ok(None);
        }
        Ok(Some(ctx))
    })
}

/// 消息以哪个指令前缀开头（没配前缀、或不是指令的样子都是 `None`）。
fn command_prefix<'p>(prefixes: &'p [String], body: &str) -> Option<&'p str> {
    prefixes.iter().map(String::as_str).find(|p| !p.is_empty() && body.starts_with(p))
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
    let prefixes = crate::command::get_prefixes(ctx);
    let at_bot = message.mentions().names(&ctx.bot.self_id());
    let max = config.limits.contains_max_chars;
    let found = BOOK
        .read(|book| {
            bodies.iter().find_map(|body| {
                let asked = Asked::new(body, at_bot, group).command(command_prefix(&prefixes, body.trim()));
                book::best(&book.rules, &asked, max).map(|(rule, keyword)| (rule.clone(), keyword))
            })
        })
        .await;
    let Some((rule, keyword)) = found else {
        return Ok(false);
    };
    let place = group.map_or_else(|| format!("private:{user}"), str::to_owned);
    if let Some(hold) = gate().hold(&place, user, &rule, config) {
        debug!(target: LOG_TARGET, "{place} 的「{keyword}」对上了 #{}，但{}，这次不回", rule.id, hold.say());
        return Ok(false);
    }
    gate().pass(&place, user, &rule, config);
    let Some(chosen) = rule.replies.get(rand::random_range(0..rule.replies.len().max(1))) else {
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

// ==================== 冷却与限流 ====================

/// 各个群、各条规则、各个人最近一次回复的时刻。
#[derive(Default)]
struct Gate {
    rules: HashMap<(String, u32), Instant>,
    users: HashMap<(String, String, u32), Instant>,
    minute: HashMap<String, VecDeque<Instant>>,
}

/// 被哪道闸拦下、还要等多久。
#[derive(Debug, PartialEq)]
enum Hold {
    Rule(Duration),
    User(Duration),
    Minute(Duration),
}

impl Hold {
    fn say(&self) -> String {
        let wait = |left: &Duration| crate::clock::duration_text(left.as_secs().max(1));
        match self {
            Self::Rule(left) => format!("这条规则在这里刚回过，{}后才会再回", wait(left)),
            Self::User(left) => format!("同一个人刚问过，{}后才会再回他", wait(left)),
            Self::Minute(left) => format!("这一分钟回得够多了，{}后再回", wait(left)),
        }
    }
}

fn gate() -> MutexGuard<'static, Gate> {
    static GATE: OnceLock<Mutex<Gate>> = OnceLock::new();
    GATE.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 还剩多久：`gap` 过去了就是 `None`。
fn left(at: Option<&Instant>, gap: Duration) -> Option<Duration> {
    at.and_then(|at| gap.checked_sub(at.elapsed())).filter(|left| !left.is_zero())
}

impl Gate {
    /// 这一次会不会被拦下（只看，不记）。
    fn hold(&mut self, place: &str, user: &str, rule: &Rule, config: &Config) -> Option<Hold> {
        let limits = &config.limits;
        let cooldown = Duration::from_secs(rule.cooldown_seconds.unwrap_or(limits.cooldown_seconds));
        if let Some(wait) = left(self.rules.get(&(place.to_string(), rule.id)), cooldown) {
            return Some(Hold::Rule(wait));
        }
        let user_key = (place.to_string(), user.to_string(), rule.id);
        if let Some(wait) = left(self.users.get(&user_key), Duration::from_secs(limits.user_cooldown_seconds)) {
            return Some(Hold::User(wait));
        }
        let window = self.minute.entry(place.to_string()).or_default();
        window.retain(|at| at.elapsed() < Duration::from_mins(1));
        if limits.per_minute > 0 && window.len() >= limits.per_minute as usize {
            return Some(Hold::Minute(left(window.front(), Duration::from_mins(1)).unwrap_or_default()));
        }
        None
    }

    /// 放行了：记下这一次。
    fn pass(&mut self, place: &str, user: &str, rule: &Rule, config: &Config) {
        let limits = &config.limits;
        let longest = Duration::from_secs(
            limits.cooldown_seconds.max(limits.user_cooldown_seconds).max(rule.cooldown_seconds.unwrap_or(0)).max(60),
        );
        if self.rules.len() + self.users.len() > 4096 {
            self.rules.retain(|_, at| at.elapsed() < longest);
            self.users.retain(|_, at| at.elapsed() < longest);
            self.minute.retain(|_, window| window.back().is_some_and(|at| at.elapsed() < Duration::from_mins(1)));
        }
        let now = Instant::now();
        self.minute.entry(place.to_string()).or_default().push_back(now);
        self.rules.insert((place.to_string(), rule.id), now);
        self.users.insert((place.to_string(), user.to_string(), rule.id), now);
    }
}

// ==================== 试一句 ====================

/// 「试一句」的结论：会不会回、由哪条回，再加几句挡在前面的情况（插件关着、冷却里……）。
pub(crate) struct Trial {
    /// 对上的规则与触发词。
    pub hit: Option<(u32, String)>,
    pub message: String,
    /// 规则对上了、但眼下还是回不出去的原因。
    pub blocks: Vec<String>,
    /// 不挡这一句、但值得知道的事。
    pub notes: Vec<String>,
}

/// 这句话发在 `group`（`None` = 私聊）会怎样。控制台的「试一句」与群里的「自动回复 试」共用：
/// 规则之外，插件开关、生效范围、指令前缀、冷却这些闸也一并说清楚，「明明设了却不回」在这儿就看得出来。
pub(crate) async fn trial(ctx: &Context, config: &Config, text: &str, group: Option<&str>, at_bot: bool) -> Trial {
    let mut trial = Trial { hit: None, message: String::new(), blocks: Vec::new(), notes: Vec::new() };
    let text = text.trim();
    if text.is_empty() {
        return trial;
    }
    let prefixes = crate::command::get_prefixes(ctx);
    let prefix = command_prefix(&prefixes, text);
    let max = config.limits.contains_max_chars;
    let Some((rule, keyword)) = lookup(text, at_bot, group, prefix, max).await else {
        trial.message = if let Some(prefix) = prefix
            && let Some((rule, _)) = lookup(text, at_bot, group, None, max).await
        {
            format!("不会回：这句以指令前缀「{prefix}」开头，交给指令。想让 #{}{} 接这种说法，把触发词写成「{prefix}…」。", rule.id, rule.title())
        } else if let Some((rule, _)) = lookup(text, true, group, prefix, max).await.filter(|_| !at_bot) {
            format!("直接说不会回；@ 机器人再说，会由 #{}{} 回复（它设了只在 @ 时回）。", rule.id, rule.title())
        } else if let Some(why) = BOOK.read(|b| near_miss(&b.rules, &Asked::new(text, true, group).command(prefix), max)).await {
            why
        } else {
            let folded = crate::text::fold(text).chars().count();
            if max > 0 && folded > max as usize {
                format!("没有规则会回这句：去掉空格标点有 {folded} 字，超过包含匹配的 {max} 字上限（精确、开头、正则不受这个限制）。")
            } else {
                "没有规则会回这句。".into()
            }
        };
        return trial;
    };
    trial.message = format!("会由 #{}{} 回复：{}匹配，对上了「{keyword}」。", rule.id, rule.title(), rule.mode.label());
    trial.hit = Some((rule.id, keyword));
    if !crate::plugins::enabled(&ctx.config.read().unwrap(), Config::NAME) {
        trial.blocks.push("但插件总开关关着，现在一句都不会回——打开「启用」才生效。".into());
    }
    let allowed = match group {
        Some(g) if g != "*" => config.channel.allows_group(g),
        Some(_) => true,
        None => config.channel.allows_direct(&[]),
    };
    if !allowed {
        trial.blocks.push("但这个群不在插件的生效范围里（见配置里的「生效范围」）。".into());
    }
    // 冷却按真实的群算；空账号不会撞上「同一个人」那道闸。
    if let Some(g) = group.filter(|g| *g != "*")
        && let Some(hold) = gate().hold(g, "", &rule, config)
    {
        trial.blocks.push(format!("{}。", hold.say()));
    }
    if !config.owner_triggers {
        trial.notes.push("号主（机器人登录的这个号）自己发的不会触发：在群里试要换个号，或者打开「号主自己说也回」。".into());
    }
    trial
}

async fn lookup(text: &str, at_bot: bool, group: Option<&str>, prefix: Option<&str>, max: u32) -> Option<(Rule, String)> {
    let asked = Asked::new(text, at_bot, group).command(prefix);
    BOOK.read(|b| book::best(&b.rules, &asked, max).map(|(rule, keyword)| (rule.clone(), keyword))).await
}

/// 没有规则会回时，找一条「本来对得上」的说明为什么：停用了、只在别的群、私聊不回。
fn near_miss(rules: &[Rule], asked: &Asked, max: u32) -> Option<String> {
    let rule = rules.iter().find(|rule| rule.hit(asked, max).is_some())?;
    let name = format!("#{}{}", rule.id, rule.title());
    Some(match asked.group {
        _ if !rule.enabled => format!("{name} 对得上，但它停用着。"),
        Some(g) if rule.exclude.iter().any(|x| x == g) => format!("{name} 对得上，但它在这个群停用了。"),
        Some("*") => format!("{name} 对得上，但它只在指定的几个群回；在上面换成那个群再试。"),
        Some(_) => format!("{name} 对得上，但它只在别的群回。"),
        None if rule.scoped() => format!("{name} 对得上，但它只在指定的群回，私聊不回。"),
        None => format!("{name} 对得上，但私聊里不回（卡片里打开「私聊里也回」）。"),
    })
}

/// 控制台表单里字段的中文名，也是表单里的先后。
pub(crate) const LABELS: &[(&str, &str)] = &[
    ("quote", "引用提问的消息"),
    ("owner_triggers", "号主自己说也回"),
    ("menu", "「问答」列出常见问题"),
    ("managers", "谁能用指令管理"),
    ("ignore_users", "不回这些账号"),
    ("channel", "生效范围"),
    ("limits", "防刷屏"),
    ("limits.contains_max_chars", "包含匹配的字数上限"),
    ("limits.cooldown_seconds", "同一条规则的冷却（秒）"),
    ("limits.user_cooldown_seconds", "同一个人的冷却（秒）"),
    ("limits.per_minute", "每群每分钟最多回几次"),
];

pub(crate) fn option_labels() -> serde_json::Value {
    serde_json::json!({
        "managers": { "staff": "群主、群管理员与机器人管理员", "admin": "只有机器人管理员" },
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
    fn migrate_moves_limits_and_drops_retired_keys() {
        let mut own: toml::Table =
            toml::from_str("enabled = true\ncooldown_seconds = 10\ndefault_mode = \"exact\"\nskip_commands = true\n").unwrap();
        assert!(Config::migrate(&mut own));
        assert_eq!(own["limits"]["cooldown_seconds"].as_integer(), Some(10));
        assert!(!own.contains_key("default_mode") && !own.contains_key("skip_commands"));
        assert!(!Config::migrate(&mut own), "第二次没有可改的");
    }

    #[test]
    fn gate_holds_repeats_and_caps_a_minute() {
        let config = Config { limits: Limits { cooldown_seconds: 30, user_cooldown_seconds: 120, per_minute: 2, ..Default::default() }, ..Default::default() };
        let rule = |id| Rule { id, ..Default::default() };
        let place = "gate-test";
        let mut gate = Gate::default();
        let mut try_once = |user: &str, rule: &Rule, config: &Config| {
            let hold = gate.hold(place, user, rule, config);
            if hold.is_none() {
                gate.pass(place, user, rule, config);
            }
            hold
        };
        assert!(try_once("a", &rule(1), &config).is_none());
        assert!(matches!(try_once("b", &rule(1), &config), Some(Hold::Rule(_))), "同一条规则冷却中");
        assert!(try_once("a", &rule(2), &config).is_none());
        assert!(matches!(try_once("c", &rule(3), &config), Some(Hold::Minute(_))), "一分钟两次用完了");
        let free = Rule { id: 9, cooldown_seconds: Some(0), ..Default::default() };
        let loose = Config { limits: Limits { per_minute: 0, user_cooldown_seconds: 0, ..config.limits.clone() }, ..config };
        let mut gate = Gate::default();
        for _ in 0..2 {
            assert!(gate.hold("gate-free", "a", &free, &loose).is_none(), "规则自己写 0 = 不冷却");
            gate.pass("gate-free", "a", &free, &loose);
        }
    }
}
