//! 起停：网页上那颗「停止」调的，就是终端里的同一条命令。
//!
//! 控制台长在机器人进程里；管理面板（`acumen panel`）是同一个可执行文件的另一个进程。
//! 页面在两处都打得开，所以「停下来」这件事只做一份：网页把一句话交给
//! `$PREFIX/bin/acumen`，与终端里敲的是同一条路。两种停法也由它定义——
//! `stop` 只停机器人，`stop --all` 连管理面板与开机守护一起停。
//!
//! 两条约束：
//!
//! 1. **命令比回执晚一步，而且跑在自己的进程组里。** 停下命令的常常就是发出这条回执
//!    的机器人：先让回执写回浏览器，再动手；命令进了自己的进程组，机器人退出时也不会
//!    把它一起带走。
//! 2. **作业记录只有一份**（`data/console/panel-job`）。面板的「已停止」页靠它显示
//!    进度与最后几行输出，两个入口写的是同一份，不会各说各话。

use super::LOG_TARGET;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// 回执写回浏览器之后，等这么久再动手。手机忙的时候，回执也可能要几百毫秒才落到页面上。
pub(super) const GRACE_SECONDS: f64 = 1.2;

/// 网页能做的几件事。名字同时是作业记录里的一格（`start`/`stop`/…）与页面上的进度动词。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    Start,
    /// 只停机器人，管理面板继续运行——那是事后在网页上再启动它的那扇门。
    Stop,
    /// 机器人、管理面板与开机守护一起停。只有 `acumen stop --all` 会做到这一步。
    Halt,
    Restart,
    Update,
}

impl Action {
    pub(super) fn parse(name: &str) -> Option<Self> {
        match name {
            "start" => Some(Self::Start),
            "stop" => Some(Self::Stop),
            "halt" => Some(Self::Halt),
            "restart" => Some(Self::Restart),
            "update" => Some(Self::Update),
            _ => None,
        }
    }

    /// 交给管理命令的参数。`halt` 落到终端里是 `acumen stop --all`。
    fn args(self) -> &'static [&'static str] {
        match self {
            Self::Start => &["start", "-q"],
            Self::Stop => &["stop"],
            Self::Halt => &["stop", "--all"],
            Self::Restart => &["restart"],
            Self::Update => &["update"],
        }
    }

    /// 作业记录里的名字。页面按它把进度翻成人话。
    fn name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Halt => "halt",
            Self::Restart => "restart",
            Self::Update => "update",
        }
    }

    /// 回给页面的一句回执。它只说明「已经排上」，真正的结果由页面轮询。
    pub(super) fn message(self) -> &'static str {
        match self {
            Self::Start => "正在启动知微",
            Self::Stop => "正在停止知微",
            Self::Halt => "正在停止知微与管理面板",
            Self::Restart => "正在重启知微",
            Self::Update => "正在检查并安装更新",
        }
    }
}

/// 启动一个管理命令没成功的原因。
pub(super) enum SpawnError {
    /// 上一个操作还没结束。
    Busy,
    /// 这台设备上没有能用的管理命令或数据目录。
    Unavailable(String),
}

// ==================== 这台设备上的路径 ====================

pub(super) fn prefix() -> PathBuf {
    PathBuf::from(std::env::var("PREFIX").unwrap_or_else(|_| "/data/data/com.termux/files/usr".into()))
}

pub(super) fn service() -> PathBuf {
    prefix().join("var/service/acumen")
}

fn panel_service() -> PathBuf {
    prefix().join("var/service/acumen-panel")
}

/// 管理命令本身。它随可执行文件一起发布，`acumen setup` 之后才在。
fn cli() -> Option<PathBuf> {
    let path = prefix().join("bin/acumen");
    path.exists().then_some(path)
}

/// 网页上能不能起停：认的是**装好的那一份**。`cargo run` 出来的进程不归管理命令管，
/// 在那儿点停止只会把线上那个停掉，所以干脆不出现这枚按钮。
pub(super) fn available() -> bool {
    crate::release::managed() && cli().is_some() && service().exists()
}

/// 机器人进程的 PID（runit 记下的那个）；进程不在就是 `None`。
pub(super) fn bot_pid() -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(service().join("supervise/pid"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    std::path::Path::new(&format!("/proc/{pid}")).exists().then_some(pid)
}

/// 管理面板的 PID。它跟机器人是同一个可执行文件，所以两处都按 runit 记的 PID 认，
/// 不去翻进程表——那份 `pgrep` 分不清谁是谁。
fn panel_pid() -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(panel_service().join("supervise/pid"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    std::path::Path::new(&format!("/proc/{pid}")).exists().then_some(pid)
}

/// 管理面板此刻答不答得出来。页面用它决定「停下来之后还能不能从这里再启动」。
pub(super) fn panel_ready() -> bool {
    panel_pid().is_some()
}

/// 面板地址（带口令）。与启动日志里打印的那条同一个拼法。
pub(super) fn panel_url(bind: &str, port: u16, token: &str) -> String {
    let host = super::state::display_host(bind);
    let encoded: String = url::form_urlencoded::byte_serialize(token.as_bytes()).collect();
    format!("http://{host}:{port}/?t={encoded}")
}

/// 机器人这次被 runit 拉起以来的秒数（管理命令启动时记下的时刻；原地重启不重记）。
pub(super) fn uptime() -> Option<u64> {
    let started: u64 = std::fs::read_to_string(prefix().join("var/run/acumen.started"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    now.checked_sub(started)
}

// ==================== 作业记录 ====================

fn job_dir() -> Option<PathBuf> {
    crate::storage::data_path("console").ok().map(|dir| dir.join("panel-job"))
}

pub(super) fn job_running() -> bool {
    let Some(dir) = job_dir() else {
        return false;
    };
    if dir.join("code").exists() {
        return false;
    }
    std::fs::read_to_string(dir.join("pid"))
        .ok()
        .and_then(|pid| pid.trim().parse::<u32>().ok())
        .is_some_and(|pid| std::path::Path::new(&format!("/proc/{pid}")).exists())
}

/// 最近一次操作：命令名、开始时刻、还在不在跑、退出码与最后几行输出。
pub(super) fn job() -> Value {
    let Some(dir) = job_dir() else {
        return Value::Null;
    };
    let Ok(command) = std::fs::read_to_string(dir.join("command")) else {
        return Value::Null;
    };
    let output = std::fs::read_to_string(dir.join("output")).unwrap_or_default();
    let output = strip_ansi(&output);
    let tail: Vec<&str> = output.lines().filter(|line| !line.trim().is_empty()).collect();
    let started = std::fs::metadata(dir.join("command"))
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |time| time.as_secs());
    json!({
        "command": command.trim(),
        "started": started,
        "running": job_running(),
        "code": std::fs::read_to_string(dir.join("code")).ok().and_then(|code| code.trim().parse::<i32>().ok()),
        "output": tail[tail.len().saturating_sub(12)..].join("\n"),
    })
}

pub(super) fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ==================== 交给管理命令 ====================

/// 启动一个管理命令：先记下这次操作，再让它进自己的进程组、晚一步动手。
///
/// 输出与退出码落在作业目录里，面板的停机页据此显示进度；这里不等它结束——
/// 等的那个人可能就是马上要被停掉的机器人。
pub(super) fn spawn(action: Action) -> Result<(), SpawnError> {
    if job_running() {
        return Err(SpawnError::Busy);
    }
    let dir = job_dir().ok_or_else(|| SpawnError::Unavailable("数据目录不可用".into()))?;
    let cli = cli().ok_or_else(|| {
        SpawnError::Unavailable("找不到管理命令 acumen，先在终端运行一次 acumen setup".into())
    })?;
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(dir.join("command"), action.name()))
        .map_err(|error| SpawnError::Unavailable(format!("记不下这次操作：{error}")))?;
    let mut command = Command::new(prefix().join("bin/sh"));
    command
        .arg("-c")
        // 先 cd 进作业目录（输出相对落盘），睡过回执那一小段，再原地换成管理命令。
        .arg(r#"cd "$1" || exit 1; shift; sleep "$1"; shift; "$@" > output 2>&1; echo $? > code"#)
        .arg("sh")
        .arg(&dir)
        .arg(GRACE_SECONDS.to_string())
        .arg(prefix().join("bin/bash"))
        .arg(&cli)
        .args(action.args())
        .env("ACUMEN_NO_UI", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    match command.spawn() {
        Ok(child) => {
            let _ = std::fs::write(dir.join("pid"), child.id().to_string());
            // 不等它结束，但也不能把它丢给僵尸：异步地看一眼就好。
            // **不能用 `spawn_blocking` 去 wait**——运行时退出时会等阻塞任务，而这里等的
            // 正是「acumen stop」，它等的又是机器人退出，绕成一个 35 秒的死结。
            tokio::spawn(async move {
                let mut child = child;
                while let Ok(None) = child.try_wait() {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            });
            info!(target: LOG_TARGET, "控制台执行 acumen {}", action.args().join(" "));
            Ok(())
        }
        Err(error) => Err(SpawnError::Unavailable(format!("管理命令没跑起来：{error}"))),
    }
}
