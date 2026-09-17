//! 号主本人手打的短句样本：发言前按当下话题与状态从实物里挑几条，当调子用。
//!
//! 人设写的是「他是谁」（形容词与规矩），这里放的是「他真这么说过」（实物）。
//! 样本比任何形容词都更能定住口吻与长度——模型是照着例子写字的，不是照着规矩。
//! 样本库随 [`crate::plugins::ambient`] 一起编译进来，改它就等于改风格；文件里的
//! 分节、调子标记与来源说明见 `res/ambient/voice.md`。
//!
//! 挑选做两件事。一是看这一轮群里在聊什么，把最贴题的那几条捞出来；二是看此刻的
//! 精神头（[`Register`]）：活跃的时候语气词多、想到哪说到哪，冷静克制的时候短而
//! 利落，同一个人这两个样子都有原话，挑错档就不像他了。冷门话题一条都贴不上时按
//! 文件顺序补几条通用的，保证每轮都有实物可看。

use super::mood::Register;
use crate::plugins::oai::chat::tone::{affinity, grams};
use super::window::Turn;

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

/// 样本库里的一条短句，以及它属于哪个调子（`None` 是两种状态都能用）。
#[derive(Debug, PartialEq, Eq)]
struct Sample<'a> {
    text: &'a str,
    register: Option<Register>,
}

/// `## 组名（活）` 里那个标记。`（静）` 是冷静那档，认不出的（含 `（通用）`）
/// 一律当两种状态都能用。
fn header_register(line: &str) -> Option<Register> {
    let name = line.trim_start_matches('#').trim();
    let inner = name.strip_suffix('）')?.rsplit_once('（')?.1;
    match inner {
        "活" => Some(Register::Lively),
        "静" => Some(Register::Calm),
        _ => None,
    }
}

/// 只认 `- ` 开头的行；`#` 分节标题、说明文字与空行都不进样本。
fn parse(raw: &str) -> Vec<Sample<'_>> {
    let mut register = None;
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            register = header_register(line);
            continue;
        }
        if let Some(text) = line.strip_prefix("- ").map(str::trim)
            && !text.is_empty()
        {
            out.push(Sample { text, register });
        }
    }
    out
}

/// 这条样本这会儿能不能用：调子对得上，或者它本来就两种状态都这么说。
fn fits(sample: &Sample<'_>, register: Register) -> bool {
    match sample.register {
        None => true,
        Some(tone) => register == Register::Even || tone == register,
    }
}

/// 挑出这一轮要贴的样本。
///
/// 先按贴近程度取，再按文件顺序补足条数；已经出现在眼前这段记录里的（自己刚说过
/// 的）跳过，免得把上一句又教一遍。
fn pick<'a>(turns: &[Turn], samples: &[Sample<'a>], register: Register) -> Vec<&'a str> {
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
    let eligible: Vec<(usize, &Sample<'a>)> = samples
        .iter()
        .enumerate()
        .filter(|(_, sample)| fits(sample, register) && !recent.contains(sample.text))
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

/// 这一轮贴进提示词的一段话：先交代「你今天是什么调子」，再摆原话。
///
/// 状态这一句不是废话——同一批原话里两种调子都有，不点明按哪档挑，模型会把松的
/// 和紧的混在一起写。公开出来是为了让测试能按开头的这句话认出「这段话在不在」。
pub(crate) fn opening(register: Register) -> &'static str {
    match register {
        Register::Lively => {
            "你今天话头松、语气词多，想到哪说到哪，有点冗余也没关系。这几句是你这么说话时的原话："
        }
        Register::Even => "你平时在群里就是这么说话的，照这个劲头、长短和口气说自己的话：",
        Register::Calm => {
            "你今天话说得紧、短而利落，一句话交代完就停。这几句是你这么说话时的原话："
        }
    }
}

/// 这一轮的样本段；样本库为空时返回空串。
pub(crate) fn brief(turns: &[Turn], register: Register) -> String {
    let samples = parse(VOICE);
    if samples.is_empty() {
        return String::new();
    }
    let picked = pick(turns, &samples, register);
    if picked.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = picked.iter().map(|text| format!("- {text}")).collect();
    format!("{}\n{}\n", opening(register), lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

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

    /// 分节标题上那个调子标记要认得出来，认不出的当「两种状态都能用」。
    #[test]
    fn the_section_header_carries_the_register() {
        let parsed = parse("## 夸与安利（活）\n- 雀氏稳定\n## 冷静克制（静）\n- 用过的都知道\n## 通用口气（通用）\n- 那好吧\n## 没标\n- 那没事了哈哈\n");
        let tones: Vec<Option<Register>> = parsed.iter().map(|s| s.register).collect();
        assert_eq!(
            tones,
            vec![
                Some(Register::Lively),
                Some(Register::Calm),
                None,
                None
            ]
        );
    }

    /// 样本库是编译进来的实物，必须真有东西，而且每条都得是能进提示词的一行。
    #[test]
    fn the_shipped_bank_is_not_empty_and_stays_in_one_line() {
        let samples = parse(VOICE);
        assert!(samples.len() >= 60, "样本只有 {} 条", samples.len());
        for sample in &samples {
            assert!(
                !sample.text.contains(['\n', '\\']),
                "样本里不该有换行或转义写法：{}",
                sample.text
            );
            assert!(sample.text.chars().count() <= 40, "样本太长：{}", sample.text);
        }
        // 两种调子都得有货，否则状态一切换就没得挑。
        for register in [Register::Lively, Register::Calm] {
            let count = samples
                .iter()
                .filter(|sample| fits(sample, register))
                .count();
            assert!(count >= 20, "{register:?} 这一档只有 {count} 条");
        }
    }

    /// 样本库的胖瘦也是形状，而形状的标尺是他本人，不是一个好记的整数。
    ///
    /// 2026-09-15 用 `scripts/mine-voice.py --shape` 量过（滤掉词意猜词之后）：
    /// 他手打的消息 46.7% 在八个字以内、16.6% 在十八个字以上，中位数 9 个字。
    /// 所以库要落在他附近——养胖了，模型会把该分两条说的话并成一条长的；
    /// 修得太瘦，它连一句完整的话都写不出来，那同样不是他。
    #[test]
    fn the_bank_keeps_the_same_shape_as_the_man() {
        let samples = parse(VOICE);
        let total = samples.len();
        let short = samples
            .iter()
            .filter(|sample| sample.text.chars().count() <= 8)
            .count();
        let long = samples
            .iter()
            .filter(|sample| sample.text.chars().count() >= 18)
            .count();
        let short_share = short * 100 / total;
        assert!(
            (40..=55).contains(&short_share),
            "八个字以内的样本占 {short_share}%（{short}/{total}），他本人是 46.7%"
        );
        assert!(
            long * 100 / total <= 17,
            "十八个字以上的样本有 {long}/{total}，他本人是 16.6%"
        );
        // 兴奋时的「！」与话尾的「～」各有实物，否则模型会当它们不存在。
        assert!(samples.iter().any(|sample| sample.text.contains('！')));
        assert!(samples.iter().any(|sample| sample.text.contains('～')));
    }

    /// 一个词被当成句式反复套，是这类小模型最容易露的馅，而它不在长短里显形。
    ///
    /// 2026-09-18 量出来的：「包」当保证词用，他 60 天里只说了 6 次（2189 条留言的
    /// 0.37%），而线上输出里一度到七八个百分点——人设把「包是的」摆在「保证的口气」
    /// 头一个，库里又攒了三条实物，模型就把它读成「这是个可以随便套的句式」。
    /// 人设那边已把它挪到末位并写明「偶尔」，这条守着库里的那一半：实物只留一条。
    /// 豆包、表情包、红包是别的词，不算。
    #[test]
    fn the_bank_does_not_teach_one_surety_word_as_a_template() {
        const HEADS: [&str; 11] = [
            "是", "真", "厉", "root", "不", "有", "你", "冲", "得", "能", "会",
        ];
        // 这些字打头的「包」是名词（表情包你都要吐槽一番），不是保证。
        const BEFORE: [char; 9] = ['豆', '红', '面', '书', '背', '系', '行', '绿', '情'];
        let samples = parse(VOICE);
        let hits: Vec<&str> = samples
            .iter()
            .map(|sample| sample.text)
            .filter(|text| {
                text.char_indices().any(|(at, c)| {
                    c == '包'
                        && !text[..at]
                            .chars()
                            .next_back()
                            .is_some_and(|prev| BEFORE.contains(&prev))
                        && HEADS
                            .iter()
                            .any(|head| text[at + c.len_utf8()..].starts_with(head))
                })
            })
            .collect();
        assert!(
            hits.len() <= 1,
            "「包」当保证词的实物有 {} 条：{hits:?}——他是偶尔一句，不是一个句式",
            hits.len()
        );
    }

    /// 话题贴题：聊折叠屏的时候，折叠那几条该排在前面。
    #[test]
    fn the_topic_decides_which_samples_come_first() {
        let samples = parse(VOICE);
        let picked = pick(
            &[turn("小米也卖折叠屏吗 折叠的好不好用")],
            &samples,
            Register::Even,
        );
        assert!(picked.iter().any(|text| text.contains("折叠")), "{picked:?}");
    }

    /// 自己刚说过的那条不该再当标尺贴回去。
    #[test]
    fn a_line_already_in_the_window_is_not_reused_as_a_sample() {
        let samples = parse(VOICE);
        let picked = pick(&[turn("牛逼克拉斯")], &samples, Register::Even);
        assert!(!picked.contains(&"牛逼克拉斯"), "{picked:?}");
    }

    /// 冷门话题也得有实物可看——补通用句，并且守住条数与字数。
    #[test]
    fn an_unrelated_topic_still_gets_enough_samples_within_budget() {
        let samples = parse(VOICE);
        for register in [Register::Lively, Register::Even, Register::Calm] {
            let picked = pick(&[turn("zzz qqq 12345")], &samples, register);
            assert!(
                picked.len() >= MIN_LINES && picked.len() <= MAX_LINES,
                "{register:?} {picked:?}"
            );
            assert!(picked.iter().map(|t| t.chars().count()).sum::<usize>() <= MAX_CHARS);
            // 同一轮里不该出现两条一样的。
            let unique: HashSet<&&str> = picked.iter().collect();
            assert_eq!(unique.len(), picked.len(), "{picked:?}");
        }
    }

    /// 调子决定挑哪一档：同一次聊鸿蒙，松的时候「鸿蒙不太顶得住」上得来，紧的时候
    /// 它进不了场。
    ///
    /// 话题写得具体到那句话本身，是为了让这条钉子盯住「哪一档」，而不是「谁排第五」：
    /// 样本库会一轮轮变胖，泛泛聊鸿蒙时前五名谁属是浮动的。
    #[test]
    fn the_register_decides_which_shelf_the_samples_come_from() {
        let samples = parse(VOICE);
        let topic = [turn("鸿蒙系统 顶得住吗 太顶了")];
        let lively = pick(&topic, &samples, Register::Lively);
        let calm = pick(&topic, &samples, Register::Calm);
        assert!(lively.contains(&"鸿蒙不太顶得住"), "{lively:?}");
        assert!(!calm.contains(&"鸿蒙不太顶得住"), "{calm:?}");
        // 两档挑出来的都得跟当下的调子对得上（两边通用的除外）。
        for (register, picked) in [(Register::Lively, &lively), (Register::Calm, &calm)] {
            for text in picked.iter() {
                let sample = samples.iter().find(|s| s.text == *text).unwrap();
                assert!(fits(sample, register), "{register:?} 挑到了 {text}");
            }
        }
    }

    #[test]
    fn the_brief_names_itself_and_lists_samples() {
        let text = brief(&[turn("手机快没电了 有啥省电的办法")], Register::Even);
        assert!(text.contains("你平时在群里就是这么说话的"), "{text}");
        assert!(text.lines().filter(|line| line.starts_with("- ")).count() >= MIN_LINES);
        // 先说调子，再摆原话：状态不一样，这段话就不一样。
        assert!(brief(&[], Register::Calm).contains("短而利落"), "{}", brief(&[], Register::Calm));
        assert!(brief(&[], Register::Lively).contains("语气词多"), "{}", brief(&[], Register::Lively));
    }

    /// 样本是给人看的实物，不是禁令清单。
    #[test]
    fn the_brief_does_not_read_like_a_rulebook() {
        for register in [Register::Lively, Register::Even, Register::Calm] {
            let text = brief(&[turn("笑死")], register);
            for word in ["禁止", "不得", "必须", "不要", "不能"] {
                assert!(!text.contains(word), "样本说明里出现了禁令「{word}」：{text}");
            }
        }
    }
}
