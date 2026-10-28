//! 群目录：账号加入的每个群叫什么、有多少人，记在手边，变了就跟着换。
//!
//! 入群欢迎要写群名，原先靠入群事件自带（靠不住：老版本知弦不带，新版本也只在它记着时才带）
//! 或现问 `guild.get`（慢，等不及就只剩群号）。现在的口径：
//!
//! - 连上时翻一遍 `guild.list` 把名字记全；之后每条带群名的事件（群消息都带）顺手核对，
//!   改名推的 `guild-updated` 直接换上，退群推的 `guild-removed` 删掉；
//! - 人数从 `guild.list` 来（知弦 0.54.0 起带），随进群、退群事件加减，[`FRESH`] 过了有人要用时才在后台问一次 `guild.get` 校正；
//! - 记着的直接用、不等；只有从没见过的群才限时现问，问也在后台跑完记下，下次不用等；
//! - 落盘（`data/satori/guilds.json`），重启后第一个进群的人就有群名。

use super::LockedWriter;
use crate::event::Context;
use crate::{debug, warn};
use futures_util::FutureExt;
use futures_util::future::{BoxFuture, Shared};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

/// 上次和实现端核对过这么久内不再问（秒）。名字有事件跟着换，这里主要是给人数校正用。
const FRESH: i64 = 30 * 60;
/// 问失败后这么久内不再问（秒），免得接口卡住时每次都去堵一次。
const FAILURE_PAUSE: i64 = 60;

const FILE: &str = "guilds.json";

/// 记着的一个群。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Guild {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<i32>,
    /// 上次和实现端（`guild.get` / `guild.list`）核对的时刻（Unix 秒）。
    #[serde(default)]
    checked: i64,
}

type Flight = Shared<BoxFuture<'static, Option<Guild>>>;

#[derive(Default)]
struct Book {
    loaded: bool,
    known: HashMap<String, Guild>,
    failed: HashMap<String, i64>,
    flying: HashMap<String, Flight>,
}

fn book() -> MutexGuard<'static, Book> {
    static BOOK: OnceLock<Mutex<Book>> = OnceLock::new();
    let mut book = BOOK
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !book.loaded {
        book.loaded = true;
        if let Some(saved) = load() {
            for (key, guild) in saved {
                book.known.entry(key).or_insert(guild);
            }
        }
    }
    book
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn key(self_id: &str, group: &str) -> String {
    format!("{self_id}/{group}")
}

fn load() -> Option<HashMap<String, Guild>> {
    let path = crate::storage::data_path("satori").ok()?.join(FILE);
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn save(book: &Book) {
    // 单测里没有运行时，也不该写到真的数据目录。
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    let Ok(bytes) = serde_json::to_vec_pretty(&book.known) else {
        return;
    };
    let Ok(dir) = crate::storage::data_path("satori") else {
        return;
    };
    tokio::spawn(async move {
        if let Err(error) = crate::storage::write_atomic_async(dir.join(FILE), bytes).await {
            debug!("群目录没能落盘：{error}");
        }
    });
}

/// 改一条记录；名字或人数真变了才落盘。
fn edit(self_id: &str, group: &str, change: impl FnOnce(&mut Guild)) {
    if self_id.is_empty() || group.is_empty() {
        return;
    }
    let mut book = book();
    let entry = book.known.entry(key(self_id, group)).or_default();
    let before = (entry.name.clone(), entry.members);
    change(entry);
    if (entry.name.clone(), entry.members) != before {
        save(&book);
    }
}

/// 事件里或列表里见到的群名。空名不算数（知弦的入群灰条、成员事件都带空名）。
pub fn learn_name(self_id: &str, group: &str, name: &str) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    edit(self_id, group, |guild| {
        if guild.name != name {
            name.clone_into(&mut guild.name);
        }
    });
}

/// 把实现端给的一份资料（`guild.get`、`guild.list` 的一项）记进去；名字或人数变了返回 `true`。
fn apply(guild: &mut Guild, resource: &Value) -> bool {
    let name = resource.get("name").and_then(Value::as_str).unwrap_or("").trim();
    let members = resource
        .get("member_count")
        .and_then(Value::as_i64)
        .and_then(|n| i32::try_from(n).ok());
    let before = (guild.name.clone(), guild.members);
    if !name.is_empty() {
        name.clone_into(&mut guild.name);
    }
    if members.is_some() {
        guild.members = members;
    }
    guild.checked = now();
    (guild.name.clone(), guild.members) != before
}

pub(super) fn learn_resource(self_id: &str, group: &str, resource: &Value) -> Guild {
    let mut book = book();
    let guild = book.known.entry(key(self_id, group)).or_default();
    let changed = apply(guild, resource);
    let guild = guild.clone();
    if changed {
        save(&book);
    }
    guild
}

/// 有人进群（`+1`）或退群（`-1`）。人数不知道就不猜，等下次核对。
pub fn adjust_members(self_id: &str, group: &str, delta: i32) {
    if book().known.get(&key(self_id, group)).is_none_or(|guild| guild.members.is_none()) {
        return;
    }
    edit(self_id, group, |guild| {
        guild.members = guild.members.map(|n| (n + delta).max(0));
    });
}

/// 账号退出或被移出了这个群。
pub fn forget(self_id: &str, group: &str) {
    let mut book = book();
    if book.known.remove(&key(self_id, group)).is_some() {
        save(&book);
    }
}

/// 连上时翻完的一份 `guild.list`：逐个记下；`complete`（翻到底了）时还把列表里没有的群
/// （离线期间退了的）删掉。整份只落一次盘。
pub fn learn_list(self_id: &str, guilds: &[Value], complete: bool) {
    let mut book = book();
    let mut changed = false;
    let mut seen = std::collections::HashSet::new();
    for resource in guilds {
        let Some(id) = resource.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()) else {
            continue;
        };
        let key = key(self_id, id);
        changed |= apply(book.known.entry(key.clone()).or_default(), resource);
        seen.insert(key);
    }
    if complete && !seen.is_empty() {
        let prefix = key(self_id, "");
        let before = book.known.len();
        book.known.retain(|key, _| !key.starts_with(&prefix) || seen.contains(key));
        changed |= book.known.len() != before;
    }
    if changed {
        save(&book);
    }
}

/// 不发请求，只看记着的。
#[cfg(test)]
fn cached(self_id: &str, group: &str) -> Option<Guild> {
    book().known.get(&key(self_id, group)).filter(|guild| !guild.name.is_empty()).cloned()
}

/// 群资料，按需等。
///
/// 记着名字（要人数时还得记着人数）就直接返回，过了 [`FRESH`] 才在后台核对一次、不等；
/// 缺了才等现问，最多等 `wait`，问的结果在后台照样记下。返回 `None` 是这个群一无所知。
pub async fn lookup(
    ctx: &Context,
    writer: &LockedWriter,
    group: &str,
    want_members: bool,
    wait: Duration,
) -> Option<Guild> {
    let self_id = ctx.bot.self_id();
    let key = key(&self_id, group);
    let (known, flight) = {
        let mut book = book();
        let at = now();
        let known = book.known.get(&key).cloned();
        let stale = known.as_ref().is_none_or(|guild| at - guild.checked >= FRESH);
        let paused = book.failed.get(&key).is_some_and(|failed| at - failed < FAILURE_PAUSE);
        let flight = match book.flying.get(&key) {
            Some(flight) => Some(flight.clone()),
            None if stale && !paused => Some(take_off(&mut book, &key, ctx, writer, &self_id, group)),
            None => None,
        };
        let enough = known
            .as_ref()
            .is_some_and(|guild| !guild.name.is_empty() && (!want_members || guild.members.is_some()));
        if enough {
            return known;
        }
        (known, flight)
    };
    let fetched = match flight {
        Some(flight) => tokio::time::timeout(wait, flight).await.ok().flatten(),
        None => None,
    };
    fetched.or(known).filter(|guild| !guild.name.is_empty() || guild.members.is_some())
}

/// 发出（或接上已经在飞的）一次 `guild.get`。交给后台任务驱动：等的人走了，结果照样记下。
fn take_off(
    book: &mut Book,
    key: &str,
    ctx: &Context,
    writer: &LockedWriter,
    self_id: &str,
    group: &str,
) -> Flight {
    let (ctx, writer, self_id, group, key_owned) = (
        ctx.clone(),
        writer.clone(),
        self_id.to_string(),
        group.to_string(),
        key.to_string(),
    );
    let flight = async move {
        let result = writer
            .call::<_, Value>(&ctx, "guild.get", json!({"guild_id": group}))
            .await;
        let guild = match result {
            Ok(resource) => {
                self::book().failed.remove(&key_owned);
                Some(learn_resource(&self_id, &group, &resource))
            }
            Err(error) => {
                warn!("群 {group} 的资料没问到：{error}");
                self::book().failed.insert(key_owned.clone(), now());
                None
            }
        };
        self::book().flying.remove(&key_owned);
        guild
    }
    .boxed()
    .shared();
    book.flying.insert(key.to_string(), flight.clone());
    tokio::spawn(flight.clone());
    flight
}

/// 从一条入站事件里学：带群名就记下（改名事件、群消息），进退群加减人数，退群删掉。
pub fn observe(self_id: &str, group: &str, event_type: &str, body: &Value) {
    if group.is_empty() {
        return;
    }
    let name = ["guild", "channel"]
        .iter()
        .filter_map(|key| body.get(*key)?.get("name")?.as_str())
        .find(|name| !name.trim().is_empty());
    match event_type {
        "guild-removed" => return forget(self_id, group),
        "guild-member-added" => adjust_members(self_id, group, 1),
        "guild-member-removed" => {
            let user = body.pointer("/user/id").and_then(Value::as_str);
            if user == Some(self_id) {
                return forget(self_id, group);
            }
            adjust_members(self_id, group, -1);
        }
        _ => {}
    }
    if let Some(name) = name {
        learn_name(self_id, group, name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_events_and_blank_names_are_ignored() {
        observe("t1", "g1", "message-created", &json!({"guild": {"id": "g1", "name": "旧名"}}));
        assert_eq!(cached("t1", "g1").map(|g| g.name).as_deref(), Some("旧名"));
        observe("t1", "g1", "guild-member-added", &json!({"guild": {"id": "g1", "name": ""}}));
        assert_eq!(cached("t1", "g1").map(|g| g.name).as_deref(), Some("旧名"));
        observe("t1", "g1", "guild-updated", &json!({"guild": {"id": "g1", "name": "新名"}}));
        assert_eq!(cached("t1", "g1").map(|g| g.name).as_deref(), Some("新名"));
        observe("t1", "g1", "guild-removed", &json!({"guild": {"id": "g1", "name": "新名"}}));
        assert_eq!(cached("t1", "g1"), None);
    }

    #[test]
    fn member_counts_move_only_once_known() {
        observe("t2", "g", "guild-member-added", &json!({"guild": {"id": "g", "name": "群"}}));
        assert_eq!(cached("t2", "g").and_then(|g| g.members), None);
        learn_list("t2", &[json!({"id": "g", "name": "群", "member_count": 10})], false);
        observe("t2", "g", "guild-member-added", &json!({}));
        observe("t2", "g", "guild-member-added", &json!({}));
        observe("t2", "g", "guild-member-removed", &json!({"user": {"id": "u"}}));
        assert_eq!(cached("t2", "g").and_then(|g| g.members), Some(11));
        observe("t2", "g", "guild-member-removed", &json!({"user": {"id": "t2"}}));
        assert_eq!(cached("t2", "g"), None);
    }

    #[test]
    fn a_complete_list_drops_groups_left_while_offline() {
        learn_name("t3", "a", "甲");
        learn_name("t3", "b", "乙");
        learn_name("t4", "b", "别的号");
        learn_list("t3", &[json!({"id": "a", "name": "甲"})], true);
        assert!(cached("t3", "a").is_some());
        assert_eq!(cached("t3", "b"), None);
        assert!(cached("t4", "b").is_some());
    }
}
