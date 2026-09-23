//! 群聊搭话：让内置 agent 以固定人格作为群成员之一存在，绝大多数时候沉默。
//!
//! 独立插件，注册在 `oai` 之后，复用它的模型接入层（接口、密钥、供应商、联网、
//! 绘图与 agent 执行层）；判定与发言模型、接口和密钥都从 `[oai]` 那份配置取，
//! 本插件只持有自己的 `[ambient]` 配置与数据目录（`data/ambient/`）。
//!
//! 三段式，每一段都可以单独调参、单独复盘：
//!
//! 1. **听**——进入本插件而没被前面插件消费的群消息都落进内存里的滚动窗口
//!    （[`window`]），窗口就是模型能看到的全部上下文。
//! 2. **判**——群里安静下来之后，用一个便宜的多模态模型读窗口，只回一个
//!    开口意愿分（[`gate`]）。分数不过线就什么都不发生，这是常态。
//! 3. **说**——过线才唤起内置 agent（[`speak`]），带人设、带工具、带描述 Satori
//!    消息元素的 skill；通过带回执的平台工具执行动作（[`bridge`]），保留旧文字输出兼容。
//!
//! 判定与措辞分开，是因为它们的成本和失败方式都不一样：判定要便宜、要多、
//! 要能看图；措辞要慢、要少、要有工具。合成一次调用就只能两头将就。
//!
//! 另有一条**搭话指令**（[`AmbientConfig::summon_command`]，默认 `/搭话`）：群里
//! 发它就直接跳到第三步，判定那一步不再发生。指令本身是命令，被剥掉之后不进
//! 窗口——人格看到的仍然只是群友聊了什么，而不是有人在按键。

use crate::adapters::satori::{LockedWriter, freshness_for, send_fresh_msg_id};
use crate::config::build_config;
use crate::event::Context;
use crate::message::Message;
use crate::plugins::{PluginError, get_data_dir};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsScalar};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use toml::Value;

mod gate;
#[cfg(test)]
#[path = "ambient/tests.rs"]
mod integration_tests;
mod mood;
mod peak;
pub(crate) mod speak;
mod voice;

// 群聊能力层（看现场、查资料、动手）归内置智能体插件，搭话是它的一个人格外壳。
use crate::plugins::oai::chat::{
    ChatConfig, Persona,
    attention,
    identity,
    memory,
    now_context,
    pace,
    plain_text,
    stickers,
    tone,
    vision,
    window::{self, Turn},
};
use speak::Called;

const LOG_TARGET: &str = "Plugin/Ambient";

/// 本插件的数据目录，`init` 成功后写入。
///
/// 人设、skill、记忆、状态与素材都放在这里（`data/ambient/`）。模型接口那一份
/// 配置不在这里——它由 `oai` 插件持有，搭话只借用（见 [`gate_endpoint`]）。
static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// 内置人设。首次启动写进数据目录，之后以磁盘上那份为准——人设是要被反复
/// 打磨的东西，改一句话不该等一次编译。
pub(crate) const PERSONA: &str = include_str!("../../res/ambient/persona.md");
/// 本体档案的模板：首次启动写进数据目录，之后以磁盘上那份为准。
///
/// 仓库是公开的，所以这里只有格式说明；「这个号后面那个人」的事实（手边有什么
/// 设备、平时在哪、作息、表过态的看法）由管理员写在数据目录那一份里，
/// 每一轮发言带着它，为的是前后说法不打架。见 [`self_facts`]。
const SELF: &str = include_str!("../../res/ambient/self.md");
/// 随代码走的 skill：每次启动按目录名覆盖写入。
///
/// 分成两份是照那套外部 CLI 的渐进披露来的——常在提示词里的只有 skill 的一行描述，
/// 正文要模型自己去 `read`。所以「怎么在群里动手」和「怎么翻旧账」拆开各自成篇，
/// 用得上哪篇才读哪篇，常驻开销仍然只是两行描述。
const SKILLS: [(&str, &str); 1] = [(
    "satori-reply",
    include_str!("../../res/ambient/skills/satori-reply/SKILL.md"),
)];
/// 判定用的「兴趣画像」。
///
/// 判定的唯一任务是在每条消息到来时判断「这个人格会不会想接这句话」。
/// 它只需要知道人格对什么感兴趣、规避什么、怎么接话，而完整写作人设
/// （语感、句式、节奏示例）是给发言模型用的。9KB 人设在每次判定输入里
/// 几乎是常量，却占了判定输入的一大半 token——换成这份几百字的画像，
/// 能让每轮判定便宜一大截，且不影响它判断该不该开口。
const GATE_PERSONA: &str = "\
你是 QQ 群里一个常年蹲着的熟面孔，说话跟群里人一个调子。这群人聊的是 AI 模型与客户端、
手机刷机与各家系统、数码外设、游戏、上班那点事、吃的喝的，还有网上刚出的新闻和八卦。
你爱接梗、爱抬杠、爱跟着起哄：看见离谱的话顺着问两句，看见有人卡在一个技术问题上愿意
搭把手，一群人吹牛或者互相拆台的时候你最想插一句。有人 @ 你、引用你刚说的那句、或者
戳你一下，你都乐意搭一句。群友甩出一张好笑的表情包、几个人斗起图来，你也手痒想回
一张。聊到你熟的领域会忍不住多说两句、安利一下。夸人实在，噎人也实在，那点锋芒朝着
事情去，不朝着人。捧不动也激不动，
能说服你的只有证据，被说中会认，还会乐一下。别人认真求助时你会认真查证并给可核实的
来源，有一说一。
兴趣淡下去的地方：复读刷屏、事情已经解决、别人明确不想继续、一个人连着刷图、
以及表白依恋色情这类情感纠缠——那些你会本能地岔开或者干脆看着。群里同时聊着好几摊事
的时候，你多半只挑一摊接一句，同一件事说过了就过。沉默对你是常态，
不是憋着。判断「你会不会想接这句话」即可，措辞风格不用你操心。";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct AmbientConfig {
    /// 总开关。
    pub enabled: bool,
    /// 开启搭话的群号；空列表等于不开启。
    pub groups: Vec<i64>,
    /// 允许人格执行群管理的群；还须具备 QQ 对应权限。
    pub management_groups: Vec<i64>,
    /// 判定模型：便宜、快、能看图。写 `供应商/模型` 时按 `[oai.providers]` 取接口，
    /// 默认走 DeepSeek 官方的 V4.1 Flash（API 模型名为 deepseek-flash）。
    pub gate_model: String,
    /// 判定用的浓缩人设画像（见 [`GATE_PERSONA`]）。判定只需知道对什么感兴趣、
    /// 避开什么、怎么接话，不需要完整写作人设；留空则回退用完整人设（更贵）。
    pub gate_persona: String,
    /// 发言模型，写成 `供应商/模型`；默认 DeepSeek 的 `deepseek-flash`。
    /// 试过更贵的 Claude / Gemini，实测在真实群聊里并不比便宜档更像人，人机感
    /// 另有来源（该长该短没控住），所以默认仍留在便宜这一档。
    pub reply_model: String,
    /// 发言模型的思考强度（off/minimal/low/medium/high）。
    pub thinking: String,
    /// 发言模型的采样温度。`None`（不写这一项）交给接口自己的默认值。
    ///
    /// 这里调的是「像不像人」那一档：同一句话有无穷多种说法，温度低了每轮都挑最
    /// 稳妥的那种，几轮下来就露出一张嘴一个调子的机器样。1.3 是从 DeepSeek 官方
    /// 那份通用对话档借来的，比接口默认的 1.0 松一档，换供应商也照用。判定模型
    /// 不跟着动——它要的是分数稳。
    pub temperature: Option<f64>,
    /// 发言时开放的工具白名单，逗号分隔。
    ///
    /// `read`/`write`/`bash` 让它能在本轮工作目录里整理材料再当文件发出去；
    /// 聊天界面的 `satori_*` 由代码按开关自动加上，不写在这里。
    /// 名字取自 [`crate::plugins::oai::agent::tools`] 实际注册的工具，没注册的会被静默忽略。
    pub tools: String,
    /// 开口意愿分的门槛，0-100。调高更沉默。
    pub score_threshold: u8,
    /// 每沉默 10 分钟，门槛下调的分数：越久没说话越容易被日常话题勾起来。
    pub silence_relief_per_10min: u8,
    /// 沉默补偿的上限，防止久不发言之后见什么接什么。
    pub silence_relief_cap: u8,
    /// 最近十分钟里每说过一轮，门槛上调的分数：刚接了几句的人本来就该消停一会儿。
    pub speech_penalty_per_turn: u8,
    /// 上面那笔加价的上限，免得说过几轮之后彻底哑掉。
    pub speech_penalty_cap: u8,
    /// 正在关注的话题被接住时，门槛下调的分数。取代从前的「直接放行」。
    ///
    /// 这一笔给得小，是有意的：群里长期只聊一个话题时（数码群整天聊手机），
    /// 「还在聊那个话题」会一直为真，折扣一大就等于常年半价放行。续聊该是
    /// 「这个人值得再多说一句」，不是「这个话题我买过票了」。
    pub focus_relief: u8,
    /// 送进模型的最近消息条数。
    pub context_turns: usize,
    /// 随上下文送进模型的最新图片张数；置 0 关闭图片判读。
    pub context_images: usize,
    /// 群里安静多少秒之后才判定，用来把一串刷屏并成一次。
    pub debounce_seconds: u64,
    /// 从第一条消息算起最多等多久就必须判定一次。
    pub max_pending_seconds: u64,
    /// 普通话题两次主动判定的最短间隔；被叫到、发图与关注中的对话不受限。
    pub gate_interval_seconds: u64,
    /// 两次主动开口之间的时间下限；0 关闭。
    ///
    /// 从前这里是一道墙——冷却没走完就一句话都不说，被点名才绕得过。现在它是一笔
    /// 账：窗口之内，门槛按 `cooldown_penalty` 加价，随时间线性退到窗口结束的 0。
    /// 「刚说完又想接」于是贵得几乎开不了口，而群里真有人顺着它的话问下去时，
    /// 分数够高仍然过得了。把「拦下」换成「抬价」，是为了不把偶发的高分时刻
    /// （有人真的在等它回）也一并挡在外面。
    pub cooldown_seconds: u64,
    /// 冷却窗口内门槛上调的分，从满额线性退到 0；0 等于不设这笔。
    pub cooldown_penalty: u8,
    /// 人格一次最多关注多少秒；0 关闭，最多 600 秒，可随互动续期。
    ///
    /// 关注意味着「续聊」判定为真、门槛打折；给得太久，一个万年不变的话题
    /// 能把它一直挂在那儿。
    pub focus_max_seconds: u64,
    /// 每群每小时的目标发言轮数；0 关闭。
    ///
    /// 这不是配额，是一条会抬价的线：超过它之后每再说一轮，门槛再加
    /// `budget_penalty` 分，加到分数够不着为止——一路抬上去而不是一刀砍断，
    /// 所以聊到兴头上仍然接得住一句特别值得接的。被 @ 与搭话指令本来就不走
    /// 这道门槛，所以真正有人叫它时不会被「这个小时聊够了」挡在外面。
    pub max_per_hour: usize,
    /// 超过每小时目标之后，每多一轮再加的门槛分。
    pub budget_penalty: u8,
    /// 被 @ 或被引用时跳过判定直接开口。
    pub reply_on_mention: bool,
    /// 群友还会怎么叫它：名片之外的小名、简称。
    ///
    /// 名片是「A宝好腻害！」，群里喊的却是「A宝」——平台不会告诉你这件事，只能写在
    /// 这里。认出来只是在记录上加一个「叫了你的名字」的记号（见 [`identity`]），
    /// 不像 @ 那样直接把人格叫醒：猜错一次的代价是它冲着一句不相干的话接了嘴。
    pub aliases: Vec<String>,
    /// 搭话指令：群里一条带它的消息跳过判定，直接把最近这段群聊交给人格。
    ///
    /// 指令本身不进窗口（`/搭话` 是命令，剥掉之后那一条消息才是群聊内容），
    /// 所以人格只看到群友聊了什么，不会看到有人在按键。留空关闭。
    ///
    /// 这是一串普通文本，**要带当前指令前缀写**（默认 `/`）。改了
    /// `command_prefix` 就得同时改这一项，否则群里照新前缀打出来的那句话
    /// 匹配不上。
    pub summon_command: String,
    /// 记住群里的人和旧事（落盘，跨重启）。关掉就只剩眼前这几十条消息。
    pub memory_enabled: bool,
    /// 按作息与互动起伏的内部状态：影响开口门槛、打字快慢和提示词里的一句状态。
    pub mood_enabled: bool,
    /// 每轮最多写几条记忆；0 关闭 `satori_memo`。
    pub memo_budget: usize,
    /// 发言时是否联网：遇到不认识的梗、新版本、比赛战况这类训练知识够不着的事，
    /// 可以先搜一下再开口。**默认开启**——人格的价值有一半在于不瞎说。
    /// 后端与房间共用 `[oai.search]` 那份配置，这里只管这个开关和预算。
    pub search_enabled: bool,
    /// 每轮最多联网几次（搜索与抓取合并）。0 等于关掉出网工具。
    pub search_budget: usize,
    /// 计价高峰时段的作息（见 [`peak`]）。DeepSeek 官方接口空闲时段半价，
    /// 而搭话是这里唯一无人触发的付费功能，最值得挑时段。**只对它家的模型生效**：
    /// 判定与发言两个模型都不走 DeepSeek 时，全天一个价，这一整段让路。
    /// `peak.model` 可以给高峰时段单独配一个便宜模型顶替主模型。
    pub peak: peak::PeakConfig,
    /// 消息时效窗口（秒）：请求交给 satori-qq 之后，群里只要又有人说话就不再发
    /// 出这一句。0 关闭。见 [`crate::adapters::satori::Freshness`]。
    pub send_freshness_seconds: u64,
    /// 发送前短暂显示 QQ 原生“正在输入”（内核实验性接口，默认关闭）。
    pub qq_typing: bool,
    /// 回话前在 QQ 原生内核中标记本群已读（实验性；默认关闭）。
    pub qq_mark_read: bool,
    /// 一次发言最多拆成几条消息。
    pub messages_budget: usize,
    /// 一条消息大约多少字就该换气：超过大约一条半的长度时，把一段话在最自然的
    /// 断句处拆成几条依次发出（总数仍受 `messages_budget` 约束）；0 关闭自动分段。
    ///
    /// 模型写出来的是一整段，群友写出来的是三条——差别只在换气。见 [`breath`]。
    /// 默认给得宽：群里的长句多半是「语音输入一条说完」，只有真成了一坨才该拆。
    pub split_chars: usize,
    /// 每轮平台写动作总数（含消息、点赞、撤回）。
    pub actions_budget: usize,
    /// 每轮最多生成图片的张数；0 关闭绘图。绘图走 `[oai]` 配置的图像模型。
    pub draw_budget: usize,
    /// 每轮最多写几首歌；0 关闭写歌。写歌走 `[oai]` 配置的 Suno 接口，一次生成
    /// 两个版本，按站点计费约 $0.5——比绘图贵两个数量级，所以默认只给 1 次。
    pub music_budget: usize,
    /// 每轮最多拍几段视频；0 关闭拍片。拍片走 `[oai]` 配置的视频接口，一次约 $1.2，
    /// 是这里最贵的一项；关掉它就是在群里收回这个能力。
    pub video_budget: usize,
    /// 偷来的表情包最多留几张；0 表示不攒（窗口里照样偷，只是转身就没了）。
    ///
    /// 群友发的图与商城表情在偷的那一刻会被抄进 `data/ambient/stickers/`，往后每轮
    /// 挑几张贴进发言提示词，人格想发就发。库满了先丢最没人用的那几张。
    pub sticker_max: usize,
    /// 打字速度（字/分钟）。调低更像在慢慢敲。
    pub typing_cpm: u32,
    /// 长句改用语音输入时的等效速度（字/分钟）。
    pub voice_cpm: u32,
    /// 看完消息到开始打字之间的思考时间（秒）；模型已经花掉的时间计入其中。
    pub think_seconds: f32,
    /// 判定的时间上限。
    pub gate_timeout_seconds: u64,
    /// 一次发言（含工具调用）的时间上限。
    pub reply_timeout_seconds: u64,
}

impl Default for AmbientConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            groups: Vec::new(),
            management_groups: Vec::new(),
            gate_model: "deepseek/deepseek-flash".to_string(),
            gate_persona: GATE_PERSONA.to_string(),
            reply_model: "deepseek/deepseek-flash".to_string(),
            thinking: "low".to_string(),
            temperature: Some(1.3),
            tools: "read,write,bash".to_string(),
            score_threshold: 60,
            silence_relief_per_10min: 0,
            silence_relief_cap: 0,
            speech_penalty_per_turn: 12,
            speech_penalty_cap: 36,
            focus_relief: 5,
            context_turns: 20,
            context_images: 2,
            debounce_seconds: 3,
            max_pending_seconds: 12,
            gate_interval_seconds: 30,
            cooldown_seconds: 90,
            cooldown_penalty: 25,
            focus_max_seconds: 180,
            max_per_hour: 8,
            budget_penalty: 12,
            reply_on_mention: true,
            aliases: Vec::new(),
            summon_command: "/搭话".to_string(),
            memory_enabled: true,
            mood_enabled: true,
            memo_budget: 3,
            search_enabled: true,
            search_budget: 3,
            peak: peak::PeakConfig::default(),
            send_freshness_seconds: 25,
            qq_typing: false,
            qq_mark_read: false,
            messages_budget: 3,
            split_chars: 60,
            actions_budget: 6,
            draw_budget: 2,
            music_budget: 1,
            video_budget: 1,
            sticker_max: 120,
            typing_cpm: 150,
            voice_cpm: 420,
            think_seconds: 3.0,
            gate_timeout_seconds: 45,
            reply_timeout_seconds: 240,
        }
    }
}

/// 这一轮压在门槛上的分量。
///
/// 三件事各自算一笔账：刚开过口的余温、最近十分钟的密度、这个小时已经说超的
/// 部分。它们都只抬价、不拦人——分数是模型看着群聊给的，真有人冲它来的时候
/// 分数自然会高。
#[derive(Debug, Clone, Copy, Default)]
struct Pressure {
    /// 距上次自己开口多久；从没说过是 None。
    since_last_spoke: Option<Duration>,
    /// 最近十分钟说过几轮。
    recent_turns: usize,
    /// 这一小时说过几轮。
    hourly_turns: usize,
}

impl AmbientConfig {
    fn debounce(&self) -> Duration {
        Duration::from_secs(self.debounce_seconds.clamp(1, 120))
    }

    fn max_wait(&self) -> Duration {
        Duration::from_secs(self.max_pending_seconds.clamp(self.debounce().as_secs(), 600))
    }

    fn gate_interval(&self) -> Duration {
        Duration::from_secs(self.gate_interval_seconds.min(3_600))
    }

    fn cooldown(&self) -> Duration {
        Duration::from_secs(self.cooldown_seconds)
    }

    pub(crate) fn gate_timeout(&self) -> Duration {
        Duration::from_secs(self.gate_timeout_seconds.clamp(5, 300))
    }

    pub(crate) fn reply_timeout(&self) -> Duration {
        Duration::from_secs(self.reply_timeout_seconds.clamp(30, 1_800))
    }

    /// 这一句还值得说多久。0 表示不带时效条件，与从前一样无条件发送。
    pub(crate) fn freshness_window(&self) -> Duration {
        Duration::from_secs(if self.send_freshness_seconds == 0 {
            0
        } else {
            self.send_freshness_seconds.clamp(3, 300)
        })
    }

    /// 可选的旧版沉默补偿；默认关闭，不为刷存在感降低人格的兴趣门槛。
    fn effective_threshold(&self, silent_for: Option<Duration>) -> u8 {
        if self.silence_relief_per_10min == 0 {
            return self.score_threshold;
        }
        let minutes = silent_for.map_or(f64::INFINITY, |elapsed| elapsed.as_secs_f64() / 60.0);
        let relief = (minutes / 10.0 * f64::from(self.silence_relief_per_10min))
            .min(f64::from(self.silence_relief_cap));
        self.score_threshold.saturating_sub(relief as u8)
    }

    /// 这一轮实际要跨过的门槛。
    ///
    /// 五笔加减：可选的沉默补偿、当下状态的微调、最近十分钟的密度、刚开过口的
    /// 余温，以及这个小时说超的部分。后三笔都是**抬价而不是拦人**：群里真有人
    /// 顺着它的话接下去时，分数够高就照样过得去；只有一直没人理它（分数上不去）
    /// 才会被一路抬高的门槛挡在外面——那正是「更安静一点」该有的样子。
    fn threshold(&self, state: mood::Snapshot, pressure: Pressure) -> u8 {
        let base = i16::from(self.effective_threshold(pressure.since_last_spoke));
        let shift = if self.mood_enabled {
            state.threshold_shift()
        } else {
            0
        };
        let crowding = (pressure.recent_turns as i16)
            .saturating_mul(i16::from(self.speech_penalty_per_turn))
            .min(i16::from(self.speech_penalty_cap));
        let total =
            base + shift + crowding + self.cooldown_charge(pressure) + self.budget_charge(pressure);
        total.clamp(1, 100) as u8
    }

    /// 刚开过口的余温：冷却窗口里按剩下的时间按比例抬价，走到窗口末尾就回到 0。
    /// 冷却写 0、或从没开过口时，都没有这笔账。
    fn cooldown_charge(&self, pressure: Pressure) -> i16 {
        let cooldown = self.cooldown();
        let Some(elapsed) = pressure.since_last_spoke else {
            return 0;
        };
        if cooldown.is_zero() || self.cooldown_penalty == 0 {
            return 0;
        }
        let left = cooldown.saturating_sub(elapsed).as_secs();
        let charge = u64::from(self.cooldown_penalty) * left / cooldown.as_secs().max(1);
        charge as i16
    }

    /// 这个小时说超的账：每多一轮再加一份 `budget_penalty`。抬到分数够不着为止，
    /// 但抬上去的是一条线而不是一堵墙——小时一过账就清了（计数只留最近一小时）。
    fn budget_charge(&self, pressure: Pressure) -> i16 {
        if self.max_per_hour == 0 || self.budget_penalty == 0 {
            return 0;
        }
        let over = pressure.hourly_turns.saturating_sub(self.max_per_hour);
        (over as i16).saturating_mul(i16::from(self.budget_penalty))
    }

    /// 这一轮里有没有走 DeepSeek 峰谷价的调用。判定与发言是两次不同的调用，
    /// 有一个还在 DeepSeek 上，高峰时段就仍有一半的钱可省。
    fn peak_applies(&self) -> bool {
        [self.gate_model.as_str(), self.reply_model.as_str()]
            .into_iter()
            .any(peak::billed_by_peak)
    }

    /// 现在该以什么姿态待着。峰谷价只跟 DeepSeek 有关，别家全天一个价，
    /// 时段管理整段让路——`[ambient.peak]` 怎么配都不影响，换回 DeepSeek 立刻生效。
    fn peak_stance(&self) -> peak::Stance {
        self.peak_stance_at(chrono::Local::now())
    }

    fn peak_stance_at<Tz: chrono::TimeZone>(&self, at: chrono::DateTime<Tz>) -> peak::Stance {
        if !self.peak_applies() {
            return peak::Stance::Awake;
        }
        match self.peak.stance_at(at) {
            // 配了替补才有「换模型」这回事；`model` 留空时这一档与照常无异。
            peak::Stance::Swapped if self.peak.model.trim().is_empty() => peak::Stance::Awake,
            stance => stance,
        }
    }

    /// 发言节奏。精神头好就敲得快、想得短，困了反过来。
    fn pace(&self, state: mood::Snapshot) -> pace::Pace {
        let (typing, think) = if self.mood_enabled {
            (state.typing_scale(), state.think_scale())
        } else {
            (1.0, 1.0)
        };
        pace::Pace {
            typing_cpm: ((self.typing_cpm as f32) * typing).round().max(20.0) as u32,
            voice_cpm: ((self.voice_cpm as f32) * typing).round().max(20.0) as u32,
            think_seconds: self.think_seconds * think,
        }
    }

    /// 高峰时段照常跑、只把模型换成替补的一份配置。
    ///
    /// `mode = "swap"` 用这一份：节奏、联网、看图、绘图全跟平时一样，只有判定与
    /// 发言两个模型换成 `[ambient.peak].model`。峰谷价是 DeepSeek 一家的事，换一家
    /// 就没有高峰期，成本既已压下来，就不必再靠睡或不说话来省。
    fn swapped(&self) -> Self {
        Self {
            gate_model: self.peak.model_or(&self.gate_model).to_string(),
            reply_model: self.peak.model_or(&self.reply_model).to_string(),
            ..self.clone()
        }
    }

    /// 高峰时段被点名唤醒时用的一份「省着来」的配置（`mode = "sleep"`）。
    ///
    /// 输入里最贵的是图片，其次是上下文长度；输出里最贵的是多发几条和顺手画张图。
    /// 醒过来回一句仍然算数，只是这一句用最少的钱说完。
    ///
    /// 模型也跟着换：主模型在 DeepSeek 高峰翻倍，替补全天一个价。这一档连上下文
    /// 一起省，所以比 [`Self::swapped`] 还紧一档。
    fn frugal(&self) -> Self {
        Self {
            context_images: 0,
            context_turns: (self.context_turns / 2).max(6),
            messages_budget: self.messages_budget.min(2),
            draw_budget: 0,
            music_budget: 0,
            video_budget: 0,
            // 高峰时段半价的是模型调用；联网搜索不便宜也更慢，这一句先不查。
            search_enabled: false,
            ..self.swapped()
        }
    }

}

/// 看头像用的接口：判定模型那一份。配不出来就不看。
async fn avatar_endpoint(
    ctx: &Context,
    mgr: &Arc<crate::plugins::oai::data::Manager>,
    config: &AmbientConfig,
) -> Option<crate::plugins::oai::chat::Avatar> {
    let (api_base, api_key, model) = gate_endpoint(ctx, mgr, &config.gate_model).await.ok()?;
    Some(crate::plugins::oai::chat::Avatar {
        api_base,
        api_key,
        model,
    })
}

/// 能力层眼里的人格：它需要问一句的地方都在这里。
///
/// 能力层（`oai::chat`）不碰人格状态——它只在这一处回问：这一轮该带什么口吻与状态、
/// 打字多快、说出去一句之后要不要记账。
pub(crate) struct Ambient {
    config: AmbientConfig,
    /// 看头像用的接口；取不到就不看，提示词里少一行而已。
    avatar: Option<crate::plugins::oai::chat::Avatar>,
}

impl Ambient {
    pub(crate) fn new(
        config: &AmbientConfig,
        avatar: Option<crate::plugins::oai::chat::Avatar>,
    ) -> Self {
        Self {
            config: config.clone(),
            avatar,
        }
    }

}

/// `[ambient]` 那份配置 → 能力层这一轮的额度与开关。
///
/// 一条一条写出来是为了看得见差异：能力层不读任何插件配置，两边怎么对上全靠这里，
/// 以后给能力层加一项，编译器会在这里提醒补上。
pub(crate) fn chat_config(config: &AmbientConfig) -> ChatConfig {
    ChatConfig {
        management_groups: config.management_groups.clone(),
        messages_budget: config.messages_budget,
        actions_budget: config.actions_budget,
        memo_budget: config.memo_budget,
        draw_budget: config.draw_budget,
        music_budget: config.music_budget,
        video_budget: config.video_budget,
        memory_enabled: config.memory_enabled,
        sticker_max: config.sticker_max,
        context_turns: config.context_turns,
        split_chars: config.split_chars,
        freshness_seconds: config.freshness_window().as_secs(),
        qq_typing: config.qq_typing,
        qq_mark_read: config.qq_mark_read,
        media_deadline_seconds: config.reply_timeout().as_secs(),
        tools: config.tools.clone(),
    }
}

impl Persona for Ambient {
    fn scene(&self, group: i64, turns: &[Turn], _rhythm: &str) -> serde_json::Value {
        let snapshot = self.config.mood_enabled.then(|| mood::snapshot(group));
        serde_json::json!({
            "register": tone::register(turns),
            "state": snapshot.map(mood::Snapshot::describe).unwrap_or_default(),
            "remember": if self.config.memory_enabled {
                memory::with_group(group, |memory| {
                    memory.brief(turns, chrono::Local::now().timestamp())
                })
            } else {
                String::new()
            },
        })
    }

    fn spoke(&self, group: i64) {
        if self.config.mood_enabled {
            mood::nudge(|mood, now| mood.spoke(group, now));
        }
    }

    fn pace(&self, group: i64) -> pace::Pace {
        self.config.pace(mood::snapshot(group))
    }

    fn avatar(&self) -> Option<crate::plugins::oai::chat::Avatar> {
        self.avatar.clone()
    }
}

/// 一轮判定与发言共用的「现场」。
///
/// 全部由本地数据算出，不额外调用模型：群里此刻的语感、自己的精神头、参与节奏、
/// 以及记得的人和旧事。小模型对这种具体锚点的反应，比再加十条抽象规则好得多。
pub(crate) struct Scene {
    /// 「我在这个群里是谁」：群里看到的那个名字、头衔、进群多久、群名与头像。
    ///
    /// 它跟着 [`Scene::brief`] 一起递给判定侧——判定要认出「有人在叫我」，
    /// 而群里叫人用的是名片上的字，不是 QQ 号。
    pub identity: String,
    /// 自己的发言节奏与当前关注。
    pub rhythm: String,
    /// 本群此刻的说话方式。
    pub register: String,
    /// 精神头与兴致；关闭状态时为空。
    pub state: String,
    /// 记得的人与旧事；关闭记忆时为空。
    pub memory: String,
    /// 这个号后面那个人自己的事，以及他平时说话的原话样本。
    ///
    /// 这两样只影响「说出来的像不像他」，对「要不要接这句话」没用，所以不跟着
    /// [`Scene::brief`] 一起递给判定侧——那是每条消息都要付一次的账。
    pub own: String,
}

impl Scene {
    pub(crate) fn build(group: i64, config: &AmbientConfig, turns: &[Turn], rhythm: String) -> Self {
        // 状态算一次用两处：一句给模型看的「你现在的状态」，以及挑样本的调子。
        let snapshot = config.mood_enabled.then(|| mood::snapshot(group));
        Self {
            identity: identity::brief(group),
            rhythm,
            register: tone::register(turns),
            state: snapshot.map(mood::Snapshot::describe).unwrap_or_default(),
            memory: if config.memory_enabled {
                memory::with_group(group, |memory| {
                    memory.brief(turns, chrono::Local::now().timestamp())
                })
            } else {
                String::new()
            },
            own: format!(
                "{}{}{}",
                self_facts(),
                voice::brief(turns, voice_register(config, group)),
                stickers::brief(turns, config.sticker_max)
            ),
        }
    }

    /// 现场 → 注入提示词的一段话。
    pub(crate) fn brief(&self) -> String {
        let mut out = format!("{}\n{}{}\n", now_context(), self.identity, self.register);
        if !self.state.is_empty() {
            out.push_str(&self.state);
            out.push('\n');
        }
        out.push_str("当前参与状态：");
        out.push_str(&self.rhythm);
        out.push('\n');
        out.push_str(&self.memory);
        out
    }
}

/// 这一刻说话的调子：精神头定松紧，挑样本与写日志用的都是它。
///
/// 关掉状态（`mood_enabled = false`）时按不上不下处理，两档样本都能挑。
fn voice_register(config: &AmbientConfig, group: i64) -> mood::Register {
    config
        .mood_enabled
        .then(|| mood::snapshot(group).register())
        .unwrap_or(mood::Register::Even)
}

/// 本体档案 → 注入发言提示词的一段话。
///
/// 文件里 `#` 开头的行是注释，其余每行一条事实。缺失、只剩注释或者读不出来时返回
/// 空串：没有档案，也好过凭空编一份档案。
fn self_facts() -> String {
    let Some(dir) = DATA_DIR.get() else {
        return String::new();
    };
    match std::fs::read_to_string(self_path(dir)) {
        Ok(raw) => facts_from(&raw),
        Err(_) => String::new(),
    }
}

/// 档案正文 → 提示词里那一段。单独拎出来，好在测试里钉住注释与空行的处理。
fn facts_from(raw: &str) -> String {
    let facts: Vec<String> = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| format!("- {line}"))
        .collect();
    if facts.is_empty() {
        return String::new();
    }
    format!(
        "关于你自己的一些事（别人问起、自己聊到时照这个来）：\n{}\n",
        facts.join("\n")
    )
}

/// 人设文件位置。插件数据目录本身就是搭话的根，人设、skill、记忆、素材都在它下面。
fn persona_path(base: &Path) -> PathBuf {
    base.join("persona.md")
}

/// 本体档案的位置。
fn self_path(base: &Path) -> PathBuf {
    base.join("self.md")
}

fn skills_root(base: &Path) -> PathBuf {
    base.join("skills")
}

/// 这一轮随身的 skill 目录清单。
pub(crate) fn skill_dirs(base: &Path) -> Vec<PathBuf> {
    let root = skills_root(base);
    SKILLS
        .iter()
        .map(|(name, _)| root.join(name))
        .collect()
}

/// 铺开人设与 skill。
///
/// 人设只在缺失时写入——它是给人改的，覆盖等于把管理员的打磨扔掉；
/// skill 描述的是本仓库实现的消息元素，属于代码的一部分，每次启动都对齐。
pub(crate) async fn setup(base: &Path) -> std::io::Result<()> {
    let persona = persona_path(base);
    if let Some(parent) = persona.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if !persona.exists() {
        tokio::fs::write(&persona, PERSONA).await?;
    }
    // 档案跟人设一样只在缺失时写：它写的是这个人自己的事，覆盖等于把攒下来的
    // 那点前后一致性抹掉。模板里只有格式说明，实际内容由管理员填。
    let facts = self_path(base);
    if !facts.exists() {
        tokio::fs::write(&facts, SELF).await?;
    }
    for (name, body) in SKILLS {
        let dir = skills_root(base).join(name);
        tokio::fs::create_dir_all(&dir).await?;
        tokio::fs::write(dir.join("SKILL.md"), body).await?;
    }
    tokio::fs::create_dir_all(base.join("media")).await?;
    // 记忆、表情包库与群身份归能力层（`data/oai/chat/`），这里只管人格自己的那份
    // 状态曲线。能力层的数据目录由 oai 插件挂上；它没启用时这里补一次。
    mood::attach(base);
    Ok(())
}

/// 记下一条群消息，必要时安排一次判定。
///
/// 由本插件的 [`handle`] 对每条没被前面插件消费的群消息调用：能走到这里的就是普通聊天。
pub(crate) async fn observe(
    ctx: &Context,
    writer: &LockedWriter,
    mgr: &Arc<crate::plugins::oai::data::Manager>,
    base: &Path,
) {
    let config = crate::plugins::get_config_or_default::<AmbientConfig>(ctx, "ambient");
    if !config.enabled || config.groups.is_empty() {
        return;
    }
    let Some(event) = ctx.as_message() else {
        observe_notice(ctx, writer, mgr, base, &config).await;
        return;
    };
    let Some(group) = event.group_id().filter(|id| config.groups.contains(id)) else {
        return;
    };

    let me = ctx
        .bot
        .login_user
        .get()
        .id
        .parse::<i64>()
        .unwrap_or_default();
    let mut turn = window::turn_from(&event, me);
    // 引用在群里的样子是「原话摆在那儿」，模型也该看见被引的是哪一句、谁说的；
    // 引到自己那条的时候就等于点了名，与 @ 同等地把它叫醒。
    window::with_group(group, |state| {
        window::resolve_quote(&mut turn, |id| state.quote_of(id))
    });
    // 搭话指令是被剥掉的那两个词，不是群聊内容：它不进窗口，只让这一批跳过判定。
    let summoned = strip_summon(&mut turn.text, &config.summon_command);
    // 其余指令是说给机器人听的，不是群聊内容；记下来只会让人格模型学着复述指令。
    let is_command = crate::command::get_prefixes(ctx)
        .iter()
        .any(|prefix| !prefix.is_empty() && turn.text.starts_with(prefix.as_str()));
    if is_command {
        return;
    }
    // 群里叫人多数时候是直接打名字，不是 @。协议里没有这件事，只能自己认一遍；
    // 认出来只在记录上留个记号，不当作点名（见 [`identity::called_by_name`]）。
    turn.call.named_me =
        !turn.from_me && identity::called_by_name(group, &config.aliases, &turn.text);
    // 每条群消息都带着群名，而 `guild.get` 未必给得出——记下来当兜底。
    identity::note_group_name(group, event.0.get_str("group_name").unwrap_or_default());
    // 剥掉指令后空无一物的那条消息没有内容可给模型看；它只是按了一次键。
    let empty = turn.text.is_empty() && turn.images.is_empty();
    if empty && !summoned {
        return;
    }
    if config.memory_enabled && !turn.from_me {
        let (id, name, at) = (turn.user_id, turn.name.clone(), turn.at);
        memory::edit(group, |memory| memory.see(id, &name, at));
    }
    if config.mood_enabled && turn.mentions_me && !turn.from_me {
        mood::nudge(|mood, now| mood.engaged(group, now));
    }
    let start = window::with_group(group, |state| {
        let pushed = !empty && state.receive(turn);
        // 闲着的群由指令自己叫起来；有 worker 在跑时它下一轮会看见这个标记。
        pushed || (summoned && state.summon())
    });
    if !start {
        return;
    }

    let ctx = ctx.clone();
    let writer = writer.clone();
    let mgr = mgr.clone();
    let base = base.to_path_buf();
    tokio::spawn(async move {
        if let Err(error) = consider(&ctx, &writer, &mgr, group, &base).await {
            warn!(target: LOG_TARGET, "群 {group} 搭话失败：{error:#}");
        }
    });
}

/// 戳一戳和撤回也是互动；表态没有操作者，不能凭空归到某位群友头上。
async fn observe_notice(
    ctx: &Context,
    writer: &LockedWriter,
    mgr: &Arc<crate::plugins::oai::data::Manager>,
    base: &Path,
    config: &AmbientConfig,
) {
    let crate::event::EventType::Satori(raw) = &ctx.event else {
        return;
    };
    let Some(group) = raw
        .get_i64("group_id")
        .filter(|g| config.groups.contains(g))
    else {
        return;
    };
    let Some((turn, recalled)) = notice_turn(raw, ctx.bot.login_user.get().id.parse().unwrap_or(0))
    else {
        return;
    };
    if config.mood_enabled && turn.mentions_me {
        mood::nudge(|mood, now| mood.engaged(group, now));
    }
    let start = window::with_group(group, |state| {
        if let Some(id) = recalled {
            state.recall(id);
        }
        // 平台变化使已准备的动作过时，但不单独唤醒人格。
        if turn.from_me && turn.user_id == 0 {
            state.seq += 1;
        }
        state.receive(turn)
    });
    if start {
        let (ctx, writer, mgr) = (ctx.clone(), writer.clone(), mgr.clone());
        let base = base.to_path_buf();
        tokio::spawn(async move {
            if let Err(error) = consider(&ctx, &writer, &mgr, group, &base).await {
                warn!(target: LOG_TARGET, "群 {group} 互动处理失败：{error:#}");
            }
        });
    }
}

fn notice_turn(raw: &simd_json::OwnedValue, me: i64) -> Option<(Turn, Option<i64>)> {
    let user = raw.get_i64("user_id").unwrap_or(0);
    let mid = raw.get_i64("message_id").unwrap_or(0);
    let kind = raw.get_str("satori_type").unwrap_or("");
    let mut recalled = None;
    let mut mentions_me = false;
    let mut call = window::Call::default();
    let mut from_me = user == me;
    let text = match kind {
        "internal" if raw.get_str("sub_type") == Some("poke") => {
            let data = raw.get("satori_data")?;
            let target = data
                .get_str("target_id")
                .and_then(|s| s.parse::<i64>().ok())
                .or_else(|| data.get_i64("target_id"))
                .unwrap_or(0);
            mentions_me = target == me && user != me;
            call.poked_me = mentions_me;
            format!("[戳一戳：{user} 戳了 {target}]")
        }
        "message-deleted" => {
            recalled = Some(mid);
            format!("[消息 {mid} 已撤回]")
        }
        "reaction-added" | "reaction-removed" | "reaction-deleted" => {
            let emoji = raw
                .get("_satori")?
                .get("emoji")?
                .get_str("id")
                .unwrap_or("?");
            // 只记录、不靠这类无操作者回声唤醒，避免自己点赞→自己接话的循环。
            from_me = true;
            format!(
                "[平台事件：消息 {mid} {}表态 {emoji}；操作者未知]",
                if kind == "reaction-added" {
                    "新增"
                } else {
                    "减少"
                }
            )
        }
        "guild-member-added" => format!("[群成员 {user} 加入了群聊]"),
        "guild-member-removed" => format!(
            "[群成员 {user} 离开了群聊；操作者 {}]",
            raw.get_i64("operator_id").unwrap_or(0)
        ),
        "guild-member-updated" => {
            // 管理动作的事件只更新现场，避免自己管理→自己评论的循环。
            from_me = true;
            if raw.get_str("notice_type") == Some("group_ban") {
                let target = if user == 0 {
                    "全体成员".into()
                } else {
                    user.to_string()
                };
                let duration = raw.get_i64("duration").unwrap_or(0);
                if duration == 0 {
                    format!("[{target} 已解除禁言]")
                } else {
                    format!("[{target} 被禁言 {duration} 秒]")
                }
            } else {
                let member = raw.get("_satori").and_then(|s| s.get("member"));
                format!(
                    "[群成员 {user} 的资料更新：{}]",
                    member
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "具体变化未知".into())
                )
            }
        }
        "guild-updated" | "channel-updated" => {
            from_me = true;
            let source = raw.get("_satori")?;
            let detail = source.get(if kind == "guild-updated" {
                "guild"
            } else {
                "channel"
            });
            format!(
                "[群资料更新：{}]",
                detail.map(ToString::to_string).unwrap_or_default()
            )
        }
        _ => return None,
    };
    Some((
        Turn {
            user_id: if from_me && user != me { 0 } else { user },
            name: if from_me && user != me {
                "平台事件".into()
            } else {
                user.to_string()
            },
            text,
            images: vec![],
            elements: Message::new(),
            message_id: 0,
            mentions_me,
            call,
            from_me,
            at: raw
                .get_i64("time")
                .unwrap_or_else(|| chrono::Local::now().timestamp()),
        },
        recalled,
    ))
}

/// 把搭话指令从正文里剥掉，返回是不是真剥到了。
///
/// 指令算一个词，出现在句首、`@我` 之后或任意空白之后都认；剥掉之后剩下的
/// 才是群友真正说的话（`/搭话 你怎么看` → `你怎么看`），照常进窗口。它本身
/// 永远不进窗口——人格看到有人在按键，就会去回应那个按键而不是群里的话题。
fn strip_summon(text: &mut String, command: &str) -> bool {
    let command = command.trim();
    if command.is_empty() {
        return false;
    }
    let Some(at) = text.find(command) else {
        return false;
    };
    let head = &text[..at];
    if !head.is_empty() && !head.ends_with(|c: char| c.is_whitespace()) {
        return false;
    }
    let head = head.trim_end();
    let tail = text[at + command.len()..].trim_start();
    *text = match head.is_empty() {
        true => tail.to_string(),
        false if tail.is_empty() => head.to_string(),
        false => format!("{head} {tail}"),
    };
    true
}

/// 取消/异常时释放 worker；正常交接已在锁内完成，不能再清掉新 worker 的标记。
struct Worker {
    group: i64,
    armed: bool,
}

fn immediate_gate(turns: &[Turn], mentioned: bool, summoned: bool, focused: bool) -> bool {
    mentioned
        || summoned
        || focused
        || turns
            .iter()
            .rev()
            .find(|turn| !turn.from_me)
            .is_some_and(|turn| turn.call.named_me || !turn.images.is_empty())
}
impl Drop for Worker {
    fn drop(&mut self) {
        if self.armed {
            window::with_group(self.group, |state| state.running = false);
        }
    }
}

async fn consider(
    ctx: &Context,
    writer: &LockedWriter,
    mgr: &Arc<crate::plugins::oai::data::Manager>,
    group: i64,
    base: &Path,
) -> anyhow::Result<()> {
    let mut worker = Worker { group, armed: true };
    loop {
        let config = crate::plugins::get_config_or_default::<AmbientConfig>(ctx, "ambient");
        if config.focus_max_seconds == 0 {
            window::with_group(group, |state| state.focus = None);
        }
        if !config.enabled || !config.groups.contains(&group) {
            return Ok(());
        }
        let deadline = Instant::now() + config.max_wait();
        loop {
            let before = window::with_group(group, |state| state.seq);
            tokio::time::sleep(
                config
                    .debounce()
                    .min(deadline.saturating_duration_since(Instant::now())),
            )
            .await;
            let after = window::with_group(group, |state| state.seq);
            if before == after || Instant::now() >= deadline {
                break;
            }
        }
        let (mut seq, turns, mentioned, summoned, silent_for, rhythm, focused) =
            window::with_group(group, |state| {
                let mentioned = state.take_mention() && config.reply_on_mention;
                let summoned = state.take_summon();
                (
                    state.seq,
                    state.recent(config.context_turns.clamp(1, 80)),
                    mentioned,
                    summoned,
                    state.last_spoke.map(|last| last.elapsed()),
                    state.rhythm(),
                    state.active_focus().is_some(),
                )
            });
        if let Err(error) = consider_batch(
            ctx, writer, mgr, group, base, &config, &mut seq, &turns, mentioned, summoned, silent_for,
            &rhythm, focused,
        )
        .await
        {
            warn!(target: LOG_TARGET, "群 {group} 搭话失败：{error:#}");
        }
        // 记性和状态每批都落盘：绝大多数批次以沉默收场，只在开口时保存等于几乎不保存。
        memory::flush(group).await;
        mood::flush().await;
        if !window::with_group(group, |state| state.finish_batch(seq)) {
            worker.armed = false;
            return Ok(());
        }
    }
}

/// 一个模型要用的接口、密钥与纯模型 id。
///
/// 模型写成 `供应商/模型` 时按 `[oai.providers]` 取该供应商的接口
/// （DeepSeek 官方即走这里）；不带前缀则沿用 `oai` 默认接口，与从前一致。
/// 判定模型与发言模型共用这一份解析——它们只是两个不同的模型名。
async fn gate_endpoint(
    ctx: &Context,
    mgr: &Arc<crate::plugins::oai::data::Manager>,
    gate_model: &str,
) -> anyhow::Result<(String, String, String)> {
    let (provider, model) = crate::plugins::oai::utils::split_provider(gate_model);
    let providers =
        crate::plugins::get_config_or_default::<crate::plugins::oai::OaiConfig>(ctx, "oai")
            .providers;
    let (base, key) = {
        let config = mgr.config.read().await;
        (config.api_base.clone(), config.api_key.clone())
    };
    let Some((base, key)) =
        crate::plugins::oai::resolve_endpoint(&providers, &base, &key, provider.as_deref())
    else {
        anyhow::bail!(
            "未知供应商：{}（在 [oai.providers] 里配置）",
            provider.as_deref().unwrap_or_default()
        );
    };
    if base.is_empty() || key.is_empty() {
        anyhow::bail!("判定模型需要 API 配置，请先设置 oai 的接口地址与密钥");
    }
    Ok((base, key, model))
}

#[allow(clippy::too_many_arguments)]
async fn consider_batch(
    ctx: &Context,
    writer: &LockedWriter,
    mgr: &Arc<crate::plugins::oai::data::Manager>,
    group: i64,
    base: &Path,
    config: &AmbientConfig,
    seq: &mut u64,
    turns: &[Turn],
    mentioned: bool,
    summoned: bool,
    silent_for: Option<Duration>,
    rhythm: &str,
    focused: bool,
) -> anyhow::Result<()> {
    if turns.is_empty() {
        return Ok(());
    }
    // 计价高峰时段：价格翻倍。最省事的做法是换一家全天同价的便宜模型照常跑
    // （`swap`）；想更保守可以让它睡着（`sleep`：不跟着消息频率一直判定，只隔
    // `doze_gate_seconds` 看一眼，每小时自主开口不超过 `doze_reply_limit` 次，
    // 被点名或搭话指令则立刻醒，并且统一换上最省的一份上下文），或者彻底不出声
    // （`pause`）。这一段只对 DeepSeek 的模型生效。
    let stance = config.peak_stance();
    let adjusted;
    let mut doze = false;
    let config = match stance {
        peak::Stance::Asleep => {
            debug!(target: LOG_TARGET, "群 {group} 处于计价高峰时段，本轮不出声");
            return Ok(());
        }
        // 高峰换了替补：节奏与平时一样，只是这一轮走便宜档。
        peak::Stance::Swapped => {
            debug!(target: LOG_TARGET, "群 {group} 处于计价高峰时段，本轮换替补模型照常跑");
            adjusted = config.swapped();
            &adjusted
        }
        peak::Stance::Dozing if mentioned || summoned => {
            info!(target: LOG_TARGET, "群 {group} 在计价高峰时段被叫醒，省着回一句");
            adjusted = config.frugal();
            &adjusted
        }
        peak::Stance::Dozing => {
            // 两条闸门都只在本地读时间戳，不产生费用：这一小时的自主开口配额，
            // 以及距上次主动判定够不够久。任一条没过就这一批不判定，群里照常攒上下文。
            let peak = &config.peak;
            let due = window::with_group(group, |state| {
                peak.doze_allows_reply(state.doze_spoke_last_hour())
                    && state.allow_doze_gate(peak.doze_gate())
            });
            if !due {
                debug!(target: LOG_TARGET, "群 {group} 处于计价高峰时段，这一批不主动判定");
                return Ok(());
            }
            info!(target: LOG_TARGET, "群 {group} 处于计价高峰时段，偶尔看一眼要不要接话");
            doze = true;
            adjusted = config.frugal();
            &adjusted
        }
        peak::Stance::Awake => config,
    };
    let turns = &turns[turns
        .len()
        .saturating_sub(config.context_turns.clamp(1, 80))..];

    if !immediate_gate(turns, mentioned, summoned, focused)
        && !window::with_group(group, |state| state.allow_passive_gate(config.gate_interval()))
    {
        return Ok(());
    }

    let persona = tokio::fs::read_to_string(persona_path(base))
        .await
        .unwrap_or_else(|_| PERSONA.to_string());

    // 上一次开口是被接住了还是掉在地上，只在这里结算一次。
    if config.mood_enabled
        && let Some(gap) = window::with_group(group, |state| state.take_feedback())
    {
        mood::nudge(|mood, now| match gap {
            ..=90 => mood.engaged(group, now),
            300.. => mood.ignored(group, now),
            _ => {}
        });
    }
    // 「我在这个群里是谁」在判定之前就要在手上：判定要认出「有人在叫我」，而群里
    // 叫人用的是名片上那几个字。资料按小时缓存，所以这一句绝大多数时候不出网。
    identity::refresh(ctx, writer, avatar_endpoint(ctx, mgr, config).await.as_ref(), group).await;
    if !mentioned && !summoned && !current(ctx, group, *seq) {
        return Ok(());
    }
    let scene = Scene::build(group, config, turns, rhythm.to_string());
    if summoned {
        // 指令是人按下的：判定那一步整个不发生，这一批直接进第三步。
        info!(target: LOG_TARGET, "群 {group} 收到搭话指令，这一批交给人格");
    } else if !mentioned {
        let (api_base, api_key, gate_model) = gate_endpoint(ctx, mgr, &config.gate_model).await?;
        let pressure = window::with_group(group, |state| Pressure {
            since_last_spoke: silent_for,
            recent_turns: state.spoken_within(window::RECENT_SPEECH),
            hourly_turns: state.spoken_last_hour(),
        });
        let threshold = config.threshold(mood::snapshot(group), pressure);
        let verdict = gate::judge(
            &api_base,
            &api_key,
            &gate_model,
            config,
            turns,
            &persona,
            &scene,
            None,
        )
        .await?;
        if !verdict.wants_composition(threshold, config.focus_relief, focused) {
            debug!(target: LOG_TARGET, "群 {group} 保持沉默（{}/{}，{}）", verdict.score, threshold, verdict.reason);
            return Ok(());
        }
        info!(target: LOG_TARGET, "群 {group} 交给人格决定（{}/{}，续聊={}，{}）",
            verdict.score, threshold, verdict.continuation, verdict.reason);
        if !current(ctx, group, *seq) {
            return Ok(());
        }
    } else {
        info!(target: LOG_TARGET, "群 {group} 被点名，由人格决定是否回应");
    }
    // 判定之后重新取最新窗口，群友连续发几条消息不必从头再筛一遍。
    let (latest, mentioned, summoned, rhythm) = window::with_group(group, |state| {
        *seq = state.seq;
        (
            state.recent(config.context_turns.clamp(1, 80)),
            (state.take_mention() && config.reply_on_mention) || mentioned,
            state.take_summon() || summoned,
            state.rhythm(),
        )
    });
    if !current(ctx, group, *seq) {
        return Ok(());
    }
    let scene = Scene::build(group, config, &latest, rhythm);
    speak_up(
        ctx,
        writer,
        mgr,
        group,
        base,
        config,
        &latest,
        Called::of(mentioned, summoned),
        &persona,
        &scene,
        seq,
        doze,
    )
    .await
}

/// 停用配置或群聊推进后，放弃尚未发送的内容，交回 worker 读取新上下文。
fn current(ctx: &Context, group: i64, seq: u64) -> bool {
    let config = crate::plugins::get_config_or_default::<AmbientConfig>(ctx, "ambient");
    config.enabled
        && config.groups.contains(&group)
        && window::with_group(group, |state| state.seq == seq)
}

/// 兼容文字路径该引用哪条消息。
///
/// 优先「叫到我的那条」——@、引用我、戳我，那才是这句回应真正对着的话；没人叫的
/// 时候才落到本批最后一条群友消息。消息号为 0 的平台事件（戳一戳、撤回）引不了。
fn reply_target(turns: &[Turn]) -> Option<i64> {
    let candidates: Vec<&Turn> = turns
        .iter()
        .rev()
        .filter(|turn| !turn.from_me && turn.message_id != 0)
        .collect();
    candidates
        .iter()
        .find(|turn| turn.mentions_me)
        .or_else(|| candidates.first())
        .map(|turn| turn.message_id)
}

/// 这条消息实际引谁。
///
/// 模型点名的那条优先——但得真在本批记录里，否则它随口写的一个消息号会让整条消息
/// 引到不存在的目标上；点不出或没点名，才用 [`reply_target`] 的默认目标。
fn quote_target(explicit: Option<i64>, fallback: Option<i64>, turns: &[Turn]) -> Option<i64> {
    explicit
        .filter(|id| *id != 0 && turns.iter().any(|turn| turn.message_id == *id))
        .or(fallback)
}

/// 让人格模型写，然后按人的节奏发出去。
#[allow(clippy::too_many_arguments)]
async fn speak_up(
    ctx: &Context,
    writer: &LockedWriter,
    mgr: &Arc<crate::plugins::oai::data::Manager>,
    group: i64,
    base: &Path,
    config: &AmbientConfig,
    turns: &[Turn],
    called: Called,
    persona: &str,
    scene: &Scene,
    seq: &mut u64,
    // 这一句是睡着时的自主开口，用来计进高峰时段的每小时上限。
    doze: bool,
) -> anyhow::Result<()> {
    let oai = crate::plugins::get_config_or_default::<crate::plugins::oai::OaiConfig>(ctx, "oai");
    // 与判定看到的是同一批图：已转码成模型收得下的格式，GIF 表情包也不例外。
    // 每张都带着出处（来自哪条消息），人格据此把「讲的那张」对回正确的消息号。
    let images = vision::usable_images(turns, config.context_images).await;

    let started = Instant::now();
    let (api_base, api_key, reply_model) = gate_endpoint(ctx, mgr, &config.reply_model).await?;
    let raw = speak::compose(
        &api_base,
        &api_key,
        &reply_model,
        base,
        &skill_dirs(base),
        persona,
        config,
        &oai.search,
        oai.request_stall(),
        turns,
        &images,
        called,
        scene,
        Some((ctx, writer, group, seq)),
    )
    .await?;

    let (raw, focus) = attention::extract(&raw, turns, config.focus_max_seconds);
    // 沉默不需要检查草稿；新消息留给下一批。关注仍可在本轮更新。
    let silent = matches!(
        pace::parse(&raw, config.messages_budget.clamp(1, 5), config.split_chars),
        pace::Speech::Silent
    );
    if !silent && !current(ctx, group, *seq) {
        let (latest_seq, latest, rhythm) = window::with_group(group, |state| {
            (
                state.seq,
                state.recent(config.context_turns.clamp(1, 80)),
                state.rhythm(),
            )
        });
        if !current(ctx, group, latest_seq) {
            return Ok(());
        }
        let (api_base, api_key, gate_model) = gate_endpoint(ctx, mgr, &config.gate_model).await?;
        let fresh = Scene::build(group, config, &latest, rhythm);
        let verdict = gate::judge(
            &api_base,
            &api_key,
            &gate_model,
            config,
            &latest,
            persona,
            &fresh,
            Some(&raw),
        )
        .await?;
        if verdict.score < 50 || !current(ctx, group, latest_seq) {
            debug!(target: LOG_TARGET, "群 {group} 收起过时草稿：{}", verdict.reason);
            return Ok(());
        }
        // 检查通过的这批也已处理；不能发完再把同一批当作新消息回应。
        window::with_group(group, |state| {
            if state.seq == latest_seq {
                state.take_mention();
            }
        });
        *seq = latest_seq;
    }
    if let Some(focus) = focus {
        window::with_group(group, |state| state.focus = focus);
    }
    let mut utterances =
        match pace::parse(&raw, config.messages_budget.clamp(1, 5), config.split_chars) {
            pace::Speech::Silent => {
                info!(target: LOG_TARGET, "群 {group} 想了想，还是没说话");
                return Ok(());
            }
            pace::Speech::Say(items) => items,
        };
    // 人不会把刚说过的话换个标点再说一遍；小模型在同一段上下文里被反复唤起时会。
    let history = window::with_group(group, |state| state.recent(40));
    utterances.retain(|utterance| {
        let text = plain_text(&utterance.message);
        if tone::echoes(&text, &history) {
            info!(target: LOG_TARGET, "群 {group} 咽回一句复读：{text}");
            return false;
        }
        true
    });
    if utterances.is_empty() {
        return Ok(());
    }

    // 兼容文字路径要引谁：模型用 `[reply:消息号]` 点名了就引那条（须在本批记录里，
    // 免得它凭记忆写一个引不到或引错的消息号）；没点名才退回本批默认目标——优先
    // 「叫到我的那条」，没人叫就引最新那条群友消息。两张图片消息挨着来时，默认目标
    // 只会是后一张，模型讲的是前一张就露馅了，所以点名这一路要留给它。
    let fallback = reply_target(turns);
    let pace = config.pace(mood::snapshot(group));
    tokio::time::sleep(pace.think_delay(started.elapsed())).await;

    let me = ctx
        .bot
        .login_user
        .get()
        .id
        .parse::<i64>()
        .unwrap_or_default();
    let mut sent = false;
    for (index, utterance) in utterances.into_iter().enumerate() {
        if index > 0 {
            tokio::time::sleep(pace.gap()).await;
        }
        if utterance.wait > 0.0 {
            tokio::time::sleep(Duration::from_secs_f32(utterance.wait)).await;
        }
        let typing = pace.typing_delay(utterance.chars);
        // 模型耗时已经是等待；首条不再额外假装打字十几秒。
        tokio::time::sleep(if index == 0 {
            typing.saturating_sub(started.elapsed())
        } else {
            typing
        })
        .await;
        if !current(ctx, group, *seq) {
            break;
        }

        let mut message = Message::new();
        if utterance.reply
            && let Some(id) = quote_target(utterance.reply_to, fallback, turns)
        {
            message = message.reply(id);
        }
        message.0.extend(utterance.message.0.iter().cloned());
        let spoken = plain_text(&utterance.message);
        let id = match send_fresh_msg_id(
            ctx,
            writer.clone(),
            Some(group),
            None,
            &message,
            freshness_for(group, config.freshness_window()),
        )
        .await
        {
            Ok(Some(id)) => id.parse::<i64>().unwrap_or_default(),
            Ok(None) => {
                info!(target: LOG_TARGET, "群 {group} 这句话没发出去：交给 QQ 之前群里又说了话");
                break;
            }
            Err(error) => {
                warn!(target: LOG_TARGET, "群 {group} 发言发送失败：{error}");
                break;
            }
        };
        // 出站日志里所有插件的消息长得一样，复读机复读一句群友原话与搭话开口无从分辨。
        // 记下自己说了什么，这一行既是回放，也是唯一能确认「它真的开口了」的凭据。
        // 带上调子（活/平/静）：回头对「今天为什么这么说」时，这是第一眼要看的东西。
        info!(
            target: LOG_TARGET,
            "群 {group} 说（{}）：{spoken}",
            voice_register(config, group).label()
        );
        window::with_group(group, |state| {
            if !sent {
                state.mark_spoke();
                // 睡着时的自主开口单独计数，好让高峰时段的费用有每小时硬上限。
                if doze {
                    state.mark_doze_spoke();
                }
            }
            // 服务端自发事件可能先到；按回执 ID 去重。
            state.receive(Turn {
                user_id: me,
                name: "我".to_string(),
                text: spoken,
                elements: message.clone(),
                images: Vec::new(),
                message_id: id,
                mentions_me: false,
                call: window::Call::default(),
                from_me: true,
                at: chrono::Local::now().timestamp(),
            });
        });
        sent = true;
    }
    if sent {
        if config.mood_enabled {
            mood::nudge(|mood, now| mood.spoke(group, now));
        }
        if config.memory_enabled
            && let Some(target) = turns.iter().rev().find(|turn| !turn.from_me)
        {
            let (id, at) = (target.user_id, chrono::Local::now().timestamp());
            memory::edit(group, |memory| memory.exchange(id, at));
        }
    }
    Ok(())
}

pub fn default_config() -> Value {
    build_config(AmbientConfig::default())
}

/// Validate control edits against the plugin's actual configuration type.
pub fn validate_config(value: &toml::Value) -> Result<(), String> {
    <AmbientConfig as serde::Deserialize>::deserialize(value.clone())
        .map(|_| ())
        .map_err(|_| "配置类型不匹配（请检查数组元素、字段类型及整数范围）".to_string())
}

/// 启动：铺开人设与 skill，并确保模型接口那一份共享配置已就绪。
pub fn init(_ctx: Context) -> BoxFuture<'static, Result<(), PluginError>> {
    Box::pin(async move {
        let dir = get_data_dir("ambient").await?;
        if let Err(error) = setup(&dir).await {
            warn!(target: LOG_TARGET, "群聊搭话资源初始化失败: {error}");
        }
        DATA_DIR.set(dir).ok();
        // 接口地址、密钥与供应商表都归 oai 插件管（见 [oai.providers]）；它没启用时，
        // 这里把管理器建起来，搭话只借它取默认接口与密钥，不碰房间那一摊。
        let mgr = match crate::plugins::oai::ensure_manager().await {
            Ok(mgr) => Some(mgr),
            Err(error) => {
                warn!(target: LOG_TARGET, "模型接口配置初始化失败: {error}");
                crate::plugins::oai::data::MANAGER.get().cloned()
            }
        };
        // 能力层的数据目录通常已经由 oai 插件挂好；它没跑起来时在这里补一次，
        // 免得记忆与表情包库静默变成空的（挂两次是安全的）。
        if let Some(dir) = mgr.as_ref().and_then(|mgr| mgr.path.parent())
            && let Err(error) = crate::plugins::oai::chat::attach(dir).await
        {
            warn!(target: LOG_TARGET, "群聊能力层的数据目录初始化失败：{error}");
        }
        Ok(())
    })
}

/// 流水线入口：记下群消息，事件照常往后传。
///
/// 注册在 `oai` 之后，所以走到这里的是没被 `oai` 的指令消费掉的群聊；
/// 本插件自己不消费任何事件，只是旁观。
pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let (Some(base), Some(mgr)) = (
            DATA_DIR.get().cloned(),
            crate::plugins::oai::data::MANAGER.get().cloned(),
        ) else {
            return Ok(Some(ctx));
        };
        observe(&ctx, &writer, &mgr, &base).await;
        Ok(Some(ctx))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use simd_json::OwnedValue;

    fn event(value: serde_json::Value) -> OwnedValue {
        simd_json::serde::to_owned_value(value).unwrap()
    }

    #[test]
    fn default_models_match_but_explicit_overrides_are_preserved() {
        let config: AmbientConfig = toml::from_str("").unwrap();
        assert_eq!(config.gate_model, "deepseek/deepseek-flash");
        assert_eq!(config.reply_model, "deepseek/deepseek-flash");
        assert_eq!(config.gate_interval(), Duration::from_secs(30));
        // 发言温度默认比接口默认松一档，判定仍是接口默认。
        assert_eq!(config.temperature, Some(1.3));
        // 判定人设默认是浓缩画像，比完整人设便宜得多，且不会被空值覆盖。
        assert!(!config.gate_persona.trim().is_empty());
        let custom: AmbientConfig =
            toml::from_str("gate_model = 'custom-gate'\nreply_model = 'custom/custom-reply'")
                .unwrap();
        assert_eq!(custom.gate_model, "custom-gate");
        assert_eq!(custom.reply_model, "custom/custom-reply");
        // 温度可以逐实例改；不写就是上面那个默认值。
        let cooled: AmbientConfig = toml::from_str("temperature = 0.8").unwrap();
        assert_eq!(cooled.temperature, Some(0.8));
        // 显式清空 gate_persona 时判定回退用完整人设。
        let no_gate_persona: AmbientConfig = toml::from_str("gate_persona = ''").unwrap();
        assert!(no_gate_persona.gate_persona.trim().is_empty());
    }

    #[test]
    fn calls_and_new_images_skip_the_ordinary_gate_interval() {
        let turn = Turn::default();
        assert!(!immediate_gate(&[turn.clone()], false, false, false));
        assert!(immediate_gate(&[turn.clone()], true, false, false));
        assert!(immediate_gate(&[turn.clone()], false, true, false));
        assert!(immediate_gate(&[turn.clone()], false, false, true));
        let mut named = turn.clone();
        named.call.named_me = true;
        assert!(immediate_gate(&[named], false, false, false));
        let mut image = turn;
        image.images.push("image.png".into());
        assert!(immediate_gate(&[image], false, false, false));
    }

    /// 人设是每轮都要付一次钱的东西，而它天然会长：每发现一种不满意的说法，
    /// 就想再加一句话把它堵住。这个上限不是审美洁癖，是提醒——要加一段之前，
    /// 先看看能不能删两段。真正的边界只有一条（色情与情感纠缠），协议在现场说明里，
    /// 工具怎么用在 skill 里，剩下的都该是「他是谁」。
    #[test]
    fn the_persona_stays_short_enough_to_pay_for_every_round() {
        assert!(
            PERSONA.len() < 4096,
            "内置人设 {} 字节，超出预算了：先删再加",
            PERSONA.len()
        );
        // 判定用的画像比完整人设还要便宜一大截——每条消息都要过它一遍。
        assert!(GATE_PERSONA.len() < PERSONA.len());
        // 唯一的硬边界仍然写着。
        assert!(PERSONA.contains("色情"));
    }

    /// 本体档案只读事实证明行，注释与空行不进提示词；仓库里那份模板整篇都是说明，
    /// 所以新装一份等于没有档案，不会凭空给谁安上几台设备。
    #[test]
    fn the_profile_reads_facts_and_keeps_the_template_out_of_the_prompt() {
        let text = facts_from("# 说明\n\n- 手上是台安卓\n手边还有台平板\n# 又一行注释\n");
        assert!(text.contains("- 手上是台安卓"), "{text}");
        assert!(text.contains("- 手边还有台平板"), "{text}");
        assert!(
            !text.contains("说明") && !text.contains("又一行注释"),
            "{text}"
        );
        assert!(facts_from("").is_empty());
        assert!(
            facts_from(SELF).is_empty(),
            "模板里留了没注释掉的事实行：{}",
            facts_from(SELF)
        );
    }

    /// 发言那一轮带着他自己的说法与样本；判定那一轮不带——判定是每条消息都要
    /// 付一次的账，而「说出来的像不像他」跟「要不要接这句话」是两码事。
    #[test]
    fn only_the_speaking_round_carries_his_own_words() {
        let config = AmbientConfig::default();
        let turns = vec![Turn {
            user_id: 7,
            name: "群友".into(),
            text: "这台折叠屏值不值".into(),
            ..Turn::default()
        }];
        let scene = Scene::build(-1, &config, &turns, "刚接了两次话".into());
        // 调子按当下状态浮动，所以认的是「哪一段在不在」，不是具体一句话。
        let tones = [
            mood::Register::Lively,
            mood::Register::Even,
            mood::Register::Calm,
        ];
        assert!(
            tones
                .iter()
                .any(|tone| scene.own.contains(voice::opening(*tone))),
            "{}",
            scene.own
        );
        assert!(
            scene
                .own
                .lines()
                .any(|line| line.starts_with("- ") && line.contains("折叠")),
            "贴题的样本没被挑出来：{}",
            scene.own
        );
        for tone in tones {
            assert!(
                !scene.brief().contains(voice::opening(tone)),
                "判定侧不该带上样本段：{}",
                scene.brief()
            );
        }
    }

    /// 偷来的表情包摆在发言那一轮，判定那一轮不摆——判定看的是「要不要接这句话」，
    /// 货架上有什么对它没用，而那是每条消息都要付一次的账。
    #[test]
    fn the_sticker_shelf_is_only_in_the_speaking_round() {
        let _guard = memory::exclusive();
        let dir = stickers::tests::scratch("scene");
        let source = Turn {
            user_id: 7,
            name: "老张".into(),
            text: "笑死".into(),
            ..Turn::default()
        };
        let mut data = simd_json::owned::Object::new();
        data.insert("emoji_id".into(), simd_json::owned::Value::from("296f"));
        data.insert("emoji_package_id".into(), simd_json::owned::Value::from("241904"));
        let segment = crate::message::Segment::new("mface", data);
        stickers::keep(&segment, &source, -1, "捂着嘴笑", None, 20);

        let config = AmbientConfig::default();
        let turns = vec![Turn {
            user_id: 7,
            name: "群友".into(),
            text: "这台折叠屏值不值".into(),
            ..Turn::default()
        }];
        let scene = Scene::build(-1, &config, &turns, "刚接了两次话".into());
        assert!(scene.own.contains("捂着嘴笑"), "{}", scene.own);
        assert!(!scene.brief().contains("捂着嘴笑"), "{}", scene.brief());
        // 库关掉（`sticker_max = 0`）时这一段整个不出现。
        let off = AmbientConfig {
            sticker_max: 0,
            ..AmbientConfig::default()
        };
        let scene = Scene::build(-1, &off, &turns, "刚接了两次话".into());
        assert!(!scene.own.contains("捂着嘴笑"), "{}", scene.own);
        let _ = std::fs::remove_dir_all(dir);
    }


    #[test]
    fn defaults_stay_silent_until_a_group_is_named() {
        let config = AmbientConfig::default();
        assert!(!config.enabled);
        assert!(config.groups.is_empty());
        assert!(config.max_wait() >= config.debounce());
        // 默认带一条时间下限与一条每小时目标：前者挡住「刚说完又想接」，后者给
        // 聊嗨了的时段兜底。两条都不是墙，而是门槛上的一笔加价（见
        // `cooldown_and_the_hourly_budget_raise_the_bar_instead_of_shutting_the_door`），
        // 所以有人真的在等它回话时不会被挡在外面。
        assert_eq!(config.cooldown(), Duration::from_secs(90));
        assert_eq!(config.cooldown_penalty, 25);
        assert_eq!(config.max_per_hour, 8);
        assert_eq!(config.budget_penalty, 12);
        assert_eq!(config.effective_threshold(None), config.score_threshold);
        let extreme = AmbientConfig {
            debounce_seconds: u64::MAX,
            max_pending_seconds: 0,
            silence_relief_cap: 20,
            ..config
        };
        assert!(extreme.max_wait() >= extreme.debounce());
        assert_eq!(extreme.effective_threshold(None), extreme.score_threshold);
    }

    /// 默认就带上时效条件：话说晚了不如不说。写 0 才回到从前的无条件发送。
    #[test]
    fn utterances_expire_by_default_and_the_window_stays_sane() {
        let config = AmbientConfig::default();
        assert_eq!(config.freshness_window(), Duration::from_secs(25));
        let off = AmbientConfig {
            send_freshness_seconds: 0,
            ..AmbientConfig::default()
        };
        assert!(off.freshness_window().is_zero());
        // 极端值被夹回可用区间，不会变成「一发出去就过期」或「永远有效」。
        let silly = AmbientConfig {
            send_freshness_seconds: 1,
            ..AmbientConfig::default()
        };
        assert_eq!(silly.freshness_window(), Duration::from_secs(3));
        let huge = AmbientConfig {
            send_freshness_seconds: u64::MAX,
            ..AmbientConfig::default()
        };
        assert_eq!(huge.freshness_window(), Duration::from_secs(300));
        // 旧配置里没有这个键也读得出来，取默认值。
        let legacy: AmbientConfig = toml::from_str("groups = [1]").unwrap();
        assert_eq!(legacy.send_freshness_seconds, config.send_freshness_seconds);
    }

    #[test]
    fn the_longer_it_stays_quiet_the_lower_the_bar() {
        let config = AmbientConfig {
            silence_relief_per_10min: 5,
            silence_relief_cap: 20,
            ..AmbientConfig::default()
        };
        assert_eq!(
            config.effective_threshold(Some(Duration::ZERO)),
            config.score_threshold
        );
        assert_eq!(
            config.effective_threshold(Some(Duration::from_secs(20 * 60))),
            config.score_threshold - 10
        );
        // 补偿有上限，久不出声也不会见什么接什么。
        assert_eq!(
            config.effective_threshold(Some(Duration::from_secs(10 * 3_600))),
            config.score_threshold - config.silence_relief_cap
        );
        // 从没说过话等同于沉默了很久。
        assert_eq!(
            config.effective_threshold(None),
            config.score_threshold - config.silence_relief_cap
        );
    }

    /// 峰谷价是 DeepSeek 一家的事：判定与发言都不走它家时，`[ambient.peak]` 整段
    /// 让路，全天照常跑；两个模型里但凡有一个还在 DeepSeek 上，这一轮就仍有一半
    /// 的钱可省，休眠照旧。换回来那天不必改配置。
    #[test]
    fn peak_hours_only_apply_while_a_deepseek_model_is_in_the_round() {
        use chrono::TimeZone as _;
        // 2026-09-10 是周四：上午十点在 DeepSeek 的高峰里，晚八点在空闲时段。
        let beijing = chrono::FixedOffset::east_opt(8 * 3_600).unwrap();
        let peak_time = beijing
            .with_ymd_and_hms(2026, 9, 10, 10, 0, 0)
            .single()
            .expect("本机时区里这个时刻存在");
        let off_peak = beijing
            .with_ymd_and_hms(2026, 9, 10, 20, 0, 0)
            .single()
            .expect("本机时区里这个时刻存在");

        let config = AmbientConfig::default();
        assert!(config.peak_applies());
        assert_eq!(config.peak_stance_at(peak_time), peak::Stance::Dozing);

        let other = AmbientConfig {
            gate_model: "mimo/mimo-v2.6-flash".to_string(),
            reply_model: "mimo/mimo-v2.6-flash".to_string(),
            ..AmbientConfig::default()
        };
        assert!(!other.peak_applies());
        assert_eq!(other.peak_stance_at(peak_time), peak::Stance::Awake);

        // 判定留在 DeepSeek 上：判定那一次调用仍按峰谷计价。
        let mixed = AmbientConfig {
            gate_model: "deepseek/deepseek-flash".to_string(),
            ..AmbientConfig::default()
        };
        assert!(mixed.peak_applies());
        assert_eq!(mixed.peak_stance_at(peak_time), peak::Stance::Dozing);
        assert_eq!(mixed.peak_stance_at(off_peak), peak::Stance::Awake);

        // 两个都换回 DeepSeek：那张表原样生效，连 pause 也照旧。
        let deepseek = AmbientConfig {
            gate_model: "deepseek/deepseek-flash".to_string(),
            reply_model: "deepseek/deepseek-flash".to_string(),
            peak: peak::PeakConfig {
                mode: peak::Mode::Pause,
                ..peak::PeakConfig::default()
            },
            ..AmbientConfig::default()
        };
        assert_eq!(deepseek.peak_stance_at(peak_time), peak::Stance::Asleep);
    }

    #[test]
    fn peak_hours_default_to_dozing_through_deepseeks_expensive_window() {
        let config = AmbientConfig::default();
        assert_eq!(config.peak.mode, peak::Mode::Sleep);
        // 醒来那一轮用最省的一份：不看图、上下文减半、少发一条、不绘图。
        let frugal = config.frugal();
        assert_eq!(frugal.context_images, 0);
        assert!(frugal.context_turns < config.context_turns);
        assert!(frugal.messages_budget <= 2);
        assert_eq!(frugal.draw_budget, 0);
        // 联网不便宜也更慢，高峰时段这一句先不查。
        assert!(!frugal.search_enabled);
        // 其余设置原样带过去。
        assert_eq!(frugal.reply_model, config.reply_model);
        assert_eq!(frugal.groups, config.groups);
        assert_eq!(config.peak.doze_gate(), Duration::ZERO);
        assert_eq!(config.peak.doze_reply_limit, 2);
        // 没配替补模型，高峰那一轮仍用主模型。
        assert!(config.peak.model.is_empty());
        // 旧配置里没有这张表也能读出来。
        let legacy: AmbientConfig = toml::from_str("groups = [1]").unwrap();
        assert_eq!(legacy.peak.windows, config.peak.windows);
        assert_eq!(legacy.peak.doze_gate(), config.peak.doze_gate());
        assert!(legacy.peak.model.is_empty());
    }

    /// `swap` 模式：高峰照常跑，只把两个模型换成替补，其余一概不动（联网、看图、
    /// 绘图都照旧）；`model` 留空时这一档与照常无异，不瞎换。
    #[test]
    fn swap_mode_only_trades_the_models_during_peak_hours() {
        use chrono::TimeZone as _;
        let beijing = chrono::FixedOffset::east_opt(8 * 3_600).unwrap();
        let peak_time = beijing
            .with_ymd_and_hms(2026, 9, 10, 10, 0, 0)
            .single()
            .expect("北京时间里这个时刻存在");
        let off_peak = beijing
            .with_ymd_and_hms(2026, 9, 10, 20, 0, 0)
            .single()
            .expect("北京时间里这个时刻存在");

        let config = AmbientConfig {
            peak: peak::PeakConfig {
                mode: peak::Mode::Swap,
                model: "mimo/mimo-v2.6-flash".to_string(),
                ..peak::PeakConfig::default()
            },
            ..AmbientConfig::default()
        };
        assert_eq!(config.peak_stance_at(peak_time), peak::Stance::Swapped);
        assert_eq!(config.peak_stance_at(off_peak), peak::Stance::Awake);
        let swapped = config.swapped();
        assert_eq!(swapped.gate_model, "mimo/mimo-v2.6-flash");
        assert_eq!(swapped.reply_model, "mimo/mimo-v2.6-flash");
        // 只换模型，别的照常：联网、看图、绘图与上下文都在，这一档不省这些。
        assert_eq!(swapped.search_enabled, config.search_enabled);
        assert_eq!(swapped.context_images, config.context_images);
        assert_eq!(swapped.context_turns, config.context_turns);
        assert_eq!(swapped.draw_budget, config.draw_budget);
        assert_eq!(swapped.messages_budget, config.messages_budget);
        // 主配置不动：离峰那一轮仍走 DeepSeek。
        assert_eq!(config.gate_model, "deepseek/deepseek-flash");
        assert_eq!(config.reply_model, "deepseek/deepseek-flash");

        // 没配替补时 `swap` 不该凭空改行为，退回照常。
        let no_model = AmbientConfig {
            peak: peak::PeakConfig {
                mode: peak::Mode::Swap,
                ..peak::PeakConfig::default()
            },
            ..AmbientConfig::default()
        };
        assert_eq!(no_model.peak_stance_at(peak_time), peak::Stance::Awake);
    }

    /// 睡着那一档（`sleep`）醒来时，连模型一起换，同时换上最省的一份上下文。
    #[test]
    fn dozing_rounds_take_the_substitute_model_and_the_thriftiest_context() {
        let config = AmbientConfig {
            peak: peak::PeakConfig {
                model: "mimo/mimo-v2.6-flash".to_string(),
                ..peak::PeakConfig::default()
            },
            ..AmbientConfig::default()
        };
        let frugal = config.frugal();
        assert_eq!(frugal.gate_model, "mimo/mimo-v2.6-flash");
        assert_eq!(frugal.reply_model, "mimo/mimo-v2.6-flash");
        // 省钱那一档更紧：不看图、上下文减半、不出网。
        assert_eq!(frugal.context_images, 0);
        assert!(frugal.context_turns < config.context_turns);
        assert!(!frugal.search_enabled);
        assert_eq!(frugal.groups, config.groups);
    }

    /// 联网搜索对搭话是默认开着的：遇到不认识的梗、新版本、比赛战况，先查再开口。
    /// 房间 agent 那边默认关，两个开关互不影响。
    #[test]
    fn search_is_on_by_default_for_ambient_and_shared_with_rooms_only_in_config() {
        let config = AmbientConfig::default();
        assert!(config.search_enabled);
        assert_eq!(config.search_budget, 3);
        // 旧配置里没有这两个键也读得出来。
        let legacy: AmbientConfig = toml::from_str("groups = [1]").unwrap();
        assert!(legacy.search_enabled);
        assert_eq!(legacy.search_budget, config.search_budget);
        // 想关就写 false；写 0 也等于关。
        let off: AmbientConfig = toml::from_str("search_enabled = false").unwrap();
        assert!(!off.search_enabled);
        let zero: AmbientConfig = toml::from_str("search_budget = 0").unwrap();
        assert_eq!(zero.search_budget, 0);
    }

    #[test]
    fn legacy_and_partial_tables_fall_back_to_defaults() {
        let config: AmbientConfig = toml::from_str("groups = [123]\nunknown_key = 1").unwrap();
        assert_eq!(config.groups, [123]);
        assert_eq!(config.gate_model, AmbientConfig::default().gate_model);
    }



    #[test]
    fn platform_events_preserve_targets_without_inventing_reaction_authors() {
        let poke = event(
            serde_json::json!({"satori_type":"internal","sub_type":"poke","user_id":42,"satori_data":{"target_id":"10000"}}),
        );
        let (turn, _) = notice_turn(&poke, 10000).unwrap();
        assert!(turn.mentions_me);
        assert!(turn.call.poked_me, "被戳要单独记下来，接法跟被 @ 不一样");
        assert!(!turn.from_me);
        assert_eq!(turn.message_id, 0);
        let reaction = event(
            serde_json::json!({"satori_type":"reaction-added","message_id":123,"_satori":{"emoji":{"id":"76"}}}),
        );
        let (turn, _) = notice_turn(&reaction, 10000).unwrap();
        assert_eq!(turn.user_id, 0);
        assert!(turn.from_me);
        assert!(!turn.mentions_me);
        assert!(window::transcript(&[turn]).contains("操作者未知"));
        let recall = event(
            serde_json::json!({"satori_type":"message-deleted","message_id":123,"user_id":42}),
        );
        assert_eq!(notice_turn(&recall, 10000).unwrap().1, Some(123));
    }

    #[test]
    fn environment_notices_keep_subjects_separate_from_the_bot() {
        for (raw, expected) in [
            (
                serde_json::json!({"satori_type":"guild-member-updated","notice_type":"group_ban","user_id":42,"duration":60}),
                "42 被禁言 60 秒",
            ),
            (
                serde_json::json!({"satori_type":"guild-member-updated","notice_type":"group_ban","user_id":0,"duration":0}),
                "全体成员 已解除禁言",
            ),
            (
                serde_json::json!({"satori_type":"guild-member-updated","notice_type":"group_member_update","user_id":42,"_satori":{"member":{"nick":"新名片"}}}),
                "新名片",
            ),
            (
                serde_json::json!({"satori_type":"channel-updated","_satori":{"channel":{"name":"新群名"}}}),
                "新群名",
            ),
            (
                serde_json::json!({"satori_type":"reaction-deleted","message_id":123,"_satori":{"emoji":{"id":"76"}}}),
                "减少表态",
            ),
        ] {
            let (turn, _) = notice_turn(&event(raw), 10000).unwrap();
            assert!(turn.text.contains(expected), "{}", turn.text);
            assert_eq!(turn.user_id, 0);
            assert!(turn.from_me && !turn.mentions_me);
            assert!(!window::transcript(&[turn]).contains("你自己"));
        }
        for kind in ["guild-member-added", "guild-member-removed"] {
            let (turn, _) = notice_turn(
                &event(serde_json::json!({"satori_type":kind,"user_id":42})),
                10000,
            )
            .unwrap();
            assert!(!turn.from_me);
            assert_eq!(turn.user_id, 42);
        }
    }

    /// 引用解析的三条路：引到别人、引到自己（等于被点名）、引到窗口外的旧消息。

    /// 兼容文字路径的引用目标：先引叫到我的那条，没人叫才引最新一条；
    /// 引不了的消息（消息号为 0 的平台事件）与自己的话都跳过。
    #[test]
    fn the_legacy_reply_quotes_the_message_that_called_us() {
        let spoken = |id: i64, from_me: bool, mentioned: bool| Turn {
            message_id: id,
            from_me,
            mentions_me: mentioned,
            call: window::Call {
                at_me: mentioned,
                ..window::Call::default()
            },
            ..Turn::default()
        };
        // 本批最后一条只是别人在闲聊，但 @ 我的那条在更前面：引它。
        let turns = [
            spoken(11, false, true),
            spoken(12, false, false),
            spoken(13, true, false),
        ];
        assert_eq!(reply_target(&turns), Some(11));
        // 没人叫我：引最新一条群友消息。
        let turns = [spoken(11, false, false), spoken(13, true, false)];
        assert_eq!(reply_target(&turns), Some(11));
        // 消息号为 0 的平台事件（戳一戳）不能引；只有它时就没人可引。
        let poked = Turn {
            mentions_me: true,
            call: window::Call {
                poked_me: true,
                ..window::Call::default()
            },
            ..Turn::default()
        };
        assert_eq!(reply_target(&[poked]), None);
        assert_eq!(reply_target(&[]), None);
    }

    /// 模型点名引用时以它为准——两条图片消息挨着发来的场景就靠这一条定准。
    #[test]
    fn an_explicit_quote_target_wins_over_the_default() {
        let turn = |id: i64| Turn {
            message_id: id,
            ..Turn::default()
        };
        // 两条群友消息，谁也没叫我：默认只会引最新那条（12，也就是第二张图）。
        let turns = [turn(11), turn(12)];
        let fallback = reply_target(&turns);
        assert_eq!(fallback, Some(12), "没人叫我时默认引最新一条");
        // 模型讲的是第一张图，点名引 11：就算默认目标是 12，也听它的。
        assert_eq!(quote_target(Some(11), fallback, &turns), Some(11));
        // 点名的是一个本批记录里没有的消息号：退回默认目标，别引到引不到的地方。
        assert_eq!(quote_target(Some(999), fallback, &turns), Some(12));
        // 没点名就照默认来。
        assert_eq!(quote_target(None, fallback, &turns), Some(12));
        // 默认也引不了（记录里全是自己或消息号为 0 的平台事件）时，点名仍能定准。
        assert_eq!(quote_target(Some(11), None, &turns), Some(11));
        assert_eq!(quote_target(None, None, &turns), None);
    }

    #[test]
    fn spoken_messages_are_written_back_as_readable_text() {
        let message = Message::new().at(114_514).text("这步缺前提").face(178);
        // at 用发言侧那种标记写法回写：它自己在记录里看到的、能再用的就是这种。
        assert_eq!(plain_text(&message), "[at:114514] 这步缺前提[表情]");
    }

    /// 别人 @ 谁，记录里写成 `[at:QQ号]`——与发言侧同一种写法。写 `@QQ号` 时人格会
    /// 照着抄进正文，群里冒出一串光秃秃的号码（线上记录 id 105888）。

    #[test]
    fn the_scene_carries_every_local_anchor_and_drops_the_ones_turned_off() {
        let _guard = memory::exclusive();
        let group = -9_100_001;
        let turns: Vec<Turn> = (0..6)
            .map(|index| Turn {
                user_id: 42,
                name: "老张".into(),
                text: "这破依赖装了半天".into(),
                message_id: index + 1,
                at: chrono::Local::now().timestamp() + index * 20,
                ..Turn::default()
            })
            .collect();
        memory::edit(group, |memory| {
            for _ in 0..10 {
                memory.see(42, "老张", chrono::Local::now().timestamp() - 86_400);
            }
            memory.remember(42, "在修驾校那台破电脑").unwrap();
        });
        let config = AmbientConfig::default();
        let brief = Scene::build(group, &config, &turns, "尚未发言".into()).brief();
        assert!(brief.starts_with("现在："), "{brief}");
        assert!(brief.contains("本群此刻："), "{brief}");
        assert!(brief.contains("你现在的状态："), "{brief}");
        assert!(brief.contains("当前参与状态：尚未发言"), "{brief}");
        assert!(brief.contains("在修驾校那台破电脑"), "{brief}");

        // 两个开关各自关掉自己那段，别的照旧。
        let quiet = AmbientConfig {
            memory_enabled: false,
            mood_enabled: false,
            ..AmbientConfig::default()
        };
        let brief = Scene::build(group, &quiet, &turns, "尚未发言".into()).brief();
        assert!(brief.contains("本群此刻："), "{brief}");
        assert!(!brief.contains("你现在的状态："), "{brief}");
        assert!(!brief.contains("在修驾校那台破电脑"), "{brief}");
    }

    /// 身份跟着现场一起递给判定：群里叫人用的是名片上的字，判定要认得出来。
    ///
    /// 还没问到平台之前这一段是空的——宁可不带，也不能摆一份空表让模型去填。
    #[test]
    fn the_scene_carries_the_name_the_room_sees() {
        let group = -9_100_002;
        let turns = [Turn {
            user_id: 42,
            name: "老张".into(),
            text: "A宝在吗".into(),
            ..Turn::default()
        }];
        let config = AmbientConfig::default();
        let bare = Scene::build(group, &config, &turns, "尚未发言".into()).brief();
        assert!(!bare.contains("你自己："), "{bare}");
        identity::seed(
            group,
            identity::Identity {
                user_id: 3373167460,
                name: "nawyjx".into(),
                card: "A宝好腻害！".into(),
                group_name: "②群心情管家•助手".into(),
                ..identity::Identity::default()
            },
        );
        let brief = Scene::build(group, &config, &turns, "尚未发言".into()).brief();
        assert!(brief.contains("群里看到的你叫「A宝好腻害！」"), "{brief}");
        assert!(brief.contains("②群心情管家•助手"), "{brief}");
        // 现在与语感照旧在它前后，身份只是插进来的一段。
        assert!(brief.starts_with("现在："), "{brief}");
        assert!(brief.contains("本群此刻："), "{brief}");
    }

    #[test]
    fn state_moves_the_bar_and_the_keyboard_only_while_it_is_enabled() {
        let config = AmbientConfig::default();
        let tired = mood::Snapshot {
            energy: 0.15,
            warmth: 0.1,
        };
        let lively = mood::Snapshot {
            energy: 0.9,
            warmth: 0.9,
        };
        assert!(config.threshold(tired, Pressure::default()) > config.threshold(lively, Pressure::default()));
        assert!(config.pace(tired).typing_cpm < config.pace(lively).typing_cpm);
        assert!(config.pace(tired).think_seconds > config.pace(lively).think_seconds);
        // 门槛仍留在有效区间里，不会被状态推到 0 或爆表。
        assert!((1..=100).contains(&config.threshold(tired, Pressure::default())));
        let fixed = AmbientConfig {
            mood_enabled: false,
            ..AmbientConfig::default()
        };
        assert_eq!(
            fixed.threshold(tired, Pressure::default()),
            fixed.threshold(lively, Pressure::default())
        );
        assert_eq!(fixed.pace(tired).typing_cpm, fixed.typing_cpm);
    }

    #[test]
    fn the_more_it_just_said_the_higher_the_bar_gets() {
        let config = AmbientConfig::default();
        let calm = mood::Snapshot {
            energy: 0.55,
            warmth: 0.35,
        };
        let quiet = config.threshold(calm, Pressure::default());
        assert_eq!(quiet, config.score_threshold);
        // 说过的每一轮都在抬价，但抬到封顶就不再往上。
        let once = Pressure {
            recent_turns: 1,
            ..Pressure::default()
        };
        assert_eq!(
            config.threshold(calm, once),
            quiet + config.speech_penalty_per_turn
        );
        let crowded = Pressure {
            recent_turns: 9,
            ..Pressure::default()
        };
        assert_eq!(
            config.threshold(calm, crowded),
            quiet + config.speech_penalty_cap
        );
        // 关掉这笔加价就回到从前的行为。
        let loose = AmbientConfig {
            speech_penalty_per_turn: 0,
            ..AmbientConfig::default()
        };
        let five = Pressure {
            recent_turns: 5,
            ..Pressure::default()
        };
        assert_eq!(loose.threshold(calm, five), quiet);
    }

    /// 冷却不再是「一到点就整段拦下」：刚开过口时门槛按剩下的时间抬价，走到窗口
    /// 末尾回到 0；这个小时说超的部分同样一轮一轮往上加。两笔都只是抬价，
    /// 分数确实高的时候照样放行。
    #[test]
    fn cooldown_and_the_hourly_budget_raise_the_bar_instead_of_shutting_the_door() {
        let config = AmbientConfig::default();
        let calm = mood::Snapshot {
            energy: 0.55,
            warmth: 0.35,
        };
        let base = config.threshold(calm, Pressure::default());

        // 刚说完：满额（默认 25），越接近冷却末尾退得越低。
        let just_spoke = config.threshold(
            calm,
            Pressure {
                since_last_spoke: Some(Duration::ZERO),
                ..Pressure::default()
            },
        );
        assert_eq!(just_spoke, base + config.cooldown_penalty);
        let halfway = config.threshold(
            calm,
            Pressure {
                since_last_spoke: Some(Duration::from_secs(config.cooldown_seconds / 2)),
                ..Pressure::default()
            },
        );
        assert!(halfway < just_spoke && halfway > base);
        // 冷却走完、以及从没开过口，都不加这一笔。
        let after = config.threshold(
            calm,
            Pressure {
                since_last_spoke: Some(Duration::from_secs(config.cooldown_seconds)),
                ..Pressure::default()
            },
        );
        assert_eq!(after, base);
        // 冷却关闭时没有这笔账。
        let no_cooldown = AmbientConfig {
            cooldown_seconds: 0,
            ..AmbientConfig::default()
        };
        assert_eq!(
            no_cooldown.threshold(
                calm,
                Pressure {
                    since_last_spoke: Some(Duration::ZERO),
                    ..Pressure::default()
                }
            ),
            base
        );

        // 每小时目标之内不加价，超出去之后一轮一份。
        let at_budget = Pressure {
            hourly_turns: config.max_per_hour,
            ..Pressure::default()
        };
        assert_eq!(config.threshold(calm, at_budget), base);
        let over_budget = Pressure {
            hourly_turns: config.max_per_hour + 1,
            ..Pressure::default()
        };
        assert_eq!(
            config.threshold(calm, over_budget),
            base + config.budget_penalty
        );
        // 抬价不是墙：第一轮超标时 100 分照样过得去，抬到 100 才真的没门。
        let far = Pressure {
            hourly_turns: config.max_per_hour + 40,
            ..Pressure::default()
        };
        assert_eq!(config.threshold(calm, far), 100);
        // 关掉这笔（写 0）或者关掉整条线（max_per_hour = 0）都一样。
        let no_budget = AmbientConfig {
            max_per_hour: 0,
            ..AmbientConfig::default()
        };
        assert_eq!(no_budget.threshold(calm, far), base);
        let free = AmbientConfig {
            budget_penalty: 0,
            ..AmbientConfig::default()
        };
        assert_eq!(free.threshold(calm, far), base);
    }

    #[test]
    fn the_summon_command_is_stripped_and_the_rest_of_the_message_stays() {
        let mut text = "/搭话".to_string();
        assert!(strip_summon(&mut text, "/搭话"));
        assert_eq!(text, "");

        // 指令后面跟着的才是群友真正说的话，它照常进窗口。
        let mut text = "/搭话 你怎么看这件事".to_string();
        assert!(strip_summon(&mut text, "/搭话"));
        assert_eq!(text, "你怎么看这件事");

        // 没加空格的连写、以及 @ 之后再说指令，都认。
        let mut text = "/搭话你怎么看".to_string();
        assert!(strip_summon(&mut text, "/搭话"));
        assert_eq!(text, "你怎么看");
        let mut text = "@我 /搭话 你说呢".to_string();
        assert!(strip_summon(&mut text, "/搭话"));
        assert_eq!(text, "@我 你说呢");

        // 嵌在词中间的不算指令；配置留空等于关掉这条通路。
        let mut text = "别/搭话了".to_string();
        assert!(!strip_summon(&mut text, "/搭话"));
        assert_eq!(text, "别/搭话了");
        let mut text = "/搭话".to_string();
        assert!(!strip_summon(&mut text, ""));
        assert_eq!(text, "/搭话");
        // 没带指令的那条消息当然原样。
        let mut text = "这游戏还更新吗".to_string();
        assert!(!strip_summon(&mut text, "/搭话"));
        assert_eq!(text, "这游戏还更新吗");
    }

    #[test]
    fn the_summon_command_is_on_by_default_and_only_the_named_thing_triggers_it() {
        let config = AmbientConfig::default();
        assert_eq!(config.summon_command, "/搭话");
        // 旧配置里没有这个键也读得出来。
        let legacy: AmbientConfig = toml::from_str("groups = [1]").unwrap();
        assert_eq!(legacy.summon_command, config.summon_command);
        // 想关掉就写空。
        let off: AmbientConfig = toml::from_str("summon_command = ''").unwrap();
        assert!(off.summon_command.is_empty());
    }
}
