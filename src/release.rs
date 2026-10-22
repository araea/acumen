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

/// 发布清单的签名公钥（Ed25519）。私钥只在维护者手里（`scripts/release.sh` 用它签名），
/// 所以经第三方加速站转来的清单与可执行文件也信得过：签名对上了，校验和才作数。
const PUBLIC_KEY: &str = "cf7ea9cc07dc55c141978b75225962705a1aeef336ce57db224f0919285ad363";

/// 内置的 GitHub 加速前缀。国内网络直连 GitHub 常常超时，这几家能把同一个地址原样转过来；
/// 失效了也不要紧，同时问、谁先回用谁，`[restart] mirrors` 还能再补。
const MIRRORS: &[&str] = &[
    "https://gh-proxy.com/",
    "https://cdn.gh-proxy.com/",
    "https://gh.llkk.cc/",
    "https://gh.idayer.com/",
    "https://ghfast.top/",
];

/// 每个源取清单的时限。
const MANIFEST_TIMEOUT: Duration = Duration::from_secs(12);
/// 第一份清单到手后再等别的源这么久，取其中最新的：加速站偶尔缓存着旧清单。
const MANIFEST_GRACE: Duration = Duration::from_millis(1500);
/// 下载时这么久一个字节都没收到，就换下一个源。
const STALL: Duration = Duration::from_secs(20);
/// 下载过了这么久平均仍低于 `MIN_SPEED`（字节/秒），且后面还有源可换，就换。
const SLOW_AFTER: Duration = Duration::from_secs(20);
const MIN_SPEED: f64 = 48.0 * 1024.0;

#[derive(Debug, Clone)]
pub struct Release {
    pub version: String,
    url: String,
    sha256: String,
    pub notes: String,
    /// 下载源（加速前缀，空串是直连），清单回得快的排前面。
    sources: Vec<String>,
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

fn version_parts(v: &str) -> Vec<u64> {
    v.trim_start_matches('v')
        .split(['.', '-'])
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

/// 版本号逐段按数字比；`a` 比 `b` 新才算。
fn newer(a: &str, b: &str) -> bool {
    version_parts(a) > version_parts(b)
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

/// restart 插件载入时交来的 `mirrors`；没交过（管理命令、管理面板这些别的进程）就读
/// 可执行文件旁边的 config.toml。
static CUSTOM_MIRRORS: std::sync::RwLock<Option<Vec<String>>> = std::sync::RwLock::new(None);

pub fn set_mirrors(mirrors: Vec<String>) {
    *CUSTOM_MIRRORS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(mirrors);
}

fn custom_mirrors() -> Vec<String> {
    if let Some(mirrors) = CUSTOM_MIRRORS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        return mirrors;
    }
    let Some(path) = exe().and_then(|exe| Some(exe.parent()?.join("config.toml"))) else {
        return Vec::new();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .and_then(|table| table.get("restart")?.get("mirrors")?.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

/// 全部下载源：自定义的、直连、内置加速，去重。前缀统一以 `/` 结尾。
fn sources() -> Vec<String> {
    let mut list: Vec<String> = Vec::new();
    let custom = custom_mirrors().into_iter().map(|m| {
        let m = m.trim().to_string();
        if m.ends_with('/') { m } else { format!("{m}/") }
    });
    for source in custom
        .chain(std::iter::once(String::new()))
        .chain(MIRRORS.iter().map(|m| (*m).to_string()))
    {
        if !list.contains(&source) {
            list.push(source);
        }
    }
    list
}

/// 下载源在日志与回执里的叫法。
fn label(source: &str) -> String {
    if source.is_empty() {
        return "GitHub".into();
    }
    reqwest::Url::parse(source)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| source.to_string())
}

/// 失败原因压成几个字：一次要列好几个源，整条错误链太长。
fn short(error: &anyhow::Error) -> String {
    if let Some(error) = error.downcast_ref::<reqwest::Error>() {
        if error.is_timeout() {
            return "超时".into();
        }
        if error.is_connect() {
            return "连不上".into();
        }
        if let Some(status) = error.status() {
            return format!("HTTP {}", status.as_u16());
        }
    }
    error.to_string()
}

fn signed_message(version: &str, sha256: &str) -> String {
    format!("acumen {version} {sha256}")
}

fn verify(version: &str, sha256: &str, signature: &str) -> Result<()> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let signature = STANDARD
        .decode(signature.trim())
        .map_err(|_| anyhow!("签名读不出来"))?;
    let key = hex::decode(PUBLIC_KEY)?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(signed_message(version, sha256).as_bytes(), &signature)
        .map_err(|_| anyhow!("签名不符"))
}

async fn fetch_manifest(client: &reqwest::Client, source: &str) -> Result<Release> {
    let manifest: serde_json::Value = client
        .get(format!("{source}{MANIFEST}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .map_err(|_| anyhow!("清单读不出来"))?;
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
        sources: Vec::new(),
    };
    if release.version.is_empty() || release.url.is_empty() || release.sha256.len() != 64 {
        bail!("清单缺字段");
    }
    verify(&release.version, &release.sha256, &field("signature"))?;
    Ok(release)
}

/// 查一次最新发布；比当前新才返回。
///
/// 所有源同时问：第一份签名对得上的清单到手后再稍等片刻，取其中版本最新的；
/// 下载时按回来的先后试。
pub async fn check() -> Result<Option<Release>> {
    use futures_util::StreamExt as _;
    let client = crate::http::builder().timeout(MANIFEST_TIMEOUT).build()?;
    let sources = sources();
    let mut pending: futures_util::stream::FuturesUnordered<_> = sources
        .iter()
        .map(|source| {
            let client = client.clone();
            async move { (source, fetch_manifest(&client, source).await) }
        })
        .collect();
    let mut found: Vec<(&String, Release)> = Vec::new();
    let mut errors = Vec::new();
    let mut deadline = None;
    loop {
        let next = match deadline {
            Some(deadline) => match tokio::time::timeout_at(deadline, pending.next()).await {
                Ok(next) => next,
                Err(_) => break,
            },
            None => pending.next().await,
        };
        let Some((source, result)) = next else { break };
        match result {
            Ok(release) => {
                deadline.get_or_insert_with(|| tokio::time::Instant::now() + MANIFEST_GRACE);
                found.push((source, release));
            }
            Err(error) => {
                debug!(target: LOG_TARGET, "{} 取不到更新清单：{error:#}", label(source));
                errors.push(format!("{} {}", label(source), short(&error)));
            }
        }
    }
    let Some(best) = found
        .iter()
        .map(|(_, release)| release)
        .max_by(|a, b| version_parts(&a.version).cmp(&version_parts(&b.version)))
        .cloned()
    else {
        bail!("{} 个下载源都不通（{}）", sources.len(), errors.join("、"));
    };
    let mut order: Vec<String> = found
        .iter()
        .filter(|(_, release)| release.version == best.version)
        .map(|(source, _)| (*source).clone())
        .collect();
    for source in &sources {
        if !order.contains(source) {
            order.push(source.clone());
        }
    }
    Ok(newer(&best.version, VERSION).then_some(Release {
        sources: order,
        ..best
    }))
}

/// 最近一次查询的结果与时刻：控制台每次打开都要问一句「有没有新版」，不必每次都去 GitHub。
type Cached = (std::time::Instant, Option<Release>);

fn last_seen() -> &'static std::sync::Mutex<Option<Cached>> {
    static LAST: std::sync::Mutex<Option<Cached>> = std::sync::Mutex::new(None);
    &LAST
}

/// 和 [`check`] 一样，但 `max_age` 以内查过就用上次的结果。
pub async fn check_cached(max_age: Duration) -> Result<Option<Release>> {
    if let Some((at, release)) = last_seen()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        && at.elapsed() < max_age
    {
        return Ok(release.clone());
    }
    let release = check().await?;
    *last_seen()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some((std::time::Instant::now(), release.clone()));
    Ok(release)
}

/// 下载、校验、换上新的可执行文件；返回用的是哪个下载源。正在跑的进程不受影响，
/// 重启后才是新版本。
pub async fn install(release: &Release) -> Result<String> {
    let target = exe().ok_or_else(|| anyhow!("找不到自己的可执行文件"))?;
    let client = client()?;
    let mut errors = Vec::new();
    for (index, source) in release.sources.iter().enumerate() {
        let patient = index + 1 == release.sources.len();
        let url = format!("{source}{}", release.url);
        let result = async {
            let packed = download(&client, &url, patient).await?;
            unpack(packed, release.sha256.clone()).await
        }
        .await;
        match result {
            Ok(binary) => {
                crate::storage::write_atomic_async(target.clone(), binary).await?;
                set_mode(&target, 0o700)?;
                return Ok(label(source));
            }
            Err(error) => {
                warn!(target: LOG_TARGET, "经 {} 下载没成：{error:#}", label(source));
                errors.push(format!("{} {}", label(source), short(&error)));
            }
        }
    }
    bail!("每个下载源都没下成（{}）", errors.join("、"))
}

/// 下一个源还有指望时，慢得不像话就放弃这个；最后一个源只要还在动就等它。
async fn download(client: &reqwest::Client, url: &str, patient: bool) -> Result<Vec<u8>> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    let expected = response.content_length();
    let mut packed = Vec::with_capacity(expected.unwrap_or(0).try_into().unwrap_or(0));
    let started = tokio::time::Instant::now();
    loop {
        let chunk = tokio::time::timeout(STALL, response.chunk())
            .await
            .map_err(|_| anyhow!("{} 秒没收到数据", STALL.as_secs()))??;
        let Some(chunk) = chunk else { break };
        packed.extend_from_slice(&chunk);
        let elapsed = started.elapsed();
        #[allow(clippy::cast_precision_loss)]
        let speed = packed.len() as f64 / elapsed.as_secs_f64();
        if !patient && elapsed > SLOW_AFTER && speed < MIN_SPEED {
            bail!("太慢（{:.0} KB/s）", speed / 1024.0);
        }
    }
    if let Some(expected) = expected
        && packed.len() as u64 != expected
    {
        bail!("下载不完整");
    }
    Ok(packed)
}

async fn unpack(packed: Vec<u8>, expected: String) -> Result<Vec<u8>> {
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let mut binary = Vec::new();
        flate2::read::GzDecoder::new(&packed[..])
            .read_to_end(&mut binary)
            .map_err(|_| anyhow!("解压失败"))?;
        if hex::encode(Sha256::digest(&binary)) != expected {
            bail!("校验不符");
        }
        Ok(binary)
    })
    .await?
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
                    Ok(via) => {
                        eprintln!("经 {via} 下载");
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

    #[test]
    fn compares_versions_numerically() {
        assert!(newer("1.10.0", "1.9.3"));
        assert!(newer("v1.0.1", "1.0.0"));
        assert!(!newer("1.0.0", "1.0.0"));
        assert!(!newer("0.9.9", "1.0.0"));
    }

    #[test]
    fn verifies_release_signature() {
        let sha = "0".repeat(64);
        let signature = "qbooniEXS6mg0q9+ECV6O34V1bDuTqas5dTtYvBAonv0FOCvPiekMvTLz+2JHMzMMXIx/EVg+SrtRQD7HCG+DA==";
        assert!(verify("1.0.0", &sha, signature).is_ok());
        assert!(verify("1.0.1", &sha, signature).is_err());
        assert!(verify("1.0.0", &"1".repeat(64), signature).is_err());
        assert!(verify("1.0.0", &sha, "").is_err());
    }

    #[test]
    fn mirror_labels() {
        assert_eq!(label(""), "GitHub");
        assert_eq!(label("https://gh-proxy.com/"), "gh-proxy.com");
    }
}
