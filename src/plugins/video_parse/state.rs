//! 预览消息指向哪一条稿件，以及这一条取过片没有。
//!
//! 与 AI 资讯的「引用卡片回复序号」是同一套做法：一级只发一张预览，
//! 用户引用它再开口时才做重活（见 `plugins/ai_news/state.rs`）。这里只记
//! 「预览消息 ID → 稿件」的对应关系，落在 `data/video_parse/state.json`；
//! 超过保留期的记录在每次写入时顺手清掉，文件不会无限长。

use crate::plugins::get_data_dir;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tokio::sync::Mutex as AsyncMutex;

const LOG_TARGET: &str = "Plugin/VideoParse";
const STATE_FILE: &str = "state.json";
/// 预览与取片的对应关系留多久。QQ 上翻得到的老消息都能继续引用。
const RETAIN_DAYS: i64 = 30;
// 与 ai_news 的 `EXTRACTION_RETAIN_DAYS` 是同一个口径（引用驱动一律留 30 天，见 docs/INTERACTION.md 第三节）。

/// 同一个人重复贴同一条稿件的判定窗口（秒）。
///
/// 手滑发两遍、编辑一下再发、从别处转回来，都落在这个窗口里；超过这个时间还贴，
/// 当成「他又想要这条」，照旧回一条预览。
pub const REPEAT_WINDOW_SECONDS: i64 = 600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preview {
    /// 会话标识：群聊是群号，私聊取用户号的负数
    pub target_id: i64,
    /// 预览那条消息的 ID
    pub message_id: String,
    pub created_ts: i64,
    /// 用户当时发的那条链接
    pub url: String,
    pub bvid: String,
    /// 选中那一 P 的 `cid`
    pub cid: i64,
    pub page: u32,
    pub title: String,
    pub duration: u64,
    /// 已经取过片。取片开始时置位，取失败时由 [`release`] 复位。
    #[serde(default)]
    pub extracted: bool,
    /// 贴这条链接的人。只用来认「同一个人又贴了一遍」，老记录缺这个字段时按 0 算。
    #[serde(default)]
    pub requester: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub previews: Vec<Preview>,
}

/// 一次「引用预览 + 回复取片词」的判定结果。
#[derive(Debug)]
pub enum Claim {
    /// 引用的不是本插件发过的预览
    Missing,
    /// 这张预览已经取过片了
    AlreadyExtracted,
    /// 可以取，附带预览里记下的稿件
    Ready(Preview),
}

static STORE: OnceLock<AsyncMutex<Option<State>>> = OnceLock::new();

fn store() -> &'static AsyncMutex<Option<State>> {
    STORE.get_or_init(|| AsyncMutex::new(None))
}

async fn load_from_disk() -> State {
    let Ok(dir) = get_data_dir("video_parse").await else {
        warn!(target: LOG_TARGET, "无法创建数据目录，预览记录本次仅驻留内存。");
        return State::default();
    };
    let Ok(content) = tokio::fs::read_to_string(dir.join(STATE_FILE)).await else {
        return State::default();
    };
    match serde_json::from_str(&content) {
        Ok(state) => state,
        Err(e) => {
            warn!(target: LOG_TARGET, "预览记录解析失败({})，将重新开始记录。", e);
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
                warn!(target: LOG_TARGET, "预览记录写入失败: {}", e);
            } else if let Err(e) = tokio::fs::rename(&temporary, &path).await {
                warn!(target: LOG_TARGET, "预览记录原子替换失败: {}", e);
            }
        }
        Err(e) => warn!(target: LOG_TARGET, "预览记录序列化失败: {}", e),
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

/// 记下一条刚发出的预览。
pub async fn remember(preview: Preview) {
    let cutoff = Utc::now().timestamp() - RETAIN_DAYS * 86_400;
    with_state(move |state| {
        state.previews.retain(|seen| {
            seen.created_ts >= cutoff
                && !(seen.target_id == preview.target_id
                    && seen.message_id == preview.message_id)
        });
        state.previews.push(preview);
    })
    .await
}

/// 同一个人短时间内又贴了同一条稿件，而那张预览还在等他开口取片。
///
/// 这种重复不该再回一条一模一样的预览——封面加正文是群里最占地方的那种消息，
/// 手滑发两遍就刷两条实在没必要。判据收得紧，只吞「同一个人 + 同一条稿件 +
/// 上一张还没取过 + 还在窗口内」这四条都成的；换个人贴、隔久了再贴、上一张已经
/// 取过片，都照旧回新预览（那时他多半是真想要这条）。
pub async fn repeated(target_id: i64, requester: i64, bvid: &str, now: i64) -> bool {
    let bvid = bvid.to_string();
    with_state(move |state| apply_repeat(state, target_id, requester, &bvid, now)).await
}

/// `repeated` 的纯逻辑部分，便于测试；不触碰全局状态与磁盘。
fn apply_repeat(state: &State, target_id: i64, requester: i64, bvid: &str, now: i64) -> bool {
    state.previews.iter().any(|seen| {
        seen.target_id == target_id
            && seen.requester == requester
            && requester != 0
            && seen.bvid == bvid
            && !seen.extracted
            && now - seen.created_ts <= REPEAT_WINDOW_SECONDS
    })
}

/// 抢占一次取片：原子地判断这张预览能不能取，并把记录标成已取。
///
/// 标在取片**开始**而不是结束时，是为了同一张预览被连着引用两次时只下一遍
/// （后一次会收到「已经取过了」）。取失败由 [`release`] 复位，用户可以再点一次。
pub async fn claim(target_id: i64, message_id: &str) -> Claim {
    let message_id = message_id.to_string();
    let cutoff = Utc::now().timestamp() - RETAIN_DAYS * 86_400;
    with_state(move |state| apply_claim(state, target_id, &message_id, cutoff)).await
}

/// 取片没成功，把标记放回去。
pub async fn release(target_id: i64, message_id: &str) {
    let message_id = message_id.to_string();
    with_state(move |state| {
        if let Some(preview) = state
            .previews
            .iter_mut()
            .find(|preview| preview.target_id == target_id && preview.message_id == message_id)
        {
            preview.extracted = false;
        }
    })
    .await
}

/// `claim` 的纯逻辑部分，便于测试；不触碰全局状态与磁盘。
fn apply_claim(state: &mut State, target_id: i64, message_id: &str, cutoff: i64) -> Claim {
    state.previews.retain(|seen| seen.created_ts >= cutoff);
    let Some(preview) = state
        .previews
        .iter_mut()
        .find(|preview| preview.target_id == target_id && preview.message_id == message_id)
    else {
        return Claim::Missing;
    };
    if preview.extracted {
        return Claim::AlreadyExtracted;
    }
    preview.extracted = true;
    Claim::Ready(preview.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(message_id: &str) -> Preview {
        Preview {
            target_id: 42,
            message_id: message_id.to_string(),
            created_ts: 100,
            url: "https://b23.tv/abc".into(),
            bvid: "BV1GJ411x7h7".into(),
            cid: 137649199,
            page: 1,
            title: "测试稿件".into(),
            duration: 213,
            extracted: false,
            requester: 7,
        }
    }

    #[test]
    fn the_same_person_pasting_the_same_link_again_does_not_get_a_second_preview() {
        let state = State {
            previews: vec![preview("m1")],
        };
        // 同一个人、同一条稿件、还没取过、还在窗口内。
        assert!(apply_repeat(&state, 42, 7, "BV1GJ411x7h7", 100));
        assert!(apply_repeat(&state, 42, 7, "BV1GJ411x7h7", 100 + REPEAT_WINDOW_SECONDS));

        // 换个人、换个群、换条稿件、隔久了，都照旧回新预览。
        assert!(!apply_repeat(&state, 42, 8, "BV1GJ411x7h7", 100));
        assert!(!apply_repeat(&state, 43, 7, "BV1GJ411x7h7", 100));
        assert!(!apply_repeat(&state, 42, 7, "BV1other", 100));
        assert!(
            !apply_repeat(&state, 42, 7, "BV1GJ411x7h7", 100 + REPEAT_WINDOW_SECONDS + 1),
            "过了窗口就该当成他又想要这条"
        );
    }

    #[test]
    fn a_taken_preview_does_not_block_the_next_one() {
        let mut state = State {
            previews: vec![preview("m1")],
        };
        state.previews[0].extracted = true;
        assert!(
            !apply_repeat(&state, 42, 7, "BV1GJ411x7h7", 100),
            "上一张已经取过片，再贴就该给一张新的"
        );
    }

    #[test]
    fn a_record_without_a_requester_never_matches() {
        let mut state = State {
            previews: vec![preview("m1")],
        };
        // 老文件里的记录没有 `requester`，反序列化后是 0：不去猜是谁贴的。
        state.previews[0].requester = 0;
        assert!(!apply_repeat(&state, 42, 0, "BV1GJ411x7h7", 100));
    }

    #[test]
    fn a_fresh_preview_can_be_claimed_exactly_once() {
        let mut state = State {
            previews: vec![preview("m1")],
        };

        let claimed = apply_claim(&mut state, 42, "m1", 0);
        assert!(matches!(claimed, Claim::Ready(_)));
        assert_eq!(
            claimed_preview(&claimed).unwrap().bvid,
            "BV1GJ411x7h7".to_string()
        );

        assert!(matches!(
            apply_claim(&mut state, 42, "m1", 0),
            Claim::AlreadyExtracted
        ));
        // 失败之后可以再点一次
        state.previews[0].extracted = false;
        assert!(matches!(apply_claim(&mut state, 42, "m1", 0), Claim::Ready(_)));
    }

    fn claimed_preview(claim: &Claim) -> Option<&Preview> {
        match claim {
            Claim::Ready(preview) => Some(preview),
            _ => None,
        }
    }

    #[test]
    fn another_group_or_another_message_is_not_ours() {
        let mut state = State {
            previews: vec![preview("m1")],
        };
        assert!(matches!(apply_claim(&mut state, 43, "m1", 0), Claim::Missing));
        assert!(matches!(apply_claim(&mut state, 42, "m2", 0), Claim::Missing));
    }

    #[test]
    fn stale_previews_are_pruned_before_matching() {
        let mut state = State {
            previews: vec![preview("m1")],
        };
        assert!(matches!(
            apply_claim(&mut state, 42, "m1", 100 + 86_400),
            Claim::Missing
        ));
        assert!(state.previews.is_empty());
    }

    #[test]
    fn records_survive_a_round_trip() {
        let state = State {
            previews: vec![preview("m1")],
        };
        let json = serde_json::to_string(&state).unwrap();
        let parsed: State = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.previews.len(), 1);
        assert_eq!(parsed.previews[0].cid, 137649199);
        assert!(!parsed.previews[0].extracted);

        // 旧文件没有 `extracted` 字段时按「没取过」处理。
        let legacy: State = serde_json::from_str(
            r#"{"previews":[{"target_id":42,"message_id":"m1","created_ts":100,"url":"u","bvid":"BV1","cid":1,"page":1,"title":"t","duration":2}]}"#,
        )
        .unwrap();
        assert!(!legacy.previews[0].extracted);
        // `requester` 同理：没有就按「不知道是谁」算。
        assert_eq!(legacy.previews[0].requester, 0);
    }
}
