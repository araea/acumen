use super::utils;
use base64::{Engine as _, engine::general_purpose};
use image::{
    AnimationDecoder, DynamicImage, Frame, GenericImageView, ImageBuffer, ImageDecoder,
    codecs::gif::{GifDecoder, GifEncoder, Repeat},
    imageops,
};
use std::io::Cursor;
use std::time::Duration;

use crate::plugins::PluginResult;

const MAX_PIXELS: u64 = 32_000_000;
const MAX_FRAMES: usize = 256;

fn check_dimensions(width: u32, height: u32, frames: usize) -> PluginResult<()> {
    if width == 0
        || height == 0
        || frames > MAX_FRAMES
        || u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|v| v.checked_mul(frames as u64))
            .is_none_or(|v| v > MAX_PIXELS)
    {
        return Err("图片尺寸或帧数过大（最多 256 帧、总计 3200 万像素）".into());
    }
    Ok(())
}

fn decode_frames(bytes: Vec<u8>) -> PluginResult<Vec<Frame>> {
    let decoder = GifDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let (width, height) = decoder.dimensions();
    check_dimensions(width, height, 1)?;
    let mut frames = Vec::new();
    for frame in decoder.into_frames() {
        check_dimensions(width, height, frames.len() + 1)?;
        frames.push(frame.map_err(|e| e.to_string())?);
    }
    if frames.is_empty() {
        return Err("GIF 没有帧".into());
    }
    Ok(frames)
}

/// 合成 GIF (网格图 -> 动图)
pub fn grid_to_gif(
    img_bytes: Vec<u8>,
    rows: u32,
    cols: u32,
    interval_secs: f64,
    margin: u32,
) -> PluginResult<String> {
    if rows == 0
        || cols == 0
        || u64::from(rows) * u64::from(cols) > MAX_FRAMES as u64
        || !interval_secs.is_finite()
        || interval_secs <= 0.0
        || interval_secs > 3600.0
    {
        return Err("网格最多 256 帧，帧间隔须在 0—3600 秒之间".into());
    }
    let img = image::load_from_memory(&img_bytes).map_err(|e| e.to_string())?;
    let (width, height) = img.dimensions();

    check_dimensions(width, height, 1)?;
    if u64::from(cols - 1) * u64::from(margin) >= u64::from(width)
        || u64::from(rows - 1) * u64::from(margin) >= u64::from(height)
    {
        return Err("边距超过图片尺寸".into());
    }
    // 计算单个切片的尺寸 (考虑边距)
    let tile_width = if cols > 1 {
        (width.saturating_sub((cols - 1) * margin)) / cols
    } else {
        width
    };
    let tile_height = if rows > 1 {
        (height.saturating_sub((rows - 1) * margin)) / rows
    } else {
        height
    };

    if tile_width == 0 || tile_height == 0 {
        return Err("图片尺寸太小或边距过大，无法分割".into());
    }

    let delay = image::Delay::from_saturating_duration(Duration::from_secs_f64(interval_secs));
    let mut frames = Vec::with_capacity((rows * cols) as usize);

    for r in 0..rows {
        for c in 0..cols {
            let x = c * (tile_width + margin);
            let y = r * (tile_height + margin);

            if x + tile_width > width || y + tile_height > height {
                continue;
            }

            let sub_img = img.view(x, y, tile_width, tile_height).to_image();
            frames.push(Frame::from_parts(sub_img, 0, 0, delay));
        }
    }

    if frames.is_empty() {
        return Err("无法生成任何帧，请检查参数".into());
    }

    encode_frames_to_b64(frames)
}

/// GIF 拼图 (动图 -> 网格图)
pub fn gif_to_grid(img_bytes: Vec<u8>, cols_opt: Option<u32>) -> PluginResult<String> {
    let frames = decode_frames(img_bytes)?;

    if frames.is_empty() {
        return Err("GIF 没有帧".into());
    }

    let count = frames.len() as u32;
    let (frame_w, frame_h) = frames[0].buffer().dimensions();

    let cols = cols_opt
        .unwrap_or_else(|| (count as f64).sqrt().ceil() as u32)
        .max(1);
    let rows = count.div_ceil(cols);

    let total_w = frame_w.checked_mul(cols).ok_or("拼图宽度过大")?;
    let total_h = frame_h.checked_mul(rows).ok_or("拼图高度过大")?;
    check_dimensions(total_w, total_h, 1)?;

    let mut canvas = ImageBuffer::new(total_w, total_h);

    for (i, frame) in frames.iter().enumerate() {
        let c = (i as u32) % cols;
        let r = (i as u32) / cols;
        image::imageops::overlay(
            &mut canvas,
            frame.buffer(),
            (c * frame_w) as i64,
            (r * frame_h) as i64,
        );
    }

    let mut buffer = Cursor::new(Vec::new());
    canvas
        .write_to(&mut buffer, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(general_purpose::STANDARD.encode(buffer.get_ref()))
}

/// GIF 拆分 (返回 base64 列表)
pub fn gif_to_frames(img_bytes: Vec<u8>) -> PluginResult<Vec<String>> {
    let frames = decode_frames(img_bytes)?;

    frames
        .into_iter()
        .map(|frame| {
            let mut buffer = Cursor::new(Vec::new());
            DynamicImage::ImageRgba8(frame.into_buffer())
                .write_to(&mut buffer, image::ImageFormat::Png)
                .map_err(|e| e.to_string().into())
                .map(|_| general_purpose::STANDARD.encode(buffer.get_ref()))
        })
        .collect()
}

/// GIF 信息
pub fn gif_info(img_bytes: Vec<u8>) -> PluginResult<String> {
    let len = img_bytes.len();
    let frames = decode_frames(img_bytes)?;

    if frames.is_empty() {
        return Err("无效 GIF".into());
    }

    let (w, h) = frames[0].buffer().dimensions();
    let count = frames.len();

    // 计算总时长 (将 Delay 转换为 Duration)
    let duration_ms: u128 = frames
        .iter()
        .map(|f| Duration::from(f.delay()).as_millis())
        .sum();

    Ok(format!(
        "尺寸：{}x{}\n帧数：{}\n时长：{:.2} 秒\n大小：{}",
        w,
        h,
        count,
        duration_ms as f64 / 1000.0,
        utils::format_size(len)
    ))
}

/// GIF 变换类型
pub enum Transform {
    Speed(f64),
    Reverse,
    Resize(u32, u32),
    Scale(f64),
    Rotate(i32),
    FlipH,
    FlipV,
}

pub fn process_gif(img_bytes: Vec<u8>, op: Transform) -> PluginResult<String> {
    let mut frames = decode_frames(img_bytes)?;

    if frames.is_empty() {
        return Err("GIF 解码失败或无帧".into());
    }

    let (orig_w, orig_h) = frames[0].buffer().dimensions();

    match op {
        Transform::Speed(factor) => {
            if !factor.is_finite() || factor <= 0.0 {
                return Err("倍率必须大于 0".into());
            }
            for frame in &mut frames {
                let old_ms = Duration::from(frame.delay()).as_millis() as f64;
                let new_ms = (old_ms / factor).max(10.0) as u64;
                let new_delay =
                    image::Delay::from_saturating_duration(Duration::from_millis(new_ms));
                *frame =
                    Frame::from_parts(frame.buffer().clone(), frame.left(), frame.top(), new_delay);
            }
        }
        Transform::Reverse => {
            frames.reverse();
        }
        Transform::Resize(w, h) => {
            check_dimensions(w, h, frames.len())?;
            frames = transform_frames(frames, |img| {
                img.resize_exact(w, h, imageops::FilterType::Lanczos3)
            });
        }
        Transform::Scale(s) => {
            if !s.is_finite() || s <= 0.0 {
                return Err("缩放倍率必须是有限正数".into());
            }
            let target_w = ((orig_w as f64 * s) as u32).max(1);
            let target_h = ((orig_h as f64 * s) as u32).max(1);
            check_dimensions(target_w, target_h, frames.len())?;
            frames = transform_frames(frames, |img| {
                img.resize_exact(target_w, target_h, imageops::FilterType::Lanczos3)
            });
        }
        Transform::Rotate(deg) => {
            frames = transform_frames(frames, |img| match deg.rem_euclid(360) {
                90 => img.rotate90(),
                180 => img.rotate180(),
                270 => img.rotate270(),
                _ => img,
            });
        }
        Transform::FlipH => {
            frames = transform_frames(frames, |img| img.fliph());
        }
        Transform::FlipV => {
            frames = transform_frames(frames, |img| img.flipv());
        }
    }

    encode_frames_to_b64(frames)
}

/// 统一的帧变换辅助函数
fn transform_frames<F>(frames: Vec<Frame>, transform: F) -> Vec<Frame>
where
    F: Fn(DynamicImage) -> DynamicImage,
{
    frames
        .into_iter()
        .map(|frame| {
            let delay = frame.delay();
            let img = DynamicImage::ImageRgba8(frame.into_buffer());
            Frame::from_parts(transform(img).into_rgba8(), 0, 0, delay)
        })
        .collect()
}

fn encode_frames_to_b64(frames: Vec<Frame>) -> PluginResult<String> {
    let mut buffer = Cursor::new(Vec::new());
    {
        let mut encoder = GifEncoder::new(&mut buffer);
        encoder.set_repeat(Repeat::Infinite)?;
        encoder.encode_frames(frames)?;
    }
    Ok(general_purpose::STANDARD.encode(buffer.get_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tiny_gif() -> Vec<u8> {
        let frame = Frame::new(image::RgbaImage::from_pixel(
            3,
            2,
            image::Rgba([30, 90, 60, 255]),
        ));
        general_purpose::STANDARD
            .decode(encode_frames_to_b64(vec![frame]).unwrap())
            .unwrap()
    }
    #[test]
    fn rejects_invalid_and_excessive_transform_sizes() {
        assert!(process_gif(tiny_gif(), Transform::Scale(f64::NAN)).is_err());
        assert!(process_gif(tiny_gif(), Transform::Speed(f64::INFINITY)).is_err());
        assert!(process_gif(tiny_gif(), Transform::Resize(u32::MAX, u32::MAX)).is_err());
        assert!(grid_to_gif(Vec::new(), 0, 1, 0.1, 0).is_err());
        assert!(check_dimensions(640, 480, 257).is_err());
        assert!(process_gif(tiny_gif(), Transform::Scale(2.0)).is_ok());
        assert_eq!(gif_to_frames(tiny_gif()).unwrap().len(), 1);
    }
}
