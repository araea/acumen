//! 群聊图片 → 模型能收下的图片。
//!
//! 群里最常见的图恰恰是模型最常拒收的：QQ 表情包多是 GIF，而 Gemini 直接以
//! 400/500 回绝 `image/gif`——一张表情包就能让整轮判定失败。这里在送进模型之前
//! 统一过一道：先验证真实图片内容，再取首帧转成 PNG；解不开的丢掉。
//! 不能只信 HTTP 的 MIME：QQ 直链有时标成 PNG 实际是损坏/不受支持的内容，
//! 原样透传会让 DeepSeek 与 MiMo 的整轮请求一起失败。过大的图也缩到边长上限。

use super::window::Turn;
use base64::Engine as _;
use std::io::Cursor;

/// 转码前的体积上限；再大的图在手机上解码不划算。
const MAX_BYTES: usize = 8 * 1024 * 1024;
/// 转码后的边长上限。
const MAX_EDGE: u32 = 1024;

/// 转码结果的缓存条数上限。一轮最多看两三张图，够覆盖判定与发言两次读取，
/// 也够覆盖同一批消息被连续几轮反复带进上下文。
const CACHE_ENTRIES: usize = 12;
/// 成功的图片缓存半小时；下载失败可能只是短暂断网，不能跟坏图一样久。
const CACHE_TTL: std::time::Duration = std::time::Duration::from_mins(30);
const NEGATIVE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// 直链 → （转好的 data URL 或「这张用不了」，记下的时刻）。
type Cache = std::sync::Mutex<
    std::collections::HashMap<String, (Option<String>, std::time::Instant)>,
>;

/// 一轮里同一张图至少要被取两次：判定看一次，人格发言再看一次。手机上这意味着
/// 两次下载加两次解码，白等好几秒——而等待时间是从「模拟打字」的预算里扣的，
/// 直接影响它看起来像不像在正常聊天。所以按直链缓存转码结果。
fn cache() -> &'static Cache {
    static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// 缓存里那份（`Some(None)` 表示这张图确认过不能用，不必再下一遍）。
fn cached(url: &str) -> Option<Option<String>> {
    let mut guard = cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (value, at) = guard.get(url)?;
    if at.elapsed() > if value.is_some() { CACHE_TTL } else { NEGATIVE_TTL } {
        guard.remove(url);
        return None;
    }
    Some(value.clone())
}

fn remember(url: &str, value: Option<String>) {
    let mut guard = cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.len() >= CACHE_ENTRIES {
        // 逐出最旧的一条；条数这么小，扫一遍比维护一个链表更省事也更好读。
        if let Some(oldest) = guard
            .iter()
            .min_by_key(|(_, (_, at))| *at)
            .map(|(key, _)| key.clone())
        {
            guard.remove(&oldest);
        }
    }
    guard.insert(url.to_string(), (value, std::time::Instant::now()));
}

/// 一张能送进模型的图，连同它出自哪条消息。
///
/// 图片是多模态块，本身不带出处；记录里每条消息又只写〔图片 ×N〕。两张图挨着来
/// 时，模型看得到两张图、却分不清哪个块是哪条消息里的——于是它明明在讲第一张，
/// 引用却指到了第二张。出处随图一起递过去，模型照着记录上的消息号引用就不会指错。
pub(crate) struct Usable {
    /// 这张图来自哪条消息；空串表示不来自记录（比如头像）。
    pub message_id: String,
    /// 在这条消息里是第几张，1 起。
    pub index: usize,
    /// 已验证并转换为 PNG 的 data URL。
    pub data_url: String,
}

/// 图片只在「还在聊着」的时候才递给模型：最近这么多条消息之内。
const FRESH_TURNS: usize = 6;
/// 且不能太旧：夜里一小时才一条消息，六条以内的图也可能是半天前的。
const FRESH_SECONDS: i64 = 15 * 60;

/// 最近的若干张图片，转成可直接送进模型的 data URL，按时间正序，各带出处。
///
/// 下载与转码都可能失败，失败的那张直接跳过——判定宁可少看一张图，也不该因为
/// 一张表情包整轮报废。
///
/// 只递「眼前还在聊」的图：最近几条消息里的，或者被最近几条引用着的。从前一律取窗口里
/// 最新的几张，话早就聊到别处、被人戳一下醒来的那一轮，十几分钟前的一张图照样附在
/// 提示词后面，模型看见图就想评一句——群友正在说它「自说自话」的当口，它接了一句
/// 「这图里挨抽的白毛怎么看着有点眼熟」（线上 2026-10-03 09:39）。
pub(crate) async fn usable_images(turns: &[Turn], limit: usize) -> Vec<Usable> {
    if limit == 0 {
        return Vec::new();
    }
    let now = chrono::Local::now().timestamp();
    let tail = turns.len().saturating_sub(FRESH_TURNS);
    let quoted: std::collections::HashSet<&str> = turns[tail..]
        .iter()
        .map(|turn| turn.call.reply_to.as_str())
        .filter(|id| !id.is_empty())
        .collect();
    let mut out = Vec::new();
    for (position, turn) in turns.iter().enumerate().rev() {
        let fresh = position >= tail && now - turn.at <= FRESH_SECONDS;
        if !fresh && !quoted.contains(turn.message_id.as_str()) {
            continue;
        }
        for (index, url) in turn.images.iter().enumerate().rev() {
            if let Some(data_url) = usable_image(url).await {
                out.push(Usable {
                    message_id: turn.message_id.clone(),
                    index: index + 1,
                    data_url,
                });
                if out.len() >= limit {
                    break;
                }
            }
        }
        if out.len() >= limit {
            break;
        }
    }
    out.reverse();
    out
}

/// 随附图与记录里消息的对应关系，写成一行给模型读。
///
/// 只列得出处的那几张，顺序与真正附在提示词后面的图片块一一对应；一句都没有时
/// 返回空串（没有图，就没有要交代的对应）。
pub(crate) fn provenance(images: &[Usable]) -> String {
    let entries: Vec<String> = images
        .iter()
        .filter(|image| !image.message_id.is_empty())
        .map(|image| format!("id={} 的第 {} 张", image.message_id, image.index))
        .collect();
    if entries.is_empty() {
        return String::new();
    }
    format!("随附的图片依次对应记录里的：{}。\n", entries.join("、"))
}

/// 一张图片直链 → 模型能收下的 data URL；下不动或解不开时返回 `None`。
///
/// 聊天记录之外的图走这条：头像就是一张——它不在任何一条消息里，却是人格
/// 「自己长什么样」的唯一实物（见 [`super::identity`]）。
pub(super) async fn usable_image(url: &str) -> Option<String> {
    if let Some(hit) = cached(url) {
        return hit;
    }
    let data_url = crate::plugins::oai::logic::to_data_url(url).await;
    let usable = normalize(&data_url);
    remember(url, usable.clone());
    usable
}

/// data URL → 模型可接受的 data URL；无法使用时返回 `None`。
fn normalize(data_url: &str) -> Option<String> {
    let (header, payload) = data_url.split_once(',')?;
    let mime = header.strip_prefix("data:")?.strip_suffix(";base64")?;
    if !mime.to_ascii_lowercase().starts_with("image/") {
        return None;
    }
    // 不相信服务端给的 MIME，所有格式都检查实际字节。也避免 HEIC 等被模型
    // 拒收，以及伪装成 PNG 的坏图使判定与发言一起报错。
    if payload.len() > MAX_BYTES * 4 / 3 + 16 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()?;
    let png = to_png(&bytes)?;
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    ))
}

/// 任意图片字节 → PNG（动图取首帧）。
fn to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return None;
    }
    let mut image = image::load_from_memory(bytes).ok()?;
    if image.width() > MAX_EDGE || image.height() > MAX_EDGE {
        image = image.thumbnail(MAX_EDGE, MAX_EDGE);
    }
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some(png.into_inner())
}
