//! 卡片渲染工具。help / ctl 使用 web 的 HTML 排版与浏览器截图；
//! canvas 与 font 是原生绘图工具（二维码标注用）。

// 画布与字体是成套提供的工具：`hgrad` 有 `vgrad`、宋黑两族各有常规与加粗。
// 某一件暂时没人调用不代表它多余，所以这里不按调用数裁剪 API。
#![allow(dead_code, unused_imports)]

pub mod canvas;
pub mod font;
pub mod markdown;
pub mod tokens;
pub mod web;
pub mod worker;

pub use canvas::{Canvas, Ink};
pub use font::Fonts;
