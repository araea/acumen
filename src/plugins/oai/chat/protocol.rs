//! 出站文字里的协议清洗。
//!
//! 模型偶尔不调用工具，而是**把工具调用当正文写出来**：
//!
//! ```text
//! [satori_action:{"request":{"action":"send","parts":[{"type":"text","text":"…"}]}}]
//! [send]parts:[{"type":"text","text":"…"},{"type":"sticker","id":1}]
//! ```
//!
//! 那是协议，不是要说的话；原样发进群，群友看到的是一串 JSON（线上记录 id 134245，
//! 群里几个人还照着抄了一遍；`[send]parts` 也在线上漏出过）。所以出站之前必须把它摘掉：`send` 的正文接过来当普通
//! 消息发，其余动作（戳一戳、撤回、管理）与读不到结尾的残片整段丢弃——从文字里执行
//! 动作会绕开额度与权限那两道闸，宁可不做。
//!
//! **清洗要排在断句之前。** JSON 里的逗号在 [`super::breath`] 眼里是换气处，先断句
//! 会把标记切成两半，后半截没有 `[satori_action:` 这个前缀，照样当正文漏进群。

use std::borrow::Cow;

/// 伪工具调用的开头。真工具调用走的是结构化 `tool_calls`，不会带这些标记。
const ACTION_MARKER: &str = "[satori_action:";
const SEND_MARKER: &str = "[send]parts:";
// 有的模型把工具调用写成 XML 参数块，甚至把 JSON 的逗号漏掉。
// 匹配不完整的开头也要拦：不能让半截参数在断句后变成两条群消息。
const PARAM_MARKER: &str = "<parameter name";

fn next_marker(raw: &str) -> Option<(usize, &str)> {
    [ACTION_MARKER, SEND_MARKER, PARAM_MARKER]
        .into_iter()
        .filter_map(|marker| raw.find(marker).map(|at| (at, marker)))
        .min_by_key(|(at, _)| *at)
}

/// 行首引用标记摘下来之后，它原本想引的是谁。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Quote {
    /// `[reply]`：引最后一条。
    Latest,
    /// `[reply:消息号]`：引点名那条。
    Message(String),
}

/// 摘掉每一行行首的 `[reply]` / `[reply:消息号]`，返回剩下的正文与第一个标记。
///
/// 那是文字路径的写法：行首写它，代码替你挂上引用。走工具发言时引用由
/// `reply_to` 参数管，模型却时常照着文字路径的习惯把标记也写进 `text`——
/// 于是群里看到一条带引用、正文开头还挂着「[reply] 」的消息（线上 2026-09-26
/// 12:55「[reply] 布丁你这是以貌取片」）。只认行首：句子中间出现的方括号
/// 多半是群友原话，不动。全角括号一并认，模型两种都写过。
pub(crate) fn take_reply_markers(text: &str) -> (Cow<'_, str>, Option<Quote>) {
    static MARKER: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let marker = MARKER.get_or_init(|| {
        regex::Regex::new(r"(?mi)^([ \t]*)[\[［]\s*reply\s*(?:[:：]\s*([^\]］\s]+)\s*)?[\]］][ \t]*")
            .unwrap()
    });
    let Some(first) = marker.captures(text) else {
        return (Cow::Borrowed(text), None);
    };
    let quote = match first.get(2) {
        Some(id) => Quote::Message(id.as_str().to_string()),
        None => Quote::Latest,
    };
    (marker.replace_all(text, "$1"), Some(quote))
}

/// 供应商把「拒绝 / 报错」当成回复正文递回来的几个固定句式。
///
/// 内容安全拦截、网关限流这类错误有的接口会以 200 + 一句话的形式返回，模型层看不出
/// 异常，那句话就一路走到发送——线上 2026-09-23 09:05 群里真的收到过一条
/// 「The request was rejected because it was considered high risk」。
/// 只收几句整句的特征，不收「rate limit」这种单词：群友问起限流时，说出来的话里
/// 可以有它。
pub(crate) fn is_provider_noise(text: &str) -> bool {
    let lowered = text.to_lowercase();
    [
        "request was rejected because it was considered",
        "considered high risk",
        "content_filter",
        "violates our usage policy",
        "service is too busy",
        "providerresponseerror",
        "the model is overloaded",
        "你的请求被拒绝，因为",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

/// 摘掉正文里的伪工具调用，返回可以照常断句、翻译标记的文字。
pub(crate) fn strip(raw: &str) -> Cow<'_, str> {
    let Some((mut at, mut marker)) = next_marker(raw) else {
        return Cow::Borrowed(raw);
    };
    let mut out = String::with_capacity(raw.len());
    let mut cursor = 0usize;
    loop {
        // 标记之前的原文原样留着——它可能是上半句正常的话。
        out.push_str(&raw[cursor..at]);
        let tail = &raw[at + marker.len()..];
        if marker == PARAM_MARKER {
            // 只有完整闭合的参数块才允许继续读后文。残缺的 XML / JSON
            // 一律丢弃余下整段，不能把 `"text"` 那半行漏给断句器。
            let Some(close) = tail.find("</parameter>") else {
                cursor = raw.len();
                break;
            };
            let block = &tail[..close];
            if let Some((_, body)) = block.split_once('>')
                && let Ok(value) = serde_json::from_str::<serde_json::Value>(body.trim())
                && let Some(text) = request_text(&value)
            {
                out.push_str(&text);
            }
            cursor = at + marker.len() + close + "</parameter>".len();
            match next_marker(&raw[cursor..]) {
                Some((next, found)) => {
                    at = cursor + next;
                    marker = found;
                    continue;
                }
                None => break,
            }
        }
        let end = if marker == ACTION_MARKER {
            json_end(tail, b'{')
        } else {
            json_end(tail, b'[')
        };
        if let Some(end) = end {
            let value = serde_json::from_str::<serde_json::Value>(&tail[..end]).ok();
            let text = if marker == ACTION_MARKER {
                send_text(value.as_ref())
            } else {
                value.as_ref().and_then(|value| parts_text(value.as_array()?))
            };
            if let Some(text) = text {
                out.push_str(&text);
            }
            cursor = at + marker.len() + end;
            if marker == ACTION_MARKER {
                // JSON 与收尾的 `]` 之间允许夹空白（含换行）。
                let rest = &raw[cursor..];
                cursor += rest.len() - rest.trim_start().len();
                if raw[cursor..].starts_with(']') {
                    cursor += 1;
                }
            }
        } else {
            cursor = raw.len();
            break;
        }
        match next_marker(&raw[cursor..]) {
            Some((next, found)) => {
                at = cursor + next;
                marker = found;
            }
            None => break,
        }
    }
    out.push_str(&raw[cursor..]);
    Cow::Owned(out)
}

/// 从 `{` 或 `[` 开始找一段配平的 JSON，返回闭括号之后的下标（字节）。
///
/// 字符串里的括号与 `\` 转义都不算数，否则正文中的括号会提前收尾。
/// 找不到开括号、或一直不配平，都给 `None`。
fn json_end(text: &str, open: u8) -> Option<usize> {
    let start = text.bytes().position(|byte| byte == open)?;
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in text.bytes().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => stack.push(byte),
            b'}' | b']' => {
                let expected = if byte == b'}' { b'{' } else { b'[' };
                if stack.pop()? != expected {
                    return None;
                }
                if stack.is_empty() {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// 从伪调用里捞出可以当普通消息发的正文。
///
/// 只认 `send`：它最常被写错，而且它的 `text` / `at` / `face` 段在旧写法里都有对应
/// 标记，接过来就是一句正常的话。`at` 与 `face` 翻回 `[at:…]` / `[face:…]`，
/// 交给 [`super::pace`] 同一套翻译；图片、文件、转发那类段留在原地不动（旧写法
/// 没有对应形状），`send` 的价值主要在文字。别的动作返回 `None`，整段丢掉。
fn send_text(value: Option<&serde_json::Value>) -> Option<String> {
    request_text(value?.get("request")?)
}

fn request_text(request: &serde_json::Value) -> Option<String> {
    if request.get("action")?.as_str()? != "send" {
        return None;
    }
    parts_text(request.get("parts")?.as_array()?)
}

fn parts_text(parts: &[serde_json::Value]) -> Option<String> {
    let mut out = String::new();
    // 只有相邻的两个 text 段之间才补换行——它们代表模型自己分的两条消息。
    let mut last_was_text = false;
    for part in parts {
        match part.get("type").and_then(|value| value.as_str()) {
            Some("text") => {
                let text = part
                    .get("text")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                if last_was_text && !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(text);
                last_was_text = true;
            }
            Some("at") => {
                if let Some(id) = part.get("user_id").and_then(id_text) {
                    out.push_str(&format!("[at:{id}]"));
                }
                last_was_text = false;
            }
            Some("face") => {
                if let Some(id) = part.get("id").and_then(id_text) {
                    out.push_str(&format!("[face:{id}]"));
                }
                last_was_text = false;
            }
            _ => last_was_text = false,
        }
    }
    (!out.trim().is_empty()).then_some(out)
}

/// `user_id` / `id` 那种字段：模型有时写字符串，有时写数字。
fn id_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}
