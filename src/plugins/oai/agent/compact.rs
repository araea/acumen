//! 长任务里的上下文瘦身。
//!
//! 一轮最多几十次请求，每次请求都带着此前的全部消息；bash 与 read 的回执动辄几万字，
//! `view_image` 附的图更是整张 data URL。十来步下来就是几十万字符——慢、贵，还会撞上
//! 模型的上下文上限，整轮报错、已经做完的事一起作废。
//!
//! 做法和人整理笔记一样：较早的回执只留头尾，最近几条原样保留；模型已经据此下过判断，
//! 原文基本不会再回看，真要再看就再调一次工具。**只动已经过期的内容**，且幂等——压过的
//! 回执很短，下一次不会再被碰。

use rig_core::completion::Message;
use rig_core::completion::message::{Text, ToolResultContent, UserContent};

/// 超过这个体量才开始压（字符；图按 [`IMAGE_COST`] 折算）。
///
/// 中文约一字一个 token、英文约四字符一个，按混合内容取一个保守值：16 万字符大致
/// 是 6 万 token 上下，远低于常见模型的窗口，又足够装下一轮正常的工具往来。
pub(crate) const BUDGET: usize = 160_000;

/// 最近这几份工具回执不动：模型正在用它们。
const KEEP_RECENT: usize = 4;

/// 压缩后保留的开头与结尾（字符）。开头多留一点：报错与结构信息多在那里，
/// 结尾留的是命令的最终结果。
const STUB_HEAD: usize = 700;
const STUB_TAIL: usize = 300;

/// 短于这个长度的回执压不出多少，也不动。
const MIN_SHRINK: usize = 1_500;

/// 一张图折算成多少字符：按视觉 token 的量级估，而不是 base64 的字节数。
const IMAGE_COST: usize = 3_000;

/// 最多保留几张 `view_image` 附的图；更早的换成一句占位。
///
/// 图每次请求都要整张重发，一张 1600 像素的图就是几百 KB。模型看过之后下的结论已经
/// 在它自己的话里，重发旧图只是在手机的上行带宽里排队。
const KEEP_VIEWS: usize = 3;

/// `view_image` 的图附在这样开头的用户消息里（见 `run.rs`）；识别旧图靠这句。
pub(crate) const VIEW_MARK: &str = "（上面 view_image 读到的图）";

/// 早先的图被拿掉之后留下的占位。
const VIEW_GONE: &str = "（更早之前看过的一张图，已省略）";

/// 估算这些消息有多大（字符）。
pub(crate) fn size(messages: &[Message]) -> usize {
    messages.iter().map(message_size).sum()
}

fn message_size(message: &Message) -> usize {
    use rig_core::completion::AssistantContent;
    match message {
        Message::System { content } => content.chars().count(),
        Message::User { content } => content
            .iter()
            .map(|part| match part {
                UserContent::Text(text) => text.text.chars().count(),
                UserContent::Image(_) => IMAGE_COST,
                UserContent::ToolResult(result) => result
                    .content
                    .iter()
                    .map(|part| match part {
                        ToolResultContent::Text(text) => text.text.chars().count(),
                        ToolResultContent::Image(_) => IMAGE_COST,
                        ToolResultContent::Json { value } => value.to_string().len(),
                    })
                    .sum(),
                _ => 0,
            })
            .sum(),
        Message::Assistant { content, .. } => content
            .iter()
            .map(|part| match part {
                AssistantContent::Text(text) => text.text.chars().count(),
                AssistantContent::ToolCall(call) => call.function.arguments.to_string().len(),
                _ => 0,
            })
            .sum(),
    }
}

/// 超出预算时压缩较早的内容，返回省下的字符数；没超就原样不动。
///
/// 先拿掉多余的旧图（体积大、最不值钱），再从最老的工具回执开始压，压到回预算内为止。
pub(crate) fn compact(messages: &mut [Message], budget: usize) -> usize {
    let before = size(messages);
    let views = drop_old_views(messages);
    let mut now = before;
    if views > 0 {
        now = size(messages);
    }
    if now > budget {
        let slots = tool_results(messages);
        let protected = slots.len().saturating_sub(KEEP_RECENT);
        for &(message, part, item) in &slots[..protected] {
            if now <= budget {
                break;
            }
            if let Some(text) = text_at(messages, message, part, item)
                && let Some(stub) = stub(text)
            {
                set_text(messages, message, part, item, stub);
                now = size(messages);
            }
        }
    }
    before.saturating_sub(now)
}

/// 所有工具回执里的文本块位置：`(消息, 内容块, 回执内第几项)`，按时间从旧到新。
fn tool_results(messages: &[Message]) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    for (m, message) in messages.iter().enumerate() {
        let Message::User { content } = message else {
            continue;
        };
        for (p, part) in content.iter().enumerate() {
            if let UserContent::ToolResult(result) = part {
                for (i, item) in result.content.iter().enumerate() {
                    if matches!(item, ToolResultContent::Text(_)) {
                        out.push((m, p, i));
                    }
                }
            }
        }
    }
    out
}

fn text_at(messages: &[Message], m: usize, p: usize, i: usize) -> Option<&str> {
    let Message::User { content } = messages.get(m)? else {
        return None;
    };
    let UserContent::ToolResult(result) = content.get(p)? else {
        return None;
    };
    result.content.get(i)?.as_text()
}

fn set_text(messages: &mut [Message], m: usize, p: usize, i: usize, text: String) {
    if let Message::User { content } = &mut messages[m]
        && let UserContent::ToolResult(result) = &mut content[p]
    {
        result.content[i] = ToolResultContent::Text(Text::new(text));
    }
}

/// 一份回执压成头 + 尾；本来就短（或已经压过）的返回 `None`。
fn stub(text: &str) -> Option<String> {
    let total = text.chars().count();
    if total <= MIN_SHRINK.max(STUB_HEAD + STUB_TAIL + 200) {
        return None;
    }
    let head: String = text.chars().take(STUB_HEAD).collect();
    let tail: String = text.chars().skip(total - STUB_TAIL).collect();
    Some(format!(
        "{head}\n…（这份回执较早，已压缩：原 {total} 字，中间省略 {} 字；需要原文请重新调用工具）…\n{tail}",
        total - STUB_HEAD - STUB_TAIL
    ))
}

/// 把更早的 `view_image` 图片换成占位，只留最近 [`KEEP_VIEWS`] 张；返回拿掉了几张。
fn drop_old_views(messages: &mut [Message]) -> usize {
    let is_view = |message: &Message| {
        matches!(message, Message::User { content }
            if matches!(content.first(), Some(UserContent::Text(text)) if text.text == VIEW_MARK))
    };
    let mut kept = 0;
    let mut dropped = 0;
    for message in messages.iter_mut().rev() {
        if !is_view(message) {
            continue;
        }
        let Message::User { content } = message else {
            continue;
        };
        // 同一条消息里的图：越靠后越新。
        for part in content.iter_mut().rev() {
            if matches!(part, UserContent::Image(_)) {
                if kept < KEEP_VIEWS {
                    kept += 1;
                } else {
                    *part = UserContent::Text(Text::new(VIEW_GONE));
                    dropped += 1;
                }
            }
        }
    }
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::message::{DocumentSourceKind, Image, ToolResult};
    use rig_core::completion::message::ToolCallId;

    fn tool(text: &str) -> Message {
        Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                call: ToolCallId::mint(),
                provider: None,
                name: "bash".into(),
                content: vec![ToolResultContent::text(text)],
            })],
        }
    }

    fn view(count: usize) -> Message {
        let mut content = vec![UserContent::Text(Text::new(VIEW_MARK))];
        for _ in 0..count {
            content.push(UserContent::Image(Image {
                data: DocumentSourceKind::Url("data:image/png;base64,AAAA".into()),
                media_type: None,
                detail: None,
                additional_params: None,
            }));
        }
        Message::User { content }
    }

    fn texts(messages: &[Message]) -> Vec<String> {
        tool_results(messages)
            .into_iter()
            .map(|(m, p, i)| text_at(messages, m, p, i).unwrap().to_string())
            .collect()
    }

    #[test]
    fn under_budget_nothing_changes() {
        let mut messages = vec![tool(&"字".repeat(5_000)), tool("短")];
        let before = texts(&messages);
        assert_eq!(compact(&mut messages, BUDGET), 0);
        assert_eq!(texts(&messages), before);
    }

    #[test]
    fn oldest_results_shrink_first_and_recent_ones_stay() {
        let big = "字".repeat(20_000);
        let mut messages: Vec<Message> = (0..8).map(|_| tool(&big)).collect();
        let saved = compact(&mut messages, 125_000);
        assert!(saved > 0);
        let now = texts(&messages);
        // 最近 KEEP_RECENT 份原样。
        for text in &now[4..] {
            assert_eq!(text.chars().count(), 20_000);
        }
        // 压的是最老的那几份，保留头尾并写明省略了多少。
        assert!(now[0].contains("已压缩：原 20000 字"));
        assert!(now[0].starts_with(&"字".repeat(10)));
        assert!(now[0].chars().count() < 1_300);
        // 压到预算内就停：不会把能留的也压掉。
        assert!(size(&messages) <= 125_000);
        assert_eq!(now.iter().filter(|t| t.contains("已压缩")).count(), 2);
    }

    #[test]
    fn compaction_is_idempotent() {
        let big = "x".repeat(30_000);
        let mut messages: Vec<Message> = (0..8).map(|_| tool(&big)).collect();
        compact(&mut messages, 50_000);
        let once = texts(&messages);
        assert_eq!(compact(&mut messages, 50_000), 0);
        assert_eq!(texts(&messages), once);
    }

    #[test]
    fn protected_recent_results_survive_even_over_budget() {
        let big = "字".repeat(100_000);
        let mut messages: Vec<Message> = (0..3).map(|_| tool(&big)).collect();
        compact(&mut messages, 10_000);
        // 一共才三份，都在保护范围里。
        assert!(texts(&messages).iter().all(|t| t.chars().count() == 100_000));
    }

    #[test]
    fn old_view_images_are_replaced_but_latest_stay() {
        let mut messages = vec![view(2), tool("a"), view(2), tool("b"), view(1)];
        let dropped = drop_old_views(&mut messages);
        assert_eq!(dropped, 2);
        let gone = |message: &Message| match message {
            Message::User { content } => content
                .iter()
                .filter(|p| matches!(p, UserContent::Text(t) if t.text == VIEW_GONE))
                .count(),
            _ => 0,
        };
        assert_eq!(gone(&messages[0]), 2);
        assert_eq!(gone(&messages[2]), 0);
        assert_eq!(gone(&messages[4]), 0);
    }

    #[test]
    fn short_text_is_never_stubbed() {
        assert!(stub(&"a".repeat(1_400)).is_none());
        assert!(stub(&"a".repeat(1_600)).is_some());
    }

    #[test]
    fn multibyte_boundaries_are_respected() {
        let text: String = "你好🙂".repeat(2_000);
        let stub = stub(&text).unwrap();
        assert!(stub.contains("已压缩"));
    }
}
