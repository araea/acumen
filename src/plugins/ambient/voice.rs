//! 号主本人手打的短句样本：发言前按当下话题从实物里挑几条，当调子用。
//!
//! 人设写的是「他是谁」（形容词与规矩），这里放的是「他真这么说过」（实物）。
//! 样本比任何形容词都更能定住口吻与长度——模型是照着例子写字的，不是照着规矩。
//! 样本库随 [`crate::plugins::ambient`] 一起编译进来，改它就等于改风格；文件里的
//! 分节与来源说明见 `res/ambient/voice.md`。
//!
//! 挑选只做一件事：看这一轮群里在聊什么，把最贴题的那几条捞出来。冷门话题一条
//! 都贴不上时按文件顺序补几条通用的，保证每轮都有实物可看。

use super::window::Turn;
use std::collections::HashSet;

/// 编译进来的样本库。加一条就是加一个标尺，改风格先改它。
const VOICE: &str = include_str!("../../../res/ambient/voice.md");

/// 一次最多贴几条。样本是标尺不是范文，三五条足够定调。
const MAX_LINES: usize = 5;
/// 一次最少贴几条；贴不满就从文件开头补通用句。
const MIN_LINES: usize = 3;
/// 贴出来的样本一共占多少字上限。它们跟着每轮的账单走。
const MAX_CHARS: usize = 220;
/// 只看最近的这些条消息来猜话题；再往前的时间隔得远，聊的多半是另一码事。
const TOPIC_TURNS: usize = 12;

/// 样本库里的一条短句。
#[derive(Debug, PartialEq, Eq)]
struct Sample<'a> {
    text: &'a str,
}

/// 只认 `- ` 开头的行；`#` 分节标题、说明文字与空行都不进样本。
fn parse(raw: &str) -> Vec<Sample<'_>> {
    raw.lines()
        .filter_map(|line| {
            let text = line.trim().strip_prefix("- ")?.trim();
            (!text.is_empty()).then_some(Sample { text })
        })
        .collect()
}

/// 一句话切成的「字组」集合：汉字、字母、数字两两成组，空白与标点不进。
///
/// 用字组而不是词，是因为群聊样本又短又口语，切词器在「降噪还是很顶的」这种
/// 半截话上切不出什么可靠的东西；字组重叠已经够把「也在聊手机」认出来了。
fn grams(text: &str) -> HashSet<String> {
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
fn affinity(sample: &str, topic: &HashSet<String>) -> f32 {
    let own = grams(sample);
    if own.is_empty() || topic.is_empty() {
        return 0.0;
    }
    let shared = own.iter().filter(|gram| topic.contains(*gram)).count();
    shared as f32 / (own.len() as f32).sqrt()
}

/// 挑出这一轮要贴的样本。
///
/// 先按贴近程度取，再按文件顺序补足条数；已经出现在眼前这段记录里的（自己刚说过
/// 的）跳过，免得把上一句又教一遍。
fn pick<'a>(turns: &[Turn], samples: &[Sample<'a>]) -> Vec<&'a str> {
    let recent: String = turns
        .iter()
        .rev()
        .take(TOPIC_TURNS)
        .map(|turn| turn.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let topic = grams(&recent);
    // 已经出现在眼前这段记录里的（多半是自己刚说过的）不当标尺，补通用句的时候
    // 也不能把它捞回来——筛一遍就定下来，后面两个循环都用这一份。
    let eligible: Vec<(usize, &Sample<'_>)> = samples
        .iter()
        .enumerate()
        .filter(|(_, sample)| !recent.contains(sample.text))
        .collect();
    let mut ranked: Vec<(usize, f32)> = eligible
        .iter()
        .map(|(index, sample)| (*index, affinity(sample.text, &topic)))
        .collect();
    // 同分按文件顺序，结果才可复现。
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));

    let mut chosen: Vec<&str> = Vec::new();
    let mut used = 0;
    for (index, score) in &ranked {
        if chosen.len() >= MAX_LINES {
            break;
        }
        if *score <= 0.0 {
            break;
        }
        take(samples[*index].text, &mut chosen, &mut used);
    }
    // 冷门话题可能一条都贴不上：从开头补通用的，标尺不能空着。
    for (_, sample) in eligible.iter() {
        if chosen.len() >= MIN_LINES {
            break;
        }
        take(sample.text, &mut chosen, &mut used);
    }
    chosen
}

/// 收下一条样本，前提是它还没把这一轮的字数用光。
fn take<'a>(text: &'a str, chosen: &mut Vec<&'a str>, used: &mut usize) {
    let chars = text.chars().count();
    if *used + chars > MAX_CHARS {
        return;
    }
    *used += chars;
    chosen.push(text);
}

/// 这一轮贴进提示词的一段话；样本库为空时也返回空串。
pub(crate) fn brief(turns: &[Turn]) -> String {
    let samples = parse(VOICE);
    if samples.is_empty() {
        return String::new();
    }
    let picked = pick(turns, &samples);
    if picked.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = picked.iter().map(|text| format!("- {text}")).collect();
    format!(
        "你平时在群里就是这么说话的，照这个劲头、长短和口气说自己的话：\n{}\n",
        lines.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(text: &str) -> Turn {
        Turn {
            user_id: 7,
            name: "群友".into(),
            text: text.into(),
            ..Turn::default()
        }
    }

    #[test]
    fn only_dash_lines_become_samples() {
        let parsed = parse("# 标题\n\n说明文字\n- 牛逼克拉斯\n  - 缩进也算\n- \n");
        assert_eq!(
            parsed.iter().map(|s| s.text).collect::<Vec<_>>(),
            vec!["牛逼克拉斯", "缩进也算"]
        );
    }

    /// 样本库是编译进来的实物，必须真有东西，而且每条都得是能进提示词的一行。
    #[test]
    fn the_shipped_bank_is_not_empty_and_stays_in_one_line() {
        let samples = parse(VOICE);
        assert!(samples.len() >= 40, "样本只有 {} 条", samples.len());
        for sample in &samples {
            assert!(
                !sample.text.contains(['\n', '\\']),
                "样本里不该有换行或转义写法：{}",
                sample.text
            );
            assert!(sample.text.chars().count() <= 40, "样本太长：{}", sample.text);
        }
    }

    /// 话题贴题：聊折叠屏的时候，折叠那几条该排在前面。
    #[test]
    fn the_topic_decides_which_samples_come_first() {
        let samples = parse(VOICE);
        let picked = pick(&[turn("小米也卖折叠屏吗 折叠的好不好用")], &samples);
        assert!(
            picked.iter().any(|text| text.contains("折叠")),
            "{picked:?}"
        );
    }

    /// 自己刚说过的那条不该再当标尺贴回去。
    #[test]
    fn a_line_already_in_the_window_is_not_reused_as_a_sample() {
        let samples = parse(VOICE);
        let picked = pick(&[turn("牛逼克拉斯")], &samples);
        assert!(!picked.contains(&"牛逼克拉斯"), "{picked:?}");
    }

    /// 冷门话题也得有实物可看——补通用句，并且守住条数与字数。
    #[test]
    fn an_unrelated_topic_still_gets_enough_samples_within_budget() {
        let samples = parse(VOICE);
        let picked = pick(&[turn("zzz qqq 12345")], &samples);
        assert!(picked.len() >= MIN_LINES && picked.len() <= MAX_LINES, "{picked:?}");
        assert!(picked.iter().map(|t| t.chars().count()).sum::<usize>() <= MAX_CHARS);
        // 同一轮里不该出现两条一样的。
        let unique: HashSet<&&str> = picked.iter().collect();
        assert_eq!(unique.len(), picked.len(), "{picked:?}");
    }

    #[test]
    fn the_brief_names_itself_and_lists_samples() {
        let text = brief(&[turn("手机快没电了 有啥省电的办法")]);
        assert!(text.contains("你平时在群里就是这么说话的"), "{text}");
        assert!(text.lines().filter(|line| line.starts_with("- ")).count() >= MIN_LINES);
    }

    /// 样本是给人看的实物，不是禁令清单。
    #[test]
    fn the_brief_does_not_read_like_a_rulebook() {
        let text = brief(&[turn("笑死")]);
        for word in ["禁止", "不得", "必须", "不要", "不能"] {
            assert!(!text.contains(word), "样本说明里出现了禁令「{word}」：{text}");
        }
    }
}
