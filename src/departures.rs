//! 经我们之手移出、已经交代过的人。
//!
//! 入群欢迎在有人被移出时会说一句「某某已被移出本群」。可移出这件事要是机器人自己做的——
//! 入群验证超时、清理潜水——动手的那一处已经说过了；清理潜水一次移出几十个人，再跟着
//! 几十句送别就是刷屏。所以动手之前在这里记一笔，退群事件回来时看一眼：记过的就不再说。
//!
//! 只在内存里，记两分钟：实现端的退群事件通常一两秒就到，进程重启后旧账也没有意义了。

use std::sync::Mutex;
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_mins(2);

fn book() -> std::sync::MutexGuard<'static, Vec<(String, String, Instant)>> {
    static BOOK: Mutex<Vec<(String, String, Instant)>> = Mutex::new(Vec::new());
    let mut book = BOOK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    book.retain(|(_, _, at)| at.elapsed() < TTL);
    book
}

/// 要把这个人移出这个群了，退群事件回来时别再报一遍。
pub fn expect(group: &str, user: &str) {
    book().push((group.to_string(), user.to_string(), Instant::now()));
}

/// 这个人是不是刚被我们移出、已经交代过。
pub fn expected(group: &str, user: &str) -> bool {
    book().iter().any(|(g, u, _)| g == group && u == user)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_who_we_removed() {
        expect("100", "7");
        assert!(expected("100", "7"));
        assert!(!expected("100", "8"));
        assert!(!expected("101", "7"));
    }
}
