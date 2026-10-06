//! 排行榜头像：下载、缩成圆形、落盘缓存。
//!
//! 缓存键是头像地址的 md5（`h_<md5>_100.png`）：换了头像地址就换一份，跨平台也不会撞。
//! 一张头像的去向只有三种：
//!
//! - **缓存里有，且在新鲜期内**：直接用。
//! - **缓存里有，但过了新鲜期**：照样先用，同时在后台换新——出图不等网络，
//!   这一张榜多半还是旧头像，下一张就是新的。
//! - **缓存里没有**：现下载，挡着出图，但整个头像阶段有总时限，到点没拿到的用灰底默认头像。

use super::data_loader::BarData;
use super::utils::{avatar_theme_color, create_default_avatar, make_circular_avatar};
use crate::plugins::get_data_dir;
use futures_util::StreamExt;
use image::RgbaImage;
use image::imageops::FilterType;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, Once};
use std::time::{Duration, Instant, SystemTime};
use tokio::fs;

const AVATAR_SIZE: u32 = 100;

/// 缓存的新鲜期。过了这个年纪的头像仍会被用，只是会顺带在后台刷新。
const CACHE_FRESH: Duration = Duration::from_secs(3 * 86400);

/// 缓存文件的最长闲置期。文件只在换新时才会被改写，所以超过这个年纪就是
/// 一个月没上过任何一张榜，没有再留的理由；不清的话目录只增不减。
const CACHE_EVICT: Duration = Duration::from_secs(30 * 86400);

/// 单张头像的请求超时（含读完正文）。本机经代理实测一张约 0.3 秒，六秒是留足余量。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);

/// 一张榜等头像的总时长。
///
/// 没有这条总时限的话，断网时每六个一批、一批吃满单次超时：二十个人就是
/// 四批 × 八秒，出一张图要等半分钟（息屏后 Doze 掐网是真发生过的）。
/// 到点没拿到的直接用默认头像，榜照常出。
const PHASE_DEADLINE: Duration = Duration::from_secs(8);

/// 一张头像拉取失败（或被总时限截断）之后，这段时间内不再为它发请求。
/// 刷新过期缓存时它还兼作「已经在刷新了」的标记。
const RETRY_COOLDOWN: Duration = Duration::from_secs(60);

/// 总时限被打满，说明不是哪一张头像的问题而是网不通：冷却记在这个键上，
/// 这段时间里所有头像都不再现下载。只记在单张头像上不够——断网时每张榜的人
/// 不一样，下一张榜照样要再等满一个总时限。冷却期一过由下一张榜探路，通了就解除。
const NETWORK_DOWN: &str = "network";

/// 头像下载的并发上限。
///
/// 一张榜最多几十个人，`join_all` 会让他们同时开连接——手机上几十套 TLS 状态一起
/// 握手，既拖慢自己也把这一轮的全部结果拖到最慢的那一张图。六个是够用的档位：
/// 浏览器对同域名的并发本来也就这个量级，何况绝大多数命中本地缓存、根本不发请求。
const AVATAR_CONCURRENCY: usize = 6;

/// 批量处理头像下载与主题色提取
pub async fn prepare_avatars(data: &mut [BarData]) {
    let dir = match get_data_dir("stats").await {
        Ok(dir) => {
            let avatar_dir = dir.join("avatars");
            let _ = fs::create_dir_all(&avatar_dir).await;
            sweep_once(&avatar_dir);
            Some(avatar_dir)
        }
        Err(e) => {
            warn!(target: "Plugin/Stats", "无法获取数据目录: {}", e);
            None
        }
    };
    prepare_in(data, dir).await;
}

async fn prepare_in(data: &mut [BarData], dir: Option<PathBuf>) {
    let default_avatar = create_default_avatar(AVATAR_SIZE);
    let deadline = tokio::time::Instant::now() + PHASE_DEADLINE;

    let jobs: Vec<_> = data
        .iter()
        .map(|item| {
            let url = item.avatar_url.clone();
            let dir = dir.clone();
            async move {
                match url {
                    Some(url) => load_one(&url, dir.as_deref(), deadline).await,
                    None => None,
                }
            }
        })
        .collect();

    // `buffered` 而不是 `join_all`：结果顺序不变，但在飞请求有上限。
    let loaded: Vec<_> = futures_util::stream::iter(jobs)
        .buffered(AVATAR_CONCURRENCY)
        .collect()
        .await;

    for (item, avatar) in data.iter_mut().zip(loaded) {
        // 图标条目（如消息类型统计）不使用头像，由渲染器按主题色绘制图标徽章
        if item.icon_char.is_some() {
            continue;
        }
        match avatar {
            Some(img) => {
                item.theme_color = avatar_theme_color(&img);
                item.avatar_img = Some(img);
            }
            None => item.avatar_img = Some(default_avatar.clone()),
        }
    }
}

/// 取一张头像：缓存优先，过期的后台换新，没有的现下载。
async fn load_one(
    url: &str,
    dir: Option<&Path>,
    deadline: tokio::time::Instant,
) -> Option<RgbaImage> {
    let key = format!("h_{:x}", md5::compute(url.as_bytes()));
    let path = dir.map(|d| d.join(format!("{key}_{AVATAR_SIZE}.png")));

    if let Some(path) = &path
        && let Some((img, age)) = read_cache(path.clone()).await
    {
        if age > CACHE_FRESH {
            refresh_in_background(url, &key, path);
        }
        return Some(img);
    }

    if cooling_down(&key) || cooling_down(NETWORK_DOWN) || tokio::time::Instant::now() >= deadline {
        return None;
    }
    match tokio::time::timeout_at(deadline, fetch(url, path.as_deref())).await {
        Ok(Some(img)) => Some(img),
        Ok(None) => {
            cool_down(&key);
            None
        }
        Err(_) => {
            cool_down(&key);
            cool_down(NETWORK_DOWN);
            None
        }
    }
}

/// 读缓存文件，连同它的年龄。不存在、读坏了都当作没有。
///
/// 磁盘读与 PNG 解码放进图像工作槽，不占着异步运行时的线程。
async fn read_cache(path: PathBuf) -> Option<(RgbaImage, Duration)> {
    crate::render::worker::run(move || {
        let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
        let img = image::open(&path).ok()?.to_rgba8();
        // 修改时间在未来（时钟回拨）当作过期，让它被换新。
        let age = SystemTime::now()
            .duration_since(modified)
            .unwrap_or(Duration::MAX);
        Some((img, age))
    })
    .await
    .ok()
    .flatten()
}

/// 下载一张头像，缩成圆形，写进缓存后返回。失败一律是 `None`。
///
/// 用进程级共享客户端，只在这一次请求上盖一个短超时：原先每张头像都
/// `builder().build()` 一个新客户端，几十张就是几十套连接池；Android 上还要
/// 重复把系统 CA 包整份解析一遍，代价远大于下载一张 40 KB 的图。
async fn fetch(url: &str, path: Option<&Path>) -> Option<RgbaImage> {
    let resp = crate::http::client()
        .get(url)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().await.ok()?;
    // 能把正文读完，网就是通的。
    release(NETWORK_DOWN);

    // 640 px 的原图解码 + Lanczos 缩放都是 CPU 活，同样放进图像工作槽。
    let path = path.map(Path::to_path_buf);
    crate::render::worker::run(move || {
        let img = image::load_from_memory(&bytes).ok()?;
        let resized = img.resize_exact(AVATAR_SIZE, AVATAR_SIZE, FilterType::Lanczos3);
        let circular = make_circular_avatar(&resized, AVATAR_SIZE);
        if let Some(path) = path {
            store(&path, &circular);
        }
        Some(circular)
    })
    .await
    .ok()
    .flatten()
}

/// 原子写：同一张头像可能被两张榜同时写，读的一方不该碰到写了一半的 PNG。
fn store(path: &Path, img: &RgbaImage) {
    let mut png = std::io::Cursor::new(Vec::new());
    if img.write_to(&mut png, image::ImageFormat::Png).is_err() {
        return;
    }
    let _ = crate::storage::write_atomic(path, png.get_ref());
}

/// 过期缓存的后台刷新：不等结果，失败了旧文件照旧留着。
fn refresh_in_background(url: &str, key: &str, path: &Path) {
    if cooling_down(key) || cooling_down(NETWORK_DOWN) {
        return;
    }
    cool_down(key);
    let (url, key, path) = (url.to_owned(), key.to_owned(), path.to_path_buf());
    tokio::spawn(async move {
        // 成功了缓存已经是新的，冷却不必再留着；失败才让它挡住一分钟内的重试。
        if fetch(&url, Some(&path)).await.is_some() {
            release(&key);
        }
    });
}

/// 各键「这个时刻之前别再发请求」：头像键，外加一个 [`NETWORK_DOWN`]。
static COOLDOWN: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(Default::default);

fn cooling_down(key: &str) -> bool {
    COOLDOWN
        .lock()
        .is_ok_and(|map| map.get(key).is_some_and(|until| *until > Instant::now()))
}

fn cool_down(key: &str) {
    if let Ok(mut map) = COOLDOWN.lock() {
        let now = Instant::now();
        map.retain(|_, until| *until > now);
        map.insert(key.to_owned(), now + RETRY_COOLDOWN);
    }
}

fn release(key: &str) {
    if let Ok(mut map) = COOLDOWN.lock() {
        map.remove(key);
    }
}

/// 每次进程启动后的第一张榜顺手清一次缓存目录，不等结果。
fn sweep_once(dir: &Path) {
    static SWEPT: Once = Once::new();
    SWEPT.call_once(|| {
        let dir = dir.to_path_buf();
        tokio::task::spawn_blocking(move || sweep(&dir));
    });
}

/// 清掉两类文件：闲置超过 [`CACHE_EVICT`] 的，以及旧版按用户号命名的 `u_*`
/// （现在的键是头像地址的 md5，那批文件再也读不到，却一直占着地方）。
fn sweep(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        let legacy = entry.file_name().to_string_lossy().starts_with("u_");
        let idle = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > CACHE_EVICT);
        if (legacy || idle) && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        info!(target: "Plugin/Stats", "头像缓存：清掉 {} 个闲置或作废的文件", removed);
    }
}
