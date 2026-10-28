//! 奶龙提醒：群里有人发奶龙、奶蛙图，把这张图与来龙去脉发给管理员，由人判断后处理。
//!
//! **主业是提醒。** 认错一张图的代价比漏掉一张高得多：默认只认图、只提醒，动不动手由群里的人决定。
//! 号主想省事时可以打开 `[nailong.manage]`（默认关）：账号是这个群的群主或管理员、这张图又
//! 认得有把握（大模型确认、教过、表情名写着，或本地分数够高）时，才顺手撤回、按次数禁言，
//! 做了什么照样写进提醒；认错了「不是」一句就解除禁言。不满足任一条件就退回只提醒。
//!
//! - **认图**见 [`model`]：图像向量 + 分类头，教过的样本优先。动图按帧抽查，哪一帧像就算。
//!   每张图下载一次、算一次，同一张图（按内容摘要）再出现直接用上次的结论。
//! - **大模型复核**见 [`ai`]：oai 里配好了多模态模型时，本地分数夹在中间、拿不准的图交给它看
//!   （`judge`）。本地觉得像的才问，很低的直接放，大模型只看一小部分；出错、超时、名额用完都退回本地。
//! - **商城表情**（QQ 表情商店里的动画表情，`mface`）消息里没有图片地址，只有编号：按编号到
//!   QQ 的表情资源站取动图来看，同一个编号只看一次。表情名里写着奶龙、奶蛙的不必等模型。
//! - **提醒**发到 `notify_targets`（群号或 `private:QQ号`，留空即号主自己的私聊）：原图、
//!   群与发图的人、凭什么认的、今天第几张。同一群同一个人 `repeat_minutes` 内只提醒一次，
//!   期间再发只记次数；全部群每小时最多提醒 `max_per_hour` 条，免得刷屏。
//!   群主、管理员与机器人管理员发的不管（`exempt_staff`）。
//! - **纠正**：管理员引用那张图（或私聊里引用提醒）发「奶龙 不是」：记住这类图不算。
//!   漏掉的图引用后发「奶龙 是」，往后同类图就提醒。任何人都能引用一张图发「奶龙 测」看分数。
//! - **事后核对**：发现的图另存一份、编上号，留 `keep_days` 天；「奶龙 记录」翻近几天的，
//!   「奶龙 不是 12」按编号改判，私聊里「奶龙 看 12」再看一遍那张图。
//!
//! 不发「正在识别」之类的过程话；没认出来就什么都不做。只有动了手（撤回、禁言）时，
//! 才按 `manage.notice` 在群里说一句为什么，两分钟后自己撤掉。

use crate::adapters::satori::{LockedWriter, api, send_msg, send_msg_id};
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
    #[serde(serialize_with = "short_f32")]
    threshold: f32,
    /// 认图方式：auto（oai 配了复核模型就用本地初筛 + 大模型复核，否则只用本地）、local（只用本地）、
    /// hybrid（本地初筛 + 大模型复核）、ai（每张图都问大模型，用量大）。
    judge: String,
    /// 复核用的多模态模型，写法同 oai：`供应商/模型`。
    ai_model: String,
    /// 本地分数到这个就直接提醒，不问大模型；1 = 本地觉得像的一律请大模型确认（默认，误判最少）。
    #[serde(serialize_with = "short_f32")]
    sure: f32,
    /// 本地分数到这个才问大模型；更低的直接放过。
    #[serde(serialize_with = "short_f32")]
    ask_floor: f32,
    /// 每天最多问大模型几张（所有群合计）；用完了当天本地觉得像的也先放过（宁漏勿错）。
    ai_daily_limit: u32,
    /// 问一张最多等几秒。
    ai_timeout_seconds: u64,
    /// 提醒发到哪：群号，或 `private:QQ号`；可以写多个。留空 = 号主自己的私聊。
    notify_targets: Vec<String>,
    /// 提醒里带不带原图（关掉只发文字）。
    notify_image: bool,
    /// 同一群同一个人多久内只提醒一次（分钟，0–1440）；期间再发只记次数。
    repeat_minutes: u64,
    /// 全部群每小时最多提醒几条（0 = 不限）；超出只记录、不再提醒。
    max_per_hour: u32,
    /// 提醒文案；留空只发原图。占位符：{no} {kind} {group} {name} {user} {basis} {count} {action} {prefix}
    notify_text: String,
    /// 群主、管理员与机器人管理员发的不管。
    exempt_staff: bool,
    /// 这些人发的不管。
    ignore_users: Vec<String>,
    /// 也看商城表情（QQ 表情商店的动画表情）。
    check_mface: bool,
    /// 发现的图（连同记录）留几天，期间能翻记录、按编号改判。
    keep_days: u64,
    /// 一条消息最多看几张图。
    max_images: usize,
    /// 动图最多抽查几帧。
    max_frames: usize,
    /// 群管理（默认关）：账号是群主或管理员时，认得有把握的图顺手撤回、禁言。
    manage: Manage,
}

/// 群管理。提醒之外的附加动作，三个条件都满足才动手：这里开着、账号是本群群主或管理员、
/// 这张图认得有把握；缺一样就只提醒。
#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Manage {
    enabled: bool,
    /// 只在这些群动手；留空 = 生效范围内账号是管理员的全部群。
    groups: Vec<String>,
    /// 撤回那张图。
    recall: bool,
    /// 禁言阶梯（秒）：近 `window_hours` 小时内第 1、2、3… 次各禁多久，超出按最后一档；0 = 不禁言。
    mute_seconds: Vec<u64>,
    /// 次数按多少小时内算。
    window_hours: u64,
    /// 只用本地模型认的，分数到这个才动手；大模型确认的、教过的、表情名写着的不看分数。
    #[serde(serialize_with = "short_f32")]
    min_score: f32,
    /// 动手后在群里说的话，两分钟后自己撤掉；留空不说。占位符：{at} {kind} {action}
    notice: String,
}

impl Default for Manage {
    fn default() -> Self {
        Self {
            enabled: false,
            groups: Vec::new(),
            recall: true,
            mute_seconds: vec![0, 600, 3600],
            window_hours: 24,
            min_score: 0.9,
            notice: "{at} 本群不收{kind}图，{action}".into(),
        }
    }
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
            notify_targets: Vec::new(),
            notify_image: true,
            repeat_minutes: 30,
            max_per_hour: 30,
            notify_text: "🐉 #{no} 可能是{kind}图\n{group} · {name}（{user}）\n{basis}\n本群今天第 {count} 张；{action}\n认错了引用这条回「{prefix}奶龙 不是」".into(),
            exempt_staff: true,
            ignore_users: Vec::new(),
            check_mface: true,
            keep_days: 7,
            max_images: 4,
            max_frames: 4,
            manage: Manage::default(),
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
        for target in &self.notify_targets {
            let target = target.trim();
            let id = target.strip_prefix("private:").unwrap_or(target);
            if target.is_empty() || id.parse::<u64>().is_err() {
                return Err("notify_targets 每项写群号或 private:QQ号；留空（不写）就发到号主自己的私聊".into());
            }
        }
        if self.repeat_minutes > 1440 {
            return Err("repeat_minutes 须在 0–1440 之间（0 表示每次都提醒）".into());
        }
        if self.max_per_hour > 1000 {
            return Err("max_per_hour 须在 0–1000 之间（0 表示不限）".into());
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
        let manage = &self.manage;
        if manage.groups.iter().any(|g| g.trim().parse::<u64>().is_err()) {
            return Err("manage.groups 每项写群号；留空 = 账号是管理员的全部群".into());
        }
        if manage.mute_seconds.is_empty() {
            return Err("manage.mute_seconds 至少要有一档（0 表示不禁言）".into());
        }
        if manage.mute_seconds.iter().any(|s| *s != 0 && !(60..=30 * 86400).contains(s)) {
            return Err("manage.mute_seconds 每档须为 0，或 60 秒到 30 天之间".into());
        }
        if !(1..=24 * 30).contains(&manage.window_hours) {
            return Err("manage.window_hours 须在 1–720 之间".into());
        }
        if !(0.5..=1.0).contains(&manage.min_score) {
            return Err("manage.min_score 须在 0.5–1 之间（1 = 只用本地模型时从不动手）".into());
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

/// 小数按最短的十进制写回配置：f32 直接存会写成 0.20000000298023224，控制台里也照样显示。
fn short_f32<S: serde::Serializer>(value: &f32, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_f64(value.to_string().parse().unwrap_or(f64::from(*value)))
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

// ==================== 发现的图 ====================

/// 发现（并提醒过）的一次。图以后可能被删、被刷走，所以原图另存一份、编上号：
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
    /// 原图那条消息（图还在群里，可引用它改判）。
    message_id: String,
    /// 发出去的提醒；一条提醒可能有多个去处。
    #[serde(default)]
    reports: Vec<String>,
    kind: Kind,
    /// 凭什么认的：「模型 87%」「像教过的一张（95%）」「表情名写着奶龙」。
    basis: String,
    /// 至少发出去一条提醒。
    #[serde(default)]
    notified: bool,
    /// 同一串里又发了几次（只在第一次提醒，之后只记次数）。
    #[serde(default = "one")]
    hits: u32,
    at: i64,
    /// 已改判「不是」。
    cleared: bool,
    /// 撤回了原图（群管理）。
    #[serde(default)]
    recalled: bool,
    /// 禁言到什么时候（时间戳，0 = 没禁）；改判「不是」时据此解除。
    #[serde(default)]
    muted_until: i64,
    /// 群里那句说明（两分钟后自己撤，改判时也撤）。
    #[serde(default)]
    notice: Option<String>,
}

fn one() -> u32 {
    1
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct CaughtLog {
    next: u64,
    items: Vec<Caught>,
}

static CAUGHT: JsonState<CaughtLog> = JsonState::new(LOG_TARGET, "nailong", "caught.json", "奶龙提醒记录");
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
        warn!(target: LOG_TARGET, "发现的图 #{no} 没存上：{error}");
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

/// 按编号，或按引用的那条（提醒、原图）找那一次。
///
/// 群里的编号与原图只在本群找；提醒可能发在别的群，引用它时不受群限制——
/// 否则把提醒发到管理员群后，在那儿回「不是」会找不到。
async fn find_caught(group: Option<&str>, no: Option<u64>, quoted: Option<&str>) -> Option<Caught> {
    CAUGHT
        .with(|log| {
            log.items
                .iter()
                .rev()
                .find(|c| match (no, quoted) {
                    (Some(no), _) => c.no == no && group.is_none_or(|g| c.group == g),
                    (None, Some(id)) if c.reports.iter().any(|m| m == id) => true,
                    (None, Some(id)) => c.message_id == id && group.is_none_or(|g| c.group == g),
                    (None, None) => false,
                })
                .cloned()
        })
        .await
}

async fn caught_image(no: u64) -> Option<Vec<u8>> {
    tokio::fs::read(caught_file(no)?).await.ok()
}

/// 提醒发到哪：(群号, 私聊对象)。`notify_targets` 里写群号或 `private:QQ号`；
/// 一个都没写就发到号主自己的私聊（登录账号）。
fn notify_places(ctx: &Context, config: &Config) -> Vec<(Option<String>, Option<String>)> {
    let mut places: Vec<(Option<String>, Option<String>)> = Vec::new();
    for target in &config.notify_targets {
        let target = target.trim();
        let place = match target.strip_prefix("private:") {
            Some(user) => (None, Some(user.to_string())),
            None => (Some(target.to_string()), None),
        };
        if !places.contains(&place) {
            places.push(place);
        }
    }
    if places.is_empty() {
        let me = ctx.bot.self_id();
        if !me.is_empty() {
            places.push((None, Some(me)));
        }
    }
    places
}

/// 提醒去哪的一句话说法，给状态指令用。
fn places_text(ctx: &Context, config: &Config) -> String {
    let places = notify_places(ctx, config);
    if places.is_empty() {
        return "没有可用的去处".into();
    }
    places
        .iter()
        .map(|(group, user)| match (group, user) {
            (Some(group), _) => format!("群 {group}"),
            (None, Some(user)) => format!("私聊 {user}"),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("、")
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

fn place_label(group_name: &str, group: &str) -> String {
    if group_name.is_empty() {
        format!("群 {group}")
    } else {
        format!("{group_name}（{group}）")
    }
}

fn group_label(caught: &Caught) -> String {
    place_label(&caught.group_name, &caught.group)
}

/// 提醒文案里的变量。
struct NotifyVars<'a> {
    no: u64,
    kind: Kind,
    group: &'a str,
    group_name: &'a str,
    name: &'a str,
    user: &'a str,
    basis: &'a str,
    count: usize,
    action: &'a str,
}

fn notify_text(ctx: &Context, template: &str, vars: &NotifyVars<'_>) -> String {
    let prefix = command::get_prefixes(ctx).into_iter().next().unwrap_or_default();
    template
        .replace("{no}", &vars.no.to_string())
        .replace("{kind}", vars.kind.name())
        .replace("{group}", &place_label(vars.group_name, vars.group))
        .replace("{name}", vars.name)
        .replace("{user}", vars.user)
        .replace("{basis}", vars.basis)
        .replace("{count}", &vars.count.to_string())
        .replace("{action}", vars.action)
        .replace("{prefix}", &prefix)
}

/// 一次命中的来龙去脉，凑在一起交给 notify，免得参数排成一长串。
struct Hit<'a> {
    no: u64,
    verdict: &'a Verdict,
    basis: &'a str,
    image: &'a [u8],
    count: usize,
    /// 群管理做了什么（或为什么没做），一句话。
    action: String,
}

/// 把原图与来龙去脉发到配置的去处。撤回、禁言在这之前由 [`manage`] 做完，这里只说结果。
async fn notify(ctx: &Context, writer: &LockedWriter, config: &Config, target: &Target, hit: &Hit<'_>) {
    let places = notify_places(ctx, config);
    if places.is_empty() {
        warn!(target: LOG_TARGET, "#{} 没有可发提醒的去处，这次只记下", hit.no);
        return;
    }
    let text = notify_text(
        ctx,
        &config.notify_text,
        &NotifyVars {
            no: hit.no,
            kind: hit.verdict.kind,
            group: &target.group,
            group_name: &target.group_name,
            name: &target.name,
            user: &target.user,
            basis: hit.basis,
            count: hit.count,
            action: &hit.action,
        },
    );
    if text.trim().is_empty() && !config.notify_image {
        warn!(target: LOG_TARGET, "#{} 的提醒文案为空且关掉了原图，这次只记下", hit.no);
        return;
    }
    let mut ids = Vec::new();
    let mut delivered = 0usize;
    for (group, user) in places {
        let mut message = Message::new();
        if config.notify_image && !hit.image.is_empty() {
            message = message.image(format!("base64://{}", STANDARD.encode(hit.image)));
        }
        if !text.trim().is_empty() {
            message = message.text(text.clone());
        }
        match send_msg_id(ctx, writer.clone(), group.as_deref(), user.as_deref(), message).await {
            Ok(id) => {
                delivered += 1;
                if let Some(id) = id {
                    ids.push(id);
                }
            }
            Err(error) => {
                let place = group.as_ref().map_or_else(|| format!("私聊 {}", user.as_deref().unwrap_or("")), |g| format!("群 {g}"));
                warn!(target: LOG_TARGET, "#{} 的提醒没发到{place}：{error}", hit.no);
            }
        }
    }
    if delivered > 0 {
        update_caught(hit.no, |c| {
            c.reports = ids;
            c.notified = true;
        })
        .await;
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
        // 管事的人发的：默认不管；就算设了也照看，也只提醒，不对他们动手。
        let staff = crate::access::is_manager(&ctx) || message.sender_role().is_some_and(api::is_staff);
        if config.exempt_staff && staff {
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
            staff,
        };
        let (task_ctx, task_writer) = (ctx.clone(), writer.clone());
        let queued = Queued::enter();
        tokio::spawn(async move {
            let found = scan(&task_ctx, &pictures, &config).await;
            drop(queued);
            if let Some(found) = found
                && let Err(error) = notify_hit(&task_ctx, &task_writer, &config, &target, found).await
            {
                warn!(target: LOG_TARGET, "群 {} 的奶龙提醒失败：{error}", target.group);
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
    /// 发图的是群主、管理员或机器人管理员：只提醒，不动手。
    staff: bool,
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

/// 今天零点（北京时间）的时间戳。
fn day_start() -> i64 {
    crate::clock::beijing_now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|t| t.and_local_timezone(crate::clock::beijing()).single())
        .map_or(0, |t| t.timestamp())
}

/// 同一群同一个人 `window` 秒内又发了同类的图：只把次数加一、时间挪到现在，不再提醒。
/// 交回那一次的编号与累计次数。
async fn bump_recent(self_id: &str, group: &str, user: &str, kind: Kind, window: i64, now: i64) -> Option<(u64, u32)> {
    if window <= 0 {
        return None;
    }
    CAUGHT
        .with(|log| {
            log.items
                .iter_mut()
                .rev()
                .find(|c| {
                    c.self_id == self_id
                        && c.group == group
                        && c.user == user
                        && c.kind == kind
                        && !c.cleared
                        && now - c.at < window
                })
                .map(|c| {
                    c.hits += 1;
                    c.at = now;
                    (c.no, c.hits)
                })
        })
        .await
}

/// 本群今天发现了几张（同一串里的重复也算，去重只省提醒、不省计数）。
async fn today_count(self_id: &str, group: &str, now: i64) -> usize {
    let start = day_start();
    CAUGHT
        .with(|log| {
            log.items
                .iter()
                .filter(|c| c.self_id == self_id && c.group == group && c.at >= start && c.at <= now)
                .map(|c| c.hits as usize)
                .sum()
        })
        .await
}

/// 这个小时已经发出去几条提醒（去重后的那一串算一条）。
async fn notified_last_hour(self_id: &str, now: i64) -> usize {
    CAUGHT
        .with(|log| {
            log.items
                .iter()
                .filter(|c| c.self_id == self_id && c.notified && now - c.at < 3600)
                .count()
        })
        .await
}

/// 近 `hours` 小时这个人在本群第几次（没改判的，连同去重期间的重复）。禁言阶梯按它走。
async fn offense_count(self_id: &str, group: &str, user: &str, hours: u64, now: i64) -> usize {
    let window = i64::try_from(hours * 3600).unwrap_or(86400);
    CAUGHT
        .with(|log| {
            log.items
                .iter()
                .filter(|c| c.self_id == self_id && c.group == group && c.user == user && !c.cleared && now - c.at < window)
                .map(|c| c.hits as usize)
                .sum()
        })
        .await
}

/// 认得有没有把握到能动手：大模型确认的、教过的、表情名写着的算；只凭本地模型的要分数够高。
fn sure_enough(verdict: &Verdict, min_score: f32) -> bool {
    if verdict.taught.is_some() || verdict.by_name {
        return true;
    }
    match verdict.ai {
        Some(opinion) => opinion.kind == verdict.kind && opinion.kind != Kind::Other,
        None => verdict.scores.of(verdict.kind) >= min_score,
    }
}

/// 群管理做了什么。
#[derive(Default)]
struct Handled {
    recalled: bool,
    muted: u64,
    /// 群里那句说明。
    notice: Option<String>,
    /// 写进提醒的一句话。
    text: String,
}

/// 群管理：开着、账号是本群管理员、认得有把握，三样都满足才撤回、禁言，否则交回为什么没动手。
/// `offense` 是近 `window_hours` 小时内第几次（含这次）。
async fn manage(ctx: &Context, writer: &LockedWriter, config: &Config, target: &Target, verdict: &Verdict, offense: usize) -> Handled {
    const MANUAL: &str = "没有自动撤回或禁言，请在群里确认后手动处理。";
    let manage = &config.manage;
    let group = &target.group;
    if !manage.enabled
        || (!manage.groups.is_empty() && !manage.groups.iter().any(|g| g.trim() == group))
        || (!manage.recall && manage.mute_seconds.iter().all(|s| *s == 0))
    {
        return Handled { text: MANUAL.into(), ..Handled::default() };
    }
    let skip = |why: &str| Handled { text: format!("{why}，没有动手，请确认后手动处理。"), ..Handled::default() };
    if target.staff {
        return skip("发图的是管理员");
    }
    if !sure_enough(verdict, manage.min_score) {
        return skip("只有本地模型认作这类、把握不够");
    }
    if !api::is_staff(&api::own_role(ctx, writer.clone(), group).await) {
        return skip("账号不是本群管理员");
    }
    let mut handled = Handled::default();
    let mut failed = Vec::new();
    if manage.recall {
        match api::delete_msg_in(ctx, writer.clone(), group, &target.message_id).await {
            Ok(()) => handled.recalled = true,
            Err(error) => {
                warn!(target: LOG_TARGET, "群 {group} 撤回 {} 的图失败：{error}", target.user);
                failed.push("撤回");
            }
        }
    }
    let duration = manage.mute_seconds[offense.max(1).min(manage.mute_seconds.len()) - 1];
    if duration > 0 {
        let result = writer
            .call::<_, serde_json::Value>(
                ctx,
                "guild.member.mute",
                serde_json::json!({"guild_id": group, "user_id": target.user, "duration": duration * 1000}),
            )
            .await;
        match result {
            Ok(_) => handled.muted = duration,
            Err(error) => {
                warn!(target: LOG_TARGET, "群 {group} 禁言 {} 失败：{error}", target.user);
                failed.push("禁言");
            }
        }
    }
    let done = match (handled.recalled, handled.muted) {
        (true, 0) => "已撤回".to_string(),
        (true, s) => format!("已撤回并禁言 {}", crate::clock::duration_text(s)),
        (false, 0) => String::new(),
        (false, s) => format!("已禁言 {}", crate::clock::duration_text(s)),
    };
    let mut parts = Vec::new();
    if !done.is_empty() {
        parts.push(format!("{done}（近 {} 小时第 {offense} 次）", manage.window_hours));
    }
    if !failed.is_empty() {
        parts.push(format!("{}失败，请手动处理", failed.join("、")));
    }
    handled.text = if parts.is_empty() { MANUAL.into() } else { format!("{}。", parts.join("；")) };
    if !done.is_empty() && !manage.notice.trim().is_empty() {
        handled.notice = say_notice(ctx, writer, &manage.notice, target, verdict.kind, &done).await;
    }
    handled
}

/// 动手后在群里说一句为什么，两分钟后自己撤掉：当事人知道图去哪了，聊天记录里也不留着。
async fn say_notice(ctx: &Context, writer: &LockedWriter, template: &str, target: &Target, kind: Kind, done: &str) -> Option<String> {
    let mut message = Message::new();
    let text = template.replace("{kind}", kind.name()).replace("{action}", done);
    let mut rest = text.as_str();
    while let Some(at) = rest.find("{at}") {
        if at > 0 {
            message = message.text(rest[..at].to_string());
        }
        message = message.at(&target.user);
        rest = &rest[at + 4..];
    }
    if !rest.is_empty() {
        message = message.text(rest.to_string());
    }
    let id = match send_msg_id(ctx, writer.clone(), Some(&target.group), None, message).await {
        Ok(id) => id?,
        Err(error) => {
            warn!(target: LOG_TARGET, "群 {} 的说明没发出去：{error}", target.group);
            return None;
        }
    };
    let (ctx, writer, group, tip) = (ctx.clone(), writer.clone(), target.group.clone(), id.clone());
    tokio::spawn(async move {
        tokio::time::sleep(NOTICE_LIFETIME).await;
        let _ = api::delete_msg_in(&ctx, writer, &group, &tip).await;
    });
    Some(id)
}

/// 群里那句说明挂多久。
const NOTICE_LIFETIME: Duration = Duration::from_secs(120);

/// 认出来之后：去重、计数、群管理（开着才有）、存档、提醒。
async fn notify_hit(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    target: &Target,
    (verdict, image): (Verdict, Vec<u8>),
) -> Result<(), PluginError> {
    let Target { group, user, name, message_id, .. } = target;
    let kind = verdict.kind;
    let why = basis(&verdict);
    let self_id = ctx.bot.self_id();
    info!(target: LOG_TARGET, "群 {group} 的 {name}（{user}）发了{}图（{why}）", kind.name());
    let now = chrono::Utc::now().timestamp();
    let window = i64::try_from(config.repeat_minutes * 60).unwrap_or(0);
    if let Some((no, hits)) = bump_recent(&self_id, group, user, kind, window, now).await {
        // 不再提醒，但开了群管理的照样撤、禁言按累计次数加码。
        let offense = offense_count(&self_id, group, user, config.manage.window_hours, now).await;
        let handled = manage(ctx, writer, config, target, &verdict, offense).await;
        if handled.recalled || handled.muted > 0 {
            update_caught(no, |c| {
                c.recalled |= handled.recalled;
                if handled.muted > 0 {
                    c.muted_until = now + i64::try_from(handled.muted).unwrap_or(0);
                }
            })
            .await;
        }
        info!(
            target: LOG_TARGET,
            "#{no} 已在 {} 分钟内提醒过，这次只记次数（第 {hits} 次）{}",
            config.repeat_minutes,
            if handled.recalled || handled.muted > 0 { format!("：{}", handled.text) } else { String::new() }
        );
        return Ok(());
    }
    let count = today_count(&self_id, group, now).await + 1;
    let offense = offense_count(&self_id, group, user, config.manage.window_hours, now).await + 1;
    let handled = manage(ctx, writer, config, target, &verdict, offense).await;
    let caught = Caught {
        no: 0,
        self_id: self_id.clone(),
        group: group.clone(),
        group_name: target.group_name.clone(),
        user: user.clone(),
        name: name.clone(),
        message_id: message_id.clone(),
        reports: Vec::new(),
        kind,
        basis: why.clone(),
        notified: false,
        hits: 1,
        at: now,
        cleared: false,
        recalled: handled.recalled,
        muted_until: if handled.muted > 0 { now + i64::try_from(handled.muted).unwrap_or(0) } else { 0 },
        notice: handled.notice.clone(),
    };
    let no = save_caught(caught, &image, config.keep_days).await;
    let over = config.max_per_hour > 0
        && notified_last_hour(&self_id, now).await >= usize::try_from(config.max_per_hour).unwrap_or(0);
    if over {
        warn!(
            target: LOG_TARGET,
            "#{no} 这个小时的提醒已达上限 {}，只记录不提醒",
            config.max_per_hour
        );
        return Ok(());
    }
    let hit = Hit { no, verdict: &verdict, basis: &why, image: &image, count, action: handled.text };
    notify(ctx, writer, config, target, &hit).await;
    Ok(())
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
     管理员引用图或私聊里的提醒发「奶龙 是」「奶龙 不是」纠正，「奶龙 忘了」撤销\n\
     没看群也不怕：「奶龙 记录」翻近几天发现的，「奶龙 不是 编号」改判（禁过言的一并解除）"
        .into()
}

/// 状态里的群管理那一行：这个群到底会不会动手、不会的话差在哪。
async fn manage_text(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str) -> String {
    let manage = &config.manage;
    if !manage.enabled {
        return "群管理：关（只提醒，不撤回、不禁言）".into();
    }
    if !manage.groups.is_empty() && !manage.groups.iter().any(|g| g.trim() == group) {
        return "群管理：本群不在 manage.groups 里，只提醒".into();
    }
    if !api::is_staff(&api::own_role(ctx, writer.clone(), group).await) {
        return "群管理：开，但账号不是本群管理员，只提醒".into();
    }
    let last = manage.mute_seconds.len();
    let mutes: Vec<String> = manage
        .mute_seconds
        .iter()
        .enumerate()
        .filter(|(_, s)| **s > 0)
        .map(|(i, s)| {
            // 最后一档管之后的每一次：「第 3 次起 1 小时」。
            let from = if i + 1 == last { "起" } else { "" };
            format!("第 {} 次{from} {}", i + 1, crate::clock::duration_text(*s))
        })
        .collect();
    let mut acts = Vec::new();
    if manage.recall {
        acts.push("撤回".to_string());
    }
    if !mutes.is_empty() {
        acts.push(format!("禁言{}（{} 小时内计次）", mutes.join("、"), manage.window_hours));
    }
    format!("群管理：开 · 有把握的{}", acts.join("；"))
}

async fn status(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str) -> String {
    let me = ctx.bot.self_id();
    let (found, people) = CAUGHT
        .with(|log| {
            let start = day_start();
            let today: Vec<&Caught> = log
                .items
                .iter()
                .filter(|c| c.self_id == me && c.group == group && c.at >= start)
                .collect();
            let mut people: Vec<&str> = today.iter().map(|c| c.user.as_str()).collect();
            people.sort_unstable();
            people.dedup();
            (today.iter().map(|c| c.hits as usize).sum::<usize>(), people.len())
        })
        .await;
    let taught = SAMPLES.with(|s| s.samples.len()).await;
    let scope = if !config.channel.white.is_empty() {
        format!("{} 个群（白名单）", config.channel.white.len())
    } else if !config.channel.black.is_empty() {
        format!("全部群，排除 {} 个", config.channel.black.len())
    } else {
        "全部群".to_string()
    };
    let repeat = if config.repeat_minutes == 0 {
        "同一人每次都提醒".to_string()
    } else {
        format!("同一人 {} 分钟内只提醒一次", config.repeat_minutes)
    };
    let cap = if config.max_per_hour == 0 {
        "不设上限".to_string()
    } else {
        format!("每小时最多 {} 条", config.max_per_hour)
    };
    let mut lines = vec![
        format!("🐉 奶龙提醒 · 认{}", config.kinds_text()),
        format!("范围：{scope}"),
        format!("提醒发到：{}", places_text(ctx, config)),
        manage_text(ctx, writer, config, group).await,
        format!("{repeat} · {cap}"),
        if found == 0 {
            format!("今天还没发现 · 教过 {taught} 张 · 阈值 {:.0}%", config.threshold * 100.0)
        } else {
            format!("今天发现 {found} 张、{people} 人 · 教过 {taught} 张 · 阈值 {:.0}%", config.threshold * 100.0)
        },
        format!("模型：{}", model::status()),
    ];
    let mode = Mode::of(ctx, config).await;
    lines.push(match mode {
        Mode::Local if config.judge != "local" => format!("认图：本地模型（{} 不可用：oai 没开或没配这个供应商）", config.ai_model),
        Mode::Local => "认图：本地模型".to_string(),
        _ => format!("认图：{} · 今天复核 {}/{} 张", mode.text(config), ai::used_today().await, config.ai_daily_limit),
    });
    lines.push(String::new());
    lines.push(usage());
    lines.join("\n")
}

/// 这次最后怎么了：只提醒、只记录，还是已改判。
fn outcome(caught: &Caught) -> String {
    let mut parts = Vec::new();
    if caught.cleared {
        parts.push("已改判不是".to_string());
    } else if caught.recalled || caught.muted_until > 0 {
        let mut acts = Vec::new();
        if caught.recalled {
            acts.push("撤回");
        }
        if caught.muted_until > 0 {
            acts.push("禁言");
        }
        parts.push(format!("已{}", acts.join("、")));
    } else if caught.notified {
        parts.push("已提醒".to_string());
    } else {
        parts.push("只记录，未提醒".to_string());
    }
    if caught.hits > 1 {
        parts.push(format!("后又发 {} 次", caught.hits - 1));
    }
    parts.join("，")
}

/// 近几天发现的：群里只列本群，私聊列全部。最新的在前，最多十条。
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
        return "📭 近几天没发现过".into();
    }
    let total: usize = items.iter().map(|c| c.hits as usize).sum();
    let mut lines = vec![format!("🐉 近几天发现 {total} 次{}", if items.len() > 10 { "，列最近 10 次" } else { "" })];
    for c in items.iter().rev().take(10) {
        let place_text = if place.group.is_none() { format!(" · {}", group_label(c)) } else { String::new() };
        lines.push(format!(
            "#{} {}{place_text}\n   {}（{}）· {} · {} · {}",
            c.no,
            when(c.at),
            c.name,
            c.user,
            c.kind.name(),
            c.basis,
            outcome(c)
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
        outcome(&caught)
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
                    let tail = if hits { "会提醒" } else { "但本群没提醒这类" };
                    format!("表情名写着{}，{tail}", verdict.kind.name())
                }
                (None, true) => format!("会提醒：{}", verdict.kind.name()),
                (None, false) if verdict.kind != Kind::Other => format!("像{}，但本群没提醒这类", verdict.kind.name()),
                (None, false) if verdict.ai.is_some() => "不提醒（大模型看过，不是）".to_string(),
                (None, false) => format!("不提醒（阈值 {:.0}%）", config.threshold * 100.0),
            };
            format!("{head}{scores}\n{result}")
        })
        .collect();
    say(lines.join("\n")).await
}

enum Correction {
    /// 是：提醒这类；可指明奶龙或奶蛙，不指明按模型更像的那类。
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
            "❌ 先引用那张图或私聊里的提醒再发这句；图过期了找不到，发{}看编号",
            spelled(ctx, "记录")
        ))
        .await;
    }
    let by = message.user_id();
    // 私聊进得来的只有号主与 ctl 管理员。
    let allowed = match &ask.place.group {
        None => true,
        Some(group) => crate::access::is_group_staff(ctx, writer, group).await,
    };
    if !allowed {
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
            let note = if config.targets().contains(&kind) { "" } else { "（本群目前没提醒这类，见 targets）" };
            format!("✅ 记住了：这类图算{}{note}\n已教 {total} 张；记错了发「奶龙 忘了」", kind.name())
        }
        Correction::IsNot => {
            let total = teach(Kind::Other, &vectors, by, group).await;
            let mut text = format!("✅ 记住了：这类图不提醒\n已教 {total} 张；记错了发「奶龙 忘了」");
            if let Some(caught) = caught.filter(|c| !c.cleared) {
                update_caught(caught.no, |c| c.cleared = true).await;
                text.push_str(&format!("\n#{} 已标记不是{}", caught.no, undo(ctx, writer, &caught).await));
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

/// 改判「不是」时把群管理做过的收回来：解除还没到期的禁言、撤掉群里那句说明。
/// 撤回的图收不回来，说一声原图还存着。交回接在「已标记不是」后面的话。
async fn undo(ctx: &Context, writer: &LockedWriter, caught: &Caught) -> String {
    let mut parts = Vec::new();
    if caught.muted_until > chrono::Utc::now().timestamp() {
        let result = writer
            .call::<_, serde_json::Value>(
                ctx,
                "guild.member.mute",
                serde_json::json!({"guild_id": caught.group, "user_id": caught.user, "duration": 0}),
            )
            .await;
        match result {
            Ok(_) => parts.push("禁言已解除".to_string()),
            Err(error) => {
                warn!(target: LOG_TARGET, "#{} 解除禁言失败：{error}", caught.no);
                parts.push("解除禁言失败，请手动解除".to_string());
            }
        }
    }
    if let Some(tip) = &caught.notice {
        let _ = api::delete_msg_in(ctx, writer.clone(), &caught.group, tip).await;
    }
    if caught.recalled {
        parts.push(format!("图已撤回、没法恢复，私聊发{}能取回原图", spelled(ctx, &format!("看 {}", caught.no))));
    }
    if parts.is_empty() {
        "；群里那张图如果还在，请自行处理".into()
    } else {
        format!("；{}", parts.join("；"))
    }
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
