//! 卡片渲染工具。help / ctl 使用 web 的 HTML 排版与浏览器截图；
//! canvas 与 font 是原生绘图工具（二维码标注用）。

// 令牌（`tokens.rs`，由 scripts/make-tokens.py 生成）与字体的四张字面（宋黑两族各有
// 常规与加粗）是成套提供的：某一件暂时没人用不代表它多余，所以这里不按调用数裁剪。
// 画布（`canvas.rs`）不在此列，只留二维码标注真正用到的几样。
#![allow(dead_code, unused_imports)]

pub mod canvas;
pub mod font;
pub mod markdown;
pub mod tokens;
pub mod web;
pub mod worker;

pub use canvas::{Canvas, Ink};
pub use font::Fonts;
