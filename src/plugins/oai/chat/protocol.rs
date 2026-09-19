//! 出站文字里的协议清洗。
//!
//! 模型偶尔不调用工具，而是**把工具调用当正文写出来**：
//!
//! ```text
//! [satori_action:{"request":{"action":"send","parts":[{"type":"text","text":"…"}]}}]
//! ```
//!
//! 那是协议，不是要说的话；原样发进群，群友看到的是一串 JSON（线上记录 id 134245，
//! 群里几个人还照着抄了一遍）。所以出站之前必须把它摘掉：`send` 的正文接过来当普通
//! 消息发，其余动作（戳一戳、撤回、管理）与读不到结尾的残片整段丢弃——从文字里执行
//! 动作会绕开额度与权限那两道闸，宁可不做。
//!
//! **清洗要排在断句之前。** JSON 里的逗号在 [`super::breath`] 眼里是换气处，先断句
//! 会把标记切成两半，后半截没有 `[satori_action:` 这个前缀，照样当正文漏进群。

use std::borrow::Cow;

/// 伪工具调用的开头。真工具调用走的是结构化 `tool_calls`，不会带这个标记。
const MARKER: &str = "[satori_action:";

/// 摘掉正文里的伪工具调用，返回可以照常断句、翻译标记的文字。
pub(crate) fn strip(raw: &str) -> Cow<'_, str> {
    let Some(mut at) = raw.find(MARKER) else {
        return Cow::Borrowed(raw);
    };
    let mut out = String::with_capacity(raw.len());
    let mut cursor = 0usize;
    loop {
        // 标记之前的原文原样留着——它可能是上半句正常的话。
        out.push_str(&raw[cursor..at]);
        let tail = &raw[at + MARKER.len()..];
        match object_end(tail) {
            // 配平的 JSON：认出 `send` 就把正文接过来，其余动作丢掉。
            Some(end) => {
                let value = serde_json::from_str::<serde_json::Value>(&tail[..end]).ok();
                if let Some(text) = send_text(value.as_ref()) {
                    out.push_str(&text);
                }
                cursor = at + MARKER.len() + end;
                // JSON 与收尾的 `]` 之间允许夹空白（含换行）。
                let rest = &raw[cursor..];
                cursor += rest.len() - rest.trim_start().len();
                if raw[cursor..].starts_with(']') {
                    cursor += 1;
                }
            }
            // 读不到结尾的残片（模型写到一半被截断）：从标记起整段丢掉，
            // 免得把半截 JSON 当正文发出去。
            None => {
                cursor = raw.len();
                break;
            }
        }
        match raw[cursor..].find(MARKER) {
            Some(next) => at = cursor + next,
            None => break,
        }
    }
    out.push_str(&raw[cursor..]);
    Cow::Owned(out)
}

/// 从 `{` 开始找一个配平的 JSON 对象，返回闭括号之后的下标（字节）。
///
/// 字符串里的花括号与 `\` 转义都不算数，否则 `{"text":" } "}` 会提前收尾。
/// 找不到 `{`、或一直不配平，都给 `None`。
fn object_end(text: &str) -> Option<usize> {
    let start = text.find('{')?;
    let mut depth = 0usize;
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
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
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
    let request = value?.get("request")?;
    if request.get("action")?.as_str()? != "send" {
        return None;
    }
    let parts = request.get("parts")?.as_array()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 线上那一串：模型把 `send` 的调用当正文写出来，正文在 JSON 里面。
    #[test]
    fn a_send_call_written_as_text_becomes_its_message() {
        let raw = r#"[satori_action:{"request":{"action":"send","parts":[{"type":"text","text":"昇腾归属这事我还真查过"}]}}]"#;
        assert_eq!(strip(raw), "昇腾归属这事我还真查过");
    }

    /// 线上真正漏出去的那一条：模型写到一半断了，只有一个前缀，没有花括号。
    #[test]
    fn a_truncated_call_never_reaches_the_group() {
        let raw = r#"[satori_action:{"request":{"action":"send","parts":[{"type":"text""#;
        assert_eq!(strip(raw), "");
        // 标记后面什么都跟不上也一样丢掉。
        assert_eq!(strip("[satori_action:"), "");
        assert_eq!(strip("前半句 [satori_action:{\"request\""), "前半句 ");
    }

    /// 前后正常的话留着，只有标记那一段被摘掉。
    #[test]
    fn text_around_the_call_survives() {
        let raw = "先回你上一句 [satori_action:{\"request\":{\"action\":\"send\",\"parts\":[{\"type\":\"text\",\"text\":\"这版驱动确实有问题\"}]}}] 然后再说别的";
        assert_eq!(strip(raw), "先回你上一句 这版驱动确实有问题 然后再说别的");
    }

    /// 一个 `send` 分成两段文字，就是两条消息：留一个换行，断句那层会分开。
    #[test]
    fn two_text_parts_stay_two_messages() {
        let raw = r#"[satori_action:{"request":{"action":"send","parts":[{"type":"text","text":"第一步先装依赖"},{"type":"text","text":"第二步再重跑"}]}}]"#;
        assert_eq!(strip(raw), "第一步先装依赖\n第二步再重跑");
    }

    /// `at` 与 `face` 翻回旧写法，交给同一套标记翻译。
    #[test]
    fn at_and_face_parts_come_back_as_markup() {
        let raw = r#"[satori_action:{"request":{"action":"send","parts":[{"type":"at","user_id":"114514"},{"type":"face","id":"76"},{"type":"text","text":" 说得对"}]}}]"#;
        assert_eq!(strip(raw), "[at:114514][face:76] 说得对");
        // QQ 号写成数字也认。
        let numeric = r#"[satori_action:{"request":{"action":"send","parts":[{"type":"at","user_id":114514}]}}]"#;
        assert_eq!(strip(numeric), "[at:114514]");
    }

    /// 别的动作从文字里执行会绕开额度与权限，整段丢掉；前后的话照旧。
    #[test]
    fn other_actions_are_dropped_whole() {
        let raw = r#"[satori_action:{"request":{"action":"recall","message_id":"123"}}]"#;
        assert_eq!(strip(raw), "");
        let raw = r#"看着 [satori_action:{"request":{"action":"poke","user_id":"114514"}}]"#;
        assert_eq!(strip(raw), "看着 ");
        // 没有 request、或 JSON 不是对象，也丢掉。
        assert_eq!(strip("[satori_action:{}]"), "");
        assert_eq!(strip("[satori_action:不是 JSON]"), "");
    }

    /// 字符串里的花括号与 `]` 不参与配平，也不许提前收尾。
    #[test]
    fn braces_inside_a_string_do_not_close_the_call() {
        let raw = r#"[satori_action:{"request":{"action":"send","parts":[{"type":"text","text":"这个 { 和 } 还有 ] 都是正文"}]}}]"#;
        assert_eq!(strip(raw), "这个 { 和 } 还有 ] 都是正文");
    }

    /// 一条消息里写了两次调用，两次都摘掉。
    #[test]
    fn several_calls_in_one_line_are_all_stripped() {
        let raw = concat!(
            r#"[satori_action:{"request":{"action":"send","parts":[{"type":"text","text":"第一句"}]}}]"#,
            " 中间 ",
            r#"[satori_action:{"request":{"action":"send","parts":[{"type":"text","text":"第二句"}]}}]"#,
        );
        assert_eq!(strip(raw), "第一句 中间 第二句");
    }

    /// 没有标记的文字一个字节都不动，也不多分配一次。
    #[test]
    fn ordinary_text_is_left_exactly_alone() {
        let raw = "行 图我先收了 [笑] 下次轮到你";
        assert!(matches!(strip(raw), Cow::Borrowed(_)));
        assert_eq!(strip(raw), raw);
    }
}
