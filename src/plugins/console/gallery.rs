//! 插件配图的本机图库：把手机相册里选的图收进插件自己的 `data/` 目录，页面再按
//! 缩略图取回。入群欢迎的配图就从这里出（`data/welcome/` 下的文件名写进 `join.images`）。
//!
//! 控制台其余接口都只动配置；这里多了一道收文件的入口，所以边界写死：
//!
//! - 只认 [`bed`] 里登记过的插件，落点固定在它自己的数据目录，不接受客户端指定路径；
//! - 收图、起名、缩图都在 [`crate::storage::picture`]：与自动回复从群里存图是同一套口径；
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
/// 缩略图最大边长；页面按容器宽度要 320–480，留一格余量给宽屏。
const THUMB_MAX: u32 = 1024;
const THUMB_MIN: u32 = 48;

/// 哪些插件有图库，以及图放在哪个数据目录。
fn bed(plugin: &str) -> Option<&'static str> {
    match plugin {
        "welcome" => Some("welcome"),
        "autoreply" => Some("autoreply"),
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
    match crate::storage::picture::keep(dirname, query.name.as_deref(), body.to_vec()).await {
        Ok(kept) => Json(json!({
            "name": kept.name,
            "width": kept.width,
            "height": kept.height,
            "bytes": kept.bytes,
            "reused": kept.reused,
        }))
        .into_response(),
        Err(crate::storage::picture::Refused::Input(message)) => bad(message),
        Err(crate::storage::picture::Refused::Internal(message)) => fail(StatusCode::INTERNAL_SERVER_ERROR, &message),
    }
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
    if !crate::storage::picture::plain(&name) {
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
    reader.limits(crate::storage::picture::limits());
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
