//! 还没处理的加群申请。
//!
//! 申请是平台推来的一次性事件：错过了就只能等实现端在下次连上时补发（satori-qq 会补）。
//! 好几处要用它——入群欢迎按附言自动审批，智能体在管理那一栏看见并处理——所以由框架
//! 在事件进流水线之前记一份，谁都能查，不必谁依赖谁。只在内存里：重连时实现端补发的
//! 就是还没处理的那些，进程重启丢掉的也会这样回来。
//!
//! 进群（`guild-member-added`）或经我们之手批过之后划掉；三天没人管的自然过期。

use crate::event::{Context, EventType};
use simd_json::derived::ValueObjectAccessAsScalar;
use std::sync::Mutex;

/// 一条加群申请。`flag` 是审批时交回给实现端的那个号（`guild.member.approve` 的 `message_id`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinRequest {
    pub flag: String,
    pub group: String,
    pub user_id: String,
    /// 申请附言（入群问题的回答）。
    pub comment: String,
    /// 收到的时刻（Unix 秒）。
    pub at: i64,
}

const TTL_SECONDS: i64 = 3 * 86400;
const LIMIT: usize = 512;

fn book() -> &'static Mutex<Vec<JoinRequest>> {
    static BOOK: Mutex<Vec<JoinRequest>> = Mutex::new(Vec::new());
    &BOOK
}

fn with<R>(f: impl FnOnce(&mut Vec<JoinRequest>) -> R) -> R {
    let mut book = book()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = chrono::Utc::now().timestamp();
    book.retain(|request| now - request.at < TTL_SECONDS);
    f(&mut book)
}

/// 流水线之前看一眼：申请记下，进群了就划掉。
pub fn observe(ctx: &Context) {
    let EventType::Satori(raw) = &ctx.event else {
        return;
    };
    let group = raw.get_str("group_id").unwrap_or("");
    let user = raw.get_str("user_id").unwrap_or("");
    if group.is_empty() || user.is_empty() {
        return;
    }
    match raw.get_str("satori_type").unwrap_or("") {
        "guild-member-request" => {
            let flag = raw.get_str("flag").unwrap_or("");
            if flag.is_empty() {
                return;
            }
            let request = JoinRequest {
                flag: flag.to_string(),
                group: group.to_string(),
                user_id: user.to_string(),
                comment: raw.get_str("comment").unwrap_or("").trim().to_string(),
                at: raw
                    .get_i64("time")
                    .filter(|time| *time > 0)
                    .unwrap_or_else(|| chrono::Utc::now().timestamp()),
            };
            with(|book| {
                // 同一个人对同一个群再申请一次，以新的为准。
                book.retain(|old| {
                    old.flag != request.flag
                        && !(old.group == request.group && old.user_id == request.user_id)
                });
                if book.len() >= LIMIT {
                    book.remove(0);
                }
                book.push(request);
            });
        }
        "guild-member-added" => {
            with(|book| book.retain(|old| !(old.group == group && old.user_id == user)));
        }
        _ => {}
    }
}

/// 这个群还没处理的申请，旧的在前。
pub fn pending(group: &str) -> Vec<JoinRequest> {
    with(|book| {
        book.iter()
            .filter(|request| request.group == group)
            .cloned()
            .collect()
    })
}

/// 按申请号（或申请人的 QQ 号）找这个群的一条申请。
pub fn find(group: &str, key: &str) -> Option<JoinRequest> {
    let key = key.trim();
    with(|book| {
        book.iter()
            .rev()
            .find(|request| {
                request.group == group && (request.flag == key || request.user_id == key)
            })
            .cloned()
    })
}

/// 处理过了（同意或拒绝都算），从账上划掉。
pub fn resolve(flag: &str) {
    with(|book| book.retain(|request| request.flag != flag));
}
