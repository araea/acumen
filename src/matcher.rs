use crate::event::Event;
use simd_json::derived::ValueObjectAccessAsScalar;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::oneshot;

/// 事件匹配器，用于处理插件的交互式消息等待
///
/// 优化要点：
///   - 使用同步 `Mutex` 替代 `tokio::sync::Mutex`：持锁期间不存在 await 点，
///     避免每事件都在异步运行时上排队。
///   - 通过 `AtomicUsize` 维护等待者数量，dispatch 在无等待者时无锁直通。
///   - 等待者超时后会主动清理自身条目，避免 Vec 因短时高并发等待累积陈旧条目。
pub struct Matcher {
    waiters: Mutex<Vec<Waiter>>,
    waiter_count: AtomicUsize,
}

struct Waiter {
    id: u64,
    // 消息匹配条件
    group_id: Option<i64>,
    user_id: Option<i64>,
    sender: oneshot::Sender<Event>,
}

struct WaitRegistration<'a>(&'a Matcher, u64);

impl Drop for WaitRegistration<'_> {
    fn drop(&mut self) {
        self.0.drop_waiter(self.1);
    }
}

/// 单调递增 id，用于超时后定位并移除自身条目
fn next_waiter_id() -> u64 {
    static WAITER_ID: AtomicUsize = AtomicUsize::new(1);
    WAITER_ID.fetch_add(1, Ordering::Relaxed) as u64
}

impl Matcher {
    pub fn new() -> Self {
        Self {
            waiters: Mutex::new(Vec::new()),
            waiter_count: AtomicUsize::new(0),
        }
    }

    /// 注册一个消息等待者 (群号/用户)
    pub async fn wait(
        &self,
        group_id: Option<i64>,
        user_id: Option<i64>,
        timeout_duration: Duration,
    ) -> Option<Event> {
        self.wait_internal(group_id, user_id, timeout_duration)
            .await
    }

    fn drop_waiter(&self, id: u64) {
        let mut guard = self.waiters.lock().unwrap();
        if let Some(idx) = guard.iter().position(|w| w.id == id) {
            guard.swap_remove(idx);
            self.waiter_count.store(guard.len(), Ordering::Release);
        }
    }

    async fn wait_internal(
        &self,
        group_id: Option<i64>,
        user_id: Option<i64>,
        timeout_duration: Duration,
    ) -> Option<Event> {
        let (tx, rx) = oneshot::channel();
        let id = next_waiter_id();

        {
            let mut guard = self.waiters.lock().unwrap();
            guard.push(Waiter {
                id,
                group_id,
                user_id,
                sender: tx,
            });
            self.waiter_count.store(guard.len(), Ordering::Release);
        }

        let _registration = WaitRegistration(self, id);
        tokio::time::timeout(timeout_duration, rx).await.ok()?.ok()
    }

    /// 尝试分发事件给等待者。如果事件被消费（匹配成功），返回 None；否则返回原事件。
    pub fn dispatch(&self, mut event: Event) -> Option<Event> {
        // 快速路径：当前没有等待者，直接放行
        if self.waiter_count.load(Ordering::Acquire) == 0 {
            return Some(event);
        }

        let g_id = event
            .get_i64("group_id")
            .or_else(|| event.get_u64("group_id").map(|v| v as i64));
        let u_id = event
            .get_i64("user_id")
            .or_else(|| event.get_u64("user_id").map(|v| v as i64));
        // 只有消息事件参与交互等待
        if g_id.is_none() && u_id.is_none() {
            return Some(event);
        }

        loop {
            let waiter_opt = {
                let mut guard = self.waiters.lock().unwrap();

                // 寻找匹配者
                let index = guard.iter().position(|w| {
                    let match_group = w.group_id.is_none() || w.group_id == g_id;
                    let match_user = w.user_id.is_none() || w.user_id == u_id;
                    match_group && match_user
                });

                if let Some(idx) = index {
                    let waiter = guard.swap_remove(idx);
                    self.waiter_count.store(guard.len(), Ordering::Release);
                    Some(waiter)
                } else {
                    None
                }
            };

            if let Some(waiter) = waiter_opt {
                // 接收者可能恰好被取消：发送失败时保留事件，继续寻找活着的等待者。
                match waiter.sender.send(event) {
                    Ok(()) => return None,
                    Err(undelivered) => event = undelivered,
                }
            } else {
                return Some(event); // 无匹配，返还事件
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_wait_removes_registration() {
        let matcher = Matcher::new();
        let mut waiting = Box::pin(matcher.wait(Some(7), Some(9), Duration::from_secs(60)));
        assert!(futures_util::poll!(&mut waiting).is_pending());
        assert_eq!(matcher.waiter_count.load(Ordering::Acquire), 1);
        drop(waiting);
        assert_eq!(matcher.waiter_count.load(Ordering::Acquire), 0);
        let event = simd_json::json!({"group_id":7,"user_id":9});
        assert!(matcher.dispatch(event).is_some());
    }

    #[tokio::test]
    async fn closed_receiver_does_not_swallow_a_message() {
        let matcher = Matcher::new();
        let (tx, rx) = oneshot::channel();
        drop(rx);
        matcher.waiters.lock().unwrap().push(Waiter {
            id: 0,
            group_id: Some(7),
            user_id: None,
            sender: tx,
        });
        matcher.waiter_count.store(1, Ordering::Release);
        let mut live = Box::pin(matcher.wait(Some(7), None, Duration::from_secs(1)));
        assert!(futures_util::poll!(&mut live).is_pending());
        assert!(
            matcher
                .dispatch(simd_json::json!({"group_id":7,"user_id":9}))
                .is_none()
        );
        assert!(live.await.is_some());
        assert_eq!(matcher.waiter_count.load(Ordering::Acquire), 0);
    }
}
