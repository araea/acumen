//! 什么时候该引用：照号主本人的习惯，而不是逢回必引。
//!
//! 2026-10-01 对着聊天记录量过：号主手打的消息只有 13.8% 带引用，而且引不引看的是
//! 现场有多乱——最近八条里只有一个别人在说话时只有 2.9% 引，三个人以上在抢话时升到
//! 18%–29%，被人 @ 了再回时是 40%。机器人那边同一个群里是 66%，几乎每句都挂着一条
//! 引用。引用是「这句话回的是哪一句」的指路牌，两个人对聊时没人需要指路牌，到处都
//! 插的那个就是机器人。
//!
//! 引用本身有用：话题已经往前走了几条、几个人在抢话、有人直接点了它，这几种情形不
//! 引就对不上号。所以这里算的是**赔率**，不是禁令——模型想引，按现场掷一次骰子。

use super::window::Turn;

/// 窗口尾部看多少条来数「几个人在说话」。
const TAIL: usize = 8;

/// 别人在说话的人数越多，越需要引用来指路。下标是最近八条里不同发言者的个数。
///
/// 数字取自号主本人的记录（2.9 / 6.4 / 9.6 / 18.0 / 24.9 / 28.9 %），再放大一点——
/// 那些是「所有手打消息」里带引用的比例，而机器人开口几乎都是在回某一句话。
const BY_AUTHORS: [f32; 6] = [0.05, 0.10, 0.15, 0.27, 0.37, 0.43];

/// 被 @ 或被引用之后的回复，号主本人是 40% 引。
const DIRECT_FLOOR: f32 = 0.45;

/// 目标话已经被冲到后面几条时，不引就对不上号。
const FAR_BEHIND: usize = 3;

/// 想引 `target` 这条消息，留下这条引用的概率。
///
/// `target` 不在窗口里（早就滚出去了、或者是平台翻回来的旧消息）时一律保留：
/// 那几乎一定是在回一句隔得很远的话，引用正是为这种情形存在的。
pub(super) fn odds(target_id: &str, turns: &[Turn]) -> f32 {
    let Some(at) = turns.iter().position(|turn| turn.message_id == target_id) else {
        return 1.0;
    };
    let behind = turns[at + 1..].iter().filter(|turn| !turn.from_me).count();
    if behind >= FAR_BEHIND {
        return 0.97;
    }
    let mut authors: Vec<&str> = turns
        .iter()
        .rev()
        .take(TAIL)
        .filter(|turn| !turn.from_me && !turn.user_id.is_empty())
        .map(|turn| turn.user_id.as_str())
        .collect();
    authors.sort_unstable();
    authors.dedup();
    let base = BY_AUTHORS[authors.len().min(BY_AUTHORS.len() - 1)];
    // 目标后面每多一条别人的话，对不上号的风险就大一截。
    let scaled = base * [1.0, 2.2, 3.5][behind];
    let target = &turns[at];
    let direct = target.call.mine() || target.mentions_me;
    let odds = if direct { scaled.max(DIRECT_FLOOR) } else { scaled };
    odds.min(0.97)
}

/// 掷一次骰子：`roll` 取 `[0, 1)`。
pub(super) fn keeps(target_id: &str, turns: &[Turn], roll: f32) -> bool {
    roll < odds(target_id, turns)
}
