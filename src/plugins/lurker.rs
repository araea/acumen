//! 潜水清理：找出群里长期不说话的人，管理员看过名单、点了头，再慢慢请出去。
//!
//! 群规要求成员保持活跃的群，管理员原先只能一个个手动踢，或用 QQ 自带的「不活跃成员」筛选——
//! 后者看不到具体多久没说话，也没有豁免、提醒和批量的节奏控制。这个插件把整件事做完：
//!
//! 1. **查**（[`roster`]）：拉全群名单，对上「多久没说话」，按口径（[`judge::Rule`]）逐个判定，
//!    排成带序号的名单。群主、管理员、号主、豁免名单、有群头衔的人不会进名单，进群不满天数的
//!    新人也不算；
//! 2. **看**（[`sheet`]）：名单出成长图（宽版、字小一档、一人一行，按沉默时长分节），装进合并转发，
//!    另附能搜、能复制号码的文字版；控制台里也能看、能勾选；
//! 3. **移出**（[`purge`]）：管理员按序号挑人，回「确认」才动手；低速分批，中途冒泡的留下，
//!    做完以名单为准报结果。也可以先 @ 一下打个招呼（[`warn`]）；
//! 4. **自动**（[`auto`]，默认关）：只管点了名的群，先提醒、宽限期满再移出，人数异常时不动手。
//!
//! **最要紧的一条：不冤枉人。** 机器人离线那几天群里说过的话它收不到，回来一查，那几天
//! 说过话的人就成了「潜水」。所以「多久没说话」先看 QQ 自己记的最后发言（知弦 0.39 起，
//! 机器人不在线的日子它也记着），再拿机器人亲眼看到的核一遍；QQ 不给、或给的对不上时，
//! 只在机器人的记录一直没断的那段里下结论（[`watch`] 记下机器人哪些时候在收消息），
//! 更早就沉默的人列成「待观察」——不编号，序号区间也就圈不到他们。

mod auto;
mod command;
mod judge;
mod panel;
mod purge;
mod roster;
mod sheet;
mod warn;
mod watch;

pub(crate) use panel::{exempt as panel_exempt, job as panel_job, kick as panel_kick, live, overview as panel_overview};
pub(crate) use panel::{scan as panel_scan, stop as panel_stop, warn as panel_warn};

use crate::adapters::satori::LockedWriter;
use crate::event::{Context, EventType};
use crate::plugins::{ChannelConfig, PluginConfig, PluginError, get_config_or_default};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use simd_json::derived::ValueObjectAccessAsScalar;
use std::collections::BTreeMap;

const LOG_TARGET: &str = "Plugin/Lurker";

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    enabled: bool,
    /// 在哪些群能用；两边都空 = 全部群。
    channel: ChannelConfig,
    /// 多少天没在群里说话算潜水。进群不满这么多天的新人不算。
    days: u32,
    /// 这段时间里至少说几句才算活跃；1 = 说过话就行。条数只数机器人收到的消息。
    min_messages: u32,
    /// 新人进群满这么多天还一句没说，也算潜水（0 = 不单独看新人）。
    newcomer_days: u32,
    /// 有群头衔的人不动。默认关：不少群的头衔是群友自己领的（「我要头衔」），不代表群主看重。
    protect_titled: bool,
    /// 群等级不低于这个数的人不动（0 = 不看等级）。
    protect_level: u32,
    /// 这些账号永远不进名单（小号、别的机器人）。群主、管理员、号主本来就不动。
    exempt: Vec<String>,
    /// 谁能用：staff = 群主、群管理员与机器人管理员；admin = 只有机器人管理员。
    managers: String,
    /// 移出后不许再申请加群。
    permanent: bool,
    list: ListConfig,
    pace: Pace,
    warn: Warn,
    auto: Auto,
    /// 每个群单独的口径。
    groups: BTreeMap<String, GroupRule>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            channel: ChannelConfig::default(),
            days: 30,
            min_messages: 1,
            newcomer_days: 0,
            protect_titled: false,
            protect_level: 0,
            exempt: Vec::new(),
            managers: "staff".into(),
            permanent: false,
            list: ListConfig::default(),
            pace: Pace::default(),
            warn: Warn::default(),
            auto: Auto::default(),
            groups: BTreeMap::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct ListConfig {
    /// 名单出成长图（关掉、或出图失败时发文字）。
    image: bool,
    /// 每张图最多几行，长了切成几张。
    rows_per_image: u32,
    /// 图上带头像（第一次要下载，最多等十几秒）。
    avatars: bool,
}

impl Default for ListConfig {
    fn default() -> Self {
        Self { image: true, rows_per_image: 50, avatars: true }
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Pace {
    /// 两次移出之间隔几秒（上下浮动四分之一）。
    interval_seconds: u32,
    /// 每移出这么多人歇一会儿。
    burst: u32,
    /// 歇几秒。
    rest_seconds: u32,
    /// 一次确认最多移出几人。
    max_per_run: u32,
}

impl Default for Pace {
    fn default() -> Self {
        Self { interval_seconds: 4, burst: 15, rest_seconds: 40, max_per_run: 100 }
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Warn {
    /// 提醒之后宽限几天。
    grace_days: u32,
    /// 提醒的话，几种说法随机一种。
    text: Vec<String>,
}

pub(crate) const WARN_TEXT: &str =
    "{at}\n你们在群里已经 {days} 天以上没说话了。按群规，{deadline}前还没冒泡的会被移出群聊——说句话就好，不用多～";

impl Default for Warn {
    fn default() -> Self {
        Self { grace_days: 3, text: vec![WARN_TEXT.into()] }
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Auto {
    /// 自动清理。默认关：打开后只管下面点了名的群。
    enabled: bool,
    groups: Vec<String>,
    /// 每周哪几天发新一轮提醒（1 = 周一 … 7 = 周日）。
    weekdays: Vec<u32>,
    /// 每天几点看一遍（HH:MM，北京时间）。
    time: String,
    /// 先提醒、宽限期满再移出；关掉就是到日子直接移出。
    warn: bool,
    /// 每个群一次最多移出几人。
    max_kicks: u32,
    /// 查出来的人超过全群的这个百分比就不动手，私聊告诉号主。
    max_ratio: u32,
    /// 做完在群里说一声。
    announce: bool,
    announce_text: Vec<String>,
}

pub(crate) const ANNOUNCE_TEXT: &str = "按群规，{count} 位 {days} 天以上没说话的成员已移出群聊：{names}。";

impl Default for Auto {
    fn default() -> Self {
        Self {
            enabled: false,
            groups: Vec::new(),
            weekdays: vec![1],
            time: "20:00".into(),
            warn: true,
            max_kicks: 30,
            max_ratio: 15,
            announce: true,
            announce_text: vec![ANNOUNCE_TEXT.into()],
        }
    }
}

/// 一个群自己的口径：没写的照全局。
#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct GroupRule {
    #[serde(skip_serializing_if = "Option::is_none")]
    days: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_messages: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    newcomer_days: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exempt: Option<Vec<String>>,
}

impl PluginConfig for Config {
    const NAME: &'static str = "lurker";

    fn check(&self) -> Result<(), String> {
        if !matches!(self.managers.as_str(), "staff" | "admin") {
            return Err("managers 只能是 staff 或 admin".into());
        }
        let days = |name: &str, value: u32| -> Result<(), String> {
            if (1..=3650).contains(&value) { Ok(()) } else { Err(format!("{name} 要在 1 到 3650 之间")) }
        };
        days("days", self.days)?;
        if self.min_messages == 0 || self.min_messages > 1000 {
            return Err("min_messages 要在 1 到 1000 之间（1 = 说过话就行）".into());
        }
        if self.newcomer_days > 3650 {
            return Err("newcomer_days 最多 3650（0 = 不单独看新人）".into());
        }
        if !(10..=200).contains(&self.list.rows_per_image) {
            return Err("list.rows_per_image 要在 10 到 200 之间".into());
        }
        let pace = &self.pace;
        if !(2..=600).contains(&pace.interval_seconds) {
            return Err("pace.interval_seconds 要在 2 到 600 之间：太快容易被 QQ 风控".into());
        }
        if pace.burst > 200 || pace.rest_seconds > 3600 {
            return Err("pace.burst 最多 200，pace.rest_seconds 最多 3600".into());
        }
        if !(1..=1000).contains(&pace.max_per_run) {
            return Err("pace.max_per_run 要在 1 到 1000 之间".into());
        }
        if self.warn.grace_days > 30 {
            return Err("warn.grace_days 最多 30".into());
        }
        let auto = &self.auto;
        if crate::clock::parse_clock(&auto.time).is_none() {
            return Err("auto.time 写成 HH:MM，比如 20:00".into());
        }
        if auto.weekdays.iter().any(|d| !(1..=7).contains(d)) {
            return Err("auto.weekdays 只能是 1（周一）到 7（周日）".into());
        }
        if !(1..=200).contains(&auto.max_kicks) {
            return Err("auto.max_kicks 要在 1 到 200 之间".into());
        }
        if !(1..=100).contains(&auto.max_ratio) {
            return Err("auto.max_ratio 要在 1 到 100 之间（百分比）".into());
        }
        for (group, own) in &self.groups {
            if let Some(value) = own.days {
                days(&format!("groups.{group}.days"), value)?;
            }
            if own.min_messages.is_some_and(|n| n == 0 || n > 1000) {
                return Err(format!("groups.{group}.min_messages 要在 1 到 1000 之间"));
            }
        }
        Ok(())
    }
}

/// 日期说成人话：今年的写「10 月 3 日」，往年的带上年份。
pub(crate) fn date(at: i64) -> String {
    use chrono::{Datelike, TimeZone};
    let tz = crate::clock::beijing();
    let Some(day) = tz.timestamp_opt(at, 0).single() else {
        return String::new();
    };
    if day.year() == crate::clock::beijing_now().year() {
        format!("{} 月 {} 日", day.month(), day.day())
    } else {
        format!("{} 年 {} 月 {} 日", day.year(), day.month(), day.day())
    }
}

pub fn init(ctx: Context) -> BoxFuture<'static, Result<(), PluginError>> {
    Box::pin(async move {
        static STARTED: std::sync::Once = std::sync::Once::new();
        let mut first = false;
        STARTED.call_once(|| first = true);
        if !first {
            return Ok(());
        }
        let interrupted = purge::recover().await;
        tokio::spawn(background(ctx, interrupted));
        Ok(())
    })
}

/// 常驻的一条循环：每五分钟打一次在线卡，每分钟看一眼自动清理；连上账号后先交代上次没做完的。
async fn background(ctx: Context, mut interrupted: Vec<(String, usize)>) {
    let mut minute = tokio::time::interval(std::time::Duration::from_mins(1));
    minute.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut round: u64 = 0;
    loop {
        minute.tick().await;
        let config = get_config_or_default::<Config>(&ctx);
        if !config.enabled {
            continue;
        }
        if round.is_multiple_of(5) {
            watch::tick(&ctx).await;
        }
        round += 1;
        let Some((ctx, writer)) = link(&ctx) else {
            continue;
        };
        if !interrupted.is_empty() {
            for (group, left) in std::mem::take(&mut interrupted) {
                tell_interrupted(&ctx, &writer, &group, left).await;
            }
        }
        auto::tick(&ctx, &writer).await;
    }
}

/// 拿一条连着的线路：后台任务没有事件可回，借现成的登录发消息、调接口。
fn link(base: &Context) -> Option<(Context, LockedWriter)> {
    let (bot, writer) = crate::adapters::satori::logins().into_iter().next()?;
    let mut ctx = base.clone();
    ctx.bot = bot;
    Some((ctx, writer))
}

async fn tell_interrupted(ctx: &Context, writer: &LockedWriter, group: &str, left: usize) {
    let prefix = crate::command::get_prefixes(ctx).into_iter().next().unwrap_or_default();
    let text = format!("上次的潜水清理被打断了（机器人重启），还剩 {left} 人没处理。发「{prefix}潜水 继续」接着移出，或「{prefix}潜水 停」作罢。");
    let message = crate::message::Message::new().text(text);
    if let Err(error) = crate::adapters::satori::send_msg(ctx, writer.clone(), Some(group), None, message).await {
        warn!(target: LOG_TARGET, "群 {group} 没能说上次的清理被打断了：{error}");
    }
}

pub fn handle(ctx: Context, writer: LockedWriter) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let EventType::Satori(raw) = &ctx.event else {
            return Ok(Some(ctx));
        };
        let Some(group) = raw.get_str("group_id").filter(|id| !id.is_empty()).map(str::to_owned) else {
            return Ok(Some(ctx));
        };
        let user = raw.get_str("user_id").unwrap_or("").to_string();
        let me = ctx.bot.self_id();
        match raw.get_str("satori_type").unwrap_or("") {
            "guild-member-removed" if raw.get_str("operator_id").is_none_or(|op| op != me) => {
                purge::mark(&group, &user, purge::Fate::Left);
                return Ok(Some(ctx));
            }
            "message-created" if !user.is_empty() => {}
            _ => return Ok(Some(ctx)),
        }
        // 排着队要被移出的人说话了：这次放过他。
        purge::mark(&group, &user, purge::Fate::Spoke);
        let config = get_config_or_default::<Config>(&ctx);
        let echo = user == me && !ctx.as_message().is_some_and(|m| m.is_manual_self());
        if echo || !config.channel.allows_group(&group) {
            return Ok(Some(ctx));
        }
        if command::answer(&ctx, &writer, &config, &group).await? {
            return Ok(None);
        }
        if let Some(matched) = crate::command::match_word_command(&ctx, "潜水")
            && command::run(&ctx, &writer, &config, &group, matched).await?
        {
            return Ok(None);
        }
        Ok(Some(ctx))
    })
}

// ==================== 控制台 ====================

/// 字段的中文名。
pub const LABELS: &[(&str, &str)] = &[
    ("channel", "生效范围"),
    ("list", "名单的样子"),
    ("pace", "移出的节奏"),
    ("warn", "提醒"),
    ("auto", "自动清理"),
    ("groups", "每群单独设置"),
    ("days", "多少天没说话算潜水"),
    ("min_messages", "至少说几句"),
    ("newcomer_days", "新人几天不说话也算"),
    ("protect_titled", "有群头衔的不动"),
    ("protect_level", "群等级不低于这个的不动"),
    ("exempt", "豁免名单"),
    ("managers", "谁能用"),
    ("permanent", "移出后不许再申请"),
    ("list.image", "名单出成长图"),
    ("list.rows_per_image", "每张图几行"),
    ("list.avatars", "图上带头像"),
    ("pace.interval_seconds", "每人间隔"),
    ("pace.burst", "每批几人"),
    ("pace.rest_seconds", "每批之后歇"),
    ("pace.max_per_run", "一次最多移出"),
    ("warn.grace_days", "提醒后宽限几天"),
    ("warn.text", "提醒的话"),
    ("auto.enabled", "自动清理"),
    ("auto.groups", "自动清理哪些群"),
    ("auto.weekdays", "哪几天发提醒"),
    ("auto.time", "每天几点看"),
    ("auto.warn", "先提醒再移出"),
    ("auto.max_kicks", "每群一次最多移出"),
    ("auto.max_ratio", "人数异常的上限（%）"),
    ("auto.announce", "做完在群里说一声"),
    ("auto.announce_text", "群里说的话"),
    ("groups.*.days", "多少天没说话算潜水"),
    ("groups.*.min_messages", "至少说几句"),
    ("groups.*.newcomer_days", "新人几天不说话也算"),
    ("groups.*.exempt", "本群豁免名单"),
];

pub fn option_labels() -> serde_json::Value {
    serde_json::json!({
        "managers": { "staff": "群主、管理员与机器人管理员", "admin": "只有机器人管理员" },
    })
}

/// 两条提示的文案编辑器：占位符与样例。
pub fn templates() -> serde_json::Value {
    serde_json::json!({
        "fields": {
            "warn.text": { "kind": "warn", "about": "提醒潜水成员的话", "silent": "没有提醒的话：「潜水 提醒」与自动清理的提醒都发不出去。" },
            "auto.announce_text": { "kind": "announce", "about": "自动清理做完在群里说的话", "silent": "做完不在群里说。" },
        },
        "placeholders": [
            { "key": "at", "label": "@ 被提醒的人", "hint": "一条提醒最多 @ 20 人，人多就分几条", "kinds": ["warn"] },
            { "key": "days", "label": "天数", "hint": "多少天没说话算潜水（本群单独设了就照本群）", "kinds": ["warn", "announce"] },
            { "key": "deadline", "label": "宽限到哪天", "hint": "提醒那天加上「提醒后宽限几天」", "kinds": ["warn"] },
            { "key": "grace", "label": "宽限几天", "kinds": ["warn"] },
            { "key": "count", "label": "人数", "hint": "这次提醒（或移出）了几个人", "kinds": ["warn", "announce"] },
            { "key": "names", "label": "移出了谁", "hint": "最多列十个名字，再多写「等 N 人」", "kinds": ["announce"] },
        ],
        "samples": { "name": "小林", "days": "30", "deadline": "10 月 26 日", "grace": "3", "count": "12", "names": "小林、阿哲、晚风 等 12 人" },
    })
}
