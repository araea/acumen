//! 发布版的自我维护：查新版本、换上新版本，以及把管理命令与 root 守护放到位。
//!
//! 发布版装在 `~/acumen/`（可执行文件、`config.toml`、`data/` 同在一处），由管理命令
//! `acumen` 与 runit 托管。从源码 `cargo build` 出来的（在 `target/` 底下）不算发布版：
//! 不自动更新，也不去碰 PATH 上的管理命令。

use anyhow::{Context as _, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

const LOG_TARGET: &str = "Release";

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const MANIFEST: &str = "https://github.com/araea/acumen/releases/latest/download/update.json";

/// 管理命令，装到 `$PREFIX/bin/acumen`。
const CLI: &str = include_str!("../scripts/acumen");
/// root 守护，装到 `/data/adb/service.d/acumen.sh`（开机由 KernelSU / Magisk 拉起）。
const KEEPER: &str = include_str!("../scripts/keeper.sh");
const KEEPER_PATH: &str = "/data/adb/service.d/acumen.sh";

#[derive(Debug, Clone)]
pub struct Release {
    pub version: String,
    url: String,
    sha256: String,
    pub notes: String,
}

fn exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let text = exe.to_string_lossy();
    Some(PathBuf::from(
        text.strip_suffix(" (deleted)").unwrap_or(&text),
    ))
}

/// 是不是发布版（不在 cargo 的 `target/` 底下）。
pub fn managed() -> bool {
    exe().is_some_and(|exe| !exe.components().any(|c| c.as_os_str() == "target"))
}

fn prefix() -> String {
    std::env::var("PREFIX").unwrap_or_else(|_| "/data/data/com.termux/files/usr".into())
}

/// 版本号逐段按数字比；`a` 比 `b` 新才算。
fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split(['.', '-'])
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    parts(a) > parts(b)
}

fn client() -> Result<reqwest::Client> {
    Ok(crate::http::builder()
        .timeout(Duration::from_mins(15))
        .build()?)
}

/// 机器人自己最近一次去查新版本的时刻（文件的修改时间）。管理命令的查询不记，
/// 免得 `acumen status` 看到有新版之后的那次重启反倒不去换。
fn stamp() -> Option<PathBuf> {
    crate::storage::data_path("release")
        .ok()
        .map(|dir| dir.join("last-check"))
}

pub fn mark_checked() {
    if let Some(path) = stamp()
        && let Err(error) = crate::storage::write_atomic(&path, b"")
    {
        warn!(target: LOG_TARGET, "记不下查更新的时刻：{error}");
    }
}

/// 这么久以内查过新版本。
pub fn checked_within(window: Duration) -> bool {
    stamp()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| time.elapsed().ok())
        .is_some_and(|age| age < window)
}

/// 查一次最新发布；比当前新才返回。
pub async fn check() -> Result<Option<Release>> {
    let manifest: serde_json::Value = client()?
        .get(MANIFEST)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("更新清单读不出来")?;
    let field = |key: &str| {
        manifest
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let release = Release {
        version: field("version"),
        url: field("url"),
        sha256: field("sha256").to_lowercase(),
        notes: field("notes"),
    };
    if release.version.is_empty() || release.url.is_empty() || release.sha256.len() != 64 {
        bail!("更新清单缺字段");
    }
    Ok(newer(&release.version, VERSION).then_some(release))
}

/// 下载、校验、换上新的可执行文件。正在跑的进程不受影响，重启后才是新版本。
pub async fn install(release: &Release) -> Result<()> {
    let target = exe().ok_or_else(|| anyhow!("找不到自己的可执行文件"))?;
    install_to(release, target).await
}

async fn install_to(release: &Release, target: PathBuf) -> Result<()> {
    let packed = client()?
        .get(&release.url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await
        .context("下载中断")?;
    let expected = release.sha256.clone();
    let binary = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let mut binary = Vec::new();
        flate2::read::GzDecoder::new(&packed[..])
            .read_to_end(&mut binary)
            .context("解压失败")?;
        let actual = hex::encode(Sha256::digest(&binary));
        if actual != expected {
            bail!("校验不符（{actual}），没有换上");
        }
        Ok(binary)
    })
    .await??;
    crate::storage::write_atomic_async(target.clone(), binary).await?;
    set_mode(&target, 0o700)?;
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> std::io::Result<()> {
    Ok(())
}

/// 启动时：管理命令跟着可执行文件一起走，自动更新之后它也是新的。
pub async fn refresh() {
    if !managed() {
        return;
    }
    let cli = PathBuf::from(prefix()).join("bin/acumen");
    if std::fs::read_to_string(&cli).ok().as_deref() != Some(CLI) {
        match write_cli(&cli) {
            Ok(()) => info!(target: LOG_TARGET, "管理命令已更新：{}", cli.display()),
            Err(error) => warn!(target: LOG_TARGET, "管理命令写不进 {}：{error}", cli.display()),
        }
    }
    // root 守护只在装过的前提下跟着换（装不装由 acumen setup 决定）。本地留一份上次装的，
    // 一样就不必每次启动都去敲 su。
    let Ok(copy) = crate::storage::data_path("release").map(|dir| dir.join("keeper.sh")) else {
        return;
    };
    if std::fs::read_to_string(&copy).ok().as_deref() == Some(KEEPER) {
        return;
    }
    if !copy.exists() {
        return;
    }
    match install_keeper().await {
        Ok(()) => info!(target: LOG_TARGET, "root 守护已更新"),
        Err(error) => warn!(target: LOG_TARGET, "root 守护没能更新：{error}"),
    }
}

fn write_cli(path: &Path) -> std::io::Result<()> {
    crate::storage::write_atomic(path, CLI.as_bytes())?;
    set_mode(path, 0o755)
}

/// 经 su 把 root 守护写进 service.d 并（重新）拉起它。
async fn install_keeper() -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let script = format!(
        "cat > {KEEPER_PATH}.new && chmod 755 {KEEPER_PATH}.new && mv -f {KEEPER_PATH}.new {KEEPER_PATH} && sh {KEEPER_PATH} restart"
    );
    let mut child = tokio::process::Command::new("su")
        .args(["-c", &script])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("没有 su")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(KEEPER.as_bytes()).await?;
    }
    let output = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .context("su 没有回应")??;
    if !output.status.success() {
        bail!(
            "su 失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let copy = crate::storage::data_path("release")?.join("keeper.sh");
    crate::storage::write_atomic(&copy, KEEPER.as_bytes())?;
    Ok(())
}

/// 管理命令调用的几个隐藏参数。返回 `None` 表示不是它们，照常启动。
pub async fn command(arg: &str) -> Option<i32> {
    let code = match arg {
        "--version" | "-V" => {
            println!("{VERSION}");
            0
        }
        "--check-update" => match check().await {
            Ok(Some(release)) => {
                println!("{}", release.version);
                0
            }
            Ok(None) => 1,
            Err(error) => {
                eprintln!("查不到新版本：{error:#}");
                2
            }
        },
        "--update" => match check().await {
            Ok(Some(release)) => {
                eprintln!("正在下载 {} …", release.version);
                match install(&release).await {
                    Ok(()) => {
                        println!("{}", release.version);
                        if !release.notes.is_empty() {
                            eprintln!("{}", release.notes);
                        }
                        0
                    }
                    Err(error) => {
                        eprintln!("更新失败：{error:#}");
                        2
                    }
                }
            }
            Ok(None) => 1,
            Err(error) => {
                eprintln!("查不到新版本：{error:#}");
                2
            }
        },
        "--install-cli" => {
            let path = PathBuf::from(prefix()).join("bin/acumen");
            match write_cli(&path) {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("写不进 {}：{error}", path.display());
                    2
                }
            }
        }
        "--install-keeper" => match install_keeper().await {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("{error:#}");
                2
            }
        },
        _ => return None,
    };
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真去 GitHub 拉一次最新发布，解压校验后写到临时目录（不碰自己的可执行文件）：
    /// `cargo test --release live_install -- --ignored`
    #[tokio::test]
    #[ignore = "需要网络与已发布的版本"]
    async fn live_install() {
        let manifest: serde_json::Value = client()
            .unwrap()
            .get(MANIFEST)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let release = Release {
            version: manifest["version"].as_str().unwrap().into(),
            url: manifest["url"].as_str().unwrap().into(),
            sha256: manifest["sha256"].as_str().unwrap().into(),
            notes: String::new(),
        };
        let dir = std::env::temp_dir().join(format!("acumen-live-install-{}", std::process::id()));
        let target = dir.join("acumen");
        install_to(&release, target.clone()).await.unwrap();
        let out = std::process::Command::new(&target)
            .arg("--version")
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), release.version);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn compares_versions_numerically() {
        assert!(newer("1.10.0", "1.9.3"));
        assert!(newer("v1.0.1", "1.0.0"));
        assert!(!newer("1.0.0", "1.0.0"));
        assert!(!newer("0.9.9", "1.0.0"));
    }
}
