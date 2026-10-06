#![allow(dead_code)]

use crate::adapters::satori::{LockedWriter, dispatch_packet};
use crate::config::build_config;
use crate::event::{BotStatus, Context, Event, EventType};
use crate::matcher::Matcher;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::fs;
use toml::Value;

/// 框架自身的日志 target（插件级日志用 `Plugin/<名字>`）。
const LOG_TARGET: &str = "Plugin/Lifecycle";

pub type PluginError = Box<dyn std::error::Error + Send + Sync>;

pub type PluginResult<T> = std::result::Result<T, PluginError>;

/// 把 `catch_unwind` 接住的 panic 载荷转成一行能读的文字。
///
/// panic 的载荷是 `&str` 或 `String` 两种（前者是 `panic!("字面量")`，后者是带
/// 格式参数的 `panic!`）；认不出来时给一句兜底，不为了日志再去 unwrap 一次。
pub fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "未知名目的 panic".to_string()
    }
}

pub type PluginHandler =
    fn(Context, LockedWriter) -> BoxFuture<'static, Result<Option<Context>, PluginError>>;

pub type PluginInitHandler = fn(Context) -> BoxFuture<'static, Result<(), PluginError>>;

/// 一条面向用户的指令说明。
///
/// `cmd` 可含参数占位符（`<必填>` / `[可选]`）与 ` / ` 分隔的别名，
/// 渲染时会把首个抬为主指令、其余降级为别名；`note` 是一句话用途。
pub struct Cmd {
    pub cmd: &'static str,
    pub note: &'static str,
}

/// 把 `("指令", "说明")` 列表展开成 `&'static [Cmd]`（结构体字面量可静态提升）
#[macro_export]
macro_rules! cmds {
    ( $( ($c:expr, $n:expr) ),* $(,)? ) => {
        &[ $( $crate::plugins::Cmd { cmd: $c, note: $n } ),* ]
    };
}

pub struct Plugin {
    pub name: &'static str,
    /// 中文显示名（面向用户的展示名，默认与 name 相同，可在注册时覆盖）
    pub display_name: &'static str,
    pub handler: PluginHandler,
    pub on_init: Option<PluginInitHandler>,
    /// 当 Bot 连接成功且获取到自身信息后触发 (用于注册主动推送任务等)
    pub on_connected: Option<PluginHandler>,
    /// 配置表的键（[`PluginConfig::NAME`]）；注册时核对它与模块标识符一致
    pub config_name: &'static str,
    /// 默认配置与校验都由注册时给的配置类型生成，见 [`PluginConfig`]
    pub default_config: fn() -> Value,
    pub validate_config: fn(&Value) -> Result<(), String>,

    // —— 帮助元数据 ——
    // 三个字段一并写在注册表里，帮助中心直接读，不再另设一份清单。
    // 新增插件只需在 registry.rs 补一条，/help 与 /ctl 自动跟上。
    /// 所属分区代号，见 `help::SECTIONS`；缺省落到「其他」
    pub section: &'static str,
    /// 一句话说明：这个插件到底做什么
    pub summary: &'static str,
    /// 完整指令清单；后台自动工作的插件留空
    pub commands: &'static [Cmd],
}

static PLUGINS: OnceLock<Vec<Plugin>> = OnceLock::new();
/// 已经跑过初始化的插件集合。启动时由 [`do_init`] 填入当时启用的插件；运行中经
/// ctl 打开的插件由 [`start`] 补跑 `on_init` 后写入，于是不必重启。
static STARTUP_ENABLED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn started() -> &'static Mutex<HashSet<String>> {
    STARTUP_ENABLED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// 插件是否带初始化钩子（注册表里的 `on_init`）。
pub fn needs_init(name: &str) -> bool {
    get_plugins()
        .iter()
        .any(|p| p.name == name && p.on_init.is_some())
}

/// 插件是否带生命周期钩子（`on_init` 或 `on_connected`）。
pub fn needs_startup(name: &str) -> bool {
    get_plugins()
        .iter()
        .any(|p| p.name == name && (p.on_init.is_some() || p.on_connected.is_some()))
}

/// 有初始化钩子、但初始化还没跑过——此时消息处理必须等初始化完成。
///
/// `on_connected` 只在连接建立时触发，不属于这里的门槛：中途打开的插件即使还接不上
/// 连接排期，消息指令也应当照常可用。`do_init` 未跑过（测试）时一律视为已就绪。
pub fn pending_startup(name: &str) -> bool {
    needs_init(name)
        && STARTUP_ENABLED
            .get()
            .is_some_and(|set| !set.lock().unwrap().contains(name))
}

/// 运行时启用一个带初始化钩子的插件：补跑一次 `on_init` 并记入已启动集合，
/// 让它的消息指令立刻可用，不必等重启。
///
/// 初始化失败时不写入集合，插件维持「待重启」，由 `do_init` 在下次启动时重试。
pub async fn start(ctx: &Context, name: &str) -> Result<(), String> {
    let Some(plugin) = get_plugins().iter().find(|p| p.name == name) else {
        return Err(format!("未找到插件「{name}」"));
    };
    let Some(init) = plugin.on_init else {
        return Ok(());
    };
    // 同一插件只跑一次初始化：并发启用时在锁内复查。
    static INIT_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    let _guard = INIT_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    if STARTUP_ENABLED
        .get()
        .is_some_and(|set| set.lock().unwrap().contains(name))
    {
        return Ok(());
    }
    let init_ctx = Context {
        event: EventType::Init,
        config: ctx.config.clone(),
        config_save_lock: ctx.config_save_lock.clone(),
        db: ctx.db.clone(),
        scheduler: ctx.scheduler.clone(),
        matcher: Arc::new(Matcher::new()),
        config_path: ctx.config_path.clone(),
        bot: Arc::new(BotStatus {
            adapter: "system".to_string(),
            platform: "internal".to_string(),
            login_user: Default::default(),
        }),
    };
    init(init_ctx).await.map_err(|error| error.to_string())?;
    started().lock().unwrap().insert(name.to_string());
    info!(target: LOG_TARGET, "🔁 [{}] 运行时启用，已补跑初始化", name);
    Ok(())
}

static CONNECTED_BOTS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn mark_connected(connection_key: String) -> bool {
    CONNECTED_BOTS
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap()
        .insert(connection_key)
}

/// 插件注册宏
///
/// 每条记录以 `config: <配置类型>` 开头，其余字段覆盖 [`Plugin`] 的缺省值。
macro_rules! register_plugins {
    (
        $(
            $module:ident { config: $config:ty $(, $key:ident : $val:expr)* $(,)? }
        ),* $(,)?
    ) => {
        // 1. 自动生成模块声明 (无需手动 pub mod)
        $( pub mod $module; )*

        // 2. 生成获取插件列表的函数
        pub fn get_plugins() -> &'static [Plugin] {
            PLUGINS.get_or_init(|| {
                vec![
                    $(
                        {
                            // 默认构造
                            #[allow(unused)]
                            let mut p = Plugin {
                                name: stringify!($module),
                                display_name: stringify!($module),
                                handler: $module::handle,
                                on_init: None,
                                on_connected: None,
                                config_name: <$config as PluginConfig>::NAME,
                                default_config: default_config_of::<$config>,
                                validate_config: validate_config_of::<$config>,
                                section: "misc",
                                summary: "",
                                commands: &[],
                            };
                            // 应用自定义覆盖 (如果有)
                            $( p.$key = $val; )*
                            p
                        }
                    ),*
                ]
            })
        }
    };
}

// 引入单独的注册文件
include!("./plugins/registry.rs");

pub fn register_plugins() -> &'static [Plugin] {
    get_plugins()
}

/// 执行所有插件的初始化逻辑
pub async fn do_init(ctx: Context) -> Result<(), PluginError> {
    let plugins = get_plugins();

    let enabled_count = {
        let guard = ctx.config.read().unwrap();
        plugins
            .iter()
            .filter(|p| {
                guard
                    .plugins
                    .get(p.name)
                    .and_then(|v| v.get("enabled"))
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false)
            })
            .count()
    };

    info!(
        target: "System",
        "正在加载插件系统 (已启用 {}/{})",
        enabled_count,
        plugins.len()
    );

    // 一次性快照所有插件的 enabled 标记，避免每个插件单独锁
    let _ = STARTUP_ENABLED.set(Mutex::new({
        let cfg = ctx.config.read().unwrap();
        plugins
            .iter()
            .filter(|p| {
                cfg.plugins
                    .get(p.name)
                    .and_then(|v| v.get("enabled"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
            .map(|p| p.name.to_string())
            .collect()
    }));
    let enabled_set = collect_enabled_set(&ctx);

    let system_bot: Arc<BotStatus> = Arc::new(BotStatus {
        adapter: "system".to_string(),
        platform: "internal".to_string(),
        login_user: Default::default(),
    });

    for plugin in plugins {
        if !enabled_set[plugin.name] {
            continue;
        }

        if let Some(init_fn) = plugin.on_init {
            let init_ctx = Context {
                event: EventType::Init,
                config: ctx.config.clone(),
                config_save_lock: ctx.config_save_lock.clone(),
                db: ctx.db.clone(),
                scheduler: ctx.scheduler.clone(),
                matcher: Arc::new(Matcher::new()),
                config_path: ctx.config_path.clone(),
                bot: system_bot.clone(),
            };

            // 执行初始化
            match init_fn(init_ctx).await {
                Ok(_) => {
                    info!(target: LOG_TARGET, "✅ [{}] 就绪 (Init Success)", plugin.name);
                }
                Err(e) => {
                    error!(target: LOG_TARGET, "❌ [{}] 初始化失败: {}", plugin.name, e);
                }
            }
        } else {
            info!(target: LOG_TARGET, "✅ [{}] 就绪", plugin.name);
        }
    }
    Ok(())
}

/// 在单次读锁下采集所有插件的 enabled 标记，避免每事件多次加锁
fn collect_enabled_set(ctx: &Context) -> EnabledSet {
    let plugins = get_plugins();
    let guard = ctx.config.read().unwrap();
    let mut set = EnabledSet::with_capacity(plugins.len());
    for p in plugins {
        let enabled = guard
            .plugins
            .get(p.name)
            .and_then(|v| v.get("enabled"))
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        set.insert(p.name, enabled && !pending_startup(p.name));
    }
    set
}

/// 轻量级 enabled 标记表：保持插件名指针稳定，按 &str 索引
struct EnabledSet {
    entries: Vec<(&'static str, bool)>,
}

impl EnabledSet {
    fn with_capacity(cap: usize) -> Self {
        Self {
            entries: Vec::with_capacity(cap),
        }
    }
    fn insert(&mut self, name: &'static str, enabled: bool) {
        self.entries.push((name, enabled));
    }
}

impl std::ops::Index<&str> for EnabledSet {
    type Output = bool;
    fn index(&self, name: &str) -> &bool {
        for (n, v) in &self.entries {
            if *n == name {
                return v;
            }
        }
        &false
    }
}

/// 当 Bot 连接建立后触发（用于注册定时任务或主动操作）
pub async fn do_connected(ctx: Context, writer: LockedWriter) -> Result<(), PluginError> {
    // Satori 的 API 走 HTTP，不依赖事件 WS：重连后已注册的定时任务仍能照常发送。
    // 因此同一登录只跑一次 connected，否则每次断线重连都会再叠一份推送任务。
    let connection_key = format!(
        "{}|{}|{}|{}",
        writer.connection_key(),
        ctx.bot.adapter,
        ctx.bot.platform,
        ctx.bot.login_user.get().id
    );
    if !mark_connected(connection_key) {
        info!(
            target: "System",
            "Bot {}/{} ({}) 已完成 connected 生命周期，重连不重复注册任务。",
            ctx.bot.adapter,
            ctx.bot.platform,
            ctx.bot.login_user.get().id
        );
        return Ok(());
    }

    let plugins = get_plugins();

    // 一次性快照启用集合
    let enabled_set = collect_enabled_set(&ctx);

    for plugin in plugins {
        if !enabled_set[plugin.name] {
            continue;
        }

        if let Some(conn_fn) = plugin.on_connected {
            if let Err(e) = conn_fn(ctx.clone(), writer.clone()).await {
                error!(target: LOG_TARGET, "❌ [{}] 连接钩子执行失败: {}", plugin.name, e);
            } else {
                info!(target: LOG_TARGET, "🔗 [{}] 连接钩子已触发", plugin.name);
            }
        }
    }
    Ok(())
}

/// 运行插件流水线
///
/// 优化点：单次拿读锁完成所有插件 enabled 检查，避免 N 次锁竞争。
pub async fn run(mut ctx: Context, writer: LockedWriter) -> Result<(), PluginError> {
    let plugins = get_plugins();

    // 一次性快照所有插件的 enabled 标记，避免每个插件单独锁
    let enabled_set = collect_enabled_set(&ctx);

    for plugin in plugins {
        if !enabled_set[plugin.name] {
            continue;
        }

        // ctx 在这里 Move 进 handler，若插件返回 Some(ctx) 则接力给下一个插件
        // 这样插件拥有 Context 的所有权，可以修改 Context.event 中的内容
        match (plugin.handler)(ctx, writer.clone()).await {
            Ok(Some(next_ctx)) => {
                ctx = next_ctx;
            }
            // None：插件消费了事件，流水线到此为止
            Ok(None) => return Ok(()),
            // 单个插件失败不应崩掉整个适配器：记录后按"事件已消费"处理
            Err(e) => {
                error!(
                    target: LOG_TARGET,
                    "❌ [{}] 处理事件失败: {}",
                    plugin.name, e
                );
                return Ok(());
            }
        }
    }

    // 注意：ctx.event 现在是 EventType，可以直接 match 引用
    match &ctx.event {
        EventType::Satori(_) => {}
        EventType::BeforeSend(packet) => {
            dispatch_packet(&ctx, writer, packet).await?;
        }
        EventType::Init => {}
    }

    Ok(())
}

// ================= 群名单 =================

/// 群黑白名单。语义在所有使用它的插件之间保持一致：
/// 黑名单命中即排除；白名单非空时只放行名单内的群；两者都留空即对所有群生效。
///
/// 黑名单优先于白名单——同时写进两边的群按"明确禁止"处理。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelConfig {
    /// 白名单：非空时只在这些群生效
    pub white: Vec<String>,
    /// 黑名单：这些群一律不生效
    pub black: Vec<String>,
}

impl ChannelConfig {
    /// 群是否放行。`None`（私聊）不受群名单约束。
    pub fn allows(&self, group_id: Option<&str>) -> bool {
        match group_id {
            Some(gid) => self.allows_group(gid),
            None => true,
        }
    }

    /// 私聊是否放行。私聊没有群号，不受白名单约束；黑名单可以点名对方——
    /// 私聊频道 ID 或对方的账号（微信里公众号、服务通知也是这样的会话，如 `gh_xxxx`）。
    pub fn allows_direct(&self, ids: &[&str]) -> bool {
        !ids.iter()
            .any(|id| !id.is_empty() && self.black.iter().any(|black| black == id))
    }

    /// 群是否放行。主动推送只发群，没有"私聊放行"这一说，因此单独一个入口。
    pub fn allows_group(&self, group_id: &str) -> bool {
        if self.black.iter().any(|id| id == group_id) {
            return false;
        }
        self.white.is_empty() || self.white.iter().any(|id| id == group_id)
    }
}

// ================= 工具函数 =================

/// 将伪造/修改过的事件推送回流水线
pub async fn send_fake_event(
    ctx: &Context,
    writer: LockedWriter,
    event: Event,
) -> Result<(), PluginError> {
    let new_ctx = Context {
        event: EventType::Satori(event),
        config: ctx.config.clone(),
        config_save_lock: ctx.config_save_lock.clone(),
        db: ctx.db.clone(),
        scheduler: ctx.scheduler.clone(),
        matcher: ctx.matcher.clone(),
        config_path: ctx.config_path.clone(),
        bot: ctx.bot.clone(),
    };
    run(new_ctx, writer).await
}

/// 插件的数据目录（`data/<插件>`），不存在就建。目录怎么算见 [`crate::storage::data_path`]。
pub async fn get_data_dir(plugin_name: &str) -> Result<PathBuf, PluginError> {
    let path = crate::storage::data_path(plugin_name)?;
    fs::create_dir_all(&path).await?;
    Ok(path)
}

// ================= 插件配置 =================

/// 插件的配置类型。每个插件恰有一个，在 `registry.rs` 里以 `config:` 登记。
///
/// 默认值、类型校验、读取与写回都从这一个类型出发：调用点不必再重复插件名，
/// 插件里也不必各抄一份 `default_config` / `validate_config`。
/// 类型须带容器级 `#[serde(default)]`，缺省字段回落到 `Default`。
pub trait PluginConfig: Serialize + DeserializeOwned + Default {
    /// 顶层 `[插件名]` 表的键，与 `registry.rs` 里的模块标识符一致。
    const NAME: &'static str;

    /// 类型对不上时给管理员看的提示；配置里有专属字段的插件可以写得更具体。
    const MISMATCH: &'static str = "配置类型不匹配（请检查数组元素、字段类型及整数范围）";

    /// 类型读通之后的取值检查（范围、可选值之类），`/ctl` 与控制台保存前调用。
    fn check(&self) -> Result<(), String> {
        Ok(())
    }
}

/// 该类型的默认配置表；注册表用它生成 [`Plugin::default_config`]。
pub fn default_config_of<T: PluginConfig>() -> Value {
    build_config(T::default())
}

/// 按真实配置类型校验一张配置表；注册表用它生成 [`Plugin::validate_config`]。
pub fn validate_config_of<T: PluginConfig>(value: &Value) -> Result<(), String> {
    T::deserialize(value.clone())
        .map_err(|_| T::MISMATCH.to_string())?
        .check()
}

/// 读取插件配置；未配置或类型对不上时返回 `None`。
pub fn get_config<T: PluginConfig>(ctx: &Context) -> Option<T> {
    let guard = ctx.config.read().unwrap();
    guard
        .plugins
        .get(T::NAME)
        .and_then(|v| T::deserialize(v.clone()).ok())
}

/// 读取插件配置，未配置或反序列化失败时回落到 `T::default()`。
///
/// 反序列化失败会打告警，避免配置类型改坏后静默失效难以排查。
pub fn get_config_or_default<T: PluginConfig>(ctx: &Context) -> T {
    let guard = ctx.config.read().unwrap();
    match guard.plugins.get(T::NAME) {
        None => T::default(),
        Some(v) => T::deserialize(v.clone()).unwrap_or_else(|e| {
            warn!(
                target: LOG_TARGET,
                "插件 [{}] 配置反序列化失败，已使用默认值: {}",
                T::NAME, e
            );
            T::default()
        }),
    }
}

/// 修改配置 (异步 & 自动持久化 & 线程安全)
pub async fn update_config<T, F>(ctx: &Context, f: F) -> Result<(), PluginError>
where
    T: PluginConfig,
    F: FnOnce(T) -> T,
{
    let _fs_guard = ctx.config_save_lock.lock().await;
    let mut snapshot = ctx.config.read().unwrap().clone();
    let current = snapshot.plugins.get(T::NAME).ok_or("插件配置不存在")?;
    let next = Value::try_from(f(T::deserialize(current.clone())?))?;
    snapshot.plugins.insert(T::NAME.to_string(), next);
    snapshot.save(&ctx.config_path).await?;
    *ctx.config.write().unwrap() = snapshot;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 注册表里每个插件的元数据与配置类型都得自洽：
    /// 配置表的键就是模块名，默认配置能通过自己的校验且带 `enabled`，摘要写了。
    #[test]
    fn every_registered_plugin_is_self_consistent() {
        let mut seen = HashSet::new();
        for plugin in get_plugins() {
            assert!(seen.insert(plugin.name), "插件名重复：{}", plugin.name);
            assert_eq!(plugin.config_name, plugin.name, "配置键与模块名不一致");
            assert!(!plugin.summary.is_empty(), "{} 缺一句话摘要", plugin.name);
            let defaults = (plugin.default_config)();
            assert!(
                defaults.get("enabled").is_some_and(Value::is_bool),
                "{} 的默认配置缺 enabled 开关",
                plugin.name
            );
            (plugin.validate_config)(&defaults)
                .unwrap_or_else(|e| panic!("{} 的默认配置通不过自己的校验：{e}", plugin.name));
        }
    }
}
