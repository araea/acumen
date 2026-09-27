use crate::config::{AppConfig, BotConfig};
use crate::event::{BotStatus, Context, Event, EventType, LoginUser, SendPacket};
use crate::matcher::Matcher;
use crate::scheduler::Scheduler;
use crate::{debug, error, info, plugins, warn};
use futures_util::future::BoxFuture;
use futures_util::{SinkExt, StreamExt};
use sea_orm::DatabaseConnection;
use serde::Serialize;
use serde_json::{Value, json};
use simd_json::base::ValueAsScalar;
use simd_json::derived::ValueObjectAccessAsScalar;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::Mutex as AsyncMutex;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, protocol::Message as WsMessage},
};

pub mod api;
pub mod forward;
pub mod message;
#[allow(dead_code)]
pub mod qq;

pub type BotError = Box<dyn std::error::Error + Send + Sync>;
pub type LockedWriter = Arc<SatoriClient>;

/// QQ has accepted a send, but its final receipt is missing. Retrying may duplicate it.
pub fn delivery_uncertain(error: &(dyn std::error::Error + 'static)) -> bool {
    if error.to_string().contains("send outcome unknown") {
        return true;
    }
    // A lost HTTP response also cannot prove that QQ rejected the submission.
    error.downcast_ref::<reqwest::Error>().is_some_and(|e| {
        (e.is_timeout() || e.is_body() || e.is_decode() || e.is_request()) && !e.is_connect()
    })
}

/// `message.create` 的可选时效条件（satori-qq 扩展）。
///
/// 实现端在拿到出站队列的发送权、以及媒体转换与重试等待之后，才把消息交给 QQ
/// 内核；这中间是 acumen 完全看不见的一段时间。条件成立要求锚点消息仍是实现端
/// 最近推送给本应用的该频道消息，条件不成立就整条跳过并返回 `[]`——不算发送
/// 失败，也不触发熔断。用它，「话说晚了」就变成「这句话干脆没说」。
#[derive(Debug, Clone)]
pub struct Freshness {
    /// 锚点消息 ID：发送前它必须仍是该频道最新的一条。
    pub message_id: String,
    /// Unix 毫秒截止时间。
    pub expires_at: u64,
}

/// 每个群最近一条**入站**消息的 ID。
///
/// 时效条件的锚点必须和实现端的记账一致，而实现端记的是「推送给本应用的每一条
/// 消息」——包括被指令消费掉、被过滤器拦掉、以及根本没走到某个插件的那些。所以
/// 这笔账只能记在流水线之前的适配器层，不能由某个插件自己攒。
fn latest_inbound() -> &'static std::sync::Mutex<std::collections::HashMap<String, String>> {
    static LATEST: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, String>>,
    > = std::sync::OnceLock::new();
    LATEST.get_or_init(Default::default)
}

/// 记下一条入站群消息，供时效条件取锚点。自己发出的消息不会作为事件回来
/// （实现端按出站 ID 去重），因此连着发几条不会把自己的条件顶掉。
pub fn note_inbound(event: &Event) {
    if event.get_str("satori_type") != Some("message-created") {
        return;
    }
    let Some(group) = event.get_str("group_id").filter(|id| !id.is_empty()) else {
        return;
    };
    let Some(id) = event
        .get_str("message_id")
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
    else {
        return;
    };
    let mut guard = latest_inbound()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    // 群数量本来就有限，但配置改动和退群都可能留下条目，给个上限兜底。
    if guard.len() > 512 {
        guard.clear();
    }
    guard.insert(group.to_string(), id);
}

/// 取一个群的时效锚点；从未收到过消息时没有可锚定的对象。
pub fn freshness_for(group_id: &str, valid_for: Duration) -> Option<Freshness> {
    if valid_for.is_zero() {
        return None;
    }
    let message_id = latest_inbound()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(group_id)
        .cloned()?;
    Some(Freshness {
        message_id,
        expires_at: (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            + valid_for)
            .as_millis() as u64,
    })
}

/// Satori 的事件和 API 使用两条独立通道：WS 只收事件，HTTP 负责所有调用。
pub struct SatoriClient {
    endpoint: String,
    token: Option<String>,
    http: reqwest::Client,
    console: bool,
    /// `READY` / `META` 下发的代理路由前缀，决定哪些平台链接要经 `/v1/proxy` 取。
    proxy_urls: RwLock<Arc<Vec<String>>>,
}

impl SatoriClient {
    pub(crate) fn new(endpoint: String, token: Option<String>) -> Self {
        Self {
            endpoint,
            token: token.filter(|value| !value.trim().is_empty()),
            http: crate::http::client(),
            console: false,
            proxy_urls: RwLock::new(Arc::new(Vec::new())),
        }
    }

    pub fn console() -> Self {
        Self {
            endpoint: String::new(),
            token: None,
            http: crate::http::client(),
            console: true,
            proxy_urls: RwLock::new(Arc::new(Vec::new())),
        }
    }

    pub fn set_proxy_urls(&self, urls: Vec<String>) {
        *self.proxy_urls.write().unwrap() = Arc::new(urls);
    }

    /// 解析消息元素里 `src` 的取件方式，交给 `message` 模块使用。
    pub fn resources(&self) -> message::ResourceProxy {
        message::ResourceProxy::new(
            self.endpoint.clone(),
            self.proxy_urls.read().unwrap().clone(),
        )
    }

    pub fn connection_key(&self) -> &str {
        if self.console {
            "console"
        } else {
            &self.endpoint
        }
    }

    pub async fn call<P, R>(&self, ctx: &Context, method: &str, params: P) -> Result<R, BotError>
    where
        P: Serialize,
        R: serde::de::DeserializeOwned,
    {
        let params = serde_json::to_value(params)?;
        // 目标属于另一个实现端（定时推送是拿先连上的那条连接注册的）就交给它。
        if let Some(route) = route_for(self, &params) {
            return route.client.post(&route.bot, method, params).await;
        }
        self.post(&ctx.bot, method, params).await
    }

    async fn post<R>(&self, bot: &BotStatus, method: &str, params: Value) -> Result<R, BotError>
    where
        R: serde::de::DeserializeOwned,
    {
        if self.console {
            return match method {
                "message.create" => {
                    println!(
                        "\x1b[36m[Bot Reply] > \x1b[0m{}",
                        params.get("content").and_then(Value::as_str).unwrap_or("")
                    );
                    Ok(serde_json::from_value(Value::Array(Vec::new()))?)
                }
                // 控制台没有私聊频道这回事，拿用户 ID 充当即可。
                "user.channel.create" => Ok(serde_json::from_value(
                    json!({"id": params.get("user_id"), "type": 1}),
                )?),
                _ => Err(format!("控制台模式不支持 Satori API: {method}").into()),
            };
        }
        let url = format!("{}/v1/{}", self.endpoint, method);
        let mut request = self
            .http
            .post(url)
            .header("Satori-Platform", &bot.platform)
            .header("Satori-User-ID", &bot.login_user.get().id)
            .json(&params);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        // QQ/Satori 主进程若被 OEM freezer 暂停，loopback HTTP 也可能无限等待。
        // message.create 留出 30 秒排队、45 秒媒体确认/重试及转换余量。
        let timeout_seconds = if method == "message.create" { 100 } else { 65 };
        let response = request
            .timeout(Duration::from_secs(timeout_seconds))
            .send()
            .await?;
        let status = response.status();
        let bytes = response.bytes().await?;
        if !status.is_success() {
            return Err(api_error(method, status, &bytes));
        }
        let value = decode_response(method, &bytes)?;
        Ok(serde_json::from_value(value)?)
    }

    /// 使用标准 `upload.create` multipart 把 acumen 侧文件传给实现端。
    pub async fn upload(
        &self,
        ctx: &Context,
        data: Vec<u8>,
        name: &str,
        mime: &str,
    ) -> Result<Value, BotError> {
        if self.console {
            return Err("控制台模式不支持文件上传".into());
        }
        let part = reqwest::multipart::Part::bytes(data)
            .file_name(name.to_string())
            .mime_str(mime)?;
        let form = reqwest::multipart::Form::new().part("file", part);
        let mut request = self
            .http
            .post(format!("{}/v1/upload.create", self.endpoint))
            .header("Satori-Platform", &ctx.bot.platform)
            .header("Satori-User-ID", &ctx.bot.login_user.get().id)
            .multipart(form);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request.timeout(Duration::from_secs(65)).send().await?;
        let status = response.status();
        let bytes = response.bytes().await?;
        if !status.is_success() {
            return Err(api_error("upload.create", status, &bytes));
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
}

/// 同时连着几个实现端（satori-qq、satori-wx）时，每条连接都登记在这里。
#[derive(Clone)]
struct Route {
    client: Arc<SatoriClient>,
    bot: Arc<BotStatus>,
}

fn routes() -> std::sync::MutexGuard<'static, Vec<Route>> {
    static ROUTES: std::sync::OnceLock<std::sync::Mutex<Vec<Route>>> = std::sync::OnceLock::new();
    ROUTES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 连上（或重连上）一个实现端时登记；同一地址只留最新的一份。
fn register_route(client: Arc<SatoriClient>, bot: Arc<BotStatus>) {
    let mut routes = routes();
    routes.retain(|route| route.client.connection_key() != client.connection_key());
    routes.push(Route { client, bot });
}

/// 目录：每个频道、群、用户 ID 是在哪条连接上见到的（值是 `connection_key`）。
///
/// Satori 的 ID 只在各自平台内唯一，插件手里却只有 ID。回话时事件自带的连接就是对的；
/// 定时推送这类拿着先连上的那条连接、目标却在另一个平台的调用，要靠目录找回主人。
/// 目录从入站事件和连上时的 `guild.list` 学来，不落盘：重启后第一次 `guild.list` 就补齐了。
fn directory() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, String>> {
    static DIRECTORY: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, String>>,
    > = std::sync::OnceLock::new();
    DIRECTORY
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn note_owner<'a>(client: &SatoriClient, ids: impl IntoIterator<Item = &'a str>) {
    let owner = client.connection_key();
    let mut directory = directory();
    for id in ids.into_iter().filter(|id| !id.is_empty()) {
        if directory.get(id).is_none_or(|known| known != owner) {
            directory.insert(id.to_string(), owner.to_string());
        }
    }
}

/// 参数里的目标 ID 属于另一条连接时，找出那条连接；属于当前连接或没见过就返回 None。
fn route_for(current: &SatoriClient, params: &Value) -> Option<Route> {
    let owner = {
        let directory = directory();
        ["channel_id", "guild_id", "user_id"]
            .iter()
            .filter_map(|key| params.get(*key)?.as_str())
            .find_map(|id| directory.get(id).cloned())?
    };
    if owner == current.connection_key() {
        return None;
    }
    routes()
        .iter()
        .find(|route| route.client.connection_key() == owner)
        .cloned()
}

/// 把连接上的群登记进目录。`guild.list` 按 `next` 翻页；失败只记一笔，入站事件照样会补。
async fn learn_guilds(client: Arc<SatoriClient>, bot: Arc<BotStatus>) {
    let mut next: Option<String> = None;
    for _ in 0..50 {
        let params = match &next {
            Some(next) => json!({ "next": next }),
            None => json!({}),
        };
        let page: Value = match client.post(&bot, "guild.list", params).await {
            Ok(page) => page,
            Err(error) => {
                debug!(target: "Bot", "guild.list 失败，目录只能靠入站事件补：{error}");
                return;
            }
        };
        let guilds = page.get("data").and_then(Value::as_array);
        note_owner(
            &client,
            guilds
                .into_iter()
                .flatten()
                .filter_map(|guild| guild.get("id")?.as_str()),
        );
        next = page
            .get("next")
            .and_then(Value::as_str)
            .filter(|next| !next.is_empty())
            .map(str::to_owned);
        if next.is_none() {
            return;
        }
    }
}

/// HTTP 成功只代表 RPC 已应答；QQ 内核可以在 JSON 中报告失败。
fn decode_response(method: &str, bytes: &[u8]) -> Result<Value, BotError> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null); // Satori 的无返回值方法可以回 204。
    }
    let value: Value = serde_json::from_slice(bytes)?;
    if method.starts_with("internal/") && value.get("ok") == Some(&Value::Bool(false)) {
        return Err(format!("Satori API {method} 内核失败: {value}").into());
    }
    Ok(value)
}

/// 把非 2xx 的响应体变成错误文案。
///
/// 实现端从 0.23.1 起在错误体里给机器可读的 `code`（例如 `removed_action`），这里一并带上：
/// 上游按 code 判断就不必去匹配会变的中文文案。
#[derive(Debug)]
pub struct SatoriApiError {
    pub method: String,
    pub status: u16,
    pub code: Option<String>,
    pub message: String,
}
impl std::fmt::Display for SatoriApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Satori API {} 失败 ({}): {}",
            self.method, self.status, self.message
        )?;
        if let Some(code) = &self.code {
            write!(f, " [code={code}]")?;
        }
        Ok(())
    }
}
impl std::error::Error for SatoriApiError {}
fn api_error(method: &str, status: reqwest::StatusCode, bytes: &[u8]) -> BotError {
    let parsed = serde_json::from_slice::<Value>(bytes).ok();
    Box::new(SatoriApiError {
        method: method.into(),
        status: status.as_u16(),
        code: parsed
            .as_ref()
            .and_then(|v| v.get("code")?.as_str())
            .map(str::to_owned),
        message: parsed
            .as_ref()
            .and_then(|v| v.get("message")?.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned()),
    })
}

/// Tasks must also be stopped when READY or a connected hook returns early.
struct AbortTask(tokio::task::JoinHandle<()>);
impl Drop for AbortTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Default)]
struct EventCursor {
    sn: Option<i64>,
    session: Option<String>,
    account: Option<(String, String)>,
}
impl EventCursor {
    fn ready(&mut self, ready: &Value, platform: &str, user: &str) {
        let session = ready
            .pointer("/body/satori_qq/session_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let account = (platform.to_string(), user.to_string());
        if self.session != session || self.account.as_ref().is_some_and(|v| v != &account) {
            self.sn = None;
        }
        self.session = session;
        self.account = Some(account);
    }
    fn accept(&mut self, body: &Value) -> bool {
        // Login events do not participate in resumption.
        if body
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t.starts_with("login-"))
        {
            return true;
        }
        let Some(sn) = body.get("sn").and_then(Value::as_i64) else {
            return true;
        };
        if self.sn.is_some_and(|previous| sn <= previous) {
            return false;
        }
        self.sn = Some(sn);
        true
    }
}

fn belongs_to_login(body: &Value, bot: &BotStatus) -> bool {
    let platform = body.pointer("/login/platform").and_then(Value::as_str);
    let user = body.pointer("/login/user/id").map(|id| raw_id(Some(id)));
    platform.is_none_or(|p| p == bot.platform)
        && user
            .as_ref()
            .is_none_or(|id| id.is_empty() || *id == bot.login_user.get().id)
}

pub fn entry(
    bot_config: BotConfig,
    global_config: Arc<RwLock<AppConfig>>,
    db: DatabaseConnection,
    scheduler: Arc<Scheduler>,
    save_lock: Arc<AsyncMutex<()>>,
    config_path: Arc<str>,
) -> BoxFuture<'static, ()> {
    Box::pin(async move {
        run_bot_loop(
            bot_config,
            global_config,
            db,
            scheduler,
            save_lock,
            config_path,
        )
        .await
    })
}

/// Satori 主循环：3 秒起指数退避，最长 60 秒。
pub async fn run_bot_loop(
    bot_config: BotConfig,
    global_config: Arc<RwLock<AppConfig>>,
    db: DatabaseConnection,
    scheduler: Arc<Scheduler>,
    save_lock: Arc<AsyncMutex<()>>,
    config_path: Arc<str>,
) {
    let endpoint = bot_config.url.clone().unwrap_or_default();
    let mut backoff = Duration::from_secs(3);
    // 最后一个收到的事件序列号。重连时带上它，实现端会补推断线期间的事件。
    let mut session_sn = EventCursor::default();
    loop {
        let connected_at = std::time::Instant::now();
        match connect_and_listen(
            &bot_config,
            global_config.clone(),
            db.clone(),
            scheduler.clone(),
            save_lock.clone(),
            config_path.clone(),
            &mut session_sn,
        )
        .await
        {
            Ok(()) => {
                warn!(target: "Bot", "Satori [{}] 连接断开，{:?} 后重连...", endpoint, backoff)
            }
            Err(err) => {
                error!(target: "Bot", "Satori [{}] 连接失败: {}。{:?} 后重试...", endpoint, err, backoff)
            }
        }
        if connected_at.elapsed() >= Duration::from_secs(60) {
            backoff = Duration::from_secs(3);
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

/// 协议规定应用每 10 秒发一次 `PING`，实现端回 `PONG`。
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

const OP_EVENT: i64 = 0;
const OP_PING: i64 = 1;
const OP_PONG: i64 = 2;
const OP_IDENTIFY: i64 = 3;
const OP_READY: i64 = 4;
const OP_META: i64 = 5;

#[allow(clippy::too_many_arguments)]
async fn connect_and_listen(
    config: &BotConfig,
    global_config: Arc<RwLock<AppConfig>>,
    db: DatabaseConnection,
    scheduler: Arc<Scheduler>,
    save_lock: Arc<AsyncMutex<()>>,
    config_path: Arc<str>,
    session_sn: &mut EventCursor,
) -> Result<(), BotError> {
    let endpoint = normalize_endpoint(config.url.as_deref().ok_or("Satori URL 未配置")?)?;
    let events_url = events_url(&endpoint)?;
    let request = events_url.into_client_request()?;
    let (stream, _) = connect_async(request).await?;
    let (mut ws_write, mut ws_read) = stream.split();

    // 出站帧统一走一条队列：心跳任务和事件循环都只是往队列里投递。
    let (outbound, mut outbound_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let _writer_task = AbortTask(tokio::spawn(async move {
        while let Some(text) = outbound_rx.recv().await {
            if ws_write.send(WsMessage::Text(text.into())).await.is_err() {
                break;
            }
        }
        let _ = ws_write.close().await;
    }));

    let token = effective_token(config);
    let mut identify_body = json!({});
    if let Some(token) = token.as_deref() {
        identify_body["token"] = json!(token);
    }
    // 省略 sn 表示开新会话；带上 sn 则请求补推断线期间的事件。
    if let Some(sn) = session_sn.sn {
        identify_body["sn"] = json!(sn);
        info!(target: "Bot", "Satori [{}] 尝试从 sn={} 恢复会话。", endpoint, sn);
    }
    outbound.send(json!({"op": OP_IDENTIFY, "body": identify_body}).to_string())?;

    let ready = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(frame) = ws_read.next().await {
            let frame = frame?;
            if let WsMessage::Text(text) = frame {
                let packet: Value = serde_json::from_str(&text)?;
                if packet.get("op").and_then(Value::as_i64) == Some(OP_READY) {
                    return Ok::<Value, BotError>(packet);
                }
            }
        }
        Err("Satori 在 READY 前关闭连接".into())
    })
    .await
    .map_err(|_| "等待 Satori READY 超时")??;

    let login = ready
        .pointer("/body/logins/0")
        .ok_or("Satori READY 未提供登录信息")?;
    let user = login.get("user").unwrap_or(&Value::Null);
    let bot_status = Arc::new(BotStatus {
        adapter: login
            .get("adapter")
            .and_then(Value::as_str)
            .unwrap_or("satori-qq")
            .to_string(),
        platform: login
            .get("platform")
            .and_then(Value::as_str)
            .unwrap_or("red")
            .to_string(),
        login_user: login_user_of(user).into(),
    });
    session_sn.ready(
        &ready,
        &bot_status.platform,
        &bot_status.login_user.get().id,
    );
    let writer = Arc::new(SatoriClient::new(endpoint.clone(), token));
    writer.set_proxy_urls(proxy_urls(&ready));
    register_route(writer.clone(), bot_status.clone());
    tokio::spawn(learn_guilds(writer.clone(), bot_status.clone()));
    let matcher = Arc::new(Matcher::new());

    info!(
        target: "Bot",
        "Bot [{}] 连接成功！(Satori {}/{}, login={})",
        endpoint,
        bot_status.adapter,
        bot_status.platform,
        bot_status.login_user.get().id
    );

    let connected_ctx = Context {
        event: EventType::Init,
        config: global_config.clone(),
        config_save_lock: save_lock.clone(),
        db: db.clone(),
        scheduler: scheduler.clone(),
        matcher: matcher.clone(),
        config_path: config_path.clone(),
        bot: bot_status.clone(),
    };
    let _heartbeat = AbortTask(tokio::spawn({
        let outbound = outbound.clone();
        async move {
            let ping = json!({"op": OP_PING, "body": {}}).to_string();
            loop {
                tokio::time::sleep(HEARTBEAT_INTERVAL).await;
                if outbound.send(ping.clone()).is_err() {
                    break;
                }
            }
        }
    }));

    plugins::do_connected(connected_ctx, writer.clone()).await?;

    listen(
        &mut ws_read,
        &outbound,
        &writer,
        &bot_status,
        &global_config,
        &db,
        &scheduler,
        &save_lock,
        &config_path,
        &matcher,
        session_sn,
    )
    .await
}

/// 事件循环：`EVENT` 进插件流水线，`META` 刷新代理路由，`PING` 回 `PONG`。
#[allow(clippy::too_many_arguments)]
async fn listen(
    ws_read: &mut futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    outbound: &tokio::sync::mpsc::UnboundedSender<String>,
    writer: &LockedWriter,
    bot_status: &Arc<BotStatus>,
    global_config: &Arc<RwLock<AppConfig>>,
    db: &DatabaseConnection,
    scheduler: &Arc<Scheduler>,
    save_lock: &Arc<AsyncMutex<()>>,
    config_path: &Arc<str>,
    matcher: &Arc<Matcher>,
    session_sn: &mut EventCursor,
) -> Result<(), BotError> {
    let mut pong_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let frame = tokio::select! {
            frame = ws_read.next() => match frame { Some(frame) => frame?, None => return Ok(()) },
            _ = tokio::time::sleep_until(pong_deadline) => return Err("Satori PONG 超时".into()),
        };
        match frame {
            WsMessage::Text(text) => {
                let packet: Value = match serde_json::from_str(&text) {
                    Ok(packet) => packet,
                    Err(err) => {
                        warn!(target: "Bot", "忽略无效 Satori 帧: {}", err);
                        continue;
                    }
                };
                match packet.get("op").and_then(Value::as_i64) {
                    Some(OP_EVENT) => {
                        let Some(body) = packet.get("body") else {
                            continue;
                        };
                        // Cursor belongs to this login, not to the entire WS stream. A foreign
                        // login's higher sn must never cause our replayed messages to be lost.
                        if !belongs_to_login(body, bot_status) {
                            if matches!(
                                body.get("type").and_then(Value::as_str),
                                Some("login-updated" | "login-removed")
                            ) && bot_status.adapter == "satori-qq"
                            {
                                return Ok(()); // same slot changed account; obtain a fresh READY
                            }
                            continue;
                        }
                        if !session_sn.accept(body) {
                            continue;
                        }
                        match body.get("type").and_then(Value::as_str) {
                            Some("login-updated" | "login-removed") => {
                                if body["type"] == "login-removed" {
                                    return Ok(());
                                }
                                apply_login_update(body, bot_status);
                                continue;
                            }
                            Some("login-added") => continue,
                            _ => {}
                        }
                        let event = match normalize_event(body, bot_status, &writer.resources()) {
                            Ok(event) => event,
                            Err(err) => {
                                warn!(target: "Bot", "Satori 事件转换失败: {}", err);
                                continue;
                            }
                        };
                        // 时效锚点要和实现端的记账一致，必须先于插件流水线记下。
                        note_inbound(&event);
                        note_owner(
                            writer,
                            ["group_id", "user_id", "channel_id"]
                                .into_iter()
                                .filter_map(|key| event.get_str(key)),
                        );
                        note_direct_channel(writer, &event);
                        let writer = writer.clone();
                        let config = global_config.clone();
                        let db = db.clone();
                        let scheduler = scheduler.clone();
                        let save_lock = save_lock.clone();
                        let config_path = config_path.clone();
                        let matcher = matcher.clone();
                        let bot = bot_status.clone();
                        let processing = process_event(
                            event,
                            writer,
                            config,
                            db,
                            scheduler,
                            save_lock,
                            config_path,
                            matcher,
                            bot,
                        );
                        tokio::spawn(async move {
                            if let Err(err) = processing.await {
                                error!(target: "Bot", "Satori event processing error: {}", err);
                            }
                        });
                    }
                    // 协议里 PING 由应用发出，这里回 PONG 只是兼容反向心跳的实现端。
                    Some(OP_PING) => {
                        outbound.send(json!({"op": OP_PONG, "body": {}}).to_string())?;
                    }
                    Some(OP_PONG) => {
                        pong_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
                    }
                    Some(OP_META) => {
                        if let Some(body) = packet.get("body") {
                            writer.set_proxy_urls(proxy_urls(body));
                        }
                    }
                    _ => {}
                }
            }
            WsMessage::Close(_) => return Ok(()),
            _ => {}
        }
    }
}

/// `login-updated` 的 `login` 是实现端当前认定的登录。账号变了就把共享账号换过去，
/// 出站请求的选择器与插件的自我识别都跟着走；否则实现端会把请求判成指向另一个登录。
fn apply_login_update(body: &Value, bot: &BotStatus) {
    let Some(login) = body.get("login") else {
        return;
    };
    let updated = login_user_of(login.get("user").unwrap_or(&Value::Null));
    if updated.id.is_empty() || updated.id != bot.login_user.get().id {
        // 账号未知的快照给不出可用账号，保留现有值。
        return;
    }
    let previous = bot.login_user.get();
    if previous.id == updated.id
        && previous.name == updated.name
        && previous.nick == updated.nick
        && previous.avatar == updated.avatar
    {
        return;
    }
    info!(
        target: "Bot",
        "Satori 登录账号更新：{} → {}",
        previous.id,
        updated.id
    );
    bot.login_user.set(updated);
}

/// `READY` 的 `logins[].user` 与 `login-updated` 的 `login.user` 是同一份结构。
fn login_user_of(user: &Value) -> LoginUser {
    LoginUser {
        // 原样保留：satori-wx 的账号是 wxid，按数字解析会变成空账号。
        id: raw_id(user.get("id")),
        name: optional_string(user.get("name")),
        nick: optional_string(user.get("nick")),
        avatar: optional_string(user.get("avatar")),
    }
}

/// `READY` 与 `META` 的 body 都带 `proxy_urls`，取值规则一致。
fn proxy_urls(packet: &Value) -> Vec<String> {
    packet
        .pointer("/body/proxy_urls")
        .or_else(|| packet.get("proxy_urls"))
        .and_then(Value::as_array)
        .map(|urls| {
            urls.iter()
                .filter_map(Value::as_str)
                .filter(|url| !url.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn effective_token(config: &BotConfig) -> Option<String> {
    std::env::var("ACUMEN_SATORI_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            config
                .access_token
                .clone()
                .filter(|value| !value.trim().is_empty())
        })
}

fn normalize_endpoint(raw: &str) -> Result<String, BotError> {
    let mut url = url::Url::parse(raw.trim())?;
    match url.scheme() {
        "ws" => url.set_scheme("http").map_err(|_| "无法转换 Satori URL")?,
        "wss" => url.set_scheme("https").map_err(|_| "无法转换 Satori URL")?,
        "http" | "https" => {}
        scheme => return Err(format!("不支持的 Satori URL scheme: {scheme}").into()),
    }
    let path = url.path().trim_end_matches('/').to_string();
    let base_path = path.strip_suffix("/v1/events").unwrap_or(&path).to_string();
    url.set_path(&base_path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn events_url(endpoint: &str) -> Result<String, BotError> {
    let mut url = url::Url::parse(endpoint)?;
    match url.scheme() {
        "http" => url
            .set_scheme("ws")
            .map_err(|_| "无法转换 Satori events URL")?,
        "https" => url
            .set_scheme("wss")
            .map_err(|_| "无法转换 Satori events URL")?,
        _ => return Err("Satori endpoint 必须是 http(s) URL".into()),
    }
    url.set_path(&format!("{}/v1/events", url.path().trim_end_matches('/')));
    Ok(url.into())
}

/// Prepare conversation state synchronously in receive order; only then spawn/poll
/// the returned future. Serializing the entire pipeline would block interactive commands.
#[allow(clippy::too_many_arguments)]
pub fn process_event(
    event: Event,
    writer: LockedWriter,
    config: Arc<RwLock<AppConfig>>,
    db: DatabaseConnection,
    scheduler: Arc<Scheduler>,
    save_lock: Arc<AsyncMutex<()>>,
    config_path: Arc<str>,
    matcher: Arc<Matcher>,
    bot: Arc<BotStatus>,
) -> BoxFuture<'static, Result<(), BotError>> {
    if let Some(group_id) = event.get_str("group_id").filter(|id| !id.is_empty()) {
        let should_drop = {
            let guard = config.read().unwrap();
            let listed = |list: &[String]| list.iter().any(|id| id == group_id);
            if guard.global_filter.enable_whitelist {
                !listed(&guard.global_filter.whitelist)
            } else if guard.global_filter.enable_blacklist {
                listed(&guard.global_filter.blacklist)
            } else {
                false
            }
        };
        if should_drop {
            return Box::pin(async { Ok(()) });
        }
    }

    let mut ctx = Context {
        event: EventType::Satori(event),
        config,
        config_save_lock: save_lock,
        db,
        scheduler,
        matcher,
        config_path,
        bot,
    };
    // Matcher dispatch only uses a synchronous mutex. Consume interactive input in
    // receive order as well, and let consumed messages interrupt pending repeats.
    if let EventType::Satori(event) = &ctx.event
        && event.get_str("post_type") == Some("message")
    {
        match ctx.matcher.dispatch(event.clone()) {
            Some(event) => ctx.event = EventType::Satori(event),
            None => {
                plugins::repeater::interrupt(&ctx, &writer);
                return Box::pin(async { Ok(()) });
            }
        }
    }
    let pending = plugins::repeater::prepare(&mut ctx, &writer);
    Box::pin(async move {
        if let Err(err) = plugins::repeater::send_prepared(&ctx, writer.clone(), pending).await {
            error!(target: "Plugin/Repeater", "复读发送失败: {}", err);
        }
        plugins::run(ctx, writer).await?;
        Ok(())
    })
}

pub async fn send_msg<M>(
    ctx: &Context,
    writer: LockedWriter,
    group_id: Option<&str>,
    user_id: Option<&str>,
    message: M,
) -> Result<(), BotError>
where
    M: Serialize,
{
    dispatch_send(ctx, writer, group_id, user_id, message, None)
        .await
        .map(|_| ())
}

/// Satori 的 message.create 是同步 HTTP RPC，成功返回即视为已确认。
pub async fn send_msg_ack<M>(
    ctx: &Context,
    writer: LockedWriter,
    group_id: Option<&str>,
    user_id: Option<&str>,
    message: M,
) -> Result<bool, BotError>
where
    M: Serialize,
{
    dispatch_send(ctx, writer, group_id, user_id, message, None).await?;
    Ok(true)
}

/// 发送消息并返回实现端分配的第一条消息 ID。
///
/// AI News 等需要让后续引用回复精确关联原消息的功能应使用此接口；普通发送
/// 仍使用 [`send_msg`]，无需关心回执内容。
pub async fn send_msg_id<M>(
    ctx: &Context,
    writer: LockedWriter,
    group_id: Option<&str>,
    user_id: Option<&str>,
    message: M,
) -> Result<Option<String>, BotError>
where
    M: Serialize,
{
    Ok(dispatch_send(ctx, writer, group_id, user_id, message, None)
        .await?
        .into_iter()
        .next())
}

/// 发送一条「群聊已经往前走了就不必再说」的消息，并返回实现端分配的消息 ID。
///
/// 搭话用它：模型思考加上模拟打字往往要十几秒，中间群里又说了话的话，这句就
/// 不该再落地了。acumen 自己已经在发送前查过一次窗口，但请求交给实现端之后还要
/// 排队，那一段只有实现端看得见——所以把同一个判断也交给它。
pub async fn send_fresh_msg_id<M>(
    ctx: &Context,
    writer: LockedWriter,
    group_id: Option<&str>,
    user_id: Option<&str>,
    message: M,
    freshness: Option<Freshness>,
) -> Result<Option<String>, BotError>
where
    M: Serialize,
{
    Ok(
        dispatch_send_with(ctx, writer, group_id, user_id, message, None, freshness)
            .await?
            .into_iter()
            .next(),
    )
}

/// A best-effort repeat may be dropped if the conversation advances while sending.
pub async fn send_repeater_msg<M: Serialize>(
    ctx: &Context,
    writer: LockedWriter,
    group_id: Option<&str>,
    user_id: Option<&str>,
    message: M,
    guard: plugins::repeater::RepeatGuard,
) -> Result<(), BotError> {
    dispatch_send(ctx, writer, group_id, user_id, message, Some(guard))
        .await
        .map(|_| ())
}

async fn dispatch_send<M>(
    ctx: &Context,
    writer: LockedWriter,
    group_id: Option<&str>,
    user_id: Option<&str>,
    message: M,
    repeat_guard: Option<plugins::repeater::RepeatGuard>,
) -> Result<Vec<String>, BotError>
where
    M: Serialize,
{
    dispatch_send_with(ctx, writer, group_id, user_id, message, repeat_guard, None).await
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_send_with<M>(
    ctx: &Context,
    writer: LockedWriter,
    group_id: Option<&str>,
    user_id: Option<&str>,
    message: M,
    repeat_guard: Option<plugins::repeater::RepeatGuard>,
    freshness: Option<Freshness>,
) -> Result<Vec<String>, BotError>
where
    M: Serialize,
{
    let (message_type, group_id, user_id) = if let Some(id) = group_id.filter(|id| !id.is_empty()) {
        ("group", Some(id), None)
    } else if let Some(id) = user_id.filter(|id| !id.is_empty()) {
        ("private", None, Some(id))
    } else {
        return Ok(Vec::new());
    };
    // 带文件/视频/语音的消息由实现端拆开发（顺媒体在 QQ 里必须单独成条，同条的
    // 引用、@、文字会把它顶成空气泡）。这里原样交给它：一次调用可能真发出几条，
    // 回执数组里就是多个 ID。见 satori-qq 的 `docs/SATORI_SUPPORT.md`。
    let params = simd_json::serde::to_owned_value(json!({
        "message_type": message_type,
        "group_id": group_id,
        "user_id": user_id,
        "message": serde_json::to_value(message)?,
    }))?;
    let original_event = match &ctx.event {
        EventType::Satori(event) => Some(event.clone()),
        EventType::BeforeSend(packet) => packet.original_event.clone(),
        EventType::Init => None,
    };
    let receipt_message_ids = Arc::new(std::sync::Mutex::new(Vec::new()));
    let packet = SendPacket {
        action: "message.create".to_string(),
        repeat_guard,
        freshness,
        params,
        original_event,
        receipt_message_ids: receipt_message_ids.clone(),
    };
    let next = Context {
        event: EventType::BeforeSend(packet),
        config: ctx.config.clone(),
        config_save_lock: ctx.config_save_lock.clone(),
        db: ctx.db.clone(),
        scheduler: ctx.scheduler.clone(),
        matcher: ctx.matcher.clone(),
        config_path: ctx.config_path.clone(),
        bot: ctx.bot.clone(),
    };
    plugins::run(next, writer).await?;
    Ok(receipt_message_ids
        .lock()
        .map_err(|_| "发送回执锁已损坏")?
        .clone())
}

/// 执行插件修改后的发送包。
pub async fn dispatch_packet(
    ctx: &Context,
    writer: LockedWriter,
    packet: &SendPacket,
) -> Result<(), BotError> {
    let group_id = packet.group_id();
    let user_id = packet.user_id();
    if group_id.is_none() && user_id.is_none() {
        return Ok(());
    }
    // 目标不在当前连接上（定时推送拿的是先连上的那条）就换到它所在的连接。
    let (client, bot) = match route_for(&writer, &json!({"guild_id": group_id, "user_id": user_id}))
    {
        Some(route) => (route.client, route.bot),
        None => (writer.clone(), ctx.bot.clone()),
    };
    let channel_id = match (group_id, user_id) {
        (Some(group_id), _) => group_id.to_string(),
        (None, Some(user_id)) => {
            direct_channel(&client, &bot, packet.original_event.as_ref(), user_id).await?
        }
        (None, None) => unreachable!(),
    };
    let content = packet
        .message()
        .map(message::to_content)
        .unwrap_or_default();
    let mut params = json!({"channel_id": &channel_id, "content": content});
    if let Some(guard) = &packet.repeat_guard
        && !guard.is_current()
    {
        debug!(target: "Plugin/Repeater", "发送前丢弃过时复读");
        return Ok(());
    }
    // 复读的接力条件本身就是一份时效条件；其余调用方（搭话）自己带一份来。
    let freshness = packet
        .repeat_guard
        .as_ref()
        .map(|guard| Freshness {
            message_id: guard.message_id.clone(),
            expires_at: guard.expires_at,
        })
        .or_else(|| packet.freshness.clone());
    if let Some(freshness) = freshness
        && bot.adapter == "satori-qq"
    {
        params["satori_qq"] = json!({
            "if_latest_message_id": freshness.message_id,
            "expires_at": freshness.expires_at,
        });
    }
    let created: Vec<Value> = client.post(&bot, "message.create", params).await?;
    if !created.is_empty() {
        plugins::repeater::confirm_send(ctx, packet);
    }
    let ids: Vec<String> = created
        .iter()
        .map(|message| raw_id(message.get("id")))
        .filter(|id| !id.is_empty())
        .collect();
    *packet
        .receipt_message_ids
        .lock()
        .map_err(|_| "发送回执锁已损坏")? = ids.clone();
    plugins::recorder::record_sent(ctx, &bot, packet, &channel_id, &ids).await;
    plugins::recall::record_sent(ctx, writer, packet, &ids).await;
    Ok(())
}

/// 私聊频道：`(connection_key, user_id) → channel.id`。
fn direct_channels()
-> std::sync::MutexGuard<'static, std::collections::HashMap<(String, String), String>> {
    static CHANNELS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<(String, String), String>>,
    > = std::sync::OnceLock::new();
    CHANNELS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 别人发来的私聊顺手记下它的频道；自己发的不记（那是对方的频道，不是自己的）。
fn note_direct_channel(client: &SatoriClient, event: &Event) {
    if event.get_str("message_type") != Some("private") {
        return;
    }
    let (Some(user), Some(channel)) = (event.get_str("user_id"), event.get_str("channel_id"))
    else {
        return;
    };
    if user.is_empty() || channel.is_empty() || Some(user) == event.get_str("self_id") {
        return;
    }
    direct_channels().insert(
        (client.connection_key().to_string(), user.to_string()),
        channel.to_string(),
    );
}

/// Satori 里消息发往频道；私聊要先知道和这个人的私聊频道。
///
/// 回话时触发事件本身就在那个频道里（包括自己在「文件传输助手」这类频道里发的指令），
/// 直接用；否则查缓存，再不行问实现端 `user.channel.create`。
async fn direct_channel(
    client: &SatoriClient,
    bot: &BotStatus,
    original: Option<&Event>,
    user_id: &str,
) -> Result<String, BotError> {
    if let Some(event) = original
        && event.get_str("message_type") == Some("private")
        && let Some(channel) = event.get_str("channel_id").filter(|id| !id.is_empty())
        && [Some(user_id), event.get_str("self_id")].contains(&event.get_str("user_id"))
    {
        return Ok(channel.to_string());
    }
    let key = (client.connection_key().to_string(), user_id.to_string());
    if let Some(channel) = direct_channels().get(&key) {
        return Ok(channel.clone());
    }
    let channel: Value = client
        .post(bot, "user.channel.create", json!({ "user_id": user_id }))
        .await?;
    let id = raw_id(channel.get("id"));
    if id.is_empty() {
        return Err(format!("user.channel.create 没给出频道：{channel}").into());
    }
    direct_channels().insert(key, id.clone());
    Ok(id)
}

fn normalize_event(
    body: &Value,
    bot: &BotStatus,
    proxy: &message::ResourceProxy,
) -> Result<Event, BotError> {
    let event_type = body.get("type").and_then(Value::as_str).unwrap_or("");
    let timestamp = body
        .get("timestamp")
        .and_then(Value::as_i64)
        .unwrap_or_default()
        / 1000;
    let guild = body.get("guild").unwrap_or(&Value::Null);
    let channel = body.get("channel").unwrap_or(&Value::Null);
    let user = body.get("user").unwrap_or(&Value::Null);
    let member = body.get("member").unwrap_or(&Value::Null);
    let guild_id = raw_id(guild.get("id"));
    let group_id = if !guild_id.is_empty() {
        guild_id
    } else if channel.get("type").and_then(Value::as_i64) == Some(0) {
        raw_id(channel.get("id"))
    } else {
        String::new()
    };
    // satori-qq 给 QQ 客户端手发的消息挂虚拟作者 `qq-client:{uin}`，真实账号在扩展字段里。
    let user_id = body
        .pointer("/satori_qq/actual_user_id")
        .map(|id| raw_id(Some(id)))
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| raw_id(user.get("id").or_else(|| member.pointer("/user/id"))));
    // 协议规定每个事件都自带 login 资源，多登录场景下它才是这条事件的归属账号；
    // 缺失时退回当前记录的登录号。
    let self_id = Some(raw_id(body.pointer("/login/user/id")))
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| bot.self_id());
    let mut out = json!({
        "time": timestamp,
        "self_id": self_id,
        "satori_type": event_type,
        "channel_id": raw_id(channel.get("id")),
        "_satori": body,
    });

    if event_type == "message-created" {
        let message = body.get("message").unwrap_or(&Value::Null);
        let content = message.get("content").and_then(Value::as_str).unwrap_or("");
        let chain = message::from_content_with(content, proxy);
        let raw_message = chain
            .0
            .iter()
            .filter(|segment| segment.type_ == "text")
            .filter_map(|segment| segment.data.get("text").and_then(|value| value.as_str()))
            .collect::<String>();
        let group = !group_id.is_empty();
        out["post_type"] = json!("message");
        out["message_type"] = json!(if group { "group" } else { "private" });
        out["sub_type"] = json!(if group { "normal" } else { "friend" });
        if group {
            let pick = |key: &str| {
                guild
                    .get(key)
                    .or_else(|| channel.get(key))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            out["group_id"] = json!(group_id);
            out["group_name"] = json!(pick("name"));
            out["group_avatar"] = json!(pick("avatar"));
        }
        out["user_id"] = json!(user_id);
        out["manual_self"] = json!(
            body.pointer("/satori_qq/manual_self")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        );
        out["message_id"] = json!(raw_id(message.get("id")));
        out["raw_message"] = json!(raw_message);
        out["message"] = serde_json::to_value(chain)?;
        let role = member
            .get("roles")
            .and_then(Value::as_array)
            .and_then(|roles| roles.first())
            .and_then(|role| role.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("member");
        out["sender"] = json!({
            "user_id": user_id,
            "nickname": user.get("name").and_then(Value::as_str).unwrap_or(""),
            "card": member.get("nick").or_else(|| member.get("name")).and_then(Value::as_str).unwrap_or(""),
            "avatar": member.get("avatar").or_else(|| user.get("avatar")).and_then(Value::as_str).unwrap_or(""),
            "role": role,
        });
    } else {
        let (post_type, notice_type, request_type, sub_type) = match event_type {
            "message-deleted" => ("notice", "message_recall", "", ""),
            "guild-member-added" => ("notice", "group_increase", "", "approve"),
            "guild-member-removed" => ("notice", "group_decrease", "", "leave"),
            "guild-member-updated"
                if body.get("_type").and_then(Value::as_str) == Some("satori-qq/mute") =>
            {
                ("notice", "group_ban", "", "ban")
            }
            "guild-member-updated" => ("notice", "group_member_update", "", ""),
            "friend-request" => ("request", "", "friend", ""),
            "guild-request" => ("request", "", "group", "invite"),
            "guild-member-request" => ("request", "", "group", "add"),
            "internal" if body.get("_type").and_then(Value::as_str) == Some("satori-qq/poke") => {
                ("notice", "notify", "", "poke")
            }
            _ => ("satori", "", "", ""),
        };
        // 禁言与解禁共用 guild-member-updated，靠 _data.duration 区分；实现端给的是毫秒。
        let ban_duration = body
            .pointer("/_data/duration")
            .and_then(value_id)
            .map(|value| value / 1000);
        let sub_type = match (notice_type, ban_duration) {
            ("group_ban", Some(0)) => "lift_ban",
            _ => sub_type,
        };
        out["post_type"] = json!(post_type);
        out["notice_type"] = json!(notice_type);
        out["request_type"] = json!(request_type);
        out["sub_type"] = json!(sub_type);
        if !group_id.is_empty() {
            out["group_id"] = json!(group_id);
        }
        out["user_id"] = json!(user_id);
        // 申请类事件的 message.id 是审批 flag。
        let message_id = raw_id(body.pointer("/message/id"));
        out["message_id"] = json!(message_id);
        if post_type == "request" {
            out["flag"] = json!(message_id);
            out["comment"] = json!(
                body.pointer("/message/content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            );
        }
        if let Some(duration) = ban_duration {
            out["duration"] = json!(duration);
        }
        out["operator_id"] = json!(raw_id(body.pointer("/operator/id")));
        if let Some(data) = body.get("_data") {
            out["satori_data"] = data.clone();
        }
    }

    let bytes = serde_json::to_vec(&out)?;
    let mut bytes = bytes;
    Ok(simd_json::to_owned_value(&mut bytes)?)
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// 原样保留实现端给的 ID：申请类事件的 `message.id` 是审批 flag，不是数字。
fn raw_id(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(value) => value_id(value)
            .map(|value| value.to_string())
            .unwrap_or_default(),
        None => String::new(),
    }
}

fn value_id(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn cursor_deduplicates_replay_and_resets_for_a_new_server_session() {
        let mut cursor = EventCursor::default();
        let ready = json!({"body":{"satori_qq":{"session_id":"one"}}});
        cursor.ready(&ready, "red", "10000");
        assert!(cursor.accept(&json!({"type":"message-created","sn":42})));
        assert!(!cursor.accept(&json!({"type":"message-created","sn":42})));
        assert!(cursor.accept(&json!({"type":"login-updated","sn":99})));
        assert_eq!(cursor.sn, Some(42));
        cursor.ready(&ready, "red", "10000");
        assert!(!cursor.accept(&json!({"type":"message-created","sn":41})));
        cursor.ready(
            &json!({"body":{"satori_qq":{"session_id":"two"}}}),
            "red",
            "10000",
        );
        assert!(cursor.accept(&json!({"type":"message-created","sn":1})));
    }

    #[test]
    fn foreign_login_does_not_advance_the_local_cursor() {
        let bot = test_bot();
        let mut cursor = EventCursor::default();
        cursor.ready(
            &json!({"body":{"satori_qq":{"session_id":"one"}}}),
            "red",
            "10000",
        );
        let foreign = json!({"type":"message-created","sn":900,"login":{"platform":"red","user":{"id":"20000"}}});
        if belongs_to_login(&foreign, &bot) {
            cursor.accept(&foreign);
        }
        let local = json!({"type":"message-created","sn":100,"login":{"platform":"red","user":{"id":"10000"}}});
        assert!(belongs_to_login(&local, &bot) && cursor.accept(&local));
        assert_eq!(cursor.sn, Some(100));
    }

    #[test]
    fn other_logins_cannot_enter_this_bots_pipeline() {
        let bot = test_bot();
        assert!(belongs_to_login(
            &json!({"login":{"platform":"red","user":{"id":"10000"}}}),
            &bot
        ));
        assert!(!belongs_to_login(
            &json!({"login":{"platform":"red","user":{"id":"20000"}}}),
            &bot
        ));
        assert!(!belongs_to_login(
            &json!({"login":{"platform":"other","user":{"id":"10000"}}}),
            &bot
        ));
    }

    #[tokio::test]
    async fn qq_helpers_preserve_ids_routes_and_structured_errors() {
        let (endpoint, mut seen, server) = scripted_peer(vec![
            (200, r#"{"message_id":"7837409278651234567","data":[{"emoji_id":"76","count":2,"self":true}],"source":"kernel_cache","observed_at":123}"#.into()),
            (200, "{}".into()),
            (200, "{}".into()),
            (200, "{}".into()),
            (404, r#"{"message":"unavailable","code":"removed_action"}"#.into()),
        ]).await;
        let (ctx, writer) = bare_context(&endpoint).await;
        let summary = qq::reactions(&ctx, &writer, "123", "7837409278651234567")
            .await
            .unwrap();
        assert_eq!(summary.message_id, "7837409278651234567");
        assert!(summary.data[0].by_self);
        qq::poke(&ctx, &writer, "private:42", "42").await.unwrap();
        qq::typing(&ctx, &writer, "123").await.unwrap();
        qq::mark_read(&ctx, &writer, "123").await.unwrap();
        let error = qq::clear_reactions(&ctx, &writer, "123", &summary.message_id)
            .await
            .unwrap_err();
        let typed = error.downcast_ref::<SatoriApiError>().unwrap();
        assert_eq!(typed.status, 404);
        assert_eq!(typed.code.as_deref(), Some("removed_action"));
        let first = seen.try_recv().unwrap();
        assert!(first.starts_with("/v1/internal/reaction_summary "));
        assert!(first.contains("7837409278651234567"));
        assert!(seen.try_recv().unwrap().starts_with("/v1/internal/poke "));
        assert!(seen.try_recv().unwrap().starts_with("/v1/internal/typing "));
        assert!(
            seen.try_recv()
                .unwrap()
                .starts_with("/v1/internal/mark_read ")
        );
        assert!(
            seen.try_recv()
                .unwrap()
                .starts_with("/v1/internal/reaction_clear ")
        );
        server.abort();
    }

    #[test]
    fn rpc_distinguishes_empty_success_kernel_failure_and_absent_payload() {
        assert!(decode_response("message.delete", b" ").unwrap().is_null());
        assert!(
            decode_response(
                "internal/like",
                br#"{"ok":false,"result":"code=2 system error"}"#
            )
            .is_err()
        );
        assert!(decode_response("message.get", b"not-json").is_err());
    }

    #[test]
    fn member_profiles_are_not_mutes_and_flat_identity_is_preserved() {
        let bot = BotStatus {
            adapter: "satori-qq".into(),
            platform: "red".into(),
            login_user: Default::default(),
        };
        let event = normalize_event(&json!({"type":"guild-member-updated","login":{"user":{"id":"10000"}},"channel":{"id":"123","type":0},"member":{"user":{"id":"42"},"nick":"新名片"}}), &bot, &Default::default()).unwrap();
        assert_eq!(event.get_str("notice_type"), Some("group_member_update"));
        assert_eq!(event.get_str("self_id"), Some("10000"));
        assert_eq!(event.get_str("user_id"), Some("42"));
        assert_eq!(event.get_str("channel_id"), Some("123"));
        assert_eq!(event.get_i64("duration"), None);
    }

    // A local HTTP peer exercises the actual BeforeSend -> message.create path.
    // It never contacts QQ or sends messages to a real chat.
    async fn repeat_fixture() -> (
        Context,
        LockedWriter,
        tokio::sync::mpsc::UnboundedReceiver<Value>,
        tokio::task::JoinHandle<()>,
    ) {
        http_fixture(None).await
    }

    pub(crate) async fn http_fixture(
        reply: Option<(&'static str, &'static str)>,
    ) -> (
        Context,
        LockedWriter,
        tokio::sync::mpsc::UnboundedReceiver<Value>,
        tokio::task::JoinHandle<()>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut bytes = Vec::new();
                let mut buf = [0; 4096];
                loop {
                    let n = stream.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(start) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..start]).to_ascii_lowercase();
                        let len: usize = headers
                            .lines()
                            .find_map(|s| s.strip_prefix("content-length:"))
                            .unwrap()
                            .trim()
                            .parse()
                            .unwrap();
                        if bytes.len() < start + 4 + len {
                            continue;
                        }
                        tx.send(
                            serde_json::from_slice(&bytes[start + 4..start + 4 + len]).unwrap(),
                        )
                        .unwrap();
                        // 回包带上实际收到的选择器，供测试核对出站请求的身份。
                        let selector = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("satori-user-id:"))
                            .map(str::trim)
                            .unwrap_or_default();
                        let body =
                            format!(r#"[{{"id":"bot-reply","login_user_id":"{selector}"}}]"#);
                        let (status, body) = reply.unwrap_or(("200 OK", &body));
                        let response = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        stream.write_all(response.as_bytes()).await.unwrap();
                        break;
                    }
                }
            }
        });
        let mut config = AppConfig::default();
        config.plugins.insert(
            "repeater".into(),
            crate::config::build_config(plugins::repeater::RepeaterConfig {
                cooldown_seconds: 0,
                max_delay_ms: 60_000,
                ..Default::default()
            }),
        );
        let ctx = Context {
            event: EventType::Init,
            config: Arc::new(RwLock::new(config)),
            config_save_lock: Arc::new(AsyncMutex::new(())),
            db: sea_orm::Database::connect("sqlite::memory:").await.unwrap(),
            scheduler: Arc::new(Scheduler::new()),
            matcher: Arc::new(Matcher::new()),
            config_path: Arc::from("unused-repeater-test.toml"),
            bot: Arc::new(BotStatus {
                adapter: "satori-qq".into(),
                platform: "red".into(),
                login_user: LoginUser {
                    id: "10000".into(),
                    ..Default::default()
                }
                .into(),
            }),
        };
        (ctx, Arc::new(SatoriClient::new(endpoint, None)), rx, server)
    }

    fn incoming(ctx: &Context, text: &str, user: i64, id: &str, timestamp: u64) -> Event {
        normalize_event(
            &json!({
                "type": "message-created", "timestamp": timestamp,
                "channel": {"id": "123", "type": 0},
                "user": {"id": user.to_string()},
                "message": {"id": id, "content": text, "created_at": timestamp},
            }),
            &ctx.bot,
            &Default::default(),
        )
        .unwrap()
    }

    fn receive(
        ctx: &Context,
        writer: &LockedWriter,
        event: Event,
    ) -> BoxFuture<'static, Result<(), BotError>> {
        process_event(
            event,
            writer.clone(),
            ctx.config.clone(),
            ctx.db.clone(),
            ctx.scheduler.clone(),
            ctx.config_save_lock.clone(),
            ctx.config_path.clone(),
            ctx.matcher.clone(),
            ctx.bot.clone(),
        )
    }

    fn timestamp_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    #[tokio::test]
    async fn repeat_is_discarded_when_later_message_runs_first() {
        let (ctx, writer, mut sent, server) = repeat_fixture().await;
        let now = timestamp_ms();
        // Prepare all events in WS receive order, then deliberately execute tasks out of order.
        let first = receive(&ctx, &writer, incoming(&ctx, "哈哈", 1, "1", now));
        let repeat = receive(&ctx, &writer, incoming(&ctx, "哈哈", 2, "2", now));
        let third = receive(&ctx, &writer, incoming(&ctx, "是吧还可以吧", 3, "3", now));
        third.await.unwrap();
        repeat.await.unwrap();
        first.await.unwrap();
        assert!(sent.try_recv().is_err(), "stale 哈哈 must never reach HTTP");
        // The newer chain must remain usable after the old task completes.
        receive(&ctx, &writer, incoming(&ctx, "是吧还可以吧", 4, "4", now))
            .await
            .unwrap();
        assert_eq!(sent.try_recv().unwrap()["content"], "是吧还可以吧");
        server.abort();
    }

    /// 时效锚点必须和实现端的记账一致：它记的是「推送给本应用的每一条群消息」，
    /// 包括被指令消费掉、被过滤器拦掉、根本没走到某个插件的那些。所以这笔账记在
    /// 适配器层，任何插件都能取到同一个锚点。
    #[test]
    fn the_freshness_anchor_follows_every_inbound_group_message() {
        let group = "freshness-anchor-test";
        let inbound = |id: &str, kind: &str| {
            simd_json::serde::to_owned_value(json!({
                "satori_type": kind,
                "group_id": group,
                "message_id": id,
            }))
            .unwrap()
        };
        assert!(freshness_for(group, Duration::from_secs(20)).is_none());
        note_inbound(&inbound("100", "message-created"));
        note_inbound(&inbound("101", "message-created"));
        let anchor = freshness_for(group, Duration::from_secs(20)).unwrap();
        assert_eq!(anchor.message_id, "101");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        assert!(anchor.expires_at > now && anchor.expires_at <= now + 20_000);
        // 撤回、戳一戳这类事件不是新消息，锚点不动。
        note_inbound(&inbound("102", "message-deleted"));
        assert_eq!(
            freshness_for(group, Duration::from_secs(20))
                .unwrap()
                .message_id,
            "101"
        );
        // 私聊没有群号，不参与这笔记账。
        note_inbound(
            &simd_json::serde::to_owned_value(
                json!({"satori_type":"message-created","message_id":"103"}),
            )
            .unwrap(),
        );
        assert_eq!(
            freshness_for(group, Duration::from_secs(20))
                .unwrap()
                .message_id,
            "101"
        );
        // 窗口为 0 就是关掉这层条件，回到从前的无条件发送。
        assert!(freshness_for(group, Duration::ZERO).is_none());
    }

    #[tokio::test]
    async fn normal_repeat_sends_once_with_server_freshness_condition() {
        let (ctx, writer, mut sent, server) = repeat_fixture().await;
        let now = timestamp_ms();
        receive(&ctx, &writer, incoming(&ctx, "哈哈", 1, "1", now))
            .await
            .unwrap();
        receive(&ctx, &writer, incoming(&ctx, "哈哈", 2, "2", now))
            .await
            .unwrap();
        receive(&ctx, &writer, incoming(&ctx, "哈哈", 3, "3", now))
            .await
            .unwrap();
        let request = sent.try_recv().unwrap();
        assert_eq!(request["content"], "哈哈");
        assert_eq!(request["satori_qq"]["if_latest_message_id"], "2");
        assert_eq!(request["satori_qq"]["expires_at"], now + 60_000);
        // 自身回显、其他插件的回复以及继续接力均不能使旧内容重发。
        for (user, text, id) in [
            (10000, "哈哈", "4"),
            (4, "哈哈", "5"),
            (10000, "机器人回复", "6"),
            (5, "哈哈", "7"),
            (6, "哈哈", "8"),
        ] {
            receive(&ctx, &writer, incoming(&ctx, text, user, id, now))
                .await
                .unwrap();
        }
        assert!(sent.try_recv().is_err());
        server.abort();
    }

    #[tokio::test]
    async fn confirmed_repeat_stays_suppressed_across_chain_changes() {
        let (ctx, writer, mut sent, server) = repeat_fixture().await;
        let now = timestamp_ms();
        for (index, text) in [
            "哈哈",
            "哈哈",
            "插话",
            "哈哈",
            "哈哈",
            "/help",
            "哈哈",
            "哈哈",
            "新内容",
            "新内容",
            "哈哈",
            "哈哈",
        ]
        .into_iter()
        .enumerate()
        {
            receive(
                &ctx,
                &writer,
                incoming(&ctx, text, index as i64 + 1, &index.to_string(), now),
            )
            .await
            .unwrap();
        }
        assert_eq!(sent.try_recv().unwrap()["content"], "哈哈");
        assert_eq!(sent.try_recv().unwrap()["content"], "新内容");
        assert!(
            sent.try_recv().is_err(),
            "confirmed content must only send once"
        );
        server.abort();
    }

    #[tokio::test]
    async fn cancelled_repeat_can_trigger_in_a_new_chain() {
        let (ctx, writer, mut sent, server) = repeat_fixture().await;
        let now = timestamp_ms();
        receive(&ctx, &writer, incoming(&ctx, "哈哈", 1, "1", now))
            .await
            .unwrap();
        let pending = receive(&ctx, &writer, incoming(&ctx, "哈哈", 2, "2", now));
        receive(&ctx, &writer, incoming(&ctx, "插话", 3, "3", now))
            .await
            .unwrap();
        pending.await.unwrap();
        assert!(sent.try_recv().is_err());
        for (user, id) in [(4, "4"), (5, "5")] {
            receive(&ctx, &writer, incoming(&ctx, "哈哈", user, id, now))
                .await
                .unwrap();
        }
        assert_eq!(sent.try_recv().unwrap()["content"], "哈哈");
        assert!(sent.try_recv().is_err());
        server.abort();
    }

    #[tokio::test]
    async fn replayed_old_messages_do_not_trigger_a_repeat() {
        let (ctx, writer, mut sent, server) = repeat_fixture().await;
        let now = timestamp_ms();
        for (user, id) in [(1, "1"), (2, "2")] {
            receive(
                &ctx,
                &writer,
                incoming(&ctx, "哈哈", user, id, now - 120_000),
            )
            .await
            .unwrap();
        }
        receive(&ctx, &writer, incoming(&ctx, "哈哈", 3, "3", now))
            .await
            .unwrap();
        assert!(
            sent.try_recv().is_err(),
            "history must not count towards a fresh chain"
        );
        server.abort();
    }

    #[tokio::test]
    async fn command_consumption_cannot_leave_a_pending_repeat_alive() {
        let (ctx, writer, mut sent, server) = repeat_fixture().await;
        let now = timestamp_ms();
        receive(&ctx, &writer, incoming(&ctx, "哈哈", 1, "1", now))
            .await
            .unwrap();
        let repeat = receive(&ctx, &writer, incoming(&ctx, "哈哈", 2, "2", now));
        receive(&ctx, &writer, incoming(&ctx, "/help", 3, "3", now))
            .await
            .unwrap();
        repeat.await.unwrap();
        assert!(sent.try_recv().is_err());
        server.abort();
    }

    #[tokio::test]
    async fn interactive_input_interrupts_pending_repeat_without_blocking() {
        let (ctx, writer, mut sent, server) = repeat_fixture().await;
        let now = timestamp_ms();
        receive(&ctx, &writer, incoming(&ctx, "哈哈", 1, "1", now))
            .await
            .unwrap();
        let repeat = receive(&ctx, &writer, incoming(&ctx, "哈哈", 2, "2", now));
        let input = ctx.wait_input(Some("123"), Some("3"), Duration::from_secs(5));
        tokio::pin!(input);
        assert!(futures_util::poll!(&mut input).is_pending());
        receive(&ctx, &writer, incoming(&ctx, "哈哈", 3, "3", now))
            .await
            .unwrap();
        assert!(
            input.await.is_some(),
            "interactive waiter must still receive its message"
        );
        repeat.await.unwrap();
        assert!(
            sent.try_recv().is_err(),
            "even same-text interactive input consumes the chain"
        );
        server.abort();
    }

    #[tokio::test]
    async fn interrupt_phrase_survives_its_own_before_send_hooks() {
        let (ctx, writer, mut sent, server) = repeat_fixture().await;
        {
            let mut config = ctx.config.write().unwrap();
            let repeater = config
                .plugins
                .get_mut("repeater")
                .unwrap()
                .as_table_mut()
                .unwrap();
            repeater.insert("interrupt_probability".into(), toml::Value::Float(1.0));
            repeater.insert(
                "interrupt_texts".into(),
                toml::Value::Array(vec![toml::Value::String("打断".into())]),
            );
        }
        let now = timestamp_ms();
        receive(&ctx, &writer, incoming(&ctx, "哈哈", 1, "1", now))
            .await
            .unwrap();
        receive(&ctx, &writer, incoming(&ctx, "哈哈", 2, "2", now))
            .await
            .unwrap();
        assert_eq!(sent.try_recv().unwrap()["content"], "打断");
        receive(&ctx, &writer, incoming(&ctx, "打断", 3, "3", now))
            .await
            .unwrap();
        receive(&ctx, &writer, incoming(&ctx, "打断", 4, "4", now))
            .await
            .unwrap();
        assert!(
            sent.try_recv().is_err(),
            "bot must not repeat its own interruption"
        );
        for (user, id) in [(5, "5"), (6, "6")] {
            receive(&ctx, &writer, incoming(&ctx, "哈哈", user, id, now))
                .await
                .unwrap();
        }
        assert!(
            sent.try_recv().is_err(),
            "continuing the original chain must not trigger another interruption"
        );
        server.abort();
    }

    // login-updated 换了账号，共享账号要跟着换：出站请求的选择器与插件读到的自我识别
    // 都取自它；账号未知的快照不能把它清掉。
    #[tokio::test]
    async fn login_update_cannot_retarget_existing_tasks() {
        async fn selector(ctx: &Context, writer: &LockedWriter) -> String {
            let created: Vec<Value> = writer
                .call(
                    ctx,
                    "message.create",
                    json!({"channel_id": "123", "content": "x"}),
                )
                .await
                .unwrap();
            raw_id(created[0].get("login_user_id"))
        }

        let (ctx, writer, _sent, server) = repeat_fixture().await;
        assert_eq!(selector(&ctx, &writer).await, "10000");
        assert_eq!(ctx.bot.login_user.get().id, "10000");
        apply_login_update(
            &json!({"type": "login-updated", "login": {"user": {"id": "20000"}}}),
            &ctx.bot,
        );
        assert_eq!(selector(&ctx, &writer).await, "10000");
        assert_eq!(ctx.bot.login_user.get().id, "10000");
        // 账号尚不可知的快照给不出账号，保留现有的。
        apply_login_update(
            &json!({"type": "login-updated", "login": {"user": {}}}),
            &ctx.bot,
        );
        assert_eq!(selector(&ctx, &writer).await, "10000");
        server.abort();
    }

    #[test]
    fn normalizes_message_event() {
        let bot = BotStatus {
            adapter: "satori-qq".to_string(),
            platform: "red".to_string(),
            login_user: LoginUser {
                id: "10000".to_string(),
                ..Default::default()
            }
            .into(),
        };
        let event = json!({
            "type": "message-created",
            "timestamp": 1_700_000_000_000i64,
            "guild": {"id": "123", "name": "test"},
            "channel": {"id": "123"},
            "user": {"id": "42", "name": "Alice"},
            "member": {"nick": "A", "roles": [{"id": "admin"}]},
            "message": {"id": "7000000000000000000", "content": "hi <at id=\"7\"/>"}
        });
        let normalized = normalize_event(&event, &bot, &Default::default()).unwrap();
        assert_eq!(normalized.get_str("post_type"), Some("message"));
        assert_eq!(normalized.get_str("group_id"), Some("123"));
        assert_eq!(
            normalized.get_str("message_id"),
            Some("7000000000000000000")
        );
        assert_eq!(normalized.get_str("raw_message"), Some("hi "));
    }

    /// satori-wx 的事件没有 guild，群是 `type: 0` 的 `<数字>@chatroom` 频道，人是 wxid；
    /// login 里只有 sn。ID 原样是字符串，和 QQ 群消息走同一条路。
    #[test]
    fn wechat_string_ids_pass_through_unchanged() {
        let bot = BotStatus {
            adapter: "satori-wx".to_string(),
            platform: "wechat".to_string(),
            login_user: LoginUser {
                id: "wxid_self".to_string(),
                ..Default::default()
            }
            .into(),
        };
        let event = json!({
            "type": "message-created",
            "timestamp": 1_700_000_000_000i64,
            "login": {"sn": 1},
            "channel": {"id": "45123456789@chatroom", "type": 0},
            "user": {"id": "wxid_alice"},
            "message": {"id": "3292", "content": "/help &amp; more"}
        });
        assert_eq!(login_user_of(&json!({"id": "wxid_self"})).id, "wxid_self");
        let normalized = normalize_event(&event, &bot, &Default::default()).unwrap();
        assert_eq!(normalized.get_str("message_type"), Some("group"));
        assert_eq!(normalized.get_str("group_id"), Some("45123456789@chatroom"));
        assert_eq!(
            normalized.get_str("channel_id"),
            Some("45123456789@chatroom")
        );
        assert_eq!(normalized.get_str("user_id"), Some("wxid_alice"));
        assert_eq!(normalized.get_str("self_id"), Some("wxid_self"));
        assert_eq!(normalized.get_str("message_id"), Some("3292"));
        assert_eq!(normalized.get_str("raw_message"), Some("/help & more"));
    }

    /// 定时推送拿的是先连上的那条连接；目标在另一条连接上见过，就交给那条。
    #[test]
    fn calls_are_routed_to_the_connection_that_owns_the_target() {
        let qq = Arc::new(SatoriClient::new("http://qq.route.test".into(), None));
        let wx = Arc::new(SatoriClient::new("http://wx.route.test".into(), None));
        let bot = |platform: &str| {
            Arc::new(BotStatus {
                adapter: String::new(),
                platform: platform.to_string(),
                login_user: Default::default(),
            })
        };
        register_route(qq.clone(), bot("red"));
        register_route(wx.clone(), bot("wechat"));
        note_owner(&qq, ["route-test-qq-group"]);
        note_owner(&wx, ["route-test@chatroom", "route-test-wxid"]);

        let to_wx = json!({"channel_id": "route-test@chatroom", "content": "x"});
        let route = route_for(&qq, &to_wx).expect("微信群的调用应转给微信那条连接");
        assert_eq!(route.client.connection_key(), wx.connection_key());
        assert_eq!(route.bot.platform, "wechat");
        // 目标就在当前连接上，或者没见过，都不转。
        assert!(route_for(&qq, &json!({"guild_id": "route-test-qq-group"})).is_none());
        assert!(route_for(&qq, &json!({"channel_id": "never-seen"})).is_none());
        assert!(route_for(&wx, &json!({"user_id": "route-test-wxid"})).is_none());
    }

    #[test]
    fn private_message_has_no_group_id() {
        let bot = BotStatus {
            adapter: "satori-qq".to_string(),
            platform: "red".to_string(),
            login_user: LoginUser {
                id: "10000".to_string(),
                ..Default::default()
            }
            .into(),
        };
        let event = json!({
            "type": "message-created",
            "timestamp": 1_700_000_000_000i64,
            "channel": {"id": "private:42", "type": 1},
            "user": {"id": "42", "name": "Alice"},
            "message": {"id": "7000000000000000000", "content": "hello"}
        });
        let normalized = normalize_event(&event, &bot, &Default::default()).unwrap();
        assert_eq!(normalized.get_str("message_type"), Some("private"));
        assert_eq!(normalized.get_str("group_id"), None);
        assert_eq!(normalized.get_str("channel_id"), Some("private:42"));
    }

    /// QQ 客户端手发的消息带虚拟作者 `qq-client:{uin}`，真实身份在 satori_qq 扩展里。
    #[test]
    fn manual_self_message_keeps_the_real_author() {
        let bot = test_bot();
        let event = json!({
            "type": "message-created",
            "timestamp": 1_700_000_000_000i64,
            "guild": {"id": "123", "name": "test"},
            "channel": {"id": "123", "type": 0},
            "user": {"id": "qq-client:10000", "name": "我"},
            "satori_qq": {"manual_self": true, "actual_user_id": "10000"},
            "message": {"id": "7000000000000000000", "content": "自己发的"}
        });
        let normalized = normalize_event(&event, &bot, &Default::default()).unwrap();
        assert_eq!(normalized.get_str("user_id"), Some("10000"));
        assert_eq!(normalized.get_str("group_id"), Some("123"));
        assert_eq!(normalized.get_bool("manual_self"), Some(true));
    }

    /// 加群申请的 `message.id` 是审批 flag，不是数字 ID，必须原样留住。
    #[test]
    fn group_request_keeps_the_approval_flag() {
        let bot = test_bot();
        let event = json!({
            "type": "guild-member-request",
            "timestamp": 1_700_000_000_000i64,
            "guild": {"id": "123"},
            "channel": {"id": "123", "type": 0},
            "user": {"id": "42"},
            "message": {"id": "flag-abc123", "content": "求进群"}
        });
        let normalized = normalize_event(&event, &bot, &Default::default()).unwrap();
        assert_eq!(normalized.get_str("post_type"), Some("request"));
        assert_eq!(normalized.get_str("flag"), Some("flag-abc123"));
        assert_eq!(normalized.get_str("comment"), Some("求进群"));
    }

    /// 禁言与解禁共用一个 Satori 事件，靠毫秒 duration 区分。
    #[test]
    fn lift_ban_is_told_apart_from_ban() {
        let bot = test_bot();
        let mute = json!({
            "type": "guild-member-updated",
            "timestamp": 1_700_000_000_000i64,
            "guild": {"id": "123"},
            "channel": {"id": "123", "type": 0},
            "user": {"id": "42"},
            "operator": {"id": "7"},
            "_type": "satori-qq/mute",
            "_data": {"duration": 600_000}
        });
        let normalized = normalize_event(&mute, &bot, &Default::default()).unwrap();
        assert_eq!(normalized.get_str("notice_type"), Some("group_ban"));
        assert_eq!(normalized.get_str("sub_type"), Some("ban"));
        assert_eq!(normalized.get_i64("duration"), Some(600));

        let lift = json!({
            "type": "guild-member-updated",
            "timestamp": 1_700_000_000_000i64,
            "guild": {"id": "123"},
            "channel": {"id": "123", "type": 0},
            "user": {"id": "42"},
            "operator": {"id": "7"},
            "_type": "satori-qq/mute",
            "_data": {"duration": 0}
        });
        let normalized = normalize_event(&lift, &bot, &Default::default()).unwrap();
        assert_eq!(normalized.get_str("sub_type"), Some("lift_ban"));
        assert_eq!(normalized.get_i64("duration"), Some(0));
    }

    fn test_bot() -> BotStatus {
        BotStatus {
            adapter: "satori-qq".to_string(),
            platform: "red".to_string(),
            login_user: LoginUser {
                id: "10000".to_string(),
                ..Default::default()
            }
            .into(),
        }
    }

    /// 只够发 HTTP 请求的最小上下文：不装插件，也不连真库。
    async fn bare_context(endpoint: &str) -> (Context, LockedWriter) {
        let ctx = Context {
            event: EventType::Init,
            config: Arc::new(RwLock::new(AppConfig::default())),
            config_save_lock: Arc::new(AsyncMutex::new(())),
            db: sea_orm::Database::connect("sqlite::memory:").await.unwrap(),
            scheduler: Arc::new(Scheduler::new()),
            matcher: Arc::new(Matcher::new()),
            config_path: Arc::from("unused-compat-test.toml"),
            bot: Arc::new(test_bot()),
        };
        let writer: LockedWriter = Arc::new(SatoriClient::new(endpoint.to_string(), None));
        (ctx, writer)
    }

    /// 按脚本回包的本地对端：把每个请求记成 `路径 请求体`，再按顺序吐准备好的响应。
    ///
    /// 用来钉住 acumen 依赖的实现端形状——列表信封与错误体是实现端单方面改一下就会
    /// 静默对不上的东西，本地跑一遍比事后到群里找症状便宜。
    async fn scripted_peer(
        replies: Vec<(u16, String)>,
    ) -> (
        String,
        tokio::sync::mpsc::UnboundedReceiver<String>,
        tokio::task::JoinHandle<()>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            for (status, body) in replies {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut bytes = Vec::new();
                let mut buf = [0; 4096];
                while let Ok(n) = stream.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    let Some(start) = bytes.windows(4).position(|s| s == b"\r\n\r\n") else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&bytes[..start]).to_string();
                    let len: usize = head
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            if key.eq_ignore_ascii_case("content-length") {
                                value.trim().parse().ok()
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0);
                    if bytes.len() < start + 4 + len {
                        continue;
                    }
                    let path = head
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap_or_default()
                        .to_string();
                    let payload = String::from_utf8_lossy(&bytes[start + 4..start + 4 + len]);
                    let _ = tx.send(format!("{path} {payload}"));
                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    break;
                }
            }
        });
        (endpoint, rx, server)
    }

    /// 列表接口统一是 `{data, next}`，`next` 是下一次要带回去的分页令牌。
    ///
    /// satori-qq 0.8.9.38 起 `guild.list`、`guild.member.list`、`friend.list` 都是这个
    /// 信封；只读第一页会在群多的时候静默漏群，所以这里连翻三页钉一遍。
    #[tokio::test]
    async fn a_paged_list_is_followed_until_the_token_runs_out() {
        let (endpoint, mut seen, server) = scripted_peer(vec![
            (
                200,
                r#"{"data":[{"id":"1","name":"一群"}],"next":"1"}"#.into(),
            ),
            (
                200,
                r#"{"data":[{"id":"2","name":"二群"}],"next":"2"}"#.into(),
            ),
            (200, r#"{"data":[{"id":"3","name":"三群"}]}"#.into()),
        ])
        .await;
        let (ctx, writer) = bare_context(&endpoint).await;
        let groups = api::get_group_list(&ctx, writer, true).await.unwrap();
        assert_eq!(
            groups
                .iter()
                .map(|group| group.group_id.as_str())
                .collect::<Vec<_>>(),
            vec!["1", "2", "3"]
        );
        // 每次回包给的令牌原样带回去；最后一页没有 next，就不再追问。
        assert_eq!(seen.try_recv().unwrap(), "/v1/guild.list {}");
        assert_eq!(seen.try_recv().unwrap(), r#"/v1/guild.list {"next":"1"}"#);
        assert_eq!(seen.try_recv().unwrap(), r#"/v1/guild.list {"next":"2"}"#);
        assert!(seen.try_recv().is_err(), "没有 next 时不该再发一次请求");
        server.abort();
    }

    #[tokio::test]
    async fn repeated_list_cursor_is_an_error_not_an_infinite_loop() {
        let (endpoint, mut seen, server) = scripted_peer(vec![
            (200, r#"{"data":[{"id":"1"}],"next":"1"}"#.into()),
            (200, r#"{"data":[{"id":"2"}],"next":"1"}"#.into()),
        ])
        .await;
        let (ctx, writer) = bare_context(&endpoint).await;
        let error = api::get_group_list(&ctx, writer, false)
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("重复分页令牌"));
        assert!(seen.try_recv().is_ok());
        assert!(seen.try_recv().is_ok());
        assert!(seen.try_recv().is_err());
        server.abort();
    }

    #[tokio::test]
    async fn capped_list_does_not_return_partial_groups() {
        let replies = (0..64)
            .map(|i| {
                (
                    200,
                    format!(r#"{{"data":[{{"id":"{}"}}],"next":"{}"}}"#, i + 1, i + 1),
                )
            })
            .collect();
        let (endpoint, _seen, server) = scripted_peer(replies).await;
        let (ctx, writer) = bare_context(&endpoint).await;
        let error = api::get_group_list(&ctx, writer, false)
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("超过 64 页"));
        server.abort();
    }

    /// 实现端的错误回包是 JSON，`message` 要能取出来，而不是把整段原文糊在错误里。
    #[tokio::test]
    async fn a_json_error_body_surfaces_its_message() {
        let (endpoint, _seen, server) = scripted_peer(vec![(
            404,
            r#"{"message":"API not found: message.update"}"#.into(),
        )])
        .await;
        let (ctx, writer) = bare_context(&endpoint).await;
        let error = writer
            .call::<_, Value>(&ctx, "message.update", json!({}))
            .await
            .expect_err("404 不该被当成成功")
            .to_string();
        assert!(error.contains("API not found"), "{error}");
        // 实现端把 404 的响应体从纯文本改成 JSON 之后，取 message 的这条路必须仍然通。
        assert!(
            !error.contains("{\"message\""),
            "整段 JSON 原文不该出现在错误里：{error}"
        );
        server.abort();
    }

    #[tokio::test]
    async fn a_removed_action_carries_its_machine_readable_code() {
        // satori-qq 0.23.1 起，曾经有过、后来移除的动作在 404 体里带 code=removed_action。
        // 聊天层按这个 code 把能力记成不可用，所以它必须活着走到错误文案里。
        let (endpoint, _seen, server) = scripted_peer(vec![(
            404,
            r#"{"message":"internal/like 已移除（0.23.0起）：资料卡点赞走 QQ 的 WUP/Handler 通道，本实现端只走 JNI 层","code":"removed_action"}"#.into(),
        )])
        .await;
        let (ctx, writer) = bare_context(&endpoint).await;
        let error = writer
            .call::<_, Value>(&ctx, "internal/like", json!({"user_id":"42","times":1}))
            .await
            .expect_err("移除的动作不该被当成成功")
            .to_string();
        assert!(error.contains("removed_action"), "{error}");
        assert!(error.contains("[code=removed_action]"), "{error}");
        assert!(error.contains("已移除"), "{error}");
        server.abort();
    }
}

/// 图片没出成时的等价文本；分段限制单条长度，长内容不撞实现端的单条上限。
pub async fn send_text_chunks(
    ctx: &crate::event::Context,
    writer: LockedWriter,
    group: Option<&str>,
    user: Option<&str>,
    text: &str,
) -> Result<(), BotError> {
    for chunk in readable_chunks(text, 2800) {
        send_msg(
            ctx,
            writer.clone(),
            group,
            user,
            crate::message::Message::new().text(chunk),
        )
        .await?;
    }
    Ok(())
}

fn readable_chunks(text: &str, limit: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut end = (start + limit.max(1)).min(chars.len());
        if end < chars.len() {
            // 不把通常长度的 URL 或单词从中间切开；超长无空白内容才硬分段。
            if let Some(at) = chars[start..end]
                .iter()
                .rposition(|c| *c == '\n')
                .or_else(|| chars[start..end].iter().rposition(|c| c.is_whitespace()))
            {
                end = start + at + 1;
            }
        }
        chunks.push(chars[start..end].iter().collect());
        start = end;
    }
    chunks
}

#[test]
fn readable_chunks_preserve_unicode_content_and_source_urls() {
    let url = "https://example.com/source?id=123";
    let text = format!("{}\n{}", "汉".repeat(2785), url);
    let chunks = readable_chunks(&text, 2800);
    assert_eq!(chunks.concat(), text);
    assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 2800));
    assert!(chunks.iter().any(|chunk| chunk.contains(url)));
    assert!(readable_chunks("", 2800).is_empty());
}
