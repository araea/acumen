//! 插件配图的本机图库：把手机相册里选的图收进插件自己的 `data/` 目录，页面再按
//! 缩略图取回。入群欢迎的配图就从这里出（`data/welcome/` 下的文件名写进 `join.images`）。
//!
//! 控制台其余接口都只动配置；这里多了一道收文件的入口，所以边界写死：
//!
//! - 只认 [`bed`] 里登记过的插件，落点固定在它自己的数据目录，不接受客户端指定路径；
//! - 只收解得开的 PNG / JPEG / WebP / GIF，认字节不认扩展名；超大的一律先缩到
//!   2560 像素再存（群聊欢迎图用不上四千万像素，上传也更快）；
//! - 落盘名由服务端起：客户端给的原名只取一段可读的词，分隔符与点一律不要，
//!   后面挂内容哈希的前 8 位，同一张图再传一次就复用原来那份；
//! - 取文件只认目录里的普通文件，`?w=` 只出缩略图，缩略图缓存在 `.thumbs/`。

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path as AxumPath, Query};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use std::io::Cursor;
use std::path::Path;

/// 收得下的最大文件：相册原图一般 2–8 MB，留出连拍与截图拼接的余量。
const MAX_BYTES: usize = 16 * 1024 * 1024;
/// 边长或体积超过这组数的先缩一遍、重编码一次：欢迎图是发进群里的缩略图，不必留原尺寸。
const KEEP_EDGE: u32 = 2560;
const KEEP_BYTES: usize = 3 * 1024 * 1024;
/// 超过这个边长的一律不认：解一张 12000×12000 的图要几百 MB 内存，不值得为一张欢迎图冒这个险。
const MAX_EDGE: u32 = 12000;
/// 缩略图最大边长；页面按容器宽度要 320–480，留一格余量给宽屏。
const THUMB_MAX: u32 = 1024;
const THUMB_MIN: u32 = 48;

/// 哪些插件有图库，以及图放在哪个数据目录。
fn bed(plugin: &str) -> Option<&'static str> {
    match plugin {
        "welcome" => Some("welcome"),
        _ => None,
    }
}

fn fail(status: StatusCode, message: &str) -> Response {
    super::server::fail(status, message)
}

fn bad(message: impl Into<String>) -> Response {
    fail(StatusCode::BAD_REQUEST, &message.into())
}

// ==================== 收文件 ====================

#[derive(Deserialize, Default)]
pub(super) struct Naming {
    /// 相册里的原文件名；只用它里头可读的那一段，当不了路径。
    name: Option<String>,
}

pub(super) async fn upload(
    AxumPath(plugin): AxumPath<String>,
    Query(query): Query<Naming>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(dirname) = bed(&plugin) else {
        return fail(StatusCode::NOT_FOUND, &format!("「{plugin}」没有本机图库"));
    };
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !mime.starts_with("image/") {
        return bad("只收图片；从相册里选一张再试。");
    }
    if body.is_empty() {
        return bad("这张图是空的。");
    }
    if body.len() > MAX_BYTES {
        return bad(format!(
            "这张图有 {} MB，超过 16 MB 的上限；换一张，或者先裁小一点。",
            body.len() / (1024 * 1024)
        ));
    }
    let Ok(dir) = crate::plugins::get_data_dir(dirname).await else {
        return bad(format!("读不到 data/{dirname}，这个插件还没有初始化过"));
    };

    let raw = body.to_vec();
    let hash = format!("{:x}", md5::compute(&raw));
    let prepared = match crate::render::worker::run(move || prepare(&raw)).await {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(message)) => return bad(message),
        Err(error) => {
            return fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("处理这张图时出错：{error}"),
            );
        }
    };

    let short = &hash[..8];
    let (name, reused) = match existing(&dir, short, prepared.ext).await {
        Some(found) => found,
        None => (
            format!("{}-{short}.{}", stem_of(query.name.as_deref()), prepared.ext),
            false,
        ),
    };
    let bytes = prepared.bytes.len();
    if !reused
        && let Err(error) = crate::storage::write_atomic_async(dir.join(&name), prepared.bytes).await
    {
        return fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("图片没能存进 data/{dirname}：{error}"),
        );
    }
    Json(json!({
        "name": name,
        "width": prepared.width,
        "height": prepared.height,
        "bytes": bytes,
        "reused": reused,
    }))
    .into_response()
}

/// 同一个内容哈希的文件已经在目录里了？在的话直接用它，不重复落盘。
async fn existing(dir: &Path, short: &str, ext: &str) -> Option<(String, bool)> {
    let tail = format!("-{short}.{ext}");
    let mut entries = tokio::fs::read_dir(dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(&tail) {
            return Some((name, true));
        }
    }
    None
}

/// 原文件名里可读的那一段：去掉目录与扩展名，只留字母、数字、汉字、下划线与连字符，最多 24 个。
fn stem_of(raw: Option<&str>) -> String {
    let base = raw
        .unwrap_or_default()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    let base = base.rsplit_once('.').map_or(base, |(stem, _)| stem);
    let kept: String = base
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
        .take(24)
        .collect();
    if kept.is_empty() { "image".into() } else { kept }
}

struct Prepared {
    bytes: Vec<u8>,
    ext: &'static str,
    width: u32,
    height: u32,
}

/// 字节 → 存下来的样子。只在渲染线程里跑。
fn prepare(bytes: &[u8]) -> Result<Prepared, String> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "这不像一张图片。".to_string())?;
    let format = reader
        .format()
        .ok_or_else(|| "认不出这张图的格式。".to_string())?;
    reader.limits(limits());
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| "这张图打不开：可能损坏，或者尺寸大得离谱。".to_string())?;
    let ext = known(format);

    // 小图、以及在动的 GIF 原样留着：前者重编码只会掉画质，后者会被压成一张静图。
    if let Some(ext) = ext
        && (format == image::ImageFormat::Gif
            || (width <= KEEP_EDGE && height <= KEEP_EDGE && bytes.len() <= KEEP_BYTES))
    {
        return Ok(Prepared {
            bytes: bytes.to_vec(),
            ext,
            width,
            height,
        });
    }

    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "这不像一张图片。".to_string())?;
    reader.limits(limits());
    let image = reader
        .decode()
        .map_err(|_| "这张图打不开：可能损坏，或者尺寸大得离谱。".to_string())?;
    let image = image.thumbnail(KEEP_EDGE, KEEP_EDGE);
    let (width, height) = (image.width(), image.height());
    let mut out = Cursor::new(Vec::new());
    // 透明的一律存 PNG，其余走 JPEG：欢迎图不在乎无损，JPEG 小得多，上传也快。
    let ext = if image.color().has_alpha() { "png" } else { "jpg" };
    let encoded = if ext == "png" {
        image.write_to(&mut out, image::ImageFormat::Png)
    } else {
        image.to_rgb8().write_to(&mut out, image::ImageFormat::Jpeg)
    };
    encoded.map_err(|_| "这张图没法重新编码。".to_string())?;
    Ok(Prepared {
        bytes: out.into_inner(),
        ext,
        width,
        height,
    })
}

fn known(format: image::ImageFormat) -> Option<&'static str> {
    match format {
        image::ImageFormat::Png => Some("png"),
        image::ImageFormat::Jpeg => Some("jpg"),
        image::ImageFormat::WebP => Some("webp"),
        image::ImageFormat::Gif => Some("gif"),
        _ => None,
    }
}

fn limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_EDGE);
    limits.max_image_height = Some(MAX_EDGE);
    limits.max_alloc = Some(256 * 1024 * 1024);
    limits
}

// ==================== 取文件 ====================

#[derive(Deserialize, Default)]
pub(super) struct Want {
    /// 要缩略图就给边长；不给就是原图。
    w: Option<u32>,
}

pub(super) async fn file(
    AxumPath((plugin, name)): AxumPath<(String, String)>,
    Query(query): Query<Want>,
) -> Response {
    let Some(dirname) = bed(&plugin) else {
        return fail(StatusCode::NOT_FOUND, &format!("「{plugin}」没有本机图库"));
    };
    if !plain(&name) {
        return fail(StatusCode::NOT_FOUND, "没有这张图");
    }
    let Ok(dir) = crate::plugins::get_data_dir(dirname).await else {
        return fail(StatusCode::NOT_FOUND, "没有这张图");
    };
    let path = dir.join(&name);
    match tokio::fs::metadata(&path).await {
        Ok(meta) if meta.is_file() => {}
        _ => return fail(StatusCode::NOT_FOUND, "没有这张图；可能已经挪走了"),
    }

    if let Some(width) = query.w.filter(|w| (THUMB_MIN..=THUMB_MAX).contains(w)) {
        let cache = dir.join(".thumbs").join(format!("{name}.{width}.jpg"));
        if let Ok(bytes) = tokio::fs::read(&cache).await {
            return picture(bytes, "image/jpeg");
        }
        let Ok(source) = tokio::fs::read(&path).await else {
            return fail(StatusCode::NOT_FOUND, "没有这张图；可能已经挪走了");
        };
        if let Ok(Ok(bytes)) = crate::render::worker::run(move || thumb(&source, width)).await {
            let _ = crate::storage::write_atomic_async(cache, bytes.clone()).await;
            return picture(bytes, "image/jpeg");
        }
        // 缩略图没做成就退成原图：能看总比看不了强。
    }

    match tokio::fs::read(&path).await {
        Ok(bytes) => picture(bytes, mime_of(&name)),
        Err(error) => fail(StatusCode::INTERNAL_SERVER_ERROR, &format!("读不出来：{error}")),
    }
}

/// 只认目录里的一个普通文件名：不带分隔符、不以点开头，也不长到离谱。
fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 120
        && !name.starts_with('.')
        && !name.contains(['/', '\\', '\0'])
}

fn mime_of(name: &str) -> &'static str {
    match Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "jpg" | "jpeg" => "image/jpeg",
        _ => "application/octet-stream",
    }
}

fn picture(bytes: Vec<u8>, mime: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "private, max-age=86400"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response()
}

fn thumb(source: &[u8], width: u32) -> Result<Vec<u8>, String> {
    let mut reader = image::ImageReader::new(Cursor::new(source))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    reader.limits(limits());
    let image = reader
        .decode()
        .map_err(|e| e.to_string())?
        .thumbnail(width, width);
    let mut out = Cursor::new(Vec::new());
    image
        .to_rgb8()
        .write_to(&mut out, image::ImageFormat::Jpeg)
        .map_err(|e| e.to_string())?;
    Ok(out.into_inner())
}
