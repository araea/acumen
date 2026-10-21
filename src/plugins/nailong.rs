//! 奶龙拦截：群里有人发奶龙、奶蛙图，撤回并按次数加码禁言；认错了，管理员一句话纠正。
//!
//! - **认图**见 [`model`]：图像向量 + 分类头，教过的样本优先。动图按帧抽查，哪一帧像就算。
//!   每张图下载一次、算一次，同一张图（按内容摘要）再出现直接用上次的结论。
//! - **大模型复核**见 [`ai`]：oai 里配好了多模态模型时，本地分数夹在中间、拿不准的图交给它看
//!   （`judge`）。本地觉得像的才问，很低的直接放，大模型只看一小部分；出错、超时、名额用完都退回本地。
//! - **商城表情**（QQ 表情商店里的动画表情，`mface`）消息里没有图片地址，只有编号：按编号到
//!   QQ 的表情资源站取动图来看，同一个编号只看一次。表情名里写着奶龙、奶蛙的不必等模型。
//! - **处理**按「近 `window_hours` 小时第几次」走 `mute_seconds` 阶梯：默认第一次只撤回提醒，
//!   之后 10 分钟、1 小时、1 天。账号不是群主或管理员时罚不了人，只引用那张图提醒一句
//!   （`remind_without_admin` 可关）。群主、管理员与机器人管理员发的不管（`exempt_staff`）。
//! - **纠正**：图被撤回后就引用不到了，所以纠错的锚点是机器人那条提醒——管理员引用它发
//!   「奶龙 不是」：记住这张图不算、解除这次禁言、这次不计入次数。漏掉的图引用后发
//!   「奶龙 是」，往后同类图就拦。任何人都能引用一张图发「奶龙 测」看分数。
//!
//! - **事后核对**：拦下的图另存一份、编上号，抄给号主（默认他自己的私聊）。当时没看群也能
//!   在那里看到撤了什么，引用那条回「不是」就改判；「奶龙 记录」翻近几天的，「奶龙 不是 12」
//!   按编号改判。被拦的本人引用提醒说「不是」，转给号主看，不能自己给自己改判。
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
use simd_json::base::{ValueAsObject, ValueAsScalar};
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsArray, ValueObjectAccessAsScalar};
use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

pub mod ai;
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
    /// 判定阈值（0.3–0.95）：越高越不容易误判、也越容易漏。只在只用本地模型时起作用。
    threshold: f32,
    /// 认图方式：auto（oai 配了复核模型就用本地初筛 + 大模型复核，否则只用本地）、local（只用本地）、
    /// hybrid（本地初筛 + 大模型复核）、ai（每张图都问大模型，用量大）。
    judge: String,
    /// 复核用的多模态模型，写法同 oai：`供应商/模型`。
    ai_model: String,
    /// 本地分数到这个就直接拦，不问大模型；1 = 本地觉得像的一律请大模型确认（默认，误判最少）。
    sure: f32,
    /// 本地分数到这个才问大模型；更低的直接放过。
    ask_floor: f32,
    /// 每天最多问大模型几张（所有群合计）；用完了当天本地觉得像的也先放过（宁漏勿错）。
    ai_daily_limit: u32,
    /// 问一张最多等几秒。
    ai_timeout_seconds: u64,
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
    /// 也看商城表情（QQ 表情商店的动画表情）。
    check_mface: bool,
    /// 撤下的图另抄一份给号主核对：没看群也知道拦了什么，认错了引用那条回「不是」。
    report: bool,
    /// 抄到哪：群号；或 `private:QQ号`；留空 = 号主自己的私聊。
    report_target: String,
    /// 拦下的图（连同记录）留几天，期间能翻记录、按编号改判。
    keep_days: u64,
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
            threshold: 0.5,
            judge: "auto".into(),
            ai_model: "deepseek/deepseek-flash".into(),
            sure: 1.0,
            ask_floor: 0.2,
            ai_daily_limit: 300,
            ai_timeout_seconds: 20,
            recall: true,
            mute_seconds: vec![0, 600, 3600, 86400],
            window_hours: 24,
            exempt_staff: true,
            ignore_users: Vec::new(),
            remind_without_admin: true,
            check_mface: true,
            report: true,
            report_target: String::new(),
            keep_days: 7,
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
        if !matches!(self.judge.as_str(), "auto" | "local" | "hybrid" | "ai") {
            return Err("judge 只能是 auto、local、hybrid 或 ai".into());
        }
        if !(0.05..=0.95).contains(&self.ask_floor) || !(self.ask_floor..=1.0).contains(&self.sure) {
            return Err("须 0.05 ≤ ask_floor ≤ sure ≤ 1".into());
        }
        if !(5..=120).contains(&self.ai_timeout_seconds) {
            return Err("ai_timeout_seconds 须在 5–120 之间".into());
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
        let target = self.report_target.trim();
        let id = target.strip_prefix("private:").unwrap_or(target);
        if !target.is_empty() && id.parse::<u64>().is_err() {
            return Err("report_target 写群号、private:QQ号，或留空（发到号主自己的私聊）".into());
        }
        if !(1..=30).contains(&self.keep_days) {
            return Err("keep_days 须在 1–30 之间".into());
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

/// 实际用哪种认图方式。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Local,
    Hybrid,
    Ai,
}

impl Mode {
    async fn of(ctx: &Context, config: &Config) -> Mode {
        let wanted = match config.judge.as_str() {
            "local" => return Mode::Local,
            "ai" => Mode::Ai,
            _ => Mode::Hybrid,
        };
        if ai::available(ctx, &config.ai_model).await { wanted } else { Mode::Local }
    }

    fn text(self, config: &Config) -> String {
        let model = config.ai_model.rsplit('/').next().unwrap_or(&config.ai_model);
        match self {
            Mode::Local => "本地模型".into(),
            Mode::Hybrid => format!("本地模型初筛 + {model} 复核拿不准的"),
            Mode::Ai => format!("每张都问 {model}"),
        }
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
    /// 模型没认出来，但商城表情的名字里写着这类。
    by_name: bool,
    /// 大模型复核的结论（问过才有）。
    ai: Option<ai::Opinion>,
    /// 最像的是第几帧（大模型看的就是这帧）。
    frame: usize,
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

/// 一张要看的图：普通图片带直链；商城表情只有编号，图从 QQ 的表情资源站取。
#[derive(Clone)]
struct Picture {
    url: String,
    mface: Option<Mface>,
}

#[derive(Clone)]
struct Mface {
    /// 32 位十六进制编号，同一个表情处处相同。
    id: String,
    /// 表情名，QQ 给的带方括号（`[奶龙摇摆]`）。
    name: String,
}

impl Mface {
    /// 动图版；个别只有静图的表情退到 PNG。
    fn urls(&self) -> [String; 2] {
        let base = format!("https://gxh.vip.qq.com/club/item/parcel/item/{}/{}", &self.id[..2], self.id);
        [format!("{base}/raw300.gif"), format!("{base}/300x300.png")]
    }

    /// 表情名里写着的那类。
    fn named_kind(&self) -> Option<Kind> {
        if self.name.contains("奶蛙") {
            Some(Kind::Naiwa)
        } else if self.name.contains("奶龙") {
            Some(Kind::Nailong)
        } else {
            None
        }
    }
}

/// 从一个消息段认出要看的图：`image` 取直链，`mface` 取编号（`check_mface` 关掉就不看）。
fn picture(kind: &str, data: &simd_json::owned::Object, config: &Config) -> Option<Picture> {
    match kind {
        "image" => {
            let url = data
                .get("url")
                .and_then(|v| v.as_str())
                .filter(|url| !url.is_empty())
                .or_else(|| data.get("file").and_then(|v| v.as_str()).filter(|f| f.starts_with("http")))?;
            Some(Picture { url: url.to_string(), mface: None })
        }
        "mface" if config.check_mface => {
            let id = data.get("emoji_id").and_then(|v| v.as_str())?.trim().to_ascii_lowercase();
            if id.len() < 2 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            let name = data.get("summary").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let mface = Mface { id, name };
            Some(Picture { url: mface.urls()[0].clone(), mface: Some(mface) })
        }
        _ => None,
    }
}

async fn download(url: &str) -> Result<Vec<u8>, String> {
    let bytes = tokio::time::timeout(Duration::from_secs(20), download_bytes(url))
        .await
        .map_err(|_| "下载超时".to_string())?
        .map_err(|e| format!("下载失败：{e}"))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!("图片有 {} MB，太大", bytes.len() / 1024 / 1024));
    }
    Ok(bytes)
}

fn cached(key: &str) -> Option<Verdict> {
    VERDICTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).map.get(key).cloned()
}

fn cache(key: String, verdict: &Verdict) {
    let mut cache = VERDICTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache.map.insert(key.clone(), verdict.clone()).is_none() {
        cache.order.push_back(key);
        if cache.order.len() > VERDICT_CACHE
            && let Some(old) = cache.order.pop_front()
        {
            cache.map.remove(&old);
        }
    }
}

async fn fetch(picture: &Picture) -> Result<Vec<u8>, String> {
    match &picture.mface {
        None => download(&picture.url).await,
        Some(mface) => {
            let [gif, png] = mface.urls();
            match download(&gif).await {
                Ok(bytes) => Ok(bytes),
                Err(_) => download(&png).await,
            }
        }
    }
}

/// 看一张图。认出来要存档时用得着原图，顺带交回（商城表情按编号命中缓存时没下图，是 `None`）。
/// 问不问大模型。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Review {
    /// 按 `judge` 与分数区间来。
    Auto,
    /// 配了模型就问（「奶龙 测」要看它怎么说）。
    Always,
    /// 不问（纠正时只要向量）。
    Never,
}

async fn inspect(ctx: &Context, picture: &Picture, config: &Config, review: Review) -> Result<(Verdict, Option<Vec<u8>>), String> {
    // 模型还没就绪（下载失败在冷却）就不必下图了。
    let encoder = model::encoder().await?;
    // 商城表情按编号记结论，再出现连图都不用下。
    let mface_key = picture.mface.as_ref().filter(|_| review == Review::Auto).map(|m| format!("mface:{}", m.id));
    if let Some(verdict) = mface_key.as_deref().and_then(cached) {
        return Ok((verdict, None));
    }
    let bytes = fetch(picture).await?;
    let mut verdict = judge_bytes(encoder, &bytes, config).await?;
    if let Some(mface) = &picture.mface {
        // 名字只补模型的漏：教过「不是」的照样放过。
        if verdict.kind == Kind::Other
            && verdict.taught.is_none()
            && let Some(kind) = mface.named_kind()
        {
            verdict.kind = kind;
            verdict.by_name = true;
        }
    }
    consult(ctx, config, &bytes, &mut verdict, review).await;
    if let Some(key) = mface_key {
        cache(key, &verdict);
    }
    Ok((verdict, Some(bytes)))
}

/// 该问大模型就问，按它的结论改判；问不成就放过（设了 `sure` 且本地够高的除外）。教过的样本与表情名优先，不问。
async fn consult(ctx: &Context, config: &Config, bytes: &[u8], verdict: &mut Verdict, review: Review) {
    if review == Review::Never || verdict.taught.is_some() || (verdict.by_name && review == Review::Auto) {
        return;
    }
    let mode = Mode::of(ctx, config).await;
    let strength = verdict.scores.nailong.max(verdict.scores.naiwa);
    let ask = match (review, mode) {
        (Review::Always, _) => ai::available(ctx, &config.ai_model).await,
        (_, Mode::Local) => false,
        (_, Mode::Ai) => true,
        (_, Mode::Hybrid) => strength >= config.ask_floor && (config.sure >= 1.0 || strength < config.sure),
    };
    if !ask {
        // 复核模式下本地分数到了 `sure`：不问，直接按本地更像的那类拦。
        if review == Review::Auto && mode != Mode::Local && config.sure < 1.0 && strength >= config.sure {
            verdict.kind = if verdict.scores.naiwa > verdict.scores.nailong { Kind::Naiwa } else { Kind::Nailong };
        }
        return;
    }
    let digest = format!("{:x}", md5::compute(bytes));
    let png = match frame_png(bytes.to_vec(), config.max_frames, verdict.frame).await {
        Ok(png) => png,
        Err(error) => return debug!(target: LOG_TARGET, "复核取帧失败：{error}"),
    };
    let timeout = Duration::from_secs(config.ai_timeout_seconds);
    match ai::ask(ctx, &config.ai_model, &png, &digest, config.ai_daily_limit, timeout).await {
        Ok(opinion) => {
            verdict.ai = Some(opinion);
            // 说不是就放；说是但自己都没把握的，仍按本地的来。
            if opinion.kind == Kind::Other || opinion.confidence >= 0.5 {
                verdict.kind = opinion.kind;
            }
        }
        Err(error) => {
            // 本地模型单独认错的多（截图都能打高分）：复核不成宁可漏，不拿本地的分数去撤别人的图。
            // 只有设了 `sure` 且本地分数到了的，照样按本地拦。
            if review == Review::Auto && !(config.sure < 1.0 && strength >= config.sure) {
                verdict.kind = Kind::Other;
            }
            debug!(target: LOG_TARGET, "没复核成，这张先放过：{error}");
        }
    }
}

/// 第 `index` 帧转成 PNG（垫白、边长不过 512），给大模型看。
async fn frame_png(bytes: Vec<u8>, max_frames: usize, index: usize) -> Result<Vec<u8>, String> {
    tokio::task::spawn_blocking(move || {
        let frames = decode_frames(&bytes, max_frames)?;
        let frame = frames.get(index).or(frames.first()).ok_or("没有可看的帧")?;
        let mut canvas = image::RgbImage::from_pixel(frame.width(), frame.height(), image::Rgb([255, 255, 255]));
        for (x, y, pixel) in frame.enumerate_pixels() {
            let alpha = u16::from(pixel[3]);
            let blend = |c: u8| ((u16::from(c) * alpha + 255 * (255 - alpha)) / 255) as u8;
            canvas.put_pixel(x, y, image::Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])]));
        }
        let mut png = Vec::new();
        canvas
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        Ok(png)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 按内容摘要记结论：同一张图换了地址、换人转发，都不必再算。
async fn judge_bytes(encoder: std::sync::Arc<model::Encoder>, bytes: &[u8], config: &Config) -> Result<Verdict, String> {
    let digest = format!("{:x}", md5::compute(bytes));
    if let Some(verdict) = cached(&digest) {
        return Ok(verdict);
    }
    let verdict = embed_and_judge(encoder, bytes.to_vec(), config).await?;
    cache(digest, &verdict);
    Ok(verdict)
}

async fn embed_and_judge(encoder: std::sync::Arc<model::Encoder>, bytes: Vec<u8>, config: &Config) -> Result<Verdict, String> {
    let _permit = INFER.acquire().await.map_err(|e| e.to_string())?;
    let (max_frames, threshold) = (config.max_frames, config.threshold);
    tokio::task::spawn_blocking(move || -> Result<Verdict, String> {
        let frames = decode_frames(&bytes, max_frames)?;
        let mut best: Option<Verdict> = None;
        let mut vectors = Vec::new();
        for (index, frame) in frames.iter().enumerate() {
            let vector = encoder.embed(frame)?;
            let (kind, scores, taught) = judge(&vector, threshold);
            vectors.push(vector);
            let strength = |v: &Verdict| v.scores.nailong.max(v.scores.naiwa);
            let candidate = Verdict { kind, scores, taught, by_name: false, ai: None, frame: index, vectors: Vec::new() };
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
    .map_err(|e| format!("识别线程异常：{e}"))?
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

// ==================== 拦下的图 ====================

/// 拦下（或提醒过）的一次。图在群里撤了就再也看不到，所以原图另存一份、编上号：
/// 当时没看群，事后也能翻出来、按编号纠正，重启也不丢。
#[derive(Serialize, Deserialize, Clone)]
struct Caught {
    /// 编号，指令里用（「奶龙 不是 12」）。
    no: u64,
    self_id: String,
    group: String,
    #[serde(default)]
    group_name: String,
    user: String,
    name: String,
    /// 原图那条消息（多半已撤回）。
    message_id: String,
    /// 群里那条提醒；文案留空时没有。
    tip: Option<String>,
    /// 抄给号主核对的那条。
    report: Option<String>,
    /// 当事人喊冤时转给号主的那条。
    appeal: Option<String>,
    kind: Kind,
    /// 凭什么认的：「模型 87%」「像教过的一张（95%）」「表情名写着奶龙」。
    basis: String,
    /// 做了什么：「撤回、禁言 10 分钟（第 2 次）」「只提醒（账号不是管理员）」。
    action: String,
    /// 禁言到什么时候；0 = 没禁言。
    muted_until: i64,
    at: i64,
    /// 已改判「不是」。
    cleared: bool,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct CaughtLog {
    next: u64,
    items: Vec<Caught>,
}

static CAUGHT: JsonState<CaughtLog> = JsonState::new(LOG_TARGET, "nailong", "caught.json", "奶龙拦截记录");
const MAX_CAUGHT: usize = 500;

fn caught_dir() -> Option<std::path::PathBuf> {
    crate::storage::data_path(<Config as PluginConfig>::NAME).ok().map(|dir| dir.join("caught"))
}

fn caught_file(no: u64) -> Option<std::path::PathBuf> {
    caught_dir().map(|dir| dir.join(no.to_string()))
}

/// 记下这一次、存好原图，返回编号。过了保留期的连图一起清掉。
async fn save_caught(mut caught: Caught, image: &[u8], keep_days: u64) -> u64 {
    let keep = i64::try_from(keep_days * 86400).unwrap_or(7 * 86400);
    let (no, expired) = CAUGHT
        .with(|log| {
            log.next = log.next.max(log.items.iter().map(|c| c.no).max().unwrap_or(0)) + 1;
            caught.no = log.next;
            let mut expired = Vec::new();
            log.items.retain(|c| {
                let keep = caught.at - c.at < keep;
                if !keep {
                    expired.push(c.no);
                }
                keep
            });
            log.items.push(caught);
            while log.items.len() > MAX_CAUGHT {
                expired.push(log.items.remove(0).no);
            }
            (log.next, expired)
        })
        .await;
    if let Some(path) = caught_file(no)
        && let Err(error) = crate::storage::write_atomic_async(path, image.to_vec()).await
    {
        warn!(target: LOG_TARGET, "拦下的图 #{no} 没存上：{error}");
    }
    for old in expired {
        if let Some(path) = caught_file(old) {
            let _ = tokio::fs::remove_file(path).await;
        }
    }
    no
}

async fn update_caught(no: u64, change: impl FnOnce(&mut Caught)) {
    CAUGHT
        .with(|log| {
            if let Some(caught) = log.items.iter_mut().find(|c| c.no == no) {
                change(caught);
            }
        })
        .await;
}

/// 按编号，或按引用的那条（提醒、抄送、喊冤、原图）找那一次。`group` 给了就只在这个群里找。
async fn find_caught(group: Option<&str>, no: Option<u64>, quoted: Option<&str>) -> Option<Caught> {
    CAUGHT
        .with(|log| {
            log.items
                .iter()
                .rev()
                .filter(|c| group.is_none_or(|g| c.group == g))
                .find(|c| match (no, quoted) {
                    (Some(no), _) => c.no == no,
                    (None, Some(id)) => {
                        c.message_id == id
                            || [&c.tip, &c.report, &c.appeal].iter().any(|m| m.as_deref() == Some(id))
                    }
                    (None, None) => false,
                })
                .cloned()
        })
        .await
}

async fn caught_image(no: u64) -> Option<Vec<u8>> {
    tokio::fs::read(caught_file(no)?).await.ok()
}

/// 抄送、喊冤发到哪：(群号, 私聊对象)。留空就是号主自己的私聊（登录账号）。
fn report_place(ctx: &Context, config: &Config) -> Option<(Option<String>, Option<String>)> {
    let target = config.report_target.trim();
    if let Some(user) = target.strip_prefix("private:") {
        return Some((None, Some(user.to_string())));
    }
    if !target.is_empty() {
        return Some((Some(target.to_string()), None));
    }
    let me = ctx.bot.self_id();
    (!me.is_empty()).then_some((None, Some(me)))
}

/// 指令怎么写：带上第一个指令前缀，照着就能发。
fn spelled(ctx: &Context, rest: &str) -> String {
    let prefix = command::get_prefixes(ctx).into_iter().next().unwrap_or_default();
    format!("「{prefix}奶龙 {rest}」")
}

fn when(at: i64) -> String {
    let Some(time) = chrono::DateTime::from_timestamp(at, 0).map(|t| t.with_timezone(&crate::clock::beijing())) else {
        return String::new();
    };
    let today = crate::clock::beijing_now().date_naive();
    match (today - time.date_naive()).num_days() {
        0 => time.format("今天 %H:%M").to_string(),
        1 => time.format("昨天 %H:%M").to_string(),
        _ => time.format("%m-%d %H:%M").to_string(),
    }
}

fn group_label(caught: &Caught) -> String {
    if caught.group_name.is_empty() {
        format!("群 {}", caught.group)
    } else {
        format!("{}（{}）", caught.group_name, caught.group)
    }
}

/// 抄给号主：原图 + 这次的情形 + 认错了怎么纠正。
async fn report(ctx: &Context, writer: &LockedWriter, config: &Config, caught: &Caught, image: &[u8]) {
    let Some((group, user)) = report_place(ctx, config) else {
        return;
    };
    let text = format!(
        "\n🐉 #{} 拦下一张{}图\n{} · {}（{}）\n{} · {}\n认错了引用这条回{}",
        caught.no,
        caught.kind.name(),
        group_label(caught),
        caught.name,
        caught.user,
        caught.basis,
        caught.action,
        spelled(ctx, "不是"),
    );
    let message = Message::new().image(format!("base64://{}", STANDARD.encode(image))).text(text);
    match send_msg_id(ctx, writer.clone(), group.as_deref(), user.as_deref(), message).await {
        Ok(Some(id)) => update_caught(caught.no, |c| c.report = Some(id)).await,
        Ok(None) => {}
        Err(error) => warn!(target: LOG_TARGET, "#{} 没抄送出去：{error}", caught.no),
    }
}

// ==================== 事件 ====================

pub fn handle(ctx: Context, writer: LockedWriter) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let Some(message) = ctx.as_message() else {
            return Ok(Some(ctx));
        };
        let config = get_config_or_default::<Config>(&ctx);
        let Some(group) = message.group_id().filter(|id| !id.is_empty()).map(str::to_owned) else {
            // 私聊里只接号主（与 ctl 管理员）：核对抄过来的图、按编号纠正。
            if crate::access::is_manager(&ctx)
                && let Some(matched) = COMMANDS.iter().find_map(|word| command::match_word_command(&ctx, word))
            {
                let place = Place { group: None, user: Some(message.user_id().to_string()) };
                if run_command(&ctx, &writer, &config, &place, matched).await? {
                    return Ok(None);
                }
            }
            return Ok(Some(ctx));
        };
        if !config.channel.allows_group(&group) {
            return Ok(Some(ctx));
        }
        if let Some(matched) = COMMANDS.iter().find_map(|word| command::match_word_command(&ctx, word)) {
            let place = Place { group: Some(group.clone()), user: None };
            if run_command(&ctx, &writer, &config, &place, matched).await? {
                return Ok(None);
            }
        }
        let user = message.user_id().to_string();
        if user.is_empty() || user == ctx.bot.self_id() || config.ignore_users.contains(&user) {
            return Ok(Some(ctx));
        }
        let pictures = pictures(message.0.get_array("message").map_or(&[][..], Vec::as_slice), &config);
        if pictures.is_empty() {
            return Ok(Some(ctx));
        }
        if config.exempt_staff
            && (crate::access::is_manager(&ctx) || message.sender_role().is_some_and(api::is_staff))
        {
            return Ok(Some(ctx));
        }
        if QUEUED.load(Ordering::Relaxed) >= MAX_QUEUED {
            debug!(target: LOG_TARGET, "排队的图太多，群 {group} 这条先不看");
            return Ok(Some(ctx));
        }
        let target = Target {
            group,
            group_name: message.0.get_str("group_name").unwrap_or_default().to_string(),
            user,
            name: message.sender_name().to_string(),
            message_id: message.message_id().to_string(),
        };
        let (task_ctx, task_writer) = (ctx.clone(), writer.clone());
        let queued = Queued::enter();
        tokio::spawn(async move {
            let found = scan(&task_ctx, &pictures, &config).await;
            drop(queued);
            if let Some(found) = found
                && let Err(error) = punish(&task_ctx, &task_writer, &config, &target, found).await
            {
                warn!(target: LOG_TARGET, "群 {} 处理奶龙图失败：{error}", target.group);
            }
        });
        Ok(Some(ctx))
    })
}

fn pictures(segments: &[OwnedValue], config: &Config) -> Vec<Picture> {
    segments
        .iter()
        .filter_map(|segment| picture(segment.get_str("type")?, segment.get("data")?.as_object()?, config))
        .take(config.max_images)
        .collect()
}

/// 一条消息里的图挨个看，认出一张就停。交回结论与原图。
async fn scan(ctx: &Context, pictures: &[Picture], config: &Config) -> Option<(Verdict, Vec<u8>)> {
    for picture in pictures {
        match inspect(ctx, picture, config, Review::Auto).await {
            Ok((verdict, bytes)) if verdict.hits(config) => {
                let bytes = match bytes {
                    Some(bytes) => bytes,
                    // 按编号命中缓存时没下图；存档要用，补下一次。
                    None => fetch(picture).await.unwrap_or_default(),
                };
                return Some((verdict, bytes));
            }
            Ok(_) => {}
            Err(error) => debug!(target: LOG_TARGET, "这张图没看成：{error}"),
        }
    }
    None
}

struct Target {
    group: String,
    group_name: String,
    user: String,
    name: String,
    message_id: String,
}

fn basis(verdict: &Verdict) -> String {
    let local = verdict.scores.nailong.max(verdict.scores.naiwa);
    match (verdict.taught, verdict.ai) {
        (Some((_, similarity)), _) => format!("像教过的一张（{:.0}%）", similarity * 100.0),
        _ if verdict.by_name => format!("表情名写着{}", verdict.kind.name()),
        (None, Some(opinion)) => format!(
            "本地 {:.0}%，大模型复核认作{}（{:.0}%）",
            local * 100.0,
            opinion.kind.name(),
            opinion.confidence * 100.0
        ),
        (None, None) => format!("模型 {:.0}%", verdict.scores.of(verdict.kind) * 100.0),
    }
}

async fn punish(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    target: &Target,
    (verdict, image): (Verdict, Vec<u8>),
) -> Result<(), PluginError> {
    let Target { group, user, name, message_id, .. } = target;
    let kind = verdict.kind;
    let basis = basis(&verdict);
    let able = api::is_staff(&api::own_role(ctx, writer.clone(), group).await);
    info!(target: LOG_TARGET, "群 {group} 的 {name}（{user}）发了{}图（{basis}）", kind.name());
    let vars = |count: usize, duration: u64| Vars { name, kind: kind.name(), count, duration: duration_text(duration) };
    let now = chrono::Utc::now().timestamp();
    let (tip, action, muted_until) = if able {
        let recalled = config.recall
            && match api::delete_msg_in(ctx, writer.clone(), group, message_id).await {
                Ok(()) => true,
                Err(error) => {
                    warn!(target: LOG_TARGET, "群 {group} 撤回 {message_id} 失败：{error}");
                    false
                }
            };
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
        let mut done = Vec::new();
        if recalled {
            done.push("撤回".to_string());
        }
        if muted {
            done.push(format!("禁言 {}", duration_text(duration)));
        }
        if done.is_empty() {
            done.push("只提醒".to_string());
        }
        let action = format!("{}（{} 小时内第 {count} 次）", done.join("、"), config.window_hours);
        let template = if muted { &config.mute_text } else { &config.recall_text };
        let tip = if template.trim().is_empty() {
            None
        } else {
            let message = render_into(Message::new(), template, user, &vars(count, duration));
            send_msg_id(ctx, writer.clone(), Some(group), None, message).await?
        };
        let muted_until = if muted { now + i64::try_from(duration).unwrap_or(0) } else { 0 };
        (tip, action, muted_until)
    } else {
        let tip = if config.remind_without_admin && !config.remind_text.trim().is_empty() {
            let message = render_into(Message::new().reply(message_id), &config.remind_text, user, &vars(0, 0));
            send_msg_id(ctx, writer.clone(), Some(group), None, message).await?
        } else {
            None
        };
        (tip, "只提醒（账号不是管理员）".to_string(), 0)
    };
    let caught = Caught {
        no: 0,
        self_id: ctx.bot.self_id(),
        group: group.clone(),
        group_name: target.group_name.clone(),
        user: user.clone(),
        name: name.clone(),
        message_id: message_id.clone(),
        tip,
        report: None,
        appeal: None,
        kind,
        basis,
        action,
        muted_until,
        at: now,
        cleared: false,
    };
    let no = save_caught(caught.clone(), &image, config.keep_days).await;
    // 只提醒的那种，图还在群里，看得见；撤了的才需要抄一份。
    if config.report && able && !image.is_empty() {
        report(ctx, writer, config, &Caught { no, ..caught }, &image).await;
    }
    Ok(())
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

/// 指令从哪来、往哪回：群里（`group`），或私聊（`user`，号主与 ctl 管理员）。
struct Place {
    group: Option<String>,
    user: Option<String>,
}

async fn run_command(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    place: &Place,
    matched: CommandMatch,
) -> Result<bool, PluginError> {
    let Some(message) = ctx.as_message() else {
        return Ok(false);
    };
    let message_id = message.message_id().to_string();
    let say = |text: String| {
        send_msg(
            ctx,
            writer.clone(),
            place.group.as_deref(),
            place.user.as_deref(),
            Message::new().reply(&message_id).text(text),
        )
    };
    let arg = command::extract_text_arg(&matched.args);
    let word = arg.split_whitespace().next().unwrap_or("");
    let rest = arg[word.len()..].trim();
    let number = rest.trim_start_matches(['#', '＃']).parse::<u64>().ok();
    let attached = pictures(&matched.args, config);
    let wants_image = matched.reply_id.is_some() || !attached.is_empty();
    let ask = Ask { place, matched: &matched, attached, number };
    match word {
        "" | "测" | "测试" | "看看" if wants_image || number.is_some() => test(ctx, writer, config, ask, &say).await?,
        "记录" | "最近" => say(history(ctx, place).await).await?,
        "" | "状态" | "帮助" => match &place.group {
            Some(group) => say(status(ctx, writer, config, group).await).await?,
            None => say(history(ctx, place).await).await?,
        },
        "看" | "图" if place.group.is_none() => show(ctx, writer, place, number, &say).await?,
        "是" | "算" | "拦" => {
            let explicit = target_kind(rest);
            correct(ctx, writer, config, ask, Correction::Is(explicit), &say).await?;
        }
        "不是" | "不算" | "放过" => correct(ctx, writer, config, ask, Correction::IsNot, &say).await?,
        "忘了" | "忘记" => correct(ctx, writer, config, ask, Correction::Forget, &say).await?,
        // 没有指令前缀时「奶龙 好可爱」只是在聊天，不接话。
        _ if command::get_prefixes(ctx).iter().all(String::is_empty) => return Ok(false),
        _ => say(format!("❌ 不认识「{word}」\n{}", usage())).await?,
    }
    Ok(true)
}

/// 一条指令问的是哪张图：编号、引用、附图。
struct Ask<'a> {
    place: &'a Place,
    matched: &'a CommandMatch,
    attached: Vec<Picture>,
    number: Option<u64>,
}

fn usage() -> String {
    "引用一张图或表情发「奶龙 测」看分数\n\
     管理员引用图或机器人的提醒发「奶龙 是」「奶龙 不是」纠正，「奶龙 忘了」撤销\n\
     没看群也不怕：「奶龙 记录」翻近几天拦下的，「奶龙 不是 编号」改判"
        .into()
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
    let mut lines = vec![
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
    ];
    let mode = Mode::of(ctx, config).await;
    lines.push(match mode {
        Mode::Local if config.judge != "local" => format!("认图：本地模型（{} 不可用：oai 没开或没配这个供应商）", config.ai_model),
        Mode::Local => "认图：本地模型".to_string(),
        _ => format!("认图：{} · 今天复核 {}/{} 张", mode.text(config), ai::used_today().await, config.ai_daily_limit),
    });
    if config.report {
        lines.push("撤下的图另抄一份给号主核对".to_string());
    }
    lines.push(String::new());
    lines.push(usage());
    lines.join("\n")
}

/// 近几天拦下的：群里只列本群，私聊列全部。最新的在前，最多十条。
async fn history(ctx: &Context, place: &Place) -> String {
    let me = ctx.bot.self_id();
    let items: Vec<Caught> = CAUGHT
        .with(|log| {
            log.items
                .iter()
                .filter(|c| c.self_id == me && place.group.as_ref().is_none_or(|g| &c.group == g))
                .cloned()
                .collect()
        })
        .await;
    if items.is_empty() {
        return "📭 近几天没拦过".into();
    }
    let mut lines = vec![format!("🐉 近几天拦下 {} 次{}", items.len(), if items.len() > 10 { "，列最近 10 次" } else { "" })];
    for c in items.iter().rev().take(10) {
        let place_text = if place.group.is_none() { format!(" · {}", group_label(c)) } else { String::new() };
        let cleared = if c.cleared { " · 已改判不是" } else { "" };
        lines.push(format!(
            "#{} {}{place_text}\n   {}（{}）· {} · {} · {}{cleared}",
            c.no,
            when(c.at),
            c.name,
            c.user,
            c.kind.name(),
            c.basis,
            c.action
        ));
    }
    lines.push(String::new());
    lines.push(format!("认错了发{}", spelled(ctx, "不是 编号")));
    if place.group.is_none() {
        lines.push(format!("看图发{}", spelled(ctx, "看 编号")));
    }
    lines.join("\n")
}

/// 私聊里按编号看那张图。
async fn show<F>(
    ctx: &Context,
    writer: &LockedWriter,
    place: &Place,
    number: Option<u64>,
    say: &impl Fn(String) -> F,
) -> Result<(), PluginError>
where
    F: Future<Output = Result<(), crate::adapters::satori::BotError>>,
{
    let Some(no) = number else {
        return say(format!("❌ 写上编号，比如{}", spelled(ctx, "看 12"))).await;
    };
    let (Some(caught), Some(image)) = (find_caught(None, Some(no), None).await, caught_image(no).await) else {
        return say(format!("📭 没有 #{no}，或者过了保留期")).await;
    };
    let text = format!(
        "\n#{} {} · {} · {}（{}）\n{} · {}",
        caught.no,
        when(caught.at),
        group_label(&caught),
        caught.name,
        caught.user,
        caught.basis,
        caught.action
    );
    let message = Message::new().image(format!("base64://{}", STANDARD.encode(&image))).text(text);
    send_msg(ctx, writer.clone(), None, place.user.as_deref(), message).await?;
    Ok(())
}

/// 问的是哪张：先认拦下过的那次（按编号，或引用了提醒、抄送、喊冤），图从存档里取；
/// 再取引用那条里的图、附带的图。
async fn subject(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    ask: &Ask<'_>,
    review: Review,
) -> Result<(Option<Caught>, Vec<Verdict>), String> {
    let scope = ask.place.group.as_deref();
    if ask.number.is_some() || ask.matched.reply_id.is_some() {
        if let Some(caught) = find_caught(scope, ask.number, ask.matched.reply_id.as_deref()).await {
            let image = caught_image(caught.no).await.ok_or("那张图没存下来，或者过了保留期")?;
            let mut verdict = judge_bytes(model::encoder().await?, &image, config).await?;
            consult(ctx, config, &image, &mut verdict, review).await;
            return Ok((Some(caught), vec![verdict]));
        }
        if let Some(no) = ask.number {
            return Err(format!("{}没有 #{no}，或者过了保留期", if scope.is_some() { "本群" } else { "" }));
        }
    }
    let mut pictures = ask.attached.clone();
    if pictures.is_empty()
        && let Some(reply) = &ask.matched.reply_id
    {
        let quoted = api::get_msg(ctx, writer.clone(), reply).await.map_err(|_| "引用的那条读不到了，可能已撤回或太久".to_string())?;
        // 指令里总要看商城表情：本群不拦，也能测分数、教样本。
        let config = Config { check_mface: true, ..config.clone() };
        pictures = quoted
            .message
            .0
            .iter()
            .filter_map(|seg| picture(&seg.type_, &seg.data, &config))
            .take(config.max_images)
            .collect();
    }
    if pictures.is_empty() {
        return Err("引用的那条里没有图".into());
    }
    let mut verdicts = Vec::new();
    for picture in &pictures {
        verdicts.push(inspect(ctx, picture, config, review).await?.0);
    }
    Ok((None, verdicts))
}

async fn test<F>(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    ask: Ask<'_>,
    say: &impl Fn(String) -> F,
) -> Result<(), PluginError>
where
    F: Future<Output = Result<(), crate::adapters::satori::BotError>>,
{
    let verdicts = match subject(ctx, writer, config, &ask, Review::Always).await {
        Ok((_, verdicts)) => verdicts,
        Err(error) => return say(format!("❌ {error}")).await,
    };
    let lines: Vec<String> = verdicts
        .iter()
        .enumerate()
        .map(|(index, verdict)| {
            let head = if verdicts.len() > 1 { format!("第 {} 张：", index + 1) } else { String::new() };
            let mut scores = format!("本地：奶龙 {:.0}% · 奶蛙 {:.0}%", verdict.scores.nailong * 100.0, verdict.scores.naiwa * 100.0);
            if let Some(opinion) = verdict.ai {
                scores.push_str(&format!("\n大模型：{}（{:.0}%）", opinion.kind.name(), opinion.confidence * 100.0));
            }
            let result = match (verdict.taught, verdict.hits(config)) {
                (Some((kind, s)), _) => format!("像教过的一张{}图（{:.0}%），按{}算", kind.name(), s * 100.0, kind.name()),
                (None, hits) if verdict.by_name => {
                    let tail = if hits { "会拦" } else { "但本群没拦这类" };
                    format!("表情名写着{}，{tail}", verdict.kind.name())
                }
                (None, true) => format!("会拦：{}", verdict.kind.name()),
                (None, false) if verdict.kind != Kind::Other => format!("像{}，但本群没拦这类", verdict.kind.name()),
                (None, false) if verdict.ai.is_some() => "不拦（大模型看过，不是）".to_string(),
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

async fn correct<F>(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    ask: Ask<'_>,
    correction: Correction,
    say: &impl Fn(String) -> F,
) -> Result<(), PluginError>
where
    F: Future<Output = Result<(), crate::adapters::satori::BotError>>,
{
    let Some(message) = ctx.as_message() else {
        return Ok(());
    };
    if ask.matched.reply_id.is_none() && ask.attached.is_empty() && ask.number.is_none() {
        return say(format!(
            "❌ 先引用那张图或机器人的那条提醒再发这句；图撤了找不到，发{}看编号",
            spelled(ctx, "记录")
        ))
        .await;
    }
    let by = message.user_id();
    // 私聊进得来的只有号主与 ctl 管理员。
    let allowed = ask.place.group.is_none()
        || crate::access::is_manager(ctx)
        || message.sender_role().is_some_and(api::is_staff);
    if !allowed {
        // 被拦的本人说「不是」：转给号主看，不让他自己给自己改判。
        if matches!(correction, Correction::IsNot)
            && let Some(caught) =
                find_caught(ask.place.group.as_deref(), ask.number, ask.matched.reply_id.as_deref()).await
            && caught.user == by
        {
            return say(appeal(ctx, writer, config, &caught).await).await;
        }
        return say("❌ 只有群主、管理员能纠正\n想看分数可以发「奶龙 测」".into()).await;
    }
    let (caught, verdicts) = match subject(ctx, writer, config, &ask, Review::Never).await {
        Ok(found) => found,
        Err(error) => return say(format!("❌ {error}")).await,
    };
    let vectors: Vec<Vec<f32>> = verdicts.iter().flat_map(|v| v.vectors.iter().cloned()).collect();
    let group = ask.place.group.as_deref().or(caught.as_ref().map(|c| c.group.as_str())).unwrap_or_default();
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
            if let Some(caught) = caught.filter(|c| !c.cleared) {
                text.push_str(&undo(ctx, writer, &caught, ask.place.group.is_none()).await);
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

/// 被拦的本人喊冤：把这次连同原图转给号主（引用当初那条抄送），每次只转一回。
async fn appeal(ctx: &Context, writer: &LockedWriter, config: &Config, caught: &Caught) -> String {
    if caught.cleared {
        return "这次已经改判过了，不算你的".into();
    }
    if caught.appeal.is_some() {
        return "已经转给号主了，认错了会解除禁言、不计次数".into();
    }
    let Some((group, user)) = report_place(ctx, config).filter(|_| config.report) else {
        return "❌ 只有群主、管理员能纠正，找他们说一声".into();
    };
    let mut message = Message::new();
    if let Some(report) = &caught.report {
        message = message.reply(report);
    } else if let Some(image) = caught_image(caught.no).await {
        message = message.image(format!("base64://{}", STANDARD.encode(&image)));
    }
    let message = message.text(format!(
        "\n🙋 #{} {}（{}）说这张不是{}图\n{} · {} · {}\n认错了引用这条回{}",
        caught.no,
        caught.name,
        caught.user,
        caught.kind.name(),
        group_label(caught),
        caught.basis,
        caught.action,
        spelled(ctx, "不是"),
    ));
    match send_msg_id(ctx, writer.clone(), group.as_deref(), user.as_deref(), message).await {
        Ok(id) => {
            update_caught(caught.no, |c| c.appeal = Some(id.unwrap_or_default())).await;
            "已转给号主核对，认错了会解除禁言、不计次数".into()
        }
        Err(error) => {
            warn!(target: LOG_TARGET, "#{} 喊冤没转出去：{error}", caught.no);
            "❌ 没转出去，找群主、管理员说一声吧".into()
        }
    }
}

/// 「不是」的善后：解除这次禁言、这次不计入次数、撤回那条提醒。
/// 在私聊里改判的，群里没人看见，补一句给当事人。
async fn undo(ctx: &Context, writer: &LockedWriter, caught: &Caught, from_private: bool) -> String {
    let group = caught.group.as_str();
    let mut done = Vec::new();
    LEDGER
        .with(|ledger| {
            ledger.offenses.retain(|o| !(o.group == caught.group && o.message_id == caught.message_id));
        })
        .await;
    let mut unmuted = false;
    if caught.muted_until > chrono::Utc::now().timestamp() {
        match writer
            .call::<_, serde_json::Value>(ctx, "guild.member.mute", json!({"guild_id": group, "user_id": caught.user, "duration": 0}))
            .await
        {
            Ok(_) => {
                unmuted = true;
                done.push("解除了禁言".to_string());
            }
            Err(error) => {
                warn!(target: LOG_TARGET, "群 {group} 解除 {} 的禁言失败：{error}", caught.user);
                done.push("禁言没能解除，请手动解一下".to_string());
            }
        }
    }
    if let Some(tip) = &caught.tip
        && api::delete_msg_in(ctx, writer.clone(), group, tip).await.is_ok()
    {
        done.push("撤回了那条提醒".to_string());
    }
    update_caught(caught.no, |c| c.cleared = true).await;
    done.push("这次不计入次数".to_string());
    if from_private && (caught.tip.is_some() || unmuted) {
        let tail = if unmuted { "，禁言已解除" } else { "" };
        let message = Message::new()
            .at(&caught.user)
            .text(format!(" 刚才撤的那张认错了，不是{}图{tail}，不计次数。", caught.kind.name()));
        if let Err(error) = send_msg(ctx, writer.clone(), Some(group), None, message).await {
            warn!(target: LOG_TARGET, "群 {group} 改判通知没发出去：{error}");
        }
        done.push("在群里说了一声".to_string());
    }
    format!("\n#{} {}", caught.no, done.join("、"))
}

/// 启动（或刚被打开）时：样本读进内存，识图模型在后台先下好、载好，第一张图不必等。
pub fn init(_ctx: Context) -> BoxFuture<'static, Result<(), PluginError>> {
    Box::pin(async move {
        SAMPLES.with(|state| refresh_taught(state)).await;
        CAUGHT.preload().await;
        if let Some(dir) = caught_dir() {
            let _ = tokio::fs::create_dir_all(dir).await;
        }
        tokio::spawn(async {
            // 失败已在 encoder() 里记了日志，第一张图到来时还会再试。
            let _ = model::encoder().await;
        });
        Ok(())
    })
}
