//! restart 插件：例行维护（按时更新与重启）、内存阈值监控、手动重启指令。
//!
//! 例行维护按配置的时刻进行：先查新版本，有就下载校验换上；再等群里安静下来、手头的
//! 活儿做完，才请求主循环重启。重启由主循环收尾：停适配器与任务、关数据库与浏览器、
//! 保存配置，Unix 下 exec 替换当前进程，PID 不变，托管它的 runit 不会当成崩溃。

use crate::adapters::satori::{LockedWriter, send_msg};
use crate::command::{get_prefixes, match_word_command};
use crate::event::Context;
use crate::message::Message;
use crate::plugins::{PluginConfig, PluginError, get_config, get_config_or_default};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::time::Duration;

const LOG_TARGET: &str = "Plugin/Restart";

// ================= 配置定义 =================

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct RestartConfig {
    enabled: bool,
    /// 每天在这些时刻做一次例行维护（HH:MM，24 小时制，可写多个）
    times: Vec<String>,
    /// 例行维护时先查新版本，有就下载、校验、换上（仅发布版）
    auto_update: bool,
    /// 例行维护时没有新版本也重启一次，放掉长时间运行积下的内存
    daily_restart: bool,
    /// 平滑重启：等到这么多秒没有新消息、手头也没有在处理的消息再动手
    quiet_seconds: u64,
    /// 最多等多少分钟；过了仍不安静也照常重启
    max_wait_minutes: u64,
    /// 进程自身 RSS 内存阈值 (MB)，超过就平滑重启；0 表示关闭内存监控
    memory_threshold_mb: u64,
    /// 内存巡检间隔(分钟)
    memory_check_interval_minutes: u64,
    /// 是否开放 /restart 手动指令(默认关闭，防止群内成员误触发重启)
    allow_manual_restart: bool,
    /// 手动重启前等待秒数(等待通知消息刷新到 WebSocket)
    restart_delay_seconds: u64,
}

impl Default for RestartConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            times: vec!["04:30".to_string()],
            auto_update: true,
            daily_restart: true,
            quiet_seconds: 120,
            max_wait_minutes: 30,
            memory_threshold_mb: 1024,
            memory_check_interval_minutes: 5,
            allow_manual_restart: false,
            restart_delay_seconds: 3,
        }
    }
}

// ================= 全局状态 =================

static RESTART_REQUEST: tokio::sync::Notify = tokio::sync::Notify::const_new();

pub async fn wait_request() {
    RESTART_REQUEST.notified().await;
}

/// 重启防抖标记:防止定时任务与内存监控同时触发导致双重重启
static RESTARTING: AtomicBool = AtomicBool::new(false);
/// 例行维护进行中（在等安静或在下载），同一时刻只跑一份。
static MAINTAINING: AtomicBool = AtomicBool::new(false);

/// 正在处理的事件数与最近一次收到事件的时刻，平滑重启靠它们判断「现在安静」。
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static LAST_ACTIVITY: AtomicI64 = AtomicI64::new(0);

/// 处理一条事件期间持有它；放下时记下时刻。
pub struct Busy(());

pub fn busy() -> Busy {
    IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
    LAST_ACTIVITY.store(chrono::Utc::now().timestamp(), Ordering::Relaxed);
    Busy(())
}

impl Drop for Busy {
    fn drop(&mut self) {
        LAST_ACTIVITY.store(chrono::Utc::now().timestamp(), Ordering::Relaxed);
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

fn quiet_for(seconds: u64) -> bool {
    let idle = chrono::Utc::now().timestamp() - LAST_ACTIVITY.load(Ordering::Relaxed);
    IN_FLIGHT.load(Ordering::SeqCst) == 0 && idle >= i64::try_from(seconds).unwrap_or(i64::MAX)
}

/// 等到安静下来，或等满 `max_wait_minutes`。
async fn wait_quiet(cfg: &RestartConfig) {
    let deadline = tokio::time::Instant::now() + Duration::from_mins(cfg.max_wait_minutes);
    let mut logged = false;
    while !quiet_for(cfg.quiet_seconds) {
        if tokio::time::Instant::now() >= deadline {
            info!(target: LOG_TARGET, "等了 {} 分钟仍不安静，照常重启", cfg.max_wait_minutes);
            return;
        }
        if !logged {
            info!(target: LOG_TARGET, "等群里安静 {} 秒再重启…", cfg.quiet_seconds);
            logged = true;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

// ================= 插件生命周期 =================

impl PluginConfig for RestartConfig {
    const NAME: &'static str = "restart";

    fn check(&self) -> Result<(), String> {
        for time in &self.times {
            if crate::clock::parse_clock(time).is_none() {
                return Err(format!("times 里的「{time}」不是 HH:MM"));
            }
        }
        Ok(())
    }
}

pub fn init(ctx: Context) -> BoxFuture<'static, Result<(), PluginError>> {
    Box::pin(async move {
        let cfg = get_config_or_default::<RestartConfig>(&ctx);
        if !cfg.enabled {
            return Ok(());
        }

        // 1. 例行维护
        for time in &cfg.times {
            let Some(clock) = crate::clock::parse_clock(time) else {
                warn!(target: LOG_TARGET, "无法解析维护时刻 [{time}]，跳过");
                continue;
            };
            use chrono::Timelike;
            let daily_ctx = ctx.clone();
            ctx.scheduler.add_daily_at(clock.hour(), clock.minute(), clock.second(), move || {
                let ctx = daily_ctx.clone();
                async move { maintain(ctx).await }
            });
        }
        if !cfg.times.is_empty() {
            let what = match (cfg.auto_update && crate::release::managed(), cfg.daily_restart) {
                (true, true) => "检查更新并重启",
                (true, false) => "检查更新（有新版本才重启）",
                (false, true) => "重启",
                (false, false) => "什么也不做",
            };
            info!(target: LOG_TARGET, "每天 {} {what}（系统本地时区）", cfg.times.join("、"));
        }

        // 2. 内存阈值监控(仅 Linux 支持读取自身 RSS)
        if cfg.memory_threshold_mb > 0 {
            let threshold = cfg.memory_threshold_mb;
            let interval_secs = cfg.memory_check_interval_minutes.max(1) * 60;
            let mem_ctx = ctx.clone();
            ctx.scheduler
                .add_interval(Duration::from_secs(interval_secs), move || {
                    let ctx = mem_ctx.clone();
                    async move {
                        match current_rss_mb() {
                            Some(mb) if mb >= threshold => {
                                if MAINTAINING.swap(true, Ordering::SeqCst) {
                                    return;
                                }
                                warn!(
                                    target: LOG_TARGET,
                                    "内存占用 {}MB 达到阈值 {}MB，安静下来就重启",
                                    mb,
                                    threshold
                                );
                                wait_quiet(&get_config_or_default::<RestartConfig>(&ctx)).await;
                                do_restart(&ctx, format!("内存超限 ({mb}MB >= {threshold}MB)"))
                                    .await;
                                MAINTAINING.store(false, Ordering::SeqCst);
                            }
                            Some(mb) => {
                                debug!(
                                    target: LOG_TARGET,
                                    "内存巡检: {}/{}MB",
                                    mb,
                                    threshold
                                );
                            }
                            None => {
                                debug!(
                                    target: LOG_TARGET,
                                    "当前平台不支持读取内存占用，跳过巡检"
                                );
                            }
                        }
                    }
                });
            info!(
                target: LOG_TARGET,
                "已开启内存监控: 阈值 {}MB，每 {} 分钟巡检一次",
                threshold,
                cfg.memory_check_interval_minutes
            );
        }

        Ok(())
    })
}

/// 例行维护：先把新版本换上（换文件不影响正在跑的进程），再等安静、重启。
async fn maintain(ctx: Context) {
    if MAINTAINING.swap(true, Ordering::SeqCst) {
        return;
    }
    let cfg = get_config_or_default::<RestartConfig>(&ctx);
    let mut reason = cfg.daily_restart.then(|| "每日定时".to_string());
    if cfg.auto_update && crate::release::managed() {
        match crate::release::check().await {
            Ok(Some(release)) => {
                info!(target: LOG_TARGET, "发现新版本 {}，正在下载…", release.version);
                match crate::release::install(&release).await {
                    Ok(()) => {
                        info!(target: LOG_TARGET, "新版本 {} 已换上，重启后生效", release.version);
                        reason = Some(format!(
                            "更新 {} → {}",
                            crate::release::VERSION,
                            release.version
                        ));
                    }
                    Err(error) => warn!(target: LOG_TARGET, "更新没装上：{error:#}"),
                }
            }
            Ok(None) => info!(target: LOG_TARGET, "已是最新版本 {}", crate::release::VERSION),
            Err(error) => warn!(target: LOG_TARGET, "查不到新版本：{error:#}"),
        }
    }
    if let Some(reason) = reason {
        wait_quiet(&cfg).await;
        do_restart(&ctx, reason).await;
    }
    MAINTAINING.store(false, Ordering::SeqCst);
}

pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        if let Some(_cmd) = match_word_command(&ctx, "restart") {
            if !crate::plugins::ctl::is_manager(&ctx) {
                let msg = ctx.as_message().unwrap();
                send_msg(
                    &ctx,
                    writer,
                    msg.group_id(),
                    Some(msg.user_id()),
                    Message::new().text(crate::plugins::ctl::DENIED),
                )
                .await?;
                return Ok(None);
            }
            let cfg = get_config_or_default::<RestartConfig>(&ctx);

            // 未开放手动重启时给出提示
            if !cfg.allow_manual_restart {
                let msg = ctx.as_message().unwrap();
                let prefix = get_prefixes(&ctx).first().cloned().unwrap_or_default();
                let reply = Message::new().reply(msg.message_id()).text(format!(
                    "❌ 重启指令未开放\n可用 {prefix}ctl set restart allow_manual_restart true 开启"
                ));
                send_msg(&ctx, writer, msg.group_id(), Some(msg.user_id()), reply).await?;
                return Ok(None);
            }

            // 防抖:重启已在进行中则忽略
            if RESTARTING.load(Ordering::SeqCst) {
                return Ok(None);
            }

            let msg = ctx.as_message().unwrap();
            let group_id = msg.group_id();
            let user_id = msg.user_id();
            let message_id = msg.message_id();
            let delay = cfg.restart_delay_seconds;

            let reply = Message::new()
                .reply(message_id)
                .text(format!("⏳ {delay} 秒后重启"));
            if let Err(e) = send_msg(&ctx, writer.clone(), group_id, Some(user_id), reply).await {
                error!(target: LOG_TARGET, "重启通知发送失败: {}", e);
            }

            // 延迟执行重启，确保回复消息已刷新到 WebSocket
            let restart_ctx = ctx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(delay)).await;
                do_restart(&restart_ctx, "手动指令".to_string()).await;
            });

            return Ok(None);
        }

        Ok(Some(ctx))
    })
}

// ================= 重启核心逻辑 =================

/// 智能体的 `acumen_plugins` 工具用：过一会儿重启。
///
/// 要等当前这一轮回复发出去——房间的卡片渲染加发送要好几秒，所以延迟不低于 8 秒。
/// 插件没开或已经在重启中时返回原因，工具把它如实转告。
pub(crate) fn schedule(ctx: &Context, reason: &str, delay: Duration) -> Result<Duration, String> {
    let delay = delay.max(Duration::from_secs(8));
    arm(ctx, reason, delay)?;
    Ok(delay)
}

/// 控制台上的那颗「重启」按钮用：回执写完就可以走，不必等群消息那 8 秒。
///
/// 与 `schedule` 共用同一道防抖与同一条请求，区别只在延迟；延迟是用来让这份
/// HTTP 回执先落到浏览器的，不能被压到零。
pub(crate) fn request_now(ctx: &Context, reason: &str, delay: Duration) -> Result<(), String> {
    arm(ctx, reason, delay)
}

fn arm(ctx: &Context, reason: &str, delay: Duration) -> Result<(), String> {
    if !get_config::<RestartConfig>(ctx).is_some_and(|cfg| cfg.enabled) {
        return Err("restart 插件没有开启，先用 enable 开启它".into());
    }
    if RESTARTING.load(Ordering::SeqCst) {
        return Err("已经有一次重启在进行中".into());
    }
    let ctx = ctx.clone();
    let reason = reason.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        do_restart(&ctx, reason).await;
    });
    Ok(())
}

/// 只提出请求，统一由主循环清理，避免定时任务中止自身或两个进程同时写配置。
async fn do_restart(ctx: &Context, reason: String) {
    if !get_config::<RestartConfig>(ctx).is_some_and(|cfg| cfg.enabled) {
        return;
    }
    if RESTARTING.swap(true, Ordering::SeqCst) {
        info!(
            target: LOG_TARGET,
            "重启流程已在进行中，忽略本次触发 ({})",
            reason
        );
        return;
    }
    info!(
        target: LOG_TARGET,
        "========== 开始重启 (原因: {}) ==========",
        reason
    );

    RESTART_REQUEST.notify_one();
}

/// 资源清理并保存成功后调用。exec 保留终端、PID、环境与 runit 的托管关系。
pub fn relaunch() -> Result<(), PluginError> {
    let exe = std::env::current_exe()?;
    // 自动更新会在运行期间换掉可执行文件，路径后面因此带上 " (deleted)"。
    let executable = exe.to_string_lossy();
    let executable = executable.strip_suffix(" (deleted)").unwrap_or(&executable);
    let mut cmd = std::process::Command::new(executable);
    cmd.args(std::env::args_os().skip(1));

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        info!(target: LOG_TARGET, "配置已保存，正在原地重启（保留 PID）...");
        // exec 只在失败时返回：正常路径下这个进程已经被新程序替掉了。
        Err(cmd.exec().into())
    }

    #[cfg(not(unix))]
    {
        let child = cmd.spawn()?;
        info!(target: LOG_TARGET, "已启动新进程，PID: {}", child.id());
        Ok(())
    }
}

// ================= 工具函数 =================

/// 读取当前进程 RSS 内存 (MB)。仅 Linux 提供 /proc/self/status，其余平台返回 None
pub(crate) fn current_rss_mb() -> Option<u64> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
                return Some(kb / 1024);
            }
        }
        None
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        None
    }
}

