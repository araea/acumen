//! 一条生活线：这个钟点，它大概在干什么。
//!
//! 人格缺的从来不是「像人说话」的写法，而是**此刻的处境**。同一句话在上班路上、
//! 在工位上摸鱼、在半夜躺床上发出去，长短、语气、回不回都该不一样；可模型每次
//! 被唤起时手里只有「现在 21:37」这一串数字，于是它永远是一个随时待命、随叫随到
//! 的聊天程序。
//!
//! 这里给它一条随钟点走的生活：几点在通勤、几点在工位、几点吃饭、几点打游戏、
//! 几点该睡了。作息基线来自 [`super::mood`] 那张按小时量的真实在线曲线——不是
//! 凭印象编的「朝九晚五」；具体在干什么则按日期抽一次、当天不变，落盘跨重启，
//! 所以它今天一整天「在加班」就是加班，不会 21:03 说在吃饭、21:04 说在打游戏。
//!
//! 它只影响三件事：提示词里那句「你这一会儿」，说话的意愿（困了不想接话），
//! 以及打字的快慢。它不编经历——不会说「我刚开完会」，只会说「你这会儿在工位上」。

use serde::{Deserialize, Serialize};
use std::hash::{Hash as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::LOG_TARGET;

/// 一段作息：起止钟点（左闭右开）、做什么、忙不忙、想不想说话。
struct Slot {
    from: u32,
    to: u32,
    /// 候选活动：同一天里抽中哪个是固定的，人不会每天晚饭都吃一样的东西。
    doing: &'static [&'static str],
    /// 0 = 完全闲，1 = 完全腾不出手。这个数直接压低开口意愿。
    busy: f32,
}

/// 一天的骨架。钟点划分照真实作息：他不是朝九晚五的人，白天多半不在，
/// 深夜反而活跃——与 [`super::mood`] 那张曲线同一份口径。
const DAY: &[Slot] = &[
    Slot { from: 0, to: 2, doing: &["还没睡，躺着刷手机", "在打游戏，懒得动", "刚洗完澡，等头发干"], busy: 0.35 },
    Slot { from: 2, to: 7, doing: &["睡了", "睡了", "睡得正沉"], busy: 1.0 },
    Slot { from: 7, to: 9, doing: &["刚醒，赖在床上", "在洗漱", "在路上，去上班"], busy: 0.7 },
    Slot { from: 9, to: 12, doing: &["在工位上", "在开会或忙手上的事", "在赶东西"], busy: 0.8 },
    Slot { from: 12, to: 14, doing: &["在吃饭", "趴着歇一会儿", "吃完饭刷手机"], busy: 0.4 },
    Slot { from: 14, to: 18, doing: &["在工位上，手头有活儿", "在工位上摸鱼", "在赶东西"], busy: 0.75 },
    Slot { from: 18, to: 20, doing: &["下班路上", "在吃饭", "刚到家，缓一缓"], busy: 0.5 },
    Slot { from: 20, to: 24, doing: &["在打游戏", "在看剧", "躺着刷手机", "在群里闲逛", "刚洗完澡"], busy: 0.25 },
];

impl Slot {
    fn at(hour: u32) -> &'static Slot {
        let hour = hour % 24;
        DAY.iter()
            .find(|slot| hour >= slot.from && hour < slot.to)
            .unwrap_or(&DAY[0])
    }

    /// 睡着了：这段时间里除非有人点名，否则不主动开口。
    fn asleep(&self) -> bool {
        self.busy >= 0.95
    }
}

/// 今天的这条线：日期与抽中的偏移，落盘。
#[derive(Debug, Default, Serialize, Deserialize)]
struct Day {
    /// `YYYY-MM-DD`（本机时区）。
    date: String,
    /// 当天每段时间抽中的候选序号；日子变了就重抽。
    picks: Vec<usize>,
}

fn store() -> &'static Mutex<(Option<PathBuf>, Day)> {
    static STORE: OnceLock<Mutex<(Option<PathBuf>, Day)>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new((None, Day::default())))
}

fn lock() -> std::sync::MutexGuard<'static, (Option<PathBuf>, Day)> {
    store().lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 指定落盘位置；启动时调用一次。
pub(crate) fn attach(base: &Path) {
    let path = base.join("life.json");
    let saved = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Day>(&raw).ok())
        .unwrap_or_default();
    let mut store = lock();
    store.0 = Some(path);
    store.1 = saved;
}

/// 今天这一条线；日子变了就重抽并落盘。
fn today() -> Day {
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut store = lock();
    if store.1.date != date || store.1.picks.len() != DAY.len() {
        let mut picks = Vec::with_capacity(DAY.len());
        for (index, slot) in DAY.iter().enumerate() {
            picks.push(pick(&date, index, slot.doing.len()));
        }
        store.1 = Day { date, picks };
        if let Some(path) = store.0.clone() {
            let json = serde_json::to_string(&store.1).unwrap_or_default();
            // 这条线每天只写一次，同步写就够了，免得为一个几十字节的文件排一个 task。
            if let Err(error) = std::fs::write(&path, json) {
                warn!(target: LOG_TARGET, "写入生活线失败：{error}");
            }
        }
    }
    Day {
        date: store.1.date.clone(),
        picks: store.1.picks.clone(),
    }
}

/// 用日期与段落下标算一个稳定的序号：同一天同一个钟点永远抽中同一样。
fn pick(date: &str, index: usize, len: usize) -> usize {
    if len <= 1 {
        return 0;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    date.hash(&mut hasher);
    index.hash(&mut hasher);
    (hasher.finish() as usize) % len
}

/// 这个钟点在干什么。
fn doing(hour: u32) -> &'static str {
    let slot = Slot::at(hour);
    let picks = today().picks;
    let index = picks.get(slot_index(slot)).copied().unwrap_or(0);
    slot.doing.get(index % slot.doing.len()).copied().unwrap_or("")
}

fn slot_index(slot: &Slot) -> usize {
    DAY.iter().position(|s| s.from == slot.from && s.to == slot.to).unwrap_or(0)
}

fn hour_now() -> u32 {
    use chrono::Timelike as _;
    chrono::Local::now().hour()
}

/// 放进提示词的一句。写成第二人称、只说处境，不编细节。
pub(crate) fn brief() -> String {
    let hour = hour_now();
    let slot = Slot::at(hour);
    let doing = doing(hour);
    if slot.asleep() {
        return format!("你这一会儿：{doing}。要是被叫醒，回话会短、会慢，也可能过一会儿才看见。");
    }
    let load = match slot.busy {
        b if b >= 0.7 => "手头正忙着，回话短、隔得久一点",
        b if b >= 0.45 => "有点事在手上，看到会回，但不一定马上",
        _ => "闲着，能聊几句",
    };
    format!("你这一会儿：{doing}，{load}。这是你此刻的处境，不是要跟人汇报的事。")
}

/// 睡着的时段：除非被点名，别主动开口。
pub(crate) fn asleep() -> bool {
    Slot::at(hour_now()).asleep()
}

/// 开口门槛的加价（加在判定分上）：睡着 +12，忙 +4，闲着 0。
pub(crate) fn threshold_shift() -> i16 {
    let slot = Slot::at(hour_now());
    if slot.asleep() {
        12
    } else if slot.busy >= 0.7 {
        4
    } else {
        0
    }
}

/// 打字与思考的缩放：忙的时候想得久一点、敲得慢一点。
pub(crate) fn pace_scale() -> f32 {
    let slot = Slot::at(hour_now());
    1.0 + slot.busy * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_hour_has_a_line() {
        for hour in 0..24 {
            let slot = Slot::at(hour);
            assert!(!slot.doing.is_empty(), "hour {hour} has no activity");
            assert!(slot.from <= hour && hour < slot.to);
        }
    }

    #[test]
    fn picks_are_stable_within_a_day() {
        assert_eq!(pick("2026-10-04", 3, 5), pick("2026-10-04", 3, 5));
        // 换一天就该换一种可能（偶尔撞上同一个也算对，只要求不是常量）。
        let days: std::collections::HashSet<usize> =
            ["2026-10-01", "2026-10-02", "2026-10-03", "2026-10-04", "2026-10-05"]
                .iter()
                .map(|date| pick(date, 3, 5))
                .collect();
        assert!(days.len() > 1);
    }

    #[test]
    fn small_hours_are_asleep() {
        assert!(Slot::at(3).asleep());
        assert!(!Slot::at(21).asleep());
        assert!(Slot::at(3).busy > Slot::at(21).busy);
    }
}
