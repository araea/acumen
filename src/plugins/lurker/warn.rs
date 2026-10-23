//! 提醒：先在群里 @ 一下潜水的人，说清几天后还没冒泡会被请出去。
//!
//! 记下谁在什么时候被提醒过（`data/lurker/warnings.json`）：名单上标「几天前提醒过」，
//! 自动清理按它数宽限期。提醒一个月后作废——那时候该重新提醒一遍。

use super::{Config, LOG_TARGET, date};
use crate::adapters::satori::{LockedWriter, send_msg};
use crate::event::Context;
use crate::message::Message;
use crate::plugins::PluginError;
use crate::storage::JsonState;
use crate::template::{Piece, pieces};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 一条提醒最多 @ 几个人：再多一屏就全是蓝字了。
const PER_MESSAGE: usize = 20;
const EXPIRE: i64 = 30 * 86_400;

#[derive(Serialize, Deserialize, Default, Clone)]
struct Book {
    /// 群 → 人 → 提醒的时刻。
    groups: HashMap<String, HashMap<String, i64>>,
}

static BOOK: JsonState<Book> = JsonState::new(LOG_TARGET, "lurker", "warnings.json", "提醒记录");

/// 这个群里谁被提醒过、什么时候（一个月内的）。
pub async fn warned(group: &str) -> HashMap<String, i64> {
    let now = chrono::Utc::now().timestamp();
    BOOK.read(|book| {
        book.groups
            .get(group)
            .map(|people| people.iter().filter(|(_, at)| now - **at < EXPIRE).map(|(id, at)| (id.clone(), *at)).collect())
            .unwrap_or_default()
    })
    .await
}

/// 移出了、或不必再记的人划掉。
pub async fn forget(group: &str, ids: &[String]) {
    if ids.is_empty() {
        return;
    }
    BOOK.with(|book| {
        if let Some(people) = book.groups.get_mut(group) {
            people.retain(|id, _| !ids.contains(id));
        }
    })
    .await;
}

/// @ 这些人发提醒，记下时刻。返回提醒到了几个人。
pub async fn send(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    group: &str,
    people: &[(String, String)],
) -> Result<usize, PluginError> {
    let now = chrono::Utc::now().timestamp();
    let deadline = date(now + i64::from(config.warn.grace_days) * 86_400);
    let days = super::roster::rule_for(config, group).days;
    let mut done = Vec::new();
    for batch in people.chunks(PER_MESSAGE) {
        let Some(text) = pick(&config.warn.text) else {
            break;
        };
        let values = HashMap::from([
            ("days", days.to_string()),
            ("deadline", deadline.clone()),
            ("grace", config.warn.grace_days.to_string()),
            ("count", people.len().to_string()),
        ]);
        let mut message = Message::new();
        for piece in pieces(&text, &values) {
            message = match piece {
                Piece::Text(text) => message.text(text),
                Piece::At => batch.iter().fold(message, |m, (id, _)| m.at(id).text(" ")),
                Piece::Face(id) => message.face(id),
                Piece::Image(_) | Piece::Avatar => message,
            };
        }
        send_msg(ctx, writer.clone(), Some(group), None, message).await?;
        done.extend(batch.iter().map(|(id, _)| id.clone()));
        if people.len() > PER_MESSAGE {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    }
    let count = done.len();
    BOOK.with(|book| {
        let people = book.groups.entry(group.to_string()).or_default();
        people.retain(|_, at| now - *at < EXPIRE);
        for id in done {
            people.insert(id, now);
        }
    })
    .await;
    info!(target: LOG_TARGET, "群 {group} 提醒了 {count} 位潜水成员");
    Ok(count)
}

/// 几种说法随机一种；全空 = 不发。
pub fn pick(variants: &[String]) -> Option<String> {
    let usable: Vec<&String> = variants.iter().filter(|text| !text.trim().is_empty()).collect();
    (!usable.is_empty()).then(|| usable[rand::random_range(0..usable.len())].clone())
}
