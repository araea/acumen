//! 把模型的一段输出拆成「像人一样发出去」的若干条消息，并给出每条的节奏。
//!
//! 两件事在这里合并处理，因为它们其实是一件事：群里发言的自然感一半来自内容
//! 怎么断句，一半来自这些断句之间隔了多久。模型只管写，断句与等待都在这里定。

use super::{breath, protocol};
use crate::message::Message;
use regex::Regex;
use std::sync::OnceLock;
use std::time::Duration;

/// 一条待发的消息。
#[derive(Debug)]
pub(crate) struct Utterance {
    pub message: Message,
    /// 正文字数，用来估算「打字」耗时；戳一戳、骰子这类没有打字过程。
    pub chars: usize,
    /// 以引用触发消息的形式发出。
    pub reply: bool,
    /// 要引哪条消息：模型写 `[reply:消息号]` 点名时是它，`[reply]` 时为 None
    /// （由发言侧退回本批默认目标）。
    pub reply_to: Option<i64>,
    /// 发出前额外停顿的秒数（模型显式要求的 `[wait:n]`）。
    pub wait: f32,
}

/// 模型这一轮的决定。
#[derive(Debug)]
pub(crate) enum Speech {
    /// 闭嘴。人设允许它随时改主意不说话。
    Silent,
    Say(Vec<Utterance>),
}

/// 显式停顿的上限，防止模型用一个 `[wait:9999]` 把发言拖到天荒地老。
const MAX_WAIT_SECONDS: f32 = 30.0;

fn markup() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[(at|face|img):([^\]]{1,512})\]").unwrap())
}

/// 模型自己写出来的 `@QQ号`。
///
/// 记录里同一个人有两种写法：`[at:QQ号]` 是标记，`@QQ号` 是历史遗留的渲染（入站消息
/// 与它自己过去那句都这么显示过）。人格照着第二种抄的时候，正文里就留下了一串光秃秃
/// 的号码（记录 id 105888）。渲染前把这种写法收进标记，两种抄法都能落到真 at 上。
fn bare_at() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"@(\d{5,12})").unwrap())
}

/// `@QQ号` → `[at:QQ号]`。
///
/// 只认五到十二位数字：QQ 号是这个长度，而正常的群里说话不会在 `@` 后面跟一串这么长的
/// 数字（`pass@1` 不够五位，邮箱的 `@` 后面不是数字）。前面紧挨着字母数字或 `[` 的也不动
/// ——`user@12345`、`[@12345]` 那是在说别的，改了就多出一层方括号。
fn mark_bare_ats<'a>(body: &'a str) -> std::borrow::Cow<'a, str> {
    use std::borrow::Cow;
    if !bare_at().is_match(body) {
        return Cow::Borrowed(body);
    }
    let mut out = String::with_capacity(body.len() + 8);
    let mut cursor = 0;
    let mut changed = false;
    for found in bare_at().find_iter(body) {
        let before = body[..found.start()].chars().next_back();
        if before.is_some_and(|c| c.is_alphanumeric() || c == '[' || c == '@') {
            continue;
        }
        out.push_str(&body[cursor..found.start()]);
        out.push_str("[at:");
        out.push_str(&found.as_str()[1..]);
        out.push(']');
        cursor = found.end();
        changed = true;
    }
    if !changed {
        return Cow::Borrowed(body);
    }
    out.push_str(&body[cursor..]);
    Cow::Owned(out)
}

/// 一行里所有标记所占的字符区间（半开），交给 [`breath`] 护住。
fn markup_spans(line: &str) -> Vec<std::ops::Range<usize>> {
    markup()
        .find_iter(line)
        .map(|found| {
            let start = line[..found.start()].chars().count();
            start..start + found.as_str().chars().count()
        })
        .collect()
}

fn action() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\[(poke:(\d{5,12})|dice|rps|wait:(\d+(?:\.\d+)?))\]$").unwrap())
}

/// 解析出来但还没定形的一条：动作照原样，文字要先等断句分完剩下的额度。
enum Draft {
    Act(Message),
    Text {
        body: String,
        reply: bool,
        reply_to: Option<i64>,
    },
}

/// 解析模型输出：一行一条消息，标记按 `satori-reply` skill 的约定翻译成消息段。
///
/// 行数是下限而不是上限：模型写了几行就是它自己分好的几条，没用完的消息额度
/// 交给 [`breath`] 去补——一口气写完的长句，在它自己的换气处切开再依次发出。
///
/// 不认识的方括号原样保留——群友本来就会打 `[笑]`，把它们吞掉比留着更糟。
pub(crate) fn parse(raw: &str, max_messages: usize, split_chars: usize) -> Speech {
    // 伪工具调用要在这里摘掉，不能等分完行、切完句：JSON 里的逗号是换气处，
    // 先断句会把 `[satori_action:…]` 或 `[send]parts:…` 切成两半，后半截照样漏进群。
    let raw = protocol::strip(raw);
    let raw: &str = &raw;
    let mut drafts: Vec<(Draft, f32)> = Vec::new();
    let mut pending_wait = 0.0_f32;

    for line in raw.lines() {
        let line = strip_decoration(line);
        if line.is_empty() {
            continue;
        }
        if line.eq_ignore_ascii_case("[silent]") {
            return Speech::Silent;
        }
        if let Some(caps) = action().captures(line) {
            if let Some(seconds) = caps.get(3) {
                pending_wait = (pending_wait + seconds.as_str().parse::<f32>().unwrap_or(0.0))
                    .min(MAX_WAIT_SECONDS);
                continue;
            }
            if drafts.len() >= max_messages {
                continue;
            }
            let message = match caps.get(2) {
                Some(target) => Message::new().poke(target.as_str()),
                None if caps[1].starts_with("dice") => Message::new().dice(),
                None => Message::new().rps(),
            };
            drafts.push((Draft::Act(message), std::mem::take(&mut pending_wait)));
            continue;
        }
        if drafts.len() >= max_messages {
            continue;
        }
        let (reply, reply_to, body) = reply_prefix(line);
        if body.is_empty() {
            continue;
        }
        drafts.push((
            Draft::Text {
                body: body.to_string(),
                reply,
                reply_to,
            },
            std::mem::take(&mut pending_wait),
        ));
    }

    // 没用完的额度就是还能换几次气；按出现顺序分给写得最长的那几行。
    let mut spare = max_messages.saturating_sub(drafts.len());
    let mut out: Vec<Utterance> = Vec::new();
    for (draft, wait) in drafts {
        match draft {
            Draft::Act(message) => out.push(Utterance {
                message,
                chars: 0,
                reply: false,
                reply_to: None,
                wait,
            }),
            Draft::Text {
                body,
                reply,
                reply_to,
            } => {
                // 标记本身不能切，但带标记的长句照样要换气：护住 `[at:…]`、`[img:…]`
                // 这些 token 的下标，标记之外该切还切。从前是整行不动，于是一句
                // `[at:…] + 一长段` 会原样发成一条几百字不带标点的长文。
                let shield = markup_spans(&body);
                let pieces = breath::split_protected(&body, spare + 1, split_chars, &shield);
                spare = spare.saturating_sub(pieces.len().saturating_sub(1));
                for (index, piece) in pieces.into_iter().enumerate() {
                    let (message, chars) = build_message(&piece);
                    if message.0.is_empty() {
                        continue;
                    }
                    out.push(Utterance {
                        message,
                        chars,
                        // 引用只挂在第一条上：后面几条是同一口气里接着说的。
                        reply: reply && index == 0,
                        reply_to: if index == 0 { reply_to } else { None },
                        wait: if index == 0 { wait } else { 0.0 },
                    });
                }
            }
        }
    }

    if out.is_empty() {
        Speech::Silent
    } else {
        Speech::Say(out)
    }
}

/// 行首的引用前缀：`[reply]` 引本批默认目标，`[reply:消息号]` 点名引某一条。
///
/// 返回「是否引用、点名的消息号、剩下要发的正文」。号码写得不合法时整行原样当文字
/// 处理——群友本来就会打方括号，认不出来的那种留着比吞掉好。
fn reply_prefix(line: &str) -> (bool, Option<i64>, &str) {
    let Some(rest) = line.strip_prefix("[reply") else {
        return (false, None, line);
    };
    if let Some(rest) = rest.strip_prefix(']') {
        return (true, None, rest.trim_start());
    }
    if let Some(rest) = rest.strip_prefix(':')
        && let Some((id, rest)) = rest.split_once(']')
        && let Ok(id) = id.trim().parse::<i64>()
        && id != 0
    {
        return (true, Some(id), rest.trim_start());
    }
    (false, None, line)
}

/// 去掉模型偶尔带上的代码围栏、列表符号与首尾空白。
fn strip_decoration(line: &str) -> &str {
    let line = line.trim();
    if line.starts_with("```") {
        return "";
    }
    let line = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .unwrap_or(line);
    line.trim()
}

/// 工具参数里的一段文字 → 消息段。
///
/// 工具路径本该用结构化元素，但模型偶尔把兼容标记（`[at:…]`、`[face:…]`、
/// `[img:…]`）写进 `text` 里。原样发出去群里就看见一串方括号——线上记录
/// id 79077 的 `[face:277]` 就是这么漏的。这里复用文字路径的翻译：只认合法
/// 标记，`[笑]` 这类不认识的方括号仍旧是文字。
pub(crate) fn text_segments(text: &str) -> Message {
    // 工具路径的 text 也可能混进伪调用；能在这里摘就先摘，`split_send` 那条
    // 提前断句的路径另有处理。
    let text = protocol::strip(text);
    build_message(&text).0
}

/// 一行文本 → 消息段 + 正文字数。
fn build_message(body: &str) -> (Message, usize) {
    // 字面的 `\n` 在这儿就还原成真换行，后面按标记定位的字节下标才对得上。
    let body = super::literal_newlines(body);
    let body = mark_bare_ats(&body);
    let body: &str = &body;
    let mut message = Message::new();
    let mut chars = 0usize;
    let mut cursor = 0usize;

    let push_text = |message: Message, text: &str, chars: &mut usize| {
        if text.is_empty() {
            return message;
        }
        *chars += text.chars().count();
        message.text(text)
    };

    for caps in markup().captures_iter(body) {
        let whole = caps.get(0).expect("regex match has group 0");
        message = push_text(message, &body[cursor..whole.start()], &mut chars);
        cursor = whole.end();
        let value = caps[2].trim();
        message = match &caps[1] {
            "at" if value.chars().all(|c| c.is_ascii_digit()) && !value.is_empty() => {
                chars += 4;
                // QQ 的 at 段不带空格，模型写没写这一下不保证：缺了才补一个，
                // 已经留了空白的、后面没话的，都不再添。
                let rest = &body[cursor..];
                let message = message.at(value);
                if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                    message
                } else {
                    message.text(" ")
                }
            }
            "face" if value.chars().all(|c| c.is_ascii_digit()) && !value.is_empty() => {
                message.face(value)
            }
            "img" if value.starts_with("http://") || value.starts_with("https://") => {
                message.image(value)
            }
            // 参数不合法就当普通文字，别悄悄吞掉内容。
            _ => push_text(message, whole.as_str(), &mut chars),
        };
    }
    message = push_text(message, &body[cursor..], &mut chars);
    (message, chars)
}

/// 发言节奏。
pub(crate) struct Pace {
    /// 逐字打字速度（字/分钟）。
    pub typing_cpm: u32,
    /// 长句改用语音输入时的等效速度（字/分钟）。
    pub voice_cpm: u32,
    /// 看完消息到开始打字之间的思考时间（秒）。
    pub think_seconds: f32,
}

impl Default for Pace {
    /// 不特别交代时的节奏：手打 150 字/分、长句按 420 字/分当语音、想三秒。
    fn default() -> Self {
        Self {
            typing_cpm: 150,
            voice_cpm: 420,
            think_seconds: 3.0,
        }
    }
}

impl Pace {
    /// 想好之前的停顿。模型已经花掉的时间算作思考，不再重复等待。
    pub(crate) fn think_delay(&self, elapsed: Duration) -> Duration {
        let target = jitter(self.think_seconds.max(0.0), 0.45);
        seconds(target - elapsed.as_secs_f32())
    }

    /// 敲完这条消息要多久。
    pub(crate) fn typing_delay(&self, chars: usize) -> Duration {
        if chars == 0 {
            // 戳一戳、骰子：抬手就发，没有打字过程。
            return seconds(jitter(0.8, 0.4));
        }
        let cpm = self.effective_cpm(chars).max(30.0);
        seconds(jitter(chars as f32 * 60.0 / cpm, 0.3).clamp(1.2, 40.0))
    }

    /// 两条消息之间的换气。
    ///
    /// 偶尔会长出一截：手机上打着字被别的事岔开一下，是群聊里最常见的停顿，
    /// 而每条都精确地隔一秒才是机器的样子。
    pub(crate) fn gap(&self) -> Duration {
        if rand::random::<f32>() < 0.15 {
            return seconds(jitter(3.2, 0.6));
        }
        seconds(jitter(0.9, 0.5))
    }

    /// 短句一个字一个字敲；长句更像按住语音键一口气说完再转写，字均耗时更低。
    fn effective_cpm(&self, chars: usize) -> f32 {
        let typing = self.typing_cpm.max(1) as f32;
        let voice = self.voice_cpm.max(self.typing_cpm) as f32;
        let ratio = ((chars as f32 - 12.0) / 48.0).clamp(0.0, 1.0);
        typing + (voice - typing) * ratio
    }
}

/// 在 `value` 上下浮动 `spread` 比例。
fn jitter(value: f32, spread: f32) -> f32 {
    value * (1.0 - spread + rand::random::<f32>() * spread * 2.0)
}

fn seconds(value: f32) -> Duration {
    Duration::from_secs_f32(value.clamp(0.0, 120.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use simd_json::base::ValueAsScalar;

    fn text_of(item: &Utterance) -> String {
        item.message
            .0
            .iter()
            .filter_map(|segment| segment.data.get("text").and_then(|v| v.as_str()))
            .collect()
    }

    fn say(raw: &str) -> Vec<Utterance> {
        match parse(raw, 3, 0) {
            Speech::Say(items) => items,
            Speech::Silent => panic!("expected speech, got silence: {raw}"),
        }
    }

    #[test]
    fn silence_wins_over_anything_else_on_the_line() {
        assert!(matches!(parse("[silent]", 3, 0), Speech::Silent));
        assert!(matches!(parse("  \n\n  ", 3, 0), Speech::Silent));
        assert!(matches!(parse("说点什么\n[silent]", 3, 0), Speech::Silent));
    }

    #[test]
    fn each_line_becomes_a_message_and_extra_lines_are_dropped() {
        let items = say("你确定？\n- 那你重读第二段\n第三条\n第四条");
        assert_eq!(items.len(), 3);
        assert_eq!(
            items[1].message.0[0].data.get("text").unwrap(),
            "那你重读第二段"
        );
        assert!(items.iter().all(|item| !item.reply));
    }

    #[test]
    fn markup_becomes_segments_and_reply_marks_the_quote() {
        // 模型没在 `@` 后面留空格，这里替它补上——at 段自己不带那一下。
        let items = say("[reply][at:114514]第三步缺了个前提 [face:178]");
        assert_eq!(items.len(), 1);
        assert!(items[0].reply);
        assert_eq!(items[0].reply_to, None, "光写 [reply] 不点名，交给发言侧定默认目标");
        let kinds: Vec<&str> = items[0]
            .message
            .0
            .iter()
            .map(|segment| segment.type_.as_str())
            .collect();
        assert_eq!(kinds, ["at", "text", "text", "face"]);
        assert_eq!(text_of(&items[0]), " 第三步缺了个前提 ");
        assert!(items[0].chars > 4);
    }

    /// 两条图片消息挨着来时，模型得能点名引哪一条：`[reply:消息号]` 把目标带出来。
    #[test]
    fn an_explicit_reply_prefix_names_the_target_message() {
        let items = say("[reply:61150]这张我上个月拍过同款");
        assert_eq!(items.len(), 1);
        assert!(items[0].reply);
        assert_eq!(items[0].reply_to, Some(61150));
        assert_eq!(text_of(&items[0]), "这张我上个月拍过同款");

        // 号码不合法时整行原样当文字，不吞内容也不谎报目标。
        for raw in ["[reply:abc] 上面那张", "[reply:] 上面那张", "[reply:0] 上面那张"] {
            let items = say(raw);
            assert!(!items[0].reply, "{raw}");
            assert_eq!(items[0].reply_to, None, "{raw}");
            assert_eq!(text_of(&items[0]), raw);
        }
        // `[reply]` 后面的正文照旧去掉前缀。
        let items = say("[reply]上面那张");
        assert!(items[0].reply);
        assert_eq!(text_of(&items[0]), "上面那张");
    }

    /// `@` 后面该有几个空格就是几个：模型自己留了就不再添，后面没话也不补。
    #[test]
    fn the_gap_after_an_at_is_never_doubled() {
        let items = say("[at:114514] 那你说");
        assert_eq!(items[0].message.0.len(), 2);
        assert_eq!(text_of(&items[0]), " 那你说");

        // 一条光秃秃的 `@` 不拖一个尾空格。
        let items = say("[at:114514]");
        assert_eq!(items[0].message.0.len(), 1);
        assert_eq!(items[0].message.0[0].type_, "at");
    }

    /// 带 `[at:…]` 的长句照样要换气：护住标记，标记之外照切。
    #[test]
    fn a_long_line_with_markup_still_breathes() {
        let raw = "[at:114514] 第一步把依赖装上 第二步重跑一次 第三步贴出错的第一行 别把整个日志都发出来";
        let Speech::Say(items) = parse(raw, 3, 14) else {
            panic!("expected speech");
        };
        assert!(items.len() > 1, "{items:?}");
        // @ 留在第一条上，标记本身没被切开。
        assert_eq!(items[0].message.0[0].type_, "at");
        assert!(
            items
                .iter()
                .all(|item| text_of(item).matches('[').count() == text_of(item).matches(']').count()),
            "{items:?}"
        );
    }

    /// 工具路径传进来的文字也要走同一套标记翻译，否则 `[face:277]` 会原样进群。
    #[test]
    fn tool_text_gets_the_same_markup_translation() {
        let message = text_segments("行 下次轮到你站中间那格[face:277]");
        let kinds: Vec<&str> = message.0.iter().map(|s| s.type_.as_str()).collect();
        assert_eq!(kinds, ["text", "face"]);
        assert_eq!(message.0[1].data.get("id").unwrap(), "277");
        // 认不出的方括号不动它。
        let plain = text_segments("[笑] 收到");
        assert_eq!(plain.0.len(), 1);
        assert_eq!(plain.0[0].data.get("text").unwrap(), "[笑] 收到");
    }

    /// 模型把 `satori_action` 当正文写出来时，发出去的是里面的那句话，不是 JSON。
    /// 线上记录 id 134245：群里真的看见过一串 `[satori_action:{"request"…`，
    /// 几个人还照着抄了一遍。
    #[test]
    fn a_tool_call_written_as_text_never_reaches_the_group() {
        let raw = r#"[satori_action:{"request":{"action":"send","parts":[{"type":"text","text":"昇腾这单我还真算过"}]}}]"#;
        let items = say(raw);
        assert_eq!(items.len(), 1);
        assert_eq!(text_of(&items[0]), "昇腾这单我还真算过");
        assert!(!text_of(&items[0]).contains("satori_action"));

        // 模型写到一半被截断的残片：一行都不发。
        let raw = r#"[satori_action:{"request":{"action":"send","parts":[{"type":"text""#;
        assert!(matches!(parse(raw, 3, 60), Speech::Silent));

        // 工具路径传进来的 text 走同一套清洗。
        let message = text_segments(
            r#"[satori_action:{"request":{"action":"send","parts":[{"type":"text","text":"行 我看看"}]}}]"#,
        );
        let text: String = message
            .0
            .iter()
            .filter_map(|s| s.data.get("text").and_then(|v| v.as_str()))
            .collect();
        assert_eq!(text, "行 我看看");
    }

    #[test]
    fn malformed_xml_call_cannot_turn_into_two_group_messages() {
        let raw = "<parameter name=\"request\">{\"action\":\"send\",\"parts\":[{\"type\":\"text\"\n\"text\":\"1.7 一度 这是服务区吧\"}],\"reply_to\":\"7689108452383409443\"}</parameter>";
        assert!(matches!(parse(raw, 3, 14), Speech::Silent));
        assert!(text_segments(raw).0.is_empty());
        let good = "<parameter name=\"request\">{\"action\":\"send\",\"parts\":[{\"type\":\"text\",\"text\":\"1.7 一度 这是服务区吧\"}]}</parameter>";
        let items = say(good);
        assert!(items.iter().map(text_of).collect::<String>().contains("服务区"));
        assert!(items.iter().all(|item| !text_of(item).contains("parameter")));
    }

    #[test]
    fn a_send_parts_call_does_not_leak_through_ambient_reply_parsing() {
        let raw = r#"[send]parts:[{"text":"痔疮还带揽客的呀","type":"text"},{"type":"sticker","id":1}]"#;
        let items = say(raw);
        assert_eq!(items.len(), 1);
        assert_eq!(text_of(&items[0]), "痔疮还带揽客的呀");
    }

    /// 清洗必须排在断句之前：JSON 里的逗号在 [`breath`] 眼里是换气处，
    /// 先切会把标记切成两半，后半截没有名字，照样漏进群。
    #[test]
    fn a_pseudo_call_is_stripped_before_its_commas_are_cut() {
        let raw = r#"[satori_action:{"request":{"action":"send","parts":[{"type":"text","text":"这个报错我刚翻到了 是驱动装岔了版本 你把显卡驱动回退一版再试"}]}}]"#;
        let Speech::Say(items) = parse(raw, 3, 20) else {
            panic!("expected speech");
        };
        let all: String = items.iter().map(text_of).collect::<Vec<_>>().join("");
        assert!(!all.contains("satori_action"), "{all}");
        assert!(!all.contains("parts"), "{all}");
        assert!(all.contains("驱动"), "{all}");
    }

    /// 模型把 `@QQ号` 照着记录抄进正文时，也得变成真的 at：线上记录 id 105888 的
    /// 「@3938463481 兄弟说到点子上了」就是这么漏出去的。
    #[test]
    fn a_bare_qq_mention_becomes_a_real_at() {
        let items = say("@3938463481 兄弟说到点子上了");
        let kinds: Vec<&str> = items[0]
            .message
            .0
            .iter()
            .map(|segment| segment.type_.as_str())
            .collect();
        assert_eq!(kinds, ["at", "text"]);
        assert_eq!(items[0].message.0[0].data.get("qq").unwrap(), "3938463481");
        assert_eq!(text_of(&items[0]), " 兄弟说到点子上了");
        // 工具路径传进来的 text 走同一套翻译。
        let message = text_segments("@3938463481 行 下次");
        assert_eq!(message.0[0].type_, "at");
    }

    /// 只有「@ + 五到十二位数字」才算提及：短数字、邮箱、已经是标记的都不动。
    #[test]
    fn only_a_long_standalone_number_after_at_reads_as_a_mention() {
        for raw in [
            "75 pass@1 有点离谱",
            "发我 user@12345 那个邮箱",
            "[@3938463481] 这是别处的写法",
        ] {
            let message = text_segments(raw);
            assert!(
                message.0.iter().all(|segment| segment.type_ == "text"),
                "{raw} 被改成了 {message:?}"
            );
        }
    }

    #[test]
    fn unknown_brackets_stay_as_text() {
        let items = say("[笑] [at:abc] [face:] 收到");
        let text: String = items[0]
            .message
            .0
            .iter()
            .filter_map(|segment| segment.data.get("text").and_then(|v| v.as_str()))
            .collect();
        assert_eq!(text, "[笑] [at:abc] [face:] 收到");
    }

    #[test]
    fn actions_and_waits_attach_to_the_next_message() {
        let items = say("[wait:3]\n[poke:114514]\n[dice]");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].message.0[0].type_, "poke");
        assert_eq!(items[0].chars, 0);
        assert!((items[0].wait - 3.0).abs() < f32::EPSILON);
        assert_eq!(items[1].message.0[0].type_, "dice");
        assert_eq!(items[1].wait, 0.0);
    }

    /// 模型写一行、群里看到三条：没用完的额度拿去换气。
    #[test]
    fn one_breathless_line_spends_the_unused_message_budget() {
        let raw = "坟挖得挺熟练 一看就不是第一次爬出来所以 Pro 比 Flash 强在哪 强在它不承认自己死了";
        let Speech::Say(items) = parse(raw, 3, 22) else {
            panic!("expected speech");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(text_of(&items[1]), "强在它不承认自己死了");
        // 关掉分段就回到一行一条。
        let Speech::Say(single) = parse(raw, 3, 0) else {
            panic!("expected speech");
        };
        assert_eq!(single.len(), 1);
    }

    /// 模型自己分好的几行优先：额度先满足它写的行数，剩下的才拿去断句。
    #[test]
    fn the_models_own_line_breaks_come_first() {
        let long = "那个报错我刚翻到了 是驱动装岔了版本 你把显卡驱动回退一版再试";
        let Speech::Say(items) = parse(&format!("{long}\n{long}\n{long}"), 3, 12) else {
            panic!("expected speech");
        };
        assert_eq!(items.len(), 3, "三行已经占满额度，不再替它换气");
        let Speech::Say(items) = parse(&format!("{long}\n{long}"), 3, 12) else {
            panic!("expected speech");
        };
        assert_eq!(items.len(), 3, "剩一条额度，给写在前面的那行");
    }

    /// 带 `[at:…]`、`[img:…]` 的行整条发：切开之后那几条会变成另一个意思。
    #[test]
    fn lines_carrying_markup_are_never_cut() {
        let items = say("[at:114514] 这事我刚查过 版本号对不上 你回退一版再试试看行不行");
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn waits_are_capped() {
        let items = say("[wait:9999]\n算了");
        assert!(items[0].wait <= MAX_WAIT_SECONDS);
    }


    #[test]
    fn long_lines_type_faster_per_character_but_never_instantly() {
        let pace = Pace {
            typing_cpm: 150,
            voice_cpm: 420,
            think_seconds: 3.0,
        };
        let short = pace.typing_delay(6);
        let long = pace.typing_delay(120);
        assert!(short >= Duration::from_millis(1_200), "{short:?}");
        assert!(long > short, "{long:?} vs {short:?}");
        assert!(pace.effective_cpm(120) > pace.effective_cpm(6));
        // 模型已经想了很久，就不必再假装思考。
        assert_eq!(
            pace.think_delay(Duration::from_secs(30)),
            Duration::from_secs(0)
        );
        assert!(pace.think_delay(Duration::ZERO) > Duration::ZERO);
    }
}
