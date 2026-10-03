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
    pub reply_to: Option<String>,
    /// 发出前额外停顿的秒数（模型显式要求的 `[wait:n]`）。
    pub wait: f32,
    /// 不发消息，真戳这位群友一下（`[poke:QQ号]`）。
    ///
    /// 消息里的「戳一戳」是一枚超级表情，要作为一条消息发进群；群友戳人用的是点头像，
    /// 对方那边弹的是「戳了你」。从前这里照着消息元素发，群里就多出一条孤零零的
    /// `[戳一戳]`（2026-10-03 09:39 线上记录），一眼不是人。所以它不再是消息，而是动作：
    /// 发的时候走平台的戳一戳接口（见 `ambient::deliver`）。
    pub nudge: Option<String>,
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

/// 记录里戳一戳的占位写法：`[戳一戳]`、`[戳一戳 123456]`、`[戳一戳:123456]`，以及入站事件
/// 那种 `[戳一戳：甲 戳了 乙]`。
///
/// 那是记录对动作的描述，不是发出去的标记；模型照着自己的历史把它当正文写出来，
/// 群里就多出一条文字「[戳一戳]」。带号码的还原成真戳，不带号码的无从知道戳谁，丢掉。
fn poke_note() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^[\[［]戳一戳(?:\s*[:：]?\s*(\d{5,12})|[:：][^\]］]{0,80})?[\]］]$").unwrap()
    })
}

/// 这段文字是不是只有一个戳一戳占位符。
pub(crate) fn is_poke_placeholder(text: &str) -> bool {
    poke_note().is_match(text.trim())
}

/// 解析出来但还没定形的一条：动作照原样，文字要先等断句分完剩下的额度。
enum Draft {
    Act(Message),
    Nudge(String),
    Text {
        body: String,
        reply: bool,
        reply_to: Option<String>,
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
        if let Some(caps) = poke_note().captures(line) {
            if drafts.len() < max_messages
                && let Some(target) = caps.get(1)
            {
                drafts.push((
                    Draft::Nudge(target.as_str().to_string()),
                    std::mem::take(&mut pending_wait),
                ));
            }
            continue;
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
            let draft = match caps.get(2) {
                Some(target) => Draft::Nudge(target.as_str().to_string()),
                None if caps[1].starts_with("dice") => Draft::Act(Message::new().dice()),
                None => Draft::Act(Message::new().rps()),
            };
            drafts.push((draft, std::mem::take(&mut pending_wait)));
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
                nudge: None,
            }),
            Draft::Nudge(user) => out.push(Utterance {
                message: Message::new(),
                chars: 0,
                reply: false,
                reply_to: None,
                wait,
                nudge: Some(user),
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
                        reply_to: if index == 0 { reply_to.clone() } else { None },
                        wait: if index == 0 { wait } else { 0.0 },
                        nudge: None,
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
fn reply_prefix(line: &str) -> (bool, Option<String>, &str) {
    let Some(rest) = line.strip_prefix("[reply") else {
        return (false, None, line);
    };
    if let Some(rest) = rest.strip_prefix(']') {
        return (true, None, rest.trim_start());
    }
    if let Some(rest) = rest.strip_prefix(':')
        && let Some((id, rest)) = rest.split_once(']')
        && !id.trim().is_empty()
    {
        return (true, Some(id.trim().to_string()), rest.trim_start());
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
