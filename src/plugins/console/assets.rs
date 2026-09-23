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

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    fn text(bytes: &[u8]) -> String {
        strip_comments(&String::from_utf8_lossy(bytes))
    }

    /// 把 `/* … */` 整段删掉。注释里写着 `#hex`、`font-size` 这些词，不剥掉会误报。
    fn strip_comments(sheet: &str) -> String {
        let mut out = String::with_capacity(sheet.len());
        let mut rest = sheet;
        while let Some(start) = rest.find("/*") {
            out.push_str(&rest[..start]);
            match rest[start..].find("*/") {
                Some(end) => {
                    out.push('\n');
                    rest = &rest[start + end + 2..];
                }
                None => return out,
            }
        }
        out.push_str(rest);
        out
    }

    /// 样式表里的每一条声明（属性名, 值）。遇到 `{` 前面的是选择器或 @ 规则，
    /// 遇到 `;` 或 `}` 前面的是声明——多行的值（过渡列表、阴影列表）也完整取到。
    fn declarations(sheet: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut buffer = String::new();
        for ch in sheet.chars() {
            match ch {
                '{' => buffer.clear(),
                ';' | '}' => {
                    if let Some((name, value)) = buffer.split_once(':') {
                        let name = name.trim();
                        if !name.is_empty() && !name.contains(char::is_whitespace) {
                            out.push((
                                name.to_string(),
                                value.split_whitespace().collect::<Vec<_>>().join(" "),
                            ));
                        }
                    }
                    buffer.clear();
                }
                _ => buffer.push(ch),
            }
        }
        out
    }

    fn colour_literal(value: &str) -> bool {
        let hex = Regex::new(r"#[0-9a-fA-F]{3,8}\b").unwrap();
        hex.is_match(value)
            || ["rgb(", "rgba(", "hsl(", "hsla(", "oklch(", "lab("]
                .iter()
                .any(|f| value.contains(f))
    }

    fn defined(sheet: &str) -> Vec<String> {
        declarations(sheet)
            .into_iter()
            .filter(|(name, _)| name.starts_with("--"))
            .map(|(name, _)| name)
            .collect()
    }

    fn used(sheet: &str) -> Vec<String> {
        let re = Regex::new(r"var\(\s*(--[a-z0-9_-]+)").unwrap();
        re.captures_iter(sheet).map(|c| c[1].to_string()).collect()
    }

    /// 页面里不许引用外部资源。界面要能在完全离线的设备上打开。
    #[test]
    fn the_page_loads_nothing_from_the_network() {
        for (name, body) in [
            ("index.html", INDEX),
            ("app.js", APP_JS),
            ("app.css", stylesheet()),
        ] {
            let text = String::from_utf8_lossy(body);
            for needle in [
                "src=\"http",
                "href=\"http",
                "@import",
                "url(http",
                "url(\"http",
                "fetch(\"http",
            ] {
                assert!(!text.contains(needle), "{name} 里出现了外部引用 {needle}");
            }
        }
    }

    /// 组件层不写视觉字面量：颜色一律来自令牌；字号、圆角、阴影必须取 `var()`。
    #[test]
    fn the_component_layer_takes_every_value_from_tokens() {
        let mut suspicious = Vec::new();
        for (name, value) in declarations(&text(APP_CSS)) {
            if colour_literal(&value) {
                suspicious.push(format!("{name}: {value}（颜色字面量）"));
            }
            let guarded = ["font-size", "border-radius", "box-shadow"].contains(&name.as_str())
                || name.starts_with("border-") && name.ends_with("-radius");
            let keyword = ["none", "inherit", "0", "initial"].contains(&value.as_str());
            if guarded && !keyword && !value.contains("var(--") {
                suspicious.push(format!("{name}: {value}（应取令牌）"));
            }
        }
        assert!(suspicious.is_empty(), "{}", suspicious.join("\n"));
    }

    /// 组件层不新造令牌。唯一的例外是 `--_` 开头的组件私有变量，
    /// 而它的值只能指向令牌（或 `transparent`）——换个名字写字面量同样不行。
    #[test]
    fn the_component_layer_adds_no_token() {
        let mut extra = Vec::new();
        for (name, value) in declarations(&text(APP_CSS)) {
            if !name.starts_with("--") {
                continue;
            }
            if !name.starts_with("--_") || !(value.contains("var(--") || value == "transparent") {
                extra.push(format!("{name}: {value}"));
            }
        }
        assert!(
            extra.is_empty(),
            "组件层自己定义了令牌，挪到 tokens.css：{}",
            extra.join("、")
        );
    }

    /// 用到的每一个令牌都得有定义：拼错一个字母不会报错，只会静默不生效。
    #[test]
    fn every_token_in_use_is_defined() {
        let tokens = text(TOKENS_CSS);
        let app = text(APP_CSS);
        let known: Vec<String> = defined(&tokens).into_iter().chain(defined(&app)).collect();
        let mut missing: Vec<String> = used(&tokens)
            .into_iter()
            .chain(used(&app))
            .filter(|name| !known.contains(name))
            .collect();
        missing.sort();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "这些令牌没有定义：{}",
            missing.join("、")
        );
    }

    /// WebUI 不得再次引入另一套视觉体系或非 M3E 的角形。
    #[test]
    fn the_webui_uses_only_material_expressive_visuals() {
        for (name, source) in [("tokens.css", TOKENS_CSS), ("app.css", APP_CSS), ("app.js", APP_JS)] {
            let source = text(source).to_lowercase();
            for forbidden in ["carbon", "miuix", "squircle", "corner-shape"] {
                assert!(!source.contains(forbidden), "{name} 包含混合视觉体系：{forbidden}");
            }
        }
    }

    /// 令牌层只定义自定义属性（`color-scheme` 除外，它是配色段的一部分）。
    #[test]
    fn the_token_layer_only_declares_custom_properties() {
        for (name, value) in declarations(&text(TOKENS_CSS)) {
            assert!(
                name.starts_with("--") || name == "color-scheme",
                "tokens.css 里混进了普通声明：{name}: {value}"
            );
        }
    }

    /// 颜色字面量只能出现在生成段里：手写的一处色值就是脱离了种子与算法。
    #[test]
    fn colours_only_come_from_the_generated_scheme() {
        let source = String::from_utf8_lossy(TOKENS_CSS);
        let start = source
            .find("@generated by scripts/make-tokens.py")
            .expect("缺少生成段起点");
        let end = source.find("@end generated").expect("缺少生成段终点");
        assert!(start < end);
        let outside = format!("{}{}", &source[..start], &source[end..]);
        for (name, value) in declarations(&strip_comments(&outside)) {
            assert!(
                !colour_literal(&value),
                "生成段之外写了颜色：{name}: {value}"
            );
        }
        let generated = &source[start..end];
        for role in [
            "primary",
            "on-primary",
            "primary-container",
            "surface",
            "on-surface",
            "on-surface-variant",
            "outline",
            "error",
            "success",
            "warning",
            "inverse-surface",
            "scrim",
        ] {
            assert!(
                generated
                    .matches(&format!("--md-sys-color-{role}:"))
                    .count()
                    == 4,
                "--md-sys-color-{role} 要在浅、深、浅高对比、深高对比四档各有一份"
            );
        }
    }

    /// 状态栏颜色（theme-color）必须等于浅深两档的 surface，否则装到桌面后顶栏与状态栏有接缝。
    #[test]
    fn theme_colour_matches_the_surface() {
        let tokens = String::from_utf8_lossy(TOKENS_CSS);
        let index = String::from_utf8_lossy(INDEX);
        let surfaces: Vec<&str> = Regex::new(r"--md-sys-color-surface: (#[0-9a-f]{6});")
            .unwrap()
            .captures_iter(&tokens)
            .map(|c| c.get(1).unwrap().as_str())
            .collect();
        let (light, dark) = (surfaces[0], surfaces[1]);
        assert!(index.contains(&format!(
            r#"content="{light}" media="(prefers-color-scheme: light)""#
        )));
        assert!(index.contains(&format!(
            r#"content="{dark}" media="(prefers-color-scheme: dark)""#
        )));
        let manifest: serde_json::Value = serde_json::from_slice(MANIFEST).unwrap();
        assert_eq!(manifest["theme_color"], light);
    }

    /// 名字只写一处。
    #[test]
    fn the_name_is_written_once() {
        assert!(String::from_utf8_lossy(MANIFEST).contains(APP_NAME));
        assert!(String::from_utf8_lossy(INDEX).contains(&format!("<title>{APP_NAME}</title>")));
        assert!(String::from_utf8_lossy(ICON).contains(&format!("<title>{APP_NAME}</title>")));
    }

    fn png_colour_type(body: &[u8]) -> u8 {
        assert_eq!(&body[..8], b"\x89PNG\r\n\x1a\n", "得是一张 PNG");
        assert_eq!(&body[12..16], b"IHDR");
        body[25]
    }

    fn png_size(body: &[u8]) -> (u32, u32) {
        let read = |at: usize| u32::from_be_bytes(body[at..at + 4].try_into().unwrap());
        (read(16), read(20))
    }

    /// 清单承诺的每一张图标都在，尺寸与格式对得上；铺满的两份不许有透明通道
    /// （系统裁形之后透明处会变黑或变白），any 那份必须有（圆角外是透明的）。
    #[test]
    fn every_icon_is_what_it_claims_to_be() {
        let manifest: serde_json::Value = serde_json::from_slice(MANIFEST).unwrap();
        let listed: Vec<&str> = manifest["icons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|icon| icon["src"].as_str().unwrap())
            .collect();
        for name in [
            "/icon.svg",
            "/icon-192.png",
            "/icon-512.png",
            "/icon-maskable-512.png",
            "/icon-monochrome.svg",
        ] {
            assert!(listed.contains(&name), "清单里没有 {name}");
        }
        assert!(ICON.starts_with(b"<svg") && ICON_MONO_SVG.starts_with(b"<svg"));
        for (body, size, alpha) in [
            (ICON_192, 192, true),
            (ICON_512, 512, true),
            (ICON_MASKABLE, 512, false),
            (APPLE_ICON, 180, false),
        ] {
            assert_eq!(png_size(body), (size, size));
            let colour = png_colour_type(body);
            assert_eq!(
                colour == 6,
                alpha,
                "{size}px 图标的透明通道不对（颜色类型 {colour}）"
            );
        }
    }

    /// 单色层透明底、纯白前景，由系统去染色。
    #[test]
    fn the_monochrome_icon_is_white_on_nothing() {
        let mono = String::from_utf8_lossy(ICON_MONO_SVG);
        assert!(mono.contains(r##"fill="#ffffff""##));
        assert!(!mono.contains("<linearGradient") && !mono.contains("<rect"));
    }

    /// 组件层的开关、页签、单选组都要带着语义出场，而不是只有样子。
    #[test]
    fn interactive_patterns_carry_their_roles() {
        let js = String::from_utf8_lossy(APP_JS);
        for needle in [
            r#"role="switch""#,
            r#"role="tab""#,
            r#"role="tabpanel""#,
            r#"type="radio""#,
            "aria-current",
            "aria-pressed",
        ] {
            assert!(js.contains(needle), "app.js 里缺少 {needle}");
        }
        // 模板插值默认转义：不许绕过 h`` 直接拼 innerHTML。
        let direct = Regex::new(r"innerHTML\s*=\s*[`'\x22]").unwrap();
        assert!(!direct.is_match(&js), "有地方绕过 h`` 直接拼 innerHTML");
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
        assert!(!ask(None));
        assert!(ask(Some(&tag)));
        assert!(ask(Some(&format!("\"x\", {tag}"))));
        assert!(ask(Some("*")));
        assert!(!ask(Some("\"0000\"")));
    }
}
