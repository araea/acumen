//! 管理动作的记账：在哪个群、什么时候、对谁、做了什么、为什么。
//!
//! 两个用处。一是**加码**：人管群是看前科的——上周刷过屏、昨天刚被禁过十分钟的人，
//! 这次就不该再从「提醒一句」开始；窗口只有最近几十条，重启还会清空，所以得落盘。
//! 二是**兜底**：每群每天的管理次数有上限（`management_daily`），判断失误或被群友
//! 带节奏时，一天之内的后果封顶。
//!
//! 记录存在 `data/oai/chat/moderation.json`，与记忆、表情包库同属能力层：房间做的与
//! 搭话做的记在同一本账上，谁上次动的手都看得见。

use super::LOG_TARGET;
use crate::storage::JsonState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 每群留多久的账（秒）。
const KEEP_SECONDS: i64 = 30 * 86_400;
/// 每群最多留几条。
const KEEP_ENTRIES: usize = 300;
/// 递给模型看的前科回溯多久（秒）。
const RECENT_SECONDS: i64 = 7 * 86_400;
/// 递给模型看的最多几条。
const RECENT_ENTRIES: usize = 12;

/// 一次管理动作。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct Entry {
    /// Unix 秒。
    pub at: i64,
    /// `mute` / `kick` / `recall` / `mute_all` / …，与 `satori_action` 的动作名一致。
    pub action: String,
    /// 对象的 QQ 号；全群动作为空。
    pub user_id: String,
    /// 对象当时的名字（名片优先）。
    pub name: String,
    /// 禁言时长（秒）；其余动作为 0。
    pub seconds: u32,
    /// 动手时写的理由。
    pub reason: String,
    /// 谁动的手：`搭话`，或房间里吩咐的那个人。
    pub by: String,
    /// 算不算进每天的次数（解禁、取消精华这类收手的不算）。
    pub counted: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Ledger {
    groups: HashMap<String, Vec<Entry>>,
}

static STORE: JsonState<Ledger> =
    JsonState::new(LOG_TARGET, "oai", "chat/moderation.json", "管理记录");

/// 记一笔。顺手把过期的与超出条数的旧账清掉。
pub(crate) async fn record(group: &str, entry: Entry) {
    let group = group.to_string();
    STORE
        .with(move |ledger| {
            let now = entry.at;
            let list = ledger.groups.entry(group).or_default();
            list.push(entry);
            list.retain(|old| now - old.at < KEEP_SECONDS);
            if list.len() > KEEP_ENTRIES {
                let extra = list.len() - KEEP_ENTRIES;
                list.drain(..extra);
            }
        })
        .await;
}

/// 这个群从 `since`（Unix 秒）起算了几次。
pub(crate) async fn used_since(group: &str, since: i64) -> usize {
    STORE
        .with(|ledger| {
            ledger
                .groups
                .get(group)
                .map_or(0, |list| count_since(list, since))
        })
        .await
}

/// 最近的前科：七天内、最多十几条，从旧到新。
pub(crate) async fn recent(group: &str, now: i64) -> Vec<Entry> {
    STORE
        .with(|ledger| {
            ledger
                .groups
                .get(group)
                .map(|list| recent_of(list, now))
                .unwrap_or_default()
        })
        .await
}

fn count_since(list: &[Entry], since: i64) -> usize {
    list.iter()
        .filter(|entry| entry.counted && entry.at >= since)
        .count()
}

fn recent_of(list: &[Entry], now: i64) -> Vec<Entry> {
    let fresh: Vec<Entry> = list
        .iter()
        .filter(|entry| now - entry.at < RECENT_SECONDS)
        .cloned()
        .collect();
    let skip = fresh.len().saturating_sub(RECENT_ENTRIES);
    fresh.into_iter().skip(skip).collect()
}

/// 北京时间今天零点（Unix 秒）：每天的次数从这里起算。
pub(crate) fn today_start() -> i64 {
    let now = crate::clock::beijing_now();
    now.date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|midnight| midnight.and_local_timezone(now.timezone()).single())
        .map_or_else(|| now.timestamp(), |midnight| midnight.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(at: i64, counted: bool) -> Entry {
        Entry {
            at,
            action: "mute".into(),
            counted,
            ..Entry::default()
        }
    }

    #[test]
    fn lifting_a_punishment_does_not_use_up_the_day() {
        let list = vec![entry(100, true), entry(200, false), entry(300, true)];
        assert_eq!(count_since(&list, 0), 2);
        assert_eq!(count_since(&list, 150), 1);
    }

    #[test]
    fn recent_keeps_the_latest_week_in_order() {
        let now = 10 * 86_400;
        let mut list: Vec<Entry> = (0..20).map(|i| entry(now - i * 3600, true)).collect();
        list.reverse();
        list.insert(0, entry(now - 8 * 86_400, true));
        let recent = recent_of(&list, now);
        assert_eq!(recent.len(), RECENT_ENTRIES);
        assert_eq!(recent.last().unwrap().at, now);
        assert!(recent.windows(2).all(|pair| pair[0].at <= pair[1].at));
    }
}
