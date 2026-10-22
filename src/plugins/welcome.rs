//! 入群欢迎：进群 @ 一句欢迎（可配图），退群说一声；可选的入群验证与加群申请自动审批。
//!
//! 四件事各有各的开关，按群名单（`channel`）与每群覆盖（`groups."群号"`）收窄：
//!
//! - **欢迎 / 送别**：每条提示是一个 [`Template`]——可以写好几种说法，每次随机挑一种；
//!   占位符见 [`PLACEHOLDERS`]（`{at}` 是真 @，`{image}` `{avatar}` `{face:ID}` 是图与表情，
//!   `{甲|乙}` 是随机片段）。同一时间涌进来好几个人时可以合并成一条（`join.merge_seconds`），
//!   发出去的欢迎可以过一会儿自己撤掉（`recall_seconds`），不占群聊。
//! - **入群验证**：新人要在时限内完成一件小事——说任意一句话、发出指定口令、回验证码或算一道
//!   加法（`verify.mode`）；期间按 `remind_seconds` 的剩余时间点 @ 他倒计时，答错有提示、
//!   可以限次数；超时按 `action` 移出或禁言。账号不是群主或管理员时罚不了人，验证整个跳过、
//!   只发欢迎。待验证名单落盘，重启后接着计时。
//! - **申请审批**：附言里含关键词的自动同意；对不上的看 `reject` 决定拒绝还是留给人
//!   （智能体的管理栏也看得到，见 [`crate::requests`]）。
//! - **群管理指令**：群主、管理员发「欢迎」看本群的设置与待验证名单，「欢迎 预览」把提示
//!   对自己发一遍，「欢迎 放行 @某人」手动放行。
//!
//! 默认文案的取舍：一句欢迎、一句规矩、一句后果，短到在手机上一屏读完；口吻是「有人
//! 管、但不凶」——后果写明白（多久、会怎样），不加感叹号堆砌与「警告」字样。欢迎词给了
//! 三种说法轮着用，群里天天进人也不像复读机。

use crate::adapters::satori::{LockedWriter, api, send_msg, send_msg_id};
use crate::clock::{beijing_now, duration_text};
use crate::command::{self, CommandMatch};
use crate::event::{Context, EventType};
use crate::message::Message;
use crate::plugins::{ChannelConfig, PluginConfig, PluginError, get_config_or_default};
use crate::storage::JsonState;
use chrono::{Datelike, Timelike};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsScalar};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const LOG_TARGET: &str = "Plugin/Welcome";

// ==================== 配置 ====================

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

/// 一条提示：可以写几种说法，每次随机挑一种；一种都没写（或都是空白）就不发。
///
/// 配置里写成一个字符串也认——那就是只有一种说法。
#[derive(Serialize, Clone, Default, PartialEq, Debug)]
#[serde(transparent)]
pub struct Template(Vec<String>);

impl<'de> Deserialize<'de> for Template {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            One(String),
            Many(Vec<String>),
        }
        Ok(Self(match Raw::deserialize(deserializer)? {
            Raw::One(text) => vec![text],
            Raw::Many(list) => list,
        }))
    }
}

impl Template {
    fn of(variants: &[&str]) -> Self {
        Self(variants.iter().map(ToString::to_string).collect())
    }

    fn variants(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str).filter(|text| !text.trim().is_empty())
    }

    fn count(&self) -> usize {
        self.variants().count()
    }

    fn silent(&self) -> bool {
        self.count() == 0
    }

    fn pick(&self) -> Option<String> {
        let variants: Vec<&str> = self.variants().collect();
        (!variants.is_empty()).then(|| variants[rand::random_range(0..variants.len())].to_string())
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct JoinConfig {
    enabled: bool,
    text: Template,
    /// 配图：图片直链，或数据目录 data/welcome/ 下的文件名（也可写绝对路径）。多张时每次随机一张。
    images: Vec<String>,
    /// 这么多秒内进群的人合并成一条欢迎；0 = 来一个欢迎一个。
    merge_seconds: u64,
    /// 欢迎发出这么多秒后撤回；0 = 不撤。要验证的人等验证有了结果再撤。
    recall_seconds: u64,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct LeaveConfig {
    enabled: bool,
    /// 自己退群。
    text: Template,
    /// 被移出；留空 = 被移出时不说。
    kicked_text: Template,
    recall_seconds: u64,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct VerifyConfig {
    enabled: bool,
    /// any 说任意一句话、answer 发出口令（answers 之一）、code 回随机验证码、math 算一道加法。
    mode: String,
    /// mode = answer 时的口令：回答里含其中任一句即通过（不分大小写与空格）。
    answers: Vec<String>,
    /// 时限（秒）。
    timeout_seconds: u64,
    /// 答错这么多次就按超时处理；0 = 不限。mode = any 时没有答错这回事。
    attempts: u32,
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
    prompt_text: Template,
    remind_text: Template,
    wrong_text: Template,
    pass_text: Template,
    fail_text: Template,
    /// 「通过」「超时」那两句发出这么多秒后撤回；0 = 不撤。
    recall_seconds: u64,
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
    join_text: Option<Template>,
    images: Option<Vec<String>>,
    leave_enabled: Option<bool>,
    leave_text: Option<Template>,
    kicked_text: Option<Template>,
    verify_enabled: Option<bool>,
    verify_mode: Option<String>,
    answers: Option<Vec<String>>,
    approve_enabled: Option<bool>,
    keywords: Option<Vec<String>>,
}

const JOIN_TEXT: &[&str] = &[
    "欢迎 {at} 加入{group}～\n群公告里有群规和常用信息，有问题直接问就好。",
    "{greeting}，{at}！欢迎来到{group}。\n先看看群公告，有什么想问的随时开口～",
    "{at} 来啦，欢迎～\n群公告里写着群规和常用信息，先看一眼，有问题直接问。",
];
const LEAVE_TEXT: &str = "{name}（{user_id}）离开了群聊。";
const KICKED_TEXT: &str = "{name}（{user_id}）已被 {operator} 移出群聊。";
const PROMPT_TEXT: &str = "请在 {timeout} 内{task}完成验证，超时会被{punish}。";
const REMIND_TEXT: &str = "{at} 验证还剩 {remaining}：{task}就好，超时会被{punish}。";
const WRONG_TEXT: &str = "{at} 还不对哦，{task}就好{chances}。";
const PASS_TEXT: &str = "{at} 验证通过，欢迎正式加入～";
const FAIL_TEXT: &str = "{name} 未能完成入群验证，已{punish}。";
const REJECT_MESSAGE: &str = "附言没有对上入群问题，看清问题再来申请吧～";

impl Default for JoinConfig {
    fn default() -> Self {
        Self { enabled: true, text: Template::of(JOIN_TEXT), images: Vec::new(), merge_seconds: 0, recall_seconds: 0 }
    }
}

impl Default for LeaveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            text: Template::of(&[LEAVE_TEXT]),
            kicked_text: Template::of(&[KICKED_TEXT]),
            recall_seconds: 0,
        }
    }
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: "code".into(),
            answers: Vec::new(),
            timeout_seconds: 300,
            attempts: 0,
            remind_seconds: vec![60],
            action: "kick".into(),
            mute_seconds: 86400,
            kick_permanent: false,
            recall_unverified: false,
            prompt_text: Template::of(&[PROMPT_TEXT]),
            remind_text: Template::of(&[REMIND_TEXT]),
            wrong_text: Template::of(&[WRONG_TEXT]),
            pass_text: Template::of(&[PASS_TEXT]),
            fail_text: Template::of(&[FAIL_TEXT]),
            recall_seconds: 0,
        }
    }
}

impl Default for ApproveConfig {
    fn default() -> Self {
        Self { enabled: false, keywords: Vec::new(), reject: false, reject_message: REJECT_MESSAGE.into() }
    }
}

const MODES: [&str; 4] = ["any", "answer", "code", "math"];

/// 1.16 及以前的默认文案。用户没动过的照新版换掉，改过的原样留着。
const RETIRED: &[(&str, &str, &str)] = &[
    ("join", "text", "欢迎 {at} 加入{group}～\n先看看群公告，有问题直接问。"),
    ("leave", "text", "{name}（{user_id}）离开了本群。"),
    ("leave", "kicked_text", "{name}（{user_id}）已被移出本群。"),
    ("verify", "prompt_text", "请在 {timeout} 内发送「{answer}」完成验证，超时将{punish}。"),
    ("verify", "remind_text", "{at} 验证还剩 {remaining}：发送「{answer}」即可，超时将{punish}。"),
    ("verify", "pass_text", "{at} 验证通过，欢迎正式入群！"),
    ("verify", "fail_text", "{name} 未在时限内完成入群验证，已{punish}。"),
    ("approve", "reject_message", "答案不对，看清入群问题再来～"),
];

impl PluginConfig for Config {
    const NAME: &'static str = "welcome";

    fn check(&self) -> Result<(), String> {
        let verify = &self.verify;
        if !MODES.contains(&verify.mode.as_str()) {
            return Err("verify.mode 只能是 any、answer、code 或 math".into());
        }
        if !matches!(verify.action.as_str(), "kick" | "mute" | "none") {
            return Err("verify.action 只能是 kick、mute 或 none".into());
        }
        if !(30..=86400).contains(&verify.timeout_seconds) {
            return Err("verify.timeout_seconds 须在 30–86400 秒之间".into());
        }
        if verify.action == "mute" && !(60..=30 * 86400).contains(&verify.mute_seconds) {
            return Err("verify.mute_seconds 须在 60 秒到 30 天之间".into());
        }
        if verify.attempts > 20 {
            return Err("verify.attempts 最多 20 次（0 = 不限）".into());
        }
        if self.join.merge_seconds > 120 {
            return Err("join.merge_seconds 最多 120 秒".into());
        }
        for (path, seconds) in [
            ("join", self.join.recall_seconds),
            ("leave", self.leave.recall_seconds),
            ("verify", verify.recall_seconds),
        ] {
            if seconds > 86400 {
                return Err(format!("{path}.recall_seconds 最多 86400 秒（一天）"));
            }
        }
        for (group, o) in &self.groups {
            if group.parse::<u64>().is_err() {
                return Err("groups 的键写群号".into());
            }
            if let Some(mode) = &o.verify_mode
                && !MODES.contains(&mode.as_str())
            {
                return Err(format!("groups.\"{group}\".verify_mode 只能是 any、answer、code 或 math"));
            }
        }
        Ok(())
    }

    fn migrate(table: &mut toml::Table) -> bool {
        let mut changed = false;
        // 旧版的 `verify.code = true` 是一种验证方式，现在并进 mode。
        if let Some(toml::Value::Table(verify)) = table.get_mut("verify")
            && !verify.contains_key("mode")
        {
            let code = verify.remove("code").and_then(|v| v.as_bool()).unwrap_or(false);
            let answers = verify.get("answers").and_then(toml::Value::as_array).is_some_and(|a| !a.is_empty());
            let mode = if code { "code" } else if answers { "answer" } else { "any" };
            verify.insert("mode".into(), mode.into());
            changed = true;
        }
        // 旧版被移出时 kicked_text 留空 = 照 text 说；现在留空 = 不说，把原来的意思写实。
        if let Some(toml::Value::Table(leave)) = table.get_mut("leave")
            && leave.get("kicked_text").and_then(toml::Value::as_str).is_some_and(|text| text.trim().is_empty())
            && let Some(text) = leave.get("text").cloned()
        {
            leave.insert("kicked_text".into(), text);
            changed = true;
        }
        let fresh = toml::Value::try_from(Self::default()).ok();
        for (section, key, old) in RETIRED {
            let Some(toml::Value::Table(part)) = table.get_mut(*section) else { continue };
            if part.get(*key).and_then(toml::Value::as_str) == Some(old)
                && let Some(new) = fresh.as_ref().and_then(|f| f.get(section)).and_then(|s| s.get(key))
            {
                part.insert((*key).to_string(), new.clone());
                changed = true;
            }
        }
        changed
    }
}

/// 这个群实际生效的那一份：全局配置叠上每群覆盖。
struct Effective {
    join: bool,
    join_text: Template,
    images: Vec<String>,
    leave: bool,
    leave_text: Template,
    kicked_text: Template,
    verify: bool,
    mode: String,
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
            leave_text: o.leave_text.unwrap_or_else(|| self.leave.text.clone()),
            kicked_text: o.kicked_text.unwrap_or_else(|| self.leave.kicked_text.clone()),
            verify: o.verify_enabled.unwrap_or(self.verify.enabled),
            mode: o.verify_mode.unwrap_or_else(|| self.verify.mode.clone()),
            answers: o.answers.unwrap_or_else(|| self.verify.answers.clone()),
            approve: o.approve_enabled.unwrap_or(self.approve.enabled),
            keywords: o.keywords.unwrap_or_else(|| self.approve.keywords.clone()),
        }
    }
}

// ==================== 占位符 ====================

/// 占位符目录：键、界面上的名字、一句说明，以及它能出现在哪几种提示里。
/// 控制台的文案编辑器照这张表给「插入」芯片与预览，渲染认的是同一批键。
struct Placeholder {
    key: &'static str,
    label: &'static str,
    hint: &'static str,
    kinds: &'static [Kind],
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Join,
    Leave,
    Kicked,
    Prompt,
    Remind,
    Wrong,
    Pass,
    Fail,
}

impl Kind {
    const ALL: [Kind; 8] = [Join, Leave, Kicked, Prompt, Remind, Wrong, Pass, Fail];

    fn code(self) -> &'static str {
        match self {
            Join => "join",
            Leave => "leave",
            Kicked => "kicked",
            Prompt => "prompt",
            Remind => "remind",
            Wrong => "wrong",
            Pass => "pass",
            Fail => "fail",
        }
    }
}

use Kind::{Fail, Join, Kicked, Leave, Pass, Prompt, Remind, Wrong};
const EVERY: &[Kind] = &Kind::ALL;
const PRESENT: &[Kind] = &[Join, Prompt, Remind, Wrong, Pass, Fail];
const CHECKS: &[Kind] = &[Prompt, Remind, Wrong];

const PLACEHOLDERS: &[Placeholder] = &[
    Placeholder { key: "at", label: "@ 新人", hint: "真的 @ 他；合并欢迎时一次 @ 好几个", kinds: PRESENT },
    Placeholder { key: "name", label: "昵称", hint: "群名片，没有就用昵称；合并欢迎时是「小林、阿杰」这样的一串", kinds: EVERY },
    Placeholder { key: "user_id", label: "账号", hint: "QQ 号 / 微信号", kinds: EVERY },
    Placeholder { key: "group", label: "群名", hint: "", kinds: EVERY },
    Placeholder { key: "group_id", label: "群号", hint: "", kinds: EVERY },
    Placeholder { key: "member_count", label: "群人数", hint: "群里现在有多少人；读不到时为空", kinds: EVERY },
    Placeholder { key: "count", label: "这批人数", hint: "合并欢迎时这一条欢迎了几个人，平时是 1", kinds: &[Join] },
    Placeholder { key: "greeting", label: "问候", hint: "按北京时间：早上好、上午好、中午好、下午好、晚上好、夜深了", kinds: EVERY },
    Placeholder { key: "time", label: "时刻", hint: "如 21:05", kinds: EVERY },
    Placeholder { key: "date", label: "日期", hint: "如 10 月 22 日", kinds: EVERY },
    Placeholder { key: "weekday", label: "星期", hint: "如 星期三", kinds: EVERY },
    Placeholder { key: "operator", label: "操作人", hint: "把他移出去的人", kinds: &[Kicked] },
    Placeholder { key: "timeout", label: "时限", hint: "如 5 分钟", kinds: &[Prompt, Fail] },
    Placeholder { key: "remaining", label: "剩余时间", hint: "如 1 分钟", kinds: &[Remind] },
    Placeholder { key: "task", label: "要做的事", hint: "随验证方式变：随便说一句话 / 发送「口令」/ 发送验证码 4821 / 发出 7 + 5 的答案", kinds: CHECKS },
    Placeholder { key: "answer", label: "口令或题目", hint: "口令、验证码，或算术题本身（7 + 5）", kinds: CHECKS },
    Placeholder { key: "punish", label: "后果", hint: "移出群聊 / 禁言 1 天 / 记录在案", kinds: &[Prompt, Remind, Fail] },
    Placeholder { key: "chances", label: "剩余次数", hint: "限了次数时是「（还能再试 2 次）」，不限时为空", kinds: &[Wrong] },
    Placeholder { key: "avatar", label: "头像", hint: "插一张他的头像（QQ）", kinds: PRESENT },
    Placeholder { key: "image", label: "配图", hint: "配图放在这里；没写时配图接在最后", kinds: &[Join] },
];

/// 一条提示里能填的值。`{at}` `{image}` `{avatar}` `{face:ID}` 不在这里：它们是消息段，由 [`pieces`] 切开。
struct Vars(HashMap<&'static str, String>);

impl Vars {
    /// 带上与人无关的那几样：问候、时刻、日期、星期。
    fn now() -> Self {
        let now = beijing_now();
        let greeting = match now.hour() {
            5..=8 => "早上好",
            9..=10 => "上午好",
            11..=12 => "中午好",
            13..=17 => "下午好",
            18..=22 => "晚上好",
            _ => "夜深了",
        };
        let weekday = ["一", "二", "三", "四", "五", "六", "日"][now.weekday().num_days_from_monday() as usize];
        let mut map = HashMap::new();
        map.insert("greeting", greeting.to_string());
        map.insert("time", format!("{:02}:{:02}", now.hour(), now.minute()));
        map.insert("date", format!("{} 月 {} 日", now.month(), now.day()));
        map.insert("weekday", format!("星期{weekday}"));
        map.insert("count", "1".into());
        Self(map)
    }

    fn set(mut self, key: &'static str, value: impl Into<String>) -> Self {
        self.0.insert(key, value.into());
        self
    }

    fn group(self, info: &GroupInfo) -> Self {
        self.set("group", info.name.clone())
            .set("group_id", info.id.clone())
            .set("member_count", info.members.map(|n| n.to_string()).unwrap_or_default())
    }

    fn person(self, user: &str, name: &str) -> Self {
        self.set("user_id", user).set("name", name)
    }
}

#[derive(Debug, PartialEq)]
enum Piece {
    Text(String),
    At,
    Image,
    Avatar,
    Face(String),
}

/// 模板切成消息段。认不出的 `{…}` 原样留着——宁可让人看到自己写错了，也不悄悄吞掉。
fn pieces(template: &str, vars: &Vars) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        text.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find(['}', '{']).filter(|&at| after.as_bytes()[at] == b'}') else {
            text.push('{');
            rest = after;
            continue;
        };
        let inner = after[..close].trim();
        let piece = match inner {
            "at" => Some(Piece::At),
            "image" => Some(Piece::Image),
            "avatar" => Some(Piece::Avatar),
            _ => match inner.strip_prefix("face:") {
                Some(id) if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) => Some(Piece::Face(id.to_string())),
                _ => None,
            },
        };
        match piece {
            Some(piece) => {
                if !text.is_empty() {
                    out.push(Piece::Text(std::mem::take(&mut text)));
                }
                out.push(piece);
            }
            None if inner.contains('|') => {
                let options: Vec<&str> = inner.split('|').collect();
                text.push_str(options[rand::random_range(0..options.len())]);
            }
            None => match vars.0.get(inner) {
                Some(value) => text.push_str(value),
                None => text.push_str(&rest[open..open + close + 2]),
            },
        }
        rest = &after[close + 1..];
    }
    text.push_str(rest);
    if !text.is_empty() {
        out.push(Piece::Text(text));
    }
    out
}

/// 发给谁、配什么图：渲染一条提示要的上下文。
struct Draft<'a> {
    at: &'a [String],
    images: &'a [String],
}

/// 模板 → 消息：`{at}` 处换成对这些人的 @，图片与表情就地插入。整条只剩空白就不发（返回 `None`）。
async fn compose(ctx: &Context, writer: &LockedWriter, template: &str, vars: &Vars, draft: &Draft<'_>) -> Option<Message> {
    let parts = pieces(template, vars);
    let placed = parts.contains(&Piece::Image);
    let mut message = Message::new();
    let mut visible = false;
    for part in parts {
        match part {
            Piece::Text(text) => {
                visible |= !text.trim().is_empty();
                message = message.text(text);
            }
            Piece::At => {
                for (index, user) in draft.at.iter().enumerate() {
                    if index > 0 {
                        message = message.text(" ");
                    }
                    message = message.at(user);
                    visible = true;
                }
            }
            Piece::Image => {
                if let Some(file) = image(ctx, writer, draft.images).await {
                    message = message.image(file);
                    visible = true;
                }
            }
            Piece::Avatar => {
                for user in draft.at.iter().take(5) {
                    if let Some(url) = avatar(ctx, user) {
                        message = message.image(url);
                        visible = true;
                    }
                }
            }
            Piece::Face(id) => {
                message = message.face(id);
                visible = true;
            }
        }
    }
    if !placed && let Some(file) = image(ctx, writer, draft.images).await {
        message = message.image(file);
        visible = true;
    }
    visible.then_some(message)
}

/// 头像：事件里带了就用它；QQ 还能用号码拼出公开头像。
fn avatar(ctx: &Context, user: &str) -> Option<String> {
    if let EventType::Satori(raw) = &ctx.event
        && raw.get_str("user_id") == Some(user)
        && let Some(url) = raw
            .get("_satori")
            .and_then(|v| v.get("user"))
            .and_then(|v| v.get_str("avatar"))
            .filter(|url| url.starts_with("http"))
    {
        return Some(url.to_string());
    }
    let qq = matches!(ctx.bot.platform.to_ascii_lowercase().as_str(), "red" | "qq");
    (qq && !user.is_empty() && user.bytes().all(|b| b.is_ascii_digit()))
        .then(|| format!("https://q.qlogo.cn/headimg_dl?dst_uin={user}&spec=640"))
}

/// 配图随机挑一张；本地文件先上传（QQ 读不到 Termux 的私有目录）。
async fn image(ctx: &Context, writer: &LockedWriter, images: &[String]) -> Option<String> {
    let images: Vec<&str> = images.iter().map(|i| i.trim()).filter(|i| !i.is_empty()).collect();
    if images.is_empty() {
        return None;
    }
    let pick = images[rand::random_range(0..images.len())];
    if pick.starts_with("http://") || pick.starts_with("https://") {
        return Some(pick.to_string());
    }
    let path = if std::path::Path::new(pick).is_absolute() {
        std::path::PathBuf::from(pick)
    } else {
        crate::storage::data_path("welcome").ok()?.join(pick)
    };
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            warn!(target: LOG_TARGET, "欢迎配图 {} 读不出来：{error}", path.display());
            return None;
        }
    };
    let uploaded = writer.upload(ctx, bytes, pick, "application/octet-stream").await.ok()?;
    uploaded.get("file").and_then(Value::as_str).map(str::to_owned)
}

/// 发一条提示；`recall` 秒后撤回（0 = 不撤）。返回消息 ID。
async fn say(ctx: &Context, writer: &LockedWriter, group: &str, message: Message, recall: u64) -> Result<Option<String>, PluginError> {
    let id = send_msg_id(ctx, writer.clone(), Some(group), None, message).await?;
    if recall > 0
        && let Some(id) = &id
    {
        recall_later(ctx, writer, group, id.clone(), recall);
    }
    Ok(id)
}

/// 过一会儿撤回自己发的一条。只记在内存里：重启时还没到点的就留着了。
fn recall_later(ctx: &Context, writer: &LockedWriter, group: &str, id: String, seconds: u64) {
    let (ctx, writer, group) = (ctx.clone(), writer.clone(), group.to_string());
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(seconds)).await;
        if let Err(error) = api::delete_msg_in(&ctx, writer, &group, &id).await {
            debug!(target: LOG_TARGET, "群 {group} 的提示 {id} 到点没撤掉：{error}");
        }
    });
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
    /// 验证方式（与 `verify.mode` 同一套词）。
    #[serde(default)]
    mode: String,
    /// 算术题的题面（7 + 5）；别的方式为空。
    #[serde(default)]
    question: String,
    /// 已经提醒过的剩余秒数点。
    reminded: Vec<u64>,
    /// 群里还挂着的那条倒计时或答错提示（消息 ID）。验证一有结果就撤掉：
    /// 过期的「还剩 30 秒」留在聊天记录里，只会让往上翻的人多读一句废话。
    #[serde(default)]
    notices: Vec<String>,
    /// 答错了几次。
    #[serde(default)]
    wrong: u32,
    /// 上一次答错提示的时刻（Unix 秒），连着说错时别句句都回。
    #[serde(default)]
    hinted_at: i64,
    /// 那条欢迎（带着验证说明）的消息 ID 与发出时刻：要撤的话等验证有了结果再撤。
    #[serde(default)]
    welcome: Option<(String, i64)>,
}

impl Pending {
    fn task(&self) -> String {
        task_text(&self.mode, &self.answers, &self.question)
    }

    fn answer(&self) -> String {
        if self.mode == "math" { self.question.clone() } else { self.answers.first().cloned().unwrap_or_default() }
    }
}

/// `{task}`：随验证方式变的那半句话。
fn task_text(mode: &str, answers: &[String], question: &str) -> String {
    match (mode, answers.first()) {
        ("math", _) => format!("发出 {question} 的答案"),
        ("code", Some(code)) => format!("发送验证码 {code}"),
        (_, Some(answer)) => format!("发送「{answer}」"),
        _ => "随便说一句话".into(),
    }
}

/// 出一道题：(方式, 答案, 题面)。口令方式没写口令时退成任意一句。
fn challenge(mode: &str, answers: &[String]) -> (String, Vec<String>, String) {
    match mode {
        "code" => ("code".into(), vec![format!("{:04}", rand::random_range(0..10000))], String::new()),
        "math" => {
            let (a, b) = (rand::random_range(2..20), rand::random_range(2..20));
            ("math".into(), vec![(a + b).to_string()], format!("{a} + {b}"))
        }
        "answer" if answers.iter().any(|a| !a.trim().is_empty()) => {
            ("answer".into(), answers.iter().filter(|a| !a.trim().is_empty()).cloned().collect(), String::new())
        }
        _ => ("any".into(), Vec::new(), String::new()),
    }
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct State {
    pending: Vec<Pending>,
}

static STATE: JsonState<State> = JsonState::new(LOG_TARGET, "welcome", "pending.json", "入群验证名单");

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 比对用的写法：去空白、全角转半角、不分大小写。
fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| match c {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            _ => c,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

/// 回答对不对。验证码与算术要整串数字对上（「12」不算答对了「123」）；口令只要包含。
fn passes(pending: &Pending, text: &str) -> bool {
    let text = normalize(text);
    match pending.mode.as_str() {
        "code" | "math" => text
            .split(|c: char| !c.is_ascii_digit())
            .any(|number| !number.is_empty() && pending.answers.iter().any(|a| a == number)),
        _ if pending.answers.is_empty() => !text.is_empty(),
        _ => pending.answers.iter().map(|a| normalize(a)).any(|a| !a.is_empty() && text.contains(&a)),
    }
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
                if let Some(matched) = command::match_word_command(&ctx, "欢迎")
                    && staff(&ctx)
                    && run_command(&ctx, &writer, &config, &group, matched).await?
                {
                    true
                } else {
                    verify_message(&ctx, &writer, &config, &group, &user).await?
                }
            }
            _ => false,
        };
        Ok((!consumed).then_some(ctx))
    })
}

/// 群主、群管理员与机器人管理员：能看设置、预览、手动放行。
fn staff(ctx: &Context) -> bool {
    crate::access::is_manager(ctx) || ctx.as_message().and_then(|m| m.sender_role()).is_some_and(api::is_staff)
}

/// 事件里带的名字；没有就问一次平台，再没有用号码。
async fn display_name(ctx: &Context, writer: &LockedWriter, group: &str, user: &str) -> String {
    if let EventType::Satori(raw) = &ctx.event
        && raw.get_str("user_id") == Some(user)
    {
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

struct GroupInfo {
    id: String,
    name: String,
    members: Option<i32>,
}

/// 群名与人数：一次 `guild.get` 都有。读不到就是群号、人数为空。
async fn group_info(ctx: &Context, writer: &LockedWriter, group: &str) -> GroupInfo {
    let info = api::get_guild_info(ctx, writer.clone(), group).await.ok();
    let event_name = match &ctx.event {
        EventType::Satori(raw) => raw
            .get("_satori")
            .and_then(|v| v.get("guild"))
            .and_then(|v| v.get_str("name"))
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
        _ => None,
    };
    let name = info
        .as_ref()
        .map(|i| i.group_name.clone())
        .filter(|name| !name.is_empty())
        .or(event_name)
        .unwrap_or_else(|| group.to_string());
    GroupInfo { id: group.to_string(), name, members: info.and_then(|i| i.member_count) }
}

/// 合并欢迎的缓冲：群 → 这一批要 @ 的人（号码, 名字）。
type Batch = HashMap<String, Vec<(String, String)>>;

fn batch() -> &'static Mutex<Batch> {
    static BATCH: OnceLock<Mutex<Batch>> = OnceLock::new();
    BATCH.get_or_init(Default::default)
}

/// 合并欢迎时 `{name}` 的说法：三个以内全写出来，再多写前三个加总数。
fn names_text(names: &[String]) -> String {
    match names.len() {
        0..=3 => names.join("、"),
        n => format!("{} 等 {n} 人", names[..3].join("、")),
    }
}

async fn joined(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str, user: &str) -> Result<(), PluginError> {
    let effective = config.for_group(group);
    let mut verify = None;
    if effective.verify {
        let exempt = crate::access::admin_ids(ctx).iter().any(|id| id == user);
        if !exempt && api::is_staff(&api::own_role(ctx, writer.clone(), group).await) {
            verify = Some(challenge(&effective.mode, &effective.answers));
        } else if !exempt {
            debug!(target: LOG_TARGET, "群 {group} 账号不是管理员，入群验证跳过");
        }
    }
    let welcome = effective.join && !effective.join_text.silent();
    if !welcome && verify.is_none() {
        return Ok(());
    }
    let name = display_name(ctx, writer, group, user).await;
    let info = group_info(ctx, writer, group).await;

    // 要验证的人单独欢迎：验证说明里有他自己的验证码与倒计时，合不进别人的那条。
    if verify.is_none() && config.join.merge_seconds > 0 {
        let first = {
            let mut batch = batch().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let list = batch.entry(group.to_string()).or_default();
            list.push((user.to_string(), name.clone()));
            list.len() == 1
        };
        if first {
            let (ctx, writer, group) = (ctx.clone(), writer.clone(), group.to_string());
            let delay = Duration::from_secs(config.join.merge_seconds);
            let (template, images, recall) = (effective.join_text.clone(), effective.images.clone(), config.join.recall_seconds);
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let people = batch()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&group)
                    .unwrap_or_default();
                let (users, names): (Vec<String>, Vec<String>) = people.into_iter().unzip();
                let vars = Vars::now()
                    .group(&info)
                    .person(&users.join("、"), &names_text(&names))
                    .set("count", users.len().to_string());
                let Some(text) = template.pick() else { return };
                let draft = Draft { at: &users, images: &images };
                let Some(message) = compose(&ctx, &writer, &text, &vars, &draft).await else { return };
                if let Err(error) = say(&ctx, &writer, &group, message, recall).await {
                    warn!(target: LOG_TARGET, "群 {group} 欢迎没发出去：{error}");
                }
            });
        }
        return Ok(());
    }

    let mut vars = Vars::now().group(&info).person(user, &name);
    let mut template = if welcome { effective.join_text.pick().unwrap_or_default() } else { String::new() };
    let mut pending = None;
    if let Some((mode, answers, question)) = verify {
        let entry = Pending {
            self_id: ctx.bot.self_id(),
            group: group.to_string(),
            user_id: user.to_string(),
            name: name.clone(),
            deadline: now() + i64::try_from(config.verify.timeout_seconds).unwrap_or(300),
            answers,
            mode,
            question,
            reminded: Vec::new(),
            notices: Vec::new(),
            wrong: 0,
            hinted_at: 0,
            welcome: None,
        };
        vars = vars
            .set("timeout", duration_text(config.verify.timeout_seconds))
            .set("task", entry.task())
            .set("answer", entry.answer())
            .set("punish", punish_text(&config.verify));
        if let Some(prompt) = config.verify.prompt_text.pick() {
            template = if !template.is_empty() {
                format!("{template}\n{prompt}")
            } else if prompt.contains("{at}") {
                prompt
            } else {
                format!("{{at}} {prompt}")
            };
        }
        info!(target: LOG_TARGET, "群 {group} 新成员 {name}（{user}）开始入群验证（{}）", entry.mode);
        pending = Some(entry);
    }
    let images = if welcome { effective.images } else { Vec::new() };
    let at = [user.to_string()];
    let message = compose(ctx, writer, &template, &vars, &Draft { at: &at, images: &images }).await;
    // 有验证时欢迎先不定时撤：上面写着验证码，撤早了新人就没处看了。
    let recall = if pending.is_some() { 0 } else { config.join.recall_seconds };
    let sent = match message {
        Some(message) => say(ctx, writer, group, message, recall).await?,
        None => None,
    };
    if let Some(mut entry) = pending {
        entry.welcome = sent.map(|id| (id, now()));
        STATE
            .with(|state| {
                state.pending.retain(|p| !(p.group == entry.group && p.user_id == entry.user_id));
                state.pending.push(entry);
            })
            .await;
    }
    Ok(())
}

async fn left(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str, user: &str) -> Result<(), PluginError> {
    let gone = STATE
        .with(|state| {
            let index = state.pending.iter().position(|p| p.group == group && p.user_id == user)?;
            Some(state.pending.remove(index))
        })
        .await;
    if let Some(p) = gone {
        settle(ctx, writer, config, &p);
        info!(target: LOG_TARGET, "群 {group} 待验证的 {user} 已离开，验证作废");
    }
    let effective = config.for_group(group);
    if !effective.leave {
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
    let template = if kicked { &effective.kicked_text } else { &effective.leave_text };
    let Some(text) = template.pick() else {
        return Ok(());
    };
    let name = display_name(ctx, writer, group, user).await;
    let operator_name = if kicked { display_name(ctx, writer, group, &operator).await } else { String::new() };
    let info = group_info(ctx, writer, group).await;
    let vars = Vars::now().group(&info).person(user, &name).set("operator", operator_name);
    let at = [user.to_string()];
    if let Some(message) = compose(ctx, writer, &text, &vars, &Draft { at: &at, images: &[] }).await {
        say(ctx, writer, group, message, config.leave.recall_seconds).await?;
    }
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
    let comment = normalize(raw.get_str("comment").unwrap_or(""));
    if flag.is_empty() {
        return Ok(());
    }
    let matched = effective.keywords.iter().all(|k| k.trim().is_empty())
        || effective
            .keywords
            .iter()
            .map(|keyword| normalize(keyword))
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
    let limit = config.verify.attempts;
    let throttle = now() - 15;
    let outcome = STATE
        .with(|state| {
            let index = state.pending.iter().position(|p| p.group == group && p.user_id == user)?;
            let p = &mut state.pending[index];
            if passes(p, &text) {
                return Some(Outcome::Passed(state.pending.remove(index)));
            }
            // 任意一句的方式没有「答错」；发图、发表情（没有文字）也不算一次。
            if p.mode == "any" || normalize(&text).is_empty() {
                return Some(Outcome::Waiting(None));
            }
            p.wrong += 1;
            if limit > 0 && p.wrong >= limit {
                return Some(Outcome::OutOfChances(state.pending.remove(index)));
            }
            if p.hinted_at > throttle {
                return Some(Outcome::Waiting(None));
            }
            p.hinted_at = now();
            Some(Outcome::Waiting(Some(p.clone())))
        })
        .await;
    let Some(outcome) = outcome else {
        return Ok(false);
    };
    let recall = config.verify.recall_unverified && !message_id.is_empty();
    match outcome {
        Outcome::Passed(p) => {
            pass(ctx, writer, config, &p).await?;
            return Ok(false);
        }
        Outcome::OutOfChances(p) => {
            info!(target: LOG_TARGET, "群 {group} 的 {}（{user}）答错 {limit} 次", p.name);
            expire(ctx, writer, config, p).await;
        }
        Outcome::Waiting(Some(p)) => wrong_hint(ctx, writer, config, &p).await,
        Outcome::Waiting(None) => {}
    }
    if recall {
        if let Err(error) = api::delete_msg_in(ctx, writer.clone(), group, &message_id).await {
            debug!(target: LOG_TARGET, "撤回待验证成员的消息失败：{error}");
        }
        return Ok(true);
    }
    Ok(false)
}

enum Outcome {
    Passed(Pending),
    OutOfChances(Pending),
    /// 还在等；带着人的话要回一句答错提示。
    Waiting(Option<Pending>),
}

/// 验证一有结果：撤掉倒计时与答错提示；欢迎设了定时撤回的，到点（或已过点就立刻）撤。
fn settle(ctx: &Context, writer: &LockedWriter, config: &Config, p: &Pending) {
    tidy(ctx, writer, &p.group, p.notices.clone());
    let recall = config.join.recall_seconds;
    if recall > 0
        && let Some((id, sent)) = &p.welcome
    {
        let left = (sent + i64::try_from(recall).unwrap_or(0) - now()).max(1);
        recall_later(ctx, writer, &p.group, id.clone(), left.unsigned_abs());
    }
}

async fn pass(ctx: &Context, writer: &LockedWriter, config: &Config, p: &Pending) -> Result<(), PluginError> {
    info!(target: LOG_TARGET, "群 {} 的 {}（{}）通过入群验证", p.group, p.name, p.user_id);
    settle(ctx, writer, config, p);
    if let Some(text) = config.verify.pass_text.pick() {
        let info = group_info(ctx, writer, &p.group).await;
        let vars = Vars::now().group(&info).person(&p.user_id, &p.name);
        let at = [p.user_id.clone()];
        if let Some(message) = compose(ctx, writer, &text, &vars, &Draft { at: &at, images: &[] }).await {
            say(ctx, writer, &p.group, message, config.verify.recall_seconds).await?;
        }
    }
    Ok(())
}

/// 答错了回一句。新提示顶替旧的：群里同一时刻只挂一条。
async fn wrong_hint(ctx: &Context, writer: &LockedWriter, config: &Config, p: &Pending) {
    let Some(text) = config.verify.wrong_text.pick() else { return };
    let limit = config.verify.attempts;
    let chances = if limit > 0 { format!("（还能再试 {} 次）", limit.saturating_sub(p.wrong)) } else { String::new() };
    let info = group_info(ctx, writer, &p.group).await;
    let vars = Vars::now()
        .group(&info)
        .person(&p.user_id, &p.name)
        .set("task", p.task())
        .set("answer", p.answer())
        .set("chances", chances);
    let at = [p.user_id.clone()];
    if let Some(message) = compose(ctx, writer, &text, &vars, &Draft { at: &at, images: &[] }).await {
        notice(ctx, writer, p, message).await;
    }
}

/// 发一条挂在验证上的提示（倒计时、答错），顶掉上一条。
async fn notice(ctx: &Context, writer: &LockedWriter, p: &Pending, message: Message) {
    match send_msg_id(ctx, writer.clone(), Some(&p.group), None, message).await {
        Ok(Some(id)) => {
            let stale = STATE
                .with(|state| {
                    let found = state.pending.iter_mut().find(|q| q.group == p.group && q.user_id == p.user_id);
                    match found {
                        Some(found) => std::mem::replace(&mut found.notices, vec![id]),
                        // 发出去的工夫里验证已经有了结果：这条也一并撤掉。
                        None => vec![id],
                    }
                })
                .await;
            tidy(ctx, writer, &p.group, stale);
        }
        Ok(None) => {}
        Err(error) => warn!(target: LOG_TARGET, "群 {} 验证提示没发出去：{error}", p.group),
    }
}

// ==================== 群管理指令 ====================

fn usage() -> String {
    "「欢迎」看本群的欢迎与验证设置、待验证名单\n\
     「欢迎 预览」把欢迎对自己发一遍；也可以预览 送别、移出、验证、提醒、答错、通过、超时\n\
     「欢迎 放行 @某人」让待验证的新人直接通过"
        .into()
}

async fn run_command(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str, matched: CommandMatch) -> Result<bool, PluginError> {
    let Some(message) = ctx.as_message() else {
        return Ok(false);
    };
    let message_id = message.message_id().to_string();
    let reply = |text: String| send_msg(ctx, writer.clone(), Some(group), None, Message::new().reply(&message_id).text(text));
    let arg = command::extract_text_arg(&matched.args);
    let word = arg.split_whitespace().next().unwrap_or("");
    let rest = arg[word.len()..].trim();
    match word {
        "" | "状态" | "设置" => reply(status(ctx, writer, config, group).await).await?,
        "预览" | "试试" | "测试" => {
            let me = message.user_id().to_string();
            if let Err(text) = preview(ctx, writer, config, group, &me, rest).await {
                reply(text).await?;
            }
        }
        "放行" | "通过" => {
            let mut ids = matched.at_ids.clone();
            ids.extend(rest.split_whitespace().filter(|w| w.bytes().all(|b| b.is_ascii_digit())).map(str::to_owned));
            if ids.is_empty() {
                reply("要放行谁？发「欢迎 放行 @某人」".into()).await?;
                return Ok(true);
            }
            let mut missed = Vec::new();
            for id in ids {
                let found = STATE
                    .with(|state| {
                        let index = state.pending.iter().position(|p| p.group == group && p.user_id == id)?;
                        Some(state.pending.remove(index))
                    })
                    .await;
                match found {
                    Some(p) => pass(ctx, writer, config, &p).await?,
                    None => missed.push(id),
                }
            }
            if !missed.is_empty() {
                reply(format!("{} 不在本群的待验证名单里", missed.join("、"))).await?;
            }
        }
        "帮助" | "-h" | "help" => reply(usage()).await?,
        // 没有指令前缀时「欢迎 新朋友」只是在聊天，不接话。
        _ if command::get_prefixes(ctx).iter().all(String::is_empty) => return Ok(false),
        _ => reply(format!("不认识「{word}」\n{}", usage())).await?,
    }
    Ok(true)
}

/// 本群现在是什么样：每一项开没开、开了是怎么个做法，验证差在哪，谁还在等。
async fn status(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str) -> String {
    let e = config.for_group(group);
    let onoff = |on: bool| if on { "开" } else { "关" };
    let mut lines = vec!["本群的入群欢迎".to_string()];
    let welcome = e.join && !e.join_text.silent();
    let mut join = format!("· 欢迎：{}", onoff(welcome));
    if welcome {
        let mut bits = vec![format!("{} 种说法轮着用", e.join_text.count())];
        if !e.images.is_empty() {
            bits.push(format!("配图 {} 张", e.images.len()));
        }
        if config.join.merge_seconds > 0 {
            bits.push(format!("{} 秒内进群的合成一条", config.join.merge_seconds));
        }
        if config.join.recall_seconds > 0 {
            bits.push(format!("{}后撤回", duration_text(config.join.recall_seconds)));
        }
        join.push_str(&format!("（{}）", bits.join("，")));
    }
    lines.push(join);
    lines.push(format!("· 送别：{}", onoff(e.leave)));
    if e.verify {
        let mode = match challenge(&e.mode, &e.answers).0.as_str() {
            "code" => "回验证码".to_string(),
            "math" => "算一道加法".to_string(),
            "answer" => format!("发口令「{}」", e.answers.join("」或「")),
            _ => "说任意一句话".to_string(),
        };
        let staff = api::is_staff(&api::own_role(ctx, writer.clone(), group).await);
        lines.push(format!(
            "· 入群验证：{mode}，{}内完成，超时{}{}",
            duration_text(config.verify.timeout_seconds),
            punish_text(&config.verify),
            if staff { "" } else { "\n  ⚠ 账号不是本群管理员，验证暂不生效（只发欢迎）" }
        ));
    } else {
        lines.push("· 入群验证：关".into());
    }
    lines.push(format!("· 申请审批：{}", onoff(e.approve)));
    let me = ctx.bot.self_id();
    let pending: Vec<Pending> = STATE
        .with(|state| state.pending.iter().filter(|p| p.group == group && p.self_id == me).cloned().collect())
        .await;
    if !pending.is_empty() {
        let now = now();
        let names: Vec<String> = pending
            .iter()
            .map(|p| format!("{}（还剩 {}）", p.name, duration_text(remaining_round(p.deadline - now))))
            .collect();
        lines.push(format!("· 待验证 {} 人：{}", pending.len(), names.join("、")));
    }
    lines.push("发「欢迎 预览」看看新人会收到什么。".into());
    lines.join("\n")
}

/// 剩余秒数凑个整：倒计时说「还剩 1 分钟」而不是「还剩 58 秒」。
fn remaining_round(seconds: i64) -> u64 {
    let seconds = seconds.max(0).unsigned_abs();
    if seconds >= 120 { seconds.div_ceil(60) * 60 } else { seconds.div_ceil(10) * 10 }
}

/// 把一条提示对指令发出人发一遍，和真的一模一样（只是不开始验证、不定时撤回）。
async fn preview(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str, me: &str, what: &str) -> Result<(), String> {
    let e = config.for_group(group);
    let verify = &config.verify;
    let (template, images): (Template, Vec<String>) = match what {
        "" | "欢迎" => {
            let mut variant = e.join_text.pick().unwrap_or_default();
            if e.verify
                && let Some(prompt) = verify.prompt_text.pick()
            {
                variant = if variant.is_empty() { format!("{{at}} {prompt}") } else { format!("{variant}\n{prompt}") };
            }
            (Template(vec![variant]), e.images.clone())
        }
        "送别" | "退群" => (e.leave_text.clone(), Vec::new()),
        "移出" | "踢" => (e.kicked_text.clone(), Vec::new()),
        "验证" => (verify.prompt_text.clone(), Vec::new()),
        "提醒" | "倒计时" => (verify.remind_text.clone(), Vec::new()),
        "答错" => (verify.wrong_text.clone(), Vec::new()),
        "通过" => (verify.pass_text.clone(), Vec::new()),
        "超时" | "失败" => (verify.fail_text.clone(), Vec::new()),
        _ => return Err(format!("没有「{what}」这条提示\n{}", usage())),
    };
    let Some(text) = template.pick() else {
        return Err("这条提示留空了，真遇到时不会发".into());
    };
    let name = display_name(ctx, writer, group, me).await;
    let info = group_info(ctx, writer, group).await;
    let (mode, answers, question) = challenge(&e.mode, &e.answers);
    let sample = Pending {
        self_id: String::new(),
        group: group.to_string(),
        user_id: me.to_string(),
        name: name.clone(),
        deadline: 0,
        answers,
        mode,
        question,
        reminded: Vec::new(),
        notices: Vec::new(),
        wrong: 1,
        hinted_at: 0,
        welcome: None,
    };
    let remind = verify.remind_seconds.iter().copied().max().unwrap_or(60).min(verify.timeout_seconds);
    let vars = Vars::now()
        .group(&info)
        .person(me, &name)
        .set("operator", ctx.as_message().map(|m| m.sender_name().to_string()).unwrap_or_default())
        .set("timeout", duration_text(verify.timeout_seconds))
        .set("remaining", duration_text(remaining_round(i64::try_from(remind).unwrap_or(60))))
        .set("task", sample.task())
        .set("answer", sample.answer())
        .set("punish", punish_text(verify))
        .set("chances", if verify.attempts > 1 { format!("（还能再试 {} 次）", verify.attempts - 1) } else { String::new() });
    let at = [me.to_string()];
    let Some(message) = compose(ctx, writer, &text, &vars, &Draft { at: &at, images: &images }).await else {
        return Err("这条提示渲染出来是空的".into());
    };
    send_msg(ctx, writer.clone(), Some(group), None, message).await.map_err(|error| format!("没发出去：{error}"))
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
    Drop(Pending),
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
                    due.push(Due::Drop(p.clone()));
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
                let Some(text) = config.verify.remind_text.pick() else { continue };
                let info = group_info(ctx, writer, &p.group).await;
                let vars = Vars::now()
                    .group(&info)
                    .person(&p.user_id, &p.name)
                    .set("remaining", duration_text(remaining_round(i64::try_from(remaining).unwrap_or(0))))
                    .set("task", p.task())
                    .set("answer", p.answer())
                    .set("punish", punish_text(&config.verify));
                let at = [p.user_id.clone()];
                if let Some(message) = compose(ctx, writer, &text, &vars, &Draft { at: &at, images: &[] }).await {
                    notice(ctx, writer, &p, message).await;
                }
            }
            Due::Expire(p) => expire(ctx, writer, &config, p).await,
            Due::Drop(p) => settle(ctx, writer, &config, &p),
        }
    }
}

/// 撤掉验证期间发的倒计时与答错提示。撤不掉（过了时限、没权限）就留着，不值得为它出声。
fn tidy(ctx: &Context, writer: &LockedWriter, group: &str, notices: Vec<String>) {
    if notices.is_empty() {
        return;
    }
    let (ctx, writer, group) = (ctx.clone(), writer.clone(), group.to_string());
    tokio::spawn(async move {
        for id in notices {
            if let Err(error) = api::delete_msg_in(&ctx, writer.clone(), &group, &id).await {
                debug!(target: LOG_TARGET, "群 {group} 的验证提醒 {id} 撤不掉：{error}");
            }
        }
    });
}

async fn expire(ctx: &Context, writer: &LockedWriter, config: &Config, p: Pending) {
    let verify = &config.verify;
    settle(ctx, writer, config, &p);
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
    info!(target: LOG_TARGET, "群 {} 的 {}（{}）验证未通过，已按 {} 处理", p.group, p.name, p.user_id, verify.action);
    let Some(text) = verify.fail_text.pick() else { return };
    let info = group_info(ctx, writer, &p.group).await;
    let vars = Vars::now()
        .group(&info)
        .person(&p.user_id, &p.name)
        .set("timeout", duration_text(verify.timeout_seconds))
        .set("punish", punish_text(verify));
    let at = [p.user_id.clone()];
    if let Some(message) = compose(ctx, writer, &text, &vars, &Draft { at: &at, images: &[] }).await
        && let Err(error) = say(ctx, writer, &p.group, message, verify.recall_seconds).await
    {
        warn!(target: LOG_TARGET, "群 {} 验证结果没发出去：{error}", p.group);
    }
}

// ==================== 控制台 ====================

/// 控制台插件页上方「现在的情况」：开没开、文案有几种、待验证的人。
pub(crate) fn live(ctx: &Context) -> Value {
    let config = get_config_or_default::<Config>(ctx);
    let mut lines = Vec::new();
    if !config.enabled {
        lines.push("插件关着。打开后，新人进群时照下面的设置欢迎；可以先在下面把文案改好、看预览。".to_string());
    }
    let join = &config.join;
    if join.enabled && !join.text.silent() {
        let images = if join.images.is_empty() { String::new() } else { format!("，配图 {} 张随机一张", join.images.len()) };
        lines.push(format!("欢迎词 {} 种说法，每次随机挑一种{images}。", join.text.count()));
    } else {
        lines.push("不发欢迎词。".to_string());
    }
    if config.verify.enabled {
        lines.push(format!(
            "入群验证开着：{}内完成，超时{}。只在账号是群主或管理员的群里生效。",
            duration_text(config.verify.timeout_seconds),
            punish_text(&config.verify)
        ));
        match STATE.peek() {
            Some(state) if !state.pending.is_empty() => {
                let now = now();
                let names: Vec<String> = state
                    .pending
                    .iter()
                    .take(8)
                    .map(|p| format!("{}（群 {}，还剩 {}）", p.name, p.group, duration_text(remaining_round(p.deadline - now))))
                    .collect();
                lines.push(format!("待验证 {} 人：{}", state.pending.len(), names.join("、")));
            }
            Some(_) => lines.push("现在没有待验证的新人。".to_string()),
            None => {}
        }
    }
    if !config.groups.is_empty() {
        lines.push(format!("{} 个群有单独的设置。", config.groups.len()));
    }
    lines.push("群主或管理员在群里发「欢迎 预览」，能把提示对自己发一遍看效果。".to_string());
    json!({ "lines": lines })
}

/// 控制台的文案编辑器要的东西：哪些字段是提示模板、各能用哪些占位符，以及预览用的样例值。
/// 样例取当前设置（时限、后果、验证方式）与此刻的时间，预览出来的话与真发的一致。
pub(crate) fn templates(current: &toml::Value) -> Value {
    let config = Config::deserialize(current.clone()).unwrap_or_default();
    let verify = &config.verify;
    let (mode, answers, question) = challenge(&verify.mode, &verify.answers);
    let sample = Pending {
        self_id: String::new(),
        group: String::new(),
        user_id: String::new(),
        name: String::new(),
        deadline: 0,
        answers,
        mode,
        question,
        reminded: Vec::new(),
        notices: Vec::new(),
        wrong: 1,
        hinted_at: 0,
        welcome: None,
    };
    let remind = verify.remind_seconds.iter().copied().max().unwrap_or(60).min(verify.timeout_seconds);
    let vars = Vars::now()
        .set("name", "小林")
        .set("user_id", "10001")
        .set("group", "示例群")
        .set("group_id", "123456")
        .set("member_count", "128")
        .set("operator", "群主")
        .set("timeout", duration_text(verify.timeout_seconds))
        .set("remaining", duration_text(remaining_round(i64::try_from(remind).unwrap_or(60))))
        .set("task", sample.task())
        .set("answer", sample.answer())
        .set("punish", punish_text(verify))
        .set("chances", if verify.attempts > 1 { format!("（还能再试 {} 次）", verify.attempts - 1) } else { String::new() });
    let placeholders: Vec<Value> = PLACEHOLDERS
        .iter()
        .map(|p| {
            json!({
                "key": p.key,
                "label": p.label,
                "hint": p.hint,
                "kinds": p.kinds.iter().map(|k| k.code()).collect::<Vec<_>>(),
            })
        })
        .collect();
    let fields = [
        ("join.text", Join, "新人进群时说的话"),
        ("leave.text", Leave, "有人自己退群时说的话"),
        ("leave.kicked_text", Kicked, "有人被移出时说的话"),
        ("verify.prompt_text", Prompt, "接在欢迎后面的验证说明"),
        ("verify.remind_text", Remind, "倒计时提醒"),
        ("verify.wrong_text", Wrong, "答错时回的话"),
        ("verify.pass_text", Pass, "通过时说的话"),
        ("verify.fail_text", Fail, "超时或答错次数用完、处理之后说的话"),
        ("groups.*.join_text", Join, "这个群的欢迎词"),
        ("groups.*.leave_text", Leave, "这个群的送别"),
        ("groups.*.kicked_text", Kicked, "这个群有人被移出时说的话"),
    ];
    let fields: serde_json::Map<String, Value> = fields
        .iter()
        .map(|(path, kind, about)| (path.to_string(), json!({ "kind": kind.code(), "about": about })))
        .collect();
    json!({
        "fields": fields,
        "placeholders": placeholders,
        "samples": vars.0,
        "extras": [
            { "insert": "{face:14}", "label": "QQ 表情", "hint": "{face:ID}：插一个 QQ 小黄脸，ID 是表情编号（14 是微笑）" },
            { "insert": "{你好|嗨}", "label": "随机片段", "hint": "{甲|乙|丙}：每次随机换成其中一个" },
        ],
    })
}

/// 控制台表单里字段的中文名。键名照旧显示在旁边，`/ctl` 用的还是它。
pub(crate) fn labels() -> Value {
    const LABELS: &[(&str, &str)] = &[
        ("channel", "生效范围"),
        ("join", "欢迎"),
        ("join.enabled", "进群时欢迎"),
        ("join.text", "欢迎词"),
        ("join.images", "配图"),
        ("join.merge_seconds", "合并欢迎（秒）"),
        ("join.recall_seconds", "欢迎自动撤回（秒）"),
        ("leave", "送别"),
        ("leave.enabled", "退群时说一声"),
        ("leave.text", "主动退群"),
        ("leave.kicked_text", "被移出"),
        ("leave.recall_seconds", "送别自动撤回（秒）"),
        ("verify", "入群验证"),
        ("verify.enabled", "新人要先验证"),
        ("verify.mode", "验证方式"),
        ("verify.answers", "口令"),
        ("verify.timeout_seconds", "时限（秒）"),
        ("verify.attempts", "可答错次数"),
        ("verify.remind_seconds", "剩这些秒时提醒"),
        ("verify.action", "超时怎么办"),
        ("verify.mute_seconds", "禁言多久（秒）"),
        ("verify.kick_permanent", "移出后不许再申请"),
        ("verify.recall_unverified", "撤回验证前的发言"),
        ("verify.prompt_text", "验证说明"),
        ("verify.remind_text", "倒计时提醒"),
        ("verify.wrong_text", "答错时"),
        ("verify.pass_text", "通过时"),
        ("verify.fail_text", "未通过时"),
        ("verify.recall_seconds", "结果自动撤回（秒）"),
        ("approve", "加群申请"),
        ("approve.enabled", "自动审批"),
        ("approve.keywords", "附言关键词"),
        ("approve.reject", "对不上就拒绝"),
        ("approve.reject_message", "拒绝理由"),
        ("groups", "按群单独设置"),
        ("groups.*.join_enabled", "进群时欢迎"),
        ("groups.*.join_text", "欢迎词"),
        ("groups.*.images", "配图"),
        ("groups.*.leave_enabled", "退群时说一声"),
        ("groups.*.leave_text", "主动退群"),
        ("groups.*.kicked_text", "被移出"),
        ("groups.*.verify_enabled", "新人要先验证"),
        ("groups.*.verify_mode", "验证方式"),
        ("groups.*.answers", "口令"),
        ("groups.*.approve_enabled", "自动审批"),
        ("groups.*.keywords", "附言关键词"),
    ];
    LABELS.iter().map(|(path, label)| (path.to_string(), Value::from(*label))).collect::<serde_json::Map<_, _>>().into()
}

/// 下拉框里每个取值的说法。
pub(crate) fn option_labels() -> Value {
    let modes = json!({ "any": "说任意一句话", "answer": "发出口令", "code": "回随机验证码", "math": "算一道加法" });
    json!({
        "verify.mode": modes,
        "groups.*.verify_mode": modes,
        "verify.action": { "kick": "移出群聊", "mute": "禁言", "none": "只记一笔，不处理" },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vars {
        Vars::now().set("name", "小林").set("group", "测试群")
    }

    #[test]
    fn pieces_split_at_image_face_and_keep_unknown_braces() {
        let parts = pieces("欢迎 {at} 来到{group}{face:14}{image}{nope}{", &vars());
        assert_eq!(
            parts,
            vec![
                Piece::Text("欢迎 ".into()),
                Piece::At,
                Piece::Text(" 来到测试群".into()),
                Piece::Face("14".into()),
                Piece::Image,
                Piece::Text("{nope}{".into()),
            ]
        );
    }

    #[test]
    fn random_fragment_picks_one_option() {
        for _ in 0..20 {
            let parts = pieces("{你好|嗨}，{name}", &vars());
            let [Piece::Text(text)] = &parts[..] else { panic!("{parts:?}") };
            assert!(text == "你好，小林" || text == "嗨，小林", "{text}");
        }
    }

    #[test]
    fn template_accepts_a_single_string_and_skips_blank_variants() {
        let one: Template = toml::from_str::<toml::Table>("t = \"你好\"").unwrap()["t"].clone().try_into().unwrap();
        assert_eq!(one, Template::of(&["你好"]));
        let blank = Template::of(&["", "  "]);
        assert!(blank.silent() && blank.pick().is_none());
    }

    #[test]
    fn numbers_must_match_whole() {
        let p = |mode: &str, answer: &str| Pending {
            self_id: String::new(),
            group: String::new(),
            user_id: String::new(),
            name: String::new(),
            deadline: 0,
            answers: vec![answer.into()],
            mode: mode.into(),
            question: String::new(),
            reminded: Vec::new(),
            notices: Vec::new(),
            wrong: 0,
            hinted_at: 0,
            welcome: None,
        };
        assert!(passes(&p("code", "0421"), "验证码 ０４２１"));
        assert!(!passes(&p("math", "12"), "123"));
        assert!(passes(&p("math", "12"), "是12吧"));
        assert!(passes(&p("answer", "我已阅读群规"), "我已 阅读 群规！"));
        assert!(!passes(&p("answer", "我已阅读群规"), "你好"));
    }

    #[test]
    fn migrate_retires_old_defaults_and_folds_code_into_mode() {
        let mut table: toml::Table = toml::from_str(
            r#"
            [verify]
            code = true
            pass_text = "{at} 验证通过，欢迎正式入群！"
            [leave]
            text = "{name} 走了"
            kicked_text = ""
            [join]
            text = "自己写的欢迎"
            "#,
        )
        .unwrap();
        assert!(Config::migrate(&mut table));
        assert_eq!(table["verify"]["mode"].as_str(), Some("code"));
        assert!(table["verify"].get("code").is_none());
        assert_eq!(table["verify"]["pass_text"].as_array().unwrap()[0].as_str(), Some(PASS_TEXT));
        assert_eq!(table["leave"]["kicked_text"].as_str(), Some("{name} 走了"));
        assert_eq!(table["join"]["text"].as_str(), Some("自己写的欢迎"));
        assert!(!Config::migrate(&mut table), "第二次没有可改的");
    }

    #[test]
    fn defaults_pass_check() {
        assert!(Config::default().check().is_ok());
    }
}
