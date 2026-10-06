//! 时间口径：这个项目里的「几点」一律是北京时间。

use chrono::{DateTime, FixedOffset, Utc};

/// 北京时间（UTC+8）。
///
/// 卡片上印的时刻、推送与计价高峰的作息都按北京时间，不按机器时区：这台机器跟着群友在
/// 同一个时区，但时区配置本身可能被改（旅行、误设、容器里没有 /etc/localtime），而
/// 『几点』要和群聊里的对话对得上——对不上就是错的，没有第二种解释。
///
/// 从前几张卡与几个插件各写了一份同样的实现，于是各算各的。现在收在一处：要改时间口径
/// 就改这里。
pub fn beijing() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).expect("UTC+8 是合法时区偏移")
}

/// 此刻的北京时间。
pub fn beijing_now() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&beijing())
}
