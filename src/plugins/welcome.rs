//! 入群欢迎：进群 @ 一句欢迎（可配图），退群说一声；可选的入群验证与加群申请自动审批。
//!
//! 四件事各有各的开关，按群名单（`channel`）与每群覆盖（`groups."群号"`）收窄：
//!
//! - **欢迎 / 送别**：每条提示是一个 [`Template`]——可以写好几种说法，每次随机挑一种；
//!   占位符见 [`PLACEHOLDERS`]（`{at}` 是真 @，`{image}` `{avatar}` `{face:ID}` 是图与表情，
//!   `{甲|乙}` 是随机片段）。常用的一段话（群规、下载地址、管理名单）可以起个名字存成**自定义
//!   占位符**（`custom`，每群还能各写一份），各条提示里写 `{群规}` 就插进去，改一处处处跟着变。
//!   内置占位符里由插件来写的那几句（`{task}` 要做的事、`{punish}` 后果、`{chances}`、`{greeting}`
//!   问候、人多时的 `{name}`）也能换成自己的说法（[`Phrases`]）。
//!   同一时间涌进来好几个人时可以合并成一条（`join.merge_seconds`），
//!   发出去的欢迎可以过一会儿自己撤掉（`recall_seconds`），不占群聊。
//! - **入群验证**：新人要在时限内完成一件小事——说任意一句话、发出指定口令、回验证码或算一道
//!   加法（`verify.mode`）；倒计时从验证说明实际发出起算，期间按 `remind_seconds` 提醒。
//!   同群提醒可用 `remind_merge_seconds` 合并，新提醒发出后撤掉旧提醒；答错有提示、
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
use crate::clock::duration_text;
use crate::command::{self, CommandMatch};
use crate::event::{Context, EventType};
use crate::message::Message;
use crate::plugins::{ChannelConfig, PluginConfig, PluginError, get_config_or_default};
use crate::storage::JsonState;
use crate::template::Piece;
use futures_util::future::{BoxFuture, join_all};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsScalar};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

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
    /// 自定义占位符：名字 → 内容（也可以写几种说法随机一种）。提示里写 `{名字}` 就插进去。
    custom: Custom,
    /// 内置占位符换成的那几句话。
    phrases: Phrases,
    /// 每个群单独的文案与开关，键是群号；没写的项沿用上面的。
    groups: HashMap<String, GroupOverride>,
}

/// 自定义占位符表。按名字排着，界面与 `/ctl show` 里的先后都稳定。
type Custom = BTreeMap<String, Template>;

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
    /// 这么秒内到点的提醒合成一条；0 = 逐人提醒。无论是否合并，本群只保留最新一条。
    remind_merge_seconds: u64,
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
    /// 这个群自己的自定义占位符：同名的顶掉全局那份，没有的照用全局的。
    custom: Option<Custom>,
}

const JOIN_TEXT: &[&str] = &[
    "欢迎 {at} 加入{group}～\n群公告里有群规和常用信息，有问题直接问就好。",
    "{greeting}，{at}！欢迎来到{group}。\n先看看群公告，有什么想问的随时开口～",
    "{at} 来啦，欢迎～\n群公告里写着群规和常用信息，先看一眼，有问题直接问。",
];
const LEAVE_TEXT: &str = "{name}（{user_id}）离开了群聊。";
const KICKED_TEXT: &str = "{name}（{user_id}）已被 {operator} 移出群聊。";
const PROMPT_TEXT: &str = "请在 {timeout}内{task}，完成验证；超时会被{punish}。";
const REMIND_TEXT: &str = "{at} 验证还剩 {remaining}：请{task}，超时会被{punish}。";
const WRONG_TEXT: &str = "{at} 还不对哦，请{task}{chances}。";
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
            remind_merge_seconds: 5,
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

// ==================== 占位符的说法 ====================

/// 内置占位符里由插件来写的那几句话：`{task}` `{punish}` `{chances}` `{greeting}`，以及合并欢迎人多时的
/// `{name}`。各句都能写几种说法随机一种，里面有自己的几个小占位符（验证码、禁言多久……）与 `{甲|乙}`；
/// 全部删掉就换成空的。它们是插在别的提示中间的一句话，所以只认文字，不认 @、图片与表情。
///
/// 一张平表而不是按占位符分小表：控制台里子表总排在叶子后面，`{chances}` 会跑到 `{task}` 前头。
#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct Phrases {
    /// `{task}` 随验证方式变；`{answer}` 是口令、验证码或算术题。
    task_any: Template,
    task_answer: Template,
    task_code: Template,
    task_math: Template,
    /// `{punish}` 随「超时怎么办」变；禁言那句的 `{duration}` 是禁言多久。
    punish_kick: Template,
    punish_mute: Template,
    punish_none: Template,
    /// 限了答错次数时 `{chances}` 的说法；`{left}` 是还能再试几次。不限次数时 `{chances}` 为空。
    chances: Template,
    /// `{greeting}`：按北京时间的六段。
    greeting_morning: Template,
    greeting_forenoon: Template,
    greeting_noon: Template,
    greeting_afternoon: Template,
    greeting_evening: Template,
    greeting_night: Template,
    /// 合并欢迎超过三个人时 `{name}` 的说法：`{names}` 前三个名字、`{count}` 总人数。
    crowd: Template,
}

impl Default for Phrases {
    fn default() -> Self {
        use crate::clock::DayPart;
        let greeting = |part: DayPart| Template::of(&[part.greeting()]);
        Self {
            task_any: Template::of(&["随便说一句话"]),
            task_answer: Template::of(&["发送「{answer}」"]),
            task_code: Template::of(&["发送验证码 {answer}"]),
            task_math: Template::of(&["发出 {answer} 的答案"]),
            punish_kick: Template::of(&["移出群聊"]),
            punish_mute: Template::of(&["禁言 {duration}"]),
            punish_none: Template::of(&["记录在案"]),
            chances: Template::of(&["（还能再试 {left} 次）"]),
            greeting_morning: greeting(DayPart::Morning),
            greeting_forenoon: greeting(DayPart::Forenoon),
            greeting_noon: greeting(DayPart::Noon),
            greeting_afternoon: greeting(DayPart::Afternoon),
            greeting_evening: greeting(DayPart::Evening),
            greeting_night: greeting(DayPart::Night),
            crowd: Template::of(&["{names} 等 {count} 人"]),
        }
    }
}

impl Phrases {
    /// 每一句的键与内容：检查与控制台都照这张表。
    fn each_mut(&mut self) -> [(&'static str, &mut Template); 15] {
        [
            ("task_any", &mut self.task_any),
            ("task_answer", &mut self.task_answer),
            ("task_code", &mut self.task_code),
            ("task_math", &mut self.task_math),
            ("punish_kick", &mut self.punish_kick),
            ("punish_mute", &mut self.punish_mute),
            ("punish_none", &mut self.punish_none),
            ("chances", &mut self.chances),
            ("greeting_morning", &mut self.greeting_morning),
            ("greeting_forenoon", &mut self.greeting_forenoon),
            ("greeting_noon", &mut self.greeting_noon),
            ("greeting_afternoon", &mut self.greeting_afternoon),
            ("greeting_evening", &mut self.greeting_evening),
            ("greeting_night", &mut self.greeting_night),
            ("crowd", &mut self.crowd),
        ]
    }

    /// 每句只留头一种说法：控制台的样例与说明照它，刷新时不跳来跳去。
    fn settled(&self) -> Self {
        let mut out = self.clone();
        for (_, template) in out.each_mut() {
            template.0 = template.variants().take(1).map(str::to_owned).collect();
        }
        out
    }

    fn task(&self, p: &Pending) -> String {
        match (p.mode.as_str(), p.answers.first()) {
            ("math", _) => phrase(&self.task_math, &[("answer", p.question.clone())]),
            ("code", Some(code)) => phrase(&self.task_code, &[("answer", code.clone())]),
            (_, Some(answer)) => phrase(&self.task_answer, &[("answer", answer.clone())]),
            _ => phrase(&self.task_any, &[]),
        }
    }

    fn punish(&self, verify: &VerifyConfig) -> String {
        match verify.action.as_str() {
            "kick" => phrase(&self.punish_kick, &[]),
            "mute" => phrase(&self.punish_mute, &[("duration", duration_text(verify.mute_seconds))]),
            _ => phrase(&self.punish_none, &[]),
        }
    }

    /// `left`：还能再试几次；不限次数是 `None`，那就为空。
    fn chances(&self, left: Option<u32>) -> String {
        left.map(|left| phrase(&self.chances, &[("left", left.to_string())])).unwrap_or_default()
    }

    fn greeting(&self, part: crate::clock::DayPart) -> String {
        use crate::clock::DayPart;
        let template = match part {
            DayPart::Morning => &self.greeting_morning,
            DayPart::Forenoon => &self.greeting_forenoon,
            DayPart::Noon => &self.greeting_noon,
            DayPart::Afternoon => &self.greeting_afternoon,
            DayPart::Evening => &self.greeting_evening,
            DayPart::Night => &self.greeting_night,
        };
        phrase(template, &[])
    }

    /// 合并欢迎时 `{name}` 的说法：三个以内全写出来，再多就照 `crowd`。
    fn crowd(&self, names: &[String]) -> String {
        match names.len() {
            0..=3 => names.join("、"),
            n => phrase(&self.crowd, &[("names", names[..3].join("、")), ("count", n.to_string())]),
        }
    }
}

/// 一句说法：随机挑一种，填上它自己的小占位符；`{甲|乙}` 照样随机，认不出的原样留着。
fn phrase(template: &Template, locals: &[(&'static str, String)]) -> String {
    let Some(text) = template.pick() else {
        return String::new();
    };
    let values: HashMap<&str, String> = locals.iter().cloned().collect();
    crate::template::pieces(&text, &values)
        .into_iter()
        .filter_map(|piece| match piece {
            Piece::Text(text) => Some(text),
            _ => None,
        })
        .collect()
}

/// 说法里放了 @、图片或表情：插在一句话中间发不出来，存的时候就挡下。
fn check_phrases(phrases: &Phrases) -> Result<(), String> {
    for (path, template) in phrases.clone().each_mut() {
        for text in &template.0 {
            if text.chars().count() > 200 {
                return Err(format!("phrases.{path} 每种说法最多 200 字"));
            }
            if crate::template::pieces(text, &HashMap::<&str, String>::new()).iter().any(|p| !matches!(p, Piece::Text(_))) {
                return Err(format!("phrases.{path} 是插在提示中间的一句话，不能放 @、图片或表情"));
            }
        }
    }
    Ok(())
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
        if verify.remind_merge_seconds > 120 {
            return Err("verify.remind_merge_seconds 最多 120 秒".into());
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
        check_custom("custom", &self.custom)?;
        check_phrases(&self.phrases)?;
        for (group, o) in &self.groups {
            if group.parse::<u64>().is_err() {
                return Err("groups 的键写群号".into());
            }
            if let Some(custom) = &o.custom {
                check_custom(&format!("groups.\"{group}\".custom"), custom)?;
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
            let Some(toml::Value::Table(part)) = table.get_mut(*section) else {
                continue;
            };
            if part.get(*key).and_then(toml::Value::as_str) == Some(old)
                && let Some(new) = fresh.as_ref().and_then(|f| f.get(section)).and_then(|s| s.get(key))
            {
                part.insert((*key).to_string(), new.clone());
                changed = true;
            }
        }
        // 提示模板允许写成单个字符串，但配置统一落成「几种说法」的数组：形状校验、
        // 控制台编辑与后续读写都只面对一种形式，免得一次合法的单字符串写法把保存拦住。
        changed |= normalize_templates(table);
        changed
    }
}

/// 把写成单个字符串的提示模板改成只含一种说法的数组。
fn normalize_templates(table: &mut toml::Table) -> bool {
    fn array(value: &mut toml::Value) -> bool {
        let toml::Value::String(text) = value else {
            return false;
        };
        *value = toml::Value::Array(vec![toml::Value::String(std::mem::take(text))]);
        true
    }
    fn in_table(table: &mut toml::Table, keys: &[&str]) -> bool {
        let mut changed = false;
        for key in keys {
            if let Some(value) = table.get_mut(*key) {
                changed |= array(value);
            }
        }
        changed
    }
    fn templates(table: &mut toml::Table) -> bool {
        table.iter_mut().map(|(_, value)| array(value)).fold(false, |changed, one| changed | one)
    }

    let mut changed = false;
    for (section, keys) in [
        ("join", &["text"][..]),
        ("leave", &["text", "kicked_text"][..]),
        ("verify", &["prompt_text", "remind_text", "wrong_text", "pass_text", "fail_text"][..]),
    ] {
        if let Some(toml::Value::Table(part)) = table.get_mut(section) {
            changed |= in_table(part, keys);
        }
    }
    if let Some(toml::Value::Table(phrases)) = table.get_mut("phrases") {
        changed |= templates(phrases);
    }
    if let Some(toml::Value::Table(custom)) = table.get_mut("custom") {
        changed |= templates(custom);
    }
    if let Some(toml::Value::Table(groups)) = table.get_mut("groups") {
        for (_, group) in groups.iter_mut() {
            let Some(group) = group.as_table_mut() else {
                continue;
            };
            changed |= in_table(group, &["join_text", "leave_text", "kicked_text"]);
            if let Some(toml::Value::Table(custom)) = group.get_mut("custom") {
                changed |= templates(custom);
            }
        }
    }
    changed
}

/// 自定义占位符的名字：短、不带花括号与分隔符，也不能和内置的撞名（撞了就永远轮不到它）。
fn check_custom(path: &str, custom: &Custom) -> Result<(), String> {
    for (name, template) in custom {
        if let Some(problem) = custom_name_problem(name) {
            return Err(format!("{path} 里的「{name}」{problem}"));
        }
        if template.0.iter().any(|text| text.chars().count() > 2000) {
            return Err(format!("{path}.{name} 每种说法最多 2000 字"));
        }
    }
    Ok(())
}

/// 名字哪里不行；行就是 `None`。控制台起名对话框照同一套规则先挡一道（映射表说明里的 `reserved`）。
fn custom_name_problem(name: &str) -> Option<&'static str> {
    if name.trim().is_empty() {
        return Some("名字是空的");
    }
    if name.chars().count() > 20 {
        return Some("名字最多 20 个字");
    }
    if name.chars().any(|c| c.is_whitespace() || "{}|:.\"'".contains(c)) {
        return Some("名字里不能有空格、花括号、竖线、冒号、点和引号");
    }
    if reserved().any(|key| key == name) {
        return Some("和内置占位符重名了，换个名字");
    }
    None
}

/// 内置占位符的键：自定义的不能和它们重名。
pub(crate) fn reserved() -> impl Iterator<Item = &'static str> {
    PLACEHOLDERS.iter().map(|p| p.key).chain(["face"])
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
    custom: Custom,
}

impl Config {
    fn for_group(&self, group: &str) -> Effective {
        let o = self.groups.get(group).cloned().unwrap_or_default();
        let mut custom = self.custom.clone();
        custom.extend(o.custom.unwrap_or_default());
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
            custom,
        }
    }

    /// 这个群的提示要用的那几样：问候、时刻这些，加上本群生效的自定义占位符。
    fn vars(&self, group: &str) -> Vars {
        Vars::now(&self.phrases).custom(self.for_group(group).custom)
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
struct Vars {
    values: HashMap<&'static str, String>,
    /// 自定义占位符：内容本身也是模板，切的时候就地展开。
    custom: Custom,
}

impl Vars {
    /// 带上与人无关的那几样：问候（照设好的说法）、时刻、日期、星期。
    fn now(phrases: &Phrases) -> Self {
        let moment = crate::clock::moment();
        let mut map = HashMap::new();
        map.insert("greeting", phrases.greeting(moment.part));
        map.insert("time", moment.time);
        map.insert("date", moment.date);
        map.insert("weekday", moment.weekday);
        map.insert("count", "1".into());
        Self { values: map, custom: Custom::new() }
    }

    fn set(mut self, key: &'static str, value: impl Into<String>) -> Self {
        self.values.insert(key, value.into());
        self
    }

    fn custom(mut self, custom: Custom) -> Self {
        self.custom = custom;
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

impl crate::template::Values for Vars {
    fn value(&self, key: &str) -> Option<String> {
        self.values.get(key).cloned()
    }

    fn snippet(&self, key: &str) -> Option<String> {
        self.custom.get(key).map(|template| template.pick().unwrap_or_default())
    }
}

fn pieces(template: &str, vars: &Vars) -> Vec<Piece> {
    crate::template::pieces(template, vars)
}

/// 发给谁、配什么图：渲染一条提示要的上下文。
struct Draft<'a> {
    at: &'a [String],
    images: &'a [String],
}

/// 模板 → 消息：`{at}` 处换成对这些人的 @，图片与表情就地插入。整条只剩空白就不发（返回 `None`）。
async fn compose(ctx: &Context, writer: &LockedWriter, template: &str, vars: &Vars, draft: &Draft<'_>) -> Option<Message> {
    let parts = pieces(template, vars);
    let placed = parts.contains(&Piece::Image(None));
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
            Piece::Image(Some(src)) => {
                if let Some(file) = image(ctx, writer, std::slice::from_ref(&src)).await {
                    message = message.image(file);
                    visible = true;
                }
            }
            Piece::Image(None) => {
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

// ==================== 待验证名单 ====================

#[derive(Serialize, Deserialize, Clone)]
struct Pending {
    self_id: String,
    group: String,
    user_id: String,
    name: String,
    /// 到期时刻（Unix 秒）。欢迎尚未发出时只是等待上限，正式开始后会按发送时刻重写。
    deadline: i64,
    /// 验证说明已经随欢迎发出，倒计时正式开始。旧名单没有这个字段时按已经开始处理。
    #[serde(default = "default_started")]
    started: bool,
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
    /// 群里还挂着的答错提示（消息 ID）。倒计时提醒按群共享，另记在 [`State::reminders`]。
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

fn default_started() -> bool {
    true
}

impl Pending {
    /// 不进名单的样例：预览、控制台的说明照它出题。
    fn sample(mode: &str, answers: &[String]) -> Self {
        let (mode, answers, question) = challenge(mode, answers);
        Self {
            self_id: String::new(),
            group: String::new(),
            user_id: String::new(),
            name: String::new(),
            deadline: 0,
            started: true,
            answers,
            mode,
            question,
            reminded: Vec::new(),
            notices: Vec::new(),
            wrong: 1,
            hinted_at: 0,
            welcome: None,
        }
    }

    fn answer(&self) -> String {
        if self.mode == "math" { self.question.clone() } else { self.answers.first().cloned().unwrap_or_default() }
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

/// 本群当前挂着的一条倒计时提醒。多人合并时共用消息 ID；有人通过后先移除他的名字，
/// 最后一个人有了结果才撤回，免得一个人通过就把其他人的提醒也撤掉。
#[derive(Serialize, Deserialize, Clone)]
struct Reminder {
    self_id: String,
    group: String,
    message_id: String,
    users: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct State {
    pending: Vec<Pending>,
    #[serde(default)]
    reminders: Vec<Reminder>,
}

static STATE: JsonState<State> = JsonState::new(LOG_TARGET, "welcome", "pending.json", "入群验证名单");

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 比对用的写法：去空白（含零宽字符）、全角转半角、不分大小写。
fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '\u{200b}'..='\u{200f}' | '\u{feff}'))
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
        "code" | "math" => {
            // 一道题只答一个数：整条消息里的数字正好对上也算（「0 4 2 1」「04​21」这种
            // 被输入法、零宽字符拆开的答法），再看能否从正文里拿出完整的一段。
            let digits: String = text.chars().filter(char::is_ascii_digit).collect();
            pending.answers.iter().any(|a| a == &digits)
                || text
                    .split(|c: char| !c.is_ascii_digit())
                    .any(|number| !number.is_empty() && pending.answers.iter().any(|a| a == number))
        }
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

/// 事件里带的名字（群名片优先）；没有就是 `None`。
fn event_name(ctx: &Context, user: &str) -> Option<String> {
    let EventType::Satori(raw) = &ctx.event else {
        return None;
    };
    if raw.get_str("user_id") != Some(user) {
        return None;
    }
    for path in [["_satori", "member", "nick"], ["_satori", "user", "nick"], ["_satori", "user", "name"]] {
        if let Some(name) = raw
            .get(path[0])
            .and_then(|v| v.get(path[1]))
            .and_then(|v| v.get_str(path[2]))
            .filter(|name| !name.trim().is_empty())
        {
            return Some(name.trim().to_string());
        }
    }
    None
}

/// 事件里带的名字；没有就问一次平台，再没有用号码。
async fn display_name(ctx: &Context, writer: &LockedWriter, group: &str, user: &str) -> String {
    if let Some(name) = event_name(ctx, user) {
        return name;
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

#[derive(Clone)]
struct GroupInfo {
    id: String,
    name: String,
    members: Option<i32>,
}

/// 事件里带的群名，`guild.name` 优先、退到 `channel.name`。
///
/// 实现端推事件时就知道群名（消息事件一直带着），入群事件也带着的话，渲染 `{group}`
/// 就不必再问一次 `guild.get`。
fn event_group_name(ctx: &Context) -> Option<String> {
    let EventType::Satori(raw) = &ctx.event else {
        return None;
    };
    let satori = raw.get("_satori")?;
    ["guild", "channel"]
        .iter()
        .filter_map(|key| satori.get(*key).and_then(|value| value.get_str("name")))
        .find(|name| !name.trim().is_empty())
        .map(|name| name.trim().to_string())
}

/// 群名与人数：一次 `guild.get` 都有。群名事件里带了就用事件里的（更新，也省一次往返），
/// 读不到就是群号、人数为空。
async fn group_info(ctx: &Context, writer: &LockedWriter, group: &str) -> GroupInfo {
    let info = api::get_guild_info(ctx, writer.clone(), group).await.ok();
    let live_name = info.as_ref().map(|i| i.group_name.clone()).filter(|name| !name.is_empty());
    let name = event_group_name(ctx).or(live_name).unwrap_or_else(|| group.to_string());
    GroupInfo { id: group.to_string(), name, members: info.and_then(|i| i.member_count) }
}

/// 昵称、群名、群人数都只是欢迎词里的装饰。给它们这么久，到点还没回来就先发欢迎，
/// 用号码与群号顶上——进群那一刻群里还有人看着，欢迎晚两秒就已经没人在意了。
const ENRICH_WAIT: Duration = Duration::from_millis(1200);

/// 名字：事件里带就用它；没带就问一次平台，最多等 `wait`，等不到用号码。
async fn display_name_soon(ctx: &Context, writer: &LockedWriter, group: &str, user: &str, wait: Duration) -> String {
    if let Some(name) = event_name(ctx, user) {
        return name;
    }
    if let Ok(name) = tokio::time::timeout(wait, display_name(ctx, writer, group, user)).await {
        return name;
    }
    warn!(target: LOG_TARGET, "群 {group} 的新成员 {user} 昵称没查回来，先用号码顶上");
    user.to_string()
}

/// 群资料：同样只等 `wait`；没等回来就照事件里的群名（再没有就是群号），人数留空。
async fn group_info_soon(ctx: &Context, writer: &LockedWriter, group: &str, wait: Duration) -> GroupInfo {
    if let Ok(info) = tokio::time::timeout(wait, group_info(ctx, writer, group)).await {
        return info;
    }
    GroupInfo { id: group.to_string(), name: event_group_name(ctx).unwrap_or_else(|| group.to_string()), members: None }
}

/// 账号在本群是不是群主或管理员：查过一次会记十分钟，通常直接命中。
/// 没查回来就按普通成员算——这一次只发欢迎、不做验证（配置里本来就约定罚不动人就跳过）。
async fn own_role_soon(ctx: &Context, writer: &LockedWriter, group: &str, wait: Duration) -> String {
    if let Ok(role) = tokio::time::timeout(wait, api::own_role(ctx, writer.clone(), group)).await {
        role
    } else {
        debug!(target: LOG_TARGET, "群 {group} 的账号身份没查回来，这一次不做验证");
        String::new()
    }
}

/// 合并欢迎按账号与群分缓冲。昵称与群资料用 OnceCell 在等待窗口里预取；
/// 到点若查询还没完成就等同一个 future，不会重复请求，也不会把查询时间白白加在窗口后面。
#[derive(Clone)]
struct Joiner {
    user: String,
    name: Arc<tokio::sync::OnceCell<String>>,
}

struct JoinBatch {
    people: Vec<Joiner>,
    info: Arc<tokio::sync::OnceCell<GroupInfo>>,
}

impl JoinBatch {
    fn new() -> Self {
        Self { people: Vec::new(), info: Arc::new(tokio::sync::OnceCell::new()) }
    }
}

type BatchKey = (String, String);
type Batch = HashMap<BatchKey, JoinBatch>;

fn prefetch_name(ctx: &Context, writer: &LockedWriter, group: &str, user: &str) -> Arc<tokio::sync::OnceCell<String>> {
    let cell = Arc::new(tokio::sync::OnceCell::new());
    if let Some(name) = event_name(ctx, user).filter(|name| !name.trim().is_empty()) {
        let _ = cell.set(name);
        return cell;
    }
    let (ctx, writer, group, user, cached) = (ctx.clone(), writer.clone(), group.to_string(), user.to_string(), cell.clone());
    tokio::spawn(async move {
        cached.get_or_init(|| display_name(&ctx, &writer, &group, &user)).await;
    });
    cell
}

fn batch() -> &'static Mutex<Batch> {
    static BATCH: OnceLock<Mutex<Batch>> = OnceLock::new();
    BATCH.get_or_init(Default::default)
}

/// 把一段拼进同一条消息；欢迎与验证说明、多人合并提醒都从这里过。
fn append(target: &mut Message, part: Message) {
    if part.0.is_empty() {
        return;
    }
    if !target.0.is_empty() {
        *target = std::mem::take(target).text("\n");
    }
    target.0.extend(part.0);
}

async fn joined(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str, user: &str) -> Result<(), PluginError> {
    let effective = config.for_group(group);
    let mut verify = None;
    if effective.verify {
        let exempt = crate::access::admin_ids(ctx).iter().any(|id| id == user);
        if !exempt && api::is_staff(&own_role_soon(ctx, writer, group, ENRICH_WAIT).await) {
            verify = Some(challenge(&effective.mode, &effective.answers));
        } else if !exempt {
            debug!(target: LOG_TARGET, "群 {group} 账号不是管理员，入群验证跳过");
        }
    }
    let welcome = effective.join && !effective.join_text.silent();
    if !welcome && verify.is_none() {
        return Ok(());
    }
    let merged = welcome && config.join.merge_seconds > 0;

    // 先把待验证登记上，再发欢迎：新人在欢迎抵达前就回话也能被认出。started = false
    // 时计时器不会提醒或处罚；只有包含验证说明的消息真正发出后才开始倒计时。
    let mut pending = None;
    if let Some((mode, answers, question)) = verify.take() {
        let wait = config.verify.timeout_seconds + if merged { config.join.merge_seconds } else { 0 };
        let entry = Pending {
            self_id: ctx.bot.self_id(),
            group: group.to_string(),
            user_id: user.to_string(),
            name: event_name(ctx, user).unwrap_or_else(|| user.to_string()),
            deadline: now() + i64::try_from(wait).unwrap_or(300),
            started: false,
            answers,
            mode,
            question,
            reminded: Vec::new(),
            notices: Vec::new(),
            wrong: 0,
            hinted_at: 0,
            welcome: None,
        };
        STATE
            .with(|state| {
                state.pending.retain(|p| !(p.self_id == entry.self_id && p.group == entry.group && p.user_id == entry.user_id));
                state.pending.push(entry.clone());
            })
            .await;
        info!(target: LOG_TARGET, "群 {group} 新成员 {}（{user}）开始入群验证（{}）", entry.name, entry.mode);
        pending = Some(entry);
    }

    if merged {
        let key = (ctx.bot.self_id(), group.to_string());
        let (first, info) = {
            let mut batches = batch().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            // first 表示新开的窗口，不是当前恰好有一个人：队列里的人可能已提前退群。
            let first = !batches.contains_key(&key);
            let batch = batches.entry(key.clone()).or_insert_with(JoinBatch::new);
            if !batch.people.iter().any(|old| old.user == user) {
                batch.people.push(Joiner { user: user.to_string(), name: prefetch_name(ctx, writer, group, user) });
            }
            (first, batch.info.clone())
        };
        if first {
            let (info_ctx, info_writer, info_group) = (ctx.clone(), writer.clone(), group.to_string());
            tokio::spawn(async move {
                info.get_or_init(|| group_info(&info_ctx, &info_writer, &info_group)).await;
            });
            let (ctx, writer, config) = (ctx.clone(), writer.clone(), config.clone());
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(config.join.merge_seconds)).await;
                send_join_batch(&ctx, &writer, &config, key).await;
            });
        }
        return Ok(());
    }

    // 不合并时两项资料互不相干，并行查，但都只等到 ENRICH_WAIT；合并时这些查询全部挪到等待窗口里。
    let (name, info) = tokio::join!(
        display_name_soon(ctx, writer, group, user, ENRICH_WAIT),
        group_info_soon(ctx, writer, group, ENRICH_WAIT),
    );
    let at = [user.to_string()];
    let mut message = Message::new();
    if welcome
        && let Some(text) = effective.join_text.pick()
        && let Some(part) = compose(
            ctx,
            writer,
            &text,
            &config.vars(group).group(&info).person(user, &name),
            &Draft {
                at: &at,
                images: &effective.images,
            },
        )
        .await
    {
        append(&mut message, part);
    }
    if let Some(entry) = &pending
        && let Some(prompt) = config.verify.prompt_text.pick()
    {
        // 分开渲染再拼接：说明被删空、或只含一个展开后为空的占位符时，不留下孤零零的换行。
        let prompt = if !welcome && !prompt.contains("{at}") { format!("{{at}} {prompt}") } else { prompt };
        let vars = config
            .vars(group)
            .group(&info)
            .person(user, &name)
            .set("timeout", duration_text(config.verify.timeout_seconds))
            .set("task", config.phrases.task(entry))
            .set("answer", entry.answer())
            .set("punish", config.phrases.punish(&config.verify));
        if let Some(part) = compose(ctx, writer, &prompt, &vars, &Draft { at: &at, images: &[] }).await {
            append(&mut message, part);
        }
    }
    let message = (!message.0.is_empty()).then_some(message);
    // 有验证时欢迎先不定时撤：上面写着验证码，撤早了新人就没处看了。
    let recall = if pending.is_some() { 0 } else { config.join.recall_seconds };
    let sent = match message {
        Some(message) => match say(ctx, writer, group, message, recall).await {
            Ok(sent) => sent,
            Err(error) => {
                if pending.is_some() {
                    forget(&ctx.bot.self_id(), group, user).await;
                }
                return Err(error);
            }
        },
        None => {
            // 没有任何内容可发就等于验证说明没送到，不能留一份到点罚人的名单。
            if pending.is_some() {
                forget(&ctx.bot.self_id(), group, user).await;
            }
            None
        }
    };
    let sent_at = now();
    if pending.is_some() {
        STATE
            .update(|state| {
                let Some(p) = state.pending.iter_mut().find(|p| p.self_id == ctx.bot.self_id() && p.group == group && p.user_id == user) else {
                    return ((), false);
                };
                p.name = name;
                p.started = true;
                p.deadline = sent_at + i64::try_from(config.verify.timeout_seconds).unwrap_or(300);
                p.welcome = sent.map(|id| (id, sent_at));
                ((), true)
            })
            .await;
    }
    Ok(())
}

/// 等待窗口从第一个入群事件开始；昵称与群资料同时预取，到点通常只需渲染并发送。
async fn send_join_batch(ctx: &Context, writer: &LockedWriter, config: &Config, key: BatchKey) {
    let Some(batch) = batch().lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&key) else {
        return;
    };
    let JoinBatch { people, info } = batch;
    if people.is_empty() {
        return;
    }
    let (me, group) = key;
    let names = join_all(people.into_iter().map(|person| {
        let group = group.clone();
        async move {
            let name = if let Some(name) = person.name.get() {
                name.clone()
            } else if let Ok(name) = tokio::time::timeout(
                // 窗口里已经在查同一个名字了：等它一下（复用那次请求），超时就先用号码顶上。
                ENRICH_WAIT,
                person.name.get_or_init(|| display_name(ctx, writer, &group, &person.user)),
            )
            .await
            {
                name.clone()
            } else {
                warn!(target: LOG_TARGET, "群 {group} 的新成员 {} 昵称没查回来，先用号码顶上", person.user);
                person.user.clone()
            };
            (person.user, name)
        }
    }));
    // 窗口里预取到的直接用；没预取完也别拿整份名单的耐心等（窗口已经等过了）。
    let info_soon = async {
        match info.get() {
            Some(info) => info.clone(),
            None => group_info_soon(ctx, writer, &group, ENRICH_WAIT).await,
        }
    };
    let (people, info) = tokio::join!(names, info_soon);
    let pending: HashMap<String, Pending> = STATE
        .read(|state| {
            state
                .pending
                .iter()
                .filter(|p| p.self_id == me && p.group == group && people.iter().any(|(user, _)| user == &p.user_id))
                .cloned()
                .map(|p| (p.user_id.clone(), p))
                .collect()
        })
        .await;
    let (users, names): (Vec<String>, Vec<String>) = people.iter().cloned().unzip();
    let effective = config.for_group(&group);
    let vars = Vars::now(&config.phrases)
        .custom(effective.custom.clone())
        .group(&info)
        .person(&users.join("、"), &config.phrases.crowd(&names))
        .set("count", users.len().to_string());
    let mut message = Message::new();
    if let Some(text) = effective.join_text.pick()
        && let Some(part) = compose(
            ctx,
            writer,
            &text,
            &vars,
            &Draft {
                at: &users,
                images: &effective.images,
            },
        )
        .await
    {
        append(&mut message, part);
    }

    // 同题、同答案的人共用一段；验证码与算术题各自不同，就各占一段并明确 @ 到本人。
    let mut prompts: BTreeMap<(String, String, String), Vec<(String, String)>> = BTreeMap::new();
    for (user, name) in &people {
        if let Some(p) = pending.get(user) {
            let key = (config.phrases.task(p), p.answer(), config.phrases.punish(&config.verify));
            prompts.entry(key).or_default().push((user.clone(), name.clone()));
        }
    }
    if !prompts.is_empty()
        && let Some(prompt) = config.verify.prompt_text.pick()
    {
        let prompt = if prompt.contains("{at}") { prompt } else { format!("{{at}} {prompt}") };
        for ((task, answer, punish), people) in prompts {
            let (users, names): (Vec<String>, Vec<String>) = people.into_iter().unzip();
            let vars = Vars::now(&config.phrases)
                .custom(effective.custom.clone())
                .group(&info)
                .person(&users.join("、"), &config.phrases.crowd(&names))
                .set("timeout", duration_text(config.verify.timeout_seconds))
                .set("task", task)
                .set("answer", answer)
                .set("punish", punish);
            if let Some(part) = compose(ctx, writer, &prompt, &vars, &Draft { at: &users, images: &[] }).await {
                append(&mut message, part);
            }
        }
    }
    if message.0.is_empty() {
        for user in pending.keys() {
            forget(&me, &group, user).await;
        }
        return;
    }
    let sent = match say(ctx, writer, &group, message, 0).await {
        Ok(sent) => sent,
        Err(error) => {
            for user in pending.keys() {
                forget(&me, &group, user).await;
            }
            warn!(target: LOG_TARGET, "群 {group} 合并欢迎没发出去：{error}");
            return;
        }
    };
    let sent_at = now();
    let started = STATE
        .update(|state| {
            let mut started = false;
            let mut changed = false;
            for (user, name) in &people {
                let Some(p) = state.pending.iter_mut().find(|p| p.self_id == me && p.group == group && &p.user_id == user) else {
                    continue;
                };
                p.name = name.clone();
                p.started = true;
                p.deadline = sent_at + i64::try_from(config.verify.timeout_seconds).unwrap_or(300);
                p.welcome = sent.clone().map(|id| (id, sent_at));
                started = true;
                changed = true;
            }
            (started, changed)
        })
        .await;
    // 没有待验证的人引用这条欢迎时直接定时撤；否则等最后一个验证有结果的人撤。
    if !started
        && config.join.recall_seconds > 0
        && let Some(id) = sent
    {
        recall_later(ctx, writer, &group, id, config.join.recall_seconds);
    }
}

/// 撤销某个人的待验证记录（欢迎没发出去时用）。
async fn forget(me: &str, group: &str, user: &str) {
    STATE
        .update(|state| {
            let before = state.pending.len();
            state.pending.retain(|p| !(p.self_id == me && p.group == group && p.user_id == user));
            ((), state.pending.len() != before)
        })
        .await;
}

/// 从名单里拿走某个人；不在名单里就不写盘。
async fn take(me: &str, group: &str, user: &str) -> Option<Pending> {
    STATE
        .update(|state| {
            let Some(index) = state.pending.iter().position(|p| p.self_id == me && p.group == group && p.user_id == user) else {
                return (None, false);
            };
            (Some(state.pending.remove(index)), true)
        })
        .await
}

async fn left(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str, user: &str) -> Result<(), PluginError> {
    let me = ctx.bot.self_id();
    batch()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_mut(&(me.clone(), group.to_string()))
        .map(|batch| batch.people.retain(|person| person.user != user));
    let gone = take(&me, group, user).await;
    if let Some(p) = gone {
        settle(ctx, writer, config, &p).await;
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
    // 自己动手移出的（验证超时、清理潜水）那一处已经说过了，别再报一遍。
    if kicked && operator == me && crate::departures::expected(group, user) {
        return Ok(());
    }
    let template = if kicked { &effective.kicked_text } else { &effective.leave_text };
    let Some(text) = template.pick() else {
        return Ok(());
    };
    let name = display_name(ctx, writer, group, user).await;
    let operator_name = if kicked { display_name(ctx, writer, group, &operator).await } else { String::new() };
    let info = group_info(ctx, writer, group).await;
    let vars = config.vars(group).group(&info).person(user, &name).set("operator", operator_name);
    let at = [user.to_string()];
    if let Some(message) = compose(ctx, writer, &text, &vars, &Draft { at: &at, images: &[] }).await {
        say(ctx, writer, group, message, config.leave.recall_seconds).await?;
    }
    Ok(())
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
        Err(error) => {
            warn!(target: LOG_TARGET, "群 {group} 的加群申请（{user}）没能自动处理：{error}")
        }
    }
    Ok(())
}

/// 待验证的人说话了：答对就放行。返回 true 表示这条消息已被处理（撤回了），不再往下传。
async fn verify_message(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str, user: &str) -> Result<bool, PluginError> {
    let Some(message) = ctx.as_message() else {
        return Ok(false);
    };
    let text = message.text().to_string();
    // QQ 的小黄脸（狗头之类）只有表情段、正文是空的：只认文字就会把一条真回复当成没说话。
    let body = message.has_body();
    let message_id = message.message_id().to_string();
    let limit = config.verify.attempts;
    let me = ctx.bot.self_id();
    let outcome = STATE
        .update(|state| {
            let Some(index) = state.pending.iter().position(|p| p.self_id == me && p.group == group && p.user_id == user) else {
                return (None, false);
            };
            let (wrong, hinted_at) = (state.pending[index].wrong, state.pending[index].hinted_at);
            let step = judge(&mut state.pending[index], &text, body, limit, now());
            // 「这次不回提示」也可能记了一笔答错，得照实落盘。
            let (outcome, changed) = match step {
                Step::Passed => (Some(Outcome::Passed(state.pending.remove(index))), true),
                Step::OutOfChances => (Some(Outcome::OutOfChances(state.pending.remove(index))), true),
                Step::Hint => (Some(Outcome::Waiting(Some(state.pending[index].clone()))), true),
                Step::Silent => {
                    let p = &state.pending[index];
                    (Some(Outcome::Waiting(None)), p.wrong != wrong || p.hinted_at != hinted_at)
                }
            };
            (outcome, changed)
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

/// 一条消息怎么算。
enum Step {
    /// 答对了。
    Passed,
    /// 答错到次数用完。
    OutOfChances,
    /// 还不对：回一句提示。
    Hint,
    /// 什么都不用做。
    Silent,
}

/// 拿一条消息去比对。`body` 是「除了引用与空文字之外还带了东西没有」（QQ 表情、图片……），
/// `at` 是现在（答错提示 15 秒内只回一次）。
fn judge(p: &mut Pending, text: &str, body: bool, limit: u32, at: i64) -> Step {
    // 这个模式认的是 message-created 本身：文字、表情、图片、语音乃至只引用欢迎词都算。
    // 不能再要求引用之外另有正文，否则「任意消息」会漏掉最自然的一种回复。
    if p.mode == "any" {
        return Step::Passed;
    }
    if passes(p, text) {
        return Step::Passed;
    }
    // 要发指定内容的方式：没有文字就没法比对，不算答错、也不占次数；
    // 但得回一句告诉他该发什么——只发表情的人多半是没看懂要做什么。
    if normalize(text).is_empty() {
        return if body { hint(p, at) } else { Step::Silent };
    }
    p.wrong += 1;
    if limit > 0 && p.wrong >= limit {
        return Step::OutOfChances;
    }
    hint(p, at)
}

/// 答错提示 15 秒内只回一次：连着发错不句句都回。
const HINT_INTERVAL: i64 = 15;

fn hint(p: &mut Pending, at: i64) -> Step {
    if p.hinted_at > at - HINT_INTERVAL {
        return Step::Silent;
    }
    p.hinted_at = at;
    Step::Hint
}

enum Outcome {
    Passed(Pending),
    OutOfChances(Pending),
    /// 还在等；带着人的话要回一句答错提示。
    Waiting(Option<Pending>),
}

/// 同一群 / 同一位成员的提示串行替换，避免网络乱序让旧提示反而顶掉新提示。
fn notice_lock(key: String) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(key)
        .or_default()
        .clone()
}

/// 验证一有结果：撤掉答错提示、从共享倒计时里去掉本人；最后一个人结束时再撤掉那条提醒。
/// 合并欢迎也由最后一个仍在验证的人负责定时撤回，不能因一个人先通过就撤掉别人的验证码。
async fn settle(ctx: &Context, writer: &LockedWriter, config: &Config, p: &Pending) {
    tidy(ctx, writer, &p.group, p.notices.clone());
    let (stale, shared_welcome) = STATE
        .update(|state| {
            let mut stale = Vec::new();
            let mut changed = false;
            state.reminders.retain_mut(|reminder| {
                if reminder.self_id != p.self_id || reminder.group != p.group {
                    return true;
                }
                let before = reminder.users.len();
                reminder.users.retain(|user| user != &p.user_id);
                changed |= reminder.users.len() != before;
                if reminder.users.is_empty() {
                    stale.push(reminder.message_id.clone());
                    changed = true;
                    false
                } else {
                    true
                }
            });
            let shared = p.welcome.as_ref().is_some_and(|(id, _)| {
                state.pending.iter().any(|other| {
                    other.self_id == p.self_id && other.group == p.group && other.user_id != p.user_id && other.welcome.as_ref().is_some_and(|(other_id, _)| other_id == id)
                })
            });
            ((stale, shared), changed)
        })
        .await;
    tidy(ctx, writer, &p.group, stale);
    let recall = config.join.recall_seconds;
    if !shared_welcome
        && recall > 0
        && let Some((id, sent)) = &p.welcome
    {
        let left = (sent + i64::try_from(recall).unwrap_or(0) - now()).max(1);
        recall_later(ctx, writer, &p.group, id.clone(), left.unsigned_abs());
    }
}

async fn pass(ctx: &Context, writer: &LockedWriter, config: &Config, p: &Pending) -> Result<(), PluginError> {
    info!(target: LOG_TARGET, "群 {} 的 {}（{}）通过入群验证", p.group, p.name, p.user_id);
    settle(ctx, writer, config, p).await;
    if let Some(text) = config.verify.pass_text.pick() {
        let info = group_info(ctx, writer, &p.group).await;
        let vars = config.vars(&p.group).group(&info).person(&p.user_id, &p.name);
        let at = [p.user_id.clone()];
        if let Some(message) = compose(ctx, writer, &text, &vars, &Draft { at: &at, images: &[] }).await {
            say(ctx, writer, &p.group, message, config.verify.recall_seconds).await?;
        }
    }
    Ok(())
}

/// 答错了回一句。同一人的新提示顶替旧提示，不撤其他人的倒计时。
async fn wrong_hint(ctx: &Context, writer: &LockedWriter, config: &Config, p: &Pending) {
    let Some(text) = config.verify.wrong_text.pick() else {
        return;
    };
    let limit = config.verify.attempts;
    let chances = config.phrases.chances((limit > 0).then(|| limit.saturating_sub(p.wrong)));
    let info = group_info(ctx, writer, &p.group).await;
    let vars = config.vars(&p.group)
        .group(&info)
        .person(&p.user_id, &p.name)
        .set("task", config.phrases.task(p))
        .set("answer", p.answer())
        .set("chances", chances);
    let at = [p.user_id.clone()];
    if let Some(message) = compose(ctx, writer, &text, &vars, &Draft { at: &at, images: &[] }).await {
        notice(ctx, writer, p, message).await;
    }
}

/// 发一条答错提示，顶掉同一人的上一条。
async fn notice(ctx: &Context, writer: &LockedWriter, p: &Pending, message: Message) {
    let lock = notice_lock(format!("wrong/{}/{}/{}", p.self_id, p.group, p.user_id));
    let _guard = lock.lock().await;
    match send_msg_id(ctx, writer.clone(), Some(&p.group), None, message).await {
        Ok(id) => {
            let stale = STATE
                .update(|state| {
                    let found = state.pending.iter_mut().find(|q| q.self_id == p.self_id && q.group == p.group && q.user_id == p.user_id);
                    match found {
                        Some(found) => {
                            let stale = std::mem::replace(&mut found.notices, id.clone().into_iter().collect());
                            (stale, true)
                        }
                        // 发出去的工夫里验证已经有了结果：这条也一并撤掉。
                        None => (id.into_iter().collect(), false),
                    }
                })
                .await;
            tidy(ctx, writer, &p.group, stale);
        }
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
            // 指令后面的 @ 留在参数里，指令前面的（引用回复自带的那个）在 at_ids 里；也认直接写号码。
            let mut ids = matched.at_ids.clone();
            ids.extend(matched.args.iter().filter(|seg| seg.get_str("type") == Some("at")).filter_map(|seg| {
                let data = seg.get("data")?;
                data.get_str("qq").map(str::to_owned).or_else(|| data.get_i64("qq").map(|n| n.to_string()))
            }));
            ids.extend(rest.split_whitespace().filter(|w| w.bytes().all(|b| b.is_ascii_digit())).map(str::to_owned));
            ids.retain(|id| id != &ctx.bot.self_id());
            ids.dedup();
            if ids.is_empty() {
                reply("要放行谁？发「欢迎 放行 @某人」".into()).await?;
                return Ok(true);
            }
            let mut missed = Vec::new();
            let me = ctx.bot.self_id();
            for id in ids {
                match take(&me, group, &id).await {
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
        let mut bits = vec![format!("{} 种说法随机发", e.join_text.count())];
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
            config.phrases.punish(&config.verify),
            if staff { "" } else { "\n  ⚠ 账号不是本群管理员，验证暂不生效（只发欢迎）" }
        ));
    } else {
        lines.push("· 入群验证：关".into());
    }
    lines.push(format!("· 申请审批：{}", onoff(e.approve)));
    if !e.custom.is_empty() {
        let names: Vec<String> = e.custom.keys().map(|name| format!("{{{name}}}")).collect();
        lines.push(format!("· 自定义占位符：{}", names.join("、")));
    }
    let me = ctx.bot.self_id();
    let pending: Vec<Pending> = STATE
        .read(|state| state.pending.iter().filter(|p| p.group == group && p.self_id == me).cloned().collect())
        .await;
    if !pending.is_empty() {
        let now = now();
        let names: Vec<String> = pending
            .iter()
            .map(|p| {
                if p.started {
                    format!("{}（还剩 {}）", p.name, duration_text(remaining_round(p.deadline - now)))
                } else {
                    format!("{}（等待欢迎发出）", p.name)
                }
            })
            .collect();
        lines.push(format!("· 待验证 {} 人：{}", pending.len(), names.join("、")));
    }
    lines.push("发「欢迎 预览」看看新人会收到什么。".into());
    lines.join("\n")
}

/// 剩余秒数凑个整：倒计时说「还剩 1 分钟」而不是「还剩 58 秒」（计时器 5 秒一跳，到点时总差几秒）。
/// 两分钟以上往下取整到分钟——宁可说少，别让人以为还有 5 分钟、其实只剩 4 分多。
fn remaining_round(seconds: i64) -> u64 {
    let seconds = seconds.max(0).unsigned_abs();
    if seconds >= 120 { seconds / 60 * 60 } else { seconds.div_ceil(10) * 10 }
}

/// 把一条提示对指令发出人发一遍，和真的一模一样（只是不开始验证、不定时撤回）。
async fn preview(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str, me: &str, what: &str) -> Result<(), String> {
    let e = config.for_group(group);
    let verify = &config.verify;
    let welcome_preview = matches!(what, "" | "欢迎");
    let (template, images): (Template, Vec<String>) = match what {
        "" | "欢迎" => (e.join_text.clone(), e.images.clone()),
        "送别" | "退群" => (e.leave_text.clone(), Vec::new()),
        "移出" | "踢" => (e.kicked_text.clone(), Vec::new()),
        "验证" => (verify.prompt_text.clone(), Vec::new()),
        "提醒" | "倒计时" => (verify.remind_text.clone(), Vec::new()),
        "答错" => (verify.wrong_text.clone(), Vec::new()),
        "通过" => (verify.pass_text.clone(), Vec::new()),
        "超时" | "失败" => (verify.fail_text.clone(), Vec::new()),
        _ => return Err(format!("没有「{what}」这条提示\n{}", usage())),
    };
    let text = template.pick();
    if text.is_none() && !welcome_preview {
        return Err("这条提示留空了，真遇到时不会发".into());
    }
    let name = display_name(ctx, writer, group, me).await;
    let info = group_info(ctx, writer, group).await;
    let sample = Pending::sample(&e.mode, &e.answers);
    let remind = verify.remind_seconds.iter().copied().max().unwrap_or(60).min(verify.timeout_seconds);
    let vars = config.vars(group)
        .group(&info)
        .person(me, &name)
        .set("operator", ctx.as_message().map(|m| m.sender_name().to_string()).unwrap_or_default())
        .set("timeout", duration_text(verify.timeout_seconds))
        .set("remaining", duration_text(remaining_round(i64::try_from(remind).unwrap_or(60))))
        .set("task", config.phrases.task(&sample))
        .set("answer", sample.answer())
        .set("punish", config.phrases.punish(verify))
        .set("chances", config.phrases.chances((verify.attempts > 1).then(|| verify.attempts - 1)));
    let at = [me.to_string()];
    let mut message = Message::new();
    if let Some(text) = text
        && let Some(part) = compose(ctx, writer, &text, &vars, &Draft { at: &at, images: &images }).await
    {
        append(&mut message, part);
    }
    if welcome_preview
        && e.verify
        && let Some(prompt) = verify.prompt_text.pick()
    {
        let prompt = if message.0.is_empty() && !prompt.contains("{at}") { format!("{{at}} {prompt}") } else { prompt };
        if let Some(part) = compose(ctx, writer, &prompt, &vars, &Draft { at: &at, images: &[] }).await {
            append(&mut message, part);
        }
    }
    if message.0.is_empty() {
        return Err("这条提示渲染出来是空的".into());
    }
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
    Remind(Pending),
    Expire(Pending),
    Drop(Pending),
}

type ReminderBatch = HashMap<BatchKey, Vec<Pending>>;

fn reminder_batch() -> &'static Mutex<ReminderBatch> {
    static BATCH: OnceLock<Mutex<ReminderBatch>> = OnceLock::new();
    BATCH.get_or_init(Default::default)
}

/// 到点提醒进入一个小窗口，同群同窗口的人只发一条。0 秒时逐人发送，但仍按群撤旧留新。
async fn queue_reminder(ctx: &Context, writer: &LockedWriter, config: &Config, p: Pending) {
    if config.verify.remind_merge_seconds == 0 {
        send_reminders(ctx, writer, config, (p.self_id.clone(), p.group.clone()), vec![p]).await;
        return;
    }
    let key = (p.self_id.clone(), p.group.clone());
    let first = {
        let mut batches = reminder_batch().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let people = batches.entry(key.clone()).or_default();
        if let Some(old) = people.iter_mut().find(|old| old.user_id == p.user_id) {
            *old = p;
            false
        } else {
            people.push(p);
            people.len() == 1
        }
    };
    if first {
        let (ctx, writer, config) = (ctx.clone(), writer.clone(), config.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(config.verify.remind_merge_seconds)).await;
            let people = reminder_batch().lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&key).unwrap_or_default();
            send_reminders(&ctx, &writer, &config, key, people).await;
        });
    }
}

/// 发一批倒计时：相同题目与剩余时间的人共享一段，不同验证码 / 算术题分段写在同一条里。
async fn send_reminders(ctx: &Context, writer: &LockedWriter, config: &Config, key: BatchKey, requested: Vec<Pending>) {
    if requested.is_empty() || !config.enabled || !config.channel.allows_group(&key.1) || !config.for_group(&key.1).verify {
        return;
    }
    let Some(text) = config.verify.remind_text.pick() else {
        return;
    };
    // 从准备到登记新 ID 都串行，保证后发出的那条一定顶掉先发出的那条。
    let lock = notice_lock(format!("reminder/{}/{}", key.0, key.1));
    let _guard = lock.lock().await;
    let current: HashMap<String, Pending> = STATE
        .read(|state| {
            state
                .pending
                .iter()
                .filter(|p| p.self_id == key.0 && p.group == key.1 && p.started)
                .cloned()
                .map(|p| (p.user_id.clone(), p))
                .collect()
        })
        .await;
    let mut pending: Vec<Pending> = Vec::new();
    for request in requested {
        if pending.iter().any(|p| p.user_id == request.user_id) {
            continue;
        }
        if let Some(p) = current.get(&request.user_id) {
            pending.push(p.clone());
        }
    }
    if pending.is_empty() {
        return;
    }
    let info = group_info(ctx, writer, &key.1).await;
    let tick_at = now();
    let punish = config.phrases.punish(&config.verify);
    let mut sections: BTreeMap<(String, String, String, String), Vec<Pending>> = BTreeMap::new();
    for p in pending {
        if p.deadline <= tick_at {
            continue;
        }
        let remaining = duration_text(remaining_round(p.deadline - tick_at));
        let key = (remaining, config.phrases.task(&p), p.answer(), punish.clone());
        sections.entry(key).or_default().push(p);
    }
    let mut message = Message::new();
    let mut recipients = Vec::new();
    for ((remaining, task, answer, punish), people) in sections {
        let (users, names): (Vec<String>, Vec<String>) = people.iter().map(|p| (p.user_id.clone(), p.name.clone())).unzip();
        let vars = config
            .vars(&key.1)
            .group(&info)
            .person(&users.join("、"), &config.phrases.crowd(&names))
            .set("remaining", remaining)
            .set("task", task)
            .set("answer", answer)
            .set("punish", punish);
        if let Some(part) = compose(ctx, writer, &text, &vars, &Draft { at: &users, images: &[] }).await {
            append(&mut message, part);
            recipients.extend(users);
        }
    }
    if message.0.is_empty() || recipients.is_empty() {
        return;
    }
    match send_msg_id(ctx, writer.clone(), Some(&key.1), None, message).await {
        Ok(id) => {
            let checked_at = now();
            let stale = STATE
                .update(|state| {
                    let mut stale = Vec::new();
                    state.reminders.retain(|old| {
                        if old.self_id == key.0 && old.group == key.1 {
                            stale.push(old.message_id.clone());
                            false
                        } else {
                            true
                        }
                    });
                    let users: Vec<String> = recipients
                        .iter()
                        .filter(|user| {
                            state
                                .pending
                                .iter()
                                .any(|p| p.self_id == key.0 && p.group == key.1 && &p.user_id == *user && p.started && p.deadline > checked_at)
                        })
                        .cloned()
                        .collect();
                    match id {
                        Some(id) if !users.is_empty() => state.reminders.push(Reminder {
                            self_id: key.0.clone(),
                            group: key.1.clone(),
                            message_id: id,
                            users,
                        }),
                        Some(id) => stale.push(id),
                        None => {}
                    }
                    (stale, true)
                })
                .await;
            // 新提醒确实发出后再撤旧提醒；失败时旧的那条仍然保留。
            tidy(ctx, writer, &key.1, stale);
        }
        Err(error) => warn!(target: LOG_TARGET, "群 {} 倒计时提醒没发出去：{error}", key.1),
    }
}

async fn tick(ctx: &Context, writer: &LockedWriter) {
    let config = get_config_or_default::<Config>(ctx);
    let me = ctx.bot.self_id();
    let now = now();
    let mut remind_points = config.verify.remind_seconds.clone();
    remind_points.sort_unstable_by(|a, b| b.cmp(a));
    let due = STATE
        .update(|state| {
            let mut due = Vec::new();
            let mut changed = false;
            state.pending.retain_mut(|p| {
                if p.self_id != me {
                    return true;
                }
                // 插件关了、或这个群不再验证：名单作废，不罚。
                if !config.enabled || !config.channel.allows_group(&p.group) || !config.for_group(&p.group).verify {
                    due.push(Due::Drop(p.clone()));
                    changed = true;
                    return false;
                }
                if !p.started {
                    // 合并任务若因重启丢失，等待上限后安全作废，绝不处罚没收到说明的人。
                    if p.deadline <= now {
                        due.push(Due::Drop(p.clone()));
                        changed = true;
                        return false;
                    }
                    return true;
                }
                let remaining = p.deadline - now;
                if remaining <= 0 {
                    due.push(Due::Expire(p.clone()));
                    changed = true;
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
                            changed = true;
                        }
                    }
                    due.push(Due::Remind(p.clone()));
                }
                true
            });
            // 大多数跳什么都没变（名单空着或还早），不必每 5 秒重写一遍文件。
            (due, changed)
        })
        .await;
    for item in due {
        match item {
            Due::Remind(p) => queue_reminder(ctx, writer, &config, p).await,
            Due::Expire(p) => expire(ctx, writer, &config, p).await,
            Due::Drop(p) => settle(ctx, writer, &config, &p).await,
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
    settle(ctx, writer, config, &p).await;
    let result = match verify.action.as_str() {
        "kick" => {
            crate::departures::expect(&p.group, &p.user_id);
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
    let Some(text) = verify.fail_text.pick() else {
        return;
    };
    let info = group_info(ctx, writer, &p.group).await;
    let vars = config.vars(&p.group)
        .group(&info)
        .person(&p.user_id, &p.name)
        .set("timeout", duration_text(verify.timeout_seconds))
        .set("punish", config.phrases.punish(verify));
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
            config.phrases.punish(&config.verify)
        ));
        match STATE.peek() {
            Some(state) if !state.pending.is_empty() => {
                let now = now();
                let names: Vec<String> = state
                    .pending
                    .iter()
                    .take(8)
                    .map(|p| {
                        if p.started {
                            format!("{}（群 {}，还剩 {}）", p.name, p.group, duration_text(remaining_round(p.deadline - now)))
                        } else {
                            format!("{}（群 {}，等待欢迎发出）", p.name, p.group)
                        }
                    })
                    .collect();
                lines.push(format!("待验证 {} 人：{}", state.pending.len(), names.join("、")));
            }
            Some(_) => lines.push("现在没有待验证的新人。".to_string()),
            None => {}
        }
    }
    if !config.custom.is_empty() {
        let names: Vec<String> = config.custom.keys().take(8).map(|name| format!("{{{name}}}")).collect();
        lines.push(format!("自定义占位符 {} 个：{}。", config.custom.len(), names.join("、")));
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
    let phrases = config.phrases.settled();
    let sample = Pending::sample(&verify.mode, &verify.answers);
    let remind = verify.remind_seconds.iter().copied().max().unwrap_or(60).min(verify.timeout_seconds);
    let chances = |left: u32| phrases.chances(Some(left));
    let vars = Vars::now(&phrases)
        .set("name", "小林")
        .set("user_id", "10001")
        .set("group", "示例群")
        .set("group_id", "123456")
        .set("member_count", "128")
        .set("operator", "群主")
        .set("timeout", duration_text(verify.timeout_seconds))
        .set("remaining", duration_text(remaining_round(i64::try_from(remind).unwrap_or(60))))
        .set("task", phrases.task(&sample))
        .set("answer", sample.answer())
        .set("punish", phrases.punish(verify))
        .set("chances", if verify.attempts > 1 { chances(verify.attempts - 1) } else { String::new() });
    // {task} {punish} 这几个的说明照现在的说法写，改了说法芯片上看得见。
    let mut hints: HashMap<&str, String> = HashMap::new();
    let probe = |mode: &str, answers: &[&str], question: &str| {
        let answers = answers.iter().map(ToString::to_string).collect();
        phrases.task(&Pending { mode: mode.into(), answers, question: question.into(), ..Pending::sample("any", &[]) })
    };
    let tasks = [probe("any", &[], ""), probe("answer", &[PHRASE_ANSWER], ""), probe("code", &["4821"], ""), probe("math", &["12"], "7 + 5")];
    hints.insert("task", format!("随验证方式变：{}。说法在「占位符的说法」里改", tasks.join(" / ")));
    let punishes: Vec<String> = ["kick", "mute", "none"]
        .iter()
        .map(|action| phrases.punish(&VerifyConfig { action: (*action).into(), ..verify.clone() }))
        .collect();
    hints.insert("punish", format!("随「超时怎么办」变：{}。说法在「占位符的说法」里改", punishes.join(" / ")));
    hints.insert("chances", format!("限了次数时是「{}」，不限时为空。说法在「占位符的说法」里改", chances(2)));
    let greetings: Vec<String> = DAY_PARTS.iter().map(|part| phrases.greeting(*part)).collect();
    hints.insert("greeting", format!("按北京时间：{}。说法在「占位符的说法」里改", greetings.join("、")));
    // 自定义占位符的内容里能用全部内置占位符（插到哪种提示里，就照那种提示的值填），所以内置的都多认一种 custom。
    let mut placeholders: Vec<Value> = PLACEHOLDERS
        .iter()
        .map(|p| {
            let mut kinds: Vec<&str> = p.kinds.iter().map(|k| k.code()).collect();
            kinds.push("custom");
            let hint = hints.get(p.key).map_or(p.hint, String::as_str);
            json!({ "key": p.key, "label": p.label, "hint": hint, "kinds": kinds })
        })
        .collect();
    // 说法里自己的小占位符：只在那一句里认得。
    placeholders.extend(PHRASE_LOCALS.iter().map(|(key, label, hint, kinds)| {
        let kinds: Vec<String> = kinds.iter().map(|k| format!("phrase_{k}")).collect();
        json!({ "key": key, "label": label, "hint": hint, "kinds": kinds })
    }));
    let every: Vec<&str> = Kind::ALL.iter().map(|k| k.code()).collect();
    let custom_holder = |name: &str, template: &Template| {
        let first = template.variants().next().unwrap_or_default();
        let hint = crate::storage::truncate_chars(&first.split_whitespace().collect::<Vec<_>>().join(" "), 40);
        json!({ "key": name, "label": name, "hint": hint, "kinds": every, "custom": true })
    };
    placeholders.extend(config.custom.iter().map(|(name, template)| custom_holder(name, template)));
    // 每群自己的那份：只在那个群的提示里认得出，预览也照那个群的内容。
    let group_custom: serde_json::Map<String, Value> = config
        .groups
        .iter()
        .filter_map(|(group, o)| {
            let custom = o.custom.as_ref().filter(|c| !c.is_empty())?;
            let holders: Vec<Value> = custom.iter().map(|(name, template)| custom_holder(name, template)).collect();
            let samples: serde_json::Map<String, Value> =
                custom.iter().map(|(name, t)| (name.clone(), t.variants().next().unwrap_or_default().into())).collect();
            Some((group.clone(), json!({ "placeholders": holders, "samples": samples })))
        })
        .collect();
    let mut samples: serde_json::Map<String, Value> = vars.values.iter().map(|(k, v)| ((*k).to_string(), v.clone().into())).collect();
    for (name, template) in &config.custom {
        samples.insert(name.clone(), template.variants().next().unwrap_or_default().into());
    }
    let fields = [
        ("join.text", Join, "新人进群时 @ 他说的话。写几种说法，每次随机发一种，天天进人也不像复读机；全部删掉就不发。"),
        ("leave.text", Leave, "有人自己退群时说的话；全部删掉就不说。"),
        ("leave.kicked_text", Kicked, "有人被移出时说的话；全部删掉就不说。"),
        ("verify.prompt_text", Prompt, "接在欢迎后面的验证说明，告诉新人要做什么、有多久、超时会怎样。"),
        ("verify.remind_text", Remind, "倒计时提醒：到了「剩这些秒时提醒」里的时间点就发，新的一条顶替旧的。"),
        ("verify.wrong_text", Wrong, "答错时回的话，15 秒内只回一次；全部删掉就不回。"),
        ("verify.pass_text", Pass, "通过时说的话；全部删掉就不说。"),
        ("verify.fail_text", Fail, "超时或答错次数用完、处理之后说的话；全部删掉就不说。"),
        ("groups.*.join_text", Join, "这个群单独的欢迎词。"),
        ("groups.*.leave_text", Leave, "这个群有人自己退群时说的话。"),
        ("groups.*.kicked_text", Kicked, "这个群有人被移出时说的话。"),
    ];
    let mut fields: serde_json::Map<String, Value> = fields
        .iter()
        .map(|(path, kind, about)| (path.to_string(), json!({ "kind": kind.code(), "about": about })))
        .collect();
    let about = "这段内容插到哪条提示里，里面的占位符就照那条提示的值填。写几种说法时每次随机一种；全部删掉 = 插进去是空的。";
    fields.insert("custom.*".into(), json!({ "kind": "custom", "about": about }));
    fields.insert("groups.*.custom.*".into(), json!({ "kind": "custom", "about": about }));
    let answer = verify.answers.iter().find(|a| !a.trim().is_empty()).cloned().unwrap_or_else(|| PHRASE_ANSWER.into());
    // 说法只是一句话，单独看不出放进去顺不顺：预览套进头一条真用到它的提示里。
    let (prompt, remind, wrong) = (("验证说明", Prompt, &verify.prompt_text), ("倒计时提醒", Remind, &verify.remind_text), ("答错时", Wrong, &verify.wrong_text));
    let (join, pass, fail) = (("欢迎词", Join, &config.join.text), ("通过时", Pass, &verify.pass_text), ("未通过时", Fail, &verify.fail_text));
    let host = |slot: &str, sources: &[(&'static str, Kind, &Template)]| {
        let mark = format!("{{{slot}}}");
        sources.iter().find_map(|(label, kind, template)| {
            let text = template.variants().find(|text| text.contains(&mark))?;
            Some((*label, json!({ "text": text, "kind": kind.code(), "slot": slot })))
        })
    };
    for (key, title, about) in PHRASE_FIELDS {
        let host = match key.split('_').next() {
            Some("task") => host("task", &[prompt, remind, wrong]),
            Some("punish") => host("punish", &[prompt, remind, fail]),
            Some("chances") => host("chances", &[wrong]),
            Some("greeting") => host("greeting", &[join, prompt, pass]),
            _ => host("name", &[join]),
        };
        let about = match &host {
            Some((label, _)) => format!("{about}预览套在「{label}」里，垫淡底的是这句。"),
            None => (*about).to_string(),
        };
        let samples = match *key {
            "task_answer" => json!({ "answer": answer }),
            "task_code" => json!({ "answer": "4821" }),
            "task_math" => json!({ "answer": "7 + 5" }),
            "punish_mute" => json!({ "duration": duration_text(verify.mute_seconds) }),
            "chances" => json!({ "left": "2" }),
            "crowd" => json!({ "names": "小林、阿杰、小美", "count": "5" }),
            _ => json!({}),
        };
        fields.insert(
            format!("phrases.{key}"),
            json!({
                "kind": format!("phrase_{key}"), "about": about, "title": title, "plain": true, "samples": samples,
                "host": host.map(|(_, host)| host), "silent": "一种说法都没有：这个占位符换成空的。",
            }),
        );
    }
    json!({
        "fields": fields,
        "placeholders": placeholders,
        "samples": samples,
        "group_custom": group_custom,
        "extras": [
            { "insert": "{face:14}", "label": "QQ 表情", "hint": "{face:ID}：插一个 QQ 小黄脸，ID 是表情编号（14 是微笑）", "kinds": every.iter().copied().chain(["custom"]).collect::<Vec<_>>() },
            { "insert": "{你好|嗨}", "label": "随机片段", "hint": "{甲|乙|丙}：每次随机换成其中一个" },
        ],
    })
}

/// 口令方式没写口令时，样例里拿来顶的那句。
const PHRASE_ANSWER: &str = "我已阅读群规";

const DAY_PARTS: [crate::clock::DayPart; 6] = {
    use crate::clock::DayPart::{Afternoon, Evening, Forenoon, Morning, Night, Noon};
    [Morning, Forenoon, Noon, Afternoon, Evening, Night]
};

/// 占位符的说法：键、编辑器标题、一句用途。先后与配置参考一致。
const PHRASE_FIELDS: &[(&str, &str, &str)] = &[
    ("task_any", "{task} · 说任意一句话", "验证方式是「说任意一句话」时，{task} 换成这句。"),
    ("task_answer", "{task} · 发出口令", "验证方式是「发出口令」时；{answer} 是口令（写了几句时用第一句）。"),
    ("task_code", "{task} · 回验证码", "验证方式是「回随机验证码」时；{answer} 是这次的 4 位验证码。"),
    ("task_math", "{task} · 算加法", "验证方式是「算一道加法」时；{answer} 是题目，如 7 + 5。"),
    ("punish_kick", "{punish} · 移出", "超时选「移出群聊」时，{punish} 换成这句。"),
    ("punish_mute", "{punish} · 禁言", "超时选「禁言」时；{duration} 是禁言多久，如 1 天。"),
    ("punish_none", "{punish} · 只记一笔", "超时选「只记一笔，不处理」时。"),
    ("chances", "{chances}", "限了答错次数时接在答错提示里；{left} 是还能再试几次。不限次数时 {chances} 是空的。"),
    ("greeting_morning", "{greeting} · 早上", "5–8 点（北京时间）的问候。"),
    ("greeting_forenoon", "{greeting} · 上午", "9–10 点的问候。"),
    ("greeting_noon", "{greeting} · 中午", "11–12 点的问候。"),
    ("greeting_afternoon", "{greeting} · 下午", "13–17 点的问候。"),
    ("greeting_evening", "{greeting} · 晚上", "18–22 点的问候。"),
    ("greeting_night", "{greeting} · 深夜", "23 点到凌晨 4 点的问候。"),
    ("crowd", "{name} · 人多时", "合并欢迎超过三个人时 {name} 换成这句；{names} 是前三个名字，{count} 是总人数。三个以内直接列全。"),
];

/// 说法里自己的小占位符：键、芯片上的名字、说明、能出现在哪几句（`phrases.` 之后的键）。
const PHRASE_LOCALS: &[(&str, &str, &str, &[&str])] = &[
    ("answer", "口令", "写了几句口令时是第一句", &["task_answer"]),
    ("answer", "验证码", "这次的 4 位验证码，如 4821", &["task_code"]),
    ("answer", "题目", "算术题本身，如 7 + 5", &["task_math"]),
    ("duration", "禁言时长", "照「禁言多久」，如 1 天", &["punish_mute"]),
    ("left", "还能试几次", "如 2", &["chances"]),
    ("names", "前三个名字", "如 小林、阿杰、小美", &["crowd"]),
    ("count", "总人数", "这一批一共几个人", &["crowd"]),
];

/// 控制台表单里字段的中文名。键名照旧显示在旁边，`/ctl` 用的还是它。
pub(crate) const LABELS: &[(&str, &str)] = &[
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
    ("verify.remind_merge_seconds", "合并提醒（秒）"),
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
    ("custom", "自定义占位符"),
    ("phrases", "占位符的说法"),
    ("phrases.task_any", "要做的事 · 说任意一句话"),
    ("phrases.task_answer", "要做的事 · 发出口令"),
    ("phrases.task_code", "要做的事 · 回验证码"),
    ("phrases.task_math", "要做的事 · 算加法"),
    ("phrases.punish_kick", "后果 · 移出"),
    ("phrases.punish_mute", "后果 · 禁言"),
    ("phrases.punish_none", "后果 · 只记一笔"),
    ("phrases.chances", "剩余次数"),
    ("phrases.greeting_morning", "问候 · 早上"),
    ("phrases.greeting_forenoon", "问候 · 上午"),
    ("phrases.greeting_noon", "问候 · 中午"),
    ("phrases.greeting_afternoon", "问候 · 下午"),
    ("phrases.greeting_evening", "问候 · 晚上"),
    ("phrases.greeting_night", "问候 · 深夜"),
    ("phrases.crowd", "人多时的昵称"),
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
    ("groups.*.custom", "本群的自定义占位符"),
];

/// 只在某个取值下才用得到的项：界面在用不到时说一句，免得人改了半天没反应。
pub(crate) fn depends() -> Value {
    let checks = ["answer", "code", "math"];
    json!({
        "verify.answers": { "on": "verify.mode", "in": ["answer"] },
        "verify.attempts": { "on": "verify.mode", "in": checks },
        "verify.wrong_text": { "on": "verify.mode", "in": checks },
        "verify.mute_seconds": { "on": "verify.action", "in": ["mute"] },
        "phrases.punish_kick": { "on": "verify.action", "in": ["kick"] },
        "phrases.punish_mute": { "on": "verify.action", "in": ["mute"] },
        "phrases.punish_none": { "on": "verify.action", "in": ["none"] },
        "verify.kick_permanent": { "on": "verify.action", "in": ["kick"] },
    })
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
    use simd_json::prelude::ValueAsScalar;

    #[test]
    fn custom_placeholders_expand_once_and_group_ones_win() {
        let mut config = Config::default();
        config.custom.insert("群规".into(), Template::of(&["不发广告，{group}欢迎你{face:1}"]));
        config.custom.insert("套娃".into(), Template::of(&["{群规}"]));
        config.groups.insert(
            "1".into(),
            GroupOverride { custom: Some(Custom::from([("群规".into(), Template::of(&["一群的规矩"]))])), ..Default::default() },
        );
        let vars = config.vars("2").set("group", "测试群");
        assert_eq!(
            pieces("看{群规}", &vars),
            vec![Piece::Text("看不发广告，测试群欢迎你".into()), Piece::Face("1".into())]
        );
        assert_eq!(pieces("{套娃}", &vars), vec![Piece::Text("{群规}".into())], "自定义里不再套自定义");
        assert_eq!(pieces("{群规}", &config.vars("1")), vec![Piece::Text("一群的规矩".into())]);
    }

    #[test]
    fn phrases_replace_builtin_wording() {
        let mut config = Config::default();
        config.phrases.task_code = Template::of(&["回复 {answer} 就行"]);
        config.phrases.punish_mute = Template::of(&["关小黑屋 {duration}"]);
        config.phrases.chances = Template::of(&[""]);
        config.verify.action = "mute".into();
        let mut p = Pending::sample("code", &[]);
        p.answers = vec!["0421".into()];
        assert_eq!(config.phrases.task(&p), "回复 0421 就行");
        assert_eq!(config.phrases.punish(&config.verify), "关小黑屋 1 天");
        assert_eq!(config.phrases.chances(Some(2)), "", "全部删掉就是空的");
        let fresh = Phrases::default();
        assert_eq!(fresh.chances(Some(2)), "（还能再试 2 次）");
        assert_eq!(fresh.chances(None), "", "不限次数时为空");
        assert_eq!(fresh.task(&Pending::sample("math", &[])).split(' ').count(), 5, "发出 a + b 的答案");
        let names = ["甲", "乙", "丙", "丁"].map(String::from);
        assert_eq!(fresh.crowd(&names), "甲、乙、丙 等 4 人");
        assert_eq!(fresh.crowd(&names[..2]), "甲、乙");
        config.phrases.greeting_night = Template::of(&["还没睡呀"]);
        assert_eq!(config.phrases.greeting(crate::clock::DayPart::Night), "还没睡呀");
        assert!(config.check().is_ok());
        config.phrases.task_any = Template::of(&["说句话{face:1}"]);
        assert!(config.check().is_err(), "说法里不能放表情");
    }

    #[test]
    fn custom_names_cannot_shadow_builtins() {
        assert!(custom_name_problem("群规").is_none());
        assert!(custom_name_problem("name").is_some());
        assert!(custom_name_problem("a b").is_some());
        assert!(custom_name_problem("a|b").is_some());
        let mut config = Config::default();
        config.custom.insert("group".into(), Template::of(&["x"]));
        assert!(config.check().is_err());
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
            started: true,
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
        // 被空格、零宽字符拆开的答法也算（输入法、复制粘贴常见）。
        assert!(passes(&p("code", "0421"), "0 4 2 1"));
        assert!(passes(&p("code", "4821"), "验证码 4\u{200b}821"));
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
            [groups."123"]
            kicked_text = "这个群有人走了"
            [groups."123".custom]
            "群规" = "不发广告"
            "#,
        )
        .unwrap();
        assert!(Config::migrate(&mut table));
        assert_eq!(table["verify"]["mode"].as_str(), Some("code"));
        assert!(table["verify"].get("code").is_none());
        assert_eq!(table["verify"]["pass_text"].as_array().unwrap()[0].as_str(), Some(PASS_TEXT));
        let root = toml::Value::Table(table.clone());
        for (path, want) in [
            ("leave.kicked_text", "{name} 走了"),
            ("join.text", "自己写的欢迎"),
            ("groups.\"123\".kicked_text", "这个群有人走了"),
            ("groups.\"123\".custom.群规", "不发广告"),
        ] {
            let value = path.split('.').fold(&root, |node, key| &node[key.trim_matches('"')]);
            assert_eq!(value.as_array().unwrap()[0].as_str(), Some(want), "{path} 应落成数组");
        }
        assert!(!Config::migrate(&mut table), "第二次没有可改的");
    }

    #[test]
    fn face_only_replies_are_counted() {
        let sample = |mode: &str, answer: &str| Pending {
            self_id: String::new(),
            group: String::new(),
            user_id: String::new(),
            name: String::new(),
            deadline: 0,
            started: true,
            answers: if mode == "any" { Vec::new() } else { vec![answer.into()] },
            mode: mode.into(),
            question: String::new(),
            reminded: Vec::new(),
            notices: Vec::new(),
            wrong: 0,
            hinted_at: 0,
            welcome: None,
        };

        // 「说任意一句话」：只丢一个 QQ 表情（狗头）也是人来了，正文是空的也算。
        let mut any = sample("any", "");
        assert!(matches!(judge(&mut any, "", true, 0, 1000), Step::Passed));
        assert!(matches!(judge(&mut any, "在的", false, 0, 1000), Step::Passed));
        assert!(matches!(judge(&mut any, "", false, 0, 1000), Step::Passed), "只引用欢迎词也算任意消息");

        // 要发指定内容时，表情没法比对：不算答错、不占次数，但要回一句告诉他发什么。
        let mut code = sample("code", "0421");
        assert!(matches!(judge(&mut code, "", true, 2, 1000), Step::Hint));
        assert_eq!(code.wrong, 0, "发表情不占答错次数");
        assert!(matches!(judge(&mut code, "", true, 2, 1005), Step::Silent), "15 秒内只回一次");
        assert!(matches!(judge(&mut code, "", true, 2, 1020), Step::Hint));
        assert!(matches!(judge(&mut code, "你好", false, 2, 1030), Step::Silent), "刚回过的提示不复读");
        assert_eq!(code.wrong, 1);
        assert!(matches!(judge(&mut code, "12", false, 2, 1060), Step::OutOfChances), "答错到次数用完");
        let mut fresh = sample("code", "0421");
        assert!(matches!(judge(&mut fresh, "验证码 ０４２１", false, 2, 1000), Step::Passed));
    }

    #[test]
    fn empty_prompt_does_not_leave_a_separator() {
        let mut message = Message::new().text("欢迎新人");
        append(&mut message, Message::new());
        assert_eq!(message.0.len(), 1, "空说明不增加换行段");
        append(&mut message, Message::new().text("请完成验证"));
        assert_eq!(message.0.len(), 3);
        assert_eq!(message.0[1].data.get("text").and_then(|text| text.as_str()), Some("\n"));
    }

    #[test]
    fn remaining_never_overstates_long_waits() {
        assert_eq!(remaining_round(292), 240);
        assert_eq!(remaining_round(57), 60);
        assert_eq!(remaining_round(-3), 0);
    }

    #[test]
    fn defaults_pass_check() {
        assert!(Config::default().check().is_ok());
    }
}
