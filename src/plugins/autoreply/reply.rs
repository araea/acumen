//! 回复怎么发出去，以及群里的一条消息怎么存成回复。
//!
//! 回复是一段带占位符的文字（写法见 [`crate::template`]）：`{at}` @ 提问的人、`{name}` 他的
//! 名字、`{keyword}` 对上的触发词、`{image:文件名或链接}` 一张图、`{face:14}` QQ 表情、
//! `{甲|乙}` 随机一个，还有 `{group}` `{greeting}` `{time}` 这些。本机的图放在
//! `data/autoreply/`，发的时候直接带上字节（QQ 读不到 Termux 的私有目录）。
//!
//! 存的方向反过来：群里引用的消息、指令后面跟着的内容里，文字原样留着，图片下载下来收进
//! 本机图库（QQ 的图片链接过几天就失效），表情记成 `{face:ID}`。合并转发整条展开，每条
//! 存成一个节点，发的时候照原样拼回一条合并转发。

use super::LOG_TARGET;
use super::book::{Node, Reply, Rule};
use crate::adapters::satori::forward;
use crate::adapters::satori::{LockedWriter, api, send_msg};
use crate::event::Context;
use crate::message::{Message, Segment};
use crate::plugins::PluginError;
use crate::template::Piece;
use base64::Engine;
use simd_json::OwnedValue;
use simd_json::base::ValueAsScalar;
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsScalar};
use std::collections::HashMap;
use std::time::Duration;

/// 谁问的、在哪问的、对上了哪个词：填占位符用。
pub struct Asker<'a> {
    pub user_id: &'a str,
    pub name: &'a str,
    pub group: Option<&'a str>,
    pub keyword: &'a str,
}

/// 回复里能用的占位符（控制台的插入芯片与预览照这张表）。
pub const PLACEHOLDERS: &[(&str, &str, &str)] = &[
    ("at", "@ 提问的人", "真的 @ 他"),
    ("name", "他的名字", "群名片，没有就用昵称"),
    ("keyword", "触发词", "这次对上的是哪个触发词"),
    ("group", "群名", ""),
    ("greeting", "问候", "按北京时间：早上好、下午好、晚上好……"),
    ("time", "时刻", "如 21:05"),
    ("date", "日期", "如 10 月 22 日"),
    ("weekday", "星期", "如 星期三"),
    ("user_id", "他的账号", "QQ 号 / 微信号"),
    ("group_id", "群号", ""),
];

/// 一段回复 → 一条消息。整条只剩空白就是 `None`。
pub async fn compose(ctx: &Context, writer: &LockedWriter, text: &str, asker: &Asker<'_>) -> Option<Message> {
    let moment = crate::clock::moment();
    let mut values: HashMap<&str, String> = HashMap::from([
        ("name", asker.name.to_string()),
        ("user_id", asker.user_id.to_string()),
        ("keyword", asker.keyword.to_string()),
        ("group_id", asker.group.unwrap_or_default().to_string()),
        ("greeting", moment.greeting.to_string()),
        ("time", moment.time),
        ("date", moment.date),
        ("weekday", moment.weekday),
    ]);
    // 群名查群目录，通常不用问平台；用到了才查。
    if text.contains("{group}") {
        let name = match asker.group {
            Some(group) => api::group_name(ctx, writer, group).await,
            None => String::new(),
        };
        values.insert("group", name);
    }
    let mut message = Message::new();
    let mut visible = false;
    for piece in crate::template::pieces(text, &values) {
        match piece {
            Piece::Text(text) => {
                visible |= !text.trim().is_empty();
                message = message.text(text);
            }
            Piece::At if !asker.user_id.is_empty() => {
                message = message.at(asker.user_id);
                visible = true;
            }
            Piece::Image(Some(src)) => match picture(&src).await {
                Some(file) => {
                    message = message.image(file);
                    visible = true;
                }
                None => warn!(target: LOG_TARGET, "回复里的图 {src} 读不出来，跳过"),
            },
            Piece::Face(id) => {
                message = message.face(id);
                visible = true;
            }
            // `{image}` 没说哪张、`{avatar}` 在这里没有意义：都不出东西。
            Piece::At | Piece::Image(None) | Piece::Avatar => {}
        }
    }
    visible.then_some(message)
}

/// 图的来源：直链原样用，其余当成本机图库里的文件名，读成字节带上。
async fn picture(src: &str) -> Option<String> {
    if src.starts_with("http://") || src.starts_with("https://") {
        return Some(src.to_string());
    }
    if !crate::storage::picture::plain(src) {
        return None;
    }
    let path = crate::storage::data_path("autoreply").ok()?.join(src);
    let bytes = tokio::fs::read(path).await.ok()?;
    Some(format!("base64://{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
}

/// 折成合并转发时怎么分条：单独一行的 `---`（或 `——`）是分隔。
fn sections(text: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    for line in text.lines() {
        if matches!(line.trim(), "---" | "——" | "———" | "----") {
            out.push(String::new());
        } else {
            let last = out.last_mut().expect("至少一段");
            if !last.is_empty() {
                last.push('\n');
            }
            last.push_str(line);
        }
    }
    out.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// 把一种回复发出去。`quote`：引用提问的那条消息（合并转发没法引用，就不引）。返回发没发出东西。
pub async fn send(
    ctx: &Context,
    writer: &LockedWriter,
    rule: &Rule,
    reply: &Reply,
    asker: &Asker<'_>,
    quote: Option<&str>,
) -> Result<bool, PluginError> {
    let login = ctx.bot.login_user.get();
    let bot_name = login.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| "自动回复".into());
    let message = match reply {
        Reply::Text { text } if rule.forward => {
            let mut message = Message::new();
            let mut any = false;
            for part in sections(text) {
                if let Some(content) = compose(ctx, writer, &part, asker).await {
                    message = message.node_custom(&login.id, &bot_name, content);
                    any = true;
                }
            }
            any.then_some(message)
        }
        Reply::Text { text } => compose(ctx, writer, text, asker).await.map(|content| match quote {
            Some(id) if !id.is_empty() => {
                let mut quoted = Message::new().reply(id);
                quoted.0.extend(content.0);
                quoted
            }
            _ => content,
        }),
        Reply::Forward { forward } => {
            let mut message = Message::new();
            let mut any = false;
            for node in forward {
                if let Some(content) = compose(ctx, writer, &node.text, asker).await {
                    let (id, name) = if node.user_id.is_empty() {
                        (login.id.clone(), bot_name.clone())
                    } else {
                        (node.user_id.clone(), if node.name.is_empty() { node.user_id.clone() } else { node.name.clone() })
                    };
                    message = message.node_custom(id, name, content);
                    any = true;
                }
            }
            any.then_some(message)
        }
    };
    let Some(message) = message else {
        return Ok(false);
    };
    let (group, user) = match asker.group {
        Some(group) => (Some(group), None),
        None => (None, Some(asker.user_id)),
    };
    send_msg(ctx, writer.clone(), group, user, message).await?;
    Ok(true)
}

// ==================== 存 ====================

/// 消息里的一小块，不管它来自指令参数（事件里的原始段）还是取回的消息（[`Segment`]）。
#[derive(Debug, Clone, PartialEq)]
pub enum Bit {
    Text(String),
    Image(String),
    Face(String),
    /// 存不了的东西（视频、语音、文件……），带着它的叫法。
    Other(&'static str),
    /// 合并转发（携带它的消息 ID 另算）：resId。
    Forward(String),
}

fn other(kind: &str) -> Option<Bit> {
    let label = match kind {
        "video" => "视频",
        "record" | "audio" => "语音",
        "file" => "文件",
        "json" | "xml" | "lightapp" | "ark" => "卡片",
        "music" => "音乐",
        _ => return None,
    };
    Some(Bit::Other(label))
}

/// 事件里的原始段（指令参数）。@、引用这类不进回复。
pub fn bits_of_args(args: &[OwnedValue]) -> Vec<Bit> {
    args.iter()
        .filter_map(|seg| {
            let kind = seg.get_str("type")?;
            let data = seg.get("data");
            let field = |key: &str| data.and_then(|d| d.get_str(key)).filter(|v| !v.is_empty()).map(str::to_owned);
            match kind {
                "text" => field("text").map(Bit::Text),
                "image" | "mface" => field("url").or_else(|| field("file")).map(Bit::Image),
                "face" => field("id").or_else(|| data.and_then(|d| d.get_i64("id")).map(|n| n.to_string())).map(Bit::Face),
                "forward" => field("id").map(Bit::Forward),
                _ => other(kind),
            }
        })
        .collect()
}

/// 取回的一条消息。
pub fn bits_of_message(message: &Message) -> Vec<Bit> {
    message.0.iter().filter_map(bit_of_segment).collect()
}

fn bit_of_segment(segment: &Segment) -> Option<Bit> {
    let field = |key: &str| segment.data.get(key).and_then(|v| v.as_str()).filter(|v| !v.is_empty()).map(str::to_owned);
    match segment.type_.as_str() {
        "text" => field("text").map(Bit::Text),
        "image" | "mface" => field("url").or_else(|| field("file")).map(Bit::Image),
        "face" => field("id").or_else(|| segment.data.get("id").and_then(ValueAsScalar::as_i64).map(|n| n.to_string())).map(Bit::Face),
        "forward" => field("id").map(Bit::Forward),
        kind => other(kind),
    }
}

/// 存下来的结果：回复正文，以及没存成的东西（给人说一声）。
#[derive(Default)]
pub struct Captured {
    pub text: String,
    /// 存不了的种类（「视频」「语音」）。
    pub skipped: Vec<&'static str>,
    /// 下载不下来的图有几张。
    pub lost_images: usize,
}

/// 一串小块 → 回复正文：图片下载进本机图库。
pub async fn capture(bits: &[Bit]) -> Captured {
    let mut out = Captured::default();
    for bit in bits {
        match bit {
            Bit::Text(text) => out.text.push_str(text),
            Bit::Face(id) => out.text.push_str(&format!("{{face:{id}}}")),
            Bit::Image(url) => match keep_image(url).await {
                Some(name) => out.text.push_str(&format!("{{image:{name}}}")),
                None => out.lost_images += 1,
            },
            Bit::Forward(_) => {
                if !out.skipped.contains(&"嵌套的聊天记录") {
                    out.skipped.push("嵌套的聊天记录");
                }
            }
            Bit::Other(label) => {
                if !out.skipped.contains(label) {
                    out.skipped.push(label);
                }
            }
        }
    }
    out.text = out.text.trim().to_string();
    out
}

/// 群里引用的一条合并转发整条展开，每条存成一个节点。展开不了是 `None`。
pub async fn capture_forward(ctx: &Context, writer: &LockedWriter, res_id: &str, message_id: &str, group: &str) -> Option<(Vec<Node>, Captured)> {
    let source = forward::Source::new(Some(res_id.to_string()), Some(message_id.to_string())).in_channel(group);
    let view = forward::expand(ctx, writer, source).await;
    let mut nodes = Vec::new();
    let mut total = Captured::default();
    for node in view.nodes.iter().filter(|node| node.depth == 0).take(super::book::MAX_NODES) {
        let part = capture(&bits_of_message(&node.message)).await;
        total.lost_images += part.lost_images;
        for label in part.skipped {
            if !total.skipped.contains(&label) {
                total.skipped.push(label);
            }
        }
        if !part.text.is_empty() {
            nodes.push(Node { name: node.name.clone(), user_id: node.user_id.clone(), text: part.text });
        }
    }
    (!nodes.is_empty()).then_some((nodes, total))
}

/// 下载一张图收进本机图库，返回文件名。
async fn keep_image(url: &str) -> Option<String> {
    const LIMIT: usize = 16 * 1024 * 1024;
    let download = async {
        let response = crate::http::get(url).await?.error_for_status()?;
        response.bytes().await
    };
    let bytes = match tokio::time::timeout(Duration::from_secs(30), download).await {
        Ok(Ok(bytes)) if !bytes.is_empty() && bytes.len() <= LIMIT => bytes,
        Ok(Ok(_)) => return None,
        Ok(Err(error)) => {
            warn!(target: LOG_TARGET, "图没下载下来：{error}");
            return None;
        }
        Err(_) => {
            warn!(target: LOG_TARGET, "图下载了 30 秒还没完，放弃");
            return None;
        }
    };
    match crate::storage::picture::keep("autoreply", Some("reply"), bytes.to_vec()).await {
        Ok(kept) => Some(kept.name),
        Err(error) => {
            warn!(target: LOG_TARGET, "图没存下来：{error}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_sections_split_on_rule_lines() {
        assert_eq!(sections("第一条\n第二行\n---\n\n第二条\n——\n"), vec!["第一条\n第二行".to_string(), "第二条".into()]);
        assert_eq!(sections("只有一条"), vec!["只有一条".to_string()]);
    }
}
