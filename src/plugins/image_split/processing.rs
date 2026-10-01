use base64::{Engine as _, engine::general_purpose};
use image::GenericImageView;
use std::io::Cursor;

use crate::plugins::PluginResult;

/// 阻塞执行图片裁剪，返回 Base64 列表
pub fn split_image_blocking(img_bytes: Vec<u8>, rows: u32, cols: u32) -> PluginResult<Vec<String>> {
    if rows == 0 || cols == 0 || u64::from(rows) * u64::from(cols) > 100 {
        return Err("切分行列必须为正数，最多 100 块".into());
    }
    let img = image::load_from_memory(&img_bytes)
        .map_err(|e| format!("图片读取失败：{}", e))?;

    let (width, height) = img.dimensions();
    let tile_width = width / cols;
    let tile_height = height / rows;

    if tile_width == 0 || tile_height == 0 {
        return Err("图片太小，无法按照指定规格裁剪".into());
    }

    let mut base64_list = Vec::with_capacity((rows * cols) as usize);

    for r in 0..rows {
        for c in 0..cols {
            // 用相邻边界的差分分配余数，右边和底边不丢像素。
            let x = (u64::from(c) * u64::from(width) / u64::from(cols)) as u32;
            let y = (u64::from(r) * u64::from(height) / u64::from(rows)) as u32;
            let right = (u64::from(c + 1) * u64::from(width) / u64::from(cols)) as u32;
            let bottom = (u64::from(r + 1) * u64::from(height) / u64::from(rows)) as u32;

            // crop_imm 是不可变裁剪，开销较小
            let sub_img = img.view(x, y, right - x, bottom - y).to_image();

            let mut buffer = Cursor::new(Vec::new());
            sub_img
                .write_to(&mut buffer, image::ImageFormat::Png)
                .map_err(|e| format!("切片生成失败：{}", e))?;

            let b64 = general_purpose::STANDARD.encode(buffer.get_ref());
            base64_list.push(b64);
        }
    }

    Ok(base64_list)
}
