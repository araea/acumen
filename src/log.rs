use chrono::Local;
use std::sync::OnceLock;

pub enum Level {
    Info,
    Warn,
    Error,
    Debug,
}

/// 已经格式化好的一行日志，供控制台面板复用。
///
/// `at` 是 `HH:MM:SS`，与终端上看到的是同一个时刻；`level` 是终端上那个四字母
/// 标签（`INFO` / `WARN` / …）；`text` 不带 ANSI 转义，网页里不需要那一层。
pub struct Line {
    pub at: String,
    pub level: &'static str,
    pub target: String,
    pub text: String,
}

/// 日志落点。控制台插件启动时装上它，没装时这个函数是空转。
///
/// 收在这里而不是让 `log.rs` 去认控制台，是为了让日志模块保持零依赖：
/// 装的是一个闭包，落点自己决定要不要缓冲、要不要丢弃。
static SINK: OnceLock<Box<dyn Fn(Line) + Send + Sync>> = OnceLock::new();

/// 装上落点。重复装会被忽略（只有第一次生效），返回是否装上。
pub fn hook(sink: impl Fn(Line) + Send + Sync + 'static) -> bool {
    SINK.set(Box::new(sink)).is_ok()
}

/// 统一日志输出函数
/// 格式: [Time] [LEVEL] [Target] Message
pub fn print(level: Level, target: &str, args: std::fmt::Arguments) {
    let now = Local::now().format("%H:%M:%S");

    // ANSI 颜色代码
    let gray = "\x1b[90m";
    let reset = "\x1b[0m";
    let cyan = "\x1b[36m";

    // Level 颜色与标签
    let (color, level_str) = match level {
        Level::Info => ("\x1b[32m", "INFO"),  // Green
        Level::Warn => ("\x1b[33m", "WARN"),  // Yellow
        Level::Error => ("\x1b[31m", "ERRO"), // Red
        Level::Debug => ("\x1b[34m", "DEBG"), // Blue
    };

    println!(
        "{}[{}] {}[{}] {}{}{}{} {}",
        gray,
        now,
        color,
        level_str,
        reset,
        cyan,
        format_args!("[{}]", target),
        reset,
        args
    );

    // 终端那一行已经打完了，落点里再走一遍格式化：`format_args!` 只能消费一次，
    // 而控制台要的是同一句话的另一份（无颜色、带级别与 target 的字段）。
    if let Some(sink) = SINK.get() {
        sink(Line {
            at: now.to_string(),
            level: level_str,
            target: target.to_string(),
            text: format!("{args}"),
        });
    }
}

#[macro_export]
macro_rules! info {
    (target: $target:expr, $($arg:tt)+) => (
        $crate::log::print($crate::log::Level::Info, $target, format_args!($($arg)+))
    );
    ($($arg:tt)+) => (
        $crate::log::print($crate::log::Level::Info, "System", format_args!($($arg)+))
    );
}

#[macro_export]
macro_rules! warn {
    (target: $target:expr, $($arg:tt)+) => (
        $crate::log::print($crate::log::Level::Warn, $target, format_args!($($arg)+))
    );
    ($($arg:tt)+) => (
        $crate::log::print($crate::log::Level::Warn, "System", format_args!($($arg)+))
    );
}

#[macro_export]
macro_rules! error {
    (target: $target:expr, $($arg:tt)+) => (
        $crate::log::print($crate::log::Level::Error, $target, format_args!($($arg)+))
    );
    ($($arg:tt)+) => (
        $crate::log::print($crate::log::Level::Error, "System", format_args!($($arg)+))
    );
}

#[macro_export]
macro_rules! debug {
    (target: $target:expr, $($arg:tt)+) => (
        $crate::log::print($crate::log::Level::Debug, $target, format_args!($($arg)+))
    );
    ($($arg:tt)+) => (
        $crate::log::print($crate::log::Level::Debug, "System", format_args!($($arg)+))
    );
}
