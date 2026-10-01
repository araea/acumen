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
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use toml::Value;

mod gate;
mod mood;
mod peak;
mod quick;
mod quote;
mod recent;
mod reflect;
mod screenshot;
pub(crate) mod speak;
mod voice;

// 群聊能力层（看现场、查资料、动手）归内置智能体插件，搭话是它的一个人格外壳。
use crate::plugins::oai::chat::{
    ChatConfig, Persona, attention, identity, memory, now_context, pace, plain_text, stickers,
    tone, vision,
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
    pub groups: Vec<String>,
    /// 允许人格执行群管理的群；还须具备 QQ 对应权限。
    pub management_groups: Vec<String>,
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
    /// 从前给到 1.3（DeepSeek 官方「通用对话」那一档），想让它别一张嘴一个调子。
    /// 线上看下来那一档松过头了：一条里三个半截念头搅在一起、前一句认了后一句又
    /// 翻回去，群友直说「前言不搭后语」（2026-09-26）。口气的变化交给样本和人设，
    /// 温度回到接口默认的 1.0，先把话说连贯。判定模型不跟着动——它要的是分数稳。
    pub temperature: Option<f64>,
    /// 答疑那一轮的思考强度：判定认出有人认真求助、或者有人 @ 它问事的时候用。
    ///
    /// 平常接话用 `thinking`（low）就够，快、便宜；答疑要的是先想清楚再开口——
    /// 群友抱怨「乱回答」的几次都是没弄清问的是什么就答了。留空则跟 `thinking` 一样。
    pub help_thinking: String,
    /// 答疑那一轮换用的发言模型，写成 `供应商/模型`；留空沿用 `reply_model`。
    pub help_model: String,
    /// 每群每小时最多紧急突破几次；0 关闭突破。
    ///
    /// 判定认定「真要紧」（有人求救、被骗、设备要变砖、它自己说错的话正在误导人）
    /// 且分数够高时，冷却、十分钟密度、每小时目标这几笔加价全部不算。次数设上限，
    /// 免得有人天天喊「救命」把它刷成常态。
    pub breakthrough_per_hour: usize,
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
    /// 没人叫它时，隔多久「扫一眼群」（主动判定一次），实际间隔在它的 0.6–1.5 倍间
    /// 随机；正聊着（在关注、或三分钟内开过口）时缩到三分之一。被 @、引用、戳、
    /// 喊名字与搭话指令不等这一眼。这一眼之间的消息不会丢：下一眼连同前情一起看。
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
    /// 名片与平时称呼可能不同，平台不会告诉你那些小名，只能写在
    /// 这里。认出来只是在记录上加一个「叫了你的名字」的记号（见 [`identity`]），
    /// 不像 @ 那样直接把人格叫醒：猜错一次的代价是它冲着一句不相干的话接了嘴。
    pub aliases: Vec<String>,
    /// 平时介绍自己用的称呼；留空沿用平台昵称，非空时优先于群名片。
    pub preferred_name: String,
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
    /// 隔一阵子复盘一次群聊（见 [`reflect`]）：整理几摊事、对人的新印象、新梗与自己说过的话，
    /// 写进群记忆。一次便宜的调用，计价高峰不做。需要 `memory_enabled`。
    pub reflect_enabled: bool,
    /// 每群每小时最多随口吭几声（见 [`quick`]）；0 关闭。
    ///
    /// 判定打分落在 `quick_floor` 与开口门槛之间时，按几率不调用发言模型、只随口回一两个字。
    pub quick_per_hour: usize,
    /// 随口一句的分数下限：判定低于它说明连吭一声的兴致都没有。
    pub quick_floor: u8,
    /// 号主本人在这个群亲手打字之后，多久之内机器人不主动接话（秒）；0 关闭。
    ///
    /// 同一个账号，他在线时机器人不该跟他抢话——群友已经为「同一个号两种口气」开过
    /// 「被夺舍了」的玩笑（2026-10-01）。窗口内只剩被点名与搭话指令能叫醒它。
    pub owner_quiet_seconds: u64,
    /// 号主刚在群里说过话、这时有人点名（@、引用、喊名字）：先等他自己答多久（秒）；
    /// 0 不等。他在这段时间里开口了，这条点名就归他，机器人不再重复答一遍。
    pub owner_grace_seconds: u64,
    /// 消息时效窗口（秒）：请求交给 satori-qq 之后，群里只要又有人说话就不再发
    /// 出这一句。0 关闭。见 [`crate::adapters::satori::Freshness`]。
    pub send_freshness_seconds: u64,
    /// 发送前短暂显示 QQ 原生“正在输入”（内核实验性接口，默认关闭）。
    pub qq_typing: bool,
    /// 回话前在 QQ 原生内核中标记本群已读（实验性；默认关闭）。
    pub qq_mark_read: bool,
    /// 群友直接质疑是不是机器人时，偶尔将本群最近 1-3 条消息渲染成图打趣回复。
    /// 仅 satori-qq；图片不是身份凭据，默认关闭，避免自动转发群聊内容。
    pub screenshot_on_suspicion: bool,
    /// 同一群两次截图至少间隔多久（秒）；实际下限 1 小时。
    pub screenshot_cooldown_seconds: u64,
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
            temperature: Some(1.0),
            help_thinking: "high".to_string(),
            help_model: String::new(),
            breakthrough_per_hour: 3,
            tools: "read,write,bash".to_string(),
            score_threshold: 60,
            silence_relief_per_10min: 0,
            silence_relief_cap: 0,
            speech_penalty_per_turn: 8,
            speech_penalty_cap: 24,
            focus_relief: 5,
            context_turns: 20,
            context_images: 2,
            debounce_seconds: 6,
            max_pending_seconds: 40,
            gate_interval_seconds: 90,
            cooldown_seconds: 150,
            cooldown_penalty: 18,
            focus_max_seconds: 180,
            max_per_hour: 5,
            budget_penalty: 12,
            reply_on_mention: true,
            aliases: Vec::new(),
            preferred_name: String::new(),
            summon_command: "/搭话".to_string(),
            memory_enabled: true,
            mood_enabled: true,
            memo_budget: 3,
            search_enabled: true,
            search_budget: 3,
            peak: peak::PeakConfig::default(),
            reflect_enabled: true,
            quick_per_hour: 4,
            quick_floor: 38,
            owner_quiet_seconds: 480,
            owner_grace_seconds: 75,
            send_freshness_seconds: 25,
            qq_typing: false,
            qq_mark_read: false,
            screenshot_on_suspicion: false,
            screenshot_cooldown_seconds: 21_600,
            messages_budget: 2,
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
        Duration::from_secs(
            self.max_pending_seconds
                .clamp(self.debounce().as_secs(), 600),
        )
    }

    fn gate_interval(&self) -> Duration {
        Duration::from_secs(self.gate_interval_seconds.min(3_600))
    }

    fn cooldown(&self) -> Duration {
        Duration::from_secs(self.cooldown_seconds)
    }

    fn owner_quiet(&self) -> Duration {
        Duration::from_secs(self.owner_quiet_seconds.min(3_600))
    }

    fn owner_grace(&self) -> Duration {
        Duration::from_secs(self.owner_grace_seconds.min(600))
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

    /// 答疑那一轮的配置：想得深一档，需要的话换一个模型。
    fn careful(&self) -> Self {
        Self {
            thinking: if self.help_thinking.trim().is_empty() {
                self.thinking.clone()
            } else {
                self.help_thinking.clone()
            },
            reply_model: if self.help_model.trim().is_empty() {
                self.reply_model.clone()
            } else {
                self.help_model.clone()
            },
            ..self.clone()
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
    fn scene(&self, group: &str, turns: &[Turn], _rhythm: &str) -> serde_json::Value {
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

    fn spoke(&self, group: &str) {
        if self.config.mood_enabled {
            mood::nudge(|mood, now| mood.spoke(group, now));
        }
    }

    fn pace(&self, group: &str) -> pace::Pace {
        self.config.pace(mood::snapshot(group))
    }

    fn avatar(&self) -> Option<crate::plugins::oai::chat::Avatar> {
        self.avatar.clone()
    }

    fn keeps_quote(&self, target: &str, turns: &[Turn]) -> bool {
        quote::keeps(target, turns, rand::random::<f32>())
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
    /// 这个群是干什么的：管理员写在 `groups/<群号>.md` 里的背景。
    ///
    /// 同一个词在不同群里是两回事：②群里「投不了屏」说的是电脑管家的多屏协同，
    /// 人格当成了投电视，被群友说「乱回答」（2026-09-26 12:12）。判定与发言都带着它。
    pub group_about: String,
    /// 这一轮是在答疑：有人认真问事。人格收到一段「先弄清再答」的交代。
    pub careful: bool,
    /// 判定那一眼注意到的是什么（它给的那句理由）；没经过判定时为空。
    ///
    /// 群里几摊话同时在聊时，人格从头读一遍记录，常常挑中另一摊、甚至把几摊搅成
    /// 一句。把「刚才是哪件事让你想开口」递过去，它接的就是那一件。
    pub noticed: String,
}

impl Scene {
    pub(crate) fn build(
        group: &str,
        config: &AmbientConfig,
        turns: &[Turn],
        rhythm: String,
    ) -> Self {
        // 状态算一次用两处：一句给模型看的「你现在的状态」，以及挑样本的调子。
        let snapshot = config.mood_enabled.then(|| mood::snapshot(group));
        Self {
            identity: {
                let mut brief = identity::brief(group);
                let name = config.preferred_name.trim();
                if !name.is_empty() {
                    brief.push_str(&format!(
                        "你平时叫「{name}」，介绍自己时用这个称呼；上面的群名片只是平台展示文字。\n"
                    ));
                }
                brief
            },
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
                "{}{}{}{}",
                self_facts(),
                if config.memory_enabled {
                    memory::with_group(group, |memory| {
                        memory.claims_brief(chrono::Local::now().timestamp())
                    })
                } else {
                    String::new()
                },
                voice::brief(turns, voice_register(config, group)),
                stickers::brief(turns, config.sticker_max)
            ),
            noticed: String::new(),
            group_about: group_about(group),
            careful: false,
        }
    }

    /// 现场 → 注入提示词的一段话。
    pub(crate) fn brief(&self) -> String {
        let mut out = format!(
            "{}\n{}{}{}\n",
            now_context(),
            self.identity,
            self.group_about,
            self.register
        );
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
fn voice_register(config: &AmbientConfig, group: &str) -> mood::Register {
    if config.mood_enabled {
        mood::snapshot(group).register()
    } else {
        mood::Register::Even
    }
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

/// 这个群的背景资料 → 提示词里那一段；没写就是空串。
fn group_about(group: &str) -> String {
    let Some(dir) = DATA_DIR.get() else {
        return String::new();
    };
    let Ok(raw) = std::fs::read_to_string(groups_dir(dir).join(format!("{group}.md"))) else {
        return String::new();
    };
    let lines: Vec<String> = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| format!("- {}", line.trim_start_matches("- ")))
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "这个群（背景，群里人默认都知道；问题先放在这个语境里理解）：\n{}\n",
        lines.join("\n")
    )
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
        "关于你自己（背景资料：有人问到你、或者正好聊到你自己时照这个说，前后才对得上；\
         平常接话用不着把它们往外掏，没人问你用什么手机、站哪家）：\n{}\n",
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

/// 各群背景资料的目录：`groups/<群号>.md`，管理员写，每轮现读。
fn groups_dir(base: &Path) -> PathBuf {
    base.join("groups")
}

fn skills_root(base: &Path) -> PathBuf {
    base.join("skills")
}

/// 这一轮随身的 skill 目录清单。
pub(crate) fn skill_dirs(base: &Path) -> Vec<PathBuf> {
    let root = skills_root(base);
    SKILLS.iter().map(|(name, _)| root.join(name)).collect()
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
    tokio::fs::create_dir_all(groups_dir(base)).await?;
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
    let Some(group) = event
        .group_id()
        .filter(|id| config.groups.iter().any(|group| group == id))
        .map(str::to_owned)
    else {
        return;
    };
    let group = group.as_str();

    // 号主最近亲手打的话：隔几个钟头从聊天记录里重捞一次，给发言当最新的口吻标尺。
    recent::ensure_fresh(ctx);
    let mut turn = window::turn_from(&event, &ctx.bot.self_id());
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
        let (id, name, at) = (turn.user_id.clone(), turn.name.clone(), turn.at);
        memory::edit(group, |memory| memory.see(&id, &name, at));
    }
    if config.mood_enabled && turn.mentions_me && !turn.from_me {
        mood::nudge(|mood, now| mood.engaged(group, now));
    }
    if !turn.from_me && !empty {
        reflect::note(group);
    }
    let start = window::with_group(group, |state| {
        record_for_consideration(state, turn, empty, summoned)
    });
    if !start {
        return;
    }

    let ctx = ctx.clone();
    let writer = writer.clone();
    let mgr = mgr.clone();
    let base = base.to_path_buf();
    let group = group.to_string();
    tokio::spawn(async move {
        if let Err(error) = consider(&ctx, &writer, &mgr, &group, &base).await {
            warn!(target: LOG_TARGET, "群 {group} 搭话失败：{error:#}");
        }
    });
}

/// 收消息并登记指令。带正文的指令不能因 receive 已启动 worker 而漏掉召唤标记。
fn record_for_consideration(
    state: &mut window::GroupState,
    turn: Turn,
    empty: bool,
    summoned: bool,
) -> bool {
    let pushed = !empty && state.receive(turn);
    // 不能短路：有 worker 在跑时也得留标记给下一批。
    let called = summoned && state.summon();
    pushed || called
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
        .get_str("group_id")
        .filter(|id| config.groups.iter().any(|group| group == id))
        .map(str::to_owned)
    else {
        return;
    };
    let group = group.as_str();
    let Some((turn, recalled)) = notice_turn(raw, &ctx.bot.self_id()) else {
        return;
    };
    if config.mood_enabled && turn.mentions_me {
        mood::nudge(|mood, now| mood.engaged(group, now));
    }
    let start = window::with_group(group, |state| {
        if let Some(id) = &recalled {
            state.recall(id);
        }
        // 平台变化使已准备的动作过时，但不单独唤醒人格。
        if turn.from_me && turn.user_id.is_empty() {
            state.seq += 1;
        }
        state.receive(turn)
    });
    if start {
        let (ctx, writer, mgr) = (ctx.clone(), writer.clone(), mgr.clone());
        let base = base.to_path_buf();
        let group = group.to_string();
        tokio::spawn(async move {
            if let Err(error) = consider(&ctx, &writer, &mgr, &group, &base).await {
                warn!(target: LOG_TARGET, "群 {group} 互动处理失败：{error:#}");
            }
        });
    }
}

fn notice_turn(raw: &simd_json::OwnedValue, me: &str) -> Option<(Turn, Option<String>)> {
    let user = raw.get_str("user_id").unwrap_or("");
    let mid = raw.get_str("message_id").unwrap_or("");
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
                .map(str::to_owned)
                .or_else(|| data.get_i64("target_id").map(|id| id.to_string()))
                .unwrap_or_default();
            mentions_me = target == me && user != me;
            call.poked_me = mentions_me;
            format!("[戳一戳：{user} 戳了 {target}]")
        }
        "message-deleted" => {
            recalled = Some(mid.to_string());
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
            raw.get_str("operator_id").unwrap_or("")
        ),
        "guild-member-updated" => {
            // 管理动作的事件只更新现场，避免自己管理→自己评论的循环。
            from_me = true;
            if raw.get_str("notice_type") == Some("group_ban") {
                let target = if user.is_empty() {
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
            user_id: if from_me && user != me {
                String::new()
            } else {
                user.to_string()
            },
            name: if from_me && user != me {
                "平台事件".into()
            } else {
                user.to_string()
            },
            text,
            images: vec![],
            elements: Message::new(),
            message_id: String::new(),
            mentions_me,
            call,
            from_me,
            manual: false,
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
    group: String,
    armed: bool,
}

impl Drop for Worker {
    fn drop(&mut self) {
        if self.armed {
            window::with_group(&self.group, |state| state.running = false);
        }
    }
}

/// 等到该看群的时候。
///
/// 没人叫它时，这一阵消息要等到下一眼才被看到（见 [`window::GroupState::look_due`]）；
/// 等的这段时间里新来的消息照样进窗口，下一眼一起看，一条都不漏。有人叫它就不等
/// 下一眼了——但没在聊的时候，从手机亮起到真的点开也要几秒到十几秒，秒回是机器。
async fn wait_for_look(ctx: &Context, group: &str, config: &AmbientConfig) {
    let interval = config.gate_interval();
    loop {
        let (urgent, engaged, due) = window::with_group(group, |state| {
            (state.urgent(), state.engaged(), state.look_due(interval))
        });
        if urgent {
            // 看群越勤，被叫到也看得越快；间隔调成 0 就是随叫随到。
            if !engaged {
                tokio::time::sleep(notice_delay().min(interval / 5)).await;
            }
            wait_for_owner(ctx, group, config).await;
            return;
        }
        if due.is_zero() {
            return;
        }
        let latest = crate::plugins::get_config_or_default::<AmbientConfig>(ctx, "ambient");
        if !latest.enabled || !latest.groups.iter().any(|id| id == group) {
            return;
        }
        tokio::time::sleep(due.min(Duration::from_secs(2))).await;
    }
}

/// 号主刚在群里亲手说过话时，有人点名：先让他自己答。
///
/// 账号是他的，点名多半是冲着他本人来的。他还在线（`owner_quiet_seconds` 之内打过字）
/// 就等一会儿——他开口了，窗口那一侧会把这条点名记成已处理（见
/// [`window::GroupState::receive`]），机器人这一批什么都不用做；等满了他还没动静，
/// 才由机器人接。搭话指令是人按下的键，不等。
async fn wait_for_owner(ctx: &Context, group: &str, config: &AmbientConfig) {
    let grace = config.owner_grace();
    let (present, summoned, baseline) = window::with_group(group, |state| {
        (
            state.owner_active(config.owner_quiet()),
            state.has_unread_summon(),
            state.owner_marks(),
        )
    });
    if grace.is_zero() || !present || summoned {
        return;
    }
    info!(target: LOG_TARGET, "群 {group} 有人叫，但本人刚在群里说过话，先等他自己答（最多 {} 秒）", grace.as_secs());
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let latest = crate::plugins::get_config_or_default::<AmbientConfig>(ctx, "ambient");
        if !latest.enabled || !latest.groups.iter().any(|id| id == group) {
            return;
        }
        if window::with_group(group, |state| state.owner_marks() != baseline) {
            info!(target: LOG_TARGET, "群 {group} 本人自己答了，这条点名不用再接");
            return;
        }
    }
}

/// 被叫到之后多久才真的看到：多数几秒，偶尔十几秒。
fn notice_delay() -> Duration {
    let roll = rand::random::<f32>();
    Duration::from_secs_f32(3.0 + roll * roll * 14.0)
}

/// 进程起来之后第一次看这个群时，先把最近的聊天记录翻回来。
///
/// 窗口只在内存里，重启、崩溃、每天凌晨的例行重启都会把它清空；清空之后的第一轮
/// 人格看不到自己刚说过的话，接出来的就是前言不搭后语。平台（satori-qq）自己存着
/// 群消息，`message.list` 拿得到——翻两三页、只留三小时以内的，垫到窗口前面。
/// 翻不到就算了：这是补救，不是前提，失败不重试，免得每一批都去敲一次平台。
async fn hydrate(ctx: &Context, writer: &LockedWriter, group: &str) {
    if window::with_group(group, |state| std::mem::replace(&mut state.hydrated, true)) {
        return;
    }
    const PAGES: usize = 3;
    const KEEP_SECONDS: i64 = 3 * 3_600;
    let mut items: Vec<serde_json::Value> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..PAGES {
        let mut body = serde_json::json!({"channel_id": group.to_string()});
        if let Some(next) = &cursor {
            body["next"] = serde_json::json!(next);
        }
        let listed: Result<serde_json::Value, _> = tokio::time::timeout(
            Duration::from_secs(8),
            writer.call(ctx, "message.list", body),
        )
        .await
        .unwrap_or_else(|_| Err("超时".into()));
        let Ok(listed) = listed else {
            break;
        };
        items.extend(listed["data"].as_array().cloned().unwrap_or_default());
        match listed["next"].as_str() {
            Some(next) if !next.is_empty() => cursor = Some(next.to_string()),
            _ => break,
        }
    }
    let now = chrono::Local::now().timestamp();
    // 平台翻回来的记录分不出「号主亲手打的」和「机器人发的」，本机的聊天记录库分得出
    // （机器人发出去的那份记作 self）：按消息号对一遍，免得重启后把号主说的话算成自己说的。
    let typed = manual_ids(ctx, group, now - KEEP_SECONDS).await;
    let history: Vec<Turn> = items
        .iter()
        .filter_map(|item| window::turn_from_platform(ctx, writer, item))
        .filter(|turn| now - turn.at < KEEP_SECONDS)
        .map(|mut turn| {
            turn.manual = turn.from_me && typed.contains(&turn.message_id);
            turn
        })
        .collect();
    if history.is_empty() {
        debug!(target: LOG_TARGET, "群 {group} 没翻到可用的聊天记录");
        return;
    }
    let (added, mine) = window::with_group(group, |state| state.seed(history));
    info!(target: LOG_TARGET, "群 {group} 翻了翻聊天记录：补回 {added} 条，其中自己说的 {mine} 句");
}

/// 本群最近号主亲手打的消息号（同一个号里不是机器人发的那些）。查不到就是空集，不影响别的。
async fn manual_ids(ctx: &Context, group: &str, since: i64) -> HashSet<String> {
    use sea_orm::{ConnectionTrait, Statement};
    let me = ctx.bot.self_id();
    if me.is_empty() {
        return HashSet::new();
    }
    let rows = ctx
        .db
        .query_all_raw(Statement::from_sql_and_values(
            ctx.db.get_database_backend(),
            "select message_id from message_records \
             where guild_id = ? and user_id = ? and member_role != 'self' and time >= ?",
            [group.into(), me.into(), since.into()],
        ))
        .await
        .unwrap_or_default();
    rows.iter()
        .filter_map(|row| row.try_get::<String>("", "message_id").ok())
        .collect()
}

async fn consider(
    ctx: &Context,
    writer: &LockedWriter,
    mgr: &Arc<crate::plugins::oai::data::Manager>,
    group: &str,
    base: &Path,
) -> anyhow::Result<()> {
    let mut worker = Worker {
        group: group.to_string(),
        armed: true,
    };
    hydrate(ctx, writer, group).await;
    loop {
        let config = crate::plugins::get_config_or_default::<AmbientConfig>(ctx, "ambient");
        if config.focus_max_seconds == 0 {
            window::with_group(group, |state| state.focus = None);
        }
        if !config.enabled || !config.groups.iter().any(|id| id == group) {
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
        wait_for_look(ctx, group, &config).await;
        let config = crate::plugins::get_config_or_default::<AmbientConfig>(ctx, "ambient");
        if !config.enabled || !config.groups.iter().any(|id| id == group) {
            return Ok(());
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
            ctx, writer, mgr, group, base, &config, &mut seq, &turns, mentioned, summoned,
            silent_for, &rhythm, focused,
        )
        .await
        {
            warn!(target: LOG_TARGET, "群 {group} 搭话失败：{error:#}");
        }
        // 记性和状态每批都落盘：绝大多数批次以沉默收场，只在开口时保存等于几乎不保存。
        memory::flush(group).await;
        mood::flush().await;
        review_if_due(ctx, mgr, group, &config);
        if !window::with_group(group, |state| state.finish_batch(seq)) {
            worker.armed = false;
            return Ok(());
        }
    }
}

/// 到点了就在后台复盘一次群聊（见 [`reflect`]）。不阻塞这一群的 worker。
///
/// 计价高峰不做：这件事不急，等到平价时段再整理也一样；高峰换了替补模型的话，
/// 就用替补，跟判定走同一条路。
fn review_if_due(
    ctx: &Context,
    mgr: &Arc<crate::plugins::oai::data::Manager>,
    group: &str,
    config: &AmbientConfig,
) {
    if !config.reflect_enabled || !config.memory_enabled {
        return;
    }
    let config = match config.peak_stance() {
        peak::Stance::Asleep | peak::Stance::Dozing => return,
        peak::Stance::Swapped => config.swapped(),
        peak::Stance::Awake => config.clone(),
    };
    if !reflect::claim_due(group) {
        return;
    }
    let (ctx, mgr, group) = (ctx.clone(), mgr.clone(), group.to_string());
    tokio::spawn(async move {
        match reflect::run(&ctx, &mgr, &group, &config).await {
            Ok(summary) => {
                info!(target: LOG_TARGET, "群 {group} 复盘：{summary}");
                reflect::finish(&group, true);
            }
            Err(error) => {
                debug!(target: LOG_TARGET, "群 {group} 复盘没成：{error:#}");
                reflect::finish(&group, false);
            }
        }
    });
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
    group: &str,
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
        peak::Stance::Dozing
            if mentioned || summoned || window::with_group(group, |state| state.pressing()) =>
        {
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

    // An opt-in visual one-liner for a direct bot accusation. The renderer only reads QQ's
    // same-group local history, and an unsuccessful render/send falls back to normal speech.
    if screenshot::try_reply(ctx, writer, group, config, *seq, turns).await {
        if config.mood_enabled {
            mood::nudge(|mood, now| mood.spoke(group, now));
        }
        return Ok(());
    }

    // 这一眼看到了哪些：上一眼之后新来的几条，判定要把它们和前情分开看。
    let (fresh, glance) = window::with_group(group, |state| {
        let fresh = state.unseen();
        state.mark_look();
        (fresh, state.recent((fresh + 6).clamp(12, 36)))
    });

    // 号主本人在线：机器人不主动插话。这一眼算看过了（上面已经 mark_look），他看过的
    // 话题不会等他走了再被机器人翻出来接。被点名与搭话指令不在此列。
    if !mentioned
        && !summoned
        && window::with_group(group, |state| state.owner_active(config.owner_quiet()))
    {
        debug!(target: LOG_TARGET, "群 {group} 本人刚在群里说过话，这一眼不插话");
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
    identity::refresh(
        ctx,
        writer,
        avatar_endpoint(ctx, mgr, config).await.as_ref(),
        group,
    )
    .await;
    if !mentioned && !summoned && !current(ctx, group, *seq) {
        return Ok(());
    }
    let scene = Scene::build(group, config, turns, rhythm.to_string());
    let mut noticed = String::new();
    let mut careful = false;
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
        debug!(target: LOG_TARGET, "群 {group} 判定模型：{}", config.gate_model);
        let verdict = gate::judge(
            &api_base,
            &api_key,
            &gate_model,
            config,
            &glance,
            fresh,
            &persona,
            &scene,
            None,
        )
        .await?;
        let ordinary = verdict.wants_composition(threshold, config.focus_relief, focused)
            || verdict.wants_to_help(config.score_threshold);
        // 紧急突破：只在按平常的账过不去时才动用，免得白占一次名额。
        let breakthrough = !ordinary
            && verdict.breaks_through()
            && window::with_group(group, |state| {
                state.allow_breakthrough(config.breakthrough_per_hour)
            });
        if !ordinary && !breakthrough {
            debug!(target: LOG_TARGET, "群 {group} 保持沉默（{}/{}，{}）", verdict.score, threshold, verdict.reason);
            quick_word(
                ctx,
                writer,
                group,
                config,
                (&api_base, &api_key, &gate_model),
                &persona,
                &verdict,
                threshold,
                silent_for,
                doze,
                seq,
            )
            .await;
            return Ok(());
        }
        if breakthrough {
            warn!(target: LOG_TARGET, "群 {group} 紧急突破（{}/{}）：{}", verdict.score, threshold, verdict.reason);
        }
        careful = verdict.help || verdict.urgent;
        info!(target: LOG_TARGET, "群 {group} 交给人格决定（{}/{}，续聊={}，求助={}，{}）",
            verdict.score, threshold, verdict.continuation, verdict.help, verdict.reason);
        noticed = verdict.reason.clone();
        if !current(ctx, group, *seq) {
            return Ok(());
        }
    } else {
        info!(target: LOG_TARGET, "群 {group} 被点名，由人格决定是否回应");
        // 被 @ 来问事的，同样按答疑来：想清楚、拿不准就查。
        careful = turns
            .iter()
            .rev()
            .filter(|turn| !turn.from_me && turn.call.mine())
            .take(1)
            .any(|turn| asks_for_answer(&turn.text));
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
    let mut scene = Scene::build(group, config, &latest, rhythm);
    scene.noticed = noticed;
    scene.careful = careful;
    let adjusted;
    let config = if careful {
        info!(target: LOG_TARGET, "群 {group} 这一轮按答疑来（思考 {}）", config.careful().thinking);
        adjusted = config.careful();
        &adjusted
    } else {
        config
    };
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

/// 判定没过线、但落在「想吭一声」区间里时：按几率随口回一两个字（见 [`quick`]）。
///
/// 不调用发言模型、不碰工具，失败一律当没发生——这一步是锦上添花，不该让任何一次
/// 出错变成日志里的警告。
#[allow(clippy::too_many_arguments)]
async fn quick_word(
    ctx: &Context,
    writer: &LockedWriter,
    group: &str,
    config: &AmbientConfig,
    (api_base, api_key, model): (&str, &str, &str),
    persona: &str,
    verdict: &gate::Verdict,
    threshold: u8,
    silent_for: Option<Duration>,
    doze: bool,
    seq: &mut u64,
) {
    let now = chrono::Local::now().timestamp();
    let facts = window::with_group(group, |state| quick::Facts {
        score: verdict.score,
        threshold,
        newest: state.newest(now),
        since_last_spoke: silent_for,
        quick_last_hour: state.quick_last_hour(),
        owner_present: state.owner_active(config.owner_quiet()),
        dozing: doze,
    });
    if !quick::wants(config, facts, rand::random::<f32>()) || !current(ctx, group, *seq) {
        return;
    }
    let turns = window::with_group(group, |state| {
        state.recent(config.context_turns.clamp(1, 80))
    });
    let voice = voice::short_lines(&turns, voice_register(config, group), 5);
    let started = Instant::now();
    let text = match quick::react(
        api_base,
        api_key,
        model,
        config,
        persona,
        &turns,
        &verdict.reason,
        &voice,
    )
    .await
    {
        Ok(Some(text)) if !quick::repeats(&text, &turns) => text,
        Ok(_) => return,
        Err(error) => {
            debug!(target: LOG_TARGET, "群 {group} 随口一句没成：{error:#}");
            return;
        }
    };
    // 问模型的这几秒里群里又动了：这一声就不吭了。
    if !current(ctx, group, *seq) {
        return;
    }
    let pace::Speech::Say(utterances) = pace::parse(&text, 1, 0) else {
        return;
    };
    info!(target: LOG_TARGET, "群 {group} 随口一句（{}/{}）：{}", verdict.score, threshold, verdict.reason);
    window::with_group(group, |state| state.mark_quick());
    if let Err(error) = deliver(ctx, writer, group, config, &turns, utterances, seq, false, started).await {
        debug!(target: LOG_TARGET, "群 {group} 随口一句没发出去：{error:#}");
    }
}

/// 一句 @ 它的话是不是在问事（而不是逗它）。刻意收窄：「你是不是人机」不算。
fn asks_for_answer(text: &str) -> bool {
    [
        "怎么", "为什么", "为啥", "咋办", "咋弄", "咋整", "能不能", "可不可以", "如何",
        "多少钱", "哪个好", "值不值", "报错", "教程", "什么意思", "是什么", "区别",
    ]
    .iter()
    .any(|word| text.contains(word))
}

/// 停用配置或群聊推进后，放弃尚未发送的内容，交回 worker 读取新上下文。
fn current(ctx: &Context, group: &str, seq: u64) -> bool {
    let config = crate::plugins::get_config_or_default::<AmbientConfig>(ctx, "ambient");
    config.enabled
        && config.groups.iter().any(|id| id == group)
        && window::with_group(group, |state| state.seq == seq)
}

/// 打完字那一刻，这句话还发不发。
enum Sendable {
    /// 群里没动静，照发。
    Fresh,
    /// 来了一两句无关的，照发并挂上引用。
    Drifted,
    /// 话题已经往前走了、或者有人点了它的名：这句作废，交回 worker 重看。
    Stale,
}

/// 打字期间最多容忍几条新消息。群里正热闹时每几秒一条，一句话还没敲完就作废
/// 的话，人格在活跃的群里几乎开不了口；多于这个数，现场多半已经变了。
const DRIFT_SLACK: u64 = 2;

fn sendable(ctx: &Context, group: &str, seq: u64) -> Sendable {
    let config = crate::plugins::get_config_or_default::<AmbientConfig>(ctx, "ambient");
    if !config.enabled || !config.groups.iter().any(|id| id == group) {
        return Sendable::Stale;
    }
    window::with_group(group, |state| match state.drift(seq) {
        0 => Sendable::Fresh,
        n if n <= DRIFT_SLACK && !state.has_unread_mention() => Sendable::Drifted,
        _ => Sendable::Stale,
    })
}

/// 兼容文字路径该引用哪条消息。
///
/// 优先「叫到我的那条」——@、引用我、戳我，那才是这句回应真正对着的话；没人叫的
/// 时候才落到本批最后一条群友消息。消息号为 0 的平台事件（戳一戳、撤回）引不了。
fn reply_target(turns: &[Turn]) -> Option<String> {
    let candidates: Vec<&Turn> = turns
        .iter()
        .rev()
        .filter(|turn| !turn.from_me && !turn.message_id.is_empty())
        .collect();
    candidates
        .iter()
        .find(|turn| turn.mentions_me)
        .or_else(|| candidates.first())
        .map(|turn| turn.message_id.clone())
}

/// 这条消息实际引谁。
///
/// 模型点名的那条优先——但得真在本批记录里，否则它随口写的一个消息号会让整条消息
/// 引到不存在的目标上；点不出或没点名，才用 [`reply_target`] 的默认目标。
fn quote_target(
    explicit: Option<String>,
    fallback: Option<String>,
    turns: &[Turn],
) -> Option<String> {
    explicit
        .filter(|id| turns.iter().any(|turn| turn.message_id == *id))
        .or(fallback)
}

/// 让人格模型写，然后按人的节奏发出去。
#[allow(clippy::too_many_arguments)]
async fn speak_up(
    ctx: &Context,
    writer: &LockedWriter,
    mgr: &Arc<crate::plugins::oai::data::Manager>,
    group: &str,
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
    debug!(target: LOG_TARGET, "群 {group} 发言模型：{}", config.reply_model);
    let composed = speak::compose(
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

    let acted = composed.acted;
    let (raw, focus) = attention::extract(&composed, turns, config.focus_max_seconds);
    // 接口把拒绝句当回复递回来：那不是它想说的话，当没说。
    if crate::plugins::oai::chat::protocol::is_provider_noise(&raw) {
        warn!(target: LOG_TARGET, "群 {group} 模型返回的是接口的拒绝句，当作没说：{}", raw.trim());
        return Ok(());
    }
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
            0,
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
    let utterances =
        match pace::parse(&raw, config.messages_budget.clamp(1, 5), config.split_chars) {
            pace::Speech::Silent => {
                if acted {
                    debug!(target: LOG_TARGET, "群 {group} 这一轮用动作工具说过了");
                } else {
                    info!(target: LOG_TARGET, "群 {group} 想了想，还是没说话");
                }
                return Ok(());
            }
            pace::Speech::Say(items) => items,
        };
    deliver(
        ctx, writer, group, config, turns, utterances, seq, doze, started,
    )
    .await?;
    Ok(())
}

/// 按人的节奏把一批话发出去：先想一会儿，再一条条打字、检查现场还新不新、发送并记账。
///
/// 兼容文字路径的发言与「随口一句」都走这里。返回有没有真的发出过一条。
#[allow(clippy::too_many_arguments)]
async fn deliver(
    ctx: &Context,
    writer: &LockedWriter,
    group: &str,
    config: &AmbientConfig,
    turns: &[Turn],
    mut utterances: Vec<pace::Utterance>,
    seq: &mut u64,
    doze: bool,
    started: Instant,
) -> anyhow::Result<bool> {
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
        return Ok(false);
    }

    // 兼容文字路径要引谁：模型用 `[reply:消息号]` 点名了就引那条（须在本批记录里，
    // 免得它凭记忆写一个引不到或引错的消息号）；没点名才退回本批默认目标——优先
    // 「叫到我的那条」，没人叫就引最新那条群友消息。两张图片消息挨着来时，默认目标
    // 只会是后一张，模型讲的是前一张就露馅了，所以点名这一路要留给它。
    let fallback = reply_target(turns);
    let pace = config.pace(mood::snapshot(group));
    tokio::time::sleep(pace.think_delay(started.elapsed())).await;

    let me = ctx.bot.self_id();
    let mut sent = false;
    for (index, utterance) in utterances.into_iter().enumerate() {
        if index > 0 {
            tokio::time::sleep(pace.gap()).await;
        }
        if utterance.wait > 0.0 {
            tokio::time::sleep(Duration::from_secs_f32(utterance.wait)).await;
        }
        // 模型那几秒是在读和想，不是在打字：字还得一个个敲出来。从前首条把模型耗时
        // 从打字时间里扣掉，四十个字的答疑八秒就到，群友一眼看出「不到 5 秒回消息」。
        tokio::time::sleep(pace.typing_delay(utterance.chars)).await;
        let drifted = match sendable(ctx, group, *seq) {
            Sendable::Stale => break,
            Sendable::Fresh => false,
            Sendable::Drifted => true,
        };

        let mut message = Message::new();
        // 打字的工夫里群里又冒出一两句：照样发，但首条挂上它回的那句，免得接错人。
        if (utterance.reply || (drifted && index == 0))
            && let Some(id) = quote_target(utterance.reply_to, fallback.clone(), turns)
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
            Ok(Some(id)) => id,
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
                user_id: me.clone(),
                name: "我".to_string(),
                text: spoken,
                elements: message.clone(),
                images: Vec::new(),
                message_id: id,
                mentions_me: false,
                call: window::Call::default(),
                from_me: true,
                manual: false,
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
            let at = chrono::Local::now().timestamp();
            memory::edit(group, |memory| memory.exchange(&target.user_id, at));
        }
    }
    Ok(sent)
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
