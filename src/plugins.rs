use crate::adapters::satori::{LockedWriter, dispatch_packet};
use crate::config::build_config;
use crate::event::{BotStatus, Context, EventType, SendPacket};
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

pub type PluginResult<T> = Result<T, PluginError>;

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

/// 事件进入流水线之前、按收到的顺序**同步**调用一次，可以改写事件。
///
/// 要在「事件任务可能被重排之前」就更新状态的插件用它（复读要按收到顺序计数）。
/// 返回的 future 在流水线之前执行（比如先把复读发出去）。
pub type ReceiveHandler =
    fn(&mut Context, &LockedWriter) -> Option<BoxFuture<'static, Result<(), PluginError>>>;

/// 一条消息被交互等待（`Matcher`）消费、不再进入流水线时调用。
pub type ConsumedHandler = fn(&Context, &LockedWriter);

/// 一次 `message.create` 发出并拿到回执之后调用。
pub type SentHandler =
    for<'a> fn(&'a Context, &'a LockedWriter, &'a Receipt<'a>) -> BoxFuture<'a, ()>;

/// 一条已经发出的消息的回执：发送包、它真正落在的频道、实现端分配的消息 ID。
///
/// `bot` 是实际发送所用的登录——目标不在当前连接上时与 `ctx.bot` 不是同一个。
pub struct Receipt<'a> {
    pub packet: &'a SendPacket,
    pub bot: &'a BotStatus,
    pub channel_id: &'a str,
    pub message_ids: &'a [String],
}

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
    /// 事件进入流水线之前按收到顺序同步调用，见 [`ReceiveHandler`]
    pub on_receive: Option<ReceiveHandler>,
    /// 消息被交互等待消费时调用，见 [`ConsumedHandler`]
    pub on_consumed: Option<ConsumedHandler>,
    /// 消息发出之后调用，见 [`SentHandler`]
    pub on_sent: Option<SentHandler>,
    /// 默认配置与校验都由注册时给的配置类型生成，见 [`PluginConfig`]
    pub default_config: fn() -> Value,
    pub validate_config: fn(&Value) -> Result<(), String>,
    /// 启动时在补默认值之前改写旧版配置，见 [`PluginConfig::migrate`]
    pub migrate_config: fn(&mut toml::Table) -> bool,

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
        agent_command: false,
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

/// 编译期的字符串相等（`==` 在 const 里还不能用于 `&str`）。
const fn same_str(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
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

        // 配置表的键必须与模块标识符一致：编译期核对，写错了过不了编译
        $(
            const _: () = assert!(
                same_str(<$config as PluginConfig>::NAME, stringify!($module)),
                concat!("`", stringify!($module), "` 的 PluginConfig::NAME 与模块名不一致")
            );
        )*

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
                                on_receive: None,
                                on_consumed: None,
                                on_sent: None,
                                default_config: default_config_of::<$config>,
                                validate_config: validate_config_of::<$config>,
                                migrate_config: <$config as PluginConfig>::migrate,
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

/// 执行所有插件的初始化逻辑
pub async fn do_init(ctx: Context) -> Result<(), PluginError> {
    let plugins = get_plugins();

    let enabled_count = {
        let guard = ctx.config.read().unwrap();
        plugins.iter().filter(|p| enabled(&guard, p.name)).count()
    };

    info!(
        "正在加载插件系统 (已启用 {}/{})",
        enabled_count,
        plugins.len()
    );

    // 已启动集合从空开始，初始化成功才记入。初始化失败的插件会停在「待启动」，
    // 消息指令先不接（`pending_startup`），由 ctl 下次打开或重启时重试。
    if STARTUP_ENABLED.set(Mutex::new(HashSet::new())).is_err() {
        error!(target: LOG_TARGET, "插件初始化只能执行一次");
    }

    let system_bot: Arc<BotStatus> = Arc::new(BotStatus {
        adapter: "system".to_string(),
        platform: "internal".to_string(),
        login_user: Default::default(),
    });

    for plugin in plugins {
        if !enabled(&ctx.config.read().unwrap(), plugin.name) {
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
                agent_command: false,
            };

            // 执行初始化
            match init_fn(init_ctx).await {
                Ok(_) => {
                    started().lock().unwrap().insert(plugin.name.to_string());
                    info!(target: LOG_TARGET, "✅ [{}] 就绪 (Init Success)", plugin.name);
                }
                Err(e) => {
                    error!(
                        target: LOG_TARGET,
                        "❌ [{}] 初始化失败，消息指令暂不接入: {}",
                        plugin.name, e
                    );
                }
            }
        } else {
            info!(target: LOG_TARGET, "✅ [{}] 就绪", plugin.name);
        }
    }
    Ok(())
}

/// 配置里该插件的 `enabled` 开关；没有这张表、或没写开关都算关。
///
/// 全项目只此一处口径：流水线、生命周期钩子、ctl 与 Web 控制台都读它。
pub fn enabled(config: &crate::config::AppConfig, name: &str) -> bool {
    config
        .plugins
        .get(name)
        .and_then(|v| v.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// 与注册表同序的开关快照：一次读锁 + 一次启动集合锁，之后按序号 O(1) 判断。
///
/// 流水线与生命周期钩子在一个事件里反复问「这个插件参不参与」；逐个按名字查配置
/// 会在每个插件上各加一次锁。快照把这件事降到一次，也顺手统一了各处的判定。
struct Activation(Vec<bool>);

impl Activation {
    fn capture(ctx: &Context) -> Self {
        let config = ctx.config.read().unwrap();
        let started = STARTUP_ENABLED.get().map(|set| set.lock().unwrap());
        Self(
            get_plugins()
                .iter()
                .map(|plugin| {
                    enabled(&config, plugin.name)
                        && !(plugin.on_init.is_some()
                            && started
                                .as_ref()
                                .is_some_and(|set| !set.contains(plugin.name)))
                })
                .collect(),
        )
    }

    fn on(&self, index: usize) -> bool {
        self.0.get(index).copied().unwrap_or(false)
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
            "Bot {}/{} ({}) 已完成 connected 生命周期，重连不重复注册任务。",
            ctx.bot.adapter,
            ctx.bot.platform,
            ctx.bot.login_user.get().id
        );
        return Ok(());
    }

    let plugins = get_plugins();
    let active = Activation::capture(&ctx);

    for (index, plugin) in plugins.iter().enumerate() {
        if !active.on(index) {
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

/// 收到事件：按注册顺序调用各启用插件的 `on_receive`（同步，可改写事件）。
///
/// 返回的 future 里是各插件要在流水线之前做的事，按同一顺序执行，出错只记日志。
pub fn receive(ctx: &mut Context, writer: &LockedWriter) -> BoxFuture<'static, ()> {
    let mut tasks = Vec::new();
    let active = Activation::capture(ctx);
    for (index, plugin) in get_plugins().iter().enumerate() {
        if !active.on(index) {
            continue;
        }
        if let Some(hook) = plugin.on_receive
            && let Some(task) = hook(ctx, writer)
        {
            tasks.push((plugin.name, task));
        }
    }
    Box::pin(async move {
        for (name, task) in tasks {
            if let Err(error) = task.await {
                error!(target: LOG_TARGET, "❌ [{}] 接收钩子失败: {}", name, error);
            }
        }
    })
}

/// 这条消息被交互等待消费了、不会进入流水线：通知各启用插件的 `on_consumed`。
pub fn consumed(ctx: &Context, writer: &LockedWriter) {
    let active = Activation::capture(ctx);
    for (index, plugin) in get_plugins().iter().enumerate() {
        if active.on(index)
            && let Some(hook) = plugin.on_consumed
        {
            hook(ctx, writer);
        }
    }
}

/// 一次 `message.create` 发出之后：按注册顺序调用各启用插件的 `on_sent`。
pub async fn delivered(ctx: &Context, writer: &LockedWriter, delivery: &Receipt<'_>) {
    let active = Activation::capture(ctx);
    for (index, plugin) in get_plugins().iter().enumerate() {
        if active.on(index)
            && let Some(hook) = plugin.on_sent
        {
            hook(ctx, writer, delivery).await;
        }
    }
}

/// 运行插件流水线
///
/// 优化点：单次拿读锁完成所有插件 enabled 检查，避免 N 次锁竞争。
pub async fn run(mut ctx: Context, writer: LockedWriter) -> Result<(), PluginError> {
    let plugins = get_plugins();
    // 加群申请由框架记账，任何插件都能查；放在流水线之前，插件消费与否都不影响。
    crate::requests::observe(&ctx);

    let active = Activation::capture(&ctx);

    for (index, plugin) in plugins.iter().enumerate() {
        if !active.on(index) {
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
        EventType::Satori(_) => remind_switched_off(&ctx, &writer).await,
        EventType::BeforeSend(packet) => {
            dispatch_packet(&ctx, writer, packet).await?;
        }
        EventType::Init => {}
    }

    Ok(())
}

/// 号主发了一条关着的插件的指令：流水线里没人接，从前就此石沉大海，看上去像是坏了。
///
/// 只回号主与全局管理员（插件开关本来就归他们管），只认带了指令前缀的整词，且没有开着的
/// 插件也认这个词——开着的插件不接，是它自己的取舍（不是管理员、只是在聊天），不在这里多嘴。
fn remind_switched_off<'a>(ctx: &'a Context, writer: &'a LockedWriter) -> BoxFuture<'a, ()> {
    // 发送会再走一遍流水线（发送前钩子），装箱断开 async 递归，与插件处理函数同理。
    Box::pin(async move {
        let Some(message) = ctx.as_message() else {
            return;
        };
        if !crate::access::is_manager(ctx) {
            return;
        }
        let prefixes = crate::command::get_prefixes(ctx);
        let Some((prefix, word)) = crate::command::spoken_bodies(message.text())
            .into_iter()
            .find_map(|body| {
                prefixes.iter().filter(|prefix| !prefix.is_empty()).find_map(|prefix| {
                    let word = body.strip_prefix(prefix.as_str())?.split_whitespace().next()?;
                    Some((prefix.clone(), word))
                })
            })
        else {
            return;
        };
        let Some(plugin) = switched_off_owner(&ctx.config.read().unwrap(), word) else {
            return;
        };
        let text = format!(
            "「{}」没开，所以「{prefix}{word}」没人接。发「{prefix}ctl on {}」打开，或到控制台里开。",
            plugin.display_name, plugin.name
        );
        let reply = crate::message::Message::new().reply(message.message_id()).text(text);
        if let Err(error) = crate::adapters::satori::send_msg(ctx, writer.clone(), message.group_id(), Some(message.user_id()), reply).await {
            warn!(target: LOG_TARGET, "提醒「{}」没开时发送失败：{error}", plugin.name);
        }
    })
}

/// 注册表里一条指令写法的指令词：每个别名的第一个词。参数占位符与 `/#`、`~名` 这类
/// 自带写法的符号指令不算。
fn command_heads(cmd: &'static str) -> impl Iterator<Item = &'static str> {
    cmd.split(" / ")
        .filter_map(|alias| alias.split_whitespace().next())
        .filter(|head| head.chars().next().is_some_and(char::is_alphanumeric))
}

/// 认 `word` 这个指令词、却关着的插件；有开着的插件也认它就是 `None`。
fn switched_off_owner(config: &crate::config::AppConfig, word: &str) -> Option<&'static Plugin> {
    let claims = |plugin: &Plugin| {
        plugin
            .commands
            .iter()
            .any(|cmd| command_heads(cmd.cmd).any(|head| head == word))
    };
    let mut off = None;
    for plugin in get_plugins().iter().filter(|plugin| claims(plugin)) {
        if enabled(config, plugin.name) {
            return None;
        }
        off.get_or_insert(plugin);
    }
    off
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

    /// 旧版配置的改写：改了名、改了形状的键，以及仍是旧默认值的文案换成新的。
    /// 启动时在补默认值之前调用一次；返回是否改了东西（改了就落盘）。
    fn migrate(_table: &mut toml::Table) -> bool {
        false
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

    /// 关着的插件的指令词认得出来；开着的插件也认这个词时不多嘴；占位符与符号指令不算词。
    #[test]
    fn switched_off_owner_names_the_plugin_that_is_off() {
        let mut config = crate::config::AppConfig::default();
        assert_eq!(switched_off_owner(&config, "欢迎").map(|p| p.name), Some("welcome"));
        assert_eq!(switched_off_owner(&config, "潜水").map(|p| p.name), Some("lurker"));
        assert!(switched_off_owner(&config, "随便聊聊").is_none());
        let on: Value = toml::from_str("enabled = true").unwrap();
        config.plugins.insert("welcome".into(), on);
        assert!(switched_off_owner(&config, "欢迎").is_none());
        assert!(command_heads("ctl list [on|off|关键词]").eq(["ctl"]));
        assert!(command_heads("<链接> / /# [文字]").next().is_none());
    }

    /// 注册表里每个插件的元数据与配置都得自洽：
    /// 默认配置能通过自己的校验且带 `enabled`，摘要写了（配置键与模块名一致在编译期核对）。
    #[test]
    fn every_registered_plugin_is_self_consistent() {
        let mut seen = HashSet::new();
        for plugin in get_plugins() {
            assert!(seen.insert(plugin.name), "插件名重复：{}", plugin.name);
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
