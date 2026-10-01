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
        .route("/icon-192.png", get(super::assets::icon_192))
        .route("/icon-512.png", get(super::assets::icon_512))
        .route("/icon-maskable-512.png", get(super::assets::icon_maskable))
        .route("/icon-monochrome.svg", get(super::assets::icon_monochrome))
        .route("/apple-touch-icon.png", get(super::assets::apple_icon))
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
        .get("x-acumen-token")
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
    // 查询串里的口令是百分号编码过的（EventSource 那边必然编码），比对前要先解回来，
    // 否则含空格、`+`、`%`、`#`、`&` 与非 ASCII 的口令永远对不上。
    request.uri().query().is_some_and(|query| {
        url::form_urlencoded::parse(query.as_bytes())
            .any(|(key, value)| key == "t" && same(&value, token))
    })
}

/// 定长比较：长度一致时逐字节走完，不提前返回。
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
