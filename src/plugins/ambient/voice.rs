//! 号主本人手打的短句样本：发言前按当下话题与状态从实物里挑几条，当调子用。
//!
//! 人设写的是「他是谁」（形容词与规矩），这里放的是「他真这么说过」（实物）。
//! 样本比任何形容词都更能定住口吻与长度——模型是照着例子写字的，不是照着规矩。
//! 样本库随 [`crate::plugins::ambient`] 一起编译进来，改它就等于改风格；文件里的
//! 分节、调子标记与来源说明见 `res/ambient/voice.md`。
//!
//! 挑选看两件事。一是此刻的精神头（[`Register`]）：活跃的时候语气词多，冷静克制
//! 的时候短而利落，同一个人这两个样子都有原话，挑错档就不像他了。二是**避开**
//! 眼前的话题：样本教的是口气，不是立场，贴题的样本会被模型当成此刻要表的态
//! （见 [`pick`]）。

use super::mood::Register;
use crate::plugins::oai::chat::tone::{affinity, grams};
use super::window::Turn;

/// 编译进来的样本库。加一条就是加一个标尺，改风格先改它。
const VOICE: &str = include_str!("../../../res/ambient/voice.md");

/// 一次最多贴几条。样本是标尺不是范文，三五条足够定调。
const MAX_LINES: usize = 5;
/// 一次最少贴几条（库够大，随机取总能取满；测试守着这条线）。
#[cfg(test)]
const MIN_LINES: usize = 3;
/// 贴出来的样本一共占多少字上限。它们跟着每轮的账单走。
const MAX_CHARS: usize = 220;
/// 只看最近的这些条消息来猜话题；再往前的时间隔得远，聊的多半是另一码事。
const TOPIC_TURNS: usize = 12;
/// 贴题到这个程度的样本不挑：再往上，模型就会把样本里的立场当成自己此刻的话。
const OFF_TOPIC: f32 = 0.5;

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
/// 从前按贴题程度挑：聊华为就贴他说过的华为那几句。结果模型学走的不是口气，
/// 是内容——「什么时候华为上双击熄屏 我就回归」被原样改写成一句立场，塞进一段
/// 根本没人问它意见的聊天里（线上 2026-09-26 10:28）。样本只该教「怎么说」，所以
/// 现在反过来：**跟眼前话题沾边的一律不挑**，在剩下的里随机取，长短搭配着摆。
/// 已经出现在眼前这段记录里的（自己刚说过的）同样跳过。
fn pick<'a>(turns: &[Turn], samples: &[Sample<'a>], register: Register) -> Vec<&'a str> {
    use rand::seq::SliceRandom;
    let recent: String = turns
        .iter()
        .rev()
        .take(TOPIC_TURNS)
        .map(|turn| turn.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let topic = grams(&recent);
    // 「沾边」看的是实词：两个字都不是虚字的字组（华为、折叠、手机），或者整个的
    // 英文数字词（mate90、root）。「不是」「怎么」这种满库都是，不算话题。
    let words = content_words(&recent);
    let on_topic = |text: &str| {
        !content_words(text).is_disjoint(&words) || affinity(text, &topic) >= OFF_TOPIC
    };
    let mut eligible: Vec<&Sample<'a>> = samples
        .iter()
        .filter(|sample| fits(sample, register) && !recent.contains(sample.text))
        .filter(|sample| !on_topic(sample.text))
        .collect();
    eligible.shuffle(&mut rand::rng());
    // 短句先占两个位置：他近一半的话在八个字以内，一屋子长句样本会把输出养胖。
    let (short, long): (Vec<&Sample<'a>>, Vec<&Sample<'a>>) = eligible
        .into_iter()
        .partition(|sample| sample.text.chars().count() <= 8);
    let mut order: Vec<&Sample<'a>> = short.iter().take(2).copied().collect();
    let mut rest: Vec<&Sample<'a>> = short.into_iter().skip(2).chain(long).collect();
    rest.shuffle(&mut rand::rng());
    order.extend(rest);

    let mut chosen: Vec<&str> = Vec::new();
    let mut used = 0;
    for sample in order {
        if chosen.len() >= MAX_LINES {
            break;
        }
        take(sample.text, &mut chosen, &mut used);
    }
    chosen
}

/// 虚字：它们组成的字组说明不了在聊什么。
const FUNCTION_CHARS: &str = "的了是不我你他她它们这那就还也都在有没么吗吧呢啊哈嘛呀哦个一二两上下来去说要会能可以到得着过把被给让很太真好对啥什怎样点些里时候看想又再才而且但就算然后";

/// 一段话里的实词：中文按相邻两字取，两字都不是虚字才算；英文数字按整词取。
fn content_words(text: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let chars: Vec<char> = text.chars().map(|c| c.to_ascii_lowercase()).collect();
    let mut word = String::new();
    for (index, c) in chars.iter().enumerate() {
        if c.is_ascii_alphanumeric() {
            word.push(*c);
            continue;
        }
        if word.chars().count() >= 2 {
            out.insert(std::mem::take(&mut word));
        }
        word.clear();
        let Some(next) = chars.get(index + 1) else {
            continue;
        };
        let cjk = |c: &char| c.is_alphabetic() && !c.is_ascii();
        if cjk(c) && cjk(next) && !FUNCTION_CHARS.contains(*c) && !FUNCTION_CHARS.contains(*next)
        {
            out.insert([*c, *next].iter().collect());
        }
    }
    if word.chars().count() >= 2 {
        out.insert(word);
    }
    out
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
            "你今天话头松、语气词多一点。下面几句是你平时的原话，借它们的口气和长短，内容跟眼下聊的无关："
        }
        Register::Even => {
            "你平时在群里就是这么说话的。下面几句原话只借口气和长短，内容跟眼下聊的无关："
        }
        Register::Calm => {
            "你今天话说得紧、短而利落，一句话交代完就停。下面几句是你平时的原话，借口气和长短，内容跟眼下聊的无关："
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

    #[test]
    fn content_words_skip_function_pairs() {
        let words = content_words("华为新机 mate90 这次是不是换了芯片");
        for word in ["华为", "新机", "mate90", "芯片"] {
            assert!(words.contains(word), "{word} {words:?}");
        }
        for word in ["是不", "不是", "这次"] {
            assert!(!words.contains(word), "{word} {words:?}");
        }
    }

    /// 样本教口气，不教立场：聊华为的时候，他说过的华为那几句一条都不该上。
    #[test]
    fn samples_on_the_current_topic_stay_out() {
        let samples = parse(VOICE);
        assert!(samples.iter().any(|sample| sample.text.contains("华为")));
        for _ in 0..20 {
            let picked = pick(
                &[turn("华为新机十月一发布 mate90 这次芯片换没换")],
                &samples,
                Register::Even,
            );
            assert!(!picked.iter().any(|text| text.contains("华为")), "{picked:?}");
            assert!(picked.len() >= MIN_LINES, "{picked:?}");
            // 短句要有：他近一半的话在八个字以内。
            assert!(picked.iter().any(|text| text.chars().count() <= 8), "{picked:?}");
        }
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

    /// 调子决定挑哪一档：挑出来的都得跟当下的调子对得上（两边通用的除外）。
    #[test]
    fn the_register_decides_which_shelf_the_samples_come_from() {
        let samples = parse(VOICE);
        for _ in 0..20 {
            for register in [Register::Lively, Register::Calm] {
                for text in pick(&[turn("随便聊聊")], &samples, register) {
                    let sample = samples.iter().find(|s| s.text == text).unwrap();
                    assert!(fits(sample, register), "{register:?} 挑到了 {text}");
                }
            }
        }
    }

    #[test]
    fn the_brief_names_itself_and_lists_samples() {
        let text = brief(&[turn("手机快没电了 有啥省电的办法")], Register::Even);
        assert!(text.contains("你平时在群里就是这么说话的"), "{text}");
        assert!(text.contains("内容跟眼下聊的无关"), "{text}");
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
