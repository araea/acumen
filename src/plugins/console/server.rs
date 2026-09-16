//! 控制台的 HTTP 面：路由、口令、起停。
//!
//! 只有两张面孔：`/` 那一套静态资源与 `/api/*` 那一套数据。资源不设防——页面本身
//! 不含任何秘密，口令存在浏览器里，提交给 `/api/*`；数据一律要口令。
//!
//! 关掉服务走两条路：优雅停（进程退出时松掉监听）与运行中停（`enabled` 改成假，
//! 接口立刻回 503，端口要到下次启动才释放）。两者都不动插件、排期与推送。

use super::LOG_TARGET;
use super::state::Console;
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use std::sync::Arc;
use tokio::net::TcpListener;

pub(super) async fn start(console: Arc<Console>, bind: &str, port: u16) -> Result<(), String> {
    let listener = TcpListener::bind((bind, port))
        .await
        .map_err(|e| format!("监听 {bind}:{port} 失败：{e}；换一个 port 或先停掉占用它的进程"))?;

    let app = router(console.clone());
    let (stop, stopped) = tokio::sync::oneshot::channel();
    console.set_stopper(stop);

    tokio::spawn(async move {
        let served = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = stopped.await;
        });
        if let Err(error) = served.await {
            warn!(target: LOG_TARGET, "控制台服务已退出：{error}");
        }
    });
    Ok(())
}

fn router(console: Arc<Console>) -> Router {
    Router::new()
        .route("/", get(super::assets::index))
        .route("/favicon.ico", get(super::assets::favicon))
        .route("/icon.svg", get(super::assets::icon))
        .route("/app.css", get(super::assets::css))
        .route("/app.js", get(super::assets::js))
        .route("/manifest.webmanifest", get(super::assets::manifest))
        .nest("/api", super::api::routes(console.clone()))
        .with_state(console)
}

/// 口令闸门。挂在 `/api` 这一棵上，静态资源不受它管。
pub(super) async fn guard(State(console): State<Arc<Console>>, request: Request, next: Next) -> Response {
    if !console.enabled() {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "控制台已关闭；重新打开要改回 [console] enabled，端口在下次启动时释放",
        );
    }
    if authorized(console.token(), &request) {
        return next.run(request).await;
    }
    fail(
        StatusCode::UNAUTHORIZED,
        "口令不对；用启动日志里那条带 ?t= 的地址打开，或把 data/console/token 的内容填进解锁页",
    )
}

/// 三处都能带口令：请求头（脚本用）、`Authorization: Bearer`（命令行用）、
/// 查询串 `?t=`（地址栏与 EventSource 用，浏览器不给后者设请求头的机会）。
///
/// 收的是口令本身而不是 `Console`，好让这条判据能单独测——它是整个服务的门。
fn authorized(token: &str, request: &Request) -> bool {
    if let Some(value) = request
        .headers()
        .get("x-zhiyan-token")
        .and_then(|value| value.to_str().ok())
        && same(value, token)
    {
        return true;
    }
    if let Some(value) = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        && let Some(bearer) = value.strip_prefix("Bearer ")
        && same(bearer, token)
    {
        return true;
    }
    request.uri().query().is_some_and(|query| {
        query
            .split('&')
            .filter_map(|pair| pair.strip_prefix("t="))
            .any(|value| same(value, token))
    })
}

/// 定长比较：口令是十六进制，长度一致时逐字节走完，不提前返回。
fn same(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// 接口的失败一律是 JSON：页面按 `error` 那一格出提示，不必去猜状态码。
pub(super) fn fail(status: StatusCode, message: &str) -> Response {
    (
        status,
        axum::Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn request(target: &str, headers: &[(&str, &str)]) -> Request {
        let mut builder = Request::builder().uri(target);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(axum::body::Body::empty()).unwrap()
    }

    #[test]
    fn the_same_token_matches_and_others_do_not() {
        assert!(same("abc123", "abc123"));
        assert!(!same("abc123", "abc124"));
        assert!(!same("abc123", "abc1234"));
        assert!(!same("abc123", ""));
    }

    /// 三条路都要通：脚本用请求头、命令行用 Bearer、地址栏与 EventSource 用查询串。
    #[test]
    fn every_credential_carrier_is_accepted() {
        assert!(authorized(TOKEN, &request("/api/overview", &[("x-zhiyan-token", TOKEN)])));
        assert!(authorized(
            TOKEN,
            &request("/api/overview", &[(header::AUTHORIZATION.as_str(), &format!("Bearer {TOKEN}"))])
        ));
        assert!(authorized(TOKEN, &request(&format!("/api/overview?t={TOKEN}"), &[])));
        assert!(authorized(TOKEN, &request(&format!("/api/overview?a=1&t={TOKEN}&b=2"), &[])));
    }

    /// 少一个字符、多一个字符、差一位都不行；没有口令的请求一律挡在外面。
    #[test]
    fn nothing_else_gets_in() {
        assert!(!authorized(TOKEN, &request("/api/overview", &[])));
        assert!(!authorized(TOKEN, &request("/api/overview?t=", &[])));
        assert!(!authorized(TOKEN, &request("/api/overview?t=0123456789abcdef0123456789abcde", &[])));
        assert!(!authorized(TOKEN, &request("/api/overview?t=0123456789abcdef0123456789abcdee", &[])));
        assert!(!authorized(TOKEN, &request("/api/overview?t=0123456789abcdef0123456789abcdeg", &[])));
        assert!(!authorized(TOKEN, &request("/api/overview", &[("x-zhiyan-token", "nope")])));
        // 查询串里出现了口令，但键名不是 t（`index.html?note=t…` 这类）不算。
        assert!(!authorized(TOKEN, &request(&format!("/api/overview?note={TOKEN}"), &[])));
    }
}
