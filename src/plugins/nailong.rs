//! 奶龙拦截：群里有人发奶龙、奶蛙图，撤回并按次数加码禁言；认错了，管理员一句话纠正。
//!
//! - **认图**见 [`model`]：图像向量 + 分类头，教过的样本优先。动图按帧抽查，哪一帧像就算。
//!   每张图下载一次、算一次，同一张图（按内容摘要）再出现直接用上次的结论。
//! - **处理**按「近 `window_hours` 小时第几次」走 `mute_seconds` 阶梯：默认第一次只撤回提醒，
//!   之后 10 分钟、1 小时、1 天。账号不是群主或管理员时罚不了人，只引用那张图提醒一句
//!   （`remind_without_admin` 可关）。群主、管理员与机器人管理员发的不管（`exempt_staff`）。
//! - **纠正**：图被撤回后就引用不到了，所以纠错的锚点是机器人那条提醒——管理员引用它发
//!   「奶龙 不是」：记住这张图不算、解除这次禁言、这次不计入次数。漏掉的图引用后发
//!   「奶龙 是」，往后同类图就拦。任何人都能引用一张图发「奶龙 测」看分数。
//!
//! 不发「正在识别」之类的过程话；没认出来就什么都不说。

use crate::adapters::satori::{LockedWriter, api, send_msg, send_msg_id};
use crate::clock::duration_text;
use crate::command::{self, CommandMatch};
use crate::event::Context;
use crate::http::download_bytes;
use crate::message::Message;
use crate::plugins::{ChannelConfig, PluginConfig, PluginError, get_config_or_default};
use crate::storage::JsonState;
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::future::BoxFuture;
use image::{AnimationDecoder, ImageDecoder, RgbaImage};
use serde::{Deserialize, Serialize};
use serde_json::json;
use simd_json::OwnedValue;
use simd_json::base::ValueAsScalar;
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsArray, ValueObjectAccessAsScalar};
use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

pub mod model;

use model::{Kind, Scores};

const LOG_TARGET: &str = "Plugin/Nailong";

/// 指令词。
const COMMANDS: [&str; 2] = ["奶龙", "奶蛙"];

// ==================== 配置 ====================

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    enabled: bool,
    /// 在哪些群生效；两边都空 = 账号所在的全部群。
    channel: ChannelConfig,
    /// 拦哪几类：nailong（奶龙）、naiwa（奶蛙）。
    targets: Vec<String>,
    /// 判定阈值（0.3–0.95）：越高越不容易误判、也越容易漏。
    threshold: f32,
    /// 撤回认出来的图。
    recall: bool,
    /// 近 window_hours 小时内第 1、2、3…次各禁言多少秒；0 = 只撤回不禁言，超出取最后一档。
    mute_seconds: Vec<u64>,
    /// 次数的统计窗口（小时）。
    window_hours: u64,
    /// 群主、管理员与机器人管理员发的不管。
    exempt_staff: bool,
    /// 这些人发的不管。
    ignore_users: Vec<String>,
    /// 账号不是管理员（罚不了人）时，仍引用那张图提醒一句。
    remind_without_admin: bool,
    /// 一条消息最多看几张图。
    max_images: usize,
    /// 动图最多抽查几帧。
    max_frames: usize,
    /// 撤回后说的话；留空不说。占位符：{at} {name} {kind} {count}
    recall_text: String,
    /// 撤回并禁言后说的话。再多一个 {duration}
    mute_text: String,
    /// 罚不了人时说的话（引用那张图）。
    remind_text: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            channel: ChannelConfig::default(),
            targets: vec!["nailong".into(), "naiwa".into()],
            threshold: 0.8,
            recall: true,
            mute_seconds: vec![0, 600, 3600, 86400],
            window_hours: 24,
            exempt_staff: true,
            ignore_users: Vec::new(),
            remind_without_admin: true,
            max_images: 4,
            max_frames: 4,
            recall_text: "{at} 本群不收{kind}图，这张先撤了。".into(),
            mute_text: "{at} 本群不收{kind}图，这是第 {count} 次，禁言 {duration}。".into(),
            remind_text: "本群不收{kind}图，麻烦撤回一下。".into(),
        }
    }
}

impl PluginConfig for Config {
    const NAME: &'static str = "nailong";

    fn check(&self) -> Result<(), String> {
        if self.targets.is_empty() {
            return Err("targets 至少要有一类（nailong、naiwa）".into());
        }
        if let Some(bad) = self.targets.iter().find(|t| target_kind(t).is_none()) {
            return Err(format!("targets 里的「{bad}」不认识，只能是 nailong 或 naiwa"));
        }
        if !(0.3..=0.95).contains(&self.threshold) {
            return Err("threshold 须在 0.3–0.95 之间".into());
        }
        if self.mute_seconds.is_empty() {
            return Err("mute_seconds 至少要有一档（0 表示只撤回）".into());
        }
        if self.mute_seconds.iter().any(|s| *s != 0 && !(60..=30 * 86400).contains(s)) {
            return Err("mute_seconds 每档须为 0，或 60 秒到 30 天之间".into());
        }
        if !(1..=24 * 30).contains(&self.window_hours) {
            return Err("window_hours 须在 1–720 之间".into());
        }
        if !(1..=10).contains(&self.max_images) {
            return Err("max_images 须在 1–10 之间".into());
        }
        if !(1..=12).contains(&self.max_frames) {
            return Err("max_frames 须在 1–12 之间".into());
        }
        Ok(())
    }
}

impl Config {
    fn targets(&self) -> Vec<Kind> {
        self.targets.iter().filter_map(|t| target_kind(t)).collect()
    }

    fn kinds_text(&self) -> String {
        self.targets().iter().map(|k| k.name()).collect::<Vec<_>>().join("、")
    }
}

fn target_kind(text: &str) -> Option<Kind> {
    match text.trim() {
        "nailong" | "奶龙" => Some(Kind::Nailong),
        "naiwa" | "奶蛙" => Some(Kind::Naiwa),
        _ => None,
    }
}

// ==================== 教过的样本 ====================

/// 新图与教过的样本相似到这个程度，就按样本算（同一张图换个压缩、裁一圈边都在这之上）。
const TAUGHT_SIMILARITY: f32 = 0.9;
/// 最多记多少张；满了丢最早的。
const MAX_SAMPLES: usize = 2000;

#[derive(Serialize, Deserialize, Clone)]
struct Sample {
    kind: Kind,
    /// 单位向量，小端 f32 的 base64。
    vector: String,
    by: String,
    group: String,
    at: i64,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct Samples {
    samples: Vec<Sample>,
}

static SAMPLES: JsonState<Samples> = JsonState::new(LOG_TARGET, "nailong", "samples.json", "奶龙样本");

/// 一张教过的图：类别与向量。
type Taught = (Kind, Vec<f32>);

/// 解码过的样本向量，和文件同步维护，免得每张图都解一遍 base64。
static TAUGHT: LazyLock<Mutex<Vec<Taught>>> = LazyLock::new(Default::default);

fn encode_vector(vector: &[f32]) -> String {
    STANDARD.encode(vector.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>())
}

fn decode_vector(text: &str) -> Option<Vec<f32>> {
    let bytes = STANDARD.decode(text).ok()?;
    (bytes.len() == model::DIM * 4).then(|| bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect())
}

fn refresh_taught(samples: &Samples) {
    let decoded = samples
        .samples
        .iter()
        .filter_map(|s| Some((s.kind, decode_vector(&s.vector)?)))
        .collect();
    *TAUGHT.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = decoded;
}

/// 和这个向量最像的样本。
fn nearest_taught(vector: &[f32]) -> Option<(Kind, f32)> {
    TAUGHT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .map(|(kind, v)| (*kind, model::similarity(vector, v)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

async fn teach(kind: Kind, vectors: &[Vec<f32>], by: &str, group: &str) -> usize {
    let now = chrono::Utc::now().timestamp();
    let total = SAMPLES
        .with(|state| {
            for vector in vectors {
                // 同一张图教第二次：改口，不重复记。
                state.samples.retain(|s| {
                    decode_vector(&s.vector).is_none_or(|v| model::similarity(&v, vector) < 0.98)
                });
                state.samples.push(Sample {
                    kind,
                    vector: encode_vector(vector),
                    by: by.to_string(),
                    group: group.to_string(),
                    at: now,
                });
            }
            let overflow = state.samples.len().saturating_sub(MAX_SAMPLES);
            state.samples.drain(..overflow);
            refresh_taught(state);
            state.samples.len()
        })
        .await;
    {
        let mut cache = VERDICTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.map.clear();
        cache.order.clear();
    }
    total
}

async fn forget(vectors: &[Vec<f32>]) -> usize {
    let removed = SAMPLES
        .with(|state| {
            let before = state.samples.len();
            state.samples.retain(|s| {
                decode_vector(&s.vector)
                    .is_none_or(|v| vectors.iter().all(|x| model::similarity(&v, x) < TAUGHT_SIMILARITY))
            });
            refresh_taught(state);
            before - state.samples.len()
        })
        .await;
    {
        let mut cache = VERDICTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.map.clear();
        cache.order.clear();
    }
    removed
}

// ==================== 认图 ====================

/// 一张图的结论。
#[derive(Clone)]
struct Verdict {
    /// 认成了哪类（已按教过的样本修正；不看配置拦不拦这类）。
    kind: Kind,
    /// 最像的那帧的模型分数。
    scores: Scores,
    /// 命中了教过的样本：那个样本的类别与相似度。
    taught: Option<(Kind, f32)>,
    /// 抽查过的帧的向量，教学用。
    vectors: Vec<Vec<f32>>,
}

impl Verdict {
    fn hits(&self, config: &Config) -> bool {
        self.kind != Kind::Other && config.targets().contains(&self.kind)
    }
}

/// 一帧的结论：教过的样本优先，其次分类头。
fn judge(vector: &[f32], threshold: f32) -> (Kind, Scores, Option<(Kind, f32)>) {
    let scores = model::classify(vector);
    if let Some((kind, similarity)) = nearest_taught(vector).filter(|(_, s)| *s >= TAUGHT_SIMILARITY) {
        return (kind, scores, Some((kind, similarity)));
    }
    let kind = if scores.nailong.max(scores.naiwa) < threshold {
        Kind::Other
    } else if scores.nailong >= scores.naiwa {
        Kind::Nailong
    } else {
        Kind::Naiwa
    };
    (kind, scores, None)
}

/// 最近看过的图：内容摘要 → 结论。群里同一张表情包常被反复发。
static VERDICTS: LazyLock<Mutex<VerdictCache>> = LazyLock::new(Default::default);

/// 结论缓存：摘要 → 结论，外加先来后到的顺序，满了丢最早的。
#[derive(Default)]
struct VerdictCache {
    map: HashMap<String, Verdict>,
    order: VecDeque<String>,
}
const VERDICT_CACHE: usize = 512;

/// 认图同一时间只跑一张：编码器本身单线程，手机上不和渲染抢更多核。
static INFER: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
/// 排队中的图。刷屏时超过这个数就不看了，免得积压到几分钟后才撤。
static QUEUED: AtomicUsize = AtomicUsize::new(0);
const MAX_QUEUED: usize = 12;

/// 排队名额：任务怎么结束（包括 panic）都还回去。
struct Queued;

impl Queued {
    fn enter() -> Self {
        QUEUED.fetch_add(1, Ordering::Relaxed);
        Queued
    }
}

impl Drop for Queued {
    fn drop(&mut self) {
        QUEUED.fetch_sub(1, Ordering::Relaxed);
    }
}

/// 单张图下载后的体积上限。
const MAX_IMAGE_BYTES: usize = 15 * 1024 * 1024;

async fn inspect(url: &str, config: &Config) -> Result<Verdict, String> {
    // 模型还没就绪（下载失败在冷却）就不必下图了。
    let encoder = model::encoder().await?;
    let bytes = tokio::time::timeout(Duration::from_secs(20), download_bytes(url))
        .await
        .map_err(|_| "下载超时".to_string())?
        .map_err(|e| format!("下载失败：{e}"))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!("图片有 {} MB，太大", bytes.len() / 1024 / 1024));
    }
    let digest = format!("{:x}", md5::compute(&bytes));
    if let Some(verdict) = VERDICTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).map.get(&digest) {
        return Ok(verdict.clone());
    }
    let _permit = INFER.acquire().await.map_err(|e| e.to_string())?;
    let (max_frames, threshold) = (config.max_frames, config.threshold);
    let verdict = tokio::task::spawn_blocking(move || -> Result<Verdict, String> {
        let frames = decode_frames(&bytes, max_frames)?;
        let mut best: Option<Verdict> = None;
        let mut vectors = Vec::new();
        for frame in &frames {
            let vector = encoder.embed(frame)?;
            let (kind, scores, taught) = judge(&vector, threshold);
            vectors.push(vector);
            let strength = |v: &Verdict| v.scores.nailong.max(v.scores.naiwa);
            let candidate = Verdict { kind, scores, taught, vectors: Vec::new() };
            let better = match &best {
                None => true,
                Some(b) => {
                    (b.kind == Kind::Other && kind != Kind::Other)
                        || (b.kind == Kind::Other) == (kind == Kind::Other) && strength(&candidate) > strength(b)
                }
            };
            if better {
                best = Some(candidate);
            }
            // 有一帧已经认出来了，后面的不必再看。
            if kind != Kind::Other {
                break;
            }
        }
        let mut verdict = best.ok_or("没有可看的帧")?;
        verdict.vectors = vectors;
        Ok(verdict)
    })
    .await
    .map_err(|e| format!("识别线程异常：{e}"))??;
    let mut cache = VERDICTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache.map.insert(digest.clone(), verdict.clone()).is_none() {
        cache.order.push_back(digest);
        if cache.order.len() > VERDICT_CACHE
            && let Some(old) = cache.order.pop_front()
        {
            cache.map.remove(&old);
        }
    }
    Ok(verdict)
}

/// 解出要看的帧：静图一帧；动图（GIF、WebP）均匀抽 `max_frames` 帧，先看第一帧。
/// 每帧先缩到长边 512 以内，省内存。
fn decode_frames(bytes: &[u8], max_frames: usize) -> Result<Vec<RgbaImage>, String> {
    let format = image::guess_format(bytes).map_err(|_| "不是能打开的图片".to_string())?;
    let shrink = |frame: RgbaImage| {
        let (w, h) = frame.dimensions();
        if w.max(h) <= 512 {
            frame
        } else {
            let scale = 512.0 / w.max(h) as f32;
            image::imageops::thumbnail(&frame, ((w as f32 * scale) as u32).max(1), ((h as f32 * scale) as u32).max(1))
        }
    };
    match format {
        image::ImageFormat::Gif => {
            let count = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))
                .map_err(|e| e.to_string())?
                .into_frames()
                .take(2048)
                .take_while(Result::is_ok)
                .count();
            let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
            pick_frames(decoder.into_frames(), &sample_indices(count, max_frames), shrink)
        }
        image::ImageFormat::WebP => {
            let decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
            if decoder.has_animation() {
                let count = decoder.into_frames().take(2048).take_while(Result::is_ok).count();
                let decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
                pick_frames(decoder.into_frames(), &sample_indices(count, max_frames), shrink)
            } else {
                still(bytes, shrink)
            }
        }
        _ => still(bytes, shrink),
    }
}

fn still(bytes: &[u8], shrink: impl Fn(RgbaImage) -> RgbaImage) -> Result<Vec<RgbaImage>, String> {
    let mut decoder = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(|_| "不是能打开的图片".to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(12_000);
    limits.max_image_height = Some(12_000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    decoder.set_limits(limits).map_err(|_| "图片尺寸太大".to_string())?;
    let orientation = decoder.orientation().ok();
    let mut image = image::DynamicImage::from_decoder(decoder).map_err(|_| "图片解码失败".to_string())?;
    if let Some(orientation) = orientation {
        image.apply_orientation(orientation);
    }
    Ok(vec![shrink(image.into_rgba8())])
}

fn pick_frames(
    frames: image::Frames<'_>,
    wanted: &[usize],
    shrink: impl Fn(RgbaImage) -> RgbaImage,
) -> Result<Vec<RgbaImage>, String> {
    let mut picked: Vec<(usize, RgbaImage)> = Vec::new();
    for (index, frame) in frames.enumerate().take(wanted.last().map_or(0, |last| last + 1)) {
        let frame = frame.map_err(|e| e.to_string())?;
        if wanted.contains(&index) {
            picked.push((index, shrink(frame.into_buffer())));
        }
    }
    if picked.is_empty() {
        return Err("动图里没有能解出的帧".into());
    }
    // 先看第一帧，再按位置均匀往后：表情包的主角多半一开头就在。
    picked.sort_by_key(|(index, _)| *index);
    Ok(picked.into_iter().map(|(_, frame)| frame).collect())
}

/// 从 `count` 帧里均匀挑至多 `max` 帧的下标，第一帧必选。
fn sample_indices(count: usize, max: usize) -> Vec<usize> {
    if count == 0 || max == 0 {
        return Vec::new();
    }
    if count <= max {
        return (0..count).collect();
    }
    let mut indices: Vec<usize> = (0..max).map(|i| i * (count - 1) / (max - 1).max(1)).collect();
    indices.dedup();
    indices
}

// ==================== 次数与处理记录 ====================

#[derive(Serialize, Deserialize, Clone)]
struct Offense {
    self_id: String,
    group: String,
    user: String,
    /// 那张图的消息 ID：「不是」按它把这次划掉。
    message_id: String,
    kind: Kind,
    at: i64,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct Ledger {
    offenses: Vec<Offense>,
}

static LEDGER: JsonState<Ledger> = JsonState::new(LOG_TARGET, "nailong", "offenses.json", "奶龙处理记录");
/// 账本留多久（秒）：够窗口用、也够状态里说「今天拦了几张」。
const LEDGER_KEEP: i64 = 31 * 86400;

/// 记一次，返回近 `window` 秒内这是第几次。
async fn record(offense: Offense, window: i64) -> usize {
    LEDGER
        .with(|ledger| {
            let now = offense.at;
            ledger.offenses.retain(|o| now - o.at < LEDGER_KEEP);
            let count = 1 + ledger
                .offenses
                .iter()
                .filter(|o| o.group == offense.group && o.user == offense.user && o.self_id == offense.self_id)
                .filter(|o| now - o.at < window)
                .count();
            ledger.offenses.push(offense);
            count
        })
        .await
}

/// 机器人提醒过的那几次：提醒消息 ID → 当时的情形。纠错时管理员引用的就是提醒。
#[derive(Clone)]
struct Incident {
    group: String,
    user: String,
    /// 原图那条消息的 ID（多半已撤回）。
    message_id: String,
    muted: bool,
    verdict: Verdict,
}

static INCIDENTS: LazyLock<Mutex<VecDeque<(String, Incident)>>> = LazyLock::new(Default::default);
const MAX_INCIDENTS: usize = 200;

fn remember_incident(tip_id: String, incident: Incident) {
    let mut incidents = INCIDENTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    incidents.push_back((tip_id, incident));
    while incidents.len() > MAX_INCIDENTS {
        incidents.pop_front();
    }
}

/// 按提醒或原图的消息 ID 找那一次。
fn find_incident(group: &str, message_id: &str) -> Option<(String, Incident)> {
    INCIDENTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .rev()
        .find(|(tip, incident)| incident.group == group && (tip == message_id || incident.message_id == message_id))
        .cloned()
}

// ==================== 事件 ====================

pub fn handle(ctx: Context, writer: LockedWriter) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let Some(message) = ctx.as_message() else {
            return Ok(Some(ctx));
        };
        let Some(group) = message.group_id().filter(|id| !id.is_empty()).map(str::to_owned) else {
            return Ok(Some(ctx));
        };
        let config = get_config_or_default::<Config>(&ctx);
        if !config.channel.allows_group(&group) {
            return Ok(Some(ctx));
        }
        if let Some(matched) = COMMANDS.iter().find_map(|word| command::match_word_command(&ctx, word))
            && run_command(&ctx, &writer, &config, &group, matched).await?
        {
            return Ok(None);
        }
        let user = message.user_id().to_string();
        if user.is_empty() || user == ctx.bot.self_id() || config.ignore_users.contains(&user) {
            return Ok(Some(ctx));
        }
        let urls = image_urls(message.0.get_array("message").map_or(&[][..], Vec::as_slice), config.max_images);
        if urls.is_empty() {
            return Ok(Some(ctx));
        }
        if config.exempt_staff
            && (crate::plugins::ctl::is_manager(&ctx) || message.sender_role().is_some_and(api::is_staff))
        {
            return Ok(Some(ctx));
        }
        if QUEUED.load(Ordering::Relaxed) >= MAX_QUEUED {
            debug!(target: LOG_TARGET, "排队的图太多，群 {group} 这条先不看");
            return Ok(Some(ctx));
        }
        let message_id = message.message_id().to_string();
        let name = message.sender_name().to_string();
        let (task_ctx, task_writer) = (ctx.clone(), writer.clone());
        let queued = Queued::enter();
        tokio::spawn(async move {
            let found = scan(&urls, &config).await;
            drop(queued);
            if let Some(verdict) = found {
                let target = Target { group, user, name, message_id };
                if let Err(error) = punish(&task_ctx, &task_writer, &config, &target, verdict).await {
                    warn!(target: LOG_TARGET, "群 {} 处理奶龙图失败：{error}", target.group);
                }
            }
        });
        Ok(Some(ctx))
    })
}

fn image_urls(segments: &[OwnedValue], max: usize) -> Vec<String> {
    segments
        .iter()
        .filter(|segment| segment.get_str("type") == Some("image"))
        .filter_map(|segment| {
            let data = segment.get("data")?;
            data.get_str("url")
                .or_else(|| data.get_str("file").filter(|f| f.starts_with("http")))
                .filter(|url| !url.is_empty())
                .map(String::from)
        })
        .take(max)
        .collect()
}

/// 一条消息里的图挨个看，认出一张就停。
async fn scan(urls: &[String], config: &Config) -> Option<Verdict> {
    for url in urls {
        match inspect(url, config).await {
            Ok(verdict) if verdict.hits(config) => return Some(verdict),
            Ok(_) => {}
            Err(error) => debug!(target: LOG_TARGET, "这张图没看成：{error}"),
        }
    }
    None
}

struct Target {
    group: String,
    user: String,
    name: String,
    message_id: String,
}

async fn punish(ctx: &Context, writer: &LockedWriter, config: &Config, target: &Target, verdict: Verdict) -> Result<(), PluginError> {
    let Target { group, user, name, message_id } = target;
    let kind = verdict.kind;
    let score = verdict.taught.map_or_else(|| verdict.scores.of(kind), |(_, s)| s);
    let able = api::is_staff(&api::own_role(ctx, writer.clone(), group).await);
    info!(
        target: LOG_TARGET,
        "群 {group} 的 {name}（{user}）发了{}图（{:.0}%{}）",
        kind.name(),
        score * 100.0,
        if verdict.taught.is_some() { "，像教过的样本" } else { "" }
    );
    let vars = |count: usize, duration: u64| Vars { name, kind: kind.name(), count, duration: duration_text(duration) };
    if !able {
        if !config.remind_without_admin || config.remind_text.trim().is_empty() {
            return Ok(());
        }
        let message = Message::new().reply(message_id);
        let message = render_into(message, &config.remind_text, user, &vars(0, 0));
        if let Some(tip) = send_msg_id(ctx, writer.clone(), Some(group), None, message).await? {
            remember_incident(tip, incident(target, false, verdict));
        }
        return Ok(());
    }
    if config.recall
        && let Err(error) = api::delete_msg_in(ctx, writer.clone(), group, message_id).await
    {
        warn!(target: LOG_TARGET, "群 {group} 撤回 {message_id} 失败：{error}");
    }
    let now = chrono::Utc::now().timestamp();
    let window = i64::try_from(config.window_hours * 3600).unwrap_or(86400);
    let offense = Offense {
        self_id: ctx.bot.self_id(),
        group: group.clone(),
        user: user.clone(),
        message_id: message_id.clone(),
        kind,
        at: now,
    };
    let count = record(offense, window).await;
    let duration = config.mute_seconds[(count - 1).min(config.mute_seconds.len() - 1)];
    let mut muted = false;
    if duration > 0 {
        match writer
            .call::<_, serde_json::Value>(
                ctx,
                "guild.member.mute",
                json!({"guild_id": group, "user_id": user, "duration": duration * 1000}),
            )
            .await
        {
            Ok(_) => muted = true,
            Err(error) => warn!(target: LOG_TARGET, "群 {group} 禁言 {user} 失败：{error}"),
        }
    }
    let template = if muted { &config.mute_text } else { &config.recall_text };
    if template.trim().is_empty() {
        return Ok(());
    }
    let message = render_into(Message::new(), template, user, &vars(count, duration));
    if let Some(tip) = send_msg_id(ctx, writer.clone(), Some(group), None, message).await? {
        remember_incident(tip, incident(target, muted, verdict));
    }
    Ok(())
}

fn incident(target: &Target, muted: bool, verdict: Verdict) -> Incident {
    Incident {
        group: target.group.clone(),
        user: target.user.clone(),
        message_id: target.message_id.clone(),
        muted,
        verdict,
    }
}

struct Vars<'a> {
    name: &'a str,
    kind: &'a str,
    count: usize,
    duration: String,
}

/// 模板接到消息后面：`{at}` 处换成对这个人的 @。
fn render_into(mut message: Message, template: &str, user: &str, vars: &Vars) -> Message {
    let fill = |text: &str| {
        text.replace("{name}", vars.name)
            .replace("{kind}", vars.kind)
            .replace("{count}", &vars.count.to_string())
            .replace("{duration}", &vars.duration)
    };
    let mut pieces = template.split("{at}").peekable();
    while let Some(piece) = pieces.next() {
        let text = fill(piece);
        if !text.is_empty() {
            message = message.text(text);
        }
        if pieces.peek().is_some() {
            message = message.at(user);
        }
    }
    message
}

// ==================== 指令 ====================

async fn run_command(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    group: &str,
    matched: CommandMatch,
) -> Result<bool, PluginError> {
    let Some(message) = ctx.as_message() else {
        return Ok(false);
    };
    let message_id = message.message_id().to_string();
    let say = |text: String| send_msg(ctx, writer.clone(), Some(group), None, Message::new().reply(&message_id).text(text));
    let arg = command::extract_text_arg(&matched.args);
    let word = arg.split_whitespace().next().unwrap_or("");
    let rest = arg[word.len()..].trim();
    let attached = image_urls(&matched.args, 1);
    let wants_image = matched.reply_id.is_some() || !attached.is_empty();
    match word {
        "" | "测" | "测试" | "看看" if wants_image => test(ctx, writer, config, group, &matched, attached, &say).await?,
        "" | "状态" | "帮助" => say(status(ctx, writer, config, group).await).await?,
        "是" | "算" | "拦" => {
            let explicit = target_kind(rest);
            correct(ctx, writer, config, group, &matched, attached, Correction::Is(explicit), &say).await?;
        }
        "不是" | "不算" | "放过" => correct(ctx, writer, config, group, &matched, attached, Correction::IsNot, &say).await?,
        "忘了" | "忘记" => correct(ctx, writer, config, group, &matched, attached, Correction::Forget, &say).await?,
        // 没有指令前缀时「奶龙 好可爱」只是在聊天，不接话。
        _ if command::get_prefixes(ctx).iter().all(String::is_empty) => return Ok(false),
        _ => say(format!("❌ 不认识「{word}」\n{}", usage())).await?,
    }
    Ok(true)
}

fn usage() -> String {
    "引用一张图发「奶龙 测」看分数\n管理员引用图或机器人的提醒发「奶龙 是」「奶龙 不是」纠正，「奶龙 忘了」撤销".into()
}

async fn status(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str) -> String {
    let able = api::is_staff(&api::own_role(ctx, writer.clone(), group).await);
    let today = crate::clock::beijing_now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|t| t.and_local_timezone(crate::clock::beijing()).single())
        .map_or(0, |t| t.timestamp());
    let me = ctx.bot.self_id();
    let (blocked, people) = LEDGER
        .with(|ledger| {
            let today: Vec<&Offense> =
                ledger.offenses.iter().filter(|o| o.group == group && o.self_id == me && o.at >= today).collect();
            let mut people: Vec<&str> = today.iter().map(|o| o.user.as_str()).collect();
            people.sort_unstable();
            people.dedup();
            (today.len(), people.len())
        })
        .await;
    let taught = SAMPLES.with(|s| s.samples.len()).await;
    let ladder = config
        .mute_seconds
        .iter()
        .map(|s| if *s == 0 { "只撤回".to_string() } else { format!("禁言 {}", duration_text(*s)) })
        .collect::<Vec<_>>()
        .join(" → ");
    let lines = [
        format!("🐉 奶龙拦截 · 拦{}", config.kinds_text()),
        if able {
            format!("处理：{ladder}（{} 小时内累计）", config.window_hours)
        } else {
            "⚠️ 账号不是管理员，只能提醒、撤不了也禁不了".to_string()
        },
        if blocked == 0 {
            format!("今天还没拦过 · 教过 {taught} 张 · 阈值 {:.0}%", config.threshold * 100.0)
        } else {
            format!("今天拦下 {blocked} 张、{people} 人 · 教过 {taught} 张 · 阈值 {:.0}%", config.threshold * 100.0)
        },
        format!("模型：{}", model::status()),
        String::new(),
        usage(),
    ];
    lines.join("\n")
}

/// 引用或附带的图：先找机器人提醒过的那次（图已撤回），再取引用那条里的图、附带的图。
async fn subject(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    group: &str,
    matched: &CommandMatch,
    attached: Vec<String>,
) -> Result<(Option<(String, Incident)>, Vec<Verdict>), String> {
    if let Some(reply) = &matched.reply_id
        && let Some(found) = find_incident(group, reply)
    {
        let verdict = found.1.verdict.clone();
        return Ok((Some(found), vec![verdict]));
    }
    let mut urls = attached;
    if urls.is_empty()
        && let Some(reply) = &matched.reply_id
    {
        let quoted = api::get_msg(ctx, writer.clone(), reply).await.map_err(|_| "引用的那条读不到了，可能已撤回或太久".to_string())?;
        urls = quoted
            .message
            .0
            .iter()
            .filter(|seg| seg.type_ == "image")
            .filter_map(|seg| seg.data.get("url").and_then(|v| v.as_str()).map(String::from))
            .take(config.max_images)
            .collect();
    }
    if urls.is_empty() {
        return Err("引用的那条里没有图".into());
    }
    let mut verdicts = Vec::new();
    for url in &urls {
        verdicts.push(inspect(url, config).await?);
    }
    Ok((None, verdicts))
}

async fn test<F>(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    group: &str,
    matched: &CommandMatch,
    attached: Vec<String>,
    say: &impl Fn(String) -> F,
) -> Result<(), PluginError>
where
    F: Future<Output = Result<(), crate::adapters::satori::BotError>>,
{
    let verdicts = match subject(ctx, writer, config, group, matched, attached).await {
        Ok((_, verdicts)) => verdicts,
        Err(error) => return say(format!("❌ {error}")).await,
    };
    let lines: Vec<String> = verdicts
        .iter()
        .enumerate()
        .map(|(index, verdict)| {
            let head = if verdicts.len() > 1 { format!("第 {} 张：", index + 1) } else { String::new() };
            let scores = format!("奶龙 {:.0}% · 奶蛙 {:.0}%", verdict.scores.nailong * 100.0, verdict.scores.naiwa * 100.0);
            let result = match (verdict.taught, verdict.hits(config)) {
                (Some((kind, s)), _) => format!("像教过的一张{}图（{:.0}%），按{}算", kind.name(), s * 100.0, kind.name()),
                (None, true) => format!("会拦：{}", verdict.kind.name()),
                (None, false) if verdict.kind != Kind::Other => format!("像{}，但本群没拦这类", verdict.kind.name()),
                (None, false) => format!("不拦（阈值 {:.0}%）", config.threshold * 100.0),
            };
            format!("{head}{scores}\n{result}")
        })
        .collect();
    say(lines.join("\n")).await
}

enum Correction {
    /// 是：拦这类；可指明奶龙或奶蛙，不指明按模型更像的那类。
    Is(Option<Kind>),
    IsNot,
    Forget,
}

#[allow(clippy::too_many_arguments)]
async fn correct<F>(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    group: &str,
    matched: &CommandMatch,
    attached: Vec<String>,
    correction: Correction,
    say: &impl Fn(String) -> F,
) -> Result<(), PluginError>
where
    F: Future<Output = Result<(), crate::adapters::satori::BotError>>,
{
    let Some(message) = ctx.as_message() else {
        return Ok(());
    };
    if !(crate::plugins::ctl::is_manager(ctx) || message.sender_role().is_some_and(api::is_staff)) {
        return say("❌ 只有群主、管理员能纠正\n想看分数可以发「奶龙 测」".into()).await;
    }
    if matched.reply_id.is_none() && attached.is_empty() {
        return say("❌ 先引用那张图，或机器人的那条提醒，再发这句".into()).await;
    }
    let (incident, verdicts) = match subject(ctx, writer, config, group, matched, attached).await {
        Ok(found) => found,
        Err(error) => return say(format!("❌ {error}")).await,
    };
    let vectors: Vec<Vec<f32>> = verdicts.iter().flat_map(|v| v.vectors.iter().cloned()).collect();
    let by = message.user_id();
    let text = match correction {
        Correction::Is(explicit) => {
            let kind = explicit.unwrap_or_else(|| {
                let scores = verdicts[0].scores;
                if scores.naiwa > scores.nailong { Kind::Naiwa } else { Kind::Nailong }
            });
            let total = teach(kind, &vectors, by, group).await;
            let note = if config.targets().contains(&kind) { "" } else { "（本群目前没拦这类，见 targets）" };
            format!("✅ 记住了：这类图算{}{note}\n已教 {total} 张；记错了发「奶龙 忘了」", kind.name())
        }
        Correction::IsNot => {
            let total = teach(Kind::Other, &vectors, by, group).await;
            let mut text = format!("✅ 记住了：这类图不拦\n已教 {total} 张；记错了发「奶龙 忘了」");
            if let Some((tip, incident)) = incident {
                text.push_str(&undo(ctx, writer, group, &tip, &incident).await);
            }
            text
        }
        Correction::Forget => match forget(&vectors).await {
            0 => "📭 这张图没教过".into(),
            n => format!("✅ 忘掉了 {n} 张，这类图回到模型自己判断"),
        },
    };
    say(text).await
}

/// 「不是」的善后：解除这次禁言、这次不计入次数。
async fn undo(ctx: &Context, writer: &LockedWriter, group: &str, tip: &str, incident: &Incident) -> String {
    let mut done = Vec::new();
    LEDGER
        .with(|ledger| {
            ledger.offenses.retain(|o| !(o.group == incident.group && o.message_id == incident.message_id));
        })
        .await;
    if incident.muted {
        match writer
            .call::<_, serde_json::Value>(
                ctx,
                "guild.member.mute",
                json!({"guild_id": group, "user_id": incident.user, "duration": 0}),
            )
            .await
        {
            Ok(_) => done.push("解除了禁言".to_string()),
            Err(error) => {
                warn!(target: LOG_TARGET, "群 {group} 解除 {} 的禁言失败：{error}", incident.user);
                done.push("禁言没能解除，请手动解一下".to_string());
            }
        }
    }
    if api::delete_msg_in(ctx, writer.clone(), group, tip).await.is_ok() {
        done.push("撤回了那条提醒".to_string());
    }
    INCIDENTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|(id, _)| id != tip);
    done.push("这次不计入次数".to_string());
    format!("\n{}", done.join("、"))
}

/// 启动（或刚被打开）时：样本读进内存，识图模型在后台先下好、载好，第一张图不必等。
pub fn init(_ctx: Context) -> BoxFuture<'static, Result<(), PluginError>> {
    Box::pin(async move {
        SAMPLES.with(|state| refresh_taught(state)).await;
        tokio::spawn(async {
            // 失败已在 encoder() 里记了日志，第一张图到来时还会再试。
            let _ = model::encoder().await;
        });
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_pass_their_own_check() {
        assert_eq!(Config::default().check(), Ok(()));
    }

    #[test]
    fn frames_are_sampled_evenly_from_the_first() {
        assert_eq!(sample_indices(3, 4), vec![0, 1, 2]);
        assert_eq!(sample_indices(100, 4), vec![0, 33, 66, 99]);
        assert_eq!(sample_indices(1, 4), vec![0]);
        assert!(sample_indices(0, 4).is_empty());
    }

    #[test]
    fn vectors_round_trip() {
        let vector: Vec<f32> = (0..model::DIM).map(|i| i as f32 / 7.0).collect();
        assert_eq!(decode_vector(&encode_vector(&vector)), Some(vector));
    }

    /// 用插件真实的解码、预处理与推理跑一个目录，逐张打印分数，与训练脚本的结果对照：
    /// `NAILONG_MODEL=…/vision_model.onnx NAILONG_IMAGES=目录 cargo test --release nailong_eval -- --ignored --nocapture`
    #[test]
    #[ignore = "需要本地模型文件与图片目录"]
    fn nailong_eval() {
        let encoder = model::Encoder::open(std::path::Path::new(&std::env::var("NAILONG_MODEL").unwrap())).unwrap();
        let mut files: Vec<_> = std::fs::read_dir(std::env::var("NAILONG_IMAGES").unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        files.sort();
        for file in files {
            let Ok(frames) = decode_frames(&std::fs::read(&file).unwrap(), 4) else { continue };
            let started = std::time::Instant::now();
            let best = frames
                .iter()
                .map(|frame| model::classify(&encoder.embed(frame).unwrap()))
                .max_by(|a, b| a.nailong.max(a.naiwa).total_cmp(&b.nailong.max(b.naiwa)))
                .unwrap();
            println!(
                "{}\t{:.4}\t{:.4}\t{}\t{}ms",
                file.display(),
                best.nailong,
                best.naiwa,
                frames.len(),
                started.elapsed().as_millis()
            );
        }
    }

    #[test]
    fn templates_put_the_mention_where_asked() {
        let vars = Vars { name: "小明", kind: "奶龙", count: 2, duration: "10 分钟".into() };
        let message = render_into(Message::new(), "{at} 第 {count} 次，禁言 {duration}。", "42", &vars);
        assert_eq!(message.0[0].type_, "at");
        assert_eq!(message.0[1].data.get("text").and_then(|v| v.as_str()), Some(" 第 2 次，禁言 10 分钟。"));
    }
}
