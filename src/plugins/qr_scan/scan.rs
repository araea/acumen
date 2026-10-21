//! 在一张图里找出全部二维码。
//!
//! 底层是 `rxing` 里 zxing-cpp 的移植（`qrcode::cpp_port::QrReader`）：一次调用找出图里
//! **所有**的 QR 与 Micro QR，定位图案配对时按大小与几何关系排序，码挨得很近也不会配错。
//!
//! 选它而不是更轻的 `rqrr`，是被一次实测教训逼的：`rqrr` 的耗时随候选定位图案的数量
//! 陡增且没有上限——一张 1200 万像素、满是方框的照片整图扫要 154 秒，一张 1200×900 的
//! 纯噪声图要 295 秒。群里任何人都能发图触发识别，这等于一条命令就能把工作池占上几分钟。
//! zxing 的找法是线性的：同样的两张图是 0.14 秒和 0.02 秒，Micro QR、rMQR 也一并认得。
//!
//! 一遍扫描认不出来时再换办法，且按「便宜、常见」到「少见」排：
//! - 多个尺度：缩小一半、四分之一（手机拍的大图上，码里的噪点会拆碎定位图案，缩小后反而清晰）；
//!   小图则放大（聊天软件压缩后的码常常一个模块不到两个像素）。
//! - 兜底：正常极性一个码都没找到，才换全局直方图二值化、反色（深色模式的白码黑底）、
//!   拉伸对比度（灰蒙蒙、反光的图）。这几种更慢，也更容易是空忙一场，所以不抢在前面。
//!
//! 整个过程带截止时间：到点就用已经找到的结果收工。

use image::{DynamicImage, GrayImage, imageops::FilterType};
use rxing::multi::MultipleBarcodeReader;
use rxing::qrcode::cpp_port::QrReader;
use rxing::{
    BarcodeFormat, Binarizer, BinaryBitmap, DecodeHints, Luma8LuminanceSource, RXingResult,
    common::{GlobalHistogramBinarizer, HybridBinarizer},
};
use std::collections::HashSet;
use std::time::Instant;

/// 解出来的一个二维码。
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    /// 内容。
    pub text: String,
    /// 四个角在**原图**里的像素坐标：左上、右上、右下、左下（以码自己的朝向为准）。
    pub corners: [(f32, f32); 4],
}

impl Decoded {
    fn center(&self) -> (f32, f32) {
        let sum = self.corners.iter().fold((0.0, 0.0), |acc, corner| {
            (acc.0 + corner.0, acc.1 + corner.1)
        });
        (sum.0 / 4.0, sum.1 / 4.0)
    }

    /// 码的大致边长：两条对角线的平均长度除以 √2。
    pub fn size(&self) -> f32 {
        let [a, b, c, d] = self.corners;
        let diagonal =
            |p: (f32, f32), q: (f32, f32)| ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2)).sqrt();
        (diagonal(a, c) + diagonal(b, d)) / 2.0 / std::f32::consts::SQRT_2
    }

}

/// 一次扫描的上限。
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// 最多找几个；够数就停。
    pub max_codes: usize,
    /// 到点收工。
    pub deadline: Instant,
}

/// 不到这个边长的图没法扫。
const MIN_SIDE: u32 = 21;
/// 扫描前把过大的图缩到这个长边以内（再大只是白费内存和时间）。
const MAX_SIDE: u32 = 4096;

/// 灰度图。带透明通道的图先垫上白底：透明像素的 RGB 通常是黑的，直接转灰度
/// 会把透明底的二维码整张染黑。
pub fn to_gray(image: &DynamicImage) -> GrayImage {
    if !image.color().has_alpha() {
        return image.to_luma8();
    }
    let rgba = image.to_rgba8();
    GrayImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let luma = (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000;
        let alpha = u32::from(a);
        image::Luma([((luma * alpha + 255 * (255 - alpha)) / 255) as u8])
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Treatment {
    Plain,
    /// 反色：白码黑底。
    Invert,
    /// 拉伸对比度：灰蒙蒙、反光的图。
    Stretch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Binarization {
    /// zxing 默认：按块自适应，抗光照不均。
    Hybrid,
    /// 全局直方图：光照均匀、对比低的图上更稳。
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Pass {
    scale: f32,
    treatment: Treatment,
    binarization: Binarization,
}

/// 排出要扫的几遍：先是正常极性的各个尺度，找不到东西才轮到兜底那几遍。
fn plan(width: u32, height: u32) -> (Vec<Pass>, Vec<Pass>) {
    // 过大的图先缩到 MAX_SIDE：这是「原尺寸」。
    let native = (MAX_SIDE as f32 / width.max(height) as f32).min(1.0);
    let long = width.max(height) as f32 * native;
    let pass = |scale, treatment, binarization| Pass {
        scale,
        treatment,
        binarization,
    };
    use Binarization::{Global, Hybrid};
    use Treatment::{Invert, Plain, Stretch};

    let mut first = vec![pass(native, Plain, Hybrid)];
    let mut fallback = vec![
        pass(native, Plain, Global),
        pass(native, Invert, Hybrid),
        pass(native, Stretch, Hybrid),
    ];
    if long >= 1000.0 {
        first.push(pass(native * 0.5, Plain, Hybrid));
        fallback.push(pass(native * 0.5, Invert, Hybrid));
    }
    if long >= 2000.0 {
        first.push(pass(native * 0.25, Plain, Hybrid));
    }
    // 小图放大：聊天软件压缩后的码常常只有几十像素，一个模块不到两个像素。
    if long <= 700.0 {
        first.push(pass(native * 2.0, Plain, Hybrid));
        fallback.push(pass(native * 2.0, Invert, Hybrid));
        fallback.push(pass(native * 2.0, Stretch, Hybrid));
    }
    if long <= 300.0 {
        first.push(pass(native * 3.0, Plain, Hybrid));
    }
    (first, fallback)
}

/// 对一张灰度图做多遍扫描，返回按阅读顺序（自上而下、同一行自左而右）排好的全部二维码。
pub fn scan(gray: &GrayImage, limits: Limits) -> Vec<Decoded> {
    let (width, height) = gray.dimensions();
    if width.min(height) < MIN_SIDE || limits.max_codes == 0 {
        return Vec::new();
    }
    let mut found: Vec<Decoded> = Vec::new();
    let (first, fallback) = plan(width, height);
    for pass in first {
        run_pass(gray, pass, &mut found, limits);
    }
    if found.is_empty() {
        for pass in fallback {
            run_pass(gray, pass, &mut found, limits);
            if !found.is_empty() {
                break;
            }
        }
    }
    reading_order(&mut found);
    found
}

/// 一遍扫描：缩放、做好处理，交给 zxing，把新找到的码并进 `found`。
fn run_pass(base: &GrayImage, pass: Pass, found: &mut Vec<Decoded>, limits: Limits) {
    if Instant::now() >= limits.deadline || found.len() >= limits.max_codes {
        return;
    }
    let mut buffer = rescale(base, pass.scale);
    match pass.treatment {
        Treatment::Plain => {}
        Treatment::Invert => buffer.iter_mut().for_each(|pixel| *pixel = 255 - *pixel),
        Treatment::Stretch => stretch_contrast(&mut buffer),
    }
    let hits = match pass.binarization {
        Binarization::Hybrid => decode_all(&buffer, HybridBinarizer::new),
        Binarization::Global => decode_all(&buffer, GlobalHistogramBinarizer::new),
    };
    for result in hits {
        let Some(code) = decoded_from(&result, pass.scale) else {
            continue;
        };
        if found.iter().any(|known| same_code(known, &code)) {
            continue;
        }
        found.push(code);
        if found.len() >= limits.max_codes {
            return;
        }
    }
}

/// 交给 zxing 扫一遍，返回这一遍找到的全部码。
///
/// 底层库在极端输入上出过恐慌，这里兜住：一遍出事只丢这一遍。
fn decode_all<B, F>(buffer: &GrayImage, binarizer: F) -> Vec<RXingResult>
where
    B: Binarizer,
    F: FnOnce(Luma8LuminanceSource) -> B,
{
    let Ok(source) =
        Luma8LuminanceSource::new(buffer.as_raw().clone(), buffer.width(), buffer.height())
    else {
        return Vec::new();
    };
    let hints = DecodeHints {
        TryHarder: Some(true),
        PossibleFormats: Some(HashSet::from([
            BarcodeFormat::QR_CODE,
            BarcodeFormat::MICRO_QR_CODE,
            BarcodeFormat::RECTANGULAR_MICRO_QR_CODE,
        ])),
        ..Default::default()
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut bitmap = BinaryBitmap::new(binarizer(source));
        QrReader
            .decode_multiple_with_hints(&mut bitmap, &hints)
            .unwrap_or_default()
    }));
    outcome.unwrap_or_default()
}

/// zxing 的结果换成 [`Decoded`]：角点除回缩放比例，次序从它的「左上、右上、左下、右下」
/// 改成绕一圈的「左上、右上、右下、左下」。
fn decoded_from(result: &RXingResult, scale: f32) -> Option<Decoded> {
    let points = result.getPoints();
    if points.len() < 4 {
        return None;
    }
    let at = |index: usize| (points[index].x / scale, points[index].y / scale);
    Some(Decoded {
        text: payload_text(result),
        corners: [at(0), at(1), at(3), at(2)],
    })
}

/// 内容文字。zxing 会按声明（ECI）或自己的猜测解码，大多数时候是对的；唯独国内早年的
/// 生成器把 GBK 字节直接塞进码里、不写编码声明，zxing 兜底成 Latin-1，一句「你好」
/// 变成「Ä㺽」。这种乱码的特征很明显：载荷不是合法 UTF-8，而 zxing 给出的文字里
/// 一个 Latin-1 以外的字符都没有；同时原始字节恰好能无损地按 GBK 读通，就改用 GBK。
/// 有 ECI 声明的、日文汉字模式的，文字里本来就有 Latin-1 以外的字，不会被动。
fn payload_text(result: &RXingResult) -> String {
    rescue_gbk(result.getText(), result.getRawBytes())
        .unwrap_or_else(|| result.getText().to_string())
}

fn rescue_gbk(text: &str, raw: &[u8]) -> Option<String> {
    if raw.len() < 2 || std::str::from_utf8(raw).is_ok() || text.chars().any(|c| c as u32 > 0xFF) {
        return None;
    }
    encoding_rs::GB18030
        .decode_without_bom_handling_and_without_replacement(raw)
        .map(std::borrow::Cow::into_owned)
}

/// 同一个码在不同尺度下会被各找一次：文字一样、中心离得近就算同一个。
/// 文字一样但位置不同的是两个码（贴纸、重复张贴），都要留。
fn same_code(a: &Decoded, b: &Decoded) -> bool {
    if a.text != b.text {
        return false;
    }
    let (ca, cb) = (a.center(), b.center());
    let distance = ((ca.0 - cb.0).powi(2) + (ca.1 - cb.1).powi(2)).sqrt();
    distance < a.size().max(b.size()) * 0.6
}

fn rescale(base: &GrayImage, scale: f32) -> GrayImage {
    if (scale - 1.0).abs() < 1e-3 {
        return base.clone();
    }
    let width = ((base.width() as f32 * scale).round() as u32).max(1);
    let height = ((base.height() as f32 * scale).round() as u32).max(1);
    let filter = if scale < 1.0 {
        FilterType::Triangle
    } else {
        FilterType::CatmullRom
    };
    image::imageops::resize(base, width, height, filter)
}

/// 把 2% 到 98% 分位的亮度拉满 0—255。动态范围本来就够宽（≥200）或窄到没救（<24）
/// 的图不动。
fn stretch_contrast(buffer: &mut GrayImage) {
    let mut histogram = [0u64; 256];
    for pixel in buffer.iter() {
        histogram[usize::from(*pixel)] += 1;
    }
    let total: u64 = histogram.iter().sum();
    let percentile = |fraction: f64| {
        let target = (total as f64 * fraction) as u64;
        let mut seen = 0;
        for (value, count) in histogram.iter().enumerate() {
            seen += count;
            if seen > target {
                return value as i32;
            }
        }
        255
    };
    let (low, high) = (percentile(0.02), percentile(0.98));
    if high - low < 24 || high - low >= 200 {
        return;
    }
    let span = (high - low) as f32;
    for pixel in buffer.iter_mut() {
        let stretched = ((i32::from(*pixel) - low) as f32 / span * 255.0).clamp(0.0, 255.0);
        *pixel = stretched as u8;
    }
}

/// 阅读顺序：自上而下；中心的纵向距离不到半个码高的算同一行，行内自左而右。
fn reading_order(codes: &mut Vec<Decoded>) {
    codes.sort_by(|a, b| a.center().1.total_cmp(&b.center().1));
    let mut rows: Vec<Vec<Decoded>> = Vec::new();
    for code in codes.drain(..) {
        match rows.last_mut() {
            Some(row) if (code.center().1 - row[0].center().1).abs() < row[0].size() * 0.5 => {
                row.push(code);
            }
            _ => rows.push(vec![code]),
        }
    }
    for row in &mut rows {
        row.sort_by(|a, b| a.center().0.total_cmp(&b.center().0));
    }
    codes.extend(rows.into_iter().flatten());
}
