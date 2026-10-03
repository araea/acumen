//! 一张图里有好几个二维码时，在原图上把每个框出来、标上序号，和文字清单里的
//! ①②③ 一一对应：不然「第二个是哪个」要靠猜。
//!
//! 只在多码时才画；一个码没有对应关系要说清，不值得多发一张图。

use crate::render::canvas::{Canvas, Ink};
use crate::render::font::Fonts;
use image::{DynamicImage, Rgb, RgbImage, Rgba, RgbaImage, imageops::FilterType};

/// 标记色。深靛蓝配白字、白色描边：压在浅色、深色、花哨的照片上都认得出来。
const ACCENT: (u8, u8, u8) = (0x4f, 0x46, 0xe5);
/// 预览图的长边上限：够看清标记，又不至于把一张手机原图再发一遍。
pub const PREVIEW_SIDE: u32 = 1280;

/// 一个要标的码：序号与它在预览图里的四个角。
#[derive(Debug, Clone, Copy)]
pub struct Mark {
    pub number: usize,
    pub corners: [(f32, f32); 4],
}

/// 缩成预览图（长边不超过 [`PREVIEW_SIDE`]，小图不放大），返回缩放比例。
/// 透明底垫白，免得标记压在一片黑上。
pub fn preview_of(image: &DynamicImage) -> (RgbImage, f32) {
    let rgba = image.to_rgba8();
    let long = rgba.width().max(rgba.height());
    let ratio = (PREVIEW_SIDE as f32 / long as f32).min(1.0);
    let rgba = if ratio < 1.0 {
        image::imageops::resize(
            &rgba,
            ((rgba.width() as f32 * ratio).round() as u32).max(1),
            ((rgba.height() as f32 * ratio).round() as u32).max(1),
            FilterType::Triangle,
        )
    } else {
        rgba
    };
    let rgb = RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let over = |c: u8| ((u32::from(c) * u32::from(a) + 255 * (255 - u32::from(a))) / 255) as u8;
        Rgb([over(r), over(g), over(b)])
    });
    (rgb, ratio)
}

/// 在预览图上画框与序号，编码成 JPEG。字体装不上时只画框、不画序号（清单仍按顺序）。
pub fn annotate(preview: &RgbImage, marks: &[Mark]) -> Option<Vec<u8>> {
    let mut image = preview.clone();
    for mark in marks {
        outline(&mut image, mark.corners);
    }
    for mark in marks {
        badge(&mut image, mark);
    }
    let mut jpeg = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 88);
    image.write_with_encoder(encoder).ok()?;
    Some(jpeg)
}

/// 四边形的描边：先一圈白、再一圈靛蓝，粗细跟着码的大小走。
fn outline(image: &mut RgbImage, corners: [(f32, f32); 4]) {
    let size = quad_size(&corners);
    let width = (size * 0.025).clamp(2.5, 7.0);
    for (color, extra) in [((255, 255, 255), 2.4), (ACCENT, 0.0)] {
        for i in 0..4 {
            stroke(
                image,
                corners[i],
                corners[(i + 1) % 4],
                width + extra,
                color,
            );
        }
    }
}

fn quad_size(corners: &[(f32, f32); 4]) -> f32 {
    let diagonal =
        |p: (f32, f32), q: (f32, f32)| ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2)).sqrt();
    (diagonal(corners[0], corners[2]) + diagonal(corners[1], corners[3]))
        / 2.0
        / std::f32::consts::SQRT_2
}

/// 一条粗线段：逐像素按到线段的距离算覆盖度，边缘抗锯齿。
fn stroke(image: &mut RgbImage, a: (f32, f32), b: (f32, f32), width: f32, color: (u8, u8, u8)) {
    let half = width / 2.0;
    let (w, h) = (image.width() as i32, image.height() as i32);
    let x0 = ((a.0.min(b.0) - half - 1.0).floor() as i32).max(0);
    let x1 = ((a.0.max(b.0) + half + 1.0).ceil() as i32).min(w - 1);
    let y0 = ((a.1.min(b.1) - half - 1.0).floor() as i32).max(0);
    let y1 = ((a.1.max(b.1) + half + 1.0).ceil() as i32).min(h - 1);
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length_sq = (dx * dx + dy * dy).max(1e-6);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let t = (((px - a.0) * dx + (py - a.1) * dy) / length_sq).clamp(0.0, 1.0);
            let distance = ((px - (a.0 + t * dx)).powi(2) + (py - (a.1 + t * dy)).powi(2)).sqrt();
            let coverage = (half + 0.5 - distance).clamp(0.0, 1.0);
            if coverage > 0.0 {
                let pixel = image.get_pixel_mut(x as u32, y as u32);
                for (channel, target) in pixel.0.iter_mut().zip([color.0, color.1, color.2]) {
                    *channel = (f32::from(*channel) * (1.0 - coverage)
                        + f32::from(target) * coverage) as u8;
                }
            }
        }
    }
}

/// 序号徽章：压在码的左上角，圆心略往外，再收回画面内。
fn badge(image: &mut RgbImage, mark: &Mark) {
    let Some(fonts) = Fonts::get() else {
        return;
    };
    let size = quad_size(&mark.corners);
    let radius = (size * 0.13).clamp(14.0, 34.0);
    let (w, h) = (image.width() as f32, image.height() as f32);
    let margin = radius + 2.0;
    let cx = (mark.corners[0].0 - radius * 0.2).clamp(margin, (w - margin).max(margin));
    let cy = (mark.corners[0].1 - radius * 0.2).clamp(margin, (h - margin).max(margin));

    let side = (radius * 2.0 + 6.0).ceil();
    let mut canvas = Canvas::new(side, side, 1.0);
    let center = side / 2.0;
    canvas.circle_fill(center, center, radius + 2.5, Ink::rgb(255, 255, 255));
    canvas.circle_fill(
        center,
        center,
        radius,
        Ink::rgb(ACCENT.0, ACCENT.1, ACCENT.2),
    );
    let label = mark.number.to_string();
    let px = if label.len() > 1 {
        radius * 1.05
    } else {
        radius * 1.3
    };
    canvas.text_center_ink(
        center,
        center,
        &label,
        &fonts.sans_b,
        px,
        Ink::rgb(255, 255, 255),
        0.0,
    );
    overlay_rgba(
        image,
        &canvas.img,
        (cx - center).round() as i64,
        (cy - center).round() as i64,
    );
}

fn overlay_rgba(base: &mut RgbImage, layer: &RgbaImage, left: i64, top: i64) {
    for (x, y, Rgba([r, g, b, a])) in layer.enumerate_pixels() {
        let (bx, by) = (left + i64::from(x), top + i64::from(y));
        if bx < 0
            || by < 0
            || bx >= i64::from(base.width())
            || by >= i64::from(base.height())
            || *a == 0
        {
            continue;
        }
        let pixel = base.get_pixel_mut(bx as u32, by as u32);
        let alpha = f32::from(*a) / 255.0;
        for (channel, source) in pixel.0.iter_mut().zip([*r, *g, *b]) {
            *channel = (f32::from(*channel) * (1.0 - alpha) + f32::from(source) * alpha) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::qr_scan::scan::fixtures::{canvas, code, paste};
    use image::GrayImage;
    use qrcode::EcLevel;

    fn gray_to_rgb(gray: &GrayImage) -> RgbImage {
        RgbImage::from_fn(gray.width(), gray.height(), |x, y| {
            let v = gray.get_pixel(x, y).0[0];
            Rgb([v, v, v])
        })
    }

    fn square(x: f32, y: f32, side: f32) -> [(f32, f32); 4] {
        [(x, y), (x + side, y), (x + side, y + side), (x, y + side)]
    }

    #[test]
    fn small_images_are_not_enlarged_and_big_ones_are_capped() {
        let small = DynamicImage::ImageRgb8(RgbImage::new(600, 400));
        let (preview, ratio) = preview_of(&small);
        assert_eq!((preview.width(), preview.height(), ratio), (600, 400, 1.0));

        let big = DynamicImage::ImageRgb8(RgbImage::new(4000, 3000));
        let (preview, ratio) = preview_of(&big);
        assert_eq!((preview.width(), preview.height()), (1280, 960));
        assert!((ratio - 0.32).abs() < 1e-3);
    }

    #[test]
    fn transparent_pixels_become_white_in_the_preview() {
        let transparent =
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(50, 50, Rgba([0, 0, 0, 0])));
        let (preview, _) = preview_of(&transparent);
        assert_eq!(preview.get_pixel(10, 10).0, [255, 255, 255]);
    }

    /// 框线真的画在码的边上（靛蓝），四个角之外的地方不动。
    #[test]
    fn the_outline_is_drawn_on_the_code_edges_only() {
        let mut page = canvas(400, 400, 255);
        paste(
            &mut page,
            &code("https://a.example.com", 6, 4, EcLevel::M),
            100,
            100,
        );
        let preview = gray_to_rgb(&page);
        let corners = square(124.0, 124.0, 160.0);
        let marked = annotate(&preview, &[Mark { number: 1, corners }]).unwrap();
        let decoded = image::load_from_memory(&marked).unwrap().to_rgb8();
        assert_eq!(decoded.dimensions(), (400, 400));
        // 上边框正中是靛蓝（JPEG 有损，给点余量）。
        let edge = decoded.get_pixel(204, 124).0;
        assert!(edge[2] > 150 && edge[0] < 140, "{edge:?}");
        // 远离码的角落仍是白的。
        assert!(decoded.get_pixel(390, 390).0.iter().all(|c| *c > 245));
    }

    #[test]
    fn the_number_badge_lands_on_the_top_left_corner_and_stays_inside_the_image() {
        if Fonts::get().is_none() {
            return; // 没有可用字体的环境不画序号，这条没法验。
        }
        let preview = RgbImage::from_pixel(300, 300, Rgb([255, 255, 255]));
        // 码贴着画面左上角：徽章要被收回画面里，不能被切掉。
        let marked = annotate(
            &preview,
            &[Mark {
                number: 7,
                corners: square(2.0, 2.0, 120.0),
            }],
        )
        .unwrap();
        let decoded = image::load_from_memory(&marked).unwrap().to_rgb8();
        let badge = decoded.get_pixel(16, 16).0;
        assert!(
            badge[2] > 150 && badge[0] < 140,
            "徽章底色应当是靛蓝：{badge:?}"
        );
    }

    #[test]
    fn several_marks_do_not_panic_even_when_off_canvas() {
        let preview = RgbImage::from_pixel(200, 200, Rgb([200, 200, 200]));
        let marks = [
            Mark {
                number: 1,
                corners: square(-30.0, -30.0, 100.0),
            },
            Mark {
                number: 12,
                corners: square(150.0, 150.0, 200.0),
            },
        ];
        assert!(annotate(&preview, &marks).is_some());
    }
}
