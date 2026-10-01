//! 内嵌的前端资源。
//!
//! 全部编译进二进制，页面不加载任何外部资源：一个跑在别人机器上的机器人，
//! 界面不该依赖任何一台服务器的可达性。
//!
//! 样式分两层，顺序不能换（见 `stylesheet()`）：
//!
//! - `res/console/tokens.css`：设计令牌，界面唯一的取值来源。配色段由
//!   `scripts/make-tokens.py` 从种子色经 HCT 生成，其余是字阶、形状、动效、
//!   间距与外壳几何；
//! - `res/console/app.css`：组件与版式，只写选择器与 `var()`。
//!
//! 界面与卡片图（`res/cards/m3e.css`）各有各的令牌：卡片是发进群里的静态位图，
//! 界面是要跟随系统明暗、对比度与动态偏好的网页，两者的约束不同。
//!
//! 图标六份产物同出 `scripts/make-icon.py` 一处几何：矢量 `any`、192/512 位图、
//! 铺满的 maskable、铺满的 Apple 180（Safari 不认 SVG 与透明底，且自己裁圆角），
//! 以及透明底纯白的 monochrome。

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

/// 应用的中文名。命令行、网页、桌面图标上的显示名都取这一处。
pub(crate) const APP_NAME: &str = "知微";

const INDEX: &[u8] = include_bytes!("../../../res/console/index.html");
const TOKENS_CSS: &[u8] = include_bytes!("../../../res/console/tokens.css");
const APP_CSS: &[u8] = include_bytes!("../../../res/console/app.css");
const APP_JS: &[u8] = include_bytes!("../../../res/console/app.js");
const ICON: &[u8] = include_bytes!("../../../res/console/icon.svg");
const ICON_192: &[u8] = include_bytes!("../../../res/console/icon-192.png");
const ICON_512: &[u8] = include_bytes!("../../../res/console/icon-512.png");
const ICON_MASKABLE: &[u8] = include_bytes!("../../../res/console/icon-maskable-512.png");
const ICON_MONO_SVG: &[u8] = include_bytes!("../../../res/console/icon-monochrome.svg");
const APPLE_ICON: &[u8] = include_bytes!("../../../res/console/apple-touch-icon.png");
const MANIFEST: &[u8] = include_bytes!("../../../res/console/manifest.webmanifest");

/// 两层拼好之后的那一份。只拼一次，之后每次请求都拿同一片内存。
fn stylesheet() -> &'static [u8] {
    static SHEET: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    SHEET.get_or_init(|| {
        let mut sheet = TOKENS_CSS.to_vec();
        sheet.push(b'\n');
        sheet.extend_from_slice(APP_CSS);
        sheet
    })
}

pub(super) async fn index(headers: HeaderMap) -> Response {
    asset(&headers, INDEX, "text/html; charset=utf-8")
}

pub(super) async fn css(headers: HeaderMap) -> Response {
    asset(&headers, stylesheet(), "text/css; charset=utf-8")
}

pub(super) async fn js(headers: HeaderMap) -> Response {
    asset(&headers, APP_JS, "text/javascript; charset=utf-8")
}

pub(super) async fn icon(headers: HeaderMap) -> Response {
    asset(&headers, ICON, "image/svg+xml")
}

pub(super) async fn icon_192(headers: HeaderMap) -> Response {
    asset(&headers, ICON_192, "image/png")
}

pub(super) async fn icon_512(headers: HeaderMap) -> Response {
    asset(&headers, ICON_512, "image/png")
}

pub(super) async fn icon_maskable(headers: HeaderMap) -> Response {
    asset(&headers, ICON_MASKABLE, "image/png")
}

pub(super) async fn icon_monochrome(headers: HeaderMap) -> Response {
    asset(&headers, ICON_MONO_SVG, "image/svg+xml")
}

pub(super) async fn apple_icon(headers: HeaderMap) -> Response {
    asset(&headers, APPLE_ICON, "image/png")
}

pub(super) async fn favicon(headers: HeaderMap) -> Response {
    asset(&headers, ICON, "image/svg+xml")
}

pub(super) async fn manifest(headers: HeaderMap) -> Response {
    asset(&headers, MANIFEST, "application/manifest+json")
}

/// 一律 `no-cache`：界面跟二进制一起走，升级之后不该还看到上一版的页面。
///
/// 但「每次都问一遍」不等于「每次都重发一遍」——带上内容算出来的 ETag，
/// 接得上 304 就不用再下一遍。手机上打开界面那一刻的等待，跟机器人抢的是
/// 同一台机器的 CPU。
fn asset(request: &HeaderMap, body: &'static [u8], mime: &'static str) -> Response {
    let tag = etag(body);
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(
        header::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    if let Ok(value) = HeaderValue::from_str(&tag) {
        headers.insert(header::ETAG, value);
    }
    if fresh(request, &tag) {
        return (StatusCode::NOT_MODIFIED, headers, ()).into_response();
    }
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    (StatusCode::OK, headers, body).into_response()
}

/// 请求里那个 `If-None-Match` 是否就是这一份。带多个标签的（`a, b`）逐个比，
/// `*` 表示「随便哪一份都行」，也认。
fn fresh(request: &HeaderMap, tag: &str) -> bool {
    request
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .map(str::trim)
                .any(|one| one == tag || one == "*")
        })
}

/// 内容的 FNV-1a 指纹，写成带引号的形式（就是 ETag 要的形状）。
fn etag(body: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in body {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("\"{hash:016x}\"")
}
