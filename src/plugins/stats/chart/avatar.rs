use super::data_loader::BarData;
use super::utils::{avatar_theme_color, create_default_avatar, make_circular_avatar};
use crate::plugins::get_data_dir;
use futures_util::StreamExt;
use image::RgbaImage;
use image::imageops::FilterType;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tokio::fs;

const AVATAR_SIZE: u32 = 100;
const CACHE_EXPIRE_DAYS: u64 = 3;

/// 头像下载的并发上限。
///
/// 一张榜最多几十个人，`join_all` 会让他们同时开连接——手机上几十套 TLS 状态一起
/// 握手，既拖慢自己也把这一轮的全部结果拖到最慢的那一张图。六个是够用的档位：
/// 浏览器对同域名的并发本来也就这个量级，何况绝大多数命中本地缓存、根本不发请求。
const AVATAR_CONCURRENCY: usize = 6;

/// 批量处理头像下载与主题色提取
pub async fn prepare_avatars(data: &mut [BarData]) {
    let default_avatar = create_default_avatar(AVATAR_SIZE);

    // 获取缓存目录
    let cache_dir = match get_data_dir("stats").await {
        Ok(dir) => {
            let avatar_dir = dir.join("avatars");
            if !avatar_dir.exists() {
                let _ = fs::create_dir_all(&avatar_dir).await;
            }
            Some(avatar_dir)
        }
        Err(e) => {
            warn!(target: "Plugin/Stats", "无法获取数据目录: {}", e);
            None
        }
    };

    let futures: Vec<_> = data
        .iter_mut()
        .map(|item| {
            let url = item.avatar_url.clone();
            let cache_dir = cache_dir.clone();
            // 简单用 URL 的 hash 或 userID 做文件名，这里如果有 ID 优先用 ID
            let file_key = if let Some(uid) = item.user_id {
                format!("u_{}", uid)
            } else if let Some(u) = &url {
                format!("h_{:x}", md5::compute(u.as_bytes()))
            } else {
                "unknown".to_string()
            };

            async move {
                if let Some(url) = url {
                    download_avatar_cached(&url, &file_key, cache_dir, AVATAR_SIZE).await
                } else {
                    None
                }
            }
        })
        .collect();

    // `buffered` 而不是 `join_all`：结果顺序不变，但在飞请求有上限。
    let avatar_results: Vec<_> = futures_util::stream::iter(futures)
        .buffered(AVATAR_CONCURRENCY)
        .collect()
        .await;

    for (i, avatar) in avatar_results.into_iter().enumerate() {
        // 图标条目（如消息类型统计）不使用头像，由渲染器按主题色绘制图标徽章
        if data[i].icon_char.is_some() {
            continue;
        }
        if let Some(img) = avatar {
            data[i].theme_color = avatar_theme_color(&img);
            data[i].avatar_img = Some(img);
        } else {
            data[i].avatar_img = Some(default_avatar.clone());
        }
    }
}

/// 下载头像并带文件缓存
async fn download_avatar_cached(
    url: &str,
    file_key: &str,
    cache_dir: Option<PathBuf>,
    size: u32,
) -> Option<RgbaImage> {
    let file_path = cache_dir.map(|dir| dir.join(format!("{}_{}.png", file_key, size)));

    // 1. 尝试从缓存读取
    if let Some(path) = &file_path
        && path.exists() {
            let should_refresh = if let Ok(metadata) = std::fs::metadata(path) {
                if let Ok(modified) = metadata.modified() {
                    match SystemTime::now().duration_since(modified) {
                        Ok(duration) => duration > Duration::from_secs(CACHE_EXPIRE_DAYS * 86400),
                        Err(_) => true,
                    }
                } else {
                    true
                }
            } else {
                true
            };

            if !should_refresh
                && let Ok(img) = image::open(path) {
                    return Some(img.to_rgba8());
                }
        }

    // 2. 下载。用进程级共享客户端，只在这一次请求上盖一个短超时：原先每张头像都
    //    `builder().build()` 一个新客户端，几十张就是几十套连接池；Android 上还要
    //    重复把系统 CA 包整份解析一遍，代价远大于下载一张 40 KB 的图。
    if let Ok(resp) = crate::http::client()
        .get(url)
        .timeout(Duration::from_secs(8))
        .send()
        .await
        && let Ok(bytes) = resp.bytes().await
        && let Ok(img) = image::load_from_memory(&bytes)
    {
        let resized = img.resize_exact(size, size, FilterType::Lanczos3);
        let circular = make_circular_avatar(&resized, size);

        // 3. 写入缓存
        if let Some(path) = &file_path {
            let png_data = encode_png(&circular);
            if let Some(data) = png_data {
                let _ = fs::write(path, data).await;
            }
        }

        return Some(circular);
    }

    // 如果下载失败但有旧缓存，勉强使用旧缓存
    if let Some(path) = &file_path
        && path.exists()
            && let Ok(img) = image::open(path) {
                return Some(img.to_rgba8());
            }

    None
}

fn encode_png(img: &RgbaImage) -> Option<Vec<u8>> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png).ok()?;
    Some(cursor.into_inner())
}

#[cfg(test)]
mod cache_survey {
    use super::super::utils::avatar_theme_color;

    /// 量一遍磁盘上真实头像的均色彩度分布。
    ///
    /// `HUE_NOISE_FLOOR`（多低才算「读不出色相」）就是按这个分布定的。合成样张里的
    /// 假头像是一块块纯色，彩度比真头像高一个量级；真头像整张求平均之后是一片洗过的
    /// 灰调，门槛按手感定高一格，线上就会大片退成同一条回退色。
    ///
    /// `STATS_AVATAR_CACHE=target/release/data/stats/avatars cargo test cache_survey
    /// -- --ignored --nocapture`
    #[test]
    #[ignore = "按真实头像缓存量取色"]
    fn survey() {
        let Ok(dir) = std::env::var("STATS_AVATAR_CACHE") else {
            return;
        };
        let mut rows: Vec<(f32, String)> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter_map(|e| image::open(e.path()).ok())
            .map(|img| {
                let c = avatar_theme_color(&img.to_rgba8());
                let chroma =
                    (c.0.max(c.1).max(c.2) - c.0.min(c.1).min(c.2)) as f32 / 255.0;
                (chroma, format!("{:?}", c))
            })
            .collect();
        assert!(!rows.is_empty(), "{dir} 里没有可读的头像");

        rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let n = rows.len();
        println!("n={n}");
        for q in [0, n / 10, n / 4, n / 2, (n * 3) / 4, n - 1] {
            println!("  p{:>3}  chroma={:.3}  {}", q * 100 / n, rows[q].0, rows[q].1);
        }
        for t in [0.01f32, 0.02, 0.03, 0.04, 0.06, 0.10] {
            let below = rows.iter().filter(|x| x.0 < t).count();
            println!("  < {t:.2} 会退成回退色：{below} 个（{}%）", below * 100 / n);
        }
    }
}
