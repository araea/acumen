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
use crate::event::Context;
use crate::message::Message;
use crate::plugins::{PluginConfig, PluginError, get_data_dir};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsScalar};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

mod gate;
mod habit;
mod learning;
mod life;
mod mood;
mod news;
mod peak;
mod quick;
mod quote;
mod recall;
mod recent;
mod reflect;
mod screenshot;
pub(crate) mod speak;
mod stray;
mod voice;

// 群聊能力层（看现场、查资料、动手）归内置智能体插件，搭话是它的一个人格外壳。
use crate::plugins::oai::chat::{
    ChatConfig, Persona, attention, identity, long_text, memory, now_context, pace, plain_text,
    stickers, tone, vision,
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
/// 群规的模板：首次启动写进数据目录（`rules.md`），之后以磁盘上那份为准。
const RULES: &str = include_str!("../../res/ambient/rules.md");
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
你在群里是个待久了的普通群友：话不算多，爱看热闹，遇到有共鸣的、好笑的、自己懂的才接一句；
不端着，也不装高冷。群里一天几百条，你大概只回其中几条。

你会想接的：有人抛梗、吐槽、发好笑的图；聊到你懂的数码、游戏、日常琐事；有人问了个你
答得上、还没人答的问题；有人叫你、@ 你、回你刚说的话；有人说的话明显不对，值得拆一句。

你懒得接的：广告引流、复读刷屏、别人吵得正凶、已经聊完的事、纯表情包对刷、黏糊糊的
情感话题；以及只是让你表演机灵的无聊挑衅。

沉默是常态，不是憋着。判断「他会不会想接这句话」就行，怎么写不用你操心。";

/// 群管辅助：把搭话从「想接就接」扩到「有事的时候搭把手」。
///
/// 两件事都只改「什么时候看一眼」，不改判定的标准：看是本地读窗口，不花钱；
/// 要不要开口仍然由判定那一步说。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Assist {
    /// 有人提问时提前看一眼：问题带着「怎么、为什么、能不能、报错」这些字眼，
    /// 或者以问号收尾。只是把下一眼提前，答不答仍由判定决定。
    pub questions: bool,
    /// 新人入群时交给人格打个招呼（跳过判定）。
    ///
    /// 与入群欢迎插件二选一：那个是每来一个人就发、话是写好的；这一句是人格看着
    /// 群里的现场说的，能接上前一句、能回答新人问的事，但只在这个批次的窗口里
    /// 有新人时才发生。不想两套欢迎同时响，就只开一个。
    pub newcomers: bool,
    /// 进群后多少秒内还能补一句招呼；超过这个时间就不追着补了（默认 180）。
    pub newcomer_within_seconds: u64,
}

impl Default for Assist {
    fn default() -> Self {
        Self { questions: true, newcomers: false, newcomer_within_seconds: 180 }
    }
}

/// 从号主亲手发的新消息里学习说话方式（见 [`learning`]）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct LearningConfig {
    /// 总开关；关闭后不再学习，已学画像也不进后续提示词。
    pub enabled: bool,
    /// 归纳模型；留空沿用便宜的判定模型。
    pub model: String,
    /// 至少攒够多少条有效新消息才考虑调用一次。
    pub min_messages: usize,
    /// 两次学习尝试（含失败）至少隔多少分钟，跨重启也生效。
    pub interval_minutes: u32,
    /// 新消息触发后先等多久，再确认整个搭话插件仍然空闲（秒）。
    pub idle_seconds: u64,
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: String::new(),
            min_messages: 16,
            interval_minutes: 360,
            idle_seconds: 90,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct AmbientConfig {
    /// 总开关。
    pub enabled: bool,
    /// 开启搭话的群号；空列表等于不开启群聊。
    pub groups: Vec<String>,
    /// 话量：`quiet`（安静）/ `normal`（适中）/ `lively`（活跃）。
    ///
    /// 想让它整体话多一点或少一点，从前要同时调门槛、每小时目标、冷却和随口一句四五个数，
    /// 还得知道它们怎么互相抵。这一项在那几个数之上整体换算（见 [`Self::talk_scale`]），
    /// 不改写它们：细调照旧，平常只动这一项。
    pub talk: String,
    /// 是否在私聊里启用同一个人格。每个私聊频道有独立窗口、记忆和情绪状态。
    pub private: bool,
    /// 允许私聊的用户 ID；空列表表示不限制。只在 `private = true` 时生效。
    pub private_users: Vec<String>,
    /// 不看账号身份、直接开放人格群管理的群；还须具备 QQ 对应权限。
    pub management_groups: Vec<String>,
    /// 账号在群里是群主或管理员时，人格自动以管理员身份管群（照 `rules.md` 与各群群规）。
    pub management_auto: bool,
    /// 永远不让人格管的群，压过上面两条。
    pub management_excluded: Vec<String>,
    /// 每群每天最多几次管理动作（解禁这类收手的不算）；0 不限。
    pub management_daily: usize,
    /// 判定模型：便宜、快、能看图。写 `供应商/模型` 时按 `[oai.providers]` 取接口，
    /// 默认走 DeepSeek 官方的 V4.1 Flash（API 模型名为 deepseek-flash）。
    pub gate_model: String,
    /// 判定用的浓缩人设画像（见 [`GATE_PERSONA`]）。判定只需知道对什么感兴趣、
    /// 避开什么、怎么接话，不需要完整写作人设；留空则回退用完整人设（更贵）。
    pub gate_persona: String,
    /// 发言模型，写成 `供应商/模型`。默认走 DeepSeek 官方的 V4.1 Flash，
    /// 与判定同一个模型：便宜、快，只配一把 DeepSeek 密钥就能开口。
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
    ///
    /// 这个数是「平时的节奏」。adaptive_gate 打开时它还会跟着群里的热闹程度伸缩：
    /// 一直有人说话就看得勤一点，半天没人吭声就看得稀一点——判定是这里唯一按次计费的
    /// 东西，没人的时候没必要按原来的节拍一直敲。
    pub gate_interval_seconds: u64,
    /// 看群间隔跟着群里的热闹程度走。
    ///
    /// 以最近五分钟的消息速率（不含自己）为准：达到 busy_rate 就缩到
    /// busy_factor 分之一，几乎没人说话就拉长到 quiet_factor 倍，中间照
    /// gate_interval_seconds。上限是 gate_interval_max。
    pub adaptive_gate: bool,
    /// 算「忙」的门槛：最近五分钟平均每分钟几条（不含自己），默认 6。
    pub busy_rate: u32,
    /// 忙的时候间隔缩到几分之一（1–6）。
    ///
    /// 默认 1：热闹的群本来就靠「攒够十几条提前看」跟住话题，再把基础间隔也砍半
    /// 等于判定次数翻倍，与省钱的初衷相反。想让它跟得更紧，再调 2。
    pub busy_factor: u32,
    /// 闲的时候间隔拉长到几倍（1–6）。
    pub quiet_factor: u32,
    /// 拉长之后最多隔多久看一眼（秒）。
    pub gate_interval_max: u64,
    /// 上一眼之后又攒了这么多条，就不等下一眼了；0 关闭。
    ///
    /// 这是「消息量波动」那一档：群里突然聊开了（十几条在几十秒里涌进来），
    /// 跟着原节拍等下去，接上话时话题已经过去。提前看这一眼仍然走完判定，
    /// 只是不再干等；两次提前看之间至少隔 burst_min_gap_seconds，免得刷屏时
    /// 每来一条就判一次，反而更贵。
    pub burst_look: usize,
    /// 两次提前看之间至少隔几秒。
    pub burst_min_gap_seconds: u64,
    /// 群管辅助（见 [Assist]）。
    pub assist: Assist,
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
    /// 一条随钟点走的生活线：这会儿在通勤、在工位、在吃饭还是在打游戏。
    ///
    /// 与 `mood_enabled` 是两件事：那一条管精神头（醒着但累），这一条管处境
    /// （人在不在、腾不腾得出手）。凌晨睡着时不主动开口，上班时间门槛抬高一点；
    /// 提示词里会多一句「你这一会儿」。关掉就退回全天一个样子。
    pub life_enabled: bool,
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
    /// 从号主手发的新消息里增量归纳风格、常用表述与接话偏好。
    pub learning: LearningConfig,
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
    /// 发送前短暂显示 QQ 原生“正在输入”。它是给人看的状态，不是消息：失败只丢一次
    /// 后台调用，不会拦下这句话。
    pub qq_typing: bool,
    /// 看过群聊就把它标成已读（一轮最多一次，随读上下文或开口触发）。
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
    /// 一条发言里的文字超过多少字就排成图片再发；0 关闭。
    ///
    /// 认真答疑时该讲的步骤照讲，只是不再把几百字直接贴进群：排成一页纸，一条消息
    /// 说完。整批话如果都是字、又合起来够长，会并成一张图，不在中间多插几条。
    pub long_text_chars: usize,
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
            talk: "normal".to_string(),
            private: false,
            private_users: Vec::new(),
            management_groups: Vec::new(),
            management_auto: true,
            management_excluded: Vec::new(),
            management_daily: 20,
            gate_model: "deepseek/deepseek-flash".to_string(),
            gate_persona: GATE_PERSONA.to_string(),
            reply_model: "deepseek/deepseek-flash".to_string(),
            thinking: "low".to_string(),
            temperature: Some(1.0),
            help_thinking: "high".to_string(),
            help_model: String::new(),
            breakthrough_per_hour: 3,
            tools: "read,write".to_string(),
            score_threshold: 64,
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
            adaptive_gate: true,
            busy_rate: 6,
            busy_factor: 1,
            quiet_factor: 3,
            gate_interval_max: 900,
            burst_look: 15,
            burst_min_gap_seconds: 30,
            assist: Assist::default(),
            cooldown_seconds: 180,
            cooldown_penalty: 18,
            focus_max_seconds: 180,
            max_per_hour: 4,
            budget_penalty: 12,
            reply_on_mention: true,
            aliases: Vec::new(),
            preferred_name: String::new(),
            summon_command: "/搭话".to_string(),
            memory_enabled: true,
            mood_enabled: true,
            life_enabled: true,
            memo_budget: 3,
            search_enabled: true,
            search_budget: 3,
            peak: peak::PeakConfig::default(),
            reflect_enabled: true,
            learning: LearningConfig::default(),
            quick_per_hour: 2,
            quick_floor: 38,
            owner_quiet_seconds: 480,
            owner_grace_seconds: 75,
            send_freshness_seconds: 25,
            qq_typing: true,
            qq_mark_read: true,
            screenshot_on_suspicion: false,
            screenshot_cooldown_seconds: 21_600,
            messages_budget: 2,
            split_chars: 60,
            long_text_chars: 240,
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

/// 私聊会话登记：平台 channel id → 对方 QQ 号。
///
/// 直接拿平台 channel 当窗口键（不再另造 `direct:` 前缀），因为能力层那一侧的
/// 常驻窗口就是按 channel 存消息的；两处用同一个键，搭话的窗口和工具看到的才是
/// 同一段对话。群在 QQ 里是纯数字，私聊是 `private:<QQ号>`，不会互相撞上；
/// 万一哪个实现端真的撞了，`config.groups` 优先（见 [`scope_enabled`]）。
fn direct_sessions() -> &'static Mutex<HashMap<String, String>> {
    static SESSIONS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    SESSIONS.get_or_init(Default::default)
}

fn remember_direct(channel: &str, user: &str) -> String {
    direct_sessions()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(channel.to_string(), user.to_string());
    channel.to_string()
}

pub(super) fn direct_user(scope: &str) -> Option<String> {
    direct_sessions()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(scope)
        .cloned()
}

pub(super) fn is_direct(scope: &str) -> bool {
    direct_sessions()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains_key(scope)
}

/// 窗口键就是平台 channel id；留这个函数是为了让调用处读起来仍是「换回平台的口径」。
pub(super) fn scope_channel(scope: &str) -> &str {
    scope
}

fn scope_enabled(config: &AmbientConfig, scope: &str) -> bool {
    // 群优先：即使某个实现端的私聊频道号恰好等于群号，也不会把群当成私聊。
    if config.groups.iter().any(|id| id == scope) {
        return true;
    }
    if !config.private {
        return false;
    }
    direct_user(scope)
        .is_some_and(|user| config.private_users.is_empty() || config.private_users.contains(&user))
}

fn direct_name(scope: &str, turns: &[Turn]) -> String {
    let Some(user) = direct_user(scope) else {
        return String::new();
    };
    turns
        .iter()
        .rev()
        .find(|turn| turn.user_id == user && !turn.from_me)
        .map(|turn| turn.name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or(user)
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
    /// 他本人在这个群里平时每小时说几轮、放宽一点之后的数（见 [`habit::Habit::hourly_target`]）；
    /// 量不出来是 None。机器人在一个他平时几乎不说话的大群里，不该比他本人更活跃。
    hourly_target: Option<usize>,
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

    /// 这一眼该隔多久（不含 look_due 自己的随机倍数与「正聊着」的三分之一）。
    ///
    /// 以最近五分钟的速率判断忙闲，只分三档：忙、平常、闲。不做连续插值，
    /// 是因为这个数只该在「群里确实热闹」与「确实没人」时变，中间地带跟着
    /// 基础间隔走最稳；分档也让人在配置里能预料到它什么时候会变。
    fn look_interval(&self, rate: f32) -> Duration {
        let base = self.gate_interval();
        if !self.adaptive_gate {
            return base;
        }
        let scale = if rate >= self.busy_rate.max(1) as f32 {
            1.0 / self.busy_factor.clamp(1, 6) as f32
        } else if rate <= 0.25 {
            self.quiet_factor.clamp(1, 6) as f32
        } else {
            1.0
        };
        let max = Duration::from_secs(self.gate_interval_max.clamp(60, 3_600));
        base.mul_f32(scale).clamp(Duration::from_secs(15), max)
    }

    /// 话量换算成的两笔：门槛加减几分，以及每小时目标、随口一句次数要乘的倍数（冷却按它除）。
    fn talk_scale(&self) -> (i16, f32) {
        match self.talk.trim() {
            "quiet" => (8, 0.6),
            "lively" => (-6, 1.5),
            _ => (0, 1.0),
        }
    }

    /// 按话量换算后的次数上限；本来写 0（关闭）的仍是 0。
    fn scaled(&self, count: usize) -> usize {
        if count == 0 {
            return 0;
        }
        ((count as f32 * self.talk_scale().1).round() as usize).max(1)
    }

    /// 每群每小时最多随口吭几声（按话量换算）。
    pub(super) fn quick_hourly(&self) -> usize {
        self.scaled(self.quick_per_hour)
    }

    fn cooldown(&self) -> Duration {
        Duration::from_secs(self.cooldown_seconds).div_f32(self.talk_scale().1)
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
        let base = i16::from(self.effective_threshold(pressure.since_last_spoke)) + self.talk_scale().0;
        let shift = if self.mood_enabled {
            state.threshold_shift()
        } else {
            0
        };
        // 处在什么处境也算一笔：半夜睡着、上班腾不出手，都该更沉得住气。
        let doing = if self.life_enabled {
            life::threshold_shift()
        } else {
            0
        };
        let crowding = (pressure.recent_turns as i16)
            .saturating_mul(i16::from(self.speech_penalty_per_turn))
            .min(i16::from(self.speech_penalty_cap));
        let total = base
            + shift
            + doing
            + crowding
            + self.cooldown_charge(pressure)
            + self.budget_charge(pressure);
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
        let target = self
            .scaled(self.max_per_hour)
            .min(pressure.hourly_target.unwrap_or(usize::MAX));
        let over = pressure.hourly_turns.saturating_sub(target);
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
        let (mut typing, mut think) = if self.mood_enabled {
            (state.typing_scale(), state.think_scale())
        } else {
            (1.0, 1.0)
        };
        // 人在忙的时候想得久一点、敲得慢一点；闲着就快。这个倍率叠在精神头之上。
        if self.life_enabled {
            let doing = life::pace_scale();
            typing /= doing;
            think *= doing;
        }
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

    /// 被点名那一轮第一次没答出来之后的一份精简配置：退回没被换过的发言模型与平常的
    /// 思考强度，不联网、不画图写歌拍片，总时限压到一分半——这时人家已经等了一阵，
    /// 要的是尽快回一句，不是回得多周全。
    fn lean_retry(&self, original: &Self) -> Self {
        Self {
            reply_model: original.reply_model.clone(),
            thinking: original.thinking.clone(),
            search_enabled: false,
            draw_budget: 0,
            music_budget: 0,
            video_budget: 0,
            reply_timeout_seconds: self.reply_timeout_seconds.min(90),
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
            // 发言默认不换：见 `peak.swap_reply`。
            reply_model: if self.peak.swap_reply {
                self.peak.model_or(&self.reply_model).to_string()
            } else {
                self.reply_model.clone()
            },
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
        management_auto: config.management_auto,
        management_excluded: config.management_excluded.clone(),
        management_daily: config.management_daily,
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
        long_text_chars: config.long_text_chars,
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
        let mut state = snapshot.map(mood::Snapshot::describe).unwrap_or_default();
        if self.config.life_enabled {
            if !state.is_empty() {
                state.push('\n');
            }
            state.push_str(&life::brief());
        }
        serde_json::json!({
            "register": tone::register(turns),
            "state": state,
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

    fn vet_text(&self, _group: &str, text: &str, turns: &[Turn]) -> Option<String> {
        if tone::echoes(text, turns) {
            return Some(
                "这句和你最近刚说过的话几乎一样。别换个标点再发一遍；有新的信息才说，否则沉默"
                    .to_string(),
            );
        }
        stray::is_ungrounded_callback(text, turns).then(|| {
            "这句在翻旧账：眼前几条里没人提起过去，你说的「上次/之前说过」也跟眼前聊的碰不上，\
             多半是从记忆里搬出来的，群友一看就是自说自话。接眼前正在聊的事，或者干脆先放着"
                .to_string()
        })
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
    /// 管群那一段：这个群把管理交给它时写着群规；没开放时为空。
    ///
    /// 判定与发言都带着：判定要知道广告、刷屏在这里是「得出手」的事，发言要知道
    /// 照哪份群规管。怎么管的那套做法只给发言（[`speak::MANAGE_RULES`]）。
    pub duty: String,
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
        me: (&str, &str),
    ) -> Self {
        // 状态算一次用两处：一句给模型看的「你现在的状态」，以及挑样本的调子。
        let snapshot = config.mood_enabled.then(|| mood::snapshot(group));
        let state = snapshot.map(mood::Snapshot::describe).unwrap_or_default();
        Self {
            identity: scene_identity(group, config, turns, me),
            rhythm,
            register: tone::register(turns),
            state: state.clone(),
            memory: if config.memory_enabled {
                memory::with_group(group, |memory| {
                    memory.brief(turns, chrono::Local::now().timestamp())
                })
            } else {
                String::new()
            },
            own: format!(
                "{}{}{}{}{}{}{}{}",
                self_facts(),
                if config.memory_enabled {
                    memory::with_group(group, |memory| {
                        memory.claims_brief(turns, chrono::Local::now().timestamp())
                    })
                } else {
                    String::new()
                },
                if is_direct(group) {
                    String::new()
                } else {
                    recent::habit_brief(group)
                },
                recent::recalled(group, turns),
                news::brief(turns),
                if config.learning.enabled {
                    learning::brief()
                } else {
                    String::new()
                },
                voice::brief(turns, voice_register(config, group), group),
                stickers::brief(turns, config.sticker_max, &state)
            ),
            noticed: String::new(),
            group_about: if is_direct(group) {
                String::new()
            } else {
                group_about(group)
            },
            duty: if is_direct(group) {
                String::new()
            } else {
                duty(group, config)
            },
            careful: false,
        }
    }

    /// 现场 → 注入提示词的一段话。
    pub(crate) fn brief(&self) -> String {
        let mut out = format!(
            "{}\n{}{}{}{}\n",
            now_context(),
            self.identity,
            self.group_about,
            self.duty,
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

fn scene_identity(scope: &str, config: &AmbientConfig, turns: &[Turn], me: (&str, &str)) -> String {
    let mut brief = if is_direct(scope) {
        let user = direct_user(scope).unwrap_or_default();
        let name = direct_name(scope, turns);
        let mut text = String::from("你自己：正在私聊，不是群聊。");
        if !me.0.is_empty() {
            text.push_str(&format!("你的 QQ 是 {}。", me.0));
        }
        if !me.1.is_empty() {
            text.push_str(&format!("对方那边看到的你的名字是「{}」。", me.1));
        }
        text.push_str(&format!(
            "对方是「{name}」（QQ {user}）。这里的话是直接说给你的，不必等 @；\
             也当自己发消息、看历史和发表情即可，不需要群管理或群成员那一套。\
             私聊内容不拿去别的群或别的私聊里转述。\n"
        ));
        text
    } else {
        identity::brief(scope)
    };
    let name = config.preferred_name.trim();
    if !name.is_empty() {
        brief.push_str(&format!(
            "你平时叫「{name}」，介绍自己时用这个称呼；平台展示名只是资料上的文字。\n"
        ));
    }
    brief
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

/// 这个群开放了管理时的那一段：一句身份加上群规；没开放就是空串。
///
/// 账号身份取群身份缓存（几小时一刷）；真动手时能力层会再问一遍平台，缓存过时
/// 顶多让这一段多写或少写一轮。
fn duty(group: &str, config: &AmbientConfig) -> String {
    let role = identity::of(group).map(|me| me.role).unwrap_or_default();
    if !crate::plugins::oai::chat::management_open(&chat_config(config), group, &role) {
        return String::new();
    }
    let Some(dir) = DATA_DIR.get() else {
        return String::new();
    };
    let mut lines = Vec::new();
    for path in [rules_path(dir), rules_dir(dir).join(format!("{group}.md"))] {
        if let Ok(raw) = std::fs::read_to_string(path) {
            lines.extend(
                raw.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .map(|line| format!("- {}", line.trim_start_matches("- "))),
            );
        }
    }
    let mut out = String::from("你在本群管事：禁言、撤回群友的消息、踢人这些管理动作对你开放。");
    if lines.is_empty() {
        out.push('\n');
    } else {
        out.push_str(&format!("群规：\n{}\n", lines.join("\n")));
    }
    out
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

/// 群规（所有开放管理的群通用）：管理员写，每轮现读。
fn rules_path(base: &Path) -> PathBuf {
    base.join("rules.md")
}

/// 各群另有的群规：`rules/<群号>.md`，与 [`rules_path`] 那份一起生效。
fn rules_dir(base: &Path) -> PathBuf {
    base.join("rules")
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

/// 历来内置过的人设原文（md5）。磁盘上那份与其中某一版逐字相同，说明没人改过，
/// 启动时换成这一版；改过一个字的都不碰。
///
/// 从前只在缺失时写入，于是老实例永远停在第一次启动时那一版：线上 2026-10-28 还在用
/// 1.62.0 的人设，1.67.0 补上的口癖与记忆说法一句都没到。
const RETIRED_PERSONAS: &[&str] = &[
    "09c27237a69809cbdf812d5cf516c095",
    "0ca8ad31f3abfeed5b11744697f2f057",
    "0f3bdbc042f824780edb6c706409222a",
    "1a560430c53fde7190a2472a2c482041",
    "26fb0c46b4004725a8995d4ac718f826",
    "423835a47031973f066ccb9969c26321",
    "4fefb6e59a5750c39eeb756f6a7f77a9",
    "600460da70f5e846d6e1b2fdf6055e1a",
    "608f390b69724bbe9b1e490ca76e307d",
    "65f9fdf9c28563b11a1271772bd2802a",
    "68d205414eb00a0f7f400224f26c2d7d",
    "6e8928f9a36866e8d79b602495931398",
    "6f249090f93778dfbd71ee0ab2fc9100",
    "71bc2086302f76d9092d9ef4d045b2e8",
    "7a93c4c9d2910b4590240ed84dc72d66",
    "81d04f7af64b631ed979b7a1e02ba001",
    "8ebdd9bd64efc95cfb96f59a68be68bd",
    "90ad484c3514eaef57689ad057e77731",
    "9303fa8c4ca29c7f302526307e3b08ad",
    "939216f11049e6b7feeb64a6e987b7bc",
    "96ab330816b127c3f37bbc1080d664a6",
    "9d60364fea7e48f9eed62313d0efebea",
    "aab2de18a2338a5e007421c0eac7e98b",
    "ab54e62a2b79bc55dfd87c8298d61313",
    "b0bf29f2fca6989962051c4c65a0ad75",
    "b17f099d4c838c3c6fe2782f9a744ef2",
    "b6ced190adf45337909c29dac5cc797e",
    "be00dc98e35ce7844d4c9b09cf923865",
    "c31c14233a62163176d4668952178c25",
    "cfd2a783abb0b8235014805f7851411a",
    "d129ba6d764fe60912a868dd336b2288",
    "d91d91b4aba61a251350eb1ad9def84e",
    "db724ce9e3c31e5098f89087e3d06e42",
    "de0a89ae8d16552d0facfd984cf5a5ff",
    "e009fc7ff5a1294a1ae622ef3dd86bf9",
    "e10beedc044f7eb2fbe1cb3d1f2022b9",
    "e1fe6eaf0f5a564a65328277809a2678",
    "e3cacb7b5f5161c8476bc59b95dc2dfd",
];

/// 铺开人设与 skill。
///
/// 人设只在缺失、或还是某一版旧内置原文时写入——它是给人改的，覆盖改过的那份等于把
/// 管理员的打磨扔掉；skill 描述的是本仓库实现的消息元素，属于代码的一部分，每次启动都对齐。
pub(crate) async fn setup(base: &Path) -> std::io::Result<()> {
    let persona = persona_path(base);
    if let Some(parent) = persona.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    match tokio::fs::read(&persona).await {
        Err(_) => tokio::fs::write(&persona, PERSONA).await?,
        Ok(current) => {
            let digest = format!("{:x}", md5::compute(&current));
            if current != PERSONA.as_bytes() && RETIRED_PERSONAS.contains(&digest.as_str()) {
                let backup = persona.with_extension(format!("md.before-{}", env!("CARGO_PKG_VERSION")));
                tokio::fs::write(&backup, &current).await?;
                tokio::fs::write(&persona, PERSONA).await?;
                info!(target: LOG_TARGET, "人设还是旧版内置原文，换成这一版（旧的留在 {}）", backup.display());
            }
        }
    }
    // 档案跟人设一样只在缺失时写：它写的是这个人自己的事，覆盖等于把攒下来的
    // 那点前后一致性抹掉。模板里只有格式说明，实际内容由管理员填。
    let facts = self_path(base);
    if !facts.exists() {
        tokio::fs::write(&facts, SELF).await?;
    }
    // 群规同理：管理员改过的不覆盖。
    let rules = rules_path(base);
    if !rules.exists() {
        tokio::fs::write(&rules, RULES).await?;
    }
    tokio::fs::create_dir_all(rules_dir(base)).await?;
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
    life::attach(base);
    learning::attach(base);
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
    let config = crate::plugins::get_config_or_default::<AmbientConfig>(ctx);
    if !config.enabled || (config.groups.is_empty() && !config.private) {
        return;
    }
    let Some(event) = ctx.as_message() else {
        observe_notice(ctx, writer, mgr, base, &config).await;
        return;
    };
    let me = ctx.bot.self_id();
    let scope = if let Some(group) = event
        .group_id()
        .filter(|id| config.groups.iter().any(|enabled| enabled == id))
    {
        group.to_string()
    } else if !event.is_group() && config.private {
        let channel = event.channel_id();
        let peer = if event.user_id() == me {
            channel.strip_prefix("private:").unwrap_or(channel)
        } else {
            event.user_id()
        };
        if channel.is_empty()
            || peer.is_empty()
            || (!config.private_users.is_empty()
                && !config.private_users.iter().any(|allowed| allowed == peer))
        {
            return;
        }
        remember_direct(channel, peer)
    } else {
        return;
    };
    let group = scope.as_str();

    // 号主最近亲手打的话：隔几个钟头从聊天记录里重捞一次，给发言当最新的口吻标尺。
    recent::ensure_fresh(ctx);
    let mut turn = window::turn_from(&event, &me);
    // 引用在群里的样子是「原话摆在那儿」，模型也该看见被引的是哪一句、谁说的；
    // 引到自己那条的时候就等于点了名，与 @ 同等地把它叫醒。
    window::with_group(group, |state| {
        window::resolve_quote(&mut turn, |id| state.quote_of(id));
    });
    // 被引的那条已经滚出窗口时窗口认不得它：向平台要原话，才看得见「引的是什么」，
    // 也才认得出「引的是我自己」。
    if !turn.call.reply_to.is_empty() && turn.call.quote.is_empty() {
        let found =
            window::quote_from_platform(ctx, writer, scope_channel(group), &turn.call.reply_to)
                .await;
        debug!(
            target: LOG_TARGET,
            "群 {group} 引用的消息已滚出窗口，向平台要回原话：{}",
            if found.is_some() { "要到了" } else { "没要到" }
        );
        if let Some(found) = found {
            turn.call.quoted_images = found.images;
            window::resolve_quote(&mut turn, |_| Some((found.from_me, found.summary)));
        }
    }
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
    turn.call.named_me = !is_direct(group)
        && !turn.from_me
        && identity::called_by_name(group, &config.aliases, &turn.text);
    // 私聊没有 @ 这个步骤，对方发来的普通正文本身就是直接搭话。
    if is_direct(group) && !turn.from_me {
        turn.call.at_me = true;
        turn.mentions_me = true;
    }
    // 每条群消息都带着群名，而 `guild.get` 未必给得出——记下来当兜底。
    if !is_direct(group) {
        identity::note_group_name(group, event.0.get_str("group_name").unwrap_or_default());
    }
    // 剥掉指令后空无一物的那条消息没有内容可给模型看；它只是按了一次键。
    let empty = turn.text.is_empty() && turn.images.is_empty();
    if empty && !summoned {
        return;
    }
    // 只学号主亲手发的日常话，不学搭话指令；学习模块内部再做文本清洗、入队与空闲调度。
    if !summoned {
        learning::note(ctx, mgr, base, &config, group, &turn);
    }
    if config.memory_enabled && !turn.from_me {
        let (id, name, at) = (turn.user_id.clone(), turn.name.clone(), turn.at);
        memory::edit(group, |memory| memory.see(&id, &name, at));
    }
    if config.memory_enabled && turn.call.mine() && !turn.from_me && !turn.user_id.is_empty() {
        let (id, at) = (turn.user_id.clone(), turn.at);
        memory::edit(group, |memory| memory.engage(&id, at));
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
    let scope = if let Some(group) = raw
        .get_str("group_id")
        .filter(|id| config.groups.iter().any(|enabled| enabled == id))
    {
        group.to_string()
    } else {
        let scope = raw.get_str("channel_id").unwrap_or_default().to_string();
        if !scope_enabled(config, &scope) {
        return;
        }
        scope
    };
    let group = scope.as_str();
    let Some((turn, recalled)) = notice_turn(raw, &ctx.bot.self_id()) else {
        return;
    };
    if config.memory_enabled && turn.call.mine() && !turn.from_me && !turn.user_id.is_empty() {
        let (id, at) = (turn.user_id.clone(), turn.at);
        memory::edit(group, |memory| memory.engage(&id, at));
    }
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
        // 申请人还不在群里，这一行是给管事的人看的：开放管理时能用 approve 处理。
        "guild-member-request" => {
            let comment = raw.get_str("comment").unwrap_or("").trim();
            format!(
                "[加群申请：{user} 申请入群，附言「{}」；申请号 {}]",
                if comment.is_empty() { "（无）" } else { comment },
                raw.get_str("flag").unwrap_or("")
            )
        }
        // 实现端不知道谁操作的（自己退群）就不带 operator，别印一个空的「操作者」。
        "guild-member-removed" => match raw.get_str("operator_id").filter(|id| !id.is_empty()) {
            Some(operator) => format!("[群成员 {user} 离开了群聊；操作者 {operator}]"),
            None => format!("[群成员 {user} 离开了群聊]"),
        },
        "guild-member-updated" => {
            // 管理动作的事件只更新现场，避免自己管理→自己评论的循环。
            from_me = true;
            if raw.get_str("notice_type") == Some("group_ban") {
                let target = if user.is_empty() {
                    "全体成员".into()
                } else {
                    user.to_string()
                };
                let duration = crate::db::bans::normalize_duration(
                    raw.get_str("sub_type") == Some("lift_ban"),
                    raw.get_i64("duration"),
                );
                match duration {
                    0 => format!("[{target} 已解除禁言]"),
                    crate::db::bans::UNKNOWN => format!("[{target} 被禁言（时长不明）]"),
                    secs => format!("[{target} 被禁言 {secs} 秒]"),
                }
            } else {
                let member = raw.get("_satori").and_then(|s| s.get("member"));
                format!(
                    "[群成员 {user} 的资料更新：{}]",
                    member.map_or_else(|| "具体变化未知".into(), ToString::to_string)
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
    loop {
        let gap = Duration::from_secs(config.burst_min_gap_seconds.clamp(0, 300));
        let (urgent, engaged, due, burst, question, joining, rate) =
            window::with_group(group, |state| {
                let rate = state.rate(Duration::from_mins(5));
                let interval = config.look_interval(rate);
                let since = state.since_look();
                let ready = since.is_none_or(|elapsed| elapsed >= gap);
                let burst = config.burst_look > 0 && state.unseen() >= config.burst_look && ready;
                let fresh = state.fresh();
                let question = config.assist.questions
                    && ready
                    && fresh
                        .iter()
                        .any(|turn| !turn.from_me && looks_like_question(&turn.text));
                let now = chrono::Local::now().timestamp();
                let joining = config.assist.newcomers
                    && ready
                    && fresh.iter().any(|turn| {
                        !turn.from_me
                            && is_join_notice(&turn.text)
                            && now - turn.at <= config.assist.newcomer_within_seconds as i64
                    });
                (
                    state.urgent(),
                    state.engaged(),
                    state.look_due(interval),
                    burst,
                    question,
                    joining,
                    rate,
                )
            });
        if urgent || question || joining {
            // 看群越勤，被叫到也看得越快；间隔调成 0 就是随叫随到。
            if !engaged {
                tokio::time::sleep(notice_delay().min(config.gate_interval() / 5)).await;
            }
            if (question || joining) && !urgent {
                debug!(
                    target: LOG_TARGET,
                    "群 {group} {}，提前看一眼",
                    if joining { "有人进群" } else { "有人提问" }
                );
            }
            wait_for_owner(ctx, group, config).await;
            return;
        }
        if burst {
            debug!(target: LOG_TARGET, "群 {group} 攒够 {burst} 条新消息，不等下一眼", burst = config.burst_look);
            return;
        }
        if due.is_zero() {
            debug!(target: LOG_TARGET, "群 {group} 这一眼（最近五分钟 {rate:.1} 条/分钟）");
            return;
        }
        let latest = crate::plugins::get_config_or_default::<AmbientConfig>(ctx);
        if !latest.enabled || !scope_enabled(&latest, group) {
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
    if is_direct(group) {
        return;
    }
    let grace = config.owner_grace();
    let (present, summoned, baseline, called) = window::with_group(group, |state| {
        let called = state.has_unread_mention()
            || state
                .fresh()
                .iter()
                .any(|turn| !turn.from_me && turn.call.named_me);
        (
            state.owner_active(config.owner_quiet()),
            state.has_unread_summon(),
            state.owner_marks(),
            called,
        )
    });
    // 普通提问、紧急信号和新人入群只是提前看一眼，最终还有号主让位那道闸；
    // 这里只给真正冲这个账号来的 @、引用、戳或称呼留本人先答的时间。
    if grace.is_zero() || !present || summoned || !called {
        return;
    }
    info!(target: LOG_TARGET, "群 {group} 有人叫，但本人刚在群里说过话，先等他自己答（最多 {} 秒）", grace.as_secs());
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let latest = crate::plugins::get_config_or_default::<AmbientConfig>(ctx);
        if !latest.enabled || !scope_enabled(&latest, group) {
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
        let mut body = serde_json::json!({"channel_id": scope_channel(group).to_string()});
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
    let location = if is_direct(group) {
        "channel_id"
    } else {
        "guild_id"
    };
    let rows = ctx
        .db
        .query_all_raw(Statement::from_sql_and_values(
            ctx.db.get_database_backend(),
            format!(
                "select message_id from message_records \
                 where {location} = ? and user_id = ? and member_role != 'self' and time >= ?"
            ),
            [scope_channel(group).into(), me.into(), since.into()],
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
        let config = crate::plugins::get_config_or_default::<AmbientConfig>(ctx);
        if config.focus_max_seconds == 0 {
            window::with_group(group, |state| state.focus = None);
        }
        if !config.enabled || !scope_enabled(&config, group) {
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
        let config = crate::plugins::get_config_or_default::<AmbientConfig>(ctx);
        if !config.enabled || !scope_enabled(&config, group) {
            return Ok(());
        }
        let (mut seq, turns, mentioned, summoned, silent_for, rhythm, focused) =
            window::with_group(group, |state| {
                let mentioned =
                    state.take_mention() && (config.reply_on_mention || is_direct(group));
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

/// 单次模型请求多久没有回音就算卡死。
///
/// 全局的 `request_stall_seconds` 是给什么都等得起的场合定的（三分钟）；群里有人在等回话，
/// 2026-10-02 16:26 有人 @ 它，高峰期换上的替补模型一声不吭，一直耗到整轮四分钟的预算
/// 用完才报错，那条 @ 就这么没了下文。平常接话 75 秒没动静就算卡死（执行层会换一次请求
/// 重来，见 `agent::run`），答疑要想得久，给 150 秒。配成不检测（0）的就仍然不检测。
fn speak_stall(global: Option<Duration>, careful: bool) -> Option<Duration> {
    let cap = Duration::from_secs(if careful { 150 } else { 75 });
    global.map(|limit| limit.min(cap))
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
        crate::plugins::get_config_or_default::<crate::plugins::oai::OaiConfig>(ctx)
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
        match provider.as_deref() {
            Some(name) => anyhow::bail!("{}", crate::plugins::oai::catalog::missing_key_hint(name)),
            None => anyhow::bail!("判定模型需要 API 配置，请先设置 oai 的接口地址与密钥"),
        }
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
    // 下面会按计价时段与答疑把 `config` 换成别的模型；被点名的那一轮答不出来时要退回这一份。
    let original = config;
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
    if !is_direct(group) && screenshot::try_reply(ctx, writer, group, config, *seq, turns).await {
        if config.mood_enabled {
            mood::nudge(|mood, now| mood.spoke(group, now));
        }
        return Ok(());
    }

    // 这一眼看到了哪些：上一眼之后新来的几条，判定要把它们和前情分开看。
    // 顺手认一下有没有人刚进群——这一行在平台上是个事件，不是谁说的话。
    let (fresh, glance, joined) = window::with_group(group, |state| {
        let fresh = state.unseen();
        let joined = config.assist.newcomers.then(|| {
            let now = chrono::Local::now().timestamp();
            state
                .fresh()
                .into_iter()
                .rev()
                .find(|turn| {
                    !turn.from_me
                        && is_join_notice(&turn.text)
                        && now - turn.at <= config.assist.newcomer_within_seconds as i64
                })
        });
        state.mark_look();
        (fresh, state.recent((fresh + 6).clamp(12, 36)), joined.flatten())
    });

    // 上一眼之后什么都没有（只多了自己说的话、或者全是没进窗口的指令）：这一眼
    // 不必问模型。这是最便宜的一道闸——不花一分钱，也最常生效。
    if fresh == 0 && joined.is_none() && !mentioned && !summoned {
        debug!(target: LOG_TARGET, "群 {group} 这一眼没有新东西，不判定");
        return Ok(());
    }

    // 这个点人在睡觉：没人点名、也没按搭话指令，就不必花钱判一次「想不想接」。
    // 判定模型看不到生活线，它只会按话题热度给分，半夜三点照样能给出「想接」。
    if config.life_enabled && !mentioned && !summoned && life::asleep() {
        debug!(target: LOG_TARGET, "群 {group} 这个点人格在睡，这一眼不判定");
        return Ok(());
    }

    // 号主本人在线：机器人不主动插话。这一眼算看过了（上面已经 mark_look），他看过的
    // 话题不会等他走了再被机器人翻出来接。被点名与搭话指令不在此列。
    if !is_direct(group)
        && !mentioned
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
        && let Some(gap) = window::with_group(group, window::GroupState::take_feedback)
    {
        mood::nudge(|mood, now| match gap {
            ..=90 => mood.engaged(group, now),
            300.. => mood.ignored(group, now),
            _ => {}
        });
    }
    // 「我在这个群里是谁」在判定之前就要在手上：判定要认出「有人在叫我」，而群里
    // 叫人用的是名片上那几个字。资料按小时缓存，所以这一句绝大多数时候不出网。
    if !is_direct(group) {
        identity::refresh(
            ctx,
            writer,
            avatar_endpoint(ctx, mgr, config).await.as_ref(),
            group,
        )
        .await;
    }
    if !mentioned && !summoned && !current(ctx, group, *seq) {
        return Ok(());
    }
    let login = ctx.bot.login_user.get();
    let me = (login.id.as_str(), login.name.as_deref().unwrap_or(""));
    let scene = Scene::build(group, config, turns, rhythm.to_string(), me);
    let mut noticed = String::new();
    let mut careful = false;
    // 刚进群的人也在这一批里：不判定，直接让人格看着现场说一句。
    let greet = joined.is_some() && !mentioned && !summoned;
    if greet {
        info!(target: LOG_TARGET, "群 {group} 有人进群，交给人格打个招呼");
    } else if summoned {
        // 指令是人按下的：判定那一步整个不发生，这一批直接进第三步。
        info!(target: LOG_TARGET, "群 {group} 收到搭话指令，这一批交给人格");
    } else if !mentioned {
        let (api_base, api_key, gate_model) = gate_endpoint(ctx, mgr, &config.gate_model).await?;
        let pressure = window::with_group(group, |state| Pressure {
            since_last_spoke: silent_for,
            recent_turns: state.spoken_within(window::RECENT_SPEECH),
            hourly_turns: state.spoken_last_hour(),
            hourly_target: recent::hourly_target(group),
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
                ctx, writer, group, config, &verdict, threshold, silent_for, doze, seq,
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
    let login = ctx.bot.login_user.get();
    let mut scene = Scene::build(
        group,
        config,
        &latest,
        rhythm,
        (login.id.as_str(), login.name.as_deref().unwrap_or("")),
    );
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
        Called::of(mentioned, summoned, greet),
        &persona,
        &scene,
        seq,
        doze,
        original,
    )
    .await
}

/// 判定没过线、但落在「想吭一声」区间里时：按几率使用判定顺带写下的第一反应。
///
/// 不再为这一两个字另问一次模型；本地清洗、去重与现场检查不过就安静放弃。
#[allow(clippy::too_many_arguments)]
async fn quick_word(
    ctx: &Context,
    writer: &LockedWriter,
    group: &str,
    config: &AmbientConfig,
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
        quick_limit: config
            .quick_hourly()
            .min(recent::hourly_target(group).map_or(usize::MAX, |target| target * 2)),
        owner_present: state.owner_active(config.owner_quiet()),
        dozing: doze,
    });
    if !quick::wants(config, facts, rand::random::<f32>()) || !current(ctx, group, *seq) {
        return;
    }
    let Some(text) = quick::clean(&verdict.quip) else {
        return;
    };
    if quick::nods(&text) {
        debug!(target: LOG_TARGET, "群 {group} 随口一句只是附和，不发：{text}");
        return;
    }
    let turns = window::with_group(group, |state| {
        state.recent(config.context_turns.clamp(1, 80))
    });
    if quick::repeats(&text, &turns) || !current(ctx, group, *seq) {
        return;
    }
    let started = Instant::now();
    let pace::Speech::Say(mut utterances) = pace::parse(&text, 1, 0) else {
        return;
    };
    // 号主四个字以内的话只有 7.4% 带引用（九到十六个字 27%，十七个字以上 38%）：
    // 短反应不挂引用才像他；几个人同时在聊、对不上号时才按现场的赔率引，所以只给一半。
    if let Some(target) = reply_target(&turns)
        && let Some(first) = utterances.first_mut()
    {
        first.reply = quote::keeps(&target, &turns, rand::random::<f32>() * 2.0);
    }
    info!(target: LOG_TARGET, "群 {group} 随口一句（{}/{}）：{}", verdict.score, threshold, verdict.reason);
    window::with_group(group, window::GroupState::mark_quick);
    if let Err(error) = deliver(ctx, writer, group, config, &turns, utterances, seq, false, started).await {
        debug!(target: LOG_TARGET, "群 {group} 随口一句没发出去：{error:#}");
    }
}

/// 平台事件里的一行「某某加入了群聊」。
fn is_join_notice(text: &str) -> bool {
    text.starts_with("[群成员 ") && text.contains(" 加入了群聊]")
}

/// 群里有人像在提问：以问号收尾，或者带着「求问」「报错」这类字眼。
///
/// 与 [`asks_for_answer`] 分开：那个判的是「@ 它的时候是不是在问事」，上下文明确，
/// 可以用「怎么」「为什么」这种很宽的词；这里没人叫它，一句「怎么会」也会命中，
/// 放得太宽等于每有闲聊就提前看一眼。所以只认问号和明确的求助词，且要求不是平台事件。
fn looks_like_question(text: &str) -> bool {
    let text = text.trim();
    if text.chars().count() < 4 || text.starts_with('[') {
        return false;
    }
    text.ends_with('？')
        || text.ends_with('?')
        || [
            "有没有人知道", "有没有人懂", "求问", "求教", "求助", "报错", "怎么办", "怎么解决",
        ]
        .iter()
        .any(|word| text.contains(word))
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
    let config = crate::plugins::get_config_or_default::<AmbientConfig>(ctx);
    config.enabled
        && scope_enabled(&config, group)
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
    let config = crate::plugins::get_config_or_default::<AmbientConfig>(ctx);
    if !config.enabled || !scope_enabled(&config, group) {
        return Sendable::Stale;
    }
    // 私聊里对方就在等这句：中间又补了一句，也不该把已经想好的话咽回去。
    if is_direct(group) {
        return Sendable::Fresh;
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
    // 没被计价时段或答疑换过模型的原配置：被点名那一轮答不出来时退回它。
    original: &AmbientConfig,
) -> anyhow::Result<()> {
    let oai = crate::plugins::get_config_or_default::<crate::plugins::oai::OaiConfig>(ctx);
    let search = oai.search_config();
    // 与判定看到的是同一批图：已转码成模型收得下的格式，GIF 表情包也不例外。
    // 每张都带着出处（来自哪条消息），人格据此把「讲的那张」对回正确的消息号。
    let images = vision::usable_images(turns, config.context_images).await;

    let started = Instant::now();
    // 三类媒体工具挂不挂：解不出接口与密钥的那一类不挂（见 [oai.media]）。
    let media = crate::plugins::oai::media::slots(&oai, mgr).await;
    let (api_base, api_key, reply_model) = gate_endpoint(ctx, mgr, &config.reply_model).await?;
    debug!(target: LOG_TARGET, "群 {group} 发言模型：{}", config.reply_model);
    let first = speak::compose(
        &api_base,
        &api_key,
        &reply_model,
        base,
        &skill_dirs(base),
        persona,
        config,
        media,
        &search,
        speak_stall(oai.request_stall(), scene.careful),
        turns,
        &images,
        called,
        scene,
        Some((ctx, writer, group, seq)),
    )
    .await;
    let composed = match first {
        Ok(composed) => composed,
        // 有人冲着它来（@、引用、戳、搭话指令），答不出来就是一个人对着空气说了话。
        // 平常自己想开口的那种失败无人知晓，不值得再花一次钱。
        Err(error) if called != Called::Ordinary => {
            warn!(target: LOG_TARGET, "群 {group} 被点名的这一轮没答出来：{error:#}；换原来的模型精简着再试一次");
            tokio::time::sleep(Duration::from_secs(3)).await;
            let retry = config.lean_retry(original);
            let (api_base, api_key, reply_model) =
                gate_endpoint(ctx, mgr, &retry.reply_model).await?;
            speak::compose(
                &api_base,
                &api_key,
                &reply_model,
                base,
                &skill_dirs(base),
                persona,
                &retry,
                media,
                &search,
                speak_stall(oai.request_stall(), false),
                turns,
                &images,
                called,
                scene,
                Some((ctx, writer, group, seq)),
            )
            .await?
        }
        Err(error) => return Err(error),
    };

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
        let login = ctx.bot.login_user.get();
        let fresh = Scene::build(
            group,
            config,
            &latest,
            rhythm,
            (login.id.as_str(), login.name.as_deref().unwrap_or("")),
        );
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

/// 这次戳一戳该不该发：只戳眼前这几位群友，不戳自己，也不在两分钟内连戳。
///
/// 号码是模型写的，认不出是谁就是编的；而人戳人是逗一下，连着戳就成了骚扰。
fn nudge_allowed(target: &str, me: &str, turns: &[Turn], history: &[Turn]) -> bool {
    const COOLDOWN: i64 = 120;
    if target == me || !turns.iter().any(|turn| !turn.from_me && turn.user_id == target) {
        return false;
    }
    let now = chrono::Local::now().timestamp();
    !history.iter().any(|turn| {
        turn.from_me && turn.text.starts_with("[戳一戳") && now - turn.at < COOLDOWN
    })
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
        // 眼前没人提起，却说「上次说过」：那是从记忆里搬来的（见 [`stray`]）。
        if stray::is_ungrounded_callback(&text, turns) {
            info!(target: LOG_TARGET, "群 {group} 咽回一句没来头的翻旧账：{text}");
            return false;
        }
        true
    });
    if utterances.is_empty() {
        return Ok(false);
    }

    // 长文不发原样：整批话都是字、合起来又够长，就并成一段排成一页图，一条消息说完。
    // 零散的两三句照旧一条条说——那是聊天，不是一篇稿子。不并的话，人格把一篇答疑
    // 写成三行，群里就是三张字墙。
    if config.long_text_chars > 0
        && utterances.len() > 1
        && utterances
            .iter()
            .all(|utterance| long_text::body(&utterance.message).is_some())
    {
        let joined = utterances
            .iter()
            .filter_map(|utterance| long_text::body(&utterance.message))
            .collect::<Vec<_>>()
            .join("\n\n");
        if joined.chars().count() >= config.long_text_chars {
            let first = &utterances[0];
            let (reply, reply_to, wait) = (first.reply, first.reply_to.clone(), first.wait);
            utterances = vec![pace::Utterance {
                message: Message::new().text(joined.clone()),
                chars: joined.chars().count(),
                reply,
                reply_to,
                wait,
                nudge: None,
            }];
        }
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
    let mut nudged = false;
    for (index, utterance) in utterances.into_iter().enumerate() {
        if index > 0 {
            tokio::time::sleep(pace.gap()).await;
        }
        if utterance.wait > 0.0 {
            tokio::time::sleep(Duration::from_secs_f32(utterance.wait)).await;
        }
        // 这一句要不要排成图（见 [`long_text`]）。排版本身要几秒，但不必再按整篇
        // 字数等一遍打字时间——那是「敲一篇稿子」的时长，不是「发一张图」的。
        let card_source = long_text::source(&utterance.message, config.long_text_chars);
        // 模型那几秒是在读和想，不是在打字：字还得一个个敲出来。从前首条把模型耗时
        // 从打字时间里扣掉，四十个字的答疑八秒就到，群友一眼看出「不到 5 秒回消息」。
        let typing_chars = match &card_source {
            Some(_) => long_text::typing_chars(),
            None => utterance.chars,
        };
        tokio::time::sleep(pace.typing_delay(typing_chars)).await;
        let drifted = match sendable(ctx, group, *seq) {
            Sendable::Stale => break,
            Sendable::Fresh => false,
            Sendable::Drifted => true,
        };

        // 戳一戳是点头像的动作，不是一条消息：走平台的戳一戳接口，对方那边弹「戳了你」。
        if let Some(target) = utterance.nudge {
            if nudged || !nudge_allowed(&target, &me, turns, &history) {
                info!(target: LOG_TARGET, "群 {group} 咽回一次戳一戳：{target} 不在眼前，或刚戳过人");
                continue;
            }
            if let Err(error) =
                crate::adapters::satori::qq::poke(ctx, writer, scope_channel(group), &target).await
            {
                warn!(target: LOG_TARGET, "群 {group} 戳一戳失败：{error}");
                break;
            }
            info!(target: LOG_TARGET, "群 {group} 戳了 {target} 一下");
            window::with_group(group, |state| {
                if !sent {
                    state.mark_spoke();
                    if doze {
                        state.mark_doze_spoke();
                    }
                }
                // 与工具路径的记法一致；平台随后回来的戳一戳事件按时间与内容并不去重，
                // 多一行不碍事，没有这一行下一轮就看不见自己刚戳过。
                state.receive(Turn {
                    user_id: me.clone(),
                    name: "我".to_string(),
                    text: format!("[戳一戳 {target}]"),
                    from_me: true,
                    at: chrono::Local::now().timestamp(),
                    ..Turn::default()
                });
            });
            sent = true;
            nudged = true;
            continue;
        }

        let mut message = Message::new();
        // 打字的工夫里群里又冒出一两句：照样发，但首条挂上它回的那句，免得接错人。
        if (utterance.reply || (drifted && index == 0))
            && let Some(id) = quote_target(utterance.reply_to, fallback.clone(), turns)
        {
            message = message.reply(id);
        }
        message.0.extend(utterance.message.0.iter().cloned());
        // 长文排成图再发；渲染失败就照原文发——宁可多一段字，也不能把话丢掉。
        let message = match &card_source {
            Some(text) => match long_text::as_card(&message, text, ctx.browser_path().as_deref()).await {
                Some(card) => card,
                None => long_text::readable(message),
            },
            // 照文字发的话，公式换成能读的写法（`$x^2$` → `x²`）。
            None => long_text::readable(message),
        };
        let spoken = plain_text(&utterance.message);
        // 私聊走 user_id：拿内部窗口键当 channel 发，实现端会把它当成一个不存在的群。
        let (target_group, target_user) = if is_direct(group) {
            (None, direct_user(group).filter(|user| !user.is_empty()))
        } else {
            (Some(group), None)
        };
        if is_direct(group) && target_user.is_none() {
            warn!(target: LOG_TARGET, "{group} 的对方身份已丢失，这句话没发出去");
            break;
        }
        let id = match send_fresh_msg_id(
            ctx,
            writer.clone(),
            target_group,
            target_user.as_deref(),
            &message,
            freshness_for(scope_channel(group), config.freshness_window()),
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

impl PluginConfig for AmbientConfig {
    const NAME: &'static str = "ambient";

    fn check(&self) -> Result<(), String> {
        if !matches!(self.talk.trim(), "quiet" | "normal" | "lively") {
            return Err("talk 只能是 quiet（安静）、normal（适中）或 lively（活跃）".into());
        }
        if !(1..=6).contains(&self.busy_factor) {
            return Err("busy_factor 要在 1 到 6 之间".into());
        }
        if !(1..=6).contains(&self.quiet_factor) {
            return Err("quiet_factor 要在 1 到 6 之间".into());
        }
        if !(1..=120).contains(&self.busy_rate) {
            return Err("busy_rate 要在 1 到 120 之间（每分钟几条）".into());
        }
        if !(60..=3_600).contains(&self.gate_interval_max) {
            return Err("gate_interval_max 要在 60 到 3600 秒之间".into());
        }
        if self.burst_look > 200 {
            return Err("burst_look 最多 200（0 = 关闭）".into());
        }
        if self.burst_min_gap_seconds > 300 {
            return Err("burst_min_gap_seconds 最多 300".into());
        }
        if self.assist.newcomer_within_seconds > 3_600 {
            return Err("assist.newcomer_within_seconds 最多 3600".into());
        }
        if !(4..=64).contains(&self.learning.min_messages) {
            return Err("learning.min_messages 要在 4 到 64 条之间".into());
        }
        if !(60..=10_080).contains(&self.learning.interval_minutes) {
            return Err("learning.interval_minutes 要在 60 到 10080 分钟之间".into());
        }
        if !(15..=900).contains(&self.learning.idle_seconds) {
            return Err("learning.idle_seconds 要在 15 到 900 秒之间".into());
        }
        Ok(())
    }
}

pub const LABELS: &[(&str, &str)] = &[
    ("enabled", "总开关"),
    ("groups", "搭话的群"),
    ("talk", "话量"),
    ("private", "私聊也用这个账号聊天"),
    ("private_users", "只对这些好友开私聊"),
    ("management_groups", "直接开放管理的群"),
    ("management_auto", "账号是管理员时自动管群"),
    ("management_excluded", "永远不管的群"),
    ("management_daily", "每天最多管几次"),
    ("gate_model", "判定模型"),
    ("gate_persona", "判定用的兴趣画像"),
    ("reply_model", "发言模型"),
    ("thinking", "思考强度"),
    ("temperature", "采样温度"),
    ("help_thinking", "答疑时的思考强度"),
    ("help_model", "答疑时的发言模型"),
    ("breakthrough_per_hour", "每小时紧急突破几次"),
    ("tools", "发言时可用的工具"),
    ("score_threshold", "开口门槛"),
    ("silence_relief_per_10min", "每沉默十分钟降几分"),
    ("silence_relief_cap", "沉默补偿的上限"),
    ("speech_penalty_per_turn", "每说过一轮加几分"),
    ("speech_penalty_cap", "加价的上限"),
    ("focus_relief", "接着聊时的折扣"),
    ("context_turns", "上下文条数"),
    ("context_images", "附几张图"),
    ("debounce_seconds", "安静几秒算一阵"),
    ("max_pending_seconds", "最多等多久"),
    ("gate_interval_seconds", "看群间隔"),
    ("adaptive_gate", "按热闹程度调节"),
    ("busy_rate", "算「忙」的门槛"),
    ("busy_factor", "忙时缩短到几分之一"),
    ("quiet_factor", "闲时拉长到几倍"),
    ("gate_interval_max", "最长隔多久看"),
    ("burst_look", "攒够几条提前看"),
    ("burst_min_gap_seconds", "两次提前看至少隔"),
    ("assist", "群管辅助"),
    ("assist.questions", "有人提问时提前看"),
    ("assist.newcomers", "新人进群打个招呼"),
    ("assist.newcomer_within_seconds", "进群多久内还打招呼"),
    ("cooldown_seconds", "开口后的冷却"),
    ("cooldown_penalty", "冷却期门槛加价"),
    ("focus_max_seconds", "关注最多多久"),
    ("max_per_hour", "每小时目标轮数"),
    ("budget_penalty", "超出后每轮加几分"),
    ("reply_on_mention", "被 @ 时直接开口"),
    ("aliases", "别名"),
    ("preferred_name", "自我介绍用的称呼"),
    ("summon_command", "搭话指令"),
    ("memory_enabled", "记住群里的人和事"),
    ("mood_enabled", "按作息起伏"),
    ("life_enabled", "按钟点过日子"),
    ("memo_budget", "每轮最多写几条记忆"),
    ("search_enabled", "允许联网"),
    ("search_budget", "每轮联网几次"),
    ("peak", "计价高峰"),
    ("peak.mode", "高峰时怎么办"),
    ("peak.windows", "高峰时段"),
    ("peak.weekdays", "哪几天算高峰"),
    ("peak.holidays", "法定节假日"),
    ("peak.model", "高峰用的模型"),
    ("peak.swap_reply", "发言也换模型"),
    ("peak.doze_gate_seconds", "睡着时隔多久看一眼"),
    ("peak.doze_reply_limit", "睡着时每小时最多开口"),
    ("reflect_enabled", "定期复盘群聊"),
    ("learning", "学习号主的说话方式"),
    ("learning.enabled", "自动学习说话方式"),
    ("learning.model", "归纳用的模型"),
    ("learning.min_messages", "攒够几条才学习"),
    ("learning.interval_minutes", "两次学习至少隔多久"),
    ("learning.idle_seconds", "空闲多久后才学习"),
    ("quick_per_hour", "每小时随口几句"),
    ("quick_floor", "随口一句的分数下限"),
    ("owner_quiet_seconds", "号主说话后安静多久"),
    ("owner_grace_seconds", "先等号主答多久"),
    ("send_freshness_seconds", "消息时效窗口"),
    ("qq_typing", "发前显示正在输入"),
    ("qq_mark_read", "看过后标记已读"),
    ("screenshot_on_suspicion", "被质疑时发聊天截图"),
    ("screenshot_cooldown_seconds", "截图冷却"),
    ("messages_budget", "一次最多几条"),
    ("split_chars", "多长换气"),
    ("long_text_chars", "多长排成图片"),
    ("actions_budget", "每轮最多几个动作"),
    ("draw_budget", "每轮最多画几张"),
    ("music_budget", "每轮最多写几首歌"),
    ("video_budget", "每轮最多拍几段视频"),
    ("sticker_max", "表情包最多留几张"),
    ("typing_cpm", "打字速度"),
    ("voice_cpm", "语音输入速度"),
    ("think_seconds", "思考时间"),
    ("gate_timeout_seconds", "判定超时"),
    ("reply_timeout_seconds", "发言超时"),
];

/// 下拉框里每个取值的说法。
pub fn option_labels() -> serde_json::Value {
    let thinking = serde_json::json!({
        "off": "不思考",
        "minimal": "很少",
        "low": "少",
        "medium": "中",
        "high": "高",
    });
    serde_json::json!({
        "talk": {
            "quiet": "安静：少开口，随口一句也少",
            "normal": "适中",
            "lively": "活跃：门槛低一点，每小时多聊几轮",
        },
        "thinking": thinking,
        "help_thinking": thinking,
        "peak.mode": {
            "normal": "照常，不理会时段",
            "swap": "换一个便宜模型照常跑",
            "sleep": "睡着，偶尔看一眼",
            "pause": "完全不说话",
        },
    })
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
        // 智能体代办的一条插件指令（见 oai::agent::command）：群里并没有人真的发出它。
        // 让它进窗口，人格下一眼就会把它当成一句待接的话——那不是群友说的，不该由它接。
        if ctx.agent_command {
            return Ok(Some(ctx));
        }
        observe(&ctx, &writer, &mgr, &base).await;
        Ok(Some(ctx))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn talk_scales_the_counts_without_touching_zeroes() {
        let mut config = AmbientConfig::default();
        assert_eq!(config.quick_hourly(), config.quick_per_hour);
        config.talk = "quiet".into();
        assert!(config.quick_hourly() < config.quick_per_hour.max(2));
        assert!(config.cooldown() > Duration::from_secs(config.cooldown_seconds));
        config.talk = "lively".into();
        assert!(config.scaled(config.max_per_hour) > config.max_per_hour);
        config.quick_per_hour = 0;
        assert_eq!(config.quick_hourly(), 0);
        config.talk = "loud".into();
        assert!(config.check().is_err());
    }

    fn config() -> AmbientConfig {
        let mut config = AmbientConfig::default();
        config.gate_interval_seconds = 90;
        config.gate_interval_max = 900;
        config
    }

    #[test]
    fn look_interval_follows_activity() {
        let mut config = config();
        // 没人说话：90 × 3 = 270 秒。
        assert_eq!(config.look_interval(0.0), Duration::from_secs(270));
        // 中间地带照基础间隔。
        assert_eq!(config.look_interval(3.0), Duration::from_secs(90));
        // 忙：默认不缩短（busy_factor = 1），照基础间隔。
        assert_eq!(config.look_interval(8.0), Duration::from_secs(90));
        // 调成 2 就跟得更紧：90 ÷ 2 = 45 秒。
        config.busy_factor = 2;
        assert_eq!(config.look_interval(8.0), Duration::from_secs(45));
    }

    #[test]
    fn quiet_interval_is_capped() {
        let mut config = config();
        config.quiet_factor = 6;
        config.gate_interval_max = 300;
        assert_eq!(config.look_interval(0.0), Duration::from_secs(300));
    }

    #[test]
    fn manual_mode_keeps_the_flat_interval() {
        let mut config = config();
        config.adaptive_gate = false;
        assert_eq!(config.look_interval(8.0), Duration::from_secs(90));
        assert_eq!(config.look_interval(0.0), Duration::from_secs(90));
    }

    #[test]
    fn questions_are_narrow() {
        assert!(looks_like_question("这个报错怎么修？"));
        assert!(looks_like_question("有没有人知道这是什么情况"));
        assert!(!looks_like_question("怎么会这样"));
        assert!(!looks_like_question("[群成员 123 加入了群聊]"));
    }

    #[test]
    fn private_scopes_are_gated_by_config() {
        let mut config = config();
        // 只开群：私聊频道不算开启。
        config.groups = vec!["123".into()];
        remember_direct("private:456", "456");
        assert!(scope_enabled(&config, "123"));
        assert!(!scope_enabled(&config, "private:456"));
        // 打开私聊、不限定人：任何已登记的私聊都开启。
        config.private = true;
        assert!(scope_enabled(&config, "private:456"));
        // 限定人之后只有名单上的那位。
        config.private_users = vec!["789".into()];
        assert!(!scope_enabled(&config, "private:456"));
        config.private_users = vec!["456".into()];
        assert!(scope_enabled(&config, "private:456"));
        // 私聊发送要拿对方 QQ 号，而不是窗口键。
        assert_eq!(direct_user("private:456").as_deref(), Some("456"));
        // 没登记过的频道不会被当成私聊。
        assert!(!is_direct("private:999"));
    }
}
