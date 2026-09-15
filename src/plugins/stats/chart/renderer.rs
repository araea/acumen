use std::collections::HashMap;

use super::data_loader::{BarData, SeriesData};
use super::utils::{
    ColorScheme, draw_rounded_rect, format_percent, format_thousands, get_contrast_color,
    get_font, get_font_family,
    get_font_with_color, mix_with_white, overlay_image, save_rgba_to_base64, truncate_text_to_fit,
};
use crate::plugins::stats::StatsConfig;
use chrono::Local;
use image::{Rgba, RgbaImage};
use plotters::prelude::*;
use plotters::style::text_anchor::{HPos, Pos, VPos};

/// 绘制水平条形图 (排行榜)
///
/// 版式分成四个纵列：头像 → 横条（实色进度 + 淡色轨道）→ 数值 → 占比。
/// 横条本身的画法（实色 + 半淡轨道、条内写名字）保持不变；排版上只做三件事：
/// 数值与占比各自右对齐成固定的一列（不再跟着条尾走成一串阶梯，也不再压在
/// 淡色轨道上）、条太窄放不下的名字改写到轨道上、刻度线只留有意义的三道。
pub fn draw_bar_chart(
    config: &StatsConfig,
    title: &str,
    data: Vec<BarData>,
) -> Result<String, String> {
    if data.is_empty() {
        return Err("暂无数据".to_string());
    }

    let s = 2u32; // Scale factor

    // === 1. 预计算与布局参数 (Scaling) ===
    let padding = 24 * s;

    // 纸面不用纯白：整屏 2000 px 的高亮白在手机上看久了刺眼，退半档到暖白，
    // 淡色轨道与横条反而更浮得出来。与走势图同一张纸。
    let page_bg = RGBColor(251, 250, 247);
    let colors = ColorScheme::default();
    let ink = colors.text_primary; // 正文墨色：纯黑太硬，统一用深蓝灰
    let ink_soft = colors.text_secondary;

    // 内部尺寸也随之放大。`row_height` 是条本身的高度，`row_pitch` 是相邻两行的
    // 行距：之间留一道空档，条与条才分得开——紧挨着排会连成一整块三色板，
    // 排行读起来反而费劲。
    let row_height = 50 * s;
    let row_gap = 10 * s;
    let row_pitch = row_height + row_gap;
    let font_size = 30 * s;
    let avatar_width = 50 * s;
    // 头像与横条之间留一道窄缝：圆头像直接贴着实色条会挤成一团。
    let avatar_gap = 6 * s;
    let gap_text = 14 * s;
    let text_inset = 10 * s; // 文字距条端/轨道端的内缩

    // 标题区域
    let title_font_size = 32 * s;
    let header_font_size = 20 * s;
    let sub_font_size = 20 * s;

    let header_margin = 10 * s;
    let sub_margin = 10 * s; // 标题与副标题
    let title_margin = 22 * s; // 副标题和列表的间距
    let sub_y = padding + header_font_size + header_margin + title_font_size + sub_margin;
    let top_area_height = sub_y + sub_font_size + title_margin;

    let base_bar_min_width = 150.0 * (s as f64);
    let base_bar_scale_width = 700.0 * (s as f64);
    let max_possible_bar_width = (base_bar_min_width + base_bar_scale_width) as u32;

    let max_val = data.iter().map(|d| d.value).max().unwrap_or(1).max(1);
    let total_val: i64 = data.iter().map(|d| d.value).sum();

    let font_family = get_font_family(config);
    let font_obj = (font_family, font_size).into_font();
    let pct_font_size = 20 * s;
    let pct_font_obj = (font_family, pct_font_size).into_font();

    // 每行都展示 "数值 + 百分比"，百分比用更小的灰色字体，提升可读性。
    // 两者各自量出最宽的一条，好让它们各占一列、右对齐——一列对齐的数字才扫得动。
    let mut formatted_counts: Vec<(String, String)> = Vec::new();
    let mut value_col_w = 0u32;
    let mut pct_col_w = 0u32;
    let pct_gap = 8 * s;

    for item in data.iter() {
        let value_text = format_thousands(item.value);
        let pct_text = format_percent(item.value, total_val);

        let (vw, _) = font_obj.box_size(&value_text).unwrap_or((0, 0));
        let (pw, _) = pct_font_obj.box_size(&pct_text).unwrap_or((0, 0));
        value_col_w = value_col_w.max(vw);
        pct_col_w = pct_col_w.max(pw);
        formatted_counts.push((value_text, pct_text));
    }

    // 计算内容区域尺寸
    let content_width = avatar_width
        + avatar_gap
        + max_possible_bar_width
        + gap_text
        + value_col_w
        + pct_gap
        + pct_col_w;
    // 最后一行的下面不留空档，否则底边会多出一段没有内容的留白。
    let content_height = data.len() as u32 * row_pitch - row_gap + top_area_height;

    // 计算画布尺寸 (增加四周边距)
    let canvas_width = content_width + padding * 2;
    let canvas_height = content_height + padding; // 底部留白

    // 横条与数字的纵向分界线：轨道到此为止，右边整列留给数字。
    let track_start_x = (padding + avatar_width + avatar_gap) as i32;
    let track_end_x = track_start_x + max_possible_bar_width as i32;
    let value_right_x = track_end_x + (gap_text + value_col_w) as i32;
    let pct_right_x = value_right_x + (pct_gap + pct_col_w) as i32;

    // === 2. 绘图 ===
    let mut buffer = vec![0u8; (canvas_width * canvas_height * 3) as usize];
    {
        let root = BitMapBackend::with_buffer(&mut buffer, (canvas_width, canvas_height))
            .into_drawing_area();

        root.fill(&page_bg).map_err(|e| e.to_string())?;

        let now_str = Local::now().format("%Y-%m-%d %H:%M").to_string();
        let header_style = get_font_with_color(config, header_font_size, &ink_soft)
            .pos(Pos::new(HPos::Center, VPos::Top));
        root.draw_text(
            &now_str,
            &header_style,
            (canvas_width as i32 / 2, padding as i32),
        )
        .map_err(|e| e.to_string())?;

        // 绘制标题 (Header 下方)
        let title_y = padding + header_font_size + header_margin;
        let title_style = get_font_with_color(config, title_font_size, &ink)
            .pos(Pos::new(HPos::Center, VPos::Top));
        root.draw_text(
            title,
            &title_style,
            (canvas_width as i32 / 2, title_y as i32),
        )
        .map_err(|e| e.to_string())?;

        // 副标题：说清楚这张榜的范围与总量——每行的百分比正是以它为基数。
        let subtitle = if data.len() > 1 {
            format!("前 {} 名 · 合计 {}", data.len(), format_thousands(total_val))
        } else {
            format!("合计 {}", format_thousands(total_val))
        };
        let sub_style = get_font_with_color(config, sub_font_size, &ink_soft)
            .pos(Pos::new(HPos::Center, VPos::Top));
        root.draw_text(
            &subtitle,
            &sub_style,
            (canvas_width as i32 / 2, sub_y as i32),
        )
        .map_err(|e| e.to_string())?;

        // 每行的行位与条长只算一次，三趟绘制（轨道 → 刻度 → 实条与文字）共用。
        let rows: Vec<(i32, i32)> = data
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let y = top_area_height as i32 + (i as u32 * row_pitch) as i32;
                let ratio = item.value as f64 / max_val as f64;
                let bar_w = (base_bar_min_width + base_bar_scale_width * ratio).round() as i32;
                (y, track_start_x + bar_w)
            })
            .collect();

        // 第一趟：淡色轨道。先铺满，刻度线才有底可落。
        for ((y, bar_end_x), item) in rows.iter().zip(data.iter()) {
            if *bar_end_x < track_end_x {
                root.draw(&Rectangle::new(
                    [(*bar_end_x, *y), (track_end_x, y + row_height as i32)],
                    mix_with_white(item.theme_color, 0.5).filled(),
                ))
                .map_err(|e| e.to_string())?;
            }
        }

        // 第二趟：刻度竖线。夹在轨道与实条之间——画在实条之前，条上才不会留下
        // 几道像被划过的灰线；画在轨道之后，线才真的落在浅色底上当刻度用
        // （原先画在轨道之前，被每行的轨道盖住，只在行距里露出几段）。
        // 位置取榜首长度的 1/4、1/2、3/4：条尾停在哪道线之间，是榜首的几成一目了然。
        let vertical_line_color = RGBAColor(0, 0, 0, 0.1);
        let line_width = 3 * s as i32;
        let content_end_y =
            top_area_height as i32 + (data.len() as u32 * row_pitch - row_gap) as i32;
        for q in [0.25f64, 0.5, 0.75] {
            let line_x =
                track_start_x + (base_bar_min_width + base_bar_scale_width * q).round() as i32;
            root.draw(&Rectangle::new(
                [
                    (line_x, top_area_height as i32),
                    (line_x + line_width, content_end_y),
                ],
                vertical_line_color.filled(),
            ))
            .map_err(|e| e.to_string())?;
        }

        // 第三趟：实色进度条与行内文字
        for (i, item) in data.iter().enumerate() {
            let (y, bar_end_x) = rows[i];
            let start_x = track_start_x;
            let theme_color = item.theme_color;

            // 3. 绘制进度条 (Solid)
            root.draw(&Rectangle::new(
                [(start_x, y), (bar_end_x, y + row_height as i32)],
                theme_color.filled(),
            ))
            .map_err(|e| e.to_string())?;

            // 4. 绘制昵称：条里写得下就写在条里（老样子），写不下才挪到轨道上。
            let text_mid_y = y + (row_height / 2) as i32 + (2 * s as i32);
            let (name_w, _) = font_obj.box_size(&item.label).unwrap_or((0, 0));
            let slot = name_slot(start_x, bar_end_x, track_end_x, text_inset as i32, name_w);
            // 条内按底色挑黑白，落到淡色轨道上就用正文墨色
            let name_color = if slot.inside {
                get_contrast_color(theme_color)
            } else {
                ink
            };

            let display_name = truncate_text_to_fit(&font_obj, &item.label, slot.width);
            if !display_name.is_empty() {
                let name_style = get_font_with_color(config, font_size, &name_color)
                    .pos(Pos::new(HPos::Left, VPos::Center));
                root.draw_text(&display_name, &name_style, (slot.x, text_mid_y))
                    .map_err(|e| e.to_string())?;
            }

            // 5. 数值与占比：各自右对齐成一列，落在轨道右侧的空白上。
            //    跟着条尾走的写法会让数字排成一道阶梯，还要压着淡色轨道念。
            let (value_text, pct_text) = &formatted_counts[i];
            let count_style = get_font_with_color(config, font_size, &ink)
                .pos(Pos::new(HPos::Right, VPos::Center));
            root.draw_text(value_text, &count_style, (value_right_x, text_mid_y))
                .map_err(|e| e.to_string())?;

            let pct_style = get_font_with_color(config, pct_font_size, &ink_soft)
                .pos(Pos::new(HPos::Right, VPos::Center));
            root.draw_text(pct_text, &pct_style, (pct_right_x, text_mid_y))
                .map_err(|e| e.to_string())?;
        }

        // 7. 绘制图标徽章 (消息类型等无头像条目：主题色圆底 + 类型字符)
        for (i, item) in data.iter().enumerate() {
            if item.avatar_img.is_some() {
                continue;
            }
            let Some(icon_char) = item.icon_char.as_deref() else {
                continue;
            };

            let y = top_area_height as i32 + (i as u32 * row_pitch) as i32;
            let cx = padding as i32 + (avatar_width / 2) as i32;
            let cy = y + (row_height / 2) as i32;
            let radius = (avatar_width as f32 * 0.46) as i32;

            // 外圈淡色光晕 + 主题色圆底
            let halo_color = mix_with_white(item.theme_color, 0.35);
            root.draw(&Circle::new(
                (cx, cy),
                radius + (3 * s as i32),
                halo_color.filled(),
            ))
            .map_err(|e| e.to_string())?;
            root.draw(&Circle::new((cx, cy), radius, item.theme_color.filled()))
                .map_err(|e| e.to_string())?;

            // 圆内字符 (自动根据底色选择黑/白)
            let icon_color = get_contrast_color(item.theme_color);
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

    // 叠加头像 (注意边距偏移)
    for (i, item) in data.iter().enumerate() {
        if let Some(avatar) = &item.avatar_img {
            let y_pos = top_area_height as i32 + (i as u32 * row_pitch) as i32;
            let x_pos = padding as i32;
            overlay_image(&mut rgba_image, avatar, x_pos, y_pos);
        }
    }

    save_rgba_to_base64(rgba_image)
}

/// 昵称的落位：写在实色条里，还是写到条尾右边的淡色轨道上。
///
/// 条内是本来的写法，也是首选；但榜尾那几行的条只有最小长度，长昵称塞进去会被
/// 截成 "很长的…" 两三个字，而右手边整条轨道都空着。于是改成「哪边宽用哪边」：
/// 条里放得下就放条里，放不下时取更宽的一侧，一样宽仍旧留在条里——榜首的条占满
/// 轨道，外侧宽度为零，写法与从前完全一致。
struct NameSlot {
    x: i32,
    width: u32,
    inside: bool,
}

fn name_slot(
    bar_start_x: i32,
    bar_end_x: i32,
    track_end_x: i32,
    inset: i32,
    name_width: u32,
) -> NameSlot {
    let inside_w = (bar_end_x - bar_start_x - 2 * inset).max(0) as u32;
    let outside_x = bar_end_x + inset;
    let outside_w = (track_end_x - inset - outside_x).max(0) as u32;

    if name_width <= inside_w || inside_w >= outside_w {
        NameSlot {
            x: bar_start_x + inset,
            width: inside_w,
            inside: true,
        }
    } else {
        NameSlot {
            x: outside_x,
            width: outside_w,
            inside: false,
        }
    }
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
pub fn draw_message_type_ranking(
    config: &StatsConfig,
    title: &str,
    data: Vec<BarData>,
) -> Result<String, String> {
    if data.is_empty() {
        return Err("暂无数据".to_string());
    }

    let s = 2u32;
    let page_bg = RGBColor(242, 245, 241);
    let card_face = RGBColor(255, 255, 255);
    let card_border = RGBColor(226, 232, 240);
    let card_shadow = RGBColor(235, 239, 233);
    let track_bg = RGBColor(237, 241, 246);
    let text_primary = RGBColor(15, 23, 42);
    let text_secondary = RGBColor(100, 116, 139);
    let text_muted = RGBColor(103, 118, 112);

    // —— 布局常量：一切间距都是 s 的整数倍，缩放后不会出现半像素毛边 ——
    let padding = 30 * s;
    let card_h = 96 * s;
    let card_gap = 14 * s;
    let card_radius = 22 * s;
    let border_w = 2 * s;
    let rank_col_w = 34 * s;
    let rail_pad = 10 * s;
    let icon_size = 54 * s;
    let icon_radius = 16 * s;
    let icon_gap = 18 * s;
    let inner_pad = 22 * s;
    let bar_h = 8 * s;

    let header_font_size = 20 * s;
    let title_font_size = 32 * s;
    let sub_font_size = 20 * s;
    let name_font_size = 27 * s;
    let value_font_size = 32 * s;
    let pct_font_size = 21 * s;
    let rank_font_size = 21 * s;
    let icon_font_size = 26 * s;

    let strip_h = 16 * s;
    let strip_gap = 4 * s;

    // 标题区：时间戳 → 标题 → 概览副标题 → 构成条
    let title_y = padding + header_font_size + 8 * s;
    let sub_y = title_y + title_font_size + 10 * s;
    let strip_y = sub_y + sub_font_size + 24 * s;
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
        let now_str = Local::now().format("%Y-%m-%d %H:%M").to_string();
        let header_style = get_font_with_color(config, header_font_size, &text_muted)
            .pos(Pos::new(HPos::Center, VPos::Top));
        root.draw_text(
            &now_str,
            &header_style,
            (canvas_width as i32 / 2, padding as i32),
        )
        .map_err(|e| e.to_string())?;

        let title_style = get_font_with_color(config, title_font_size, &text_primary)
            .pos(Pos::new(HPos::Center, VPos::Top));
        root.draw_text(
            title,
            &title_style,
            (canvas_width as i32 / 2, title_y as i32),
        )
        .map_err(|e| e.to_string())?;

        let subtitle = format!(
            "共 {} 条消息 · {} 种类型",
            format_thousands(total_val),
            data.len()
        );
        let sub_style = get_font_with_color(config, sub_font_size, &text_secondary)
            .pos(Pos::new(HPos::Center, VPos::Top));
        root.draw_text(
            &subtitle,
            &sub_style,
            (canvas_width as i32 / 2, sub_y as i32),
        )
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
            let leading = i == 0;

            // 卡片：投影 → 描边 → 卡面。榜首用更明显的类型色微染做视觉锚点。
            draw_rounded_rect(
                &root,
                card_x0,
                y0 + (3 * s) as i32,
                card_x1,
                y1 + (4 * s) as i32,
                card_radius as i32,
                card_shadow,
            )?;
            draw_rounded_rect(
                &root,
                card_x0,
                y0,
                card_x1,
                y1,
                card_radius as i32,
                if leading {
                    mix_with_white(accent, 0.22)
                } else {
                    card_border
                },
            )?;
            let inner_x0 = card_x0 + border_w as i32;
            let inner_y0 = y0 + border_w as i32;
            let inner_x1 = card_x1 - border_w as i32;
            let inner_y1 = y1 - border_w as i32;
            let inner_r = (card_radius - border_w) as i32;
            let face = if leading {
                mix_with_white(accent, 0.06)
            } else {
                card_face
            };
            draw_rounded_rect(&root, inner_x0, inner_y0, inner_x1, inner_y1, inner_r, face)?;

            // 名次：卡片左起的第一段，弱化处理，只作次序参照
            let rank_color = if leading {
                accent
            } else {
                mix_with_white(accent, 0.62)
            };
            let rank_style = get_font_with_color(config, rank_font_size, &rank_color)
                .pos(Pos::new(HPos::Center, VPos::Center));
            root.draw_text(
                &(i + 1).to_string(),
                &rank_style,
                (
                    inner_x0 + (rail_pad + rank_col_w / 2) as i32,
                    cy + (2 * s) as i32,
                ),
            )
            .map_err(|e| e.to_string())?;

            // 类型图标：淡色底 + 同色字，比满色底更耐看，也不抢数值的视线
            let icon_x0 = inner_x0 + (rail_pad + rank_col_w) as i32;
            let icon_y0 = cy - (icon_size / 2) as i32;
            let icon_x1 = icon_x0 + icon_size as i32;
            let icon_fill = mix_with_white(accent, 0.16);
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
                let icon_style = get_font_with_color(config, icon_font_size, &accent)
                    .pos(Pos::new(HPos::Center, VPos::Center));
                root.draw_text(
                    icon_char,
                    &icon_style,
                    (icon_x0 + (icon_size / 2) as i32, cy + (2 * s) as i32),
                )
                .map_err(|e| e.to_string())?;
            }

            // 上行右侧：数值（主色大字）+ 占比（次级小字），右对齐收边
            let value_text = format_thousands(item.value);
            let pct_text = format_percent(item.value, total_val);
            let (pw, _) = pct_font.box_size(&pct_text).unwrap_or((0, 0));
            let (vw, _) = value_font.box_size(&value_text).unwrap_or((0, 0));

            let stats_right = inner_x1 - inner_pad as i32;
            let top_row_y = cy - (13 * s) as i32;
            let pct_style = get_font_with_color(config, pct_font_size, &text_secondary)
                .pos(Pos::new(HPos::Right, VPos::Center));
            root.draw_text(&pct_text, &pct_style, (stats_right, top_row_y))
                .map_err(|e| e.to_string())?;
            let value_right = stats_right - pw as i32 - (14 * s) as i32;
            let value_style = get_font_with_color(config, value_font_size, &text_primary)
                .pos(Pos::new(HPos::Right, VPos::Center));
            root.draw_text(&value_text, &value_style, (value_right, top_row_y))
                .map_err(|e| e.to_string())?;

            // 上行左侧：类型名，与数值同基线；过长按可用宽度截断
            let name_x = icon_x1 + icon_gap as i32;
            let name_max_w = (value_right - vw as i32 - (20 * s) as i32 - name_x).max(0) as u32;
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

    save_rgba_to_base64(rgba_image)
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

/// 将坐标轴刻度取整为 1/2/2.5/5×10^k 的"美观"步长，返回 (步长, 格数)
fn nice_axis(max_val: i64) -> (f64, usize) {
    let target = (max_val.max(1) as f64) * 1.05;
    let raw_step = (target / 5.0).max(1.0);
    let exp = raw_step.log10().floor() as i32;
    let base = 10f64.powi(exp);
    let frac = raw_step / base;
    let step = if frac <= 1.0 {
        base
    } else if frac <= 2.0 {
        2.0 * base
    } else if frac <= 2.5 {
        2.5 * base
    } else if frac <= 5.0 {
        5.0 * base
    } else {
        10.0 * base
    };
    let steps = ((target / step).ceil() as usize).clamp(2, 6);
    (step, steps)
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
) -> Result<String, String> {
    if series_list.is_empty() {
        return Err("暂无数据".to_string());
    }

    let s = 2u32;
    if !(480..=2400).contains(&config.width) || !(360..=2400).contains(&config.height) {
        return Err("走势图尺寸应为宽 480—2400、高 360—2400 像素".into());
    }
    let width = config.width * s;
    let height = config.height * s;

    let colors = ColorScheme::default();
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
    let header_font_size = 20 * s;
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

    let title_y = padding + header_font_size + gap;

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

    let legend_y = title_y + title_font_size + gap;
    let chart_top = if multi {
        legend_y + legend_h + (12 * s)
    } else {
        title_y + title_font_size + (24 * s)
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
        root.fill(&RGBColor(251, 250, 247))
            .map_err(|e| e.to_string())?;

        // 4.1 时间戳 + 标题 (与柱状图一致)
        let now_str = Local::now().format("%Y-%m-%d %H:%M").to_string();
        let header_style = get_font(config, header_font_size)
            .pos(Pos::new(HPos::Center, VPos::Top))
            .color(&RGBColor(100, 116, 139));
        root.draw_text(&now_str, &header_style, (width as i32 / 2, padding as i32))
            .map_err(|e| e.to_string())?;

        let title_style = get_font(config, title_font_size).pos(Pos::new(HPos::Center, VPos::Top));
        root.draw_text(title, &title_style, (width as i32 / 2, title_y as i32))
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
            let line_color = if i == 0 {
                RGBAColor(0, 0, 0, 0.12)
            } else {
                RGBAColor(
                    colors.grid_line.0,
                    colors.grid_line.1,
                    colors.grid_line.2,
                    1.0,
                )
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

            // 数据点：白底圆 + 主题色内圆
            for &(x, y) in &pts {
                let (xi, yi) = (x.round() as i32, y.round() as i32);
                root.draw(&Circle::new(
                    (xi, yi),
                    (6 * s) as i32,
                    RGBColor(255, 255, 255).filled(),
                ))
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
            let text = peak.value.to_string();
            let peak_style = get_font_with_color(config, axis_font_size, &colors.text_primary)
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

    save_rgba_to_base64(rgba_image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::stats::chart::data_loader::message_type_style;

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

    #[test]
    fn a_name_moves_onto_the_track_only_when_the_bar_cannot_hold_it() {
        // 轨道 100..1800，条内外都留 10 的内缩
        let (start, track_end, inset) = (100, 1800, 10);

        // 条够长：仍旧写在条里
        let wide = name_slot(start, 900, track_end, inset, 400);
        assert!(wide.inside);
        assert_eq!((wide.x, wide.width), (110, 780));

        // 榜尾的短条塞不下长名字：挪到右边的轨道上，能完整显示
        let narrow = name_slot(start, 400, track_end, inset, 700);
        assert!(!narrow.inside);
        assert_eq!((narrow.x, narrow.width), (410, 1380));
        assert!(narrow.width >= 700, "轨道要能放下整个名字");

        // 榜首的条占满轨道：外侧没有空间，维持条内写法
        let leading = name_slot(start, track_end, track_end, inset, 9_000);
        assert!(leading.inside);
        assert_eq!(leading.x, 110);
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
        let out = draw_message_type_ranking(&config, "本群 今日 消息类型 排行榜", data)
            .expect("消息类型排行榜应当能渲染");
        save_preview(&out, "AYJX_CHART_PREVIEW");
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
            draw_bar_chart(&config, "本群 今日 发言 排行榜", data).expect("发言排行榜应当能渲染");
        save_preview(&out, "AYJX_CHART_PREVIEW_BAR");
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
        let tints = [
            (86, 104, 128),
            (150, 120, 96),
            (92, 126, 110),
            (128, 106, 140),
            (176, 152, 104),
            (104, 132, 156),
            (140, 112, 112),
            (96, 118, 96),
            (120, 120, 132),
            (158, 132, 112),
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

        let out = draw_bar_chart(&StatsConfig::default(), "本群 今日 发言 排行榜", data)
            .expect("排行榜样张应当能渲染");
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(out.trim_start_matches("base64://"))
            .unwrap();
        std::fs::write(format!("{dir}/ranking-full.png"), bytes).unwrap();
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
        let out =
            draw_line_chart(&StatsConfig::default(), "本群 · 近 7 天消息走势", series).unwrap();
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(out.trim_start_matches("base64://"))
            .unwrap();
        std::fs::write(format!("{dir}/trend.png"), bytes).unwrap();
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
