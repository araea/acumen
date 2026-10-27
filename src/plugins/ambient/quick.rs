//! 随口一句：分数没到开口的线，但落在「想吭一声」的区间里。
//!
//! 号主手打的消息有四分之一只有四五个字以内，短到只剩一个反应；机器人每次开口都是
//! 「判定过线 → 人格带着内容写一句」，于是最短的那一档几乎是空的（同一个群里它的
//! 下四分位是 9 个字，号主是 4–6 个字）。真人在群里的存在感很大一部分正是这种不费
//! 力的吭声：一个「？」、一声「笑死」、一句「好可爱啊」，看一眼就走。
//!
//! 判定打分落在 [`AmbientConfig::quick_floor`] 与开口门槛之间——也就是判定自己说的
//! 「想说点什么，可不说也完全没关系」那一档——并且现场够新、够久没开过口、本人不在、
//! 这个小时还没用完额度时，按一定几率随口回一两个字。短反应由刚才那次判定顺带返回，
//! 这里只做本地清洗、去重与节流：不调用发言模型，不再补一次判定，也不碰任何工具。

use super::window::Turn;
use super::AmbientConfig;
use crate::plugins::oai::chat::protocol::is_provider_noise;
use std::time::Duration;

/// 够格的时刻里，真去说一句的几率。留一半的沉默：吭声是偶尔的。
pub(super) const CHANCE: f32 = 0.45;
/// 最新一条消息超过这么久就不接了——隔了一会儿才冒出来的「？」是怪的。
pub(super) const FRESH_SECONDS: i64 = 75;
/// 距上一次开口至少隔这么久；刚说完话再补一个「？」像是在刷存在感。
pub(super) const GAP: Duration = Duration::from_secs(120);
/// 一句话最长这么多字，超了说明它在认真说话，那是发言模型的活。
const MAX_CHARS: usize = 12;
/// 判定当下的几项事实，用来决定要不要随口吭一声。
#[derive(Debug, Clone, Copy)]
pub(super) struct Facts {
    pub score: u8,
    pub threshold: u8,
    /// 窗口里最新一条消息：是不是自己的、多久以前（秒）。
    pub newest: Option<(bool, i64)>,
    pub since_last_spoke: Option<Duration>,
    pub quick_last_hour: usize,
    /// 这个群这一小时最多吭几声：配置的上限，与他本人在这个群里平时的发言量（见
    /// [`super::habit::Habit::hourly_target`]，吭声算一个回合的一半）里较小的那个。
    pub quick_limit: usize,
    /// 号主本人刚在群里亲手打过字。
    pub owner_present: bool,
    /// 计价高峰的省钱档：这一档连判定都是偶尔的，不再添随口一句。
    pub dozing: bool,
}

/// 要不要随口回一句。`roll` 取 `[0, 1)`。
pub(super) fn wants(config: &AmbientConfig, facts: Facts, roll: f32) -> bool {
    if config.quick_per_hour == 0 || facts.owner_present || facts.dozing {
        return false;
    }
    if facts.score < config.quick_floor || facts.score >= facts.threshold {
        return false;
    }
    match facts.newest {
        Some((false, age)) if age <= FRESH_SECONDS => {}
        _ => return false,
    }
    if facts.since_last_spoke.is_some_and(|elapsed| elapsed < GAP) {
        return false;
    }
    facts.quick_last_hour < facts.quick_limit && roll < CHANCE
}

/// 模型的输出 → 能发的一句；不是一句短话（沉默、带标记、太长、带链接）就是 `None`。
pub(super) fn clean(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|line| !line.is_empty())?;
    if line.starts_with('[') || line.contains("[silent]") || is_provider_noise(line) {
        return None;
    }
    let line = line.trim_matches(|c: char| "「」“”\"'`。.".contains(c) || c.is_whitespace());
    let count = line.chars().count();
    if count == 0 || count > MAX_CHARS || line.contains(['@', '\\']) || line.contains("http") {
        return None;
    }
    Some(line.to_string())
}

/// 这句是不是只在给别人点头：「确实」「有道理」「备份确实不靠谱」。
///
/// 随口一句的价值在于它是**自己的**反应（「？」「好怪」「这也行」）；附和对方的那几个
/// 说法谁都能接，接多了就是凑数的捧哏（线上 2026-10-27：「备份确实不靠谱」接的是一句
/// 没在问它的吐槽）。正常发言里同意就说同意，不受这一条管；这里只拦随口吭的那一声。
pub(super) fn nods(text: &str) -> bool {
    const NODS: [&str; 8] = ["确实", "有道理", "没毛病", "说得对", "你说得对", "是的", "对的", "同意"];
    let text = text.trim();
    NODS.iter().any(|nod| text.starts_with(nod)) || (text.chars().count() > 4 && text.contains("确实"))
}

/// 不跟自己最近说过的话重样：同一句「笑死」十分钟内说两遍，是机器人的节奏。
pub(super) fn repeats(text: &str, turns: &[Turn]) -> bool {
    turns
        .iter()
        .rev()
        .filter(|turn| turn.from_me)
        .take(6)
        .any(|turn| turn.text.trim() == text)
}

#[cfg(test)]
mod nod_tests {
    #[test]
    fn nodding_quips_are_told_apart_from_reactions() {
        for nod in ["确实", "有道理", "备份确实不靠谱", "说得对"] {
            assert!(super::nods(nod), "{nod}");
        }
        for own in ["？", "好怪", "这也行", "有点东西", "扎手不", "物理抠图还行"] {
            assert!(!super::nods(own), "{own}");
        }
    }
}
