//! 插件的本机图库：一张图收进 `data/<插件>/`，配置或规则里只记文件名。
//!
//! 控制台从相册上传的配图、自动回复从群里存下来的图都走这里，口径只有一份：
//!
//! - 只收解得开的 PNG / JPEG / WebP / GIF，认字节不认扩展名；边长或体积超大的先缩到
//!   [`KEEP_EDGE`] 像素再存（群聊里用不上四千万像素，发起来也快），在动的 GIF 原样留着；
//! - 落盘名由这里起：调用方给的原名只取一段可读的词，后面挂内容哈希的前 8 位，
//!   同一张图再收一次就复用原来那份。

use std::io::Cursor;
use std::path::Path;

/// 边长或体积超过这组数的先缩一遍、重编码一次。
const KEEP_EDGE: u32 = 2560;
const KEEP_BYTES: usize = 3 * 1024 * 1024;
/// 超过这个边长的一律不认：解一张 12000×12000 的图要几百 MB 内存，不值得冒这个险。
const MAX_EDGE: u32 = 12000;

pub struct Kept {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
    /// 同一张图早就收过，用的是原来那份。
    pub reused: bool,
}

/// 收不下的原因：`Input` 是图本身的问题（给人看的话），`Internal` 是这台机器的问题。
pub enum Refused {
    Input(String),
    Internal(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(text) | Self::Internal(text) => f.write_str(text),
        }
    }
}

/// 收一张图进 `data/<dirname>/`。`hint` 是原文件名（只取一段可读的词）。
pub async fn keep(dirname: &str, hint: Option<&str>, raw: Vec<u8>) -> Result<Kept, Refused> {
    let Ok(dir) = crate::plugins::get_data_dir(dirname).await else {
        return Err(Refused::Internal(format!(
            "读不到 data/{dirname}，这个插件还没有初始化过"
        )));
    };
    let hash = format!("{:x}", md5::compute(&raw));
    let prepared = match crate::render::worker::run(move || prepare(&raw)).await {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(message)) => return Err(Refused::Input(message)),
        Err(error) => return Err(Refused::Internal(format!("处理这张图时出错：{error}"))),
    };
    let short = &hash[..8];
    let (name, reused) = match existing(&dir, short, prepared.ext).await {
        Some(name) => (name, true),
        None => (format!("{}-{short}.{}", stem_of(hint), prepared.ext), false),
    };
    let bytes = prepared.bytes.len();
    if !reused && let Err(error) = super::write_atomic_async(dir.join(&name), prepared.bytes).await
    {
        return Err(Refused::Internal(format!(
            "图片没能存进 data/{dirname}：{error}"
        )));
    }
    Ok(Kept {
        name,
        width: prepared.width,
        height: prepared.height,
        bytes,
        reused,
    })
}

/// 同一个内容哈希的文件已经在目录里了？在的话直接用它，不重复落盘。
async fn existing(dir: &Path, short: &str, ext: &str) -> Option<String> {
    let tail = format!("-{short}.{ext}");
    let mut entries = tokio::fs::read_dir(dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(&tail) {
            return Some(name);
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
    if kept.is_empty() {
        "image".into()
    } else {
        kept
    }
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
    // 透明的一律存 PNG，其余走 JPEG：群聊里的图不在乎无损，JPEG 小得多，发起来也快。
    let ext = if image.color().has_alpha() {
        "png"
    } else {
        "jpg"
    };
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

/// 解图的上限：边长与内存都卡住，一张恶意的大图撑不爆进程。
pub fn limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_EDGE);
    limits.max_image_height = Some(MAX_EDGE);
    limits.max_alloc = Some(256 * 1024 * 1024);
    limits
}

/// 图库里的一个文件名：不带分隔符、不以点开头，也不长到离谱。拼路径之前先过这一道。
pub fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 120
        && !name.starts_with('.')
        && !name.contains(['/', '\\', '\0'])
}
