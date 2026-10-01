//! 群聊上下文的滚动窗口与每个群的发言节流状态。
//!
//! 搭话不像房间对话那样有明确的一问一答，模型要读的是「刚才这一段群聊」。
//! 窗口只存在内存里：重启后重新攒几条就够用，落库反而要为一个随时会被丢弃的
//! 上下文承担迁移与清理成本。
//!
//! 记消息的是 [`record`]（内置智能体插件在群消息过手时调用），搭话侧再用
//! [`GroupState::receive`] 把更全的那一份换进去——同一条消息只占一格。节流那几项
//! （`seq`／`running`／`focus`／发言时刻）只有搭话用，和消息记在同一把锁下是为了
//! 「收到消息」与「占用 worker」不会在两把锁之间交错；房间只读消息那部分。

use crate::event::MessageEvent;
use simd_json::base::ValueAsScalar;
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsArray, ValueObjectAccessAsScalar};
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::attention::Focus;

/// 每个群保留的消息条数上限。取值比 `context_turns` 宽一些，
/// 好让配置调大时不必等窗口重新攒满。
const WINDOW_CAPACITY: usize = 80;

/// 开过口之后多久之内还算「正聊着」：看群看得勤，见 [`GroupState::look_due`]。
const ENGAGED_AFTER_SPEAKING: Duration = Duration::from_secs(180);
/// 翻回来的记录里，自己隔这么近的几条算同一轮。
const ROUND_GAP_SECONDS: i64 = 90;
/// 正聊着的时候，看群的间隔是平时的几分之几。
const ENGAGED_LOOK: f32 = 0.3;

/// 自己翻回来的一条是不是「说话」：只剩媒体或卡片的那种多半是别的插件发的
/// （统计图、视频解析），不算人格开过口。
fn speech_like(text: &str) -> bool {
    let mut rest = text.to_string();
    for tag in ["[图片]", "[表情包]", "[视频]", "[语音]", "[合并转发]", "[文件]"] {
        rest = rest.replace(tag, "");
    }
    let rest = rest.trim();
    !rest.is_empty() && !rest.starts_with("[卡片") && !rest.starts_with("[合并转发")
}

/// 看着就急的说法。刻意收得窄：「急了」「你急什么」这种玩笑满群都是，不算。
const PRESSING: [&str; 14] = [
    "救命", "求救", "求助", "在线等", "急急急", "很急", "挺急", "紧急", "帮帮我",
    "被骗", "报警", "出事了", "有没有人", "有人在吗",
];

fn sounds_pressing(text: &str) -> bool {
    let lower = text.to_lowercase();
    PRESSING.iter().any(|word| lower.contains(word)) || lower.contains("help")
}

/// 「刚才说了几轮」的观察窗口。群聊的节奏以十分钟为单位看正合适：
/// 再短看不出是不是一直在接话，再长又会把半小时前的事算到现在头上。
pub(crate) const RECENT_SPEECH: std::time::Duration = std::time::Duration::from_secs(600);

/// 号主亲手插话时给 `seq` 加多少：比搭话能容忍的「打字期间新来几条」大，保证在途草稿作废。
const OWNER_DRIFT: u64 = 4;

/// 一条消息「冲着谁来的」：@、引用，还是戳。
///
/// 三者在群里是三种不同的动作，接法也不一样——被 @ 是要你答话，被引用多半是追问
/// 或吐槽你刚说的那句，被戳则是逗你。合成一个布尔值递进去，人格只能一律当作
/// 「有人在叫我」，接出来的话就没有分寸。引用到的那条原话也一起记下来：人看见
/// 「引用」是能直接看见被引内容的，模型也该看见。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Call {
    /// @ 了我。
    pub at_me: bool,
    /// 引用了我发过的消息。
    pub replied_me: bool,
    /// 戳了我。
    pub poked_me: bool,
    /// 直接叫了我的名字（名片、账号昵称或 `aliases` 里的小名）。
    ///
    /// 与前三者不同，这一条是猜出来的而不是协议里的明码，所以它只加一个记号，
    /// 不进 [`Call::mine`]——见 [`super::identity::called_by_name`]。
    pub named_me: bool,
    /// 这条消息引用了哪条消息；空串表示没有引用。
    pub reply_to: String,
    /// 被引用那条的摘要（`谁：说了什么`）；不在窗口里时为空。
    pub quote: String,
}

impl Call {
    /// 这一条是不是冲着我来的。
    pub(crate) fn mine(&self) -> bool {
        self.at_me || self.replied_me || self.poked_me
    }
}

/// 群聊上下文里的一条消息。
#[derive(Clone, Debug, Default)]
pub(crate) struct Turn {
    pub user_id: String,
    pub name: String,
    pub text: String,
    /// 图片直链，供多模态判定使用。
    pub images: Vec<String>,
    /// 保留资源和引用参数，供工具按消息 ID 复用。
    pub elements: crate::message::Message,
    pub message_id: String,
    /// 是否 @ 了机器人自己。
    pub mentions_me: bool,
    /// 被叫到的细节：哪一种动作、引用的是哪条。
    pub call: Call,
    /// 是否是机器人自己说的话。
    pub from_me: bool,
    /// 这句是号主本人在 QQ 客户端里亲手打的（`manual_self`），不是机器人发的。
    ///
    /// 两者共用同一个账号，`from_me` 分不开。只有实时到达的事件带得出这个标记；
    /// 重启后从平台翻回来的旧记录没有，一律当作不是。
    pub manual: bool,
    /// Unix 秒。
    pub at: i64,
}

/// 一个群的上下文与节流状态。
#[derive(Default)]
pub(crate) struct GroupState {
    turns: VecDeque<Turn>,
    /// 收到消息就自增，防抖任务据此判断「还在刷屏」。
    pub seq: u64,
    /// 已经有一个防抖任务在等这个群。
    pub running: bool,
    /// 只消费本批次的点名；人格选择沉默后不反复拿旧 @ 强制唤醒。
    unread_mention: bool,
    /// 本批次收到过搭话指令；与点名一样只消费一次。
    unread_summon: bool,
    pub focus: Option<Focus>,
    /// 最近一次发言时刻。
    pub last_spoke: Option<Instant>,
    /// 自己上次开口的墙上时刻（Unix 秒），等着看多久有人接。
    spoke_at: Option<i64>,
    /// 一次性的反馈：上次开口之后隔了多少秒才有人再说话。
    feedback: Option<i64>,
    /// 近期发言时刻，用于每小时上限。
    spoken: VecDeque<Instant>,
    /// 睡着（计价高峰）时上一次主动判定的时刻：把自主判定压到隔一段时间一次。
    doze_gate_at: Option<Instant>,
    /// 上一次「扫一眼群」（主动判定）的时刻；见 [`GroupState::look_due`]。
    looked_at: Option<Instant>,
    /// 上一眼看到的最新一条消息号：它之后的就是这一眼新看到的。
    looked_id: String,
    /// 这一次隔多久再看的随机倍数。人看手机没有节拍器。
    look_jitter: f32,
    /// 进程起来之后是否已经从平台翻过这个群的聊天记录。
    pub hydrated: bool,
    /// 睡着时自主开口的时刻，用于每小时上限——判定便宜、开口贵，这条管的是后者。
    doze_spoken: VecDeque<Instant>,
    /// 紧急突破的时刻，用于每小时上限：突破是给真出事的时候留的，不能被刷成常态。
    breakthroughs: VecDeque<Instant>,
    /// Opt-in chat screenshot gag: at most once per group per cooldown window.
    last_screenshot: Option<Instant>,
    last_screenshot_message: String,
    /// 号主本人上一次在这个群里亲手打字的时刻。
    owner_at: Option<Instant>,
    /// 号主亲手打过几条（只增不减）：等他答话时靠它看「他有没有开口」。
    owner_marks: u64,
    /// 随口一句的时刻，用于每小时上限。
    quick: VecDeque<Instant>,
}

impl GroupState {
    pub(crate) fn push(&mut self, turn: Turn) {
        if self.turns.len() >= WINDOW_CAPACITY {
            self.turns.pop_front();
        }
        self.turns.push_back(turn);
    }

    /// 最近 `count` 条消息，按时间正序。
    pub(crate) fn recent(&self, count: usize) -> Vec<Turn> {
        self.turns
            .iter()
            .skip(self.turns.len().saturating_sub(count))
            .cloned()
            .collect()
    }

    /// 窗口里最后一条消息是不是自己说的——自言自语要及时打住。
    /// 收消息与占用 worker 必须在同一把锁下完成，避免交接时漏消息。
    ///
    /// 同一个 `message_id` 的第二次投递（先由能力层记下现场、搭话侧再补记号）
    /// 只换内容，不动节流状态，也不重新唤醒 worker。
    pub(crate) fn receive(&mut self, turn: Turn) -> bool {
        let fresh = turn.message_id.is_empty()
            || !self
                .turns
                .iter()
                .any(|old| old.message_id == turn.message_id);
        let incoming = !turn.from_me;
        if fresh && turn.manual {
            // 本人亲自上场了：他在看这个群，手里准备好的话和没处理的点名都归他。
            // 让位的细则在 [`GroupState::owner_active`]；这里让在途的草稿作废。
            self.owner_at = Some(Instant::now());
            self.owner_marks += 1;
            self.unread_mention = false;
            self.seq += OWNER_DRIFT;
        }
        if fresh && incoming {
            self.seq += 1;
            self.unread_mention |= turn.mentions_me;
            // 说完之后第一个开口的人，决定这次发言是被接住了还是掉地上了。
            if let Some(spoke_at) = self.spoke_at.take() {
                self.feedback = Some((turn.at - spoke_at).max(0));
            }
        }
        self.record(turn);
        if !fresh || !incoming || self.running {
            return false;
        }
        self.running = true;
        true
    }

    /// 记一条消息进窗口；同一条消息再来一次就替换那一格。
    ///
    /// 先记下的往往是「现场」（谁在什么时候说了什么），后到的可能带着更多记号
    /// （引用原话、是不是叫了你的名字）。返回 true 表示这是一条没见过的消息。
    pub(crate) fn record(&mut self, turn: Turn) -> bool {
        if !turn.message_id.is_empty()
            && let Some(old) = self
                .turns
                .iter_mut()
                .find(|old| old.message_id == turn.message_id)
        {
            *old = turn;
            return false;
        }
        self.push(turn);
        true
    }

    pub(crate) fn recall(&mut self, id: &str) {
        if let Some(turn) = self
            .turns
            .iter_mut()
            .find(|turn| !id.is_empty() && turn.message_id == id)
        {
            turn.text = "[消息已撤回]".into();
            turn.images.clear();
            turn.elements = crate::message::Message::new();
            // 不再允许引用、转发或再次撤回这个 ID。
            turn.message_id.clear();
        }
    }

    pub(crate) fn allow_screenshot(&self, cooldown_seconds: u64) -> bool {
        self.last_screenshot
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(cooldown_seconds.max(3600)))
    }

    pub(crate) fn mark_screenshot(&mut self, message_id: &str) {
        self.last_screenshot = Some(Instant::now());
        self.last_screenshot_message = message_id.to_string();
    }

    pub(crate) fn screenshot_seen(&self, message_id: &str) -> bool {
        !message_id.is_empty() && self.last_screenshot_message == message_id
    }

    pub(crate) fn take_mention(&mut self) -> bool {
        std::mem::take(&mut self.unread_mention)
    }

    /// 收到一条搭话指令。
    ///
    /// 指令本身没有内容可给模型看，所以它不进窗口，只说明「这一批欠一句回应」；
    /// 若那条消息还带着正文，由调用方按普通消息另行 `receive`。
    /// 返回 true 表示这个群现在没有 worker，调用方去跑一次 `consider`。
    pub(crate) fn summon(&mut self) -> bool {
        self.seq += 1;
        self.unread_summon = true;
        if self.running {
            return false;
        }
        self.running = true;
        true
    }

    pub(crate) fn take_summon(&mut self) -> bool {
        std::mem::take(&mut self.unread_summon)
    }

    /// 新消息即使出现在模型执行或发送期间，也由同一 worker 接着处理。
    pub(crate) fn finish_batch(&mut self, processed: u64) -> bool {
        if self.seq != processed {
            true
        } else {
            self.running = false;
            false
        }
    }

    pub(crate) fn active_focus(&self) -> Option<&Focus> {
        self.focus
            .as_ref()
            .filter(|focus| focus.until > Instant::now())
    }

    pub(crate) fn rhythm(&mut self) -> String {
        let count = self.spoken_last_hour();
        let recent = self.spoken_within(RECENT_SPEECH);
        let since = self.last_spoke.map_or("尚未发言".to_string(), |at| {
            format!("{} 秒前发过言", at.elapsed().as_secs())
        });
        let focus = self
            .active_focus()
            .map_or("无，按兴趣旁观".to_string(), |focus| {
                format!(
                    "群友 {:?}；话题 {}；还关注 {} 秒",
                    focus.users,
                    focus.topic,
                    focus
                        .until
                        .saturating_duration_since(Instant::now())
                        .as_secs()
                )
            });
        let crowding = match recent {
            0 => "",
            1 => "刚接过一轮，这一轮交给别人也正好。",
            _ => "最近这十分钟已经由你说了好几轮，这会儿看着就好。",
        };
        format!(
            "你{since}，最近十分钟发言 {recent} 轮，近一小时 {count} 轮。{crowding}\
             当前关注：{focus}。关注是给自己留个念想，接不接随你；有新意又还在继续的对话最值得接。"
        )
    }

    /// 窗口里被引用的那条消息，返回「是不是我自己说的」与一句摘要。
    ///
    /// 摘要是给模型读的：群里的引用显示的是被引原话，模型也该看见，而不是一个
    /// 光秃秃的消息号。引用的是自己发的那条时换成「你说的」，好让它知道这是在
    /// 追问或吐槽它刚说的话。
    pub(crate) fn quote_of(&self, id: &str) -> Option<(bool, String)> {
        if id.is_empty() {
            return None;
        }
        self.turns
            .iter()
            .find(|turn| turn.message_id == id)
            .map(|turn| (turn.from_me, summarize(&turn.text, QUOTE_PREVIEW_CHARS)))
    }

    /// 取走「上次开口多久才有人接」，只取一次。
    pub(crate) fn take_feedback(&mut self) -> Option<i64> {
        self.feedback.take()
    }

    /// 记一次发言，同时淘汰一小时之前的记录。
    pub(crate) fn mark_spoke(&mut self) {
        let now = Instant::now();
        self.last_spoke = Some(now);
        self.spoke_at = Some(chrono::Local::now().timestamp());
        self.feedback = None;
        self.spoken.push_back(now);
        self.prune(now);
    }

    /// 最近一小时内的发言次数。
    pub(crate) fn spoken_last_hour(&mut self) -> usize {
        self.prune(Instant::now());
        self.spoken.len()
    }

    /// 最近这段时间里说了几轮。刚说过好几句的人本来就该消停一会儿。
    pub(crate) fn spoken_within(&self, window: std::time::Duration) -> usize {
        let now = Instant::now();
        self.spoken
            .iter()
            .filter(|at| now.duration_since(**at) < window)
            .count()
    }

    fn prune(&mut self, now: Instant) {
        while let Some(first) = self.spoken.front() {
            if now.duration_since(*first).as_secs() >= 3_600 {
                self.spoken.pop_front();
            } else {
                break;
            }
        }
    }

    /// 睡着时到了可以主动看一眼的时候吗。到了就在同一把锁里记下这一刻，
    /// 让同一段时间内到达的其他批次都跳过——判定是最频繁的那次调用。
    /// `interval` 为 0 表示关掉自主判定。
    pub(crate) fn allow_doze_gate(&mut self, interval: Duration) -> bool {
        if interval.is_zero() {
            return false;
        }
        let now = Instant::now();
        if self
            .doze_gate_at
            .is_some_and(|at| now.duration_since(at) < interval)
        {
            return false;
        }
        self.doze_gate_at = Some(now);
        true
    }

    /// 还要多久才到下一次「扫一眼群」。
    ///
    /// 人不是一直盯着群的：隔一会儿拿起手机看一眼，一眼看到的是一串消息。从前每阵
    /// 消息一停就判一次，热闹的群里等于每句话都在它眼皮底下过一遍，于是每摊都想
    /// 插一句。现在没人叫它的时候按 `interval` 上下浮动地看；正聊在兴头上（在关注、
    /// 或者刚开过口）时看得勤，约三分之一的间隔——那时候人本来就捧着手机。
    pub(crate) fn look_due(&self, interval: Duration) -> Duration {
        let Some(at) = self.looked_at else {
            return Duration::ZERO;
        };
        let scale = if self.engaged() {
            ENGAGED_LOOK
        } else if self.look_jitter > 0.0 {
            self.look_jitter
        } else {
            1.0
        };
        interval.mul_f32(scale).saturating_sub(at.elapsed())
    }

    /// 记一次「看过了」：这一眼之前的消息都算看过，下一眼隔多久重新掷一次。
    pub(crate) fn mark_look(&mut self) {
        self.looked_at = Some(Instant::now());
        self.looked_id = self
            .turns
            .iter()
            .rev()
            .find(|turn| !turn.message_id.is_empty())
            .map(|turn| turn.message_id.clone())
            .unwrap_or_default();
        self.look_jitter = 0.6 + rand::random::<f32>() * 0.9;
    }

    /// 上一眼之后又来了几条群友的消息。
    ///
    /// 按位置数，不按消息号大小：窗口里的顺序就是到达顺序，消息号未必处处递增。
    /// 上一眼看到的那条已经滚出窗口（或者从没看过）时，整个窗口都算新的。
    pub(crate) fn unseen(&self) -> usize {
        let mut count = 0;
        for turn in self.turns.iter().rev() {
            if !self.looked_id.is_empty() && turn.message_id == self.looked_id {
                break;
            }
            if !turn.from_me {
                count += 1;
            }
        }
        count
    }

    /// 正聊在兴头上：在关注某个人或话题，或者几分钟内刚开过口。
    pub(crate) fn engaged(&self) -> bool {
        self.active_focus().is_some()
            || self
                .last_spoke
                .is_some_and(|at| at.elapsed() < ENGAGED_AFTER_SPEAKING)
    }

    /// 有人在叫它：@、引用、戳、搭话指令，或者上一眼之后有人喊了它的名字。
    /// 这些不等下一眼——手机会亮，或者名字本来就扎眼。
    pub(crate) fn urgent(&self) -> bool {
        self.unread_mention
            || self.unread_summon
            || self.pressing()
            || self
                .turns
                .iter()
                .rev()
                .take_while(|turn| self.looked_id.is_empty() || turn.message_id != self.looked_id)
                .any(|turn| !turn.from_me && turn.call.named_me)
    }

    /// 上一眼之后有没有「看着就急」的话：救命、在线等、被骗……
    ///
    /// 只是个便宜的本地信号，让这一阵不等下一眼、立刻交给判定去认真估；真要不要
    /// 破例开口，还是判定说了算（见 `Verdict::urgent`）。
    pub(crate) fn pressing(&self) -> bool {
        self.turns
            .iter()
            .rev()
            .take_while(|turn| self.looked_id.is_empty() || turn.message_id != self.looked_id)
            .any(|turn| !turn.from_me && sounds_pressing(&turn.text))
    }

    /// 这一小时还能不能紧急突破一次；能就记下这一次。
    pub(crate) fn allow_breakthrough(&mut self, per_hour: usize) -> bool {
        let now = Instant::now();
        while self
            .breakthroughs
            .front()
            .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(3_600))
        {
            self.breakthroughs.pop_front();
        }
        if self.breakthroughs.len() >= per_hour {
            return false;
        }
        self.breakthroughs.push_back(now);
        true
    }

    /// 最近一小时随口回过几次。
    pub(crate) fn quick_last_hour(&mut self) -> usize {
        let now = Instant::now();
        while self
            .quick
            .front()
            .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(3_600))
        {
            self.quick.pop_front();
        }
        self.quick.len()
    }

    /// 记一次随口一句。
    pub(crate) fn mark_quick(&mut self) {
        self.quick.push_back(Instant::now());
    }

    /// 窗口里最新一条消息：是不是自己的、多久以前（秒）。空窗口是 `None`。
    pub(crate) fn newest(&self, now: i64) -> Option<(bool, i64)> {
        self.turns
            .back()
            .map(|turn| (turn.from_me, (now - turn.at).max(0)))
    }

    /// 号主本人在 `within` 之内亲手在这个群里打过字。
    ///
    /// 账号是他的，机器人只是他不在时替他蹲着：他自己在群里说话的时候，机器人再
    /// 插话就是同一个账号两个人在抢话，群友已经为此开过「被夺舍了」的玩笑。
    pub(crate) fn owner_active(&self, within: Duration) -> bool {
        !within.is_zero() && self.owner_at.is_some_and(|at| at.elapsed() < within)
    }

    /// 号主亲手打过的条数，等他答话时用来判断「他开口了没有」。
    pub(crate) fn owner_marks(&self) -> u64 {
        self.owner_marks
    }

    /// 还没被取走的搭话指令（人按下的键，不等号主）。
    pub(crate) fn has_unread_summon(&self) -> bool {
        self.unread_summon
    }

    /// 还没被取走的点名：打字的工夫里有人 @ 了它，这一句就得重新想。
    pub(crate) fn has_unread_mention(&self) -> bool {
        self.unread_mention
    }

    /// 从 `seq` 那一刻起又来了多少条（群友消息、戳一戳、搭话指令都算一条）。
    pub(crate) fn drift(&self, seq: u64) -> u64 {
        self.seq.saturating_sub(seq)
    }

    /// 把从平台翻回来的旧消息垫到窗口前面。
    ///
    /// 窗口只在内存里，重启就空了：刚重启的那一轮，人格看不到自己五分钟前说过的话，
    /// 于是前脚认了「是真的」，后脚又问「咋了」（线上 2026-09-26 10:36）。翻回来的
    /// 记录按消息号去重、按时间排好，自己说过的那几句顺带把「这一小时说了几轮」的
    /// 账补回来——否则每次重启都等于把发言额度清零。
    pub(crate) fn seed(&mut self, history: Vec<Turn>) -> (usize, usize) {
        let mut added = 0;
        let mut mine = 0;
        let now = chrono::Local::now().timestamp();
        // 账记的是「轮」不是「条」：一轮里分两三条发的，翻回来是挨着的几条。
        let mut last_round: Option<i64> = None;
        let mut history = history;
        history.sort_by(|a, b| (a.at, &a.message_id).cmp(&(b.at, &b.message_id)));
        for turn in history {
            if turn.message_id.is_empty()
                || self
                    .turns
                    .iter()
                    .any(|old| old.message_id == turn.message_id)
            {
                continue;
            }
            // 号主亲手打的不算机器人开过口：它不占「这一小时说了几轮」的账，
            // 却说明他刚才在场——按那条消息离现在多久，把「本人在场」的钟补回来。
            if turn.manual {
                let age = Duration::from_secs((now - turn.at).max(0) as u64);
                if let Some(at) = Instant::now().checked_sub(age) {
                    self.owner_at = self.owner_at.max(Some(at));
                }
            } else if turn.from_me
                && speech_like(&turn.text)
                && now - turn.at < 3_600
                && last_round.is_none_or(|round| turn.at - round > ROUND_GAP_SECONDS)
            {
                last_round = Some(turn.at);
                if let Some(at) = Instant::now()
                    .checked_sub(Duration::from_secs((now - turn.at).max(0) as u64))
                {
                    self.spoken.push_back(at);
                    self.last_spoke = self.last_spoke.max(Some(at));
                }
                mine += 1;
            }
            self.turns.push_back(turn);
            added += 1;
        }
        self.turns
            .make_contiguous()
            .sort_by(|a, b| (a.at, &a.message_id).cmp(&(b.at, &b.message_id)));
        while self.turns.len() > WINDOW_CAPACITY {
            self.turns.pop_front();
        }
        self.spoken.make_contiguous().sort();
        self.hydrated = true;
        (added, mine)
    }

    /// 睡着时最近一小时自主开口了几次。
    pub(crate) fn doze_spoke_last_hour(&mut self) -> usize {
        let now = Instant::now();
        while let Some(first) = self.doze_spoken.front() {
            if now.duration_since(*first).as_secs() >= 3_600 {
                self.doze_spoken.pop_front();
            } else {
                break;
            }
        }
        self.doze_spoken.len()
    }

    /// 记一次睡着时的自主开口。
    pub(crate) fn mark_doze_spoke(&mut self) {
        self.doze_spoken.push_back(Instant::now());
    }
}

/// 引用摘要的字数上限。引用是打断句用的，长了会把上下文撑散。
const QUOTE_PREVIEW_CHARS: usize = 40;

/// 把一条消息压成单行的短摘要。
fn summarize(text: &str, limit: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        return flat;
    }
    let head: String = flat.chars().take(limit).collect();
    format!("{head}…")
}

/// 群聊窗口 → 交给模型阅读的聊天记录。
///
/// 带上 QQ 号是为了让回复能 `[at:]` 到人；带上时刻是为了让模型知道哪些话已经
/// 凉了——隔了二十分钟的梗再接就不叫接梗了。被叫到的原因也一并标出来：被 @、
/// 被引用、被戳对应三种接法，混成一句「有人叫你」，接出来的话就没有分寸。
pub(crate) fn transcript(turns: &[Turn]) -> String {
    let mut out = String::new();
    for turn in turns {
        let clock = chrono::DateTime::from_timestamp(turn.at, 0)
            .map(|time| {
                time.with_timezone(&chrono::Local)
                    .format("%H:%M")
                    .to_string()
            })
            .unwrap_or_else(|| "--:--".to_string());
        let who = if turn.from_me && turn.user_id.is_empty() {
            "平台事件（操作者未知）".to_string()
        } else if turn.manual {
            "你自己（亲手打的）".to_string()
        } else if turn.from_me {
            "你自己".to_string()
        } else {
            format!("{}({})", turn.name, turn.user_id)
        };
        out.push_str(&format!("[{clock} id={}] {who}: ", turn.message_id));
        out.push_str(turn.text.trim());
        if !turn.images.is_empty() {
            out.push_str(&format!("〔图片 ×{}〕", turn.images.len()));
        }
        if turn.call.at_me {
            out.push_str("〔@了你〕");
        }
        if turn.call.replied_me {
            out.push_str("〔引用了你的消息〕");
        }
        if turn.call.poked_me {
            out.push_str("〔戳了你〕");
        }
        if turn.call.named_me {
            out.push_str("〔叫了你的名字〕");
        }
        if !turn.call.quote.is_empty() {
            out.push_str(&format!("〔引用 {}〕", turn.call.quote));
        }
        out.push('\n');
    }
    out
}

/// 把「引用了哪条」解析成「谁说了什么」。
///
/// 群聊里的引用是连着原话一起显示的，人格看到的记录也该带上这句；解析到引用的
/// 是自己发过的消息时，等于有人点了它的名，与 @ 一样直接把它叫醒。
pub(crate) fn resolve_quote(turn: &mut Turn, lookup: impl FnOnce(&str) -> Option<(bool, String)>) {
    if turn.call.reply_to.is_empty() {
        return;
    }
    let Some((replied_me, quote)) = lookup(&turn.call.reply_to) else {
        return;
    };
    if replied_me {
        turn.mentions_me = true;
        turn.call.replied_me = true;
    }
    turn.call.quote = quote;
}

/// 事件 → 窗口里的一条消息。
///
/// 能力层与搭话都从这里进窗口，所以它只做「现场」那一层：谁在什么时候说了什么。
/// 点名与引用是不是冲着人格来的，由调用方在自己那一侧补。
pub(crate) fn turn_from(event: &MessageEvent<'_>, me: &str) -> Turn {
    let mut text = String::new();
    let mut images = Vec::new();
    let mut mentions_me = false;
    let mut call = Call::default();
    // 平台在 at 段后面又跟着一条「@名字 正文」的文本段，那个 `@名字` 是 QQ 客户端的
    // 显示方式，不是群友打的字。留着它，人格就会学着写 `@某某`（线上记录 id 61150 的
    // 「@子屿 什么样不行」），而它能发得出去的写法只有 `[at:QQ号]`；摘掉那个 `@`，
    // 名字本身不动——多字昵称没法猜到哪里为止，宁可留个名字也不啃掉半截。
    let mut after_at = false;
    if let Some(segments) = event.0.get_array("message") {
        for segment in segments {
            let kind = segment.get_str("type").unwrap_or_default();
            // 这一条是不是紧跟在 at 段后面——是，才轮到上面那条规则。
            let follows_at = std::mem::replace(&mut after_at, kind == "at");
            let Some(data) = segment.get("data") else {
                continue;
            };
            match kind {
                "text" => {
                    let body = data.get_str("text").unwrap_or_default();
                    text.push_str(if follows_at {
                        body.strip_prefix('@').unwrap_or(body)
                    } else {
                        body
                    });
                }
                "at" => {
                    let target = data.get_str("qq").unwrap_or_default();
                    if !me.is_empty() && target == me {
                        mentions_me = true;
                        call.at_me = true;
                        text.push_str("@我 ");
                    } else {
                        // 用与发言同一种写法渲染：人格照着眼前的记录写话，记录里写成
                        // `@QQ号`，它就会把这串号码原样抄进正文（记录 id 105888）。
                        text.push_str(&format!("[at:{target}] "));
                    }
                }
                "image" | "mface" => {
                    if let Some(url) = data
                        .get("url")
                        .or_else(|| data.get("file"))
                        .and_then(|value| value.as_str())
                        .filter(|url| url.starts_with("http"))
                    {
                        images.push(url.to_string());
                    }
                    if kind == "mface" {
                        text.push_str(&crate::adapters::satori::forward::mface_label(
                            data.get_str("summary").unwrap_or(""),
                        ));
                    } else if crate::adapters::satori::forward::is_sticker_picture(
                        data.get("sub_type"),
                    ) {
                        text.push_str("[表情包]");
                    } else {
                        text.push_str("[图片]");
                    }
                }
                "face" => text.push_str(&format!("[表情:{}]", data.get_str("id").unwrap_or("?"))),
                "record" => text.push_str("[语音]"),
                "video" => text.push_str("[视频]"),
                "reply" => {
                    // 引用了哪条先记下来，等窗口在手里时再解析成「谁：说了什么」。
                    let id = data.get_str("id").unwrap_or_default();
                    call.reply_to = id.to_string();
                    text.push_str(&format!("[引用:{}] ", if id.is_empty() { "?" } else { id }));
                }
                "file" => text.push_str(&format!(
                    "[文件:{}]",
                    data.get_str("name").unwrap_or("未命名")
                )),
                "json" => {
                    // QQ 的分享/小程序卡是 JSON 段；若只留在原始 elements，搭话与
                    // agent 房间的窗口都看不到卡片。落地地址复用指令侧已核过的提取规则。
                    let payload = data
                        .get_str("data")
                        .or_else(|| data.get_str("content"))
                        .unwrap_or("");
                    text.push_str("[卡片");
                    if let Some(url) = crate::command::card_target_url(payload).filter(|url| {
                        (url.starts_with("http://") || url.starts_with("https://"))
                            && url.len() <= 2048
                    }) {
                        text.push_str(": ");
                        text.push_str(&url);
                    }
                    text.push(']');
                }
                "forward" | "node" => text.push_str("[合并转发，可用 satori_read 展开]"),
                "poke" => text.push_str("[戳一戳]"),
                "dice" => text.push_str("[骰子]"),
                "rps" => text.push_str("[猜拳]"),
                _ => {}
            }
        }
    }
    if text.trim().is_empty() && !images.is_empty() {
        text = "[图片]".to_string();
    }
    // 记录里一条消息占一行，换行与连续空格都压平，免得多行消息把上下文撑散。
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");

    Turn {
        user_id: event.user_id().to_string(),
        name: event.sender_name().to_string(),
        text,
        images,
        elements: event
            .0
            .get("message")
            .and_then(|v| simd_json::serde::from_owned_value(v.clone()).ok())
            .unwrap_or_default(),
        message_id: event.message_id().to_string(),
        mentions_me,
        call,
        from_me: !me.is_empty() && event.user_id() == me,
        manual: !me.is_empty() && event.user_id() == me && event.is_manual_self(),
        at: event
            .0
            .get_i64("time")
            .unwrap_or_else(|| chrono::Local::now().timestamp()),
    }
}

/// 平台存的那条消息（`message.get` 与 `message.list` 回的是同一种形状）→ 现场里的一条。
///
/// 房间没有常驻窗口，要看某条消息时就是拿它换回来的。正文是 satori XML，
/// 走与窗口里同一条渲染，模型不必学第二种读法。
pub(crate) fn turn_from_platform(
    ctx: &crate::event::Context,
    writer: &crate::adapters::satori::LockedWriter,
    item: &serde_json::Value,
) -> Option<Turn> {
    let message_id = item["id"].as_str().filter(|id| !id.is_empty())?.to_string();
    let user_id = item["user"]["id"].as_str().unwrap_or("").to_string();
    let name = item["member"]["nick"]
        .as_str()
        .filter(|nick| !nick.is_empty())
        .or_else(|| item["user"]["name"].as_str())
        .unwrap_or("")
        .to_string();
    let elements = crate::adapters::satori::message::from_content_with(
        item["content"].as_str().unwrap_or(""),
        &writer.resources(),
    );
    let images = elements
        .0
        .iter()
        .filter(|segment| matches!(segment.type_.as_str(), "image" | "mface"))
        .filter_map(|segment| {
            segment
                .data
                .get("url")
                .or_else(|| segment.data.get("file"))
                .and_then(|value| value.as_str())
                .filter(|url| url.starts_with("http"))
                .map(str::to_string)
        })
        .collect();
    let me = ctx.bot.self_id();
    Some(Turn {
        from_me: !user_id.is_empty() && user_id == me,
        user_id,
        name,
        text: crate::adapters::satori::forward::describe(&elements),
        images,
        elements,
        message_id,
        at: item["created_at"]
            .as_i64()
            .map(|millis| millis / 1000)
            .unwrap_or_else(|| chrono::Local::now().timestamp()),
        ..Turn::default()
    })
}

fn states() -> &'static Mutex<HashMap<String, GroupState>> {
    static STATES: OnceLock<Mutex<HashMap<String, GroupState>>> = OnceLock::new();
    STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 取出锁访问某个群的状态。闭包里不要 await——锁是同步的。
pub(crate) fn with_group<T>(group_id: &str, action: impl FnOnce(&mut GroupState) -> T) -> T {
    let mut guard = states().lock().unwrap_or_else(|error| error.into_inner());
    action(guard.entry(group_id.to_string()).or_default())
}
