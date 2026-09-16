use crate::plugins::stats::StatsConfig;
use base64::{Engine as _, engine::general_purpose};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use plotters::prelude::*;
use plotters::style::{FontStyle, register_font};
use std::path::Path;
use std::sync::OnceLock;

// ================= 字体加载 =================

/// 注册到 plotters 的内部字体名。使用固定名称避免与系统字体族名冲突 —
/// 不论用户提供路径还是字体族，最终绘制时都查找该名称即可命中我们注册的字节。
const CHART_FONT_NAME: &str = "ChartFont";

/// 当用户未指定字体或指定的字体不可用时按顺序尝试的 CJK 回退字体。
const CJK_FALLBACK_FAMILIES: &[&str] = &[
    "Noto Sans CJK SC",
    "Noto Sans SC",
    "Noto Sans CJK JP",
    "Noto Sans CJK TC",
    "Source Han Sans SC",
    "Source Han Sans CN",
    "Source Han Sans",
    "WenQuanYi Micro Hei",
    "WenQuanYi Zen Hei",
    "Microsoft YaHei",
    "SimHei",
    "SimSun",
    "PingFang SC",
    "Heiti SC",
    "STHeiti",
    "Arial Unicode MS",
];

/// fontdb 的 `load_system_fonts` 明确排除了 Android，Termux 里因此一个系统字体
/// 都发现不了，图表会直接报 FontUnavailable。这里补上 Android / Termux 的字体目录。
const EXTRA_FONT_DIRS: &[&str] = &[
    "/system/fonts",
    "/system/font",
    "/data/fonts",
    "/product/fonts",
    "/system/product/fonts",
];

/// 连族名都查不到时的兜底字体文件（Android 自带的 CJK 字体）。
/// `.ttc` 取第 0 号 face，够用来渲染中日韩汉字。
const CJK_FALLBACK_FILES: &[&str] = &[
    "/system/fonts/NotoSansCJK-Regular.ttc",
    "/system/fonts/NotoSerifCJK-Regular.ttc",
    "/system/fonts/DroidSansFallbackFull.ttf",
    "/system/fonts/DroidSansFallback.ttf",
];

static FONT_DB: OnceLock<fontdb::Database> = OnceLock::new();
static RESOLVED_FONT: OnceLock<String> = OnceLock::new();

fn get_font_db() -> &'static fontdb::Database {
    FONT_DB.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        for dir in EXTRA_FONT_DIRS {
            if Path::new(dir).is_dir() {
                db.load_fonts_dir(dir);
            }
        }
        for dir in user_font_dirs() {
            if dir.is_dir() {
                db.load_fonts_dir(&dir);
            }
        }
        db
    })
}


/// Termux 前缀与用户目录下的字体目录（`PREFIX` / `HOME` 由 Termux 注入）。
fn user_font_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(prefix) = std::env::var("PREFIX") {
        dirs.push(Path::new(&prefix).join("share/fonts"));
    }
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(Path::new(&home).join(".fonts"));
        dirs.push(Path::new(&home).join(".local/share/fonts"));
    }
    dirs
}

/// 将字节注册到 plotters 的 ab_glyph 后端。注册成功返回 true。
/// 注意：plotters 需要 `'static` 字节切片，因此我们 leak 一次（每进程最多发生一次）。
fn register_bytes(bytes: Vec<u8>) -> bool {
    let static_bytes: &'static [u8] = bytes.leak();
    register_font(CHART_FONT_NAME, FontStyle::Normal, static_bytes).is_ok()
}

/// 从文件路径加载字体并注册。
fn try_load_path(path: &str) -> bool {
    let p = Path::new(path);
    if !p.is_file() {
        warn!(target: "Plugin/Stats", "字体路径不存在或不是文件: {}", path);
        return false;
    }
    match std::fs::read(p) {
        Ok(bytes) => {
            if register_bytes(bytes) {
                info!(target: "Plugin/Stats", "已加载字体文件: {}", path);
                true
            } else {
                warn!(target: "Plugin/Stats", "字体文件无法被解析: {}", path);
                false
            }
        }
        Err(e) => {
            warn!(target: "Plugin/Stats", "字体文件读取失败 {}: {}", path, e);
            false
        }
    }
}

/// 通过 fontdb 在系统字体中查找 family 并注册。
fn try_load_family(family: &str) -> bool {
    let db = get_font_db();
    let query = fontdb::Query {
        families: &[fontdb::Family::Name(family)],
        ..Default::default()
    };
    let id = match db.query(&query) {
        Some(id) => id,
        None => return false,
    };
    let bytes: Option<Vec<u8>> = db.with_face_data(id, |data, _idx| data.to_vec());
    match bytes {
        Some(b) => register_bytes(b),
        None => false,
    }
}

/// 加载并注册字体，优先级：
/// 1. `font_path`（路径优先，绕过任何系统字体发现）
/// 2. `font_family`（通过 fontdb 在系统字体中查找）
/// 3. CJK 回退字体列表
/// 4. 失败时返回 "sans-serif"，由 plotters 兜底处理
fn resolve_font(config: &StatsConfig) -> &str {
    RESOLVED_FONT.get_or_init(|| {
        let path = config.font_path.trim();
        let family = config.font_family.trim();

        // 1. 路径优先
        if !path.is_empty() && try_load_path(path) {
            return CHART_FONT_NAME.to_string();
        }

        // 2. 字体族
        if !family.is_empty() {
            if try_load_family(family) {
                info!(target: "Plugin/Stats", "已加载字体族: {}", family);
                return CHART_FONT_NAME.to_string();
            }
            warn!(
                target: "Plugin/Stats",
                "字体族 '{}' 在系统中不可用, 尝试 CJK 回退字体",
                family
            );
        }

        // 3. CJK 回退
        for &fb in CJK_FALLBACK_FAMILIES {
            // 跳过用户已尝试过的 family，避免重复警告
            if !family.is_empty() && fb.eq_ignore_ascii_case(family) {
                continue;
            }
            if try_load_family(fb) {
                warn!(target: "Plugin/Stats", "使用回退字体族: {}", fb);
                return CHART_FONT_NAME.to_string();
            }
        }

        // 4. 按文件路径兜底：Android 的系统字体没有可查询的 fontconfig 索引
        for &file in CJK_FALLBACK_FILES {
            if Path::new(file).is_file() && try_load_path(file) {
                warn!(target: "Plugin/Stats", "使用回退字体文件: {}", file);
                return CHART_FONT_NAME.to_string();
            }
        }

        warn!(
            target: "Plugin/Stats",
            "未找到任何可用 CJK 字体，图表中的中文可能无法渲染。请通过 font_path 指定字体文件，或安装对应字体族。"
        );
        "sans-serif".to_string()
    })
}

pub fn get_font_family(config: &StatsConfig) -> &str {
    resolve_font(config)
}

// ================= 配色方案 =================

pub struct ColorScheme {
    pub background: RGBColor,
    pub card_background: RGBColor,
    pub primary: RGBColor,
    pub text_primary: RGBColor,
    pub text_secondary: RGBColor,
    pub grid_line: RGBColor,
}

impl Default for ColorScheme {
    /// 配色取卡片设计系统里的「手册」一套（`res/cards/m3e.css` 的 `scheme-manual`）。
    ///
    /// 统计图与卡片经常在同一条消息里前后出现，纸色、墨色与主色不一致，看起来就是
    /// 两个产品各画各的：从前这里是一套 Tailwind 蓝（`#3B82F6` + 石板灰），与全站
    /// 的松绿毫无关系，一张排行榜接在一张绿卡片后面，像换了个人做的。
    ///
    /// **对不上 CSS 的地方只有一处**：这里必须是字面量——plotters 画的是位图，
    /// 拿不到 CSS 的自定义属性。改了 `m3e.css` 的 `scheme-manual`，这一组要跟着改
    /// （六个数：surface / surface-dim / primary / on-surface / on-surface-variant /
    /// outline-variant），`a_chart_is_painted_in_the_card_scheme` 那条单测钉着它们。
    fn default() -> Self {
        Self {
            // 相纸：卡片外的底
            background: RGBColor(237, 241, 237),
            // 卡面：统计图自己就是一张纸，用卡面那档
            card_background: RGBColor(255, 254, 250),
            primary: RGBColor(31, 99, 80),
            text_primary: RGBColor(31, 42, 39),
            text_secondary: RGBColor(79, 92, 87),
            grid_line: RGBColor(222, 229, 223),
        }
    }
}

#[cfg(test)]
mod color_scheme_guard {
    use super::*;

    /// 图表的六个色值必须与 `res/cards/m3e.css` 的 `scheme-manual` 一致。
    ///
    /// 这条测试把两个世界的同一个决定绑在一起：CSS 那边改了纸色而这边没跟，
    /// 群里就会出现「绿卡片 + 另一套配色的图表」。断言方式是从样式表里**读**
    /// 那几个令牌，而不是把十六进制再抄一遍——抄一遍就等于没钉。
    #[test]
    fn a_chart_is_painted_in_the_card_scheme() {
        let sheet = crate::render::web::DESIGN_SYSTEM;
        let scheme = sheet
            .split("body.scheme-manual {")
            .nth(1)
            .expect("样式表里应当有 scheme-manual")
            .split('}')
            .next()
            .unwrap();
        let token = |name: &str| -> String {
            let at = scheme
                .find(&format!("{name}:"))
                .unwrap_or_else(|| panic!("scheme-manual 里没有 {name}"));
            let rest = &scheme[at + name.len() + 1..];
            rest[..rest.find(';').expect("令牌应当以分号结束")]
                .trim()
                .to_string()
        };
        let hex = |value: &str| -> RGBColor {
            let raw = value.trim_start_matches('#');
            assert_eq!(raw.len(), 6, "{value} 应当是六位十六进制");
            RGBColor(
                u8::from_str_radix(&raw[0..2], 16).unwrap(),
                u8::from_str_radix(&raw[2..4], 16).unwrap(),
                u8::from_str_radix(&raw[4..6], 16).unwrap(),
            )
        };
        let colors = ColorScheme::default();
        assert_eq!(colors.card_background, hex(&token("--md-sys-color-surface")));
        assert_eq!(colors.background, hex(&token("--md-sys-color-surface-dim")));
        assert_eq!(colors.primary, hex(&token("--md-sys-color-primary")));
        assert_eq!(colors.text_primary, hex(&token("--md-sys-color-on-surface")));
        assert_eq!(
            colors.text_secondary,
            hex(&token("--md-sys-color-on-surface-variant"))
        );
        assert_eq!(
            colors.grid_line,
            hex(&token("--md-sys-color-outline-variant"))
        );
        // 色相表（排行榜、走势图、消息类型共用的一套）也得整表在样式表里找得到，
        // 否则某天有人「顺手加一个好看的颜色」，图表就悄悄脱离系统了。
        for hue in super::super::data_loader::HUES {
            let literal = format!("#{:02X}{:02X}{:02X}", hue.0, hue.1, hue.2);
            assert!(
                sheet.to_ascii_uppercase().contains(&literal),
                "{literal} 不在 res/cards/m3e.css 里：图表的色相要取自系统那张色表"
            );
        }
    }
}

pub fn get_font<'a>(config: &'a StatsConfig, size: u32) -> TextStyle<'a> {
    let colors = ColorScheme::default();
    let family = get_font_family(config);
    (family, size).into_font().color(&colors.text_primary)
}

pub fn get_font_with_color<'a>(
    config: &'a StatsConfig,
    size: u32,
    color: &'a RGBColor,
) -> TextStyle<'a> {
    let family = get_font_family(config);
    (family, size).into_font().color(color)
}

// ================= 数值排版 =================

/// 千位分隔。四位数以上的计数挤在一起很难一眼读出量级，排行榜里尤其明显。
pub fn format_thousands(value: i64) -> String {
    let negative = value < 0;
    let digits = value.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if negative {
        out.push('-');
    }
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// 统一的占比文案：一律四舍五入到整数，一列百分数里不夹小数点看着才干净；
/// 不足半个百分点的写 "<1%"，免得非零的零头被舍成一个没意义的 "0%"。
/// 排行榜与信息卡共用，避免同一批数据在两张图里写法不一致。
pub fn format_percent(value: i64, total: i64) -> String {
    if total <= 0 || value <= 0 {
        return "0%".to_string();
    }
    let pct = value as f64 / total as f64 * 100.0;
    // `{:.0}` 是「四舍六入五成双」，2.5 会写成 2；这里要的是四舍五入，先 round 再写
    let rounded = pct.round() as i64;
    if rounded == 0 {
        "<1%".to_string()
    } else {
        format!("{}%", rounded)
    }
}

// ================= 同一支色相里的调子 =================
//
// 条色是从头像里取的平均色，什么都有：雪白的自拍、全黑的剪影、荧光的二次元图。
// 直接拿来铺条，一张二十行的榜就是二十种互不相干的颜色，字色也只能碰运气。
// 这里按 Material 3 的 tonal 思路收一道：色相留给个人，饱和度与明度收进一条窄带，
// 条上的浅字、条外的深字都从同一支色相里取——底淡字深，对比稳定，通篇一套调子。

/// RGB → HSL，H 为 0—360，S/L 为 0—1。
fn to_hsl(c: RGBColor) -> (f32, f32, f32) {
    let (r, g, b) = (
        c.0 as f32 / 255.0,
        c.1 as f32 / 255.0,
        c.2 as f32 / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d.abs() < f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    ((h + 360.0) % 360.0, s, l)
}

/// HSL → RGB。
fn from_hsl(h: f32, s: f32, l: f32) -> RGBColor {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h % 360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to8 = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    RGBColor(to8(r), to8(g), to8(b))
}

/// 主题色的明度与饱和度收进窄带，只留色相。
/// 明度上限压在 0.5：条上的浅色字才有足够对比；下限 0.36：不至于黑成一块煤。
pub fn harmonize_theme(color: RGBColor) -> RGBColor {
    let (h, s, l) = to_hsl(color);
    // 本来就没有色相的头像（纯灰、纯白、纯黑）保持中性：HSL 里它们的 H 一律是 0，
    // 硬给饱和度会凭空染出一条粉红，与头像对不上。中性色只压明度。
    if s < 0.06 {
        return from_hsl(0.0, 0.0, l.clamp(0.36, 0.50));
    }
    from_hsl(h, s.clamp(0.18, 0.42), l.clamp(0.36, 0.50))
}

/// 同色相的深调：`strength` 越小越深。给淡底上的字用。
/// 一次压暗对本来就很浅的色还不够，再压到 YIQ 亮度 96 以下为止。
pub fn deep_tone(color: RGBColor, strength: f32) -> RGBColor {
    let black = RGBColor(0, 0, 0);
    let mut c = mix_with_color(color, black, strength.clamp(0.05, 1.0));
    for _ in 0..4 {
        if yiq_brightness(c) <= 96 {
            break;
        }
        c = mix_with_color(c, black, 0.75);
    }
    c
}

fn yiq_brightness(c: RGBColor) -> u32 {
    (c.0 as u32 * 299 + c.1 as u32 * 587 + c.2 as u32 * 114) / 1000
}

/// 实色条上的字色。纯白/纯黑盖在彩色上像两片贴纸；取同色相的极浅调或极深调，
/// 对比度一样够，字却像是从这块颜色里长出来的。
pub fn get_contrast_color(bg_color: RGBColor) -> RGBColor {
    if yiq_brightness(bg_color) >= 128 {
        deep_tone(bg_color, 0.26)
    } else {
        mix_with_white(bg_color, 0.10)
    }
}

pub fn mix_with_white(color: RGBColor, opacity: f32) -> RGBColor {
    let r = (color.0 as f32 * opacity + 255.0 * (1.0 - opacity)) as u8;
    let g = (color.1 as f32 * opacity + 255.0 * (1.0 - opacity)) as u8;
    let b = (color.2 as f32 * opacity + 255.0 * (1.0 - opacity)) as u8;
    RGBColor(r, g, b)
}

/// 向白以外的底色混合：`opacity=1` 保留原色，`0` 变为 `base`。
pub fn mix_with_color(color: RGBColor, base: RGBColor, opacity: f32) -> RGBColor {
    let t = opacity.clamp(0.0, 1.0);
    let r = (color.0 as f32 * t + base.0 as f32 * (1.0 - t)) as u8;
    let g = (color.1 as f32 * t + base.1 as f32 * (1.0 - t)) as u8;
    let b = (color.2 as f32 * t + base.2 as f32 * (1.0 - t)) as u8;
    RGBColor(r, g, b)
}

/// 用矩形 + 四角圆近似填充圆角矩形（plotters 无原生圆角）。
pub fn draw_rounded_rect<DB: DrawingBackend>(
    root: &DrawingArea<DB, plotters::coord::Shift>,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    radius: i32,
    color: RGBColor,
) -> Result<(), String> {
    if x1 <= x0 || y1 <= y0 {
        return Ok(());
    }
    let max_r = ((x1 - x0).min(y1 - y0) / 2).max(0);
    let r = radius.clamp(0, max_r);

    if r == 0 {
        root.draw(&Rectangle::new([(x0, y0), (x1, y1)], color.filled()))
            .map_err(|e| e.to_string())?;
        return Ok(());
    }

    root.draw(&Rectangle::new(
        [(x0 + r, y0), (x1 - r, y1)],
        color.filled(),
    ))
    .map_err(|e| e.to_string())?;
    root.draw(&Rectangle::new(
        [(x0, y0 + r), (x1, y1 - r)],
        color.filled(),
    ))
    .map_err(|e| e.to_string())?;
    root.draw(&Circle::new((x0 + r, y0 + r), r, color.filled()))
        .map_err(|e| e.to_string())?;
    root.draw(&Circle::new((x1 - r, y0 + r), r, color.filled()))
        .map_err(|e| e.to_string())?;
    root.draw(&Circle::new((x0 + r, y1 - r), r, color.filled()))
        .map_err(|e| e.to_string())?;
    root.draw(&Circle::new((x1 - r, y1 - r), r, color.filled()))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 左侧色条：左上/左下圆角，右侧平切，贴在圆角卡片内沿。
pub fn draw_left_accent_bar<DB: DrawingBackend>(
    root: &DrawingArea<DB, plotters::coord::Shift>,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    radius: i32,
    color: RGBColor,
) -> Result<(), String> {
    if x1 <= x0 || y1 <= y0 {
        return Ok(());
    }
    let max_r = ((x1 - x0).min(y1 - y0) / 2).max(0);
    let r = radius.clamp(0, max_r);

    if r == 0 {
        root.draw(&Rectangle::new([(x0, y0), (x1, y1)], color.filled()))
            .map_err(|e| e.to_string())?;
        return Ok(());
    }

    // 主体：右侧平齐，不画右圆角
    root.draw(&Rectangle::new([(x0 + r, y0), (x1, y1)], color.filled()))
        .map_err(|e| e.to_string())?;
    root.draw(&Rectangle::new([(x0, y0 + r), (x0 + r, y1 - r)], color.filled()))
        .map_err(|e| e.to_string())?;
    root.draw(&Circle::new((x0 + r, y0 + r), r, color.filled()))
        .map_err(|e| e.to_string())?;
    root.draw(&Circle::new((x0 + r, y1 - r), r, color.filled()))
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn save_rgba_to_base64(img: RgbaImage) -> Result<String, String> {
    let dynamic_image = DynamicImage::ImageRgba8(img);
    let mut cursor = std::io::Cursor::new(Vec::new());
    dynamic_image
        .write_to(&mut cursor, ImageFormat::Png)
        .map_err(|e| format!("图片编码失败：{}", e))?;
    let b64 = general_purpose::STANDARD.encode(cursor.into_inner());
    Ok(format!("base64://{}", b64))
}

pub fn truncate_text_to_fit(
    font: &plotters::style::FontDesc,
    text: &str,
    max_width: u32,
) -> String {
    let (w, _) = font.box_size(text).unwrap_or((0, 0));
    if w <= max_width {
        return text.to_string();
    }

    let mut s = text.to_string();
    while !s.is_empty() {
        s.pop();
        let candidate = format!("{}…", s);
        let (w, _) = font.box_size(&candidate).unwrap_or((0, 0));
        if w <= max_width {
            return candidate;
        }
    }
    "…".to_string()
}

pub fn get_average_color(img: &RgbaImage) -> RGBColor {
    let mut r_sum = 0u64;
    let mut g_sum = 0u64;
    let mut b_sum = 0u64;
    let count = (img.width() * img.height()) as u64;

    if count == 0 {
        return RGBColor(59, 130, 246);
    }

    for p in img.pixels() {
        r_sum += p[0] as u64;
        g_sum += p[1] as u64;
        b_sum += p[2] as u64;
    }

    RGBColor(
        (r_sum / count) as u8,
        (g_sum / count) as u8,
        (b_sum / count) as u8,
    )
}

pub fn make_circular_avatar(img: &DynamicImage, size: u32) -> RgbaImage {
    let rgba = img.to_rgba8();
    let mut result = RgbaImage::new(size, size);
    let center = size as f32 / 2.0;
    let radius = center - 1.0;

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center + 0.5;
            let dy = y as f32 - center + 0.5;
            let dist = (dx * dx + dy * dy).sqrt();

            if dist <= radius - 0.5 {
                result.put_pixel(x, y, *rgba.get_pixel(x, y));
            } else if dist <= radius + 0.5 {
                let alpha = (radius + 0.5 - dist).clamp(0.0, 1.0);
                let mut pixel = *rgba.get_pixel(x, y);
                pixel[3] = (pixel[3] as f32 * alpha) as u8;
                result.put_pixel(x, y, pixel);
            }
        }
    }
    result
}

pub fn create_default_avatar(size: u32) -> RgbaImage {
    let mut result = RgbaImage::new(size, size);
    let center = size as f32 / 2.0;
    let radius = center - 1.0;
    let bg_color = Rgba([200, 200, 200, 255]);

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center + 0.5;
            let dy = y as f32 - center + 0.5;
            let dist = (dx * dx + dy * dy).sqrt();

            if dist <= radius - 0.5 {
                result.put_pixel(x, y, bg_color);
            } else if dist <= radius + 0.5 {
                let alpha = (radius + 0.5 - dist).clamp(0.0, 1.0);
                let mut pixel = bg_color;
                pixel[3] = (255.0 * alpha) as u8;
                result.put_pixel(x, y, pixel);
            }
        }
    }
    result
}

pub fn overlay_image(base: &mut RgbaImage, overlay: &RgbaImage, x: i32, y: i32) {
    let (base_w, base_h) = base.dimensions();
    let (overlay_w, overlay_h) = overlay.dimensions();

    for oy in 0..overlay_h {
        for ox in 0..overlay_w {
            let bx = x + ox as i32;
            let by = y + oy as i32;

            if bx >= 0 && bx < base_w as i32 && by >= 0 && by < base_h as i32 {
                let bg = base.get_pixel(bx as u32, by as u32);
                let fg = overlay.get_pixel(ox, oy);

                let alpha = fg[3] as f32 / 255.0;
                if alpha > 0.0 {
                    let blended = Rgba([
                        ((1.0 - alpha) * bg[0] as f32 + alpha * fg[0] as f32) as u8,
                        ((1.0 - alpha) * bg[1] as f32 + alpha * fg[1] as f32) as u8,
                        ((1.0 - alpha) * bg[2] as f32 + alpha * fg[2] as f32) as u8,
                        255,
                    ]);
                    base.put_pixel(bx as u32, by as u32, blended);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 收调之后，一行里「条 → 轨道 → 数字」的明度必须始终拉得开：
    /// 头像什么颜色都可能，拉不开的那一行数字就糊在轨道上看不清。
    #[test]
    fn every_avatar_color_lands_in_the_same_tonal_band() {
        let samples = [
            RGBColor(238, 234, 228), // 雪白自拍
            RGBColor(26, 24, 30),    // 近黑剪影
            RGBColor(255, 64, 160),  // 荧光二次元
            RGBColor(140, 140, 140), // 纯灰
            RGBColor(62, 111, 151),  // 本来就合适的中间调
        ];
        for raw in samples {
            let bar = harmonize_theme(raw);
            let track = mix_with_white(bar, 0.5);
            let ink = deep_tone(bar, 0.34);

            let luma = |c: RGBColor| (c.0 as u32 * 299 + c.1 as u32 * 587 + c.2 as u32 * 114) / 1000;
            assert!((80..=170).contains(&luma(bar)), "条色应落在中间调: {:?}", bar);
            assert!(luma(track) > luma(bar), "轨道要比条浅");
            assert!(
                luma(track) - luma(ink) > 90,
                "数字与轨道的明度差不够: ink={:?} track={:?}",
                ink,
                track
            );
        }

        // 灰头像不该被凭空染上色相
        let gray = harmonize_theme(RGBColor(140, 140, 140));
        assert_eq!(gray.0, gray.1);
        assert_eq!(gray.1, gray.2);
    }

    #[test]
    fn percents_round_to_whole_numbers() {
        // 四舍五入，不是「五成双」：2.5% 写作 3%
        assert_eq!(format_percent(25, 1000), "3%");
        assert_eq!(format_percent(54, 1000), "5%");
        assert_eq!(format_percent(280, 1000), "28%");
        assert_eq!(format_percent(1, 1), "100%");

        // 不足半个百分点的零头不塌成 "0%"
        assert_eq!(format_percent(4, 1000), "<1%");
        assert_eq!(format_percent(1, 1_000_000), "<1%");

        // 真正的零与无效总数仍写 0%
        assert_eq!(format_percent(0, 1000), "0%");
        assert_eq!(format_percent(5, 0), "0%");
    }
}
