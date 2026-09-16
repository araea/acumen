//! 本机控制台：知言（ayjx）的图形界面。
//!
//! 一个进程里那 22 个插件、一份配置树、一条日志流，在终端里各自有各自的看法
//! （`/ctl list`、`/ctl show`、`tail -F` 日志）。控制台把同一批东西摆到一个
//! 本机网页上，**不另存一份状态**：读的是注册表与 `config.toml`，写的是
//! `ctl::change` 那条唯一路径，日志是 `log::hook` 挂上来的同一行。
//!
//! 三条边界，写在这儿免得后来者当成疏漏：
//!
//! 1. **只绑回环、要口令。** 默认 `127.0.0.1:7801`，口令留空时首次启动自动生成
//!    并写在 `data/console/token`（0600），启动日志里打印的是带口令的完整地址。
//!    换到非回环地址仍然能跑，但那时它已经不只是一个本机面板了，口令是唯一的门。
//! 2. **它管的是这份部署，不是群。** 控制台里的每一个动作都等价于在本机敲
//!    `/ctl`，不往群里发消息、不碰 QQ 账号。群里的那一面仍由 22 个插件负责，
//!    设计上的分野见 `docs/GUIDELINES.md`。
//! 3. **关掉它，终端一切照旧。** 服务是可有可无的一层：`[console] enabled` 为假、
//!    或者本次启动带 `--no-ui`，都不影响任何指令、排期与推送。

mod api;
mod assets;
mod server;
mod state;

use crate::adapters::satori::LockedWriter;
use crate::config::build_config;
use crate::event::Context;
use crate::plugins::PluginError;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use toml::Value;

const LOG_TARGET: &str = "Plugin/Console";


#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub(crate) struct Config {
    /// 是否启动控制台服务。它是图形界面与终端共用的后端，关掉之后命令行一切照旧；
    /// 运行中改成关也会立刻停止应答，端口要到下次启动才释放。
    enabled: bool,
    /// 监听的地址。默认只绑回环地址，只有本机能连。
    bind: String,
    /// 监听的端口。
    port: u16,
    /// 访问口令。留空表示首次启动自动生成一个，保存在 data/console/token，权限 0600。
    token: String,
    /// 日志面板往回保留的行数。
    log_lines: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            bind: "127.0.0.1".to_string(),
            port: 7801,
            token: String::new(),
            log_lines: 400,
        }
    }
}

pub fn default_config() -> Value {
    build_config(Config::default())
}

/// Validate control edits against the plugin's actual configuration type.
pub fn validate_config(value: &Value) -> Result<(), String> {
    Config::deserialize(value.clone())
        .map(|_| ())
        .map_err(|_| "配置类型不匹配（bind 与 token 必须是字符串，port 与 log_lines 必须是整数）".to_string())
}

/// 启动控制台服务。
///
/// 幂等：重复调用（例如运行时用 `/ctl on console` 再触发一次初始化）只会在第二次
/// 直接返回，不会起第二个监听。
pub fn init(ctx: Context) -> BoxFuture<'static, Result<(), PluginError>> {
    Box::pin(async move {
        let cfg: Config = crate::plugins::get_config_or_default(&ctx, "console");
        if !cfg.enabled {
            info!(target: LOG_TARGET, "控制台已关闭；命令行与群里的指令不受影响。");
            return Ok(());
        }
        state::install(ctx, cfg).await.map_err(Into::into)
    })
}

/// 连接建立时登记这个适配器，总览页要显示「连着哪个账号」。
pub fn on_connected(
    ctx: Context,
    _writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        if let Some(console) = state::get() {
            console.register_bot(ctx.bot.clone());
        }
        Ok(Some(ctx))
    })
}

/// 控制台不处理任何消息，原样放行。
pub fn handle(
    ctx: Context,
    _writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move { Ok(Some(ctx)) })
}

/// 进程退出前把监听松掉，让下一次启动立刻能抢到同一个端口。
pub(crate) async fn shutdown() {
    if let Some(console) = state::get() {
        console.stop();
    }
}

/// 命令行对本进程这一次启动的覆盖。
///
/// 只有两个开关，都用 `OnceLock` 在 `plugins::do_init` 之前写一次：`--no-ui`
/// 让整台服务不启动，`--ui <端口>` 顶着配置里的端口跑（临时换一个端口调试用）。
#[derive(Default, Clone, Copy)]
pub(crate) struct Options {
    pub ui: bool,
    pub port: Option<u16>,
}

static OPTIONS: std::sync::OnceLock<Options> = std::sync::OnceLock::new();

/// 记下本次启动的覆盖项；重复调用只认第一次。
pub(crate) fn set_options(options: Options) {
    let _ = OPTIONS.set(options);
}

pub(crate) fn options() -> Options {
    OPTIONS.get().copied().unwrap_or(Options {
        ui: true,
        port: None,
    })
}

/// 控制台正在用的地址（带口令），没在跑时是 None。
pub(crate) fn url() -> Option<String> {
    state::get().map(|console| console.url.clone())
}
