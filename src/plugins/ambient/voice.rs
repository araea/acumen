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
use super::recent;
pub(super) use crate::plugins::oai::chat::tone::content_words;
use crate::plugins::oai::chat::tone::{affinity, grams};
use super::window::Turn;

/// 编译进来的样本库。加一条就是加一个标尺，改风格先改它。
const VOICE: &str = include_str!("../../../res/ambient/voice.md");

/// 一次最多贴几条。样本是标尺不是范文，三五条足够定调。
const MAX_LINES: usize = 5;
/// 其中最多几条取自「近期原话」（见 [`recent`]）：样本库管他一贯怎么说，这几条管他最近怎么说。
const RECENT_SLOTS: usize = 2;
/// 贴出来的样本一共占多少字上限。它们跟着每轮的账单走。
const MAX_CHARS: usize = 220;
/// 近期原话那几条一共最多多少字。
const RECENT_CHARS: usize = 80;
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

/// 同 [`pick`]，只是条数与字数上限由调用方给（近期原话先占了几个位置）。
fn pick_within<'a>(
    turns: &[Turn],
    samples: &[Sample<'a>],
    register: Register,
    slots: usize,
    budget: usize,
) -> Vec<&'a str> {
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
        if chosen.len() >= slots {
            break;
        }
        take(sample.text, budget, &mut chosen, &mut used);
    }
    chosen
}

/// 样本库里的全部原话（近期那一层要避开它们）。
pub(crate) fn bank_lines() -> Vec<&'static str> {
    parse(VOICE).into_iter().map(|sample| sample.text).collect()
}

/// 从近期原话里抽几条：与样本库一样避开眼前的话题、不重复记录里已有的。
///
/// 近期原话没有调子标记（活 / 静）：它们是他这几天随手打的，两种状态都可能这么说。
/// 第一条尽量取自这个群：他在这群人面前怎么说话、怎么称呼群友，比别处的更对这群听众。
fn pick_recent(turns: &[Turn], recent: &[recent::Line], here: &[recent::Line]) -> Vec<String> {
    use rand::seq::SliceRandom;
    if recent.is_empty() {
        return Vec::new();
    }
    let window: String = turns
        .iter()
        .rev()
        .take(TOPIC_TURNS)
        .map(|turn| turn.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let topic = grams(&window);
    let words = content_words(&window);
    let usable = |line: &&recent::Line| {
        !window.contains(line.text.as_str())
            && content_words(&line.text).is_disjoint(&words)
            && affinity(&line.text, &topic) < OFF_TOPIC
    };
    let mut mine: Vec<&recent::Line> = here.iter().filter(usable).collect();
    let mut elsewhere: Vec<&recent::Line> = recent
        .iter()
        .filter(usable)
        .filter(|line| !mine.iter().any(|own| own.text == line.text))
        .collect();
    mine.shuffle(&mut rand::rng());
    elsewhere.shuffle(&mut rand::rng());
    let mut ordered: Vec<&recent::Line> = Vec::new();
    if !mine.is_empty() {
        ordered.push(mine.remove(0));
    }
    ordered.extend(elsewhere);
    ordered.extend(mine);
    let mut chosen = Vec::new();
    let mut used = 0;
    for line in ordered {
        if chosen.len() >= RECENT_SLOTS {
            break;
        }
        let chars = line.text.chars().count();
        if used + chars > RECENT_CHARS {
            continue;
        }
        used += chars;
        chosen.push(line.text.clone());
    }
    chosen
}

/// 「随口一句」要的调子：几条最短的原话（六个字以内），跟眼前话题无关。
///
/// 号主手打的消息有四分之一在四到六个字以内，短到只剩一个反应（「？」「笑死」
/// 「好可爱啊」）；机器人每次开口都带着内容，这一档几乎是空的。随口一句的提示词里只
/// 摆这几条，模型才写得出那么短的话。样本库里挑三条，再从他最近几天亲手打的短句里补
/// 两条（见 [`recent`]）：这几天挂在嘴边的「（」「绷不住了」，样本库里没有。
pub(crate) fn short_lines(
    turns: &[Turn],
    register: Register,
    count: usize,
    group: &str,
) -> Vec<String> {
    use rand::seq::SliceRandom;
    let samples = parse(VOICE);
    let recent_text: String = turns
        .iter()
        .rev()
        .take(TOPIC_TURNS)
        .map(|turn| turn.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let words = content_words(&recent_text);
    let short = |text: &str| (1..=6).contains(&text.chars().count());
    let mut shorts: Vec<&'static str> = samples
        .iter()
        .filter(|sample| fits(sample, register))
        .filter(|sample| short(sample.text))
        .filter(|sample| !recent_text.contains(sample.text))
        .filter(|sample| content_words(sample.text).is_disjoint(&words))
        .map(|sample| sample.text)
        .collect();
    shorts.shuffle(&mut rand::rng());
    let here = recent::lines_in(group);
    let mut fresh: Vec<recent::Line> = recent::lines()
        .into_iter()
        .chain(here)
        .filter(|line| short(&line.text))
        .filter(|line| !recent_text.contains(line.text.as_str()))
        .filter(|line| content_words(&line.text).is_disjoint(&words))
        .collect();
    fresh.sort_by(|a, b| a.text.cmp(&b.text));
    fresh.dedup_by(|a, b| a.text == b.text);
    fresh.shuffle(&mut rand::rng());
    // 这个群里说的先上。
    fresh.sort_by_key(|line| line.group != group);
    let fresh: Vec<String> = fresh.into_iter().map(|line| line.text).take(2).collect();
    let mut out: Vec<String> = fresh;
    out.extend(
        shorts
            .into_iter()
            .map(str::to_string)
            .take(count.saturating_sub(out.len())),
    );
    out.shuffle(&mut rand::rng());
    out
}

/// 收下一条样本，前提是它还没把这一轮的字数用光。
fn take<'a>(text: &'a str, budget: usize, chosen: &mut Vec<&'a str>, used: &mut usize) {
    let chars = text.chars().count();
    if *used + chars > budget {
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
pub(crate) fn brief(turns: &[Turn], register: Register, group: &str) -> String {
    use rand::seq::SliceRandom;
    let samples = parse(VOICE);
    if samples.is_empty() {
        return String::new();
    }
    let recent = pick_recent(turns, &recent::lines(), &recent::lines_in(group));
    let used: usize = recent.iter().map(|line| line.chars().count()).sum();
    let picked = pick_within(
        turns,
        &samples,
        register,
        MAX_LINES - recent.len(),
        MAX_CHARS.saturating_sub(used),
    );
    // 两处来的原话混在一起摆：哪几条是新的，对模型没有意义。
    let mut lines: Vec<String> = picked.iter().map(|text| text.to_string()).chain(recent).collect();
    if lines.is_empty() {
        return String::new();
    }
    lines.shuffle(&mut rand::rng());
    let lines: Vec<String> = lines.iter().map(|text| format!("- {text}")).collect();
    format!("{}\n{}\n", opening(register), lines.join("\n"))
}
