//! 原生绘制画布：不依赖浏览器的小图出图底座（目前给二维码标注用）。
//!
//! 文字用 `ab_glyph` 直接光栅化，圆形用 SDF 逐像素判定覆盖度做抗锯齿。
//! 所有坐标都是「逻辑像素」，绘制时统一乘设备比例 `s`，放大不糊。
//! 出图不需要 Chrome，不受无头浏览器启动失败影响。

// 绘图原语（x/y/w/h/r/ink…）参数个数是自然的，不做结构体打包
#![allow(clippy::too_many_arguments)]

use super::font::Face;
use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use image::{Rgba, RgbaImage};

/// 带透明度的颜色。最终都往不透明底上 over 混合。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ink {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: f32,
}

impl Ink {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Ink { r, g, b, a: 1.0 }
    }
}

/// 逻辑坐标画布：内部图像是逻辑尺寸 × `s`。
pub struct Canvas {
    pub img: RgbaImage,
    s: f32,
    /// 文字覆盖度的伽马修正。深底浅字线性混合会显得过细，
    /// `<1` 加粗笔画，`1.0` 关闭。见 [`Canvas::set_text_gamma`]。
    text_gamma: f32,
}

impl Canvas {
    pub fn new(w_logical: f32, h_logical: f32, s: f32) -> Self {
        let img = RgbaImage::new((w_logical * s).ceil() as u32, (h_logical * s).ceil() as u32);
        Canvas {
            img,
            s,
            text_gamma: 1.0,
        }
    }

    #[inline]
    fn blend(&mut self, x: i32, y: i32, ink: Ink, cov: f32) {
        let (w, h) = self.img.dimensions();
        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
            return;
        }
        let a = (ink.a * cov).clamp(0.0, 1.0);
        if a <= 0.0 {
            return;
        }
        let p = self.img.get_pixel_mut(x as u32, y as u32);
        // 标准 over 合成（straight alpha）：底为透明时保留墨色本来的透明度，
        // 图层上的半透明墨色才不会混进黑色背景
        let pa = p[3] as f32 / 255.0;
        let oa = a + pa * (1.0 - a);
        let mix = |fg: u8, bg: u8| -> u8 {
            if oa <= 0.0 {
                0
            } else {
                ((fg as f32 * a + bg as f32 * pa * (1.0 - a)) / oa) as u8
            }
        };
        *p = Rgba([
            mix(ink.r, p[0]),
            mix(ink.g, p[1]),
            mix(ink.b, p[2]),
            (oa * 255.0) as u8,
        ]);
    }

    pub fn circle_fill(&mut self, cx: f32, cy: f32, r: f32, ink: Ink) {
        let s = self.s;
        let (cx, cy, r) = (cx * s, cy * s, r * s);
        for py in (cy - r - 1.0).floor() as i32..(cy + r + 1.0).ceil() as i32 {
            for px in (cx - r - 1.0).floor() as i32..(cx + r + 1.0).ceil() as i32 {
                let d = (px as f32 - cx).hypot(py as f32 - cy) - r;
                if d <= 0.0 {
                    self.blend(px, py, ink, (0.5 - d).clamp(0.0, 1.0));
                }
            }
        }
    }

    // ================= 文字 =================

    /// 字号按 em 像素解释，与 CSS font-size 一致。ab_glyph 的 PxScale 是
    /// ascent - descent，直接传字号会把 CJK 字面缩小。逐字体换算也让回退字形等大。
    fn font_scale(&self, font: &FontVec, px: f32) -> PxScale {
        let em = font
            .units_per_em()
            .unwrap_or_else(|| font.height_unscaled());
        PxScale::from(px * self.s * font.height_unscaled() / em)
    }

    /// 单个字符的步进宽度（逻辑像素）。缺字时按首选字体的 `.notdef` 步进，
    /// 保证「画不出来」与「量出来的宽度」一致，版式不会错位。
    fn advance(&self, face: &Face, ch: char, px: f32) -> f32 {
        match face.glyph(ch) {
            Some((font, gid)) => font.as_scaled(self.font_scale(font, px)).h_advance(gid) / self.s,
            None => {
                let font = face.primary();
                font.as_scaled(self.font_scale(font, px))
                    .h_advance(font.glyph_id(ch))
                    / self.s
            }
        }
    }

    /// 文字宽度（逻辑像素）
    pub fn text_w(&self, text: &str, face: &Face, px: f32, spacing: f32) -> f32 {
        let mut w = 0.0f32;
        let n = text.chars().count();
        for (i, ch) in text.chars().enumerate() {
            w += self.advance(face, ch, px);
            if i + 1 < n {
                w += spacing;
            }
        }
        w
    }

    /// 在基线处绘制一行文字，返回占用宽度。坐标为逻辑像素。
    pub fn text(
        &mut self,
        x: f32,
        baseline: f32,
        text: &str,
        face: &Face,
        px: f32,
        ink: Ink,
        spacing: f32,
    ) -> f32 {
        let s = self.s;
        let gamma = self.text_gamma;
        let mut pen = x * s;
        for ch in text.chars() {
            // 缺字则整字跳过（不画豆腐块），但仍按度量步进
            if let Some((font, gid)) = face.glyph(ch) {
                let glyph = gid
                    .with_scale_and_position(self.font_scale(font, px), point(pen, baseline * s));
                if let Some(og) = font.outline_glyph(glyph) {
                    let b = og.px_bounds();
                    let (bx, by) = (b.min.x as i32, b.min.y as i32);
                    og.draw(|gx, gy, cov| {
                        let cov = if gamma == 1.0 { cov } else { cov.powf(gamma) };
                        self.blend(bx + gx as i32, by + gy as i32, ink, cov);
                    });
                }
            }
            pen += self.advance(face, ch, px) * s + spacing * s;
        }
        (pen - x * s - if text.is_empty() { 0.0 } else { spacing * s }) / s
    }

    /// 水平垂直居中绘制
    pub fn text_center(
        &mut self,
        cx: f32,
        cy: f32,
        text: &str,
        face: &Face,
        px: f32,
        ink: Ink,
        spacing: f32,
    ) {
        let w = self.text_w(text, face, px, spacing);
        let sc = face
            .primary()
            .as_scaled(self.font_scale(face.primary(), px));
        // top = cy - 行高/2；baseline = top + ascent（均换算回逻辑像素）
        let baseline = cy - (sc.ascent() - sc.descent()) / 2.0 / self.s + sc.ascent() / self.s;
        self.text(cx - w / 2.0, baseline, text, face, px, ink, spacing);
    }

    /// 按**字形实际墨迹**居中，而不是按步进宽度。
    ///
    /// 全角标点（尤其「？」「，」）的字身在字面里是偏侧的：按步进宽度居中，
    /// 看上去就会歪在格子一角。田字格里的占位问号、印章里的单字这类
    /// 「一个字要正对着一个框」的场合必须按墨迹居中。
    pub fn text_center_ink(
        &mut self,
        cx: f32,
        cy: f32,
        text: &str,
        face: &Face,
        px: f32,
        ink: Ink,
        spacing: f32,
    ) {
        // 量出墨迹包围盒后反推落笔点：让包围盒的中心正好落在 (cx, cy)
        match self.ink_bounds(text, face, px, spacing) {
            Some((x0, y0, x1, y1)) => {
                self.text(
                    cx - (x0 + x1) / 2.0,
                    cy - (y0 + y1) / 2.0,
                    text,
                    face,
                    px,
                    ink,
                    spacing,
                );
            }
            None => self.text_center(cx, cy, text, face, px, ink, spacing),
        }
    }

    /// 一段文字的墨迹包围盒，相对「落笔点 (0,0)、基线 y=0」，单位逻辑像素。
    /// 全是空白或缺字时返回 None。
    pub fn ink_bounds(
        &self,
        text: &str,
        face: &Face,
        px: f32,
        spacing: f32,
    ) -> Option<(f32, f32, f32, f32)> {
        let s = self.s;
        let mut pen = 0.0f32;
        let mut acc: Option<(f32, f32, f32, f32)> = None;
        for ch in text.chars() {
            if let Some((font, gid)) = face.glyph(ch)
                && let Some(og) = font.outline_glyph(
                    gid.with_scale_and_position(self.font_scale(font, px), point(pen, 0.0)),
                )
            {
                let b = og.px_bounds();
                acc = Some(match acc {
                    None => (b.min.x, b.min.y, b.max.x, b.max.y),
                    Some((x0, y0, x1, y1)) => (
                        x0.min(b.min.x),
                        y0.min(b.min.y),
                        x1.max(b.max.x),
                        y1.max(b.max.y),
                    ),
                });
            }
            pen += self.advance(face, ch, px) * s + spacing * s;
        }
        acc.map(|(x0, y0, x1, y1)| (x0 / s, y0 / s, x1 / s, y1 / s))
    }
}
