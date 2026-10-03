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

    /// 外接矩形 `(左, 上, 右, 下)`。
    pub fn bounds(&self) -> (f32, f32, f32, f32) {
        self.corners
            .iter()
            .fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |acc, &(x, y)| {
                (acc.0.min(x), acc.1.min(y), acc.2.max(x), acc.3.max(y))
            })
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
        .map(|gbk| gbk.into_owned())
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
                row.push(code)
            }
            _ => rows.push(vec![code]),
        }
    }
    for row in &mut rows {
        row.sort_by(|a, b| a.center().0.total_cmp(&b.center().0));
    }
    codes.extend(rows.into_iter().flatten());
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! 测试用的二维码画面：用 `qrcode` 生成，再拼进各种场景。
    use image::{GrayImage, Luma, Rgba, RgbaImage};
    use qrcode::{Color, EcLevel, QrCode};

    /// 一个码：`module` 像素一格，四周留 `quiet` 格静区。
    pub fn code(text: &str, module: u32, quiet: u32, level: EcLevel) -> GrayImage {
        let code = QrCode::with_error_correction_level(text, level).unwrap();
        let n = code.width() as u32;
        let colors = code.to_colors();
        let side = (n + quiet * 2) * module;
        GrayImage::from_fn(side, side, |x, y| {
            let (cx, cy) = (x / module, y / module);
            let dark = cx >= quiet
                && cy >= quiet
                && cx < quiet + n
                && cy < quiet + n
                && colors[((cy - quiet) * n + (cx - quiet)) as usize] == Color::Dark;
            Luma([if dark { 0 } else { 255 }])
        })
    }

    /// 内容是任意字节的码（给 GBK 之类的非 UTF-8 载荷用）。
    pub fn code_of_bytes(data: &[u8], module: u32, quiet: u32) -> GrayImage {
        render(
            &QrCode::with_error_correction_level(data, EcLevel::M).unwrap(),
            module,
            quiet,
        )
    }

    /// Micro QR（M3，内容 `MICRO12`）。`qrcode` 库的 Micro 输出 zxing 读不出来，所以这张图
    /// 是另一个独立的编码器（Python 的 segno）生成的 PNG：12 像素一格、两格静区。
    pub fn micro_code() -> GrayImage {
        const PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAOQAAADkCAIAAAAHNR/aAAADR0lEQVR4nO3dUW4iMRBAwZ1V7n9l9gbIUqzeflB1ADKEp/5pY57X6/UHCv7+7weAU2IlQ6xkiJUMsZIhVjLESoZYyRArGWIlQ6xkiJUMsZIhVjLESoZYyRArGWIlQ6xkiJUMsZIhVjLESsbPrRd6nufWS+Wc3L1w8v/55tc5YbKSIVYyxEqGWMkQKxliJUOsZIiVDLGSIVYyxErGtbMBJ4q/X3DrzMO2PfuJbec9TFYyxEqGWMkQKxliJUOsZIiVDLGSIVYyxEqGWMkYPRtwYnIfXdyzb/v/TD6PyUqGWMkQKxliJUOsZIiVDLGSIVYyxEqGWMkQKxnrzgZ8s8ld/La9/wmTlQyxkiFWMsRKhljJECsZYiVDrGSIlQyxkiFWMpwN+EDF3244YbKSIVYyxEqGWMkQKxliJUOsZIiVDLGSIVYyxErGurMBn7rXPrHte/rb7hYwWckQKxliJUOsZIiVDLGSIVYyxEqGWMkQKxliJWP0bMC23fekyT37yets2/ufMFnJECsZYiVDrGSIlQyxkiFWMsRKhljJECsZYiXj+ebv6U+a3LN/6mdqspIhVjLESoZYyRArGWIlQ6xkiJUMsZIhVjLESsa1ewNufVd90q19/a33te1ugROTn6nJSoZYyRArGWIlQ6xkiJUMsZIhVjLESoZYyRArGevuDSieMThRfF/bzg+YrGSIlQyxkiFWMsRKhljJECsZYiVDrGSIlQyxkuHegAtuva/J3x04se3zMlnJECsZYiVDrGSIlQyxkiFWMsRKhljJECsZYiVj3b0BJ26dQ7CLf2/beQ+TlQyxkiFWMsRKhljJECsZYiVDrGSIlQyxkiFWMkbvDThxsmue3EdP/i1nFd4zWckQKxliJUOsZIiVDLGSIVYyxEqGWMkQKxliJWP03oDJ8wOTe/Zbz7NtF7/tmU1WMsRKhljJECsZYiVDrGSIlQyxkiFWMsRKhljJSN4bsG3vf0vx3oDJ8wMmKxliJUOsZIiVDLGSIVYyxEqGWMkQKxliJUOsZIzeGwC/YbKSIVYyxEqGWMkQKxliJUOsZIiVDLGSIVYyxEqGWMkQKxliJUOsZIiVDLGSIVYyxEqGWMkQKxliJUOsZPwDwluUyi9P1PMAAAAASUVORK5CYII=";
        let png =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, PNG_BASE64).unwrap();
        image::load_from_memory(&png).unwrap().to_luma8()
    }

    fn render(code: &QrCode, module: u32, quiet: u32) -> GrayImage {
        let n = code.width() as u32;
        let colors = code.to_colors();
        let side = (n + quiet * 2) * module;
        GrayImage::from_fn(side, side, |x, y| {
            let (cx, cy) = (x / module, y / module);
            let dark = cx >= quiet
                && cy >= quiet
                && cx < quiet + n
                && cy < quiet + n
                && colors[((cy - quiet) * n + (cx - quiet)) as usize] == Color::Dark;
            Luma([if dark { 0 } else { 255 }])
        })
    }

    pub fn canvas(width: u32, height: u32, value: u8) -> GrayImage {
        GrayImage::from_pixel(width, height, Luma([value]))
    }

    pub fn paste(canvas: &mut GrayImage, image: &GrayImage, x: i64, y: i64) {
        image::imageops::overlay(canvas, image, x, y);
    }

    /// 确定性的噪声，免得测试时好时坏。
    pub fn noise(image: &mut GrayImage, amplitude: i32, seed: u32) {
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        for pixel in image.iter_mut() {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let jitter = ((state >> 24) as i32 % (amplitude * 2 + 1)) - amplitude;
            *pixel = (i32::from(*pixel) + jitter).clamp(0, 255) as u8;
        }
    }

    /// 绕中心旋转（双线性），空出来的角用 `fill` 填。
    pub fn rotate(image: &GrayImage, degrees: f32, fill: u8) -> GrayImage {
        let (w, h) = (image.width() as f32, image.height() as f32);
        let radians = degrees.to_radians();
        let (sin, cos) = radians.sin_cos();
        let side = ((w * w + h * h).sqrt()).ceil() as u32;
        GrayImage::from_fn(side, side, |x, y| {
            let (dx, dy) = (x as f32 - side as f32 / 2.0, y as f32 - side as f32 / 2.0);
            let (sx, sy) = (
                dx * cos + dy * sin + w / 2.0,
                -dx * sin + dy * cos + h / 2.0,
            );
            if sx < 0.0 || sy < 0.0 || sx >= w - 1.0 || sy >= h - 1.0 {
                return Luma([fill]);
            }
            let (x0, y0) = (sx.floor() as u32, sy.floor() as u32);
            let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
            let at = |px: u32, py: u32| f32::from(image.get_pixel(px, py).0[0]);
            let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1, y0) * fx;
            let bottom = at(x0, y0 + 1) * (1.0 - fx) + at(x0 + 1, y0 + 1) * fx;
            Luma([(top * (1.0 - fy) + bottom * fy) as u8])
        })
    }

    /// 透明底的码：深色模块不透明，其余全透明（RGB 故意留黑，正是转灰度时会出事的样子）。
    pub fn transparent(code: &GrayImage) -> RgbaImage {
        RgbaImage::from_fn(code.width(), code.height(), |x, y| {
            if code.get_pixel(x, y).0[0] < 128 {
                Rgba([0, 0, 0, 255])
            } else {
                Rgba([0, 0, 0, 0])
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use image::Luma;
    use qrcode::EcLevel;
    use std::time::Duration;

    fn limits(max_codes: usize) -> Limits {
        Limits {
            max_codes,
            deadline: Instant::now() + Duration::from_secs(120),
        }
    }

    fn texts(found: &[Decoded]) -> Vec<&str> {
        found.iter().map(|code| code.text.as_str()).collect()
    }

    #[test]
    fn a_single_code_is_found_with_its_corners_in_clockwise_order() {
        let mut page = canvas(500, 500, 255);
        paste(
            &mut page,
            &code("https://example.com/a?b=1", 6, 4, EcLevel::M),
            60,
            80,
        );
        let found = scan(&page, limits(10));
        assert_eq!(texts(&found), ["https://example.com/a?b=1"]);
        let [tl, tr, br, bl] = found[0].corners;
        // 没旋转的码：左上、右上、右下、左下，且都落在码所在的那块区域里。
        assert!(
            tl.0 < tr.0 && tl.1 < bl.1 && br.0 > bl.0 && br.1 > tr.1,
            "{:?}",
            found[0].corners
        );
        let (left, top, right, bottom) = found[0].bounds();
        assert!(
            left > 60.0 && top > 80.0 && right < 440.0 && bottom < 460.0,
            "{:?}",
            found[0].corners
        );
        assert!(found[0].size() > 120.0);
    }

    #[test]
    fn cjk_and_wifi_payloads_survive() {
        for text in [
            "你好，二维码！https://例子.中国/路径?a=1&b=二",
            "WIFI:T:WPA;S:MyHome;P:secret123;;",
        ] {
            let mut page = canvas(420, 420, 255);
            paste(&mut page, &code(text, 6, 4, EcLevel::M), 20, 20);
            assert_eq!(texts(&scan(&page, limits(10))), [text]);
        }
    }

    /// 国内早年的生成器把 GBK 字节直接塞进码里、不写编码声明；不能显示成乱码。
    #[test]
    fn a_gbk_payload_without_an_encoding_declaration_is_read_as_gbk() {
        let (gbk, _, _) = encoding_rs::GBK.encode("你好，世界！扫码领取");
        assert!(std::str::from_utf8(&gbk).is_err());
        let mut page = canvas(420, 420, 255);
        paste(&mut page, &code_of_bytes(&gbk, 6, 4), 20, 20);
        assert_eq!(texts(&scan(&page, limits(10))), ["你好，世界！扫码领取"]);
    }

    /// 九个码紧挨着排（一版贴纸），要找全，并按阅读顺序给。
    #[test]
    fn a_tight_sheet_of_nine_codes_is_read_completely_and_in_order() {
        let expected: Vec<String> = (0..9)
            .map(|i| format!("https://sheet.example.com/{i}"))
            .collect();
        let tiles: Vec<GrayImage> = expected.iter().map(|t| code(t, 5, 2, EcLevel::M)).collect();
        let side = tiles[0].width() as i64;
        let mut page = canvas(side as u32 * 3 + 40, side as u32 * 3 + 40, 255);
        for (index, tile) in tiles.iter().enumerate() {
            let (col, row) = ((index % 3) as i64, (index / 3) as i64);
            paste(
                &mut page,
                tile,
                10 + col * (side + 10),
                10 + row * (side + 10),
            );
        }
        let found = scan(&page, limits(20));
        assert_eq!(
            texts(&found),
            expected.iter().map(String::as_str).collect::<Vec<_>>()
        );
    }

    #[test]
    fn two_codes_of_different_sizes_are_both_found() {
        let mut page = canvas(900, 500, 255);
        paste(
            &mut page,
            &code("https://big.example.com/b", 9, 4, EcLevel::M),
            30,
            30,
        );
        paste(
            &mut page,
            &code("https://small.example.com/s", 4, 3, EcLevel::M),
            600,
            300,
        );
        let found = scan(&page, limits(10));
        assert_eq!(
            texts(&found),
            ["https://big.example.com/b", "https://small.example.com/s"]
        );
    }

    /// 文字相同、位置不同的是两个码（同一张海报贴了两处），不能当重复吞掉；
    /// 多个尺度各找一次的同一个码才合并。
    #[test]
    fn the_same_text_at_two_places_stays_two_codes() {
        let tile = code("https://same.example.com/x", 6, 3, EcLevel::M);
        let mut page = canvas(900, 400, 255);
        paste(&mut page, &tile, 20, 40);
        paste(&mut page, &tile, 520, 60);
        assert_eq!(scan(&page, limits(10)).len(), 2);
        // 同一个码在多个尺度下都扫得到，也只算一个。
        let mut single = canvas(1500, 1500, 255);
        paste(
            &mut single,
            &code("https://once.example.com/x", 10, 4, EcLevel::M),
            400,
            400,
        );
        assert_eq!(scan(&single, limits(10)).len(), 1);
    }

    #[test]
    fn the_limit_on_codes_is_respected() {
        let tile = code("https://limit.example.com/x", 4, 3, EcLevel::M);
        let side = tile.width() as i64;
        let mut page = canvas(side as u32 * 3 + 40, side as u32 + 20, 255);
        for i in 0..3 {
            paste(&mut page, &tile, 10 + i * (side + 10), 10);
        }
        assert_eq!(scan(&page, limits(2)).len(), 2);
    }

    /// 深色模式的反色码：正常极性一个都扫不出来，兜底才去反色。
    #[test]
    fn an_inverted_code_is_found_by_the_fallback() {
        let mut inverted = code("https://inverted.example.com/x", 6, 4, EcLevel::M);
        inverted.iter_mut().for_each(|pixel| *pixel = 255 - *pixel);
        let mut page = canvas(400, 400, 0);
        paste(&mut page, &inverted, 20, 20);
        assert_eq!(
            texts(&scan(&page, limits(10))),
            ["https://inverted.example.com/x"]
        );
    }

    /// 灰蒙蒙的低对比度图：拉伸对比度那一遍救回来。
    #[test]
    fn a_dull_low_contrast_code_is_found() {
        let mut page = canvas(400, 400, 255);
        paste(
            &mut page,
            &code("https://dull.example.com/x", 6, 4, EcLevel::M),
            20,
            20,
        );
        for pixel in page.iter_mut() {
            *pixel = 120 + *pixel / 8; // 0..255 压进 120..151
        }
        noise(&mut page, 3, 21);
        assert_eq!(
            texts(&scan(&page, limits(10))),
            ["https://dull.example.com/x"]
        );
    }

    #[test]
    fn a_rotated_code_is_found() {
        let rotated = rotate(
            &code("https://rotated.example.com/r", 7, 4, EcLevel::M),
            28.0,
            255,
        );
        assert_eq!(
            texts(&scan(&rotated, limits(10))),
            ["https://rotated.example.com/r"]
        );
    }

    /// 手机拍的海报：码只占画面一小块，周围是噪声。
    #[test]
    fn a_small_code_in_a_noisy_photo_is_found() {
        let mut page = canvas(1600, 1200, 140);
        noise(&mut page, 40, 7);
        paste(
            &mut page,
            &code("https://poster.example.com/join", 4, 2, EcLevel::M),
            1100,
            700,
        );
        let found = scan(&page, limits(10));
        assert_eq!(texts(&found), ["https://poster.example.com/join"]);
        let (left, top, ..) = found[0].bounds();
        assert!(
            left > 1000.0 && top > 600.0,
            "位置应在右下：{:?}",
            found[0].corners
        );
    }

    /// 聊天软件压缩后的小码：一个模块只有两三个像素。
    #[test]
    fn a_tiny_compressed_code_is_found() {
        let mut page = code("https://tiny.example.com/t", 3, 3, EcLevel::M);
        noise(&mut page, 10, 3);
        assert_eq!(
            texts(&scan(&page, limits(10))),
            ["https://tiny.example.com/t"]
        );
    }

    /// 带 logo 的码（纠错级别 H 容许盖住中间一块）。
    #[test]
    fn a_code_with_a_logo_in_the_middle_is_found() {
        let mut image = code(
            "https://logo.example.com/abcdef?token=1234567890",
            8,
            4,
            EcLevel::H,
        );
        let side = image.width();
        let logo = canvas(side / 5, side / 5, 90);
        paste(
            &mut image,
            &logo,
            i64::from(side) * 2 / 5,
            i64::from(side) * 2 / 5,
        );
        assert_eq!(
            texts(&scan(&image, limits(10))),
            ["https://logo.example.com/abcdef?token=1234567890"]
        );
    }

    /// Micro QR 与普通码并排：两种都认得，这是换用 zxing 之后才有的。
    #[test]
    fn micro_qr_codes_are_found_next_to_regular_ones() {
        let mut page = canvas(700, 300, 255);
        paste(&mut page, &micro_code(), 20, 20);
        paste(
            &mut page,
            &code("https://regular.example.com/r", 5, 3, EcLevel::M),
            350,
            20,
        );
        let found = scan(&page, limits(10));
        assert_eq!(texts(&found), ["MICRO12", "https://regular.example.com/r"]);
    }

    /// 透明底的 PNG：透明像素的 RGB 是黑的，直接转灰度会整张染黑。
    #[test]
    fn a_transparent_png_is_composited_on_white() {
        let rgba = transparent(&code("https://alpha.example.com/a", 6, 4, EcLevel::M));
        let image = DynamicImage::ImageRgba8(rgba);
        let gray = to_gray(&image);
        assert_eq!(gray.get_pixel(0, 0).0[0], 255, "透明处应当是白底");
        assert_eq!(
            texts(&scan(&gray, limits(10))),
            ["https://alpha.example.com/a"]
        );
    }

    #[test]
    fn nothing_is_found_in_a_blank_noisy_or_tiny_image() {
        assert!(scan(&canvas(300, 300, 255), limits(10)).is_empty());
        let mut noisy = canvas(300, 300, 128);
        noise(&mut noisy, 120, 11);
        assert!(scan(&noisy, limits(10)).is_empty());
        assert!(scan(&canvas(20, 20, 0), limits(10)).is_empty());
        assert!(scan(&canvas(300, 300, 255), limits(0)).is_empty());
    }

    /// 截止时间已过：立刻收工，不 panic。
    #[test]
    fn an_expired_deadline_returns_immediately() {
        let mut page = canvas(400, 400, 255);
        paste(
            &mut page,
            &code("https://late.example.com/x", 6, 4, EcLevel::M),
            20,
            20,
        );
        let expired = Limits {
            max_codes: 10,
            deadline: Instant::now() - Duration::from_secs(1),
        };
        assert!(scan(&page, expired).is_empty());
    }

    /// 回归：换用 zxing 的原因。满是「定位图案」模样的方框的杂乱大图，以及纯噪声图，
    /// 上一个引擎（rqrr）整图扫分别要 154 秒与 295 秒；现在应在几秒内返回，一个码也不报。
    #[test]
    fn cluttered_and_noisy_big_images_finish_quickly_and_report_nothing() {
        let mut page = canvas(2400, 1600, 150);
        noise(&mut page, 25, 9);
        let mut state = 12345u32;
        let mut next = |bound: u32| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 8) % bound
        };
        for _ in 0..200 {
            let (x, y, side) = (next(2200), next(1400), 40 + next(60));
            let mut finder = canvas(side, side, 0);
            paste(
                &mut finder,
                &canvas(side / 3, side / 3, 255),
                i64::from(side / 3 - side / 6),
                i64::from(side / 3 - side / 6),
            );
            paste(
                &mut finder,
                &canvas(side / 9 + 1, side / 9 + 1, 0),
                i64::from(side / 2 - side / 18),
                i64::from(side / 2 - side / 18),
            );
            paste(&mut page, &finder, i64::from(x), i64::from(y));
        }
        let mut static_noise = canvas(1200, 900, 128);
        noise(&mut static_noise, 127, 11);
        for image in [page, static_noise] {
            let started = Instant::now();
            let found = scan(&image, limits(10));
            let elapsed = started.elapsed();
            assert!(found.is_empty(), "杂乱的图里不该凭空出码：{found:?}");
            assert!(
                elapsed < Duration::from_secs(20),
                "耗时 {elapsed:?}，zxing 应当在几秒内返回"
            );
        }
    }

    /// 大图里右下角一个 150 像素的小码，周围是噪声：要找得到。
    #[test]
    fn a_small_code_in_a_big_image_is_found() {
        let mut page = canvas(2400, 1600, 150);
        noise(&mut page, 30, 5);
        paste(
            &mut page,
            &code("https://big.example.com/small", 4, 2, EcLevel::M),
            2050,
            1250,
        );
        let found = scan(&page, limits(10));
        assert_eq!(texts(&found), ["https://big.example.com/small"]);
        let (left, top, ..) = found[0].bounds();
        assert!(
            left > 2000.0 && top > 1200.0,
            "位置应在右下：{:?}",
            found[0].corners
        );
    }

    fn passes(list: &[Pass]) -> Vec<(f32, Treatment, Binarization)> {
        list.iter()
            .map(|pass| (pass.scale, pass.treatment, pass.binarization))
            .collect()
    }

    /// 中等大小的图：原尺寸一遍就够；兜底再换二值化、反色、拉伸。
    #[test]
    fn a_medium_image_gets_one_native_pass_and_a_few_fallbacks() {
        let (first, fallback) = plan(800, 600);
        assert_eq!(
            passes(&first),
            [(1.0, Treatment::Plain, Binarization::Hybrid)]
        );
        let fallback = passes(&fallback);
        assert!(fallback.contains(&(1.0, Treatment::Plain, Binarization::Global)));
        assert!(fallback.contains(&(1.0, Treatment::Invert, Binarization::Hybrid)));
        assert!(fallback.contains(&(1.0, Treatment::Stretch, Binarization::Hybrid)));
    }

    /// 大图多几个缩小的尺度；特大的图先缩到 MAX_SIDE 作为「原尺寸」。
    #[test]
    fn big_images_add_downscaled_passes_and_huge_ones_are_capped_first() {
        let first: Vec<f32> = plan(3000, 2000).0.iter().map(|pass| pass.scale).collect();
        assert_eq!(first, [1.0, 0.5, 0.25]);
        let huge: Vec<f32> = plan(8000, 6000).0.iter().map(|pass| pass.scale).collect();
        assert!(
            (huge[0] - 0.512).abs() < 1e-3,
            "长边要先缩到 {MAX_SIDE}：{huge:?}"
        );
        assert!(huge.iter().all(|scale| *scale <= 0.512 + 1e-3));
    }

    #[test]
    fn tiny_images_are_enlarged() {
        let scales = |w, h| -> Vec<f32> { plan(w, h).0.iter().map(|pass| pass.scale).collect() };
        assert_eq!(scales(200, 200), [1.0, 2.0, 3.0]);
        assert_eq!(
            scales(500, 500),
            [1.0, 2.0],
            "长边 700 以内放大一次，300 以内才放大两次"
        );
    }

    /// 只有「不是合法 UTF-8、zxing 兜底出 Latin-1 乱码、字节又能无损按 GBK 读通」才改判；
    /// 正常的文字、带非 Latin-1 字符的（ECI 声明或日文汉字模式给出的）一概不动。
    #[test]
    fn gbk_is_rescued_only_from_latin1_mojibake() {
        let (gbk, _, _) = encoding_rs::GBK.encode("你好，世界");
        let mojibake: String = gbk.iter().map(|&byte| byte as char).collect();
        assert_eq!(rescue_gbk(&mojibake, &gbk).as_deref(), Some("你好，世界"));
        // 本来就对：合法 UTF-8 不动。
        assert_eq!(rescue_gbk("你好", "你好".as_bytes()), None);
        // zxing 已经解出了 Latin-1 以外的字（ECI 声明过的）：不动。
        assert_eq!(rescue_gbk("你好", &gbk), None);
        // 真正的 Latin-1 文字：字节读不通 GBK，不动。
        assert_eq!(rescue_gbk("café", b"caf\xe9"), None);
        // 太短、空：不动。
        assert_eq!(rescue_gbk("a", b"a"), None);
        assert_eq!(rescue_gbk("", b""), None);
    }

    #[test]
    fn codes_are_listed_in_reading_order() {
        let at = |text: &str, x: f32, y: f32| Decoded {
            text: text.to_string(),
            corners: [
                (x, y),
                (x + 100.0, y),
                (x + 100.0, y + 100.0),
                (x, y + 100.0),
            ],
        };
        // 第一行的右边那个比左边略高几十像素，仍算同一行。
        let mut codes = vec![
            at("下", 10.0, 300.0),
            at("右", 300.0, 20.0),
            at("左", 10.0, 60.0),
        ];
        reading_order(&mut codes);
        assert_eq!(texts(&codes), ["左", "右", "下"]);
    }

    #[test]
    fn low_contrast_is_stretched_but_a_healthy_range_is_left_alone() {
        let mut dull = GrayImage::from_fn(100, 100, |x, _| Luma([if x < 50 { 110 } else { 150 }]));
        stretch_contrast(&mut dull);
        assert!(dull.get_pixel(10, 10).0[0] < 20 && dull.get_pixel(90, 10).0[0] > 235);
        let mut healthy =
            GrayImage::from_fn(100, 100, |x, _| Luma([if x < 50 { 10 } else { 245 }]));
        stretch_contrast(&mut healthy);
        assert_eq!(healthy.get_pixel(10, 10).0[0], 10);
        let mut flat = canvas(100, 100, 128);
        stretch_contrast(&mut flat);
        assert_eq!(flat.get_pixel(5, 5).0[0], 128);
    }
}
