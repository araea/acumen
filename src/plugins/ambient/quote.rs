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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::oai::chat::window::Call;

    fn turn(id: &str, user: &str, mine: bool) -> Turn {
        Turn {
            message_id: id.into(),
            user_id: user.into(),
            from_me: mine,
            text: "随便聊聊".into(),
            ..Turn::default()
        }
    }

    /// 两个人对聊（算上自己），对方刚说完：不需要指路牌。
    #[test]
    fn a_two_person_chat_rarely_needs_a_quote() {
        let turns = [turn("1", "a", false), turn("2", "a", false), turn("3", "a", false)];
        assert!(odds("3", &turns) <= 0.12, "{}", odds("3", &turns));
    }

    /// 几个人抢话时，引用才是对得上号的办法；话题冲到后面几条更是。
    #[test]
    fn a_crowded_room_and_a_buried_target_raise_the_odds() {
        let crowded: Vec<Turn> = (0..6)
            .map(|index| turn(&index.to_string(), &format!("u{index}"), false))
            .collect();
        let newest = odds("5", &crowded);
        let one_back = odds("4", &crowded);
        let buried = odds("2", &crowded);
        assert!(newest > 0.3 && newest < one_back && one_back <= buried, "{newest} {one_back} {buried}");
        assert!(buried >= 0.95, "{buried}");
        // 窗口里找不到的目标一律保留。
        assert_eq!(odds("没这条", &crowded), 1.0);
    }

    /// 被 @ 之后回，号主本人 40% 引：抬到地板线，不会低于它。
    #[test]
    fn answering_a_direct_call_has_a_floor() {
        let mut called = turn("2", "b", false);
        called.call = Call {
            at_me: true,
            ..Call::default()
        };
        let turns = [turn("1", "a", false), called];
        assert!(odds("2", &turns) >= DIRECT_FLOOR);
        // 自己说的话不算「别人在抢话」，也不让目标后面多出一格。
        let with_mine = [turn("1", "a", false), turn("2", "b", false), turn("3", "", true)];
        assert_eq!(odds("2", &with_mine), odds("2", &with_mine[..2]));
    }

    /// 骰子按赔率落：0 一定留，接近 1 一定去。
    #[test]
    fn the_roll_decides_against_the_odds() {
        let turns = [turn("1", "a", false), turn("2", "b", false)];
        let p = odds("2", &turns);
        assert!(keeps("2", &turns, 0.0));
        assert!(keeps("2", &turns, p - 0.001));
        assert!(!keeps("2", &turns, p + 0.001));
        assert!(!keeps("2", &turns, 0.999) || p > 0.99);
    }

    /// 长期平均下来：两人对聊里，引用的占比应该落在号主本人那一档，而不是 66%。
    #[test]
    fn over_many_rounds_the_quote_rate_matches_the_owner_not_the_old_bot() {
        let turns = [turn("1", "a", false), turn("2", "a", false), turn("3", "a", false)];
        let p = odds("3", &turns);
        let kept = (0..1000)
            .filter(|n| keeps("3", &turns, *n as f32 / 1000.0))
            .count();
        let rate = kept as f32 / 1000.0;
        assert!((rate - p).abs() < 0.01 && rate < 0.2, "{rate}");
    }
}
