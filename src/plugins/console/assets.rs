//! 内嵌的前端资源。
//!
//! 三个文件一起编译进二进制，页面上不加载任何外部资源——这条与卡片出图同源
//! （见 `res/cards/m3e.css` 的文件头）：一个跑在别人手机上的机器人，界面不该
//! 依赖任何一台服务器的可达性。
//!
//! 样式分两层，顺序不能换：`res/cards/m3e.css` 是系统层（令牌与静态基元，
//! 六张卡片图共用），`res/console/app.css` 是控制台的版式层与交互基元，只写
//! 「摆在哪儿」与「按下去会怎样」，色值字号一律 `var()` 取令牌。

use axum::response::{IntoResponse, Response};
use axum::http::{StatusCode, header};

/// 应用的中文名。命令行、网页、Android 包里的显示名都取这一处。
pub(crate) const APP_NAME: &str = "知言";

const INDEX: &str = include_str!("../../../res/console/index.html");
const APP_CSS: &str = include_str!("../../../res/console/app.css");
const APP_JS: &str = include_str!("../../../res/console/app.js");
const ICON: &str = include_str!("../../../res/console/icon.svg");
const MANIFEST: &str = include_str!("../../../res/console/manifest.webmanifest");

/// 系统层 + 版式层拼好之后的那一份。只拼一次，之后每次请求都拿同一个字符串。
fn stylesheet() -> &'static str {
    static SHEET: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SHEET.get_or_init(|| {
        format!(
            "{}\n{}",
            crate::render::web::DESIGN_SYSTEM,
            APP_CSS
        )
    })
}

pub(super) async fn index() -> Response {
    text(INDEX, "text/html; charset=utf-8")
}

pub(super) async fn css() -> Response {
    text(stylesheet(), "text/css; charset=utf-8")
}

pub(super) async fn js() -> Response {
    text(APP_JS, "text/javascript; charset=utf-8")
}

pub(super) async fn icon() -> Response {
    text(ICON, "image/svg+xml")
}

pub(super) async fn favicon() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        ICON,
    )
        .into_response()
}

pub(super) async fn manifest() -> Response {
    text(MANIFEST, "application/manifest+json")
}

/// 一律 `no-cache`：界面跟二进制一起走，升级之后不该还看到上一版的页面。
fn text(body: &'static str, mime: &'static str) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 页面里不许引用外部资源：没有 `src="http`、`href="http`、`@import`、
    /// `url(http`。内联 SVG 的 `xmlns` 不算——那不是去网络上取东西。
    ///
    /// 与 `render::web` 那条同源：控制台要能在完全离线的设备上打开。
    #[test]
    fn the_page_loads_nothing_from_the_network() {
        for (name, body) in [
            ("index.html", INDEX),
            ("app.js", APP_JS),
            ("app.css", APP_CSS),
        ] {
            for needle in ["src=\"http", "href=\"http", "@import", "url(http"] {
                assert!(
                    !body.contains(needle),
                    "{name} 里出现了外部引用 {needle}；界面不该依赖任何一台服务器的可达性"
                );
            }
        }
    }

    /// 版式层不许写死颜色、字号、圆角、阴影，一律取系统层的令牌。
    ///
    /// 这条与 `docs/GUIDELINES.md` 第五节的分工是同一条：系统层说「是什么」，
    /// 版式层只说「摆在哪儿」。查法很土但不漏——先把注释整段拿掉（本文件开头的
    /// 说明里就写着 `#hex`、`font-size` 这些词，不剥掉会自己把自己判死），
    /// 再看十六进制色、`rgb(`/`hsl(` 两种函数写法，以及那三个属性后面跟着数字。
    #[test]
    fn the_layout_layer_borrows_every_value_from_the_system_layer() {
        const BANNED: &[&str] = &["font-size", "border-radius", "box-shadow"];
        let mut suspicious = Vec::new();
        for (index, line) in strip_comments(APP_CSS).lines().enumerate() {
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

    /// 图标与清单里的名字取自同一处。
    #[test]
    fn the_name_is_written_once() {
        assert!(MANIFEST.contains(APP_NAME), "manifest 里的名字要对得上");
        assert!(INDEX.contains(APP_NAME), "页面标题要对得上");
    }
}
