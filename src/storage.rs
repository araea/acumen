//! 插件数据的落盘：数据目录、原子写文件，以及「一份 JSON 状态」。
//!
//! 数据都在可执行文件旁的 `data/<插件>/` 下。这里把几种各插件原先各写一遍的做法收成
//! 一处：
//!
//! - [`data_path`]：数据目录怎么算只有一份，同步与异步调用方得到同一个路径；
//! - [`write_atomic`]：先写同目录下的临时文件、落盘、再改名，读的一方永远看不到写了
//!   一半的文件，进程在写的中途被杀也只会留下旧版本；
//! - [`JsonState`]：一份常驻内存、改完整份落盘的 JSON 状态（去重表、取片记录这类）。

use serde::{Serialize, de::DeserializeOwned};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex as AsyncMutex;

/// 插件数据目录（`<可执行文件目录>/data/<插件>`），不创建。
///
/// 写文件时会按需建父目录；要先把目录建好再用，走 [`crate::plugins::get_data_dir`]。
pub fn data_path(plugin: &str) -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| io::Error::other("无法确定可执行文件所在目录"))?;
    Ok(dir.join("data").join(plugin))
}

/// 临时文件名里的序号，让同一进程里同时写同一个文件的两次调用互不覆盖对方的临时文件。
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// 原子地把 `bytes` 写成 `path`：父目录不存在就建，写同目录下的临时文件并落盘，再改名覆盖。
///
/// 失败时清掉临时文件，原文件保持原样。
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_with(path, bytes, None)
}

/// 同 [`write_atomic`]，文件权限为 0600（只有属主能读写）。配置、密钥这类用它。
pub fn write_atomic_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_with(path, bytes, Some(0o600))
}

/// [`write_atomic`] 的异步版：写盘与落盘放进阻塞线程池，不占异步工作线程。
pub async fn write_atomic_async(path: PathBuf, bytes: Vec<u8>) -> io::Result<()> {
    blocking(move || write_atomic(&path, &bytes)).await
}

/// [`write_atomic_private`] 的异步版。
pub async fn write_atomic_private_async(path: PathBuf, bytes: Vec<u8>) -> io::Result<()> {
    blocking(move || write_atomic_private(&path, &bytes)).await
}

async fn blocking(work: impl FnOnce() -> io::Result<()> + Send + 'static) -> io::Result<()> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(io::Error::other)?
}

fn write_atomic_with(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let parent = path.parent().filter(|dir| !dir.as_os_str().is_empty());
    if let Some(dir) = parent {
        std::fs::create_dir_all(dir)?;
    }
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("路径没有文件名"))?
        .to_string_lossy();
    let temporary = path.with_file_name(format!(
        "{name}.tmp-{}-{}",
        std::process::id(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        #[cfg(not(unix))]
        let _ = mode;
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

// ================= 常驻 JSON 状态 =================

fn parse_json<T: DeserializeOwned>(text: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str(text)
}

/// 一份常驻内存、整份落盘的 JSON 状态，存在 `data/<插件>/<文件>`。
///
/// 首次访问时读盘；之后每次 [`with`](Self::with) 在锁内读改写，并把结果原子写回。
/// 读不出来的文件（损坏、被手改坏）不会被直接覆盖：先改名挪到一旁留作证据，再从空白
/// 开始。数据目录建不出来时状态只驻留内存，功能照常。
///
/// 声明成 `static`：
///
/// ```ignore
/// static STORE: JsonState<State> = JsonState::new(LOG_TARGET, "video_parse", "state.json", "取片记录");
/// ```
pub struct JsonState<T> {
    target: &'static str,
    plugin: &'static str,
    file: &'static str,
    /// 日志里怎么称呼这份数据（「去重状态」「取片记录」）
    label: &'static str,
    parse: fn(&str) -> Result<T, serde_json::Error>,
    cell: AsyncMutex<Option<T>>,
}

impl<T> JsonState<T>
where
    T: Serialize + DeserializeOwned + Default + Clone,
{
    pub const fn new(
        target: &'static str,
        plugin: &'static str,
        file: &'static str,
        label: &'static str,
    ) -> Self {
        Self::with_parser(target, plugin, file, label, parse_json::<T>)
    }

    /// 自带解析函数：文件要宽容读取（坏一条只丢一条）时用它。
    pub const fn with_parser(
        target: &'static str,
        plugin: &'static str,
        file: &'static str,
        label: &'static str,
        parse: fn(&str) -> Result<T, serde_json::Error>,
    ) -> Self {
        Self {
            target,
            plugin,
            file,
            label,
            parse,
            cell: AsyncMutex::const_new(None),
        }
    }

    /// 在锁内读改写状态，并把结果落盘。
    pub async fn with<R>(&self, change: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.cell.lock().await;
        let state = self.loaded(&mut guard).await;
        let result = change(state);
        let snapshot = state.clone();
        self.save(&snapshot).await;
        result
    }

    /// 提前把文件读进内存（插件初始化时调用），免得第一次用到时才读盘。
    pub async fn preload(&self) {
        let mut guard = self.cell.lock().await;
        self.loaded(&mut guard).await;
    }

    async fn loaded<'a>(&self, slot: &'a mut Option<T>) -> &'a mut T {
        if slot.is_none() {
            *slot = Some(self.load().await);
        }
        slot.as_mut().expect("状态已在上一步载入")
    }

    fn path(&self) -> io::Result<PathBuf> {
        Ok(data_path(self.plugin)?.join(self.file))
    }

    async fn load(&self) -> T {
        let Ok(path) = self.path() else {
            warn!(target: self.target, "无法确定数据目录，{}本次仅驻留内存。", self.label);
            return T::default();
        };
        let Ok(content) = tokio::fs::read_to_string(&path).await else {
            return T::default();
        };
        match (self.parse)(&content) {
            Ok(state) => state,
            Err(error) => {
                // 别直接覆盖：挪到一旁留作证据，也方便手工抢救。
                let aside = path.with_file_name(format!(
                    "{}.broken-{}",
                    self.file,
                    chrono::Utc::now().timestamp()
                ));
                let moved = tokio::fs::rename(&path, &aside).await.is_ok();
                warn!(
                    target: self.target,
                    "{}文件无法解析（{}），{}，将重新开始记录。",
                    self.label,
                    error,
                    if moved {
                        format!("原文件已移到 {}", aside.display())
                    } else {
                        "原文件保持原样".to_string()
                    }
                );
                T::default()
            }
        }
    }

    async fn save(&self, state: &T) {
        let Ok(path) = self.path() else {
            return;
        };
        match serde_json::to_vec(state) {
            Ok(json) => {
                if let Err(error) = write_atomic_async(path, json).await {
                    warn!(target: self.target, "{}写入失败: {}", self.label, error);
                }
            }
            Err(error) => warn!(target: self.target, "{}序列化失败: {}", self.label, error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "acumen-storage-{name}-{}-{}",
            std::process::id(),
            TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn atomic_write_creates_parents_replaces_and_leaves_no_temporary() {
        let dir = scratch("write");
        let path = dir.join("nested/deeper/state.json");
        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, ["state.json"], "临时文件没清掉");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn private_write_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("private");
        let path = dir.join("secret.toml");
        write_atomic_private(&path, b"token = 1").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_write_keeps_the_old_file() {
        let dir = scratch("failed");
        let path = dir.join("keep.json");
        write_atomic(&path, b"old").unwrap();
        // 目标是个目录：改名必然失败，旧内容与临时文件清理都要照常。
        let blocked = dir.join("blocked");
        std::fs::create_dir_all(blocked.join("child")).unwrap();
        assert!(write_atomic(&blocked, b"new").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().all(|name| !name.contains(".tmp-")),
            "{names:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
