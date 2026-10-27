//! bash 工具：在房间的工作目录里跑一条命令，把合并后的输出交回模型。
//!
//! 这里的边界与从前那套外部 CLI 一致——**没有沙箱**。工具本身能做的事 bash 都能做，
//! 真正的边界在「谁能在房间里说话」这一层。留给这里的只有三样：超时、
//! 输出上限，以及取消时把整棵进程树收干净。
//!
//! 进程组终止放在这个文件里是因为它必须紧贴 `Command`：agent 的一轮对话可能被
//! 外层超时或停止指令 drop 掉，`ProcessGroup` 随 future 一起销毁才来得及在那一刻
//! 杀进程；一旦把子进程 `tokio::spawn` 出去，drop 就只是丢掉一个句柄。

use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// 交给模型的输出上限（字符）；超出部分从中间省略。
const MAX_OUTPUT: usize = 32_000;
/// 读取阶段的字节上限。比交付上限宽松得多，是为了让「中间省略」能同时留下
/// 命令开头的报错与结尾的结果——边读边砍只会把尾巴整段丢掉。
const MAX_COLLECT: usize = 1024 * 1024;

/// 单条命令的默认与最长时限（秒）。
const DEFAULT_TIMEOUT: u64 = 120;
const MAX_TIMEOUT: u64 = 600;

/// 跑得比这久的命令在回执里写上用时：模型据此知道下次该不该放到后台。
const SLOW: std::time::Duration = std::time::Duration::from_secs(20);

/// 跑一条命令；返回合并后的 stdout + stderr。
///
/// `spill` 是本轮的临时目录：输出超过交付上限时，完整输出（读取上限以内）落一份到那里，
/// 回执末尾写明路径——模型要看被省略的那一段就 `grep` / `read` 那个文件，而不是把命令
/// 再跑一遍（耗时的编译、下载再跑一遍可不止多花几秒）。
pub(crate) async fn run(
    command: &str,
    timeout_seconds: Option<u64>,
    cwd: Option<&std::path::Path>,
    env: &[(String, String)],
    spill: Option<&std::path::Path>,
) -> anyhow::Result<String> {
    let started = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(
        timeout_seconds
            .unwrap_or(DEFAULT_TIMEOUT)
            .clamp(1, MAX_TIMEOUT),
    );

    let mut process = Command::new("bash");
    #[cfg(unix)]
    process.process_group(0);
    if let Some(cwd) = cwd {
        process.current_dir(cwd);
    }
    for (key, value) in env {
        process.env(key, value);
    }
    let mut child = process
        .args(["-lc", command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| anyhow::anyhow!("无法启动 bash：{error}"))?;

    #[cfg(unix)]
    let mut group = ProcessGroup {
        pid: child.id().expect("spawned child has a PID") as i32,
        armed: true,
    };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("bash stdout 不可用"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("bash stderr 不可用"))?;

    // 输出收进外面的缓冲：超时被丢掉的是读取的 future，已经读到的不该跟着丢——
    // 一条跑了两分钟的命令，超时前打出来的进度和报错往往正是要看的。
    let mut out = Vec::new();
    let mut err = Vec::new();
    let finished = {
        let collect = async {
            // 两路一起排空：只读一路会在另一路写满管道时互相卡死。
            tokio::join!(
                read_capped(stdout, &mut out, MAX_COLLECT),
                read_capped(stderr, &mut err, MAX_COLLECT)
            );
            child.wait().await
        };
        tokio::time::timeout(timeout, collect).await
    };
    let Ok(status) = finished else {
        // 超时即放弃这一轮：进程组连同 bash 派生的独立进程一起收掉（`group` 在返回时 drop）。
        let partial = merged(&out, &err, spill);
        return Err(anyhow::anyhow!(
            "命令超过 {} 秒未结束，已终止{}",
            timeout.as_secs(),
            if partial.is_empty() {
                String::new()
            } else {
                format!("；终止前的输出：\n{partial}")
            }
        ));
    };
    let status = status?;
    #[cfg(unix)]
    {
        group.armed = false;
    }

    let mut text = merged(&out, &err, spill);
    let elapsed = started.elapsed();
    if elapsed >= SLOW {
        let note = format!("（用时 {} 秒）", elapsed.as_secs());
        text = if text.is_empty() { note } else { format!("{text}\n{note}") };
    }
    if !status.success() {
        // 非零退出把状态写清楚再交回去：命令失败本身也是有用的结果，模型据此改做法。
        anyhow::bail!(
            "命令以 {} 退出{}",
            status.code().unwrap_or(-1),
            if text.is_empty() {
                String::new()
            } else {
                format!("：\n{text}")
            }
        );
    }
    if text.is_empty() {
        return Ok("（命令没有输出）".to_string());
    }
    Ok(text)
}

/// 取消时结束整个调用树；bash 会为管道与后台任务创建独立进程组。
#[cfg(unix)]
struct ProcessGroup {
    pid: i32,
    armed: bool,
}
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        #[cfg(any(target_os = "linux", target_os = "android"))]
        kill_descendants(self.pid);
        // SAFETY: 本次 spawn 创建独立进程组，负 PID 不指向 bot 所在组。
        unsafe {
            libc::kill(-self.pid, libc::SIGKILL);
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn kill_descendants(pid: i32) {
    // 先暂停父进程，避免遍历时继续产生新子进程。
    // Android 内核可能没有 /proc/PID/task/TID/children，使用 stat 的 PPID。
    // SAFETY: pid 来自本次子进程及其 /proc 父子关系，不对用户输入发送信号。
    if unsafe { libc::kill(pid, libc::SIGSTOP) } != 0 {
        return;
    }
    if let Ok(processes) = std::fs::read_dir("/proc") {
        for process in processes.flatten() {
            let Some(child) = process
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<i32>().ok())
            else {
                continue;
            };
            if child <= 0 || child == pid {
                continue;
            }
            if let Ok(stat) = std::fs::read_to_string(process.path().join("stat")) {
                let parent = stat
                    .rsplit_once(") ")
                    .and_then(|(_, fields)| fields.split_whitespace().nth(1))
                    .and_then(|id| id.parse::<i32>().ok());
                if parent == Some(pid) {
                    kill_descendants(child);
                }
            }
        }
    }
    // SAFETY: 同上，只终止本次请求的子进程。
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}

/// 把 stdout 与 stderr 合成交给模型的文本：stderr 另起一行接在后面，整体超限时从中间省略。
///
/// 省略了东西且有 `spill` 目录时，完整输出另存一份，回执末尾写明路径。
fn merged(out: &[u8], err: &[u8], spill: Option<&std::path::Path>) -> String {
    let mut text = String::from_utf8_lossy(out).into_owned();
    let err = String::from_utf8_lossy(err);
    if !err.trim().is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&err);
    }
    let text = text.trim_end();
    if text.chars().count() <= MAX_OUTPUT {
        return text.to_string();
    }
    let short = elide(text, MAX_OUTPUT);
    let capped = out.len() >= MAX_COLLECT || err.len() >= MAX_COLLECT;
    let Some(dir) = spill else { return short };
    let file = dir.join(format!("bash-{:08x}.log", rand::random::<u32>()));
    match std::fs::write(&file, text) {
        Ok(()) => format!(
            "{short}\n（输出共 {} 行，中间有省略；完整输出{}已存到 {}，要看哪一段用 grep 或 read 的 offset 去读，别把命令重跑一遍）",
            text.lines().count(),
            if capped { "（前 1 MiB）" } else { "" },
            file.display()
        ),
        Err(_) => short,
    }
}

/// 超长输出留头尾、按整行切，中间写明省了多少行。
///
/// 尾巴多留：命令的结论（报错的最后一行、测试汇总、最终状态）几乎总在末尾；开头留的是
/// 命令最早的报错与上下文。只用一个「…」，模型不知道中间丢了多少，常把残缺的输出当全部。
fn elide(text: &str, max_chars: usize) -> String {
    let head_budget = max_chars / 3;
    let tail_budget = max_chars - head_budget;
    let lines: Vec<&str> = text.lines().collect();
    let mut head = 0;
    let mut used = 0;
    while head < lines.len() && used + lines[head].chars().count() < head_budget {
        used += lines[head].chars().count() + 1;
        head += 1;
    }
    let mut tail = lines.len();
    used = 0;
    while tail > head && used + lines[tail - 1].chars().count() < tail_budget {
        used += lines[tail - 1].chars().count() + 1;
        tail -= 1;
    }
    if head == 0 && tail == lines.len() {
        // 一两行就撑爆上限（压缩过的 JSON、进度条）：按字符从中间省。
        return super::super::utils::truncate_middle(text, max_chars);
    }
    format!(
        "{}\n…（中间省略 {} 行）…\n{}",
        lines[..head].join("\n"),
        tail - head,
        lines[tail..].join("\n")
    )
}

async fn read_capped(mut reader: impl tokio::io::AsyncRead + Unpin, kept: &mut Vec<u8>, max_bytes: usize) {
    let mut chunk = [0_u8; 4 * 1024];
    loop {
        let read = match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let remaining = max_bytes.saturating_sub(kept.len());
        kept.extend_from_slice(&chunk[..read.min(remaining)]);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn elide_keeps_whole_lines_and_counts_the_gap() {
        let text: String = (1..=2000).map(|n| format!("line {n}\n")).collect();
        let short = super::elide(text.trim_end(), 3000);
        assert!(short.starts_with("line 1\n"));
        assert!(short.ends_with("line 2000"));
        assert!(short.contains("（中间省略 "));
        assert!(short.chars().count() < 3200);
    }
}
