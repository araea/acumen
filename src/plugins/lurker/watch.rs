//! 机器人哪些时候在收消息。
//!
//! 只看消息记录判断「谁多久没说话」，最怕的是机器人自己离线：号主关了几天机器人，回来一查，
//! 那几天说过话的人全成了「潜水」。这里记下事件连接通着、消息记录也开着的每一段时间
//! （`data/lurker/watch.json`），判定时据此知道记录在哪儿断过。
//!
//! 另外从收到过的消息往回推（插件装上之前、插件关着的那些日子没有打卡）：机器人在很多群里，
//! 平时每十分钟总有人说话，连着三个小时一条都没收到才算断。原始消息有保留期，再早的就没有了。

use super::LOG_TARGET;
use super::judge::DAY;
use crate::event::Context;
use crate::storage::JsonState;
use serde::{Deserialize, Serialize};

/// 两次打卡之间允许的空档：打卡五分钟一次，重启几十秒，二十分钟内接得上就算没断。
pub const TOLERANCE: i64 = 20 * 60;
/// 从消息推断时允许的空档。
const INFERRED_TOLERANCE: i64 = 3 * 3600;
/// 记多久：判定最多看一年。
const KEEP: i64 = 400 * DAY;

#[derive(Serialize, Deserialize, Default, Clone)]
struct Log {
    /// 在线的各段 `[开始, 结束]`（Unix 秒），升序。
    spans: Vec<(i64, i64)>,
}

static LOG: JsonState<Log> = JsonState::new(LOG_TARGET, "lurker", "watch.json", "在线记录");

/// 打一次卡：事件连接一分钟内有过动静、消息记录插件开着，就把在线的这一段延到此刻。
pub async fn tick(ctx: &Context) {
    let now = chrono::Utc::now().timestamp();
    let listening = now - crate::adapters::satori::heard_at() < 90;
    let recording = crate::plugins::enabled(&ctx.config.read().unwrap(), "recorder");
    if !listening || !recording {
        return;
    }
    LOG.with(|log| {
        match log.spans.last_mut() {
            Some(last) if now - last.1 <= TOLERANCE => last.1 = now,
            _ => log.spans.push((now, now)),
        }
        log.spans.retain(|span| span.1 >= now - KEEP);
    })
    .await;
}

/// 记录在的各段（升序、互不重叠）：自己打的卡，并上从收到的消息推出来的。
///
/// 两份取并集：收到过消息就说明那会儿在线；插件关着那阵子没打卡，消息记录照样能补上。
pub async fn spans(ctx: &Context, now: i64) -> Vec<(i64, i64)> {
    let mut spans = LOG.read(|log| log.spans.clone()).await;
    match crate::db::activity::heard_slots(&ctx.db, now - KEEP).await {
        Ok(slots) => spans.extend(from_slots(slots.into_iter().map(|slot| slot * 600))),
        Err(error) => warn!(target: LOG_TARGET, "读不出消息记录的时间分布：{error}"),
    }
    merge(spans)
}

fn merge(mut spans: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    spans.sort_unstable();
    let mut out: Vec<(i64, i64)> = Vec::with_capacity(spans.len());
    for (start, end) in spans {
        match out.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

/// 一串收到过消息的十分钟（起点，升序）连成段：相邻不超过 [`INFERRED_TOLERANCE`] 的并成一段。
fn from_slots(slots: impl Iterator<Item = i64>) -> Vec<(i64, i64)> {
    let mut out: Vec<(i64, i64)> = Vec::new();
    for at in slots {
        match out.last_mut() {
            Some(last) if at - last.1 <= INFERRED_TOLERANCE => last.1 = at + 600,
            _ => out.push((at, at + 600)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_unions_overlapping_spans() {
        assert_eq!(merge(vec![(10, 20), (0, 5), (4, 12), (30, 40)]), vec![(0, 20), (30, 40)]);
    }

    #[test]
    fn slots_join_across_short_quiet_spells() {
        let spans = from_slots([0, 600, 3600, 4 * 3600 + 600, 9 * 3600].into_iter());
        assert_eq!(spans, vec![(0, 4 * 3600 + 1200), (9 * 3600, 9 * 3600 + 600)]);
    }
}
