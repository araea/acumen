//! 管理面板：知微没在运行时也打得开的那扇门（`acumen --panel`，runit 单独托管）。
//!
//! 控制台长在机器人进程里，机器人一停，页面也跟着没了——不会用 Termux 的人从此
//! 只能干瞪眼。面板是同一个可执行文件的另一种跑法，只做三件事：
//!
//! 1. **同一张页面。** 静态资源原样给，`/api/*` 原样转给机器人的控制台（日志流也是
//!    边收边转）。机器人在跑时，打开面板和打开控制台没有区别。
//! 2. **机器人不在时说清楚。** 转发连不上就回 503 + `offline`，页面据此换成「已停止」
//!    那一屏，而不是一张报错页。
//! 3. **起停与更新。** `/panel/*` 调的是管理命令 `acumen start / stop / stop --all /
//!    restart / update`，与终端里敲的是同一件事（[`super::power`] 是共用的那一份）。
//!
//! 口令与控制台是同一个（`[console] token` 或 `data/console/token`）。可执行文件被更新
//! 换掉之后，面板在空闲时自己退出，由 runit 用新版本拉起。

use super::LOG_TARGET;
use super::power::{self, Action, SpawnError};
use axum::Router;
use axum::body::Body;
use axum::extract::{Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

struct Panel {
    token: String,
    /// 机器人控制台的地址（`http://127.0.0.1:7801`）。
    upstream: String,
    client: reqwest::Client,
}

/// 从 `config.toml` 读出来的那几项：面板跟控制台同一套口令，端口另起一个。
struct Settings {
    bind: String,
    port: u16,
    panel_port: u16,
    token: String,
}

fn settings() -> Settings {
    let table = std::fs::read_to_string("config.toml")
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .and_then(|mut table| table.remove("console"))
        .and_then(|value| value.as_table().cloned())
        .unwrap_or_default();
    let port = |key: &str, default: u16| {
        table
            .get(key)
            .and_then(toml::Value::as_integer)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(default)
    };
    Settings {
        bind: table
            .get("bind")
            .and_then(toml::Value::as_str)
            .unwrap_or("127.0.0.1")
            .to_string(),
        port: port("port", 7801),
        panel_port: port("panel_port", 7800),
        token: table
            .get("token")
            .and_then(toml::Value::as_str)
            .unwrap_or("")
            .to_string(),
    }
}

/// 面板的入口：跑到进程结束为止，返回退出码。
pub(crate) async fn run() -> i32 {
    let settings = settings();
    if settings.panel_port == 0 {
        // runit 会把退出的服务立刻拉起来：关掉的面板就安静地待着。
        info!(target: LOG_TARGET, "管理面板已关闭（[console] panel_port = 0）");
        loop {
            tokio::time::sleep(Duration::from_hours(24)).await;
        }
    }
    let token = match super::state::resolve_token(&settings.token).await {
        Ok(token) => token,
        Err(error) => {
            error!(target: LOG_TARGET, "管理面板拿不到口令：{error}");
            return 1;
        }
    };
    let client = match reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(3))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            error!(target: LOG_TARGET, "管理面板起不来：{error}");
            return 1;
        }
    };
    let host = super::state::display_host(&settings.bind);
    let panel = Arc::new(Panel {
        token: token.clone(),
        upstream: format!("http://{host}:{}", settings.port),
        client,
    });
    let listener = match tokio::net::TcpListener::bind((settings.bind.as_str(), settings.panel_port)).await {
        Ok(listener) => listener,
        Err(error) => {
            error!(target: LOG_TARGET, "管理面板监听 {}:{} 失败：{error}", settings.bind, settings.panel_port);
            // 端口被占时别每秒重试一次。
            tokio::time::sleep(Duration::from_secs(30)).await;
            return 1;
        }
    };
    let url = power::panel_url(&settings.bind, settings.panel_port, &token);
    if let Ok(dir) = crate::storage::data_path("console") {
        let _ = crate::storage::write_atomic_private_async(dir.join("panel-url"), format!("{url}\n").into_bytes()).await;
    }
    info!(target: LOG_TARGET, "管理面板已就绪 {url}");
    tokio::spawn(watch_binary());

    let guarded = Router::new()
        .route("/status", get(status))
        .route("/log", get(log_tail))
        .route("/{action}", post(act))
        .layer(axum::middleware::from_fn_with_state(panel.clone(), guard));
    let app = super::server::assets(Router::new())
        .route("/api/{*rest}", any(proxy))
        .nest("/panel", guarded)
        .with_state(panel);
    if let Err(error) = axum::serve(listener, app).await {
        error!(target: LOG_TARGET, "管理面板退出：{error}");
        return 1;
    }
    0
}

async fn guard(State(panel): State<Arc<Panel>>, request: Request, next: Next) -> Response {
    if super::server::authorized(&panel.token, &request) {
        return next.run(request).await;
    }
    super::server::fail(StatusCode::UNAUTHORIZED, "口令不对；用 acumen ui 打开，或把 data/console/token 的内容填进解锁页")
}

/// 可执行文件被更新换掉了：没有命令在跑就退出，runit 会用新版本拉起来。
async fn watch_binary() {
    loop {
        tokio::time::sleep(Duration::from_secs(20)).await;
        let replaced = std::fs::read_link("/proc/self/exe")
            .is_ok_and(|path| path.to_string_lossy().ends_with(" (deleted)"));
        if replaced && !power::job_running() {
            info!(target: LOG_TARGET, "可执行文件已更新，管理面板换用新版本");
            std::process::exit(0);
        }
    }
}

// ==================== 转发 ====================

/// 原样转给机器人的控制台。连不上就是机器人没在跑：回 503，页面换成「已停止」那一屏。
async fn proxy(State(panel): State<Arc<Panel>>, request: Request) -> Response {
    let path = request
        .uri()
        .path_and_query()
        .map_or("/api", |value| value.as_str())
        .to_string();
    let method = request.method().clone();
    let mut headers = request.headers().clone();
    headers.remove(header::HOST);
    let Ok(body) = axum::body::to_bytes(request.into_body(), 16 * 1024 * 1024).await else {
        return super::server::fail(StatusCode::PAYLOAD_TOO_LARGE, "请求太大");
    };
    let upstream = panel
        .client
        .request(method, format!("{}{path}", panel.upstream))
        .headers(headers)
        .body(body)
        .send()
        .await;
    let response = match upstream {
        Ok(response) => response,
        Err(error) if error.is_connect() || error.is_timeout() => return offline(),
        Err(error) => {
            return super::server::fail(StatusCode::BAD_GATEWAY, &format!("转给知微时出错：{error}"));
        }
    };
    let status = response.status();
    let mut out = Response::builder().status(status);
    for (name, value) in response.headers() {
        if name != header::CONNECTION && name != header::TRANSFER_ENCODING {
            out = out.header(name, value);
        }
    }
    // 日志流是长连接：收一块转一块，不攒整份。
    let stream = futures_util::stream::unfold(response, |mut response| async move {
        match response.chunk().await {
            Ok(Some(chunk)) => Some((Ok::<_, std::io::Error>(chunk), response)),
            _ => None,
        }
    });
    out.body(Body::from_stream(stream))
        .unwrap_or_else(|_| super::server::fail(StatusCode::BAD_GATEWAY, "知微的回复读不出来"))
}

fn offline() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(json!({ "error": "知微没在运行", "offline": true })),
    )
        .into_response()
}

// ==================== 状态与日志 ====================

async fn status() -> Response {
    let pid = power::bot_pid();
    let latest = if crate::release::managed() {
        crate::release::check_cached(Duration::from_mins(30))
            .await
            .ok()
            .flatten()
            .map(|release| json!({ "version": release.version, "notes": release.notes }))
    } else {
        None
    };
    axum::Json(json!({
        "version": crate::release::VERSION,
        "managed": crate::release::managed(),
        "service": power::service().exists(),
        "running": pid.is_some(),
        "pid": pid,
        "uptime": pid.and(power::uptime()),
        "stopped": power::service().join("down").exists(),
        "latest": latest,
        "job": power::job(),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct LogQuery {
    #[serde(default)]
    lines: Option<usize>,
}

/// 机器人自己写的终端日志（runit 收的那份）。机器人没在跑时，这是唯一看得到原因的地方。
async fn log_tail(Query(query): Query<LogQuery>) -> Response {
    let limit = query.lines.unwrap_or(120).clamp(10, 1000);
    let path = power::prefix().join("var/log/sv/acumen/current");
    let text = tokio::fs::read(&path).await.unwrap_or_default();
    let text = power::strip_ansi(&String::from_utf8_lossy(&text));
    let lines: Vec<&str> = text.lines().collect();
    axum::Json(json!({ "lines": lines[lines.len().saturating_sub(limit)..] })).into_response()
}

// ==================== 起停 ====================

/// 起停与更新：交给管理命令，在它自己的进程组里跑（面板因更新重启也不会把它带走）。
async fn act(axum::extract::Path(action): axum::extract::Path<String>) -> Response {
    let Some(action) = Action::parse(&action) else {
        return super::server::fail(StatusCode::NOT_FOUND, "没有这个操作");
    };
    match power::spawn(action) {
        Ok(()) => (StatusCode::ACCEPTED, axum::Json(json!({ "message": action.message() }))).into_response(),
        Err(SpawnError::Busy) => super::server::fail(StatusCode::CONFLICT, "上一个操作还没结束，稍等一下"),
        Err(SpawnError::Unavailable(why)) => super::server::fail(StatusCode::INTERNAL_SERVER_ERROR, &why),
    }
}
