//! 控制台的进程内状态：只此一份，谁读都读它。
//!
//! 这里存的是「进程里已经有的东西」——配置树、日志落点、各适配器的连接状态、
//! 启动时刻——而不是一份为界面准备的副本。唯一新产生的东西是那圈日志缓冲：
//! 页面刚打开时要能立刻看到前几百行，光靠订阅只能等到下一行才有内容。

use super::{Config, LOG_TARGET};
use crate::event::{BotStatus, Context};
use crate::log;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use tokio::sync::broadcast;

/// 日志面板里的一行。与终端上那一行的区别只有两处：不带 ANSI 转义，
/// 级别与 target 是分开的字段（页面上要按它们筛）。
#[derive(Clone, Serialize)]
pub(crate) struct Entry {
    pub at: String,
    pub level: String,
    pub target: String,
    pub text: String,
}

pub(crate) struct Console {
    /// 装起来的那份上下文：配置、数据库、保存锁、配置路径都在里面。
    ctx: Context,
    started: Instant,
    started_at: String,
    /// 带口令的完整地址，启动日志与 `./bot ui` 都打印它。
    pub(crate) url: String,
    /// 口令。回环之外的请求全靠它。
    token: String,
    capacity: usize,
    logs: Mutex<VecDeque<Entry>>,
    feed: broadcast::Sender<Entry>,
    bots: Mutex<Vec<Arc<BotStatus>>>,
    stopping: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

static CONSOLE: OnceLock<Arc<Console>> = OnceLock::new();

pub(crate) fn get() -> Option<&'static Arc<Console>> {
    CONSOLE.get()
}

/// 装好控制台：解析口令、挂上日志落点、记下启动时刻，然后交给 `server` 起监听。
pub(super) async fn install(ctx: Context, cfg: Config) -> Result<(), String> {
    if CONSOLE.get().is_some() {
        return Ok(());
    }

    let options = super::options();
    if !options.ui {
        info!(target: LOG_TARGET, "本次启动带了 --no-ui，控制台不开放。");
        return Ok(());
    }

    let port = options.port.unwrap_or(cfg.port);
    let token = resolve_token(&cfg.token).await?;
    let now = chrono::Local::now();
    let url = format!("http://{}:{}/?t={}", display_host(&cfg.bind), port, token);

    let (feed, _) = broadcast::channel(512);
    let console = Arc::new(Console {
        ctx,
        started: Instant::now(),
        started_at: now.format("%Y-%m-%d %H:%M:%S").to_string(),
        url,
        token,
        capacity: cfg.log_lines.clamp(50, 5000),
        logs: Mutex::new(VecDeque::with_capacity(cfg.log_lines.clamp(50, 5000))),
        feed,
        bots: Mutex::new(Vec::new()),
        stopping: Mutex::new(None),
    });

    // 顺序要紧：先把单例放进去，日志落点才能在里面取到自己。
    if CONSOLE.set(console.clone()).is_err() {
        return Ok(());
    }
    log::hook(|line| {
        if let Some(console) = CONSOLE.get() {
            console.push(line);
        }
    });

    let shutdown = console.clone();
    let bind = cfg.bind.clone();
    super::server::start(console, &bind, port).await?;
    record_url(&shutdown.url).await;
    // 记一笔就绪：终端里那一行是「这台机器的控制台在哪儿」唯一的口径。
    info!(target: LOG_TARGET, "控制台已就绪 {}", shutdown.url);
    Ok(())
}

/// 把带口令的地址写一份在 `data/console/url`（0600）。
///
/// 启动日志里有同一行，但日志会被群里刷走（`tail -F` 那个窗口尤其，
/// 见 docs/CONTROL.md 末尾那条），落一个文件之后 `./bot ui` 随时能把它捞出来。
async fn record_url(url: &str) {
    let Ok(dir) = crate::plugins::get_data_dir("console").await else {
        return;
    };
    let path = dir.join("url");
    use tokio::io::AsyncWriteExt;
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    if let Ok(mut file) = options.open(&path).await {
        let _ = file.write_all(format!("{url}\n").as_bytes()).await;
    }
}

/// 口令：配置里写了就用它，没写就首启动生成一个落盘。
///
/// 落在 `data/console/token`（0600）。放在数据目录而不是配置里，是因为它要能被
/// 「已经在跑的那个进程」自己换掉，也让 `./bot ui` 不必去解析 config.toml。
async fn resolve_token(configured: &str) -> Result<String, String> {
    if !configured.trim().is_empty() {
        return Ok(configured.trim().to_string());
    }
    let dir = crate::plugins::get_data_dir("console")
        .await
        .map_err(|e| format!("无法创建 data/console：{e}"))?;
    let path = dir.join("token");
    if let Ok(text) = tokio::fs::read_to_string(&path).await {
        let text = text.trim().to_string();
        if !text.is_empty() {
            return Ok(text);
        }
    }
    let token = fresh_token();
    use tokio::io::AsyncWriteExt;
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&path)
        .await
        .map_err(|e| format!("无法写入 {}：{e}", path.display()))?;
    file.write_all(format!("{token}\n").as_bytes())
        .await
        .map_err(|e| format!("无法写入 {}：{e}", path.display()))?;
    Ok(token)
}

/// 32 位十六进制。够长到不可猜，又短到能整条贴进地址栏。
fn fresh_token() -> String {
    format!(
        "{:016x}{:016x}",
        rand::random::<u64>(),
        rand::random::<u64>()
    )
}

/// 地址栏里给人看的主机名：绑的是通配地址时换成回环，否则照写。
fn display_host(bind: &str) -> String {
    match bind {
        "0.0.0.0" | "::" | "[::]" | "*" => "127.0.0.1".to_string(),
        other => other.to_string(),
    }
}

impl Console {
    pub(crate) fn ctx(&self) -> &Context {
        &self.ctx
    }

    /// 以本机控制台的身份拿一份上下文。
    ///
    /// `ctl::is_manager` 认的正是这一对适配器与平台名——控制台本来就是文档里
    /// 「本机控制台管理」的那一条路，口令是它的门。写配置照旧走 `ctl::change`，
    /// 不绕过校验、不另开一条保存路径。
    pub(crate) fn local(&self) -> Context {
        let mut ctx = self.ctx.clone();
        ctx.bot = Arc::new(BotStatus {
            adapter: "console".to_string(),
            platform: "console".to_string(),
            login_user: Default::default(),
        });
        ctx
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    pub(crate) fn started_at(&self) -> &str {
        &self.started_at
    }

    pub(crate) fn uptime_seconds(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// 本次进程是不是还开着控制台。运行中被 `/ctl set console enabled 关` 关掉之后
    /// 这里就变成假，接口随即停止应答（端口要到重启才释放）。
    pub(crate) fn enabled(&self) -> bool {
        crate::plugins::get_config_or_default::<Config>(&self.ctx, "console").enabled
    }

    pub(crate) fn register_bot(&self, bot: Arc<BotStatus>) {
        let mut bots = self.bots.lock().unwrap();
        if bots
            .iter()
            .any(|b| b.adapter == bot.adapter && b.platform == bot.platform)
        {
            return;
        }
        bots.push(bot);
    }

    pub(crate) fn bots(&self) -> Vec<Arc<BotStatus>> {
        self.bots.lock().unwrap().clone()
    }

    /// 往回保留的那一段日志，供页面刚打开时补齐。
    pub(crate) fn recent(&self) -> Vec<Entry> {
        self.logs.lock().unwrap().iter().cloned().collect()
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Entry> {
        self.feed.subscribe()
    }

    /// 终端上打完那一行之后落进来的同一个副本。
    fn push(&self, line: log::Line) {
        let entry = Entry {
            at: line.at,
            level: line.level.to_string(),
            target: line.target,
            text: line.text,
        };
        if let Ok(mut logs) = self.logs.lock() {
            while logs.len() >= self.capacity {
                logs.pop_front();
            }
            logs.push_back(entry.clone());
        }
        // 没有订阅者时 send 返回错误，这是常态（页面没开着），不是问题。
        let _ = self.feed.send(entry);
    }

    pub(crate) fn stop(&self) {
        if let Some(sender) = self.stopping.lock().unwrap().take() {
            let _ = sender.send(());
        }
    }

    pub(crate) fn set_stopper(&self, sender: tokio::sync::oneshot::Sender<()>) {
        *self.stopping.lock().unwrap() = Some(sender);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wildcard_bind_is_shown_as_loopback() {
        assert_eq!(display_host("0.0.0.0"), "127.0.0.1");
        assert_eq!(display_host("::"), "127.0.0.1");
        assert_eq!(display_host("192.168.1.9"), "192.168.1.9");
    }

    #[test]
    fn a_token_is_long_enough_to_not_be_guessed() {
        let token = fresh_token();
        assert_eq!(token.len(), 32);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(token, fresh_token());
    }
}
