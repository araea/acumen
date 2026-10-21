//! 认图：MobileCLIP-S0 的图像编码器把一张图压成 512 维向量，再过一个两层的小分类头，
//! 得到「其他 / 奶龙 / 奶蛙」三类的概率。
//!
//! - **编码器**（约 45 MB）不进仓库也不进发布包：第一次用到时从 Hugging Face（或国内镜像）
//!   下到数据目录，按 SHA-256 校验后才用。权重随 Apple 的研究许可证，只在本机下载使用。
//! - **分类头**（[`HEAD`]，约 0.5 MB，编进可执行文件）是本仓库用公开数据集训的，见 `docs/nailong.md`。
//! - **教过的样本**：管理员说「是 / 不是」的图把向量记下来；新图和某个样本足够像时，
//!   以样本为准——模型漏判、误判的那一类图，教一次往后就对了，也认得出新冒出来的变体。
//!
//! 推理用 tract（纯 Rust），发布版不依赖系统里的推理库；手机上一张图约 0.25 秒。

use super::LOG_TARGET;
use crate::storage;
use image::{RgbImage, RgbaImage, imageops};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tract_onnx::prelude::*;

/// 编码器输入边长。
const SIZE: u32 = 256;
/// 向量维数。
pub const DIM: usize = 512;

const MODEL_FILE: &str = "mobileclip_s0_vision.onnx";
const MODEL_SHA256: &str = "17d3c037b1d488c10c50e09f6009ea5a198caef4e0e8f4ea5617b7cb2d067ac0";
/// 钉在固定提交上：上游换了文件，校验也对得上旧值，不会悄悄换模型。
const MODEL_URLS: [&str; 2] = [
    "https://huggingface.co/Xenova/mobileclip_s0/resolve/757d59c9c6870a76a4b0306f05f5061bca15c39f/onnx/vision_model.onnx",
    "https://hf-mirror.com/Xenova/mobileclip_s0/resolve/757d59c9c6870a76a4b0306f05f5061bca15c39f/onnx/vision_model.onnx",
];
/// 下载或加载失败后，隔这么久再试，免得每张图都去撞一次。
const RETRY_AFTER: Duration = Duration::from_mins(10);

/// 分类头：`NLH1`、三个 u32（输入维数、隐藏层宽、类别数），接着依次是第一层权重
/// `[隐藏][输入]`、偏置、第二层权重 `[类别][隐藏]`、偏置，全是小端 f32。类别顺序见 [`Kind`]。
const HEAD: &[u8] = include_bytes!("head.bin");
const CLASSES: usize = 3;

/// 认出来是什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Other,
    Nailong,
    Naiwa,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Other => "其他",
            Kind::Nailong => "奶龙",
            Kind::Naiwa => "奶蛙",
        }
    }
}

/// 一张图（一帧）的判断。
#[derive(Debug, Clone, Copy)]
pub struct Scores {
    pub nailong: f32,
    pub naiwa: f32,
}

impl Scores {
    pub fn of(&self, kind: Kind) -> f32 {
        match kind {
            Kind::Nailong => self.nailong,
            Kind::Naiwa => self.naiwa,
            Kind::Other => 1.0 - self.nailong - self.naiwa,
        }
    }
}

/// 分类头给的概率：线性 → ReLU → 线性 → softmax。
pub fn classify(embedding: &[f32]) -> Scores {
    let head = head();
    let hidden: Vec<f32> = head
        .w1
        .as_chunks::<DIM>()
        .0
        .iter()
        .zip(&head.b1)
        .map(|(row, bias)| (dot(row, embedding) + bias).max(0.0))
        .collect();
    let mut logits = [0f32; CLASSES];
    for ((logit, row), bias) in logits.iter_mut().zip(head.w2.chunks_exact(head.b1.len())).zip(&head.b2) {
        *logit = dot(row, &hidden) + bias;
    }
    let max = logits.iter().copied().fold(f32::MIN, f32::max);
    let exp = logits.map(|logit| (logit - max).exp());
    let total: f32 = exp.iter().sum();
    Scores { nailong: exp[1] / total, naiwa: exp[2] / total }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

struct Head {
    w1: Vec<f32>,
    b1: Vec<f32>,
    w2: Vec<f32>,
    b2: Vec<f32>,
}

fn head() -> &'static Head {
    static HEAD_WEIGHTS: std::sync::OnceLock<Head> = std::sync::OnceLock::new();
    HEAD_WEIGHTS.get_or_init(|| parse_head(HEAD).expect("编进来的分类头格式不对"))
}

fn parse_head(bytes: &[u8]) -> Option<Head> {
    let rest = bytes.strip_prefix(b"NLH1")?;
    let (header, rest) = rest.split_at_checked(12)?;
    let number = |i: usize| u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().ok()?).try_into().ok();
    let (input, hidden, classes): (usize, usize, usize) = (number(0)?, number(1)?, number(2)?);
    if input != DIM || classes != CLASSES {
        return None;
    }
    let floats: Vec<f32> = rest.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
    if floats.len() != hidden * input + hidden + classes * hidden + classes {
        return None;
    }
    let (w1, rest) = floats.split_at(hidden * input);
    let (b1, rest) = rest.split_at(hidden);
    let (w2, b2) = rest.split_at(classes * hidden);
    Some(Head { w1: w1.to_vec(), b1: b1.to_vec(), w2: w2.to_vec(), b2: b2.to_vec() })
}

/// 两个单位向量的余弦相似度。
pub fn similarity(a: &[f32], b: &[f32]) -> f32 {
    dot(a, b)
}

// ==================== 编码器 ====================

type Plan = Arc<TypedRunnableModel>;

pub struct Encoder {
    plan: Plan,
}

impl Encoder {
    /// 从文件加载并优化成可执行计划（阻塞，约一秒）。
    pub fn open(path: &std::path::Path) -> Result<Encoder, String> {
        let size = SIZE as usize;
        let plan = onnx()
            .model_for_path(path)
            .and_then(|model| model.with_input_fact(0, f32::fact([1, 3, size, size]).into()))
            .and_then(InferenceModelExt::into_optimized)
            .and_then(IntoRunnable::into_runnable)
            .map_err(|e| format!("模型加载失败：{e}"))?;
        Ok(Encoder { plan })
    }

    /// 一帧图 → 单位长度的向量。透明部分垫白、等比缩进正方形居中，与训练分类头时同一个做法。
    pub fn embed(&self, frame: &RgbaImage) -> Result<Vec<f32>, String> {
        let input = to_input(frame);
        let output = self.plan.run(tvec!(input.into())).map_err(|e| format!("推理失败：{e}"))?;
        let view = output[0].to_plain_array_view::<f32>().map_err(|e| e.to_string())?;
        let mut vector: Vec<f32> = view.iter().copied().collect();
        if vector.len() != DIM {
            return Err(format!("编码器输出 {} 维，应为 {DIM}", vector.len()));
        }
        let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        vector.iter_mut().for_each(|x| *x /= norm);
        Ok(vector)
    }
}

fn to_input(frame: &RgbaImage) -> Tensor {
    let (width, height) = frame.dimensions();
    let scale = SIZE as f32 / width.max(height) as f32;
    let (w, h) = (
        ((width as f32 * scale).round() as u32).clamp(1, SIZE),
        ((height as f32 * scale).round() as u32).clamp(1, SIZE),
    );
    let resized = imageops::resize(frame, w, h, imageops::FilterType::CatmullRom);
    let mut canvas = RgbImage::from_pixel(SIZE, SIZE, image::Rgb([255, 255, 255]));
    let (left, top) = ((SIZE - w) / 2, (SIZE - h) / 2);
    for (x, y, pixel) in resized.enumerate_pixels() {
        let alpha = u16::from(pixel[3]);
        let blend = |c: u8| ((u16::from(c) * alpha + 255 * (255 - alpha)) / 255) as u8;
        canvas.put_pixel(left + x, top + y, image::Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])]));
    }
    let size = SIZE as usize;
    tract_ndarray::Array4::from_shape_fn((1, 3, size, size), |(_, c, y, x)| {
        f32::from(canvas.get_pixel(x as u32, y as u32)[c]) / 255.0
    })
    .into()
}

// ==================== 下载与加载 ====================

enum Slot {
    Empty,
    Ready(Arc<Encoder>),
    Failed(Instant, String),
}

static SLOT: Mutex<Slot> = Mutex::new(Slot::Empty);
/// 同一时间只有一个人去下载、加载。
static LOADING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 编码器现在的状态，给状态指令看。
pub fn status() -> String {
    match &*SLOT.lock().unwrap_or_else(std::sync::PoisonError::into_inner) {
        Slot::Empty if LOADING.try_lock().is_err() => "正在准备（首次要下载约 45 MB，几分钟）".into(),
        Slot::Empty => "尚未加载（开启或第一张图到来时自动下载，约 45 MB）".into(),
        Slot::Ready(_) => "已就绪".into(),
        Slot::Failed(at, error) => {
            let wait = RETRY_AFTER.saturating_sub(at.elapsed()).as_secs().div_ceil(60);
            format!("加载失败：{error}（{wait} 分钟后自动重试）")
        }
    }
}

/// 拿编码器：没有就下载、加载；最近刚失败过就直接报错，等冷却。
pub async fn encoder() -> Result<Arc<Encoder>, String> {
    if let Some(ready) = cached()? {
        return Ok(ready);
    }
    let _guard = LOADING.lock().await;
    if let Some(ready) = cached()? {
        return Ok(ready);
    }
    let result = load().await;
    let mut slot = SLOT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match result {
        Ok(encoder) => {
            *slot = Slot::Ready(encoder.clone());
            Ok(encoder)
        }
        Err(error) => {
            warn!(target: LOG_TARGET, "识图模型没能就绪：{error}");
            *slot = Slot::Failed(Instant::now(), error.clone());
            Err(error)
        }
    }
}

fn cached() -> Result<Option<Arc<Encoder>>, String> {
    match &*SLOT.lock().unwrap_or_else(std::sync::PoisonError::into_inner) {
        Slot::Ready(encoder) => Ok(Some(encoder.clone())),
        Slot::Failed(at, error) if at.elapsed() < RETRY_AFTER => Err(error.clone()),
        _ => Ok(None),
    }
}

fn model_path() -> Result<PathBuf, String> {
    storage::data_path(<super::Config as crate::plugins::PluginConfig>::NAME)
        .map(|dir| dir.join(MODEL_FILE))
        .map_err(|e| format!("数据目录不可用：{e}"))
}

async fn load() -> Result<Arc<Encoder>, String> {
    let path = model_path()?;
    let present = match tokio::fs::read(&path).await {
        Ok(bytes) => sha256_hex(&bytes) == MODEL_SHA256,
        Err(_) => false,
    };
    if !present {
        download(&path).await?;
    }
    let started = Instant::now();
    let encoder = tokio::task::spawn_blocking(move || Encoder::open(&path))
        .await
        .map_err(|e| format!("加载线程异常：{e}"))??;
    info!(target: LOG_TARGET, "识图模型已加载（{:.1} 秒）", started.elapsed().as_secs_f32());
    Ok(Arc::new(encoder))
}

/// 模型文件比这大就不是它（正常 45 MB）。
const MAX_MODEL_BYTES: usize = 64 * 1024 * 1024;

async fn download(path: &std::path::Path) -> Result<(), String> {
    // 共享客户端整体只等两分钟，慢一点的网 45 MB 下不完：这里单独给十分钟。
    let client = crate::http::builder()
        .timeout(Duration::from_mins(10))
        .build()
        .map_err(|e| format!("下载客户端建不起来：{e}"))?;
    let mut errors = Vec::new();
    for url in MODEL_URLS {
        let host = url.split('/').nth(2).unwrap_or(url);
        info!(target: LOG_TARGET, "下载识图模型（约 45 MB）：{host}");
        let started = Instant::now();
        let bytes = match fetch(&client, url).await {
            Ok(bytes) => bytes,
            Err(error) => {
                errors.push(format!("{host}：{error}"));
                continue;
            }
        };
        info!(target: LOG_TARGET, "识图模型下载完成（{:.0} 秒）", started.elapsed().as_secs_f32());
        if sha256_hex(&bytes) != MODEL_SHA256 {
            errors.push(format!("{host}：文件校验不符"));
            continue;
        }
        return storage::write_atomic_async(path.to_path_buf(), bytes)
            .await
            .map_err(|e| format!("模型写入失败：{e}"));
    }
    Err(format!("模型下载失败（{}）", errors.join("；")))
}

async fn fetch(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::with_capacity(48 * 1024 * 1024);
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if bytes.len() + chunk.len() > MAX_MODEL_BYTES {
            return Err("文件大得不对".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_parses() {
        let head = head();
        assert_eq!(head.w1.len(), head.b1.len() * DIM);
        assert_eq!(head.w2.len(), head.b1.len() * CLASSES);
        assert!(parse_head(b"NLH1").is_none());
    }

    #[test]
    fn classify_returns_probabilities() {
        let mut vector = vec![0f32; DIM];
        vector[0] = 1.0;
        let scores = classify(&vector);
        assert!((0.0..=1.0).contains(&scores.nailong));
        assert!((0.0..=1.0).contains(&scores.naiwa));
        assert!(scores.nailong + scores.naiwa <= 1.0 + 1e-5);
    }

    #[test]
    fn input_is_letterboxed_on_white() {
        let frame = RgbaImage::from_pixel(100, 50, image::Rgba([0, 0, 0, 255]));
        let tensor = to_input(&frame);
        let view = tensor.to_plain_array_view::<f32>().unwrap();
        assert_eq!(view.shape(), &[1, 3, 256, 256]);
        assert!((view[[0, 0, 0, 0]] - 1.0).abs() < 1e-6, "上方留白");
        assert!(view[[0, 0, 128, 128]] < 1e-6, "中间是原图");
    }
}
