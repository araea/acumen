//! 排行榜前三名的头像边框：奖牌色的渐变圈；榜首另有一圈柔光与一顶斜戴的小皇冠。
//!
//! **克制是设计的一部分。** 名次那一列早已用奖牌色写了「第几名」，头像上这一圈只是
//! 把同一个含义再说一遍，让榜首一眼落在名字之前；它不能比头像本身还抢眼。所以：
//!
//! - 圈只有头像半径的 8%—10%，圈与头像之间留一道纸色的缝，头像不被颜色压住；
//! - 渐变只走同一支色相里很窄的一段（左上略亮、右下略深），读成「有一点金属光泽」，
//!   而不是一条彩虹；两端对纸面都过非文字的 3∶1，深端就是名次数字用的那一档（[`MEDALS`]）；
//! - 柔光只给榜首：金色 30% 起、向外平方衰减，画在所有条与文字**之下**，只染纸面；
//! - 银、铜不加任何饰物，名次之间靠「有没有柔光和皇冠」拉开层次，而不是靠颜色更艳。
//!
//! 圈与皇冠都按像素解析式抗锯齿，不走 plotters 的圆（它在位图后端上不做抗锯齿）。

use super::utils::MEDALS;
use image::{Rgba, RgbaImage};
use plotters::style::RGBColor;

/// 每个名次的色调：`(亮端, 深端)`。深端直接取名次数字的奖牌色，亮端在对纸面 3∶1 之内
/// 尽量往亮处走（算出来的：金 3.28、银 3.08、铜 3.23）。
const HIGHLIGHTS: [RGBColor; 3] = [
    RGBColor(178, 130, 14),
    RGBColor(136, 144, 152),
    RGBColor(190, 124, 76),
];

/// 榜首柔光的色：比圈亮得多的暖金。它不承载信息（只是一圈光），没有对比度要求。
const GLOW: RGBColor = RGBColor(232, 184, 62);
/// 柔光的最大不透明度（贴着圈的那一圈）。
const GLOW_PEAK: f32 = 0.30;
/// 柔光向外延伸多远，以 `s` 为单位。
const GLOW_REACH: f32 = 8.0;

/// 圈与头像之间的纸缝，以 `s` 为单位。
const GAP: f32 = 1.5;

/// 一个名次的奖牌。
#[derive(Clone, Copy, Debug)]
pub struct Medal {
    rank: usize,
    highlight: RGBColor,
    shade: RGBColor,
}

/// 第 `rank` 名（从 1 起）的奖牌；四名开外没有。
pub fn medal(rank: usize) -> Option<Medal> {
    let index = rank.checked_sub(1)?;
    Some(Medal {
        rank,
        highlight: *HIGHLIGHTS.get(index)?,
        shade: *MEDALS.get(index)?,
    })
}

impl Medal {
    /// 圈的粗细，以 `s` 为单位：榜首厚一点点。
    fn thickness(self) -> f32 {
        if self.rank == 1 { 2.5 } else { 2.0 }
    }

    /// 圈外沿的半径（像素）。
    pub fn outer_radius(self, avatar_radius: f32, s: f32) -> f32 {
        avatar_radius + (GAP + self.thickness()) * s
    }

    /// 色带上某处的颜色：左上亮、右下深，平滑过渡。`t` 取 0—1。
    fn tone(self, t: f32) -> RGBColor {
        let t = t * t * (3.0 - 2.0 * t);
        let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
        RGBColor(
            mix(self.highlight.0, self.shade.0),
            mix(self.highlight.1, self.shade.1),
            mix(self.highlight.2, self.shade.2),
        )
    }

    /// 在已经叠好头像的图上画圈。`(cx, cy)` 是头像中心。
    pub fn paint_ring(self, img: &mut RgbaImage, cx: f32, cy: f32, avatar_radius: f32, s: f32) {
        let inner = avatar_radius + GAP * s;
        let outer = self.outer_radius(avatar_radius, s);
        for_each_pixel(img, cx, cy, outer + 1.0, |img, x, y| {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let d = dx.hypot(dy);
            // 内外两条边各自一像素的线性过渡。
            let coverage = ((d - inner + 0.5).clamp(0.0, 1.0)) * ((outer - d + 0.5).clamp(0.0, 1.0));
            if coverage <= 0.0 {
                return;
            }
            // 沿左上 → 右下的方向取色：(dx + dy) / (d·√2) 在左上是 -1、右下是 1。
            let t = 0.5 + 0.5 * (dx + dy) / (d.max(1.0) * std::f32::consts::SQRT_2);
            blend(img, x as i32, y as i32, self.tone(t), coverage);
        });
    }

    /// 榜首的柔光：返回要写进画布的像素 `(x, y, 颜色)`，颜色已经与 `paper` 混好。
    /// 其余名次没有。要在画条与文字**之前**写——这样条会盖住它，它只染纸面。
    pub fn glow(self, cx: f32, cy: f32, avatar_radius: f32, s: f32, paper: RGBColor) -> Vec<(i32, i32, RGBColor)> {
        if self.rank != 1 {
            return Vec::new();
        }
        let start = self.outer_radius(avatar_radius, s) - 0.5 * s;
        let reach = GLOW_REACH * s;
        let (x0, x1) = ((cx - start - reach).floor() as i32, (cx + start + reach).ceil() as i32);
        let (y0, y1) = ((cy - start - reach).floor() as i32, (cy + start + reach).ceil() as i32);
        let mut out = Vec::new();
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy);
                let fade = 1.0 - ((d - start) / reach);
                if fade <= 0.0 || d < start {
                    continue;
                }
                let alpha = GLOW_PEAK * fade * fade;
                out.push((x, y, mix(paper, GLOW, alpha)));
            }
        }
        out
    }

    /// 榜首的小皇冠，斜戴在圈的右上角：底边贴着圈的外沿，朝外倾斜。其余名次没有。
    pub fn paint_crown(self, img: &mut RgbaImage, cx: f32, cy: f32, avatar_radius: f32, s: f32) {
        if self.rank != 1 {
            return;
        }
        // 皇冠的「上」是圈的径向朝外：戴在 −50° 的位置（右上），整体向外倾 40°。
        let angle = -50f32.to_radians();
        let (ux, uy) = (angle.cos(), angle.sin());
        let (vx, vy) = (-uy, ux); // 沿底边的方向
        let base = self.outer_radius(avatar_radius, s) - 0.5 * s;
        let (bx, by) = (cx + ux * base, cy + uy * base);
        // 本地坐标：x 沿底边、y 从底边向上（0 在底、HEIGHT 在最高的尖）。
        let to_world = |x: f32, y: f32| (bx + (vx * x + ux * y) * s, by + (vy * x + uy * y) * s);
        let to_local = |wx: f32, wy: f32| {
            let (rx, ry) = ((wx - bx) / s, (wy - by) / s);
            (rx * vx + ry * vy, rx * ux + ry * uy)
        };

        // 轮廓（本地坐标，单位 s）：底边 y=0，三个尖在 y≈8—10。
        const BODY: [(f32, f32); 7] = [
            (-7.0, 0.0),
            (-7.0, 7.4),
            (-3.4, 3.8),
            (0.0, 9.6),
            (3.4, 3.8),
            (7.0, 7.4),
            (7.0, 0.0),
        ];
        const TIPS: [(f32, f32); 3] = [(-7.0, 7.9), (0.0, 10.0), (7.0, 7.9)];
        const TIP_RADIUS: f32 = 1.05;
        const BAND: f32 = 2.2;

        // 取景框：把四个角变到世界坐标求包围盒。
        let corners = [(-9.0, -1.0), (9.0, -1.0), (-9.0, 12.0), (9.0, 12.0)].map(|(x, y)| to_world(x, y));
        let min_x = corners.iter().map(|c| c.0).fold(f32::MAX, f32::min).floor() as i32;
        let max_x = corners.iter().map(|c| c.0).fold(f32::MIN, f32::max).ceil() as i32;
        let min_y = corners.iter().map(|c| c.1).fold(f32::MAX, f32::min).floor() as i32;
        let max_y = corners.iter().map(|c| c.1).fold(f32::MIN, f32::max).ceil() as i32;

        const GRID: u32 = 4;
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                // 每个像素 4×4 采样：抗锯齿覆盖率，同时记下哪一部分占的多。
                let (mut body, mut tips) = (0u32, 0u32);
                for sy in 0..GRID {
                    for sx in 0..GRID {
                        let wx = x as f32 + (sx as f32 + 0.5) / GRID as f32;
                        let wy = y as f32 + (sy as f32 + 0.5) / GRID as f32;
                        let (lx, ly) = to_local(wx, wy);
                        if TIPS.iter().any(|&(tx, ty)| (lx - tx).hypot(ly - ty) <= TIP_RADIUS) {
                            tips += 1;
                        } else if inside(&BODY, lx, ly) {
                            body += 1;
                        }
                    }
                }
                let total = (GRID * GRID) as f32;
                if body + tips == 0 {
                    continue;
                }
                let (_, ly) = to_local(x as f32 + 0.5, y as f32 + 0.5);
                // 顶端小球用亮端，帽身自下而上由深到亮，底边那一条带压深一档。
                let body_color = if ly < BAND {
                    self.shade
                } else {
                    self.tone(1.0 - (ly - BAND) / (10.0 - BAND))
                };
                let tip_color = self.highlight;
                if body > 0 {
                    blend(img, x, y, body_color, body as f32 / total);
                }
                if tips > 0 {
                    blend(img, x, y, tip_color, tips as f32 / total);
                }
            }
        }
    }
}

/// 点是否在多边形内（奇偶规则）。
fn inside(polygon: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut hit = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let ((xi, yi), (xj, yj)) = (polygon[i], polygon[j]);
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            hit = !hit;
        }
        j = i;
    }
    hit
}

/// 在以 `(cx, cy)` 为中心、`reach` 为半径的方框内逐像素调用 `paint`。
fn for_each_pixel(img: &mut RgbaImage, cx: f32, cy: f32, reach: f32, mut paint: impl FnMut(&mut RgbaImage, u32, u32)) {
    let (width, height) = img.dimensions();
    let x0 = ((cx - reach).floor().max(0.0)) as u32;
    let y0 = ((cy - reach).floor().max(0.0)) as u32;
    let x1 = ((cx + reach).ceil().max(0.0) as u32).min(width.saturating_sub(1));
    let y1 = ((cy + reach).ceil().max(0.0) as u32).min(height.saturating_sub(1));
    for y in y0..=y1 {
        for x in x0..=x1 {
            paint(img, x, y);
        }
    }
}

/// 把 `color` 以 `alpha` 的不透明度盖到已有像素上（目标不透明）。
fn blend(img: &mut RgbaImage, x: i32, y: i32, color: RGBColor, alpha: f32) {
    let (width, height) = img.dimensions();
    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
        return;
    }
    let base = img.get_pixel(x as u32, y as u32);
    let over = |back: u8, front: u8| (f32::from(back) * (1.0 - alpha) + f32::from(front) * alpha).round() as u8;
    img.put_pixel(
        x as u32,
        y as u32,
        Rgba([over(base[0], color.0), over(base[1], color.1), over(base[2], color.2), 255]),
    );
}

fn mix(back: RGBColor, front: RGBColor, alpha: f32) -> RGBColor {
    let over = |back: u8, front: u8| (f32::from(back) * (1.0 - alpha) + f32::from(front) * alpha).round() as u8;
    RGBColor(over(back.0, front.0), over(back.1, front.1), over(back.2, front.2))
}

#[cfg(test)]
mod tests {
    use super::super::utils::contrast_ratio;
    use super::*;
    use crate::render::tokens::SURFACE;

    #[test]
    fn both_ends_of_every_ring_stay_readable_against_the_paper() {
        for rank in 1..=3 {
            let medal = medal(rank).unwrap();
            for color in [medal.highlight, medal.shade] {
                let ratio = contrast_ratio(color, SURFACE);
                assert!(ratio >= 3.0, "第 {rank} 名的圈只有 {ratio:.2}∶1");
            }
        }
    }

    #[test]
    fn ring_sits_outside_the_avatar_with_a_paper_gap() {
        let paper = Rgba([251, 248, 254, 255]);
        let mut img = RgbaImage::from_pixel(240, 240, paper);
        let (cx, cy, avatar, s) = (120.0, 120.0, 50.0, 2.0);
        let medal = medal(2).unwrap();
        medal.paint_ring(&mut img, cx, cy, avatar, s);
        let at = |radius: f32| *img.get_pixel((cx + radius) as u32, cy as u32);
        assert_eq!(at(0.0), paper, "头像里面不动");
        assert_eq!(at(51.5), paper, "圈与头像之间是纸缝");
        let outer = medal.outer_radius(avatar, s);
        assert!((outer - 57.0).abs() < 1e-3, "{outer}");
        assert_ne!(at(55.0), paper, "圈本身");
        assert_eq!(at(60.0), paper, "圈外不动");
    }

    #[test]
    fn glow_and_crown_belong_to_the_champion_alone() {
        let paper = RGBColor(251, 248, 254);
        assert!(medal(1).unwrap().glow(120.0, 120.0, 50.0, 2.0, paper).len() > 100);
        for rank in [2, 3] {
            assert!(medal(rank).unwrap().glow(120.0, 120.0, 50.0, 2.0, paper).is_empty());
            let mut img = RgbaImage::from_pixel(240, 240, Rgba([1, 2, 3, 255]));
            medal(rank).unwrap().paint_crown(&mut img, 120.0, 120.0, 50.0, 2.0);
            assert!(img.pixels().all(|p| *p == Rgba([1, 2, 3, 255])));
        }
        assert!(medal(0).is_none() && medal(4).is_none());
    }

    #[test]
    fn crown_stays_clear_of_the_bar_column() {
        // 皇冠戴在右上：它的右缘不能越过头像与横条之间的那道缝（头像右沿 + 12·s）。
        let mut img = RgbaImage::from_pixel(300, 300, Rgba([0, 0, 0, 255]));
        medal(1).unwrap().paint_crown(&mut img, 150.0, 150.0, 50.0, 2.0);
        let rightmost = (0..300)
            .filter(|&x| (0..300).any(|y| *img.get_pixel(x, y) != Rgba([0, 0, 0, 255])))
            .max()
            .unwrap();
        assert!(rightmost < 150 + 50 + 12 * 2, "皇冠右缘在 {rightmost}");
    }
}
