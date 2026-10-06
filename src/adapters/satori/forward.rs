//! 把一条合并转发读成完整的聊天记录。
//!
//! QQ 有两条取回路径，质量差得很远：
//!
//! - `internal/get_forward` 传 resId 时走 `SsoRecvLongMsg` 的伪造节点协议。NT 客户端
//!   发出的图片、表情包在那条路上整段消失，节点只剩空正文，也没有逐条消息 ID。
//! - 传 `native:<父消息 ID>` 时走 QQ 内核缓存，图片、逐条 ID 和时间戳都在，代价是
//!   父消息一旦被缓存淘汰就查不到。
//!
//! 所以这里先要内核，失败再退回 resId，并把「退回过」写进 notes——否则模型会把
//! 协议丢失的正文当成群友原本就没说话。嵌套转发按同样规则逐层展开，受节点和层数
//! 双重预算约束，避免一条恶意转发把上下文撑爆。

use super::{LockedWriter, message};
use crate::event::Context;
use crate::message::{Message, Segment};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use simd_json::base::ValueAsScalar;

/// 一次展开最多读回多少条节点（含所有层）。
pub const MAX_NODES: usize = 60;
/// 嵌套转发最多再往里读几层。
pub const MAX_DEPTH: usize = 3;
/// 单条节点正文在转写里的最大字符数。
const MAX_NODE_CHARS: usize = 400;

/// 从哪里读这条合并转发。两个来源都给上时先试内核。
#[derive(Debug, Clone, Default)]
pub struct Source {
    /// `<message forward id="...">` 里的 resId。
    pub resource_id: Option<String>,
    /// 携带这条合并转发的那条消息的 ID，用于走内核缓存。
    pub message_id: Option<String>,
    /// 这条消息所在的会话。satori-qq 的父消息缓存被淘汰后靠它重新定位内核记录。
    pub channel: Option<String>,
}

impl Source {
    pub fn new(resource_id: Option<String>, message_id: Option<String>) -> Self {
        Self {
            resource_id: resource_id.filter(|value| !value.is_empty()),
            message_id: message_id.filter(|value| !value.is_empty()),
            channel: None,
        }
    }

    /// 会话跟着整条展开链走：嵌套转发和父消息在同一个群里。
    pub fn in_channel(mut self, channel: impl Into<String>) -> Self {
        let channel = channel.into();
        self.channel = (!channel.is_empty()).then_some(channel);
        self
    }

    fn is_empty(&self) -> bool {
        self.resource_id.is_none() && self.message_id.is_none()
    }
}

/// 转发里的一条消息。
#[derive(Debug, Clone)]
pub struct Node {
    /// 0 是最外层，往里每嵌套一层加一。
    pub depth: usize,
    /// 内核路径才有；伪造节点协议返回空 ID。
    pub message_id: Option<String>,
    pub user_id: String,
    pub name: String,
    /// Unix 秒；0 表示这条路径没给时间。
    pub time: i64,
    pub message: Message,
}

#[derive(Debug, Clone, Default)]
pub struct View {
    pub nodes: Vec<Node>,
    /// 触到节点或层数上限，后面还有没读的内容。
    pub truncated: bool,
    /// 读取过程中的降级与失败，必须让模型看见，别把缺失当原文。
    pub notes: Vec<String>,
}

impl View {
    /// 同一种降级在嵌套里会反复发生，说明一次就够。
    fn note(&mut self, text: String) {
        if !self.notes.contains(&text) {
            self.notes.push(text);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// 转发里出现过的图片直链，供多模态模型真正看到内容。
    pub fn images(&self) -> Vec<String> {
        let mut out = Vec::new();
        for node in &self.nodes {
            for segment in &node.message.0 {
                if !matches!(segment.type_.as_str(), "image" | "mface") {
                    continue;
                }
                if let Some(url) = segment
                    .data
                    .get("url")
                    .or_else(|| segment.data.get("file"))
                    .and_then(|value| value.as_str())
                    .filter(|url| url.starts_with("http"))
                    && !out.iter().any(|seen| seen == url)
                {
                    out.push(url.to_string());
                }
            }
        }
        out
    }

    /// 展开成给模型读的纯文本：每条一行，嵌套层缩进。
    pub fn transcript(&self) -> String {
        let mut out = String::new();
        for (index, node) in self.nodes.iter().enumerate() {
            let indent = "  ".repeat(node.depth);
            let clock = if node.time > 0 {
                chrono::DateTime::from_timestamp(node.time, 0)
                    .map(|time| {
                        time.with_timezone(&chrono::Local)
                            .format("%m-%d %H:%M")
                            .to_string()
                    })
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let who = if node.user_id.is_empty() {
                node.name.clone()
            } else {
                format!("{}({})", node.name, node.user_id)
            };
            out.push_str(&format!("{indent}{}. {who}", index + 1));
            if !clock.is_empty() {
                out.push_str(&format!(" {clock}"));
            }
            out.push('：');
            out.push_str(&describe(&node.message));
            out.push('\n');
        }
        if self.truncated {
            out.push_str("（已达展开上限，后面还有内容未读取）\n");
        }
        for note in &self.notes {
            out.push_str(&format!("（{note}）\n"));
        }
        out
    }
}

/// 读回一条合并转发的全部内容，失败也返回带 notes 的空视图。
pub async fn expand(ctx: &Context, writer: &LockedWriter, source: Source) -> View {
    let mut view = View::default();
    if source.is_empty() {
        return view;
    }
    walk(ctx, writer, source, 0, &mut view).await;
    view
}

/// 从一条消息里找出合并转发的入口；`message_id` 是这条消息自己的 ID。
pub fn source_of(message: &Message, message_id: Option<String>) -> Option<Source> {
    let forward = message
        .0
        .iter()
        .find(|segment| segment.type_ == "forward")?;
    Some(Source::new(
        forward
            .data
            .get("id")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        message_id,
    ))
}

fn walk<'a>(
    ctx: &'a Context,
    writer: &'a LockedWriter,
    source: Source,
    depth: usize,
    view: &'a mut View,
) -> BoxFuture<'a, ()> {
    Box::pin(async move {
        if view.nodes.len() >= MAX_NODES {
            view.truncated = true;
            return;
        }
        if depth > MAX_DEPTH {
            view.truncated = true;
            return;
        }
        let Some(raw) = resolve(ctx, writer, &source, view).await else {
            return;
        };
        for mut node in raw {
            if view.nodes.len() >= MAX_NODES {
                view.truncated = true;
                return;
            }
            node.depth = depth;
            let nested = nested_sources(&node, source.channel.as_deref());
            let inline = inline_nodes(&node, depth + 1);
            view.nodes.push(node);
            for child in inline {
                if view.nodes.len() >= MAX_NODES {
                    view.truncated = true;
                    return;
                }
                view.nodes.push(child);
            }
            for child in nested {
                walk(ctx, writer, child, depth + 1, view).await;
            }
        }
    })
}

/// 先内核后 resId；两条都失败时把原因留在 notes 里。
async fn resolve(
    ctx: &Context,
    writer: &LockedWriter,
    source: &Source,
    view: &mut View,
) -> Option<Vec<Node>> {
    let mut failures = Vec::new();
    if let Some(message_id) = &source.message_id {
        match fetch(
            ctx,
            writer,
            &format!("native:{message_id}"),
            source.channel.as_deref(),
        )
        .await
        {
            Ok(nodes) if !nodes.is_empty() => return Some(nodes),
            Ok(_) => failures.push("内核缓存返回空".to_string()),
            Err(error) => failures.push(format!("内核缓存不可用：{error}")),
        }
    }
    let Some(resource) = source.resource_id.clone() else {
        if !failures.is_empty() {
            view.note(format!("合并转发读取失败：{}", failures.join("；")));
        }
        return None;
    };
    match fetch(ctx, writer, &resource, source.channel.as_deref()).await {
        Ok(nodes) if !nodes.is_empty() => {
            if !failures.is_empty() {
                view.note(
                    "已退回旧协议读取：该路径不返回图片、表情包和逐条消息 ID，缺失的媒体不代表原文没有"
                        .to_string(),
                );
            }
            Some(nodes)
        }
        Ok(_) => {
            failures.push("转发内容为空".to_string());
            view.note(format!("合并转发读取失败：{}", failures.join("；")));
            None
        }
        Err(error) => {
            failures.push(format!("{error}"));
            view.note(format!("合并转发读取失败：{}", failures.join("；")));
            None
        }
    }
}

async fn fetch(
    ctx: &Context,
    writer: &LockedWriter,
    id: &str,
    channel: Option<&str>,
) -> Result<Vec<Node>, super::BotError> {
    let mut params = json!({"id": id});
    if let Some(channel) = channel {
        params["channel_id"] = json!(channel);
    }
    let value: Value = writer.call(ctx, "internal/get_forward", params).await?;
    Ok(parse(&value, &writer.resources()))
}

fn parse(value: &Value, resources: &message::ResourceProxy) -> Vec<Node> {
    let mut out = Vec::new();
    for item in value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let user = item.get("user").unwrap_or(&Value::Null);
        out.push(Node {
            depth: 0,
            message_id: item.get("id").and_then(text_id),
            user_id: user
                .get("id")
                .and_then(text_id)
                .filter(|id| id != "0")
                .unwrap_or_default(),
            name: user
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            time: item
                .get("created_at")
                .and_then(Value::as_i64)
                .map(|millis| millis / 1000)
                .unwrap_or_default(),
            message: message::from_content_with(
                item.get("content").and_then(Value::as_str).unwrap_or(""),
                resources,
            ),
        });
    }
    out
}

/// 节点正文里还嵌着的合并转发；父消息 ID 用节点自己的，好继续走内核路径。
fn nested_sources(node: &Node, channel: Option<&str>) -> Vec<Source> {
    node.message
        .0
        .iter()
        .filter(|segment| segment.type_ == "forward")
        .map(|segment| {
            Source::new(
                segment
                    .data
                    .get("id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                node.message_id.clone(),
            )
            .in_channel(channel.unwrap_or_default())
        })
        .filter(|source| !source.is_empty())
        .collect()
}

/// `<message forward>` 直接内联了子消息时，正文里就是 node 段，不必再发请求。
fn inline_nodes(node: &Node, depth: usize) -> Vec<Node> {
    node.message
        .0
        .iter()
        .filter(|segment| segment.type_ == "node")
        .map(|segment| Node {
            depth,
            message_id: segment
                .data
                .get("id")
                .and_then(|value| value.as_str())
                .and_then(|value| value.parse().ok()),
            user_id: segment
                .data
                .get("user_id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            name: segment
                .data
                .get("nickname")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            time: 0,
            message: segment
                .data
                .get("content")
                .cloned()
                .and_then(|content| simd_json::serde::from_owned_value::<Message>(content).ok())
                .unwrap_or_default(),
        })
        .collect()
}

/// 一条节点正文的可读形式；媒体保留可辨认的占位，别让模型以为是空消息。
pub fn describe(message: &Message) -> String {
    let mut out = String::new();
    for segment in &message.0 {
        match segment.type_.as_str() {
            "text" => out.push_str(string(segment, "text")),
            "at" => {
                let target = string(segment, "qq");
                let name = string(segment, "name");
                if name.is_empty() {
                    out.push_str(&format!("@{target}"));
                } else {
                    out.push_str(&format!("@{name}({target})"));
                }
            }
            "face" => out.push_str(&format!("[表情:{}]", string(segment, "id"))),
            "image" if is_sticker_picture(segment.data.get("sub_type")) => out.push_str("[表情包]"),
            "image" => out.push_str("[图片]"),
            "mface" => out.push_str(&mface_label(string(segment, "summary"))),
            "record" => out.push_str("[语音]"),
            "video" => out.push_str("[视频]"),
            "file" => {
                let name = string(segment, "name");
                if name.is_empty() {
                    out.push_str("[文件]");
                } else {
                    out.push_str(&format!("[文件:{name}]"));
                }
            }
            "reply" => out.push_str(&format!("[引用:{}]", string(segment, "id"))),
            "json" => out.push_str("[卡片]"),
            "poke" => out.push_str("[戳一戳]"),
            "dice" => out.push_str("[骰子]"),
            "rps" => out.push_str("[猜拳]"),
            "forward" | "node" => out.push_str("[嵌套合并转发]"),
            other => out.push_str(&format!("[{other}]")),
        }
    }
    let flat = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > MAX_NODE_CHARS {
        let kept: String = flat.chars().take(MAX_NODE_CHARS).collect();
        format!("{kept}…（本条已截断）")
    } else {
        flat
    }
}

/// 这张图是不是 QQ 的收藏/自定义表情：看的是它的 `sub_type`（图片子类型 1，satori-qq
/// 带在 `sub-type` 上）。
///
/// 群里斗图用的大多是这种，而不是商城表情；不认出来，它就和截图一样只是一张 `[图片]`。
pub(crate) fn is_sticker_picture(sub_type: Option<&simd_json::OwnedValue>) -> bool {
    sub_type.is_some_and(|value| {
        value.as_i64() == Some(1) || value.as_str().is_some_and(|value| value.trim() == "1")
    })
}

/// 商城表情在记录里的样子：带上它自己的名字（`[表情包:开心]`）。
///
/// 商城表情没有图片地址，模型看不见它长什么样；只写一个 `[图片]`，它就和截图、照片
/// 混在一起，也就想不到这是一张能偷来回人的表情包。QQ 给的名字自带方括号
/// （`[开心]`），套进标记之前先剥掉，免得方括号嵌套把占位标记切坏。
pub(crate) fn mface_label(summary: &str) -> String {
    let name: String = summary
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !matches!(c, '[' | ']'))
        .take(16)
        .collect();
    if name.is_empty() {
        "[表情包]".to_string()
    } else {
        format!("[表情包:{name}]")
    }
}

fn string<'a>(segment: &'a Segment, key: &str) -> &'a str {
    segment
        .data
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or("")
}

fn text_id(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_i64().map(|id| id.to_string()))
        .or_else(|| value.as_u64().map(|id| id.to_string()))
        .filter(|id| !id.is_empty())
}
