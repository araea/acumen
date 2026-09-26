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

/// 「刚才说了几轮」的观察窗口。群聊的节奏以十分钟为单位看正合适：
/// 再短看不出是不是一直在接话，再长又会把半小时前的事算到现在头上。
pub(crate) const RECENT_SPEECH: std::time::Duration = std::time::Duration::from_secs(600);

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
    /// 这条消息引用了哪条消息；0 表示没有引用。
    pub reply_to: i64,
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
    pub user_id: i64,
    pub name: String,
    pub text: String,
    /// 图片直链，供多模态判定使用。
    pub images: Vec<String>,
    /// 保留资源和引用参数，供工具按消息 ID 复用。
    pub elements: crate::message::Message,
    pub message_id: i64,
    /// 是否 @ 了机器人自己。
    pub mentions_me: bool,
    /// 被叫到的细节：哪一种动作、引用的是哪条。
    pub call: Call,
    /// 是否是机器人自己说的话。
    pub from_me: bool,
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
    looked_id: i64,
    /// 这一次隔多久再看的随机倍数。人看手机没有节拍器。
    look_jitter: f32,
    /// 进程起来之后是否已经从平台翻过这个群的聊天记录。
    pub hydrated: bool,
    /// 睡着时自主开口的时刻，用于每小时上限——判定便宜、开口贵，这条管的是后者。
    doze_spoken: VecDeque<Instant>,
    /// Opt-in chat screenshot gag: at most once per group per cooldown window.
    last_screenshot: Option<Instant>,
    last_screenshot_message: i64,
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
    #[cfg(test)]
    fn last_is_mine(&self) -> bool {
        self.turns.back().is_some_and(|turn| turn.from_me)
    }

    /// 收消息与占用 worker 必须在同一把锁下完成，避免交接时漏消息。
    ///
    /// 同一个 `message_id` 的第二次投递（先由能力层记下现场、搭话侧再补记号）
    /// 只换内容，不动节流状态，也不重新唤醒 worker。
    pub(crate) fn receive(&mut self, turn: Turn) -> bool {
        let fresh = turn.message_id == 0
            || !self
                .turns
                .iter()
                .any(|old| old.message_id == turn.message_id);
        let incoming = !turn.from_me;
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
        if turn.message_id != 0
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

    pub(crate) fn recall(&mut self, id: i64) {
        if let Some(turn) = self
            .turns
            .iter_mut()
            .find(|turn| turn.message_id == id && id != 0)
        {
            turn.text = "[消息已撤回]".into();
            turn.images.clear();
            turn.elements = crate::message::Message::new();
            // 不再允许引用、转发或再次撤回这个 ID。
            turn.message_id = 0;
        }
    }

    pub(crate) fn allow_screenshot(&self, cooldown_seconds: u64) -> bool {
        self.last_screenshot
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(cooldown_seconds.max(3600)))
    }

    pub(crate) fn mark_screenshot(&mut self, message_id: i64) {
        self.last_screenshot = Some(Instant::now());
        self.last_screenshot_message = message_id;
    }

    pub(crate) fn screenshot_seen(&self, message_id: i64) -> bool {
        self.last_screenshot_message == message_id && message_id != 0
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
    pub(crate) fn quote_of(&self, id: i64) -> Option<(bool, String)> {
        if id == 0 {
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
            .find(|turn| turn.message_id != 0)
            .map_or(0, |turn| turn.message_id);
        self.look_jitter = 0.6 + rand::random::<f32>() * 0.9;
    }

    /// 上一眼之后又来了几条群友的消息。
    ///
    /// 按位置数，不按消息号大小：窗口里的顺序就是到达顺序，消息号未必处处递增。
    /// 上一眼看到的那条已经滚出窗口（或者从没看过）时，整个窗口都算新的。
    pub(crate) fn unseen(&self) -> usize {
        let mut count = 0;
        for turn in self.turns.iter().rev() {
            if self.looked_id != 0 && turn.message_id == self.looked_id {
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
            || self
                .turns
                .iter()
                .rev()
                .take_while(|turn| self.looked_id == 0 || turn.message_id != self.looked_id)
                .any(|turn| !turn.from_me && turn.call.named_me)
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
        history.sort_by_key(|turn| (turn.at, turn.message_id));
        for turn in history {
            if turn.message_id == 0
                || self
                    .turns
                    .iter()
                    .any(|old| old.message_id == turn.message_id)
            {
                continue;
            }
            if turn.from_me
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
            .sort_by_key(|turn| (turn.at, turn.message_id));
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
        let who = if turn.from_me && turn.user_id == 0 {
            "平台事件（操作者未知）".to_string()
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
pub(crate) fn resolve_quote(turn: &mut Turn, lookup: impl FnOnce(i64) -> Option<(bool, String)>) {
    if turn.call.reply_to == 0 {
        return;
    }
    let Some((replied_me, quote)) = lookup(turn.call.reply_to) else {
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
pub(crate) fn turn_from(event: &MessageEvent<'_>, me: i64) -> Turn {
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
                    if target == me.to_string() {
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
                    let id = data
                        .get_str("id")
                        .and_then(|id| id.parse::<i64>().ok())
                        .or_else(|| data.get_i64("id"))
                        .unwrap_or(0);
                    if id != 0 {
                        call.reply_to = id;
                    }
                    text.push_str(&format!("[引用:{}] ", data.get_str("id").unwrap_or("?")));
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
        user_id: event.user_id(),
        name: event.sender_name().to_string(),
        text,
        images,
        elements: event
            .0
            .get("message")
            .and_then(|v| simd_json::serde::from_owned_value(v.clone()).ok())
            .unwrap_or_default(),
        message_id: event.message_id(),
        mentions_me,
        call,
        from_me: event.user_id() == me && me != 0,
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
    let message_id = item["id"]
        .as_str()?
        .parse::<i64>()
        .ok()
        .filter(|id| *id != 0)?;
    let user_id = item["user"]["id"]
        .as_str()
        .unwrap_or("")
        .parse::<i64>()
        .unwrap_or(0);
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
    let me = ctx
        .bot
        .login_user
        .get()
        .id
        .parse::<i64>()
        .unwrap_or_default();
    Some(Turn {
        user_id,
        name,
        text: crate::adapters::satori::forward::describe(&elements),
        images,
        elements,
        message_id,
        from_me: user_id != 0 && user_id == me,
        at: item["created_at"]
            .as_i64()
            .map(|millis| millis / 1000)
            .unwrap_or_else(|| chrono::Local::now().timestamp()),
        ..Turn::default()
    })
}

fn states() -> &'static Mutex<HashMap<i64, GroupState>> {
    static STATES: OnceLock<Mutex<HashMap<i64, GroupState>>> = OnceLock::new();
    STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 取出锁访问某个群的状态。闭包里不要 await——锁是同步的。
pub(crate) fn with_group<T>(group_id: i64, action: impl FnOnce(&mut GroupState) -> T) -> T {
    let mut guard = states().lock().unwrap_or_else(|error| error.into_inner());
    action(guard.entry(group_id).or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn turn(text: &str, from_me: bool) -> Turn {
        Turn {
            user_id: 1,
            name: "谁".into(),
            text: text.into(),
            message_id: 1,
            from_me,
            ..Turn::default()
        }
    }

    #[test]
    fn window_keeps_the_tail_in_order() {
        let mut state = GroupState::default();
        for index in 0..WINDOW_CAPACITY + 5 {
            state.push(turn(&index.to_string(), false));
        }
        let recent = state.recent(3);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[2].text, (WINDOW_CAPACITY + 4).to_string());
        assert_eq!(recent[0].text, (WINDOW_CAPACITY + 2).to_string());
        assert!(!state.last_is_mine());
        state.push(turn("我说的", true));
        assert!(state.last_is_mine());
    }

    #[test]
    fn worker_drains_messages_arriving_during_a_reply_and_consumes_mentions_once() {
        let mut state = GroupState::default();
        let mut first = turn("在吗", false);
        first.mentions_me = true;
        assert!(state.receive(first));
        assert!(state.take_mention());
        let snapshot = state.seq;
        let mut second = turn("接着聊", false);
        second.message_id = 2;
        assert!(!state.receive(second.clone()));
        assert!(!state.receive(second)); // 重复事件不唤醒
        assert_eq!(state.seq, snapshot + 1);
        state.push(turn("刚生成的回复", true));
        assert!(state.last_is_mine());
        assert!(state.finish_batch(snapshot)); // 即使自己的回填在最后，也不能漏掉新消息
        assert!(!state.take_mention());
        assert!(!state.finish_batch(state.seq));
        assert!(!state.running);
        let mut third = turn("新一轮", false);
        third.message_id = 3;
        assert!(state.receive(third));
    }

    /// 搭话指令不进窗口，但它必须能把一个闲着的群叫起来，且只叫一次。
    #[test]
    fn a_summon_wakes_an_idle_group_once_without_taking_a_turn() {
        let mut state = GroupState::default();
        assert!(!state.take_summon());
        assert!(state.summon());
        assert!(state.running);
        assert_eq!(state.seq, 1);
        assert!(state.turns.is_empty());
        assert!(state.take_summon());
        // 一次指令只算一次；人格沉默之后不会拿旧指令再唤醒。
        assert!(!state.take_summon());
        // 已经有 worker 在跑时只留下指令，由它下一轮自己看见。
        assert!(!state.summon());
        assert_eq!(state.seq, 2);
        assert!(state.take_summon());
    }

    #[test]
    fn focus_expires_and_never_wakes_itself() {
        let mut state = GroupState {
            focus: Some(Focus {
                users: vec![1],
                topic: "游戏".into(),
                until: Instant::now() + Duration::from_secs(30),
            }),
            ..GroupState::default()
        };
        assert!(state.active_focus().is_some());
        assert!(!state.running);
        state.focus.as_mut().unwrap().until = Instant::now() - Duration::from_secs(1);
        assert!(state.active_focus().is_none());
        assert!(state.rhythm().contains("无，按兴趣旁观"));
    }

    #[test]
    fn transcript_names_speakers_and_marks_media_and_how_it_was_called() {
        let mut mine = turn("嗯", true);
        mine.at = 1_788_800_000;
        let mut theirs = turn("看这个", false);
        theirs.images = vec!["https://example.com/a.png".into()];
        theirs.mentions_me = true;
        theirs.call.at_me = true;
        let text = transcript(&[theirs, mine]);
        assert!(text.contains("谁(1): 看这个〔图片 ×1〕〔@了你〕"), "{text}");
        assert!(text.contains("你自己: 嗯"), "{text}");

        // 三种叫法各标各的：被引用与被戳不该读成「@了你」。
        let quoted = Turn {
            call: Call {
                replied_me: true,
                reply_to: 7,
                quote: "老张：这破依赖装了半天".into(),
                ..Call::default()
            },
            ..turn("你说得对", false)
        };
        let poked = Turn {
            call: Call {
                poked_me: true,
                ..Call::default()
            },
            ..turn("[戳一戳：42 戳了 10000]", false)
        };
        let text = transcript(&[quoted, poked]);
        assert!(
            text.contains("〔引用了你的消息〕〔引用 老张：这破依赖装了半天〕"),
            "{text}"
        );
        assert!(text.contains("〔戳了你〕"), "{text}");
        assert!(!text.contains("〔@了你〕"), "{text}");
    }

    /// 引用解析拿得到「谁说了什么」，引用到自己那条要认出来。
    #[test]
    fn quoting_resolves_who_said_what_and_whether_it_was_mine() {
        let mut state = GroupState::default();
        let mut old = turn("这破依赖装了半天 一直报错", false);
        old.message_id = 88;
        old.name = "老张".into();
        state.push(old);
        let mut mine = turn("别用那个版本了", true);
        mine.message_id = 99;
        state.push(mine);

        assert_eq!(
            state.quote_of(88),
            Some((false, "这破依赖装了半天 一直报错".to_string()))
        );
        assert_eq!(
            state.quote_of(99),
            Some((true, "别用那个版本了".to_string()))
        );
        // 引用到窗口外或没引用，都解析不出来。
        assert_eq!(state.quote_of(1_000), None);
        assert_eq!(state.quote_of(0), None);
        // 太长的话压到上限再加省略号。
        let mut long = turn(&"错".repeat(120), false);
        long.message_id = 77;
        state.push(long);
        let (_, quote) = state.quote_of(77).unwrap();
        assert_eq!(quote.chars().count(), QUOTE_PREVIEW_CHARS + 1);
        assert!(quote.ends_with('…'));
    }

    #[test]
    fn recall_erases_content_and_media_and_invalidates_target() {
        let mut state = GroupState::default();
        let mut t = turn("不再显示", true);
        t.images.push("https://example.com/private.png".into());
        t.elements = crate::message::Message::new().text("不再显示");
        state.receive(t);
        state.recall(1);
        let t = &state.recent(1)[0];
        assert_eq!(t.text, "[消息已撤回]");
        assert!(t.images.is_empty());
        assert!(t.elements.0.is_empty());
        assert_eq!(t.message_id, 0);
        assert_eq!(state.quote_of(1), None);
    }

    #[test]
    fn how_long_the_room_took_to_answer_is_reported_exactly_once() {
        let mut state = GroupState::default();
        assert_eq!(state.take_feedback(), None);
        state.mark_spoke();
        let spoke_at = state.spoke_at.unwrap();
        let mut reply = turn("哦", false);
        reply.message_id = 9;
        reply.at = spoke_at + 12;
        state.receive(reply);
        assert_eq!(state.take_feedback(), Some(12));
        // 反馈只算一次，后面的消息不再反复给同一次发言打分。
        let mut later = turn("再说一句", false);
        later.message_id = 10;
        later.at = spoke_at + 600;
        state.receive(later);
        assert_eq!(state.take_feedback(), None);
    }

    #[test]
    fn hourly_counter_tracks_recent_speech() {
        let mut state = GroupState::default();
        assert_eq!(state.spoken_last_hour(), 0);
        assert_eq!(state.spoken_within(RECENT_SPEECH), 0);
        state.mark_spoke();
        state.mark_spoke();
        assert_eq!(state.spoken_last_hour(), 2);
        // 刚说的两轮当然落在最近十分钟里，节奏描述也要让模型看见这件事。
        assert_eq!(state.spoken_within(RECENT_SPEECH), 2);
        assert_eq!(state.spoken_within(Duration::ZERO), 0);
        let rhythm = state.rhythm();
        assert!(rhythm.contains("最近十分钟发言 2 轮"), "{rhythm}");
        assert!(rhythm.contains("看着就好"), "{rhythm}");
        assert!(state.last_spoke.is_some());
    }

    /// 睡着时的两条闸门：判定按间隔放行，开口按每小时计数。
    #[test]
    fn dozing_gates_judgement_by_interval_and_keeps_its_own_hourly_tally() {
        let mut state = GroupState::default();
        // 写 0 关掉自主判定。
        assert!(!state.allow_doze_gate(Duration::ZERO));
        // 到点放行一次，紧接着的批次都被挡下。
        assert!(state.allow_doze_gate(Duration::from_secs(3_600)));
        assert!(!state.allow_doze_gate(Duration::from_secs(3_600)));
        // 睡着时的自主开口单独计数，不和清醒时的发言混在一起。
        assert_eq!(state.doze_spoke_last_hour(), 0);
        state.mark_doze_spoke();
        state.mark_doze_spoke();
        assert_eq!(state.doze_spoke_last_hour(), 2);
        assert_eq!(state.spoken_last_hour(), 0);
    }

    /// 一眼之后新来的才算「新看到的」；有人喊名字不等下一眼；刚开过口看得勤。
    #[test]
    fn looking_tracks_what_is_new_and_who_is_calling() {
        let mut state = GroupState::default();
        for id in 1..=3 {
            let mut old = turn("旧的", false);
            old.message_id = id;
            state.receive(old);
        }
        assert_eq!(state.unseen(), 3, "从没看过，整个窗口都是新的");
        state.mark_look();
        assert_eq!(state.unseen(), 0);
        let mut fresh = turn("新的", false);
        fresh.message_id = 4;
        state.receive(fresh);
        let mut mine = turn("我说的", true);
        mine.message_id = 5;
        state.receive(mine);
        assert_eq!(state.unseen(), 1, "自己说的不算新看到的");
        assert!(!state.urgent());
        let mut named = turn("A宝你来说说", false);
        named.message_id = 6;
        named.call.named_me = true;
        state.receive(named);
        assert!(state.urgent(), "上一眼之后有人喊了名字");
        state.mark_look();
        assert!(!state.urgent(), "喊过的那一句已经看过了");

        // 刚开过口：下一眼来得快得多。
        let interval = Duration::from_secs(100);
        let idle = state.look_due(interval);
        state.mark_spoke();
        assert!(state.engaged());
        assert!(state.look_due(interval) < idle.min(Duration::from_secs(31)));

        // 打字的工夫里来了几条：数得出来。
        let seq = state.seq;
        let mut late = turn("插一句", false);
        late.message_id = 7;
        state.receive(late);
        assert_eq!(state.drift(seq), 1);
    }

    /// 重启后翻回来的记录垫在前面、按时间排好，自己说过的话把发言账补回来。
    #[test]
    fn seeding_history_restores_context_and_the_speech_ledger() {
        let now = chrono::Local::now().timestamp();
        let mut state = GroupState::default();
        let mut live = turn("刚到的一条", false);
        live.message_id = 30;
        live.at = now;
        state.receive(live);
        let history: Vec<Turn> = [
            (10, "刘欢去世了", false, now - 600),
            (11, "行，我收回刚才那句", true, now - 300),
            (14, "界面新闻都发了", true, now - 295),
            (12, "[图片]", true, now - 200),
            (13, "很久以前说的", true, now - 7_200),
            (30, "刚到的一条", false, now),
        ]
        .into_iter()
        .map(|(id, text, mine, at)| Turn {
            message_id: id,
            at,
            ..turn(text, mine)
        })
        .collect();
        let (added, mine) = state.seed(history);
        assert_eq!((added, mine), (5, 1), "去重；挨着的两条算一轮；只剩图片的、一小时以前的不记账");
        assert!(state.hydrated);
        let texts: Vec<String> = state.recent(10).into_iter().map(|t| t.text).collect();
        assert_eq!(texts.first().map(String::as_str), Some("很久以前说的"));
        assert_eq!(texts.last().map(String::as_str), Some("刚到的一条"));
        assert_eq!(state.spoken_last_hour(), 1);
        let since = state.last_spoke.unwrap().elapsed().as_secs();
        assert!((295..=310).contains(&since), "{since}");
    }

    #[test]
    fn ordinary_judgements_are_bounded_without_affecting_doze() {
        let mut state = GroupState::default();
        // 没看过就立刻看；看过一眼之后要隔一阵，间隔为 0 时随时都能看。
        assert!(state.look_due(Duration::from_secs(90)).is_zero());
        state.mark_look();
        assert!(!state.look_due(Duration::from_secs(90)).is_zero());
        assert!(state.look_due(Duration::ZERO).is_zero());
        assert!(state.allow_doze_gate(Duration::from_secs(30)));
    }

    /// 一份 satori 事件，测试造事件用。
    fn event(value: serde_json::Value) -> simd_json::OwnedValue {
        simd_json::serde::to_owned_value(value).unwrap()
    }

    #[test]
    fn turns_flatten_segments_and_notice_mentions() {
        let raw = event(serde_json::json!({
            "post_type": "message",
            "message_type": "group",
            "group_id": 1,
            "user_id": 42,
            "message_id": 7,
            "time": 1_788_800_000_i64,
            "sender": {"nickname": "张三", "card": "老张"},
            "message": [
                {"type": "reply", "data": {"id": "6"}},
                {"type": "at", "data": {"qq": "3373167460"}},
                {"type": "text", "data": {"text": " 你怎么看"}},
                {"type": "image", "data": {"url": "https://example.com/a.png"}},
            ],
        }));
        let turn = turn_from(&MessageEvent(&raw), 3_373_167_460);
        assert_eq!(turn.name, "老张");
        assert_eq!(turn.text, "[引用:6] @我 你怎么看[图片]");
        assert!(turn.mentions_me);
        assert!(turn.call.at_me);
        assert_eq!(turn.call.reply_to, 6);
        assert!(!turn.call.replied_me);
        assert!(turn.call.quote.is_empty(), "窗口没参与，摘要留空");
        assert!(!turn.from_me);
        assert_eq!(turn.images, ["https://example.com/a.png"]);
        assert_eq!(turn.at, 1_788_800_000);
    }

    #[test]
    fn bot_markdown_buttons_and_qq_card_are_visible_in_both_chat_windows() {
        let raw = event(serde_json::json!({
            "post_type": "message", "message_type": "group", "group_id": 1,
            "user_id": 42, "message_id": 77,
            "sender": {"nickname": "官方机器人"},
            "message": [
                {"type":"text","data":{"text":"**公告**\n今天更新\n按钮: [查看] [稍后]"}},
                {"type":"json","data":{"data":
                    "{\"meta\":{\"news\":{\"jumpUrl\":\"https:\\/\\/example.com\\/release\"}}}"}}
            ]
        }));
        let turn = turn_from(&MessageEvent(&raw), 10000);
        assert!(
            turn.text.contains("**公告** 今天更新 按钮: [查看] [稍后]"),
            "{}",
            turn.text
        );
        assert!(
            turn.text.contains("[卡片: https://example.com/release]"),
            "{}",
            turn.text
        );
        assert!(transcript(&[turn]).contains("https://example.com/release"));
    }

    /// 商城表情带着自己的名字进记录：它没有图片地址，模型看不见它，只写 `[图片]`
    /// 就和截图混成一样的东西，也想不到那是一张能偷来回人的表情包。
    #[test]
    fn shop_stickers_are_named_apart_from_pictures() {
        let raw = event(serde_json::json!({
            "post_type": "message", "message_type": "group", "group_id": 1,
            "user_id": 42, "message_id": 9,
            "sender": {"nickname": "老张"},
            "message": [
                {"type": "mface", "data": {"emoji_id": "296f", "emoji_package_id": 241904,
                    "key": "k1", "summary": "[捂脸笑]"}},
                {"type": "mface", "data": {"emoji_id": "1"}},
            ],
        }));
        let turn = turn_from(&MessageEvent(&raw), 10000);
        assert_eq!(turn.text, "[表情包:捂脸笑][表情包]");
        // 群里斗图多半用的是收藏表情：图片子类型 1。它有图，模型看得见，也照样记成表情包。
        let saved = event(serde_json::json!({
            "post_type": "message", "message_type": "group", "group_id": 1,
            "user_id": 42, "message_id": 10,
            "sender": {"nickname": "老张"},
            "message": [
                {"type": "image", "data": {"url": "https://example.com/a.gif", "sub_type": 1,
                    "summary": "[动画表情]"}},
                {"type": "image", "data": {"url": "https://example.com/b.png"}},
            ],
        }));
        let saved = turn_from(&MessageEvent(&saved), 10000);
        assert_eq!(saved.text, "[表情包][图片]");
        assert_eq!(saved.images.len(), 2);
        assert!(turn.images.is_empty());
        // 与商城表情那一段原样留着，偷的时候取得到。
        assert_eq!(
            crate::plugins::oai::chat::actions::sticker(&turn, 1)
                .unwrap()
                .type_,
            "mface"
        );
    }

    #[test]
    fn own_messages_are_recognized_and_media_only_turns_keep_a_label() {
        let raw = event(serde_json::json!({
            "post_type": "message",
            "message_type": "group",
            "group_id": 1,
            "user_id": 3_373_167_460_i64,
            "message": [{"type": "image", "data": {"file": "https://example.com/b.png"}}],
        }));
        let turn = turn_from(&MessageEvent(&raw), 3_373_167_460);
        assert!(turn.from_me);
        assert_eq!(turn.text, "[图片]");
    }

    #[test]
    fn quoting_rides_along_and_being_quoted_counts_as_being_called() {
        let mut quoted = Turn {
            text: "[引用:88] 你说得对".into(),
            call: Call {
                reply_to: 88,
                ..Call::default()
            },
            ..Turn::default()
        };
        // 引的是群友的话：记下原话，但不算被叫到。
        resolve_quote(&mut quoted, |id| {
            assert_eq!(id, 88);
            Some((false, "老张：这破依赖装了半天".to_string()))
        });
        assert_eq!(quoted.call.quote, "老张：这破依赖装了半天");
        assert!(!quoted.call.replied_me);
        assert!(!quoted.mentions_me);

        // 引的是自己发的那条：与 @ 同等地被叫醒。
        let mut called = Turn {
            call: Call {
                reply_to: 99,
                ..Call::default()
            },
            ..Turn::default()
        };
        resolve_quote(&mut called, |_| {
            Some((true, "你说的：别用那个版本".to_string()))
        });
        assert!(called.call.replied_me);
        assert!(called.mentions_me);
        assert_eq!(called.call.quote, "你说的：别用那个版本");

        // 引用不在窗口里的旧消息：留个消息号，不编造内容，也不惊醒。
        let mut old = Turn {
            call: Call {
                reply_to: 1_000,
                ..Call::default()
            },
            ..Turn::default()
        };
        resolve_quote(&mut old, |_| None);
        assert!(old.call.quote.is_empty());
        assert!(!old.mentions_me);
        assert_eq!(old.call.reply_to, 1_000);

        // 没引用的时候压根不去查。
        let mut plain = Turn::default();
        resolve_quote(&mut plain, |_| panic!("没有引用就不该查窗口"));
        assert!(plain.call.quote.is_empty());
    }

    #[test]
    fn a_mention_of_someone_else_is_written_the_way_the_persona_may_write_it() {
        let raw = event(serde_json::json!({
            "post_type": "message",
            "message_type": "group",
            "group_id": 1,
            "user_id": 42,
            "message_id": 8,
            "time": 1_788_800_000_i64,
            "sender": {"nickname": "张三", "card": "老张"},
            "message": [
                {"type": "at", "data": {"qq": "3938463481"}},
                {"type": "text", "data": {"text": "兄弟说到点子上了"}},
            ],
        }));
        let turn = turn_from(&MessageEvent(&raw), 3_373_167_460);
        assert_eq!(turn.text, "[at:3938463481] 兄弟说到点子上了");
        assert!(!turn.mentions_me && !turn.call.at_me);
        assert!(transcript(&[turn]).contains("[at:3938463481] 兄弟说到点子上了"));

        // 平台在 at 段后面又写了一遍「@名字」：那个 `@` 要摘掉，否则人格会跟着学。
        let echoed = event(serde_json::json!({
            "post_type": "message",
            "message_type": "group",
            "group_id": 1,
            "user_id": 42,
            "message_id": 9,
            "time": 1_788_800_000_i64,
            "sender": {"nickname": "张三", "card": "老张"},
            "message": [
                {"type": "at", "data": {"qq": "3938463481"}},
                {"type": "text", "data": {"text": "@呜呜呜呜云 兄弟说到点子上了"}},
            ],
        }));
        let turn = turn_from(&MessageEvent(&echoed), 3_373_167_460);
        assert_eq!(turn.text, "[at:3938463481] 呜呜呜呜云 兄弟说到点子上了");
    }
}
