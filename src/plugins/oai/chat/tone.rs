//! 本群此刻怎么说话，以及别把自己说过的话再说一遍。
//!
//! 小模型最容易露馅的地方不是内容，是「形」：群里都在发七八个字的碎句，它回一段
//! 带句号的完整段落；群里三分钟没人说话，它还在追问。这些都不需要模型判断——
//! 窗口里现成的统计就能给出锚点，喂给模型比再写十条规则管用。
//!
//! 复读检测同理：人不会把刚说过的话换个标点再说一遍，但小模型在同一段上下文里
//! 反复被唤起时非常容易这样。两件事都是纯本地计算，零调用成本。

use super::window::Turn;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// 消息里的占位标记与提及，不计入「这句话有多长」。
fn noise() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[[^\]]{0,64}\]|@\d+|https?://\S+").unwrap())
}

/// 常见到没有信息量的字；含这些字的组合不当作「本群高频词」。
const STOP: &str = "的了是我你他她它不在这那有就也很吗吧啊呢么什怎都还要没个会说去来能好过想把被给和跟又再只但而且";

/// 去掉占位标记之后真正被打出来的正文。
fn visible(text: &str) -> String {
    noise().replace_all(text, " ").trim().to_string()
}

fn ends_with_punctuation(text: &str) -> bool {
    text.chars()
        .last()
        .is_some_and(|last| "。．.!！?？~～…、,，;；".contains(last))
}

/// 窗口里的高频二字组合，粗糙但够用：中文没分词也能捞出当下在聊什么。
fn hot_words(texts: &[String], limit: usize) -> Vec<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for text in texts {
        let chars: Vec<char> = text.chars().collect();
        for pair in chars.windows(2) {
            if pair
                .iter()
                .any(|c| !matches!(*c as u32, 0x4E00..=0x9FFF) || STOP.contains(*c))
            {
                continue;
            }
            *counts.entry(pair.iter().collect()).or_default() += 1;
        }
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().filter(|(_, n)| *n >= 3).collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked.into_iter().take(limit).map(|(word, _)| word).collect()
}

/// 把窗口读成一句「本群此刻的语感」。
pub(crate) fn register(turns: &[Turn]) -> String {
    let others: Vec<&Turn> = turns.iter().filter(|turn| !turn.from_me).collect();
    if others.len() < 3 {
        return "本群此刻：刚有人开口，还看不出节奏。".to_string();
    }
    let bodies: Vec<String> = others.iter().map(|turn| visible(&turn.text)).collect();
    let spoken: Vec<&String> = bodies.iter().filter(|body| !body.is_empty()).collect();
    let speakers = {
        let mut ids: Vec<i64> = others.iter().map(|turn| turn.user_id).collect();
        ids.sort_unstable();
        ids.dedup();
        ids.len()
    };
    let mut parts = vec![format!("{speakers} 个人在说")];

    let (first, last) = (others[0].at, others[others.len() - 1].at);
    if last > first && others.len() > 1 {
        let gap = (last - first) as f64 / (others.len() - 1) as f64;
        parts.push(if gap < 60.0 {
            format!("约每 {} 秒一条", gap.round().max(1.0))
        } else {
            format!("约每 {} 分钟一条", (gap / 60.0).round().max(1.0))
        });
    }
    if !spoken.is_empty() {
        let average: usize = spoken.iter().map(|body| body.chars().count()).sum::<usize>()
            / spoken.len();
        parts.push(format!("平均每条 {average} 字"));
        let punctuated = spoken
            .iter()
            .filter(|body| ends_with_punctuation(body))
            .count();
        parts.push(if punctuated * 2 < spoken.len() {
            "多数不带句末标点".to_string()
        } else {
            "多数把标点打全".to_string()
        });
    }
    let media = others
        .iter()
        .filter(|turn| !turn.images.is_empty() || turn.text.contains("[表情"))
        .count();
    if media * 3 >= others.len() {
        parts.push("图和表情不少".to_string());
    }
    let hot = hot_words(&bodies, 3);
    if !hot.is_empty() {
        parts.push(format!(
            "反复出现：{}",
            hot.iter()
                .map(|word| format!("「{word}」"))
                .collect::<Vec<_>>()
                .join("")
        ));
    }
    format!(
        "本群此刻：{}。说话往这个劲儿上靠，顺着大家的调子来。",
        parts.join("，")
    )
}

/// 两段话的字面重合度（相邻二字组合的 Jaccard）。
fn overlap(a: &str, b: &str) -> f32 {
    let grams = |text: &str| {
        let chars: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
        chars
            .windows(2)
            .map(|pair| pair.iter().collect::<String>())
            .collect::<std::collections::HashSet<String>>()
    };
    let (left, right) = (grams(a), grams(b));
    if left.is_empty() || right.is_empty() {
        return if a.trim() == b.trim() { 1.0 } else { 0.0 };
    }
    let shared = left.intersection(&right).count() as f32;
    shared / (left.len() + right.len()) as f32 * 2.0
}

/// 太短的话（「确实」「行吧」）本来就会重复，不算复读。
const ECHO_MIN_CHARS: usize = 8;
/// 重合到这个程度就是同一句话换了个说法。
const ECHO_LIMIT: f32 = 0.68;

/// 这句话是不是在重复自己最近说过的某一句。
pub(crate) fn echoes(text: &str, turns: &[Turn]) -> bool {
    let body = visible(text);
    if body.chars().count() < ECHO_MIN_CHARS {
        return false;
    }
    turns
        .iter()
        .filter(|turn| turn.from_me)
        .rev()
        .take(10)
        .any(|turn| overlap(&body, &visible(&turn.text)) >= ECHO_LIMIT)
}

/// 一句话切成的「字组」集合：汉字、字母、数字两两成组，空白与标点不进。
///
/// 用字组而不是词，是因为群聊样本又短又口语，切词器在「降噪还是很顶的」这种
/// 半截话上切不出什么可靠的东西；字组重叠已经够把「也在聊手机」认出来了。
/// 偷来的表情包挑贴题的那几张时用的是同一把尺子（见 [`super::stickers`]）。
pub(crate) fn grams(text: &str) -> HashSet<String> {
    let chars: Vec<char> = text
        .chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let mut out = HashSet::new();
    match chars.len() {
        0 => {}
        1 => {
            out.insert(chars[0].to_string());
        }
        _ => {
            for pair in chars.windows(2) {
                out.insert(pair.iter().collect());
            }
        }
    }
    out
}

/// 一条样本与当下话题的贴近程度：共有的字组数除以自身字组数的平方根。
///
/// 除以平方根是为了不让长句单靠长就赢——样本长短差得不多，但「牛逼克拉斯」和
/// 「运存高一点还是有点用的 毕竟我有时候会在手机上玩盖世游戏」不该按长度排座次。
pub(crate) fn affinity(sample: &str, topic: &HashSet<String>) -> f32 {
    let own = grams(sample);
    if own.is_empty() || topic.is_empty() {
        return 0.0;
    }
    let shared = own.iter().filter(|gram| topic.contains(*gram)).count();
    shared as f32 / (own.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(user_id: i64, text: &str, at: i64) -> Turn {
        Turn {
            user_id,
            name: format!("群友{user_id}"),
            text: text.into(),
            message_id: at,
            from_me: user_id == 0,
            at,
            ..Turn::default()
        }
    }


    #[test]
    fn the_register_reports_length_punctuation_and_what_is_being_talked_about() {
        let mut turns = Vec::new();
        for index in 0..9 {
            turns.push(turn(
                index % 3 + 1,
                "这个保底真的抽卡保底没了",
                index * 30,
            ));
        }
        // 只有一两条消息时说不出节奏，也不硬编一个。
        assert!(register(&turns[..1]).contains("看不出节奏"));

        let text = register(&turns);
        assert!(text.contains("3 个人在说"), "{text}");
        assert!(text.contains("约每 30 秒一条"), "{text}");
        assert!(text.contains("多数不带句末标点"), "{text}");
        assert!(text.contains("「保底」"), "{text}");
        // 占位标记不计入字数。
        let plain = visible("[引用:12] @114514 就这样[图片]");
        assert_eq!(plain, "就这样");
    }


    #[test]
    fn repeating_yourself_is_caught_but_short_interjections_are_free() {
        let mine = vec![
            turn(0, "那你重启一下路由器试试 不行再说", 0),
            turn(0, "行吧", 1),
        ];
        assert!(echoes("那你重启一下路由器试试，不行再说", &mine));
        assert!(echoes("你重启一下路由器试试 不行再说别的", &mine));
        // 短句、新内容都放行。
        assert!(!echoes("行吧", &mine));
        assert!(!echoes("那是驱动的问题 跟路由器没关系", &mine));
        // 只跟自己比，不跟群友比。
        let theirs = vec![turn(1, "那你重启一下路由器试试 不行再说", 0)];
        assert!(!echoes("那你重启一下路由器试试 不行再说", &theirs));
    }

}
