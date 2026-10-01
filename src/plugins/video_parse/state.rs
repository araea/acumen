//! 同一条稿件刚取过没有。
//!
//! 取片是重活（几十兆下载加一次上传），同一个人在同一会话里把同一条链接贴两遍
//! 不该下两遍。每次动手前先在这里原子地占一次名额，落在
//! `data/video_parse/state.json`；超过保留期的记录在每次写入时顺手清掉，
//! 文件不会无限长。
//!
//! 名额在取片**开始**时占：同一毫秒进来的两条（手滑连发、客户端重发）只有一条
//! 真的去下。没取到由 [`release`] 撤掉，他重贴一次就能再来。

use crate::plugins::get_data_dir;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tokio::sync::Mutex as AsyncMutex;

const LOG_TARGET: &str = "Plugin/VideoParse";
const STATE_FILE: &str = "state.json";
/// 记录留多久。只有「刚刚是不是取过同一条」这一个用途，比判定窗口（十分钟）
/// 宽出一大截就够，不必留成一份台账。
const RETAIN_DAYS: i64 = 1;

/// 同一个人重复贴同一条稿件的判定窗口（秒）。
///
/// 手滑发两遍、编辑一下再发、从别处转回来，都落在这个窗口里；超过这个时间还贴，
/// 当成「他又要这条」，照旧再取一遍。
pub const REPEAT_WINDOW_SECONDS: i64 = 600;

/// 一条取过的稿件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Taken {
    /// 会话标识：群聊 `g:群号`，私聊 `u:用户 ID`
    pub target_id: String,
    /// 稿件号
    pub bvid: String,
    /// 贴这条链接的人
    pub requester: String,
    pub created_ts: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub takes: Vec<Taken>,
}

/// 一次「这条现在该不该取」的判定。
#[derive(Debug, PartialEq, Eq)]
pub enum Claim {
    /// 同一个人在同一个会话里刚贴过同一条：片子已经发出去了，或者正在取
    Recent,
    /// 可以取。名额已经占下，取失败由 [`release`] 撤掉
    Ready,
}

static STORE: OnceLock<AsyncMutex<Option<State>>> = OnceLock::new();

fn store() -> &'static AsyncMutex<Option<State>> {
    STORE.get_or_init(|| AsyncMutex::new(None))
}

async fn load_from_disk() -> State {
    let Ok(dir) = get_data_dir("video_parse").await else {
        warn!(target: LOG_TARGET, "无法创建数据目录，取片记录本次仅驻留内存。");
        return State::default();
    };
    let Ok(content) = tokio::fs::read_to_string(dir.join(STATE_FILE)).await else {
        return State::default();
    };
    match serde_json::from_str(&content) {
        Ok(state) => state,
        Err(e) => {
            warn!(target: LOG_TARGET, "取片记录解析失败({})，将重新开始记录。", e);
            State::default()
        }
    }
}

async fn save_to_disk(state: &State) {
    let Ok(dir) = get_data_dir("video_parse").await else {
        return;
    };
    let path = dir.join(STATE_FILE);
    let temporary = dir.join(format!("{STATE_FILE}.tmp"));
    match serde_json::to_string(state) {
        Ok(json) => {
            if let Err(e) = tokio::fs::write(&temporary, json).await {
                warn!(target: LOG_TARGET, "取片记录写入失败: {}", e);
            } else if let Err(e) = tokio::fs::rename(&temporary, &path).await {
                warn!(target: LOG_TARGET, "取片记录原子替换失败: {}", e);
            }
        }
        Err(e) => warn!(target: LOG_TARGET, "取片记录序列化失败: {}", e),
    }
}

/// 在全局锁内读改写状态，并把结果落盘。
async fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut guard = store().lock().await;
    if guard.is_none() {
        *guard = Some(load_from_disk().await);
    }
    let state = guard.as_mut().expect("状态已在上一步初始化");
    let result = f(state);
    let snapshot = state.clone();
    save_to_disk(&snapshot).await;
    result
}

/// 占一次取片名额：原子地判断这条能不能取，能取就记下来。
pub async fn claim(target_id: &str, requester: &str, bvid: &str, now: i64) -> Claim {
    let bvid = bvid.to_string();
    with_state(move |state| apply_claim(state, target_id, requester, &bvid, now)).await
}

/// 取片没成功，把名额放回去。
pub async fn release(target_id: &str, requester: &str, bvid: &str) {
    let bvid = bvid.to_string();
    with_state(move |state| apply_release(state, target_id, requester, &bvid)).await
}

/// `claim` 的纯逻辑部分，便于测试；不触碰全局状态与磁盘。
fn apply_claim(state: &mut State, target_id: &str, requester: &str, bvid: &str, now: i64) -> Claim {
    let cutoff = now - RETAIN_DAYS * 86_400;
    state.takes.retain(|taken| taken.created_ts >= cutoff);

    // 不知道是谁贴的（平台没给用户号）就没有「同一个人」可言，不去重——
    // 宁可多下一遍，也不把另一个人贴的链接一起吞掉。
    if requester.is_empty() {
        return Claim::Ready;
    }
    let recent = state.takes.iter().any(|taken| {
        taken.target_id == target_id
            && taken.requester == requester
            && taken.bvid == bvid
            && now - taken.created_ts <= REPEAT_WINDOW_SECONDS
    });
    if recent {
        return Claim::Recent;
    }
    state.takes.push(Taken {
        target_id: target_id.to_string(),
        bvid: bvid.to_string(),
        requester: requester.to_string(),
        created_ts: now,
    });
    Claim::Ready
}

/// `release` 的纯逻辑部分，便于测试；不触碰全局状态与磁盘。
fn apply_release(state: &mut State, target_id: &str, requester: &str, bvid: &str) {
    state.takes.retain(|taken| {
        !(taken.target_id == target_id && taken.requester == requester && taken.bvid == bvid)
    });
}
