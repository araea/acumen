use std::collections::HashMap;

use super::ChartError;
use super::data_loader::{BarData, SeriesData};
use super::utils::{
    ColorScheme, deep_tone, draw_left_accent_bar, draw_rounded_rect, ensure_contrast,
    format_percent, format_thousands, get_contrast_color, get_font, get_font_family,
    get_font_with_color, harmonize_theme, mix_with_color, mix_with_white, overlay_image, rank_ink,
    save_rgba_to_base64, track_tone, truncate_text_to_fit,
};
use crate::plugins::stats::StatsConfig;
use chrono::Local;
use image::{Rgba, RgbaImage};
use plotters::prelude::*;
use plotters::style::text_anchor::{HPos, Pos, VPos};

/// 一行的几何与配色：位置、条尾、以及从同一支色相里分出来的四个调子。
struct RowStyle {
    y: i32,
    bar_end_x: i32,
    bar: RGBColor,
    track: RGBColor,
    value_ink: RGBColor,
    pct_ink: RGBColor,
}

/// 排行榜的构图线：一组等距的浅色竖线，只刻在每行的色带里。
///
/// **它不表示任何数值。** 间距按像素定、与数据无关，每张图都画在同一处；它要解决的
/// 是构图问题——榜尾那几行整条轨道几乎都是空的，二十行摞起来右半边就是一面空墙，
/// 视线没有落点。曾经试过把它钉在榜首数值的 1/4、1/2、3/4 上，算术是对的，可那会把
/// 一个构图元素变成一个没有标注的假刻度，反而教人用错误的方式量条长（条长与数值不成
/// 正比，见 `base_bar_min_width` 处）。所以位置只跟像素有关，不跟数值有关。
///
/// 只画在色带上、不穿过行距的纸面：线是刻在条上的记号，不是铺在纸上的网格，
/// 行与行之间留白干净，整张图也就轻快。
///
/// 单独拎出来是因为它要么画在实色条之前（被条盖住），要么画在之后（压在条上），
/// 由 `stats.ranking_grid_over_bars` 决定，两处调用同一份几何。
struct ScaleGrid {
    first_x: i32,
    end_x: i32,
    step: i32,
    width: i32,
    row_height: i32,
}

impl ScaleGrid {
    /// `row_tops` 是各行色带的上沿，逐行画出该行高度内的那一段。
    fn draw<DB: DrawingBackend>(
        &self,
        root: &DrawingArea<DB, plotters::coord::Shift>,
        row_tops: impl Iterator<Item = i32> + Clone,
    ) -> Result<(), String> {
        let color = RGBAColor(0, 0, 0, 0.08);
        let mut x = self.first_x;
        while x <= self.end_x {
            // 末尾那道向内收一个线宽，落在轨道里，不跑到数字那一列的留白上
            let x0 = x.min(self.end_x - self.width);
            for y in row_tops.clone() {
                root.draw(&Rectangle::new(
                    [(x0, y), (x0 + self.width, y + self.row_height)],
                    color.filled(),
                ))
                .map_err(|e| e.to_string())?;
            }
            x += self.step;
        }
        Ok(())
    }
}

/// 绘制水平条形图 (排行榜)
///
/// 版式分成五个纵列：名次 → 头像 → 横条（实色进度 + 淡色轨道）→ 数值 → 占比。
///
/// 三条排版上的约定：
///
/// - **名次单独成列。** 从前这张榜只能靠数行数才知道第几名——排行榜没有名次，
///   是缺了它最该有的那一列。前三名走固定的奖牌色，不跟头像色走：名次的颜色
///   本身就是含义，跟着主题走一遍就不认得了。数字本身是第二个通道，转成灰度
///   也还读得出第几名。
/// - **数值与占比各自右对齐成固定的一列。** 跟着条尾走会排成一串阶梯，二十行
///   就是二十个不同的起点，上下比大小要一个个找；而且短条那几行的数字压在淡色
///   轨道上，长条那几行压在纸上，同一列字踩着两种底。
/// - **色带上有构图线，但没有刻度。** 构图线等距排布、与数据无关，只为给右半边那片
///   留白一点落点；刻度会被读成数值，而这根条的长度不与数值成正比（见
///   `base_bar_min_width`），所以这张图上不能有刻度。精确的比较交给右边那两列数字，
///   色带只负责一眼看出长尾有多陡。
pub fn draw_bar_chart(
    config: &StatsConfig,
    title: &str,
    data: Vec<BarData>,
) -> Result<String, ChartError> {
    if data.is_empty() {
        return Err(ChartError::NoData);
    }

    let s = 2u32; // Scale factor

    // === 1. 预计算与布局参数 (Scaling) ===
    let padding = 24 * s;

    let colors = ColorScheme::default();
    // 纸面不用纯白：整屏 2000 px 的高亮白在手机上看久了刺眼，退半档到暖白，
    // 淡色轨道与横条反而更浮得出来。取设计系统的卡面（`scheme-manual` 的
    // surface），不另写一个色——图表与卡片得是同一张纸。与走势图同一张纸。
    let page_bg = colors.card_background;
    let ink = colors.text_primary; // 正文墨色：不写纯黑，与卡片同一个墨色
    let ink_soft = colors.text_secondary;
    let ink_faint = colors.readable_faint();

    // 内部尺寸也随之放大。`row_height` 是条本身的高度，`row_pitch` 是相邻两行的
    // 行距：之间留一道空档，条与条才分得开——紧挨着排会连成一整块三色板，
    // 排行读起来反而费劲。
    let row_height = 50 * s;
    let row_gap = 10 * s;
    let row_pitch = row_height + row_gap;
    let font_size = 30 * s;
    // 名次比昵称小两档：它只作次序参照，不该抢头像与名字的视线
    let rank_font_size = 22 * s;
    let rank_gap = 12 * s;
    let avatar_width = 50 * s;
    // 头像与横条之间留一道窄缝：圆头像直接贴着实色条会挤成一团。
    let avatar_gap = 6 * s;
    let gap_text = 14 * s;
    let text_inset = 10 * s; // 文字距条端/轨道端的内缩
    // 色带的圆角：约条高的两成，够软但仍是一根条，不至于圆成胶囊
    let bar_radius = 10 * s as i32;

    // 标题区：标题在最上，时间、榜单范围与合计并成一行小字跟在下面。
    // 把时间挪到标题之下是 iOS/Material 一类版式的通行做法——先看清这是什么，
    // 再看它是什么时候、多大范围的数据，层级比"小字压在标题头上"顺。
    let title_font_size = 32 * s;
    let meta_font_size = 18 * s;

    let meta_margin = 12 * s; // 标题与元信息行
    let title_margin = 24 * s; // 元信息行与列表
    let meta_y = padding + title_font_size + meta_margin;
    let top_area_height = meta_y + meta_font_size + title_margin;

    // 条长 = 最短长度 + 比例长度。最短那一截是给名字留的——名字写在条内，值为 1 的
    // 那一行也得写得下几个字，所以条不能从零开始。
    //
    // **代价是这根条的长度不与数值成正比**：值为 0 的条也占满轨道的 17.6%，值是榜首
    // 一半的条看着有榜首的 59% 长。诚实的读法是看条**尾**的位置——位置对数值是仿射的，
    // 所以两条的尾差与数值差成正比；不诚实的读法是量条长。这两种读法长得一模一样，
    // 所以色带上不画任何刻度：一画，读者就会去量长度。精确的比较在右边两列数字里。
    let base_bar_min_width = 150.0 * (s as f64);
    let base_bar_scale_width = 700.0 * (s as f64);
    let max_possible_bar_width = (base_bar_min_width + base_bar_scale_width) as u32;

    let max_val = data.iter().map(|d| d.value).max().unwrap_or(1).max(1);
    let total_val: i64 = data.iter().map(|d| d.value).sum();

    let font_family = get_font_family(config);
    let font_obj = (font_family, font_size).into_font();
    let pct_font_size = 20 * s;
    let pct_font_obj = (font_family, pct_font_size).into_font();
    let rank_font_obj = (font_family, rank_font_size).into_font();

    // 数值与占比写在哪儿，由 `stats.ranking_value_follows_bar` 决定。
    //
    // **跟着条尾（默认）**：值与条连着读，眼睛被条的颜色牵到条尾，答案就在那里，
    // 中间不用换一次视线。代价是二十个数字排成一串阶梯——那串阶梯本身就是条尾的
    // 轮廓，不算噪声。
    //
    // **排成右对齐的两列**：上下扫一眼就能比大小，画面也更齐整。代价是条尾什么都
    // 没有，读完颜色还得横着扫到画面最右边再回头认这是哪一行；一列二十个数字，
    // 认错行是常事。
    //
    // 两种都要留位：跟着条尾时按**最宽的那一串**在轨道右边留（任何一行都从自己的
    // 条尾起写，榜首那根条恰好顶到轨道尽头，所以 `text_inset + 最宽的一串` 就够）；
    // 排成列时按两列各自最宽的一行留。
    let follows_bar = config.ranking_value_follows_bar;
    let pct_gap = 8 * s;
    let mut formatted_counts: Vec<(String, String)> = Vec::new();
    let mut max_count_text_width = 0u32;
    let mut value_col_w = 0u32;
    let mut pct_col_w = 0u32;

    for item in data.iter() {
        let value_text = format_thousands(item.value);
        let pct_text = format_percent(item.value, total_val);

        let (vw, _) = font_obj.box_size(&value_text).unwrap_or((0, 0));
        let (pw, _) = pct_font_obj.box_size(&pct_text).unwrap_or((0, 0));
        max_count_text_width = max_count_text_width.max(vw + pct_gap + pw);
        value_col_w = value_col_w.max(vw);
        pct_col_w = pct_col_w.max(pw);
        formatted_counts.push((value_text, pct_text));
    }
    let numbers_width = if follows_bar {
        text_inset + max_count_text_width
    } else {
        gap_text + value_col_w + pct_gap + pct_col_w
    };

    let rank_texts: Vec<String> = (1..=data.len()).map(|r| r.to_string()).collect();
    let rank_col_w = rank_texts
        .iter()
        .map(|t| rank_font_obj.box_size(t).unwrap_or((0, 0)).0)
        .max()
        .unwrap_or(0);

    // 计算内容区域尺寸
    let content_width = rank_col_w
        + rank_gap
        + avatar_width
        + avatar_gap
        + max_possible_bar_width
        + numbers_width;
    // 最后一行的下面不留空档，否则底边会多出一段没有内容的留白。
    let content_height = data.len() as u32 * row_pitch - row_gap + top_area_height;

    // 计算画布尺寸 (增加四周边距)
    let canvas_width = content_width + padding * 2;
    let canvas_height = content_height + padding; // 底部留白

    // 各纵列的边界，从左到右一次算清
    let rank_right_x = (padding + rank_col_w) as i32;
    let avatar_x = rank_right_x + rank_gap as i32;
    let track_start_x = avatar_x + (avatar_width + avatar_gap) as i32;
    let track_end_x = track_start_x + max_possible_bar_width as i32;
    let value_right_x = track_end_x + (gap_text + value_col_w) as i32;
    let pct_right_x = value_right_x + (pct_gap + pct_col_w) as i32;

    // === 2. 绘图 ===
    let mut buffer = vec![0u8; (canvas_width * canvas_height * 3) as usize];
    {
        let root = BitMapBackend::with_buffer(&mut buffer, (canvas_width, canvas_height))
            .into_drawing_area();

        root.fill(&page_bg).map_err(|e| e.to_string())?;

        let title_style = get_font_with_color(config, title_font_size, &ink)
            .pos(Pos::new(HPos::Center, VPos::Top));
        root.draw_text(
            title,
            &title_style,
            (canvas_width as i32 / 2, padding as i32),
        )
        .map_err(|e| e.to_string())?;

        // 元信息行：榜单范围 + 合计（每行的百分比正是以它为基数）+ 出图时间。
        // 「·」在这套 CJK 字体里自带右侧空腔，两侧各补一个空格才等宽。
        let now_str = Local::now().format("%Y-%m-%d %H:%M").to_string();
        let meta = if data.len() > 1 {
            format!(
                "前 {} 名 · 合计 {} 次 · {}",
                data.len(),
                format_thousands(total_val),
                now_str
            )
        } else {
            format!("合计 {} 次 · {}", format_thousands(total_val), now_str)
        };
        let meta_style = get_font_with_color(config, meta_font_size, &ink_soft)
            .pos(Pos::new(HPos::Center, VPos::Top));
        root.draw_text(&meta, &meta_style, (canvas_width as i32 / 2, meta_y as i32))
            .map_err(|e| e.to_string())?;

        // 每行的行位、条长与这一行的四个色调只算一次，后面几趟共用。
        // 一行四色全部出自同一支色相：实色条 → 淡色轨道 → 条外的数值 → 占比，
        // 明度依次拉开，底淡字深，二十行也就是二十套同构的配色。
        // 数值与占比落在纸上（不再压着轨道），所以都按纸面量对比度。
        let rows: Vec<RowStyle> = data
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let y = top_area_height as i32 + (i as u32 * row_pitch) as i32;
                let ratio = item.value as f64 / max_val as f64;
                let bar_w = (base_bar_min_width + base_bar_scale_width * ratio).round() as i32;
                let bar = harmonize_theme(item.theme_color);
                let track = track_tone(bar);
                // 数字踩在什么底上，就按什么底量对比度：跟着条尾时压在淡色轨道上
                // （榜首那一行越过轨道落在纸上，纸更浅，一并够）；排成列时全在纸上。
                let ground = if follows_bar { track } else { page_bg };
                let value_ink = ensure_contrast(deep_tone(bar, 0.34), ground, 4.5);
                RowStyle {
                    y,
                    bar_end_x: track_start_x + bar_w,
                    bar,
                    track,
                    value_ink,
                    // 占比是次要信息：把数值的墨往纸里调一点，同一支色相退半档，
                    // 退到刚好还在正文阈值上为止
                    pct_ink: ensure_contrast(
                        mix_with_color(value_ink, ground, 0.62),
                        ground,
                        4.5,
                    ),
                }
            })
            .collect();

        // 头像底下垫一圈发丝细的暗边：浅色头像贴在暖白纸上边缘会化掉，
        // 一圈描边正好把圆形收住（iOS 给头像与应用图标描内边同理）。
        for (row, item) in rows.iter().zip(data.iter()) {
            if item.avatar_img.is_none() {
                continue;
            }
            root.draw(&Circle::new(
                (
                    avatar_x + (avatar_width / 2) as i32,
                    row.y + (row_height / 2) as i32,
                ),
                (avatar_width / 2) as i32 + s as i32,
                colors.grid_line.filled(),
            ))
            .map_err(|e| e.to_string())?;
        }

        // 第一趟：整条色带（淡色轨道）。先整条铺满再让实色条盖上去，四角的圆
        // 只需在这里做一次，实色条与轨道交界处自然是平切，不会露出豁口。
        for row in rows.iter() {
            draw_rounded_rect(
                &root,
                track_start_x,
                row.y,
                track_end_x,
                row.y + row_height as i32,
                bar_radius,
                row.track,
            )?;
        }

        // 刻度竖线与实色条的先后由 `ranking_grid_over_bars` 决定：默认构图线画在最上层，
        // 每行的色带都被刻满；关掉则实色条盖住它，每根条是完整的一块颜色。
        // 无论哪种，线只落在色带上、不越进行距的纸面，文字也都在最后一趟画。
        //
        // 间距沿用 100*s：自条的零点（最小条长处）起一格一道。右端那道收在圆角之前——
        // 它若落在圆角上，方头的线会戳出色带的轮廓；色带自己的圆角就是这张表的右边界，
        // 不必再描一道。
        let grid = ScaleGrid {
            first_x: track_start_x + base_bar_min_width as i32,
            end_x: track_end_x - bar_radius,
            step: 100 * s as i32,
            width: 2 * s as i32,
            row_height: row_height as i32,
        };
        let row_tops = || rows.iter().map(|row| row.y);

        // 第二趟：条与构图线，孰上孰下看配置
        if !config.ranking_grid_over_bars {
            grid.draw(&root, row_tops())?;
        }
        for row in rows.iter() {
            // 条没铺满整条色带时右端平切，与后面的轨道接成一条；铺满了（榜首）
            // 就连右边两个角一起圆，正好落在色带的轮廓上。
            if row.bar_end_x >= track_end_x {
                draw_rounded_rect(
                    &root,
                    track_start_x,
                    row.y,
                    track_end_x,
                    row.y + row_height as i32,
                    bar_radius,
                    row.bar,
                )?;
            } else {
                draw_left_accent_bar(
                    &root,
                    track_start_x,
                    row.y,
                    row.bar_end_x,
                    row.y + row_height as i32,
                    bar_radius,
                    row.bar,
                )?;
            }
        }

        if config.ranking_grid_over_bars {
            grid.draw(&root, row_tops())?;
        }

        // 第三趟：行内文字（名次、昵称、数值、占比），始终画在最上层
        for (i, item) in data.iter().enumerate() {
            let row = &rows[i];
            let (y, bar_end_x) = (row.y, row.bar_end_x);
            let start_x = track_start_x;
            let text_mid_y = y + (row_height / 2) as i32 + (2 * s as i32);

            // 名次：右对齐收在头像左边。前三名是奖牌色，固定不跟主题也不跟头像走。
            let rank_color = rank_ink(i + 1, ink_faint);
            let rank_style = get_font_with_color(config, rank_font_size, &rank_color)
                .pos(Pos::new(HPos::Right, VPos::Center));
            root.draw_text(&rank_texts[i], &rank_style, (rank_right_x, text_mid_y))
                .map_err(|e| e.to_string())?;

            // 昵称：一律写在实色条内，放不下就截断。名字挪到条外读起来反而费劲，
            // 短条那几行宁可截，也保持每行同一个视线落点。
            let name_color = get_contrast_color(row.bar);
            let max_name_width = (bar_end_x - start_x - 2 * text_inset as i32).max(0) as u32;
            let display_name = truncate_text_to_fit(&font_obj, &item.label, max_name_width);
            if !display_name.is_empty() {
                let name_style = get_font_with_color(config, font_size, &name_color)
                    .pos(Pos::new(HPos::Left, VPos::Center));
                root.draw_text(
                    &display_name,
                    &name_style,
                    (start_x + text_inset as i32, text_mid_y),
                )
                .map_err(|e| e.to_string())?;
            }

            // 数值与占比
            let (value_text, pct_text) = &formatted_counts[i];
            if follows_bar {
                let count_x = bar_end_x + text_inset as i32;
                let count_style = get_font_with_color(config, font_size, &row.value_ink)
                    .pos(Pos::new(HPos::Left, VPos::Center));
                root.draw_text(value_text, &count_style, (count_x, text_mid_y))
                    .map_err(|e| e.to_string())?;

                let (vw, _) = font_obj.box_size(value_text).unwrap_or((0, 0));
                let pct_style = get_font_with_color(config, pct_font_size, &row.pct_ink)
                    .pos(Pos::new(HPos::Left, VPos::Center));
                root.draw_text(
                    pct_text,
                    &pct_style,
                    (count_x + vw as i32 + pct_gap as i32, text_mid_y),
                )
                .map_err(|e| e.to_string())?;
            } else {
                let count_style = get_font_with_color(config, font_size, &row.value_ink)
                    .pos(Pos::new(HPos::Right, VPos::Center));
                root.draw_text(value_text, &count_style, (value_right_x, text_mid_y))
                    .map_err(|e| e.to_string())?;

                let pct_style = get_font_with_color(config, pct_font_size, &row.pct_ink)
                    .pos(Pos::new(HPos::Right, VPos::Center));
                root.draw_text(pct_text, &pct_style, (pct_right_x, text_mid_y))
                    .map_err(|e| e.to_string())?;
            }
        }

        // 7. 绘制图标徽章 (消息类型等无头像条目：主题色圆底 + 类型字符)
        for (i, item) in data.iter().enumerate() {
            if item.avatar_img.is_some() {
                continue;
            }
            let Some(icon_char) = item.icon_char.as_deref() else {
                continue;
            };

            let cx = avatar_x + (avatar_width / 2) as i32;
            let cy = rows[i].y + (row_height / 2) as i32;
            let radius = (avatar_width as f32 * 0.46) as i32;

            // 外圈淡色光晕 + 主题色圆底，用的是与这一行同一支色相
            let accent = rows[i].bar;
            root.draw(&Circle::new(
                (cx, cy),
                radius + (3 * s as i32),
                mix_with_white(accent, 0.35).filled(),
            ))
            .map_err(|e| e.to_string())?;
            root.draw(&Circle::new((cx, cy), radius, accent.filled()))
                .map_err(|e| e.to_string())?;

            // 圆内字符 (同色相的深调/浅调)
            let icon_color = get_contrast_color(accent);
            let icon_style = get_font_with_color(config, 24 * s, &icon_color)
                .pos(Pos::new(HPos::Center, VPos::Center));
            root.draw_text(icon_char, &icon_style, (cx, cy + (2 * s as i32)))
                .map_err(|e| e.to_string())?;
        }

        root.present().map_err(|e| e.to_string())?;
    }

    // === 3. 转换并叠加头像 ===
    let mut rgba_image = RgbaImage::new(canvas_width, canvas_height);
    for y in 0..canvas_height {
        for x in 0..canvas_width {
            let idx = ((y * canvas_width + x) * 3) as usize;
            let r = buffer[idx];
            let g = buffer[idx + 1];
            let b = buffer[idx + 2];
            rgba_image.put_pixel(x, y, Rgba([r, g, b, 255]));
        }
    }

    // 叠加头像：落在名次列右边的那一格里，与上面画的描边圈同心
    for (i, item) in data.iter().enumerate() {
        if let Some(avatar) = &item.avatar_img {
            let y_pos = top_area_height as i32 + (i as u32 * row_pitch) as i32;
            overlay_image(&mut rgba_image, avatar, avatar_x, y_pos);
        }
    }

    save_rgba_to_base64(rgba_image).map_err(ChartError::Failed)
}

/// 消息类型排行榜：标题区 + 构成条 + 竖排信息卡。
///
/// 消息类型本质上是「一个整体的构成」，而不是彼此独立的选手，所以在卡片列表之上
/// 先放一条分段构成条：一眼看到各类型占了多大一块，再往下看逐条的名次与数字。
/// 卡片内部按两行栅格排布——上行是名称与数值（同一条基线），下行是长度条；
/// 左起固定是「名次 + 类型色图标」，保证每张卡的视线落点一致。
///
/// 与发言/表情包的头像条形榜共用配色、字号层级与时间戳/标题写法，
/// 但不复用「满色横条塞字」的版式——那套是为头像行设计的。
///
/// **分层只用容器色与圆角，不加描边也不垫假投影。** 从前这里是「投影 + 描边 +
/// 卡面」三件套堆出来的层次，三样东西在说同一件事；而且那一套颜色是留在旧配色里
/// 的一份 Tailwind 石板灰，与全站的松绿没有关系——一张类型榜接在一张绿卡片后面，
/// 像是另一个人做的。现在三层各取一支令牌：相纸（surface-dim）→ 卡面（surface）
/// → 卡内的轨道（surface-container-high），明度一路往上走，层次自己就出来了。
pub fn draw_message_type_ranking(
    config: &StatsConfig,
    title: &str,
    data: Vec<BarData>,
) -> Result<String, ChartError> {
    if data.is_empty() {
        return Err(ChartError::NoData);
    }

    let s = 2u32;
    let colors = ColorScheme::default();
    let page_bg = colors.background;
    let card_face = colors.card_background;
    let track_bg = colors.container_high;
    let text_primary = colors.text_primary;
    let text_secondary = colors.text_secondary;
    let text_faint = colors.readable_faint();

    // —— 布局常量：一切间距都是 s 的整数倍，缩放后不会出现半像素毛边 ——
    let padding = 30 * s;
    let card_h = 96 * s;
    let card_gap = 14 * s;
    let card_radius = 22 * s;
    let rank_col_w = 34 * s;
    let rail_pad = 10 * s;
    let icon_size = 54 * s;
    let icon_radius = 16 * s;
    let icon_gap = 18 * s;
    let inner_pad = 22 * s;
    let bar_h = 8 * s;

    let title_font_size = 32 * s;
    let meta_font_size = 18 * s;
    let name_font_size = 27 * s;
    let value_font_size = 32 * s;
    let pct_font_size = 21 * s;
    let rank_font_size = 21 * s;
    let icon_font_size = 26 * s;

    let strip_h = 16 * s;
    let strip_gap = 4 * s;

    // 标题区：标题 → 元信息行（总量、种数、出图时间）→ 构成条。
    // 与排行榜同一套写法：标题在最上，小字跟在下面，不再把时间压在标题头上。
    let title_y = padding;
    let meta_y = title_y + title_font_size + 12 * s;
    let strip_y = meta_y + meta_font_size + 26 * s;
    let top_area = strip_y + strip_h + 28 * s;

    let canvas_width = 760 * s;
    let canvas_height = top_area
        + data.len() as u32 * card_h
        + data.len().saturating_sub(1) as u32 * card_gap
        + padding;

    let total_val: i64 = data.iter().map(|d| d.value).sum();
    let max_val = data.iter().map(|d| d.value).max().unwrap_or(1).max(1);

    let font_family = get_font_family(config);
    let name_font = (font_family, name_font_size).into_font();
    let value_font = (font_family, value_font_size).into_font();
    let pct_font = (font_family, pct_font_size).into_font();

    let card_x0 = padding as i32;
    let card_x1 = (canvas_width - padding) as i32;

    // 数值与占比各占一列固定宽度：占比写成 "72%" 还是 "<1%" 宽度不一样，跟着它
    // 排版会让数值那一列在行与行之间左右晃，一列对齐的数字是排版给出的顺序。
    let pct_gap = 14 * s;
    let texts: Vec<(String, String)> = data
        .iter()
        .map(|item| {
            (
                format_thousands(item.value),
                format_percent(item.value, total_val),
            )
        })
        .collect();
    let value_col_w = texts
        .iter()
        .map(|(v, _)| value_font.box_size(v).unwrap_or((0, 0)).0)
        .max()
        .unwrap_or(0);
    let pct_col_w = texts
        .iter()
        .map(|(_, p)| pct_font.box_size(p).unwrap_or((0, 0)).0)
        .max()
        .unwrap_or(0);

    let stats_right = card_x1 - inner_pad as i32;
    let value_right = stats_right - (pct_col_w + pct_gap) as i32;

    // 构成条各段宽度：先按占比分配，再把不足一格的段抬到最小可见宽度，
    // 多出来的像素从最宽的一段里扣回去，保证整条正好填满且不留缝。
    let strip_widths = allocate_strip_widths(
        &data,
        total_val,
        card_x1 - card_x0,
        strip_gap as i32,
        strip_h as i32,
    );

    let mut buffer = vec![0u8; (canvas_width * canvas_height * 3) as usize];
    {
        let root = BitMapBackend::with_buffer(&mut buffer, (canvas_width, canvas_height))
            .into_drawing_area();
        root.fill(&page_bg).map_err(|e| e.to_string())?;

        // === 标题区 ===
        let title_style = get_font_with_color(config, title_font_size, &text_primary)
            .pos(Pos::new(HPos::Center, VPos::Top));
        root.draw_text(
            title,
            &title_style,
            (canvas_width as i32 / 2, title_y as i32),
        )
        .map_err(|e| e.to_string())?;

        // 「·」在这套 CJK 字体里自带右侧空腔，两侧各补一个空格才等宽
        let now_str = Local::now().format("%Y-%m-%d %H:%M").to_string();
        let meta = format!(
            "共 {} 条消息 · {} 种类型 · {}",
            format_thousands(total_val),
            data.len(),
            now_str
        );
        let meta_style = get_font_with_color(config, meta_font_size, &text_secondary)
            .pos(Pos::new(HPos::Center, VPos::Top));
        root.draw_text(&meta, &meta_style, (canvas_width as i32 / 2, meta_y as i32))
            .map_err(|e| e.to_string())?;

        // === 构成条：整体占比的一眼概览 ===
        let strip_radius = (strip_h / 2) as i32;
        let mut seg_x = card_x0;
        for (item, width) in data.iter().zip(strip_widths.iter()) {
            draw_rounded_rect(
                &root,
                seg_x,
                strip_y as i32,
                seg_x + width,
                (strip_y + strip_h) as i32,
                strip_radius,
                item.theme_color,
            )?;
            seg_x += width + strip_gap as i32;
        }

        // === 信息卡 ===
        for (i, item) in data.iter().enumerate() {
            let y0 = (top_area + i as u32 * (card_h + card_gap)) as i32;
            let y1 = y0 + card_h as i32;
            let cy = y0 + (card_h / 2) as i32;
            let accent = item.theme_color;

            // 卡面：每张一样。层次靠卡面与相纸的明度差 + 圆角，不描边、不垫影子。
            //
            // 榜首从前另外染一层类型色——可是「文本」的类型色本来就是近灰的
            // on-surface-variant，染出来的不是更醒目，是更脏，那张卡看着像没画完。
            // 榜首已经有两个通道在说它是榜首：最长的那根条，和金色的「1」。
            draw_rounded_rect(
                &root,
                card_x0,
                y0,
                card_x1,
                y1,
                card_radius as i32,
                card_face,
            )?;

            // 名次：卡片左起的第一段。前三名是固定的奖牌色——名次的颜色本身就是
            // 含义，从前这里跟着类型色走，于是「第 1 名」在每张榜上都是另一个颜色。
            let rank_color = rank_ink(i + 1, text_faint);
            let rank_style = get_font_with_color(config, rank_font_size, &rank_color)
                .pos(Pos::new(HPos::Center, VPos::Center));
            root.draw_text(
                &(i + 1).to_string(),
                &rank_style,
                (
                    card_x0 + (rail_pad + rank_col_w / 2) as i32,
                    cy + (2 * s) as i32,
                ),
            )
            .map_err(|e| e.to_string())?;

            // 类型图标：淡色底 + 同色字，比满色底更耐看，也不抢数值的视线。
            // 底色从容器那一档往类型色里调，不是往白里调——往白里调会在暖白卡面上
            // 淡到看不出有个底板（「文本」那一格尤其明显，它的色本来就接近灰）。
            let icon_x0 = card_x0 + (rail_pad + rank_col_w) as i32;
            let icon_y0 = cy - (icon_size / 2) as i32;
            let icon_x1 = icon_x0 + icon_size as i32;
            let icon_fill = mix_with_color(accent, colors.container, 0.20);
            draw_rounded_rect(
                &root,
                icon_x0,
                icon_y0,
                icon_x1,
                icon_y0 + icon_size as i32,
                icon_radius as i32,
                icon_fill,
            )?;
            if let Some(icon_char) = item.icon_char.as_deref() {
                let icon_ink = ensure_contrast(accent, icon_fill, 4.5);
                let icon_style = get_font_with_color(config, icon_font_size, &icon_ink)
                    .pos(Pos::new(HPos::Center, VPos::Center));
                root.draw_text(
                    icon_char,
                    &icon_style,
                    (icon_x0 + (icon_size / 2) as i32, cy + (2 * s) as i32),
                )
                .map_err(|e| e.to_string())?;
            }

            // 上行右侧：数值（主色大字）+ 占比（次级小字），各自右对齐收在自己那一列
            let (value_text, pct_text) = &texts[i];
            let top_row_y = cy - (13 * s) as i32;
            let pct_style = get_font_with_color(config, pct_font_size, &text_secondary)
                .pos(Pos::new(HPos::Right, VPos::Center));
            root.draw_text(pct_text, &pct_style, (stats_right, top_row_y))
                .map_err(|e| e.to_string())?;
            let value_style = get_font_with_color(config, value_font_size, &text_primary)
                .pos(Pos::new(HPos::Right, VPos::Center));
            root.draw_text(value_text, &value_style, (value_right, top_row_y))
                .map_err(|e| e.to_string())?;

            // 上行左侧：类型名，与数值同基线；过长按可用宽度截断
            let name_x = icon_x1 + icon_gap as i32;
            let name_max_w =
                (value_right - value_col_w as i32 - (20 * s) as i32 - name_x).max(0) as u32;
            let display_name = truncate_text_to_fit(&name_font, &item.label, name_max_w);
            if !display_name.is_empty() {
                let name_style = get_font_with_color(config, name_font_size, &text_primary)
                    .pos(Pos::new(HPos::Left, VPos::Center));
                root.draw_text(&display_name, &name_style, (name_x, top_row_y))
                    .map_err(|e| e.to_string())?;
            }

            // 下行：相对榜首的长度条，贯通名称到数值的整个宽度
            let bar_x0 = name_x;
            let bar_x1 = stats_right;
            let bar_y0 = cy + (17 * s) as i32;
            let bar_y1 = bar_y0 + bar_h as i32;
            let bar_radius = (bar_h / 2) as i32;
            draw_rounded_rect(&root, bar_x0, bar_y0, bar_x1, bar_y1, bar_radius, track_bg)?;

            if item.value > 0 {
                // 至少画成一个圆点，否则量级极小的类型在条上会完全消失
                let ratio = (item.value as f64 / max_val as f64).clamp(0.0, 1.0);
                let fill_w = ((bar_x1 - bar_x0) as f64 * ratio).round() as i32;
                let fill_x1 = (bar_x0 + fill_w.max(bar_h as i32)).min(bar_x1);
                draw_rounded_rect(&root, bar_x0, bar_y0, fill_x1, bar_y1, bar_radius, accent)?;
            }
        }

        root.present().map_err(|e| e.to_string())?;
    }

    let mut rgba_image = RgbaImage::new(canvas_width, canvas_height);
    for y in 0..canvas_height {
        for x in 0..canvas_width {
            let idx = ((y * canvas_width + x) * 3) as usize;
            rgba_image.put_pixel(
                x,
                y,
                Rgba([buffer[idx], buffer[idx + 1], buffer[idx + 2], 255]),
            );
        }
    }

    save_rgba_to_base64(rgba_image).map_err(ChartError::Failed)
}

/// 构成条的分段宽度。按占比切分总宽度（已扣除段间空隙），再把小到看不见的段
/// 抬到 `min_width`，多出来的像素从当前最宽的段里逐格扣回，最后把舍入误差补给
/// 最宽的一段——这样整条始终正好填满，不会因为四舍五入在右端留下一道缝。
fn allocate_strip_widths(
    data: &[BarData],
    total_val: i64,
    strip_width: i32,
    gap: i32,
    min_width: i32,
) -> Vec<i32> {
    let count = data.len() as i32;
    let usable = (strip_width - gap * (count - 1)).max(count * min_width);
    if total_val <= 0 {
        let even = usable / count;
        return (0..count).map(|_| even).collect();
    }

    let mut widths: Vec<i32> = data
        .iter()
        .map(|d| {
            ((usable as f64 * d.value as f64 / total_val as f64).round() as i32).max(min_width)
        })
        .collect();

    let widest = |widths: &[i32]| {
        widths
            .iter()
            .enumerate()
            .max_by_key(|(_, w)| **w)
            .map(|(i, _)| i)
            .unwrap_or(0)
    };

    let mut sum: i32 = widths.iter().sum();
    while sum > usable {
        let i = widest(&widths);
        if widths[i] <= min_width {
            break;
        }
        widths[i] -= 1;
        sum -= 1;
    }
    if sum < usable {
        let i = widest(&widths);
        widths[i] += usable - sum;
    }
    widths
}

// ================= 走势图 (与排行榜统一的手绘风格) =================

/// 纵轴的步长与格数：取 1/2/2.5/5×10^k 里能把 `max_val` 装进 2—5 格、
/// 且**顶格最低**的那一档；一样低就取格子多的那一档，网格细一点。
///
/// 从前是先把上限抬到 `max × 1.05`，再拿抬高后的值反算格数——195 会画成 0—250，
/// 顶上白白空掉两成的高度，真正有起伏的那一段反而被压扁。现在按 `ceil(max/step)`
/// 定格数，同一组数画到 0—200，折线铺满整个绘图区。
///
/// 步长只取整数：刻度文案是整数，2.5 这样的步长取整之后会写出两个一样的刻度。
fn nice_axis(max_val: i64) -> (f64, usize) {
    let max = max_val.max(1) as f64;
    let mut best: Option<(f64, usize)> = None;
    let start = (max / 5.0).log10().floor() as i32 - 1;
    for k in start..start + 6 {
        let base = 10f64.powi(k);
        for m in [1.0, 2.0, 2.5, 5.0] {
            let step = m * base;
            if step < 1.0 || step.fract() != 0.0 {
                continue;
            }
            let steps = (max / step).ceil() as usize;
            if !(2..=5).contains(&steps) {
                continue;
            }
            let top = step * steps as f64;
            let better = match best {
                None => true,
                Some((bs, bn)) => {
                    let btop = bs * bn as f64;
                    top < btop - f64::EPSILON || ((top - btop).abs() < f64::EPSILON && steps > bn)
                }
            };
            if better {
                best = Some((step, steps));
            }
        }
    }
    // 只有 max_val 小到 1 才落到这里：0—2 两格
    best.unwrap_or((1.0, 2))
}

/// Y 轴刻度文本：过万缩写为 "x.x万"，其余直接显示整数
fn format_y_label(v: f64) -> String {
    let vi = v.round() as i64;
    if vi >= 10000 {
        let t = format!("{:.1}", vi as f64 / 10000.0);
        let t = t.strip_suffix(".0").unwrap_or(&t).to_string();
        format!("{}万", t)
    } else {
        vi.to_string()
    }
}

/// X 轴标签：去掉日期前缀中的年份部分 (如 "2026-08-01" -> "08-01")
fn short_x_label(label: &str) -> String {
    if label.is_ascii() && label.len() >= 10 && label.as_bytes()[4] == b'-' {
        label[5..].to_string()
    } else {
        label.to_string()
    }
}

fn timeline(series: &[SeriesData]) -> Vec<String> {
    series
        .iter()
        .flat_map(|s| s.points.iter().map(|p| p.label.clone()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// 绘制折线图 (支持单线/多线)。
/// 与排行榜柱状图共用同一套视觉规范：白底、灰阶时间戳/标题、统一字体字号与边距、
/// 浅色网格线、白描边圆点数据点，配色取自系列颜色（消息类型与排行榜共用调色板）。
pub fn draw_line_chart(
    config: &StatsConfig,
    title: &str,
    series_list: Vec<SeriesData>,
) -> Result<String, ChartError> {
    if series_list.is_empty() {
        return Err(ChartError::NoData);
    }

    let s = 2u32;
    if !(480..=2400).contains(&config.width) || !(360..=2400).contains(&config.height) {
        return Err(ChartError::Failed(
            "走势图尺寸应为宽 480—2400、高 360—2400 像素；改 stats.width 与 stats.height".into(),
        ));
    }
    let width = config.width * s;
    let height = config.height * s;

    let colors = ColorScheme::default();
    // 与排行榜、与卡片同一张纸（见 ColorScheme::default 的说明）
    let page_bg = colors.card_background;
    let multi = series_list.len() > 1;

    // 时间标签来自固定宽度的日期或时刻，排序后稀疏系列不会折回到较早的日期。
    let x_labels = timeline(&series_list);
    let label_index: HashMap<&str, usize> = x_labels
        .iter()
        .enumerate()
        .map(|(i, label)| (label.as_str(), i))
        .collect();
    let point_count = x_labels.len().max(1);

    let max_val = series_list
        .iter()
        .flat_map(|sr| sr.points.iter().map(|p| p.value))
        .max()
        .unwrap_or(0);
    let (step, steps) = nice_axis(max_val);
    let y_max = step * steps as f64;

    // === 2. 布局 (与柱状图一致的字号与边距) ===
    let padding = 24 * s;
    let meta_font_size = 18 * s;
    let title_font_size = 32 * s;
    let axis_font_size = 20 * s;
    let legend_font_size = 20 * s;
    let gap = 10 * s;

    let font_family = get_font_family(config);
    let axis_font = (font_family, axis_font_size).into_font();
    let legend_font = (font_family, legend_font_size).into_font();

    // Y 轴标签宽度
    let mut y_label_w = 0u32;
    for i in 0..=steps {
        let text = format_y_label(step * i as f64);
        let (w, _) = axis_font.box_size(&text).unwrap_or((0, 0));
        y_label_w = y_label_w.max(w);
    }

    // 标题在最上，副标题跟在下面，与排行榜、类型卡同一套层级。
    // 副标题这一层放的是「范围与规模」：跨了多久、一共多少次、什么时候出的图。
    // 从前这里只有一个时间戳——同一条消息里三张图，两张写着范围，一张什么都不写。
    let title_y = padding;
    let meta_y = title_y + title_font_size + gap;

    // 时间粒度从标签写法认出来：`%H:%M` 是按小时聚的，`%Y-%m-%d` 是按天聚的
    let hourly = x_labels
        .first()
        .is_some_and(|l| l.len() == 5 && l.contains(':'));
    let span_unit = if hourly { "小时" } else { "天" };
    let grand_total: i64 = series_list
        .iter()
        .flat_map(|sr| sr.points.iter().map(|p| p.value))
        .sum();
    let now_str = Local::now().format("%Y-%m-%d %H:%M").to_string();
    let meta = format!(
        "共 {} {} · 合计 {} 次 · {}",
        x_labels.len(),
        span_unit,
        format_thousands(grand_total),
        now_str
    );

    // === 3. 图例布局 (多系列时)：圆点 + 名称，水平排列，超宽自动换行 ===
    let dot_r = 6 * s;
    let item_gap = 24 * s;
    let legend_row_h = 32 * s;
    let mut legend_rows: Vec<Vec<(usize, u32)>> = Vec::new();
    let mut legend_h = 0u32;

    if multi {
        let avail = width.saturating_sub(2 * padding);
        let mut row: Vec<(usize, u32)> = Vec::new();
        let mut row_w = 0u32;

        for (idx, series) in series_list.iter().enumerate() {
            let (tw, _) = legend_font.box_size(&series.name).unwrap_or((0, 0));
            let item_w = 2 * dot_r + (8 * s) + tw;
            let new_w = if row.is_empty() {
                item_w
            } else {
                row_w + item_gap + item_w
            };
            if !row.is_empty() && new_w > avail {
                legend_rows.push(std::mem::take(&mut row));
                row_w = item_w;
            } else {
                row_w = new_w;
            }
            row.push((idx, item_w));
        }
        if !row.is_empty() {
            legend_rows.push(row);
        }
        legend_h = legend_rows.len() as u32 * legend_row_h;
    }

    let legend_y = meta_y + meta_font_size + gap;
    // 单系列会在最高点上方标一个数：纵轴收紧之后峰值常常顶到第一条网格线，
    // 这里按标注自己的高度先把位置留出来，免得它撞到副标题那一行。
    let peak_headroom = if multi { 0 } else { axis_font_size + 14 * s };
    let chart_top = if multi {
        legend_y + legend_h + (12 * s)
    } else {
        meta_y + meta_font_size + (24 * s) + peak_headroom
    };
    let x_label_area = axis_font_size + 14 * s;
    let chart_bottom = height
        .saturating_sub(padding + x_label_area)
        .max(chart_top + 40 * s);
    let chart_left = padding + y_label_w + (14 * s);
    let chart_right = width.saturating_sub(padding).max(chart_left + 40 * s);

    let chart_w = (chart_right - chart_left) as f64;
    let chart_h = (chart_bottom - chart_top) as f64;

    // X/Y 坐标换算
    let x_pos = |i: usize| chart_left as f64 + chart_w * ((i as f64 + 0.5) / point_count as f64);
    let y_pos = |v: i64| chart_bottom as f64 - chart_h * (v as f64 / y_max).clamp(0.0, 1.0);

    // === 4. 绘制 ===
    let mut buffer = vec![0u8; (width * height * 3) as usize];
    {
        let root = BitMapBackend::with_buffer(&mut buffer, (width, height)).into_drawing_area();

        // 与排行榜同一张暖白纸，两张图连着看不会一亮一暗
        root.fill(&page_bg).map_err(|e| e.to_string())?;

        // 4.1 标题 + 出图时间 (与柱状图一致)
        let title_style = get_font(config, title_font_size).pos(Pos::new(HPos::Center, VPos::Top));
        root.draw_text(title, &title_style, (width as i32 / 2, title_y as i32))
            .map_err(|e| e.to_string())?;

        let meta_style = get_font(config, meta_font_size)
            .pos(Pos::new(HPos::Center, VPos::Top))
            .color(&colors.text_secondary);
        root.draw_text(&meta, &meta_style, (width as i32 / 2, meta_y as i32))
            .map_err(|e| e.to_string())?;

        // 4.2 图例
        for (row_i, row) in legend_rows.iter().enumerate() {
            let row_total: u32 =
                row.iter().map(|(_, w)| *w).sum::<u32>() + item_gap * (row.len() as u32 - 1);
            let mut x = (width.saturating_sub(row_total)) as i32 / 2;
            let row_mid_y = legend_y + row_i as u32 * legend_row_h + legend_row_h / 2;

            for &(idx, item_w) in row {
                let color = series_list[idx].color;
                root.draw(&Circle::new(
                    (x + dot_r as i32, row_mid_y as i32),
                    dot_r as i32,
                    color.filled(),
                ))
                .map_err(|e| e.to_string())?;
                let legend_style =
                    get_font_with_color(config, legend_font_size, &colors.text_primary)
                        .pos(Pos::new(HPos::Left, VPos::Center));
                root.draw_text(
                    &series_list[idx].name,
                    &legend_style,
                    (
                        x + 2 * dot_r as i32 + (8 * s as i32),
                        row_mid_y as i32 + (2 * s as i32),
                    ),
                )
                .map_err(|e| e.to_string())?;
                x += item_w as i32 + item_gap as i32;
            }
        }

        // 4.3 水平网格线 + Y 轴刻度 (基线加深，与柱状图竖线装饰同色系)
        let y_label_style = get_font_with_color(config, axis_font_size, &colors.text_secondary)
            .pos(Pos::new(HPos::Right, VPos::Center));

        for i in 0..=steps {
            let v = step * i as f64;
            let y = y_pos(v.round() as i64);
            // 零线用描边那一档，其余用更淡的 outline-variant：基线是这张图的地面，
            // 它比网格重一级，但两者都还是「图形元素」，取的都是系统的描边色。
            let line_color = if i == 0 {
                colors.outline
            } else {
                colors.grid_line
            };
            root.draw(&PathElement::new(
                vec![
                    (chart_left as i32, y as i32),
                    (chart_right as i32, y as i32),
                ],
                line_color.stroke_width(s),
            ))
            .map_err(|e| e.to_string())?;

            let label = format_y_label(v);
            root.draw_text(
                &label,
                &y_label_style,
                ((chart_left - (10 * s)) as i32, y as i32),
            )
            .map_err(|e| e.to_string())?;
        }

        // 4.4 X 轴标签 (过多时自动抽稀)
        let x_label_style = get_font_with_color(config, axis_font_size, &colors.text_secondary)
            .pos(Pos::new(HPos::Center, VPos::Top));
        let label_every = ((point_count as f64) / 10.0).ceil().max(1.0) as usize;

        for (i, label) in x_labels.iter().enumerate() {
            if i % label_every != 0 && i != point_count - 1 {
                continue;
            }
            let text = short_x_label(label);
            root.draw_text(
                &text,
                &x_label_style,
                (x_pos(i) as i32, (chart_bottom + (12 * s)) as i32),
            )
            .map_err(|e| e.to_string())?;
        }

        // 4.5 数据系列：面积填充(单系列) + 折线 + 白描边圆点
        for series in &series_list {
            let color = series.color;

            // 将系列数据点映射到统一 X 轴位置
            let pts: Vec<(f64, f64)> = series
                .points
                .iter()
                .filter_map(|p| {
                    label_index
                        .get(p.label.as_str())
                        .map(|&i| (x_pos(i), y_pos(p.value)))
                })
                .collect();
            if pts.is_empty() {
                continue;
            }

            // 面积填充 (仅单系列，避免多系列叠加混淆)
            if !multi {
                let mut poly: Vec<(i32, i32)> = pts
                    .iter()
                    .map(|&(x, y)| (x.round() as i32, y.round() as i32))
                    .collect();
                poly.push((pts[pts.len() - 1].0.round() as i32, chart_bottom as i32));
                poly.push((pts[0].0.round() as i32, chart_bottom as i32));
                root.draw(&Polygon::new(
                    poly,
                    RGBAColor(color.0, color.1, color.2, 0.13).filled(),
                ))
                .map_err(|e| e.to_string())?;
            }

            // 折线
            let line_pts: Vec<(i32, i32)> = pts
                .iter()
                .map(|&(x, y)| (x.round() as i32, y.round() as i32))
                .collect();
            root.draw(&PathElement::new(line_pts, color.stroke_width(3 * s)))
                .map_err(|e| e.to_string())?;

            // 数据点：纸色底圆 + 主题色内圆。底圆取的是这张纸本身的颜色，不是纯白——
            // 纸是暖白的，压一圈纯白上去等于在每个点周围点了一圈更亮的光斑。
            for &(x, y) in &pts {
                let (xi, yi) = (x.round() as i32, y.round() as i32);
                root.draw(&Circle::new((xi, yi), (6 * s) as i32, page_bg.filled()))
                    .map_err(|e| e.to_string())?;
                root.draw(&Circle::new((xi, yi), (4 * s) as i32, color.filled()))
                    .map_err(|e| e.to_string())?;
            }
        }

        // 4.6 峰值标注 (仅单系列：在最高点上方标注数值)
        if !multi
            && max_val > 0
            && let Some(peak) = series_list[0].points.iter().max_by_key(|p| p.value)
            && let Some(&i) = label_index.get(peak.label.as_str())
        {
            let px = x_pos(i);
            let py = y_pos(peak.value);
            let text = format_thousands(peak.value);
            // 峰值是这条线的数，墨色就取这支色相的深调——与排行榜里「条外的数值」
            // 同一套做法，读者一眼知道这个数属于哪根线
            let peak_ink = ensure_contrast(deep_tone(series_list[0].color, 0.34), page_bg, 4.5);
            let peak_style = get_font_with_color(config, axis_font_size, &peak_ink)
                .pos(Pos::new(HPos::Center, VPos::Bottom));
            root.draw_text(
                &text,
                &peak_style,
                (px.round() as i32, (py - (10 * s) as f64).round() as i32),
            )
            .map_err(|e| e.to_string())?;
        }

        root.present().map_err(|e| e.to_string())?;
    }

    // === 5. RGB -> RGBA 并编码 ===
    let mut rgba_image = RgbaImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 3) as usize;
            let r = buffer[idx];
            let g = buffer[idx + 1];
            let b = buffer[idx + 2];
            rgba_image.put_pixel(x, y, Rgba([r, g, b, 255]));
        }
    }

    save_rgba_to_base64(rgba_image).map_err(ChartError::Failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::stats::chart::data_loader::message_type_style;
    use crate::plugins::stats::chart::utils::{
        avatar_theme_color, from_hsl_for_test, get_average_color, to_hsl_for_test,
    };

    fn sample(label: &str, value: i64) -> BarData {
        let (color, icon) = message_type_style(label);
        BarData {
            label: label.to_string(),
            value,
            user_id: None,
            avatar_url: None,
            avatar_img: None,
            theme_color: color,
            icon_char: (icon != "?").then(|| icon.to_string()),
        }
    }

    #[test]
    fn sparse_series_share_a_chronological_axis() {
        let series = |days: &[&str]| SeriesData {
            name: "测试".into(),
            color: BLACK,
            points: days
                .iter()
                .map(|d| super::super::data_loader::ChartDataPoint {
                    label: d.to_string(),
                    value: 1,
                })
                .collect(),
        };
        assert_eq!(
            timeline(&[
                series(&["2026-09-01", "2026-09-03"]),
                series(&["2026-09-02"])
            ]),
            ["2026-09-01", "2026-09-02", "2026-09-03"]
        );
        let (step, count) = nice_axis(195);
        assert!(step * count as f64 >= 195.0 && step * count as f64 <= 250.0);
        assert_eq!(short_x_label("中文标题-日期"), "中文标题-日期");
    }

    /// 量级差三个数量级的两行也要画得出来：最短的那根条落在 `base_bar_min_width` 上，
    /// 不会缩成一条看不见的线，也不会把名字挤没。两种数字版式都跑一遍。
    #[test]
    fn a_thousand_fold_gap_still_renders_both_rows() {
        for follows in [true, false] {
            let config = StatsConfig {
                ranking_value_follows_bar: follows,
                ..StatsConfig::default()
            };
            let data = vec![sample("文本", 8_120), sample("图片", 3)];
            let out = draw_bar_chart(&config, "本群今日发言排行榜", data)
                .expect("悬殊的两行也应当能渲染");
            assert!(out.starts_with("base64://"));
        }
    }

    #[test]
    fn strip_segments_fill_the_width_exactly() {
        let data = vec![
            sample("文本", 8_120),
            sample("图片", 2_004),
            sample("表情", 1),
        ];
        let total: i64 = data.iter().map(|d| d.value).sum();
        let (width, gap, min) = (1400, 8, 16);
        let widths = allocate_strip_widths(&data, total, width, gap, min);

        assert_eq!(widths.len(), 3);
        let laid_out: i32 = widths.iter().sum::<i32>() + gap * (data.len() as i32 - 1);
        assert_eq!(laid_out, width, "分段加空隙应正好铺满整条");
        assert!(widths.iter().all(|w| *w >= min), "极小占比也要看得见");
        assert!(widths[0] > widths[1] && widths[1] > widths[2]);
    }

    #[test]
    fn strip_handles_single_and_empty_totals() {
        let one = vec![sample("文本", 5)];
        assert_eq!(allocate_strip_widths(&one, 5, 600, 8, 16), vec![600]);

        // 全为 0 时不做除零，均分即可
        let zeros = vec![sample("文本", 0), sample("图片", 0)];
        let widths = allocate_strip_widths(&zeros, 0, 600, 8, 16);
        assert_eq!(widths, vec![296, 296]);
    }

    #[test]
    fn message_type_ranking_renders_a_png() {
        let config = StatsConfig::default();
        let data = vec![
            sample("文本", 8_120),
            sample("图片", 2_004),
            sample("动画表情", 947),
            sample("表情", 133),
            sample("语音", 21),
            sample("视频", 2),
        ];
        let out = draw_message_type_ranking(&config, "本群今日消息类型排行榜", data)
            .expect("消息类型排行榜应当能渲染");
        save_preview(&out, "ACUMEN_CHART_PREVIEW");
    }

    #[test]
    fn bar_chart_still_renders_with_the_shared_number_formatting() {
        let config = StatsConfig::default();
        let data = vec![
            sample("每天读一点书的阿青", 12_345),
            sample("喜欢散步和咖啡的朋友", 678),
            sample("很长很长的群昵称依然保留清晰的阅读位置", 3),
        ];
        let out =
            draw_bar_chart(&config, "本群今日发言排行榜", data).expect("发言排行榜应当能渲染");
        save_preview(&out, "ACUMEN_CHART_PREVIEW_BAR");
    }

    /// 圆形假头像：用于本地样张，颜色与线上「头像均色」取到的调子接近。
    fn fake_avatar(color: (u8, u8, u8)) -> image::RgbaImage {
        let size = 100u32;
        let mut img = image::RgbaImage::new(size, size);
        let center = size as f32 / 2.0;
        for y in 0..size {
            for x in 0..size {
                let (dx, dy) = (x as f32 - center + 0.5, y as f32 - center + 0.5);
                if (dx * dx + dy * dy).sqrt() <= center - 1.0 {
                    let shade = 1.0 - (y as f32 / size as f32) * 0.25;
                    img.put_pixel(
                        x,
                        y,
                        Rgba([
                            (color.0 as f32 * shade) as u8,
                            (color.1 as f32 * shade) as u8,
                            (color.2 as f32 * shade) as u8,
                            255,
                        ]),
                    );
                }
            }
        }
        img
    }

    /// 线上最常见的一张榜：20 人、长尾陡峭、昵称长短不一、头像色各异。
    /// 三行的小样看不出短条那几行的排版问题，样张要按真实规模来看。
    #[test]
    #[ignore = "生成本地排行榜样张"]
    fn dump_full_ranking_sample() {
        let Ok(dir) = std::env::var("STATS_CARD_DUMP") else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();

        let names = [
            "★ 每天读一点书的阿青",
            "夜航船",
            "把咖啡当水喝的老周同学",
            "琉璃",
            "不想上班只想睡觉的猫猫头",
            "Mira",
            "山有木兮",
            "阿吱",
            "很长很长的群昵称依然保留清晰的阅读位置",
            "清风徐来",
            "十七",
            "写代码的小陈",
            "月半",
            "秋刀鱼的滋味",
            "Tom",
            "南山南",
            "一只会摸鱼的水母",
            "小满",
            "晚来天欲雪",
            "卖火柴的小女孩",
        ];
        // 前八种是常见的中间调，后四种是极端：雪白的自拍、近黑的剪影、
        // 荧光的二次元图、以及一张灰度头像——收调之后这四行也要站得住。
        let tints = [
            (86, 104, 128),
            (150, 120, 96),
            (92, 126, 110),
            (128, 106, 140),
            (176, 152, 104),
            (104, 132, 156),
            (140, 112, 112),
            (96, 118, 96),
            (238, 234, 228),
            (26, 24, 30),
            (255, 64, 160),
            (140, 140, 140),
        ];
        let data: Vec<BarData> = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let value = (4200.0 * 0.72f64.powi(i as i32)).round() as i64 + 1;
                BarData {
                    label: name.to_string(),
                    value,
                    user_id: Some(10_000 + i as i64),
                    avatar_url: None,
                    avatar_img: Some(fake_avatar(tints[i % tints.len()])),
                    theme_color: {
                        let t = tints[i % tints.len()];
                        RGBColor(t.0, t.1, t.2)
                    },
                    icon_char: None,
                }
            })
            .collect();

        let out = draw_bar_chart(&sample_config(), "本群今日发言排行榜", data)
            .expect("排行榜样张应当能渲染");
        dump_png(&dir, "ranking-full", &out);
    }


    /// 用磁盘上真实的头像缓存出一张榜。
    ///
    /// 合成样张里的假头像是一块块纯色，彩度比真头像高得多；真头像求完平均是一片
    /// 洗过的灰调（本机 142 张的中位彩度只有 0.08）。「读不出色相就退回回退色」那条
    /// 门槛一旦定高，合成样张上一点看不出来，线上却会大片变成同一条绿。
    /// 取色相关的改动都要看这一张。
    ///
    /// `STATS_AVATAR_CACHE=<目录> STATS_CARD_DUMP=<目录> cargo test
    /// dump_real_avatar_ranking -- --ignored`
    #[test]
    #[ignore = "用真实头像缓存出样张"]
    fn dump_real_avatar_ranking() {
        let (Ok(dir), Ok(cache)) = (
            std::env::var("STATS_CARD_DUMP"),
            std::env::var("STATS_AVATAR_CACHE"),
        ) else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();

        let mut files: Vec<_> = std::fs::read_dir(&cache)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        files.sort();

        let data: Vec<BarData> = files
            .iter()
            .filter_map(|path| image::open(path).ok())
            .take(20)
            .enumerate()
            .map(|(i, img)| {
                let img = img.to_rgba8();
                BarData {
                    label: format!("群友 {}", i + 1),
                    value: (4200.0 * 0.78f64.powi(i as i32)).round() as i64 + 1,
                    user_id: Some(10_000 + i as i64),
                    avatar_url: None,
                    theme_color: avatar_theme_color(&img),
                    avatar_img: Some(img),
                    icon_char: None,
                }
            })
            .collect();
        assert!(!data.is_empty(), "{cache} 里没有可读的头像");

        for (follows, name) in [
            (true, "ranking-real-avatars"),
            (false, "ranking-real-avatars-columns"),
        ] {
            let config = StatsConfig {
                ranking_value_follows_bar: follows,
                ..sample_config()
            };
            let rows: Vec<BarData> = data
                .iter()
                .map(|d| BarData {
                    label: d.label.clone(),
                    value: d.value,
                    user_id: d.user_id,
                    avatar_url: d.avatar_url.clone(),
                    avatar_img: d.avatar_img.clone(),
                    theme_color: d.theme_color,
                    icon_char: d.icon_char.clone(),
                })
                .collect();
            let out = draw_bar_chart(&config, "本群今日发言排行榜", rows)
                .expect("真头像样张应当能渲染");
            dump_png(&dir, name, &out);
        }
    }



    /// 三版取色的对照表：一行一个真头像，右边挨着摆原版、当前、提议三块色。
    ///
    /// 规范里那句「同一份内容做两版摆在一起看，输了就改规范」。取色这种事讲不清楚，
    /// 得出图；而且要画**最终**的条色，不是中间值——`draw_bar_chart` 自己会再收一次调，
    /// 从外面塞一个已经收过调的颜色进去，量到的就不是那一版真正的样子。
    ///
    /// `STATS_AVATAR_CACHE=<目录> STATS_CARD_DUMP=<目录> cargo test
    /// compare_avatar_tone_variants -- --ignored`
    #[test]
    #[ignore = "三版取色并排出图"]
    fn compare_avatar_tone_variants() {
        let (Ok(dir), Ok(cache)) = (
            std::env::var("STATS_CARD_DUMP"),
            std::env::var("STATS_AVATAR_CACHE"),
        ) else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();

        let mut files: Vec<_> = std::fs::read_dir(&cache)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        files.sort();
        let imgs: Vec<image::RgbaImage> = files
            .iter()
            .filter_map(|p| image::open(p).ok())
            .map(|i| i.to_rgba8())
            .take(24)
            .collect();
        assert!(!imgs.is_empty(), "{cache} 里没有可读的头像");

        // A：原版。整张图（含透明角的纯黑）求平均，HSL 饱和度 < 0.06 退成纯灰。
        let legacy = |img: &image::RgbaImage| {
            let (mut r, mut g, mut b) = (0u64, 0u64, 0u64);
            let n = (img.width() * img.height()) as u64;
            for p in img.pixels() {
                r += p[0] as u64;
                g += p[1] as u64;
                b += p[2] as u64;
            }
            let mean = RGBColor((r / n) as u8, (g / n) as u8, (b / n) as u8);
            let (h, s, l) = to_hsl_for_test(mean);
            if s < 0.06 {
                from_hsl_for_test(0.0, 0.0, l.clamp(0.36, 0.50))
            } else {
                from_hsl_for_test(h, s.clamp(0.18, 0.42), l.clamp(0.36, 0.50))
            }
        };

        let s = 2u32;
        let cell = 100 * s;
        let swatch_w = 190 * s;
        let pad = 24 * s;
        let head = 60 * s;
        let width = pad * 2 + cell + swatch_w * 3;
        let height = head + pad + imgs.len() as u32 * cell + pad;

        let colors = ColorScheme::default();
        let config = sample_config();
        let mut buffer = vec![0u8; (width * height * 3) as usize];
        {
            let root = BitMapBackend::with_buffer(&mut buffer, (width, height)).into_drawing_area();
            root.fill(&colors.card_background).map_err(|e| e.to_string()).unwrap();

            for (i, label) in ["原版", "当前", "提议"].iter().enumerate() {
                let style = get_font_with_color(&config, 22 * s, &colors.text_secondary)
                    .pos(Pos::new(HPos::Center, VPos::Center));
                let x = (pad + cell) as i32 + (swatch_w * i as u32 + swatch_w / 2) as i32;
                root.draw_text(label, &style, (x, (head / 2) as i32)).unwrap();
            }

            for (row, img) in imgs.iter().enumerate() {
                let y = (head + pad + row as u32 * cell) as i32;
                for (i, bar) in [
                    legacy(img),
                    harmonize_theme(get_average_color(img)),
                    harmonize_theme(avatar_theme_color(img)),
                ]
                .into_iter()
                .enumerate()
                {
                    let x0 = (pad + cell + swatch_w * i as u32) as i32;
                    draw_rounded_rect(
                        &root,
                        x0 + (4 * s) as i32,
                        y + (4 * s) as i32,
                        x0 + swatch_w as i32 - (4 * s) as i32,
                        y + cell as i32 - (4 * s) as i32,
                        (10 * s) as i32,
                        bar,
                    )
                    .unwrap();
                }
            }
            root.present().unwrap();
        }

        let mut sheet = RgbaImage::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let o = ((y * width + x) * 3) as usize;
                sheet.put_pixel(
                    x,
                    y,
                    Rgba([buffer[o], buffer[o + 1], buffer[o + 2], 255]),
                );
            }
        }
        for (row, img) in imgs.iter().enumerate() {
            let y = (head + pad + row as u32 * cell) as i32;
            overlay_image(&mut sheet, img, pad as i32, y);
        }
        dump_png(&dir, "tone-compare", &save_rgba_to_base64(sheet).unwrap());
    }

    #[test]
    #[ignore = "生成本地走势图样张"]
    fn dump_sample_cards() {
        let Ok(dir) = std::env::var("STATS_CARD_DUMP") else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();
        let series = ["文本", "图片", "语音"]
            .iter()
            .enumerate()
            .map(|(i, name)| SeriesData {
                name: name.to_string(),
                color: message_type_style(name).0,
                points: (1..=7)
                    .map(|day| super::super::data_loader::ChartDataPoint {
                        label: format!("2026-09-{day:02}"),
                        value: ((day * 73 + i as i64 * 47) % 190 + 20) / (i as i64 + 1),
                    })
                    .collect(),
            })
            .collect();
        let out = draw_line_chart(&sample_config(), "本群近 7 天消息走势", series).unwrap();
        dump_png(&dir, "trend", &out);

        // 单系列另出一张：面积填充、峰值标注与纵轴顶格只在这一种里看得到
        let single = vec![SeriesData {
            name: "消息量".into(),
            color: message_type_style("图片").0,
            points: (0..24)
                .map(|h: i64| super::super::data_loader::ChartDataPoint {
                    label: format!("{h:02}:00"),
                    value: ((h * 37) % 61) * 3 + if h == 21 { 195 } else { 0 },
                })
                .collect(),
        }];
        let out = draw_line_chart(&sample_config(), "本群今日消息走势", single).unwrap();
        dump_png(&dir, "trend-single", &out);
    }

    /// 样张用的配置。线上装的字体不一定是默认那支（这台机器配的是 MiSans），
    /// 字宽一换，截断位置、数字列宽、行内留白全跟着变——看样张要看线上那一套。
    /// 设 `STATS_CARD_FONT=<字体文件路径>` 就按它出图。
    fn sample_config() -> StatsConfig {
        StatsConfig {
            font_path: std::env::var("STATS_CARD_FONT").unwrap_or_default(),
            ..StatsConfig::default()
        }
    }

    fn dump_png(dir: &str, name: &str, out: &str) {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(out.trim_start_matches("base64://"))
            .unwrap();
        std::fs::write(format!("{dir}/{name}.png"), bytes).unwrap();
    }

    /// 断言产物是 PNG；设了环境变量时顺手落盘一份，方便人工看效果。
    fn save_preview(out: &str, env_key: &str) {
        assert!(out.starts_with("base64://"));
        if let Ok(path) = std::env::var(env_key) {
            use base64::{Engine as _, engine::general_purpose};
            let bytes = general_purpose::STANDARD
                .decode(out.trim_start_matches("base64://"))
                .unwrap();
            std::fs::write(path, bytes).unwrap();
        }
    }
}

