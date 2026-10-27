//! 群里的身份（群主 / 管理员 / 普通成员）：查一次记一阵，查不到不当成普通成员。
//!
//! 入群验证、关键词守卫、奶龙群管理动手前都先问一句「账号罚得动人吗」。原先查询失败记成
//! 空串、一记十分钟——接口偶尔超时一次，整整十分钟都当自己不是管理员，入群验证悄悄跳过。
//! 现在的口径：
//!
//! - 查到的身份 [`FRESH`] 内直接用；过了也先用旧的，同时在后台换新（身份很少变，旧的
//!   比「不知道」可靠得多），旧到 [`STALE_LIMIT`] 才作废；
//! - 同一个人同时被问好几次只发一个请求，请求在后台跑完，调用方等不及先走也不会半途作废；
//! - 查询失败（超时、实现端报错）不记成普通成员：调用方拿到 `None`，自己决定怎么降级；
//!   失败后 [`FAILURE_PAUSE`] 内不再重问，免得接口卡住时每条消息都去堵一次；
//! - 账号自己的身份落盘（`data/satori/own-roles.json`），重启后头一个进群的人不必等现查；
//! - 事件里带的身份（实现端推的身份变更、发言者是群主或管理员）顺手记下。

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

/// 查到的身份这么久内直接用（秒）。
const FRESH: i64 = 10 * 60;
/// 过了 [`FRESH`] 还能先拿来用、同时后台换新的上限（秒）。
const STALE_LIMIT: i64 = 7 * 86400;
/// 查询失败后这么久内不再重问（秒）。
const FAILURE_PAUSE: i64 = 20;
/// 第一次失败后隔多久再试一次：实现端拉成员表、内核刚连上时常有一两秒的空窗。
const RETRY_AFTER: Duration = Duration::from_millis(800);

const FILE: &str = "own-roles.json";

#[derive(Clone, Serialize, Deserialize)]
struct Known {
    role: String,
    /// 查到的时刻（Unix 秒）
    at: i64,
}

type Flight = Shared<BoxFuture<'static, Option<String>>>;

#[derive(Default)]
struct Book {
    loaded: bool,
    known: HashMap<String, Known>,
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
            book.known.extend(saved);
        }
    }
    book
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn key(self_id: &str, group: &str, user: &str) -> String {
    format!("{self_id}/{group}/{user}")
}

fn is_own(key: &str) -> bool {
    let mut parts = key.split('/');
    let (me, _, user) = (parts.next(), parts.next(), parts.next());
    me.is_some() && me == user
}

fn load() -> Option<HashMap<String, Known>> {
    let path = crate::storage::data_path("satori").ok()?.join(FILE);
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 只存账号自己的那几条：别人的身份变得勤，重启后现查就是。
fn save(book: &Book) {
    let own: HashMap<&String, &Known> = book.known.iter().filter(|(key, _)| is_own(key)).collect();
    let Ok(bytes) = serde_json::to_vec_pretty(&own) else {
        return;
    };
    let Ok(dir) = crate::storage::data_path("satori") else {
        return;
    };
    tokio::spawn(async move {
        if let Err(error) = crate::storage::write_atomic_async(dir.join(FILE), bytes).await {
            debug!("账号身份没能落盘：{error}");
        }
    });
}

/// 记下一条身份。实现端主动告诉的、查回来的都走这里；同样的结果只刷新时刻，不重写文件。
pub fn learn(self_id: &str, group: &str, user: &str, role: &str) {
    if self_id.is_empty() || group.is_empty() || user.is_empty() || role.is_empty() {
        return;
    }
    let key = key(self_id, group, user);
    let mut book = book();
    book.failed.remove(&key);
    let changed = book.known.get(&key).is_none_or(|known| known.role != role);
    book.known.insert(
        key.clone(),
        Known {
            role: role.to_string(),
            at: now(),
        },
    );
    if changed && is_own(&key) {
        save(&book);
    }
}

/// 这个人在群里的身份；`None` 是这次没查到（不是「普通成员」）。
pub async fn role(ctx: &Context, writer: &LockedWriter, group: &str, user: &str) -> Option<String> {
    let key = key(&ctx.bot.self_id(), group, user);
    let flight = {
        let mut book = book();
        let at = now();
        if let Some(known) = book.known.get(&key) {
            let age = at - known.at;
            if age < FRESH {
                return Some(known.role.clone());
            }
            if age < STALE_LIMIT {
                let role = known.role.clone();
                drop(take_off(&mut book, &key, ctx, writer, group, user));
                return Some(role);
            }
        }
        if !book.flying.contains_key(&key)
            && book
                .failed
                .get(&key)
                .is_some_and(|failed| at - failed < FAILURE_PAUSE)
        {
            return None;
        }
        take_off(&mut book, &key, ctx, writer, group, user)
    };
    flight.await
}

/// 不等结果：没记着或记的旧了，就在后台问一次。消息多的群里随手调用，开销只是查一下表。
pub fn warm(ctx: &Context, writer: &LockedWriter, group: &str, user: &str) {
    let key = key(&ctx.bot.self_id(), group, user);
    let mut book = book();
    let at = now();
    if book
        .known
        .get(&key)
        .is_some_and(|known| at - known.at < FRESH)
        || book.flying.contains_key(&key)
        || book
            .failed
            .get(&key)
            .is_some_and(|failed| at - failed < FAILURE_PAUSE)
    {
        return;
    }
    drop(take_off(&mut book, &key, ctx, writer, group, user));
}

/// 发出（或接上已经在飞的）一次查询。查询本身交给后台任务驱动：等的人超时走开了，
/// 结果照样记下，下一个人就能直接用。
fn take_off(
    book: &mut Book,
    key: &str,
    ctx: &Context,
    writer: &LockedWriter,
    group: &str,
    user: &str,
) -> Flight {
    if let Some(flight) = book.flying.get(key) {
        return flight.clone();
    }
    let (ctx, writer, group, user, key_owned) = (
        ctx.clone(),
        writer.clone(),
        group.to_string(),
        user.to_string(),
        key.to_string(),
    );
    let flight = async move {
        let role = query(&ctx, &writer, &group, &user).await;
        let mut book = self::book();
        book.flying.remove(&key_owned);
        if let Some(role) = &role {
            book.failed.remove(&key_owned);
            let changed = book
                .known
                .get(&key_owned)
                .is_none_or(|known| &known.role != role);
            book.known.insert(
                key_owned.clone(),
                Known {
                    role: role.clone(),
                    at: now(),
                },
            );
            if changed && is_own(&key_owned) {
                save(&book);
            }
        } else {
            book.failed.insert(key_owned.clone(), now());
            if let Some(known) = book.known.get(&key_owned) {
                // 有旧结果就接着用旧的，只是这次没换成新的。
                return Some(known.role.clone());
            }
        }
        role
    }
    .boxed()
    .shared();
    book.flying.insert(key.to_string(), flight.clone());
    tokio::spawn(flight.clone());
    flight
}

/// 问一次 `guild.member.get`，失败隔一会儿再问一次。实现端没给 `roles` 的当普通成员
/// （有的平台根本没有群身份这回事）；两次都失败才是「没查到」。
async fn query(ctx: &Context, writer: &LockedWriter, group: &str, user: &str) -> Option<String> {
    let params = json!({"guild_id": group, "user_id": user});
    let mut last = String::new();
    for attempt in 0..2 {
        if attempt > 0 {
            tokio::time::sleep(RETRY_AFTER).await;
        }
        match writer
            .call::<_, Value>(ctx, "guild.member.get", params.clone())
            .await
        {
            Ok(value) => return Some(role_of(&value).unwrap_or("member").to_string()),
            Err(error) => last = error.to_string(),
        }
    }
    warn!("群 {group} 里 {user} 的身份没查到：{last}");
    None
}

/// 成员资源里的身份：`roles` 的第一项。
pub fn role_of(member: &Value) -> Option<&str> {
    member
        .get("roles")
        .and_then(Value::as_array)
        .and_then(|roles| roles.first())
        .and_then(|role| role.get("id"))
        .and_then(Value::as_str)
        .filter(|role| !role.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_keys_are_the_ones_saved() {
        assert!(is_own("10/20/10"));
        assert!(!is_own("10/20/30"));
    }

    #[test]
    fn learned_role_is_cached_and_replaced() {
        let role = |user| {
            book()
                .known
                .get(&key("t1", "g", user))
                .map(|known| known.role.clone())
        };
        learn("t1", "g", "u", "admin");
        assert_eq!(role("u").as_deref(), Some("admin"));
        learn("t1", "g", "u", "member");
        assert_eq!(role("u").as_deref(), Some("member"));
        assert_eq!(role("other"), None);
    }

    #[test]
    fn roles_come_from_the_first_entry() {
        assert_eq!(role_of(&json!({"roles": [{"id": "owner"}]})), Some("owner"));
        assert_eq!(role_of(&json!({"roles": []})), None);
        assert_eq!(role_of(&json!({})), None);
    }
}
