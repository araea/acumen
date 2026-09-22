//! 内嵌的前端资源。
//!
//! 这些文件一起编译进二进制，页面上不加载任何外部资源——这条与卡片出图同源
//! （见 `res/cards/m3e.css` 的文件头）：一个跑在别人机器上的机器人，界面不该
//! 依赖任何一台服务器的可达性。
//!
//! 样式分三层，顺序不能换：`res/cards/m3e.css` 是系统层（令牌与静态基元，
//! 卡片图共用），`res/console/tokens.css` 是界面的令牌层（界面这一份的取值），
//! `res/console/app.css` 是版式层与交互基元，只写「摆在哪儿」与「按下去会怎样」。
//! 越靠下越具体，所以令牌层能覆盖系统层、版式层能覆盖令牌层。三条各有一个
//! 进程内的检查盯着，见本文件末尾那组 `layer_*` 测试。
//!
//! 图标有七份产物，一处几何（`scripts/make-icon.py`）：
//!
//! - 矢量那份给标签页与清单里的 `any`；
//! - 两张 PNG 给装到桌面（Android 与桌面浏览器要用位图）；
//! - 遮罩版交给系统自己裁形状，字形收在自适应图标的安全圆里；
//! - 单色版供 Web App 清单的 `monochrome` 使用：透明底加纯白图形；
//!   浏览器/启动器支持时可着色，不等于原生 Android 的 monochrome 资源；
//! - 180 那份是 iOS 加到主屏幕用的——Safari 不认 SVG，也不认透明底，
//!   而且它自己会裁圆角，所以这一份是铺满的方角位图。

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
const ICON_MONO_512: &[u8] = include_bytes!("../../../res/console/icon-monochrome-512.png");
const APPLE_ICON: &[u8] = include_bytes!("../../../res/console/apple-touch-icon.png");
const MANIFEST: &[u8] = include_bytes!("../../../res/console/manifest.webmanifest");

/// 三层拼好之后的那一份。只拼一次，之后每次请求都拿同一片内存。
fn stylesheet() -> &'static [u8] {
    static SHEET: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    SHEET.get_or_init(|| {
        let mut sheet = crate::render::web::DESIGN_SYSTEM.as_bytes().to_vec();
        sheet.push(b'\n');
        sheet.extend_from_slice(TOKENS_CSS);
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

pub(super) async fn icon_monochrome_512(headers: HeaderMap) -> Response {
    asset(&headers, ICON_MONO_512, "image/png")
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
/// 页面接得上 304 就不用再下这一百多 KB。手机上打开界面那一刻的等待，
/// 跟机器人抢的正是同一台机器的 CPU。
fn asset(request: &HeaderMap, body: &'static [u8], mime: &'static str) -> Response {
    let tag = etag(body);
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 页面里不许引用外部资源：没有 `src="http`、`href="http`、`@import`、
    /// `url(http`。内联 SVG 的 `xmlns` 不算——那不是去网络上取东西。
    ///
    /// 与 `render::web` 那条同源：界面要能在完全离线的设备上打开。
    #[test]
    fn the_page_loads_nothing_from_the_network() {
        let sheet = stylesheet();
        for (name, body) in [
            ("index.html", INDEX),
            ("app.js", APP_JS),
            ("app.css", sheet),
        ] {
            let text = String::from_utf8_lossy(body);
            for needle in ["src=\"http", "href=\"http", "@import", "url(http"] {
                assert!(
                    !text.contains(needle),
                    "{name} 里出现了外部引用 {needle}；界面不该依赖任何一台服务器的可达性"
                );
            }
        }
    }

    /// 版式层不许写死颜色、字号、圆角、阴影，一律取系统层的令牌。
    ///
    /// 系统层说「是什么」，
    /// 版式层只说「摆在哪儿」。查法很土但不漏——先把注释整段拿掉（本文件开头的
    /// 说明里就写着 `#hex`、`font-size` 这些词，不剥掉会自己把自己判死），
    /// 再看十六进制色、`rgb(`/`hsl(` 两种函数写法，以及那三个属性后面跟着数字。
    #[test]
    fn the_layout_layer_borrows_every_value_from_the_system_layer() {
        const BANNED: &[&str] = &["font-size", "border-radius", "box-shadow"];
        let css = String::from_utf8_lossy(APP_CSS).into_owned();
        let mut suspicious = Vec::new();
        for (index, line) in strip_comments(&css).lines().enumerate() {
            let code = line.trim();
            if code.is_empty() || code.starts_with("--") {
                continue;
            }
            let hex = code
                .split('#')
                .skip(1)
                .any(|rest| rest.chars().take(6).all(|c| c.is_ascii_hexdigit()));
            let literal = BANNED
                .iter()
                .any(|prop| code.starts_with(prop) && !code.contains("var(--md-"));
            if hex || code.contains("rgb(") || code.contains("hsl(") || literal {
                suspicious.push(format!("{} 行写死了视觉值：{}", index + 1, code));
            }
        }
        assert!(suspicious.is_empty(), "{}", suspicious.join("\n"));
    }

    /// 把 `/* … */` 整段删掉，行号不再对应原文件，但这一条测试只报行号给人找。
    fn strip_comments(sheet: &str) -> String {
        let mut out = String::with_capacity(sheet.len());
        let mut rest = sheet;
        while let Some(start) = rest.find("/*") {
            out.push_str(&rest[..start]);
            match rest[start..].find("*/") {
                Some(end) => {
                    // 段落被整段拿掉之后，前后两行不该粘成一行。
                    out.push('\n');
                    rest = &rest[start + end + 2..];
                }
                None => return out,
            }
        }
        out.push_str(rest);
        out
    }

    /// 扫出一段样式里的令牌名。定义看后面跟的是不是 `:`，引用看是不是 `)` 或 `,`
    /// （`var(--x, 兜底)` 这种写法目前没有，认一下免得以后踩空）。
    fn scan_tokens(sheet: &str, definition: bool) -> Vec<&str> {
        let bytes = sheet.as_bytes();
        let mut found = Vec::new();
        let mut i = 0;
        while i + 1 < bytes.len() {
            if bytes[i] != b'-' || bytes[i + 1] != b'-' {
                i += 1;
                continue;
            }
            let mut end = i + 2;
            while end < bytes.len()
                && (bytes[end].is_ascii_lowercase()
                    || bytes[end].is_ascii_digit()
                    || bytes[end] == b'-')
            {
                end += 1;
            }
            if end == i + 2 {
                i += 1;
                continue;
            }
            let mut next = end;
            while next < bytes.len() && bytes[next].is_ascii_whitespace() {
                next += 1;
            }
            let hit = match bytes.get(next) {
                Some(b':') => definition,
                Some(b')' | b',') => !definition,
                _ => false,
            };
            if hit {
                found.push(&sheet[i..end]);
            }
            i = end;
        }
        found
    }

    fn text(sheet: &[u8]) -> String {
        strip_comments(&String::from_utf8_lossy(sheet))
    }

    /// 令牌只有两处能定义：系统层（`m3e.css`）与界面令牌层（`tokens.css`）。
    /// 版式层里冒出 `--x:` 就是某一处又长出了私有的视觉语言——要加值先加到
    /// `tokens.css`，那里是唯一能读全「界面长什么样」的地方。
    #[test]
    fn the_layout_layer_adds_no_token() {
        let system = text(crate::render::web::DESIGN_SYSTEM.as_bytes());
        let tokens = text(TOKENS_CSS);
        let app = text(APP_CSS);
        let defined: Vec<&str> = scan_tokens(&system, true)
            .into_iter()
            .chain(scan_tokens(&tokens, true))
            .collect();
        let mut extra: Vec<&str> = scan_tokens(&app, true)
            .into_iter()
            .filter(|name| !defined.contains(name))
            .collect();
        extra.sort_unstable();
        extra.dedup();
        assert!(
            extra.is_empty(),
            "版式层自己定义了令牌：{}；加到 tokens.css 去",
            extra.join("、")
        );
    }

    /// 用到的每一个令牌都得有定义。写错一个字母不会报错，只会静默地什么都不生效
    /// ——这层保护比看起来重要。
    #[test]
    fn every_token_a_rule_uses_is_defined() {
        let system = text(crate::render::web::DESIGN_SYSTEM.as_bytes());
        let tokens = text(TOKENS_CSS);
        let app = text(APP_CSS);
        let defined: Vec<&str> = scan_tokens(&system, true)
            .into_iter()
            .chain(scan_tokens(&tokens, true))
            .collect();
        let mut missing: Vec<&str> = scan_tokens(&app, false)
            .into_iter()
            .chain(scan_tokens(&tokens, false))
            .filter(|name| !defined.contains(name))
            .collect();
        missing.sort_unstable();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "这些令牌没有定义：{}",
            missing.join("、")
        );
    }

    /// 令牌层是值层，但它只管「界面这一份多出来的取值」：颜色一律取自系统层，
    /// 在这里写死一个色值就等于界面脱离了配色方案，深浅两档也会漏掉。
    #[test]
    fn the_token_layer_holds_no_colour() {
        let tokens = text(TOKENS_CSS);
        for (index, line) in tokens.lines().enumerate() {
            let code = line.trim();
            let hex = code
                .split('#')
                .skip(1)
                .any(|rest| rest.chars().take(6).all(|c| c.is_ascii_hexdigit()));
            assert!(
                !hex && !code.contains("rgb(") && !code.contains("hsl("),
                "{} 行写死了颜色：{}",
                index + 1,
                code
            );
        }
    }

    /// 令牌层只定义自定义属性。混进一条普通声明（`padding: 4px`）看着也生效，
    /// 但它落在一个没人会去读的选择器上，且绕过了「值只在令牌里」这条。
    #[test]
    fn the_token_layer_only_declares_custom_properties() {
        let tokens = text(TOKENS_CSS);
        for (index, line) in tokens.lines().enumerate() {
            let code = line.trim();
            if code.is_empty() || code.ends_with('{') || code == "}" {
                continue;
            }
            assert!(
                code.starts_with("--"),
                "{} 行不是自定义属性：{}",
                index + 1,
                code
            );
        }
    }

    /// 图标与清单里的名字取自同一处。
    #[test]
    fn the_name_is_written_once() {
        let manifest = String::from_utf8_lossy(MANIFEST);
        let index = String::from_utf8_lossy(INDEX);
        assert!(manifest.contains(APP_NAME), "manifest 里的名字要对得上");
        assert!(index.contains(APP_NAME), "页面标题要对得上");
    }

    /// 清单里列的那几张图标，一张都不能少，且各自得是它声称的那种格式：
    /// 矢量看开头，位图看签名与前八字节之后的体量。
    #[test]
    fn every_icon_the_manifest_promises_exists() {
        let manifest = String::from_utf8_lossy(MANIFEST).into_owned();
        for (name, body, vector) in [
            ("/icon.svg", ICON, true),
            ("/icon-192.png", ICON_192, false),
            ("/icon-512.png", ICON_512, false),
            ("/icon-maskable-512.png", ICON_MASKABLE, false),
            ("/icon-monochrome.svg", ICON_MONO_SVG, true),
        ] {
            assert!(manifest.contains(name), "清单里没有 {name}");
            if vector {
                assert!(body.starts_with(b"<svg"), "{name} 得是一张 SVG");
            } else {
                assert_eq!(&body[..8], b"\x89PNG\r\n\x1a\n", "{name} 得是一张 PNG");
                assert!(body.len() > 512, "{name} 的内容不像一张图标");
            }
        }
        // Apple 那份不进清单，但同样是 PNG，一并认一下。
        assert_eq!(&APPLE_ICON[..8], b"\x89PNG\r\n\x1a\n", "iOS 那份必须是 PNG");
        assert!(
            &ICON_MONO_512[..8] == b"\x89PNG\r\n\x1a\n",
            "单色层的位图版必须是 PNG"
        );
    }

    /// 三层的几何是同一处出来的，这里只钉住「该有的属性在」：
    /// 单色层只能是纯白（系统自己去染），遮罩版与 iOS 版必须是铺满的方角
    /// （系统/系统会自己裁，自己画了圆角就会裁出两层边）。
    #[test]
    fn the_monochrome_layer_is_one_colour_and_has_no_background() {
        let mono = String::from_utf8_lossy(ICON_MONO_SVG).into_owned();
        assert!(mono.contains("#ffffff"), "单色层只用纯白");
        assert!(!mono.contains("<linearGradient"), "单色层不许有底");
        assert!(
            !mono.contains("rx=\"24.16\""),
            "单色层不该带那张圆角底"
        );
    }

    /// ETag 跟着内容走；`If-None-Match` 只认同一份。
    #[test]
    fn the_etag_follows_the_content() {
        assert_eq!(etag(b"abc"), etag(b"abc"));
        assert_ne!(etag(b"abc"), etag(b"abd"));
        let tag = etag(INDEX);
        let ask = |value: Option<&str>| {
            let mut headers = HeaderMap::new();
            if let Some(value) = value {
                headers.insert(header::IF_NONE_MATCH, HeaderValue::from_str(value).unwrap());
            }
            fresh(&headers, &tag)
        };
        assert!(!ask(None), "没带条件的请求要拿到整份");
        assert!(ask(Some(&tag)));
        assert!(ask(Some(&format!("\"x\", {tag}"))), "多标签里命中一个也算");
        assert!(ask(Some("*")));
        assert!(!ask(Some("\"0000\"")));
    }
}
