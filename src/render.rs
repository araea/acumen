//! 卡片渲染工具。help / ctl 使用 web 的 HTML 排版与浏览器截图；
//! canvas、font、kit 保留为原生绘图工具。

// 渲染层是一套「画卡片用的工具箱」，成套提供才好用：`hgrad` 有 `vgrad`、
// 宋黑两族各有常规与加粗、Block 里留着 `Gap`。某一件暂时没人调用不代表它多余，
// 下一张卡片就可能要它，所以这里不按调用数裁剪 API。
#![allow(dead_code, unused_imports)]

pub mod canvas;
pub mod font;
pub mod kit;
pub mod web;
pub mod worker;

pub use canvas::{Canvas, Ink};
pub use font::Fonts;

use chrono::{DateTime, FixedOffset, Utc};

/// 北京时间。
///
/// 卡片上的时刻一律按北京时间印，不按机器时区：这台机器跟着群友在同一个时区，
/// 但时区配置本身可能被改（旅行、误设、容器里没有 /etc/localtime），而『卡片上的
/// 时间』要和群聊里的对话对得上——对不上就是错的，没有第二种解释。
///
/// 从前有几张卡各写了一份同样的实现，手册与控制两份又都没有，
/// 于是几张卡里只有一部分带时刻、还都各算各的。现在收在一处：要改时间口径就改这里。
pub(crate) fn beijing() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).expect("UTC+8 是合法时区偏移")
}

/// 此刻的北京时间。
pub(crate) fn beijing_now() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&beijing())
}
