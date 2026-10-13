//! 房间这一轮是谁在问：提问的人、问的那条消息引用了谁、@了谁。
//!
//! 群里的房间收到的提示词只有正文——「他昨天说了什么」「我上周说过啥」里的「他」「我」
//! 是谁，模型无从得知，能做的只剩反问或乱猜。这里把触发消息的出处整理成一份，放进
//! `satori_context` 的回执，翻记录时拿 QQ 号去查。
//!
//! 放回执而不是系统提示词：提示词是请求的开头，一变整段前缀缓存就作废，而提问的人
//! 每个都不一样。

use crate::event::MessageEvent;
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsArray, ValueObjectAccessAsScalar};

#[derive(Clone, Debug, Default)]
pub(crate) struct Trigger {
    /// 触发这一轮的那条消息。
    pub message_id: String,
    pub user_id: String,
    /// 提问的人此刻用的名字（名片优先）。
    pub name: String,
    pub role: String,
    /// 它引用的那条消息。
    pub quoted: Option<String>,
    /// 它 @ 的人（QQ 号，去重，不含 `@全体`）。
    pub mentions: Vec<String>,
}

impl Trigger {
    pub(crate) fn from_event(event: &MessageEvent<'_>) -> Self {
        let mut trigger = Self {
            message_id: event.message_id().to_string(),
            user_id: event.user_id().to_string(),
            name: event.sender_name().to_string(),
            role: event.sender_role().unwrap_or("").to_string(),
            ..Self::default()
        };
        for segment in event.0.get_array("message").into_iter().flatten() {
            let Some(data) = segment.get("data") else {
                continue;
            };
            match segment.get_str("type") {
                Some("reply") if trigger.quoted.is_none() => {
                    trigger.quoted = data
                        .get_str("id")
                        .map(str::to_string)
                        .or_else(|| data.get_i64("id").map(|id| id.to_string()))
                        .filter(|id| !id.is_empty());
                }
                Some("at") => {
                    let Some(qq) = data
                        .get_str("qq")
                        .filter(|qq| !qq.is_empty() && qq.bytes().all(|b| b.is_ascii_digit()))
                    else {
                        continue;
                    };
                    if !trigger.mentions.iter().any(|seen| seen == qq) {
                        trigger.mentions.push(qq.to_string());
                    }
                }
                _ => {}
            }
        }
        trigger
    }
}
