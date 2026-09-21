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
    /// 卡面之上、还要再分一层的底（信息卡的图标底板）
    pub container: RGBColor,
    /// 容器里最实的一档，用来做长度条的轨道
    pub container_high: RGBColor,
    pub primary: RGBColor,
    pub text_primary: RGBColor,
    pub text_secondary: RGBColor,
    /// 比次级前景再弱一档：名次这类只作参照、不需要被读的数字
    pub text_faint: RGBColor,
    /// 图形元素的描边。按 3∶1 设计，不用来写字
    pub outline: RGBColor,
    pub grid_line: RGBColor,
}

impl ColorScheme {
    /// 卡面上够得着正文对比度的那档弱化前景。
    ///
    /// `on-surface-faint` 这支令牌压在 `#fffefa` 上量出来是 4.44∶1，离 WCAG 2.2 的
    /// 正文 4.5∶1 差一线。可达性过不去就不发，
    /// 所以这里**换取值来源**（从令牌推出来，不是另挑一个色），不放宽约束。
    /// 留痕在这里：令牌本身该往深里走半档，那是卡片那侧的事，改了之后这个方法会
    /// 自动变成恒等映射。
    pub fn readable_faint(&self) -> RGBColor {
        ensure_contrast(self.text_faint, self.card_background, 4.5)
    }
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
    /// （surface / surface-dim / surface-container / surface-container-high / primary /
    /// on-surface / on-surface-variant / on-surface-faint / outline / outline-variant），
    /// `a_chart_is_painted_in_the_card_scheme` 那条单测逐个钉着它们。
    fn default() -> Self {
        Self {
            // 相纸：卡片外的底
            background: RGBColor(237, 241, 237),
            // 卡面：统计图自己就是一张纸，用卡面那档
            card_background: RGBColor(255, 254, 250),
            container: RGBColor(241, 244, 241),
            container_high: RGBColor(231, 236, 232),
            primary: RGBColor(31, 99, 80),
            text_primary: RGBColor(31, 42, 39),
            text_secondary: RGBColor(79, 92, 87),
            text_faint: RGBColor(109, 122, 116),
            outline: RGBColor(163, 178, 170),
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
        assert_eq!(
            colors.container,
            hex(&token("--md-sys-color-surface-container"))
        );
        assert_eq!(
            colors.container_high,
            hex(&token("--md-sys-color-surface-container-high"))
        );
        assert_eq!(colors.primary, hex(&token("--md-sys-color-primary")));
        assert_eq!(colors.text_primary, hex(&token("--md-sys-color-on-surface")));
        assert_eq!(
            colors.text_secondary,
            hex(&token("--md-sys-color-on-surface-variant"))
        );
        assert_eq!(
            colors.text_faint,
            hex(&token("--md-sys-color-on-surface-faint"))
        );
        assert_eq!(colors.outline, hex(&token("--md-sys-color-outline")));
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

/// 名次色：金、银、铜。含义在颜色本身，因此**不跟主题走**，也不跟头像色走——
/// 语义固定的元素不跟随主题变色。取的是能在暖白纸上过正文对比度
/// 的深调，不是屏幕上那种亮闪闪的金银；名次的数字本身是第二个通道，色觉障碍下
/// 丢掉颜色也还认得出第几名。
pub const MEDALS: [RGBColor; 3] = [
    RGBColor(138, 100, 10),  // 金
    RGBColor(104, 112, 118), // 银
    RGBColor(140, 88, 52),   // 铜
];

/// 第 `rank` 名（从 1 起）该用的墨色：前三名是奖牌色，其余是弱化的前景色。
pub fn rank_ink(rank: usize, faint: RGBColor) -> RGBColor {
    MEDALS.get(rank.wrapping_sub(1)).copied().unwrap_or(faint)
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

/// 读不出色相的下限：RGB 三分量的极差（彩度）不到这个比例，剩下的方向就是噪声。
///
/// 判彩度要看极差，不能看 HSL 的 S：雪白的自拍 `(238,234,228)` 三分量只差 10，
/// 眼里就是一张白纸，HSL 却因为明度贴着顶而算出 0.23 的饱和度——照着它染，
/// 一张白头像会得到一条橘色的条。近黑的剪影同理。
///
/// **这个数是量出来的，不是估的。** 按本机 142 张缓存头像量，经 [`avatar_theme_color`]
/// 取出来的调子中位彩度是 0.20，`0.02`（三分量差 5 格）以下的只占 6%，翻出来看都是
/// 真正的灰度线稿与近乎中性的照片。分布离这道线很远，是件好事：判断不再卡在刀口上。
///
/// 从前不是这样——朴素平均让白底一起投票，中位彩度只有 0.082，四分之一在 0.04 以下，
/// 门槛往上挪一格就会大片退成同一支色（`0.10` 会推掉一多半）。真正该修的是取色，
/// 不是把门槛压到更低。
///
/// 收调时饱和度本来会被抬到 0.18 以上，所以方向只要不是噪声就留着它。
/// 重新量用 `chart::avatar` 里的 `cache_survey`。
const HUE_NOISE_FLOOR: f32 = 0.02;

/// 回退色相：系统主色的那一支。灰头像不是"没有颜色"，是"没有自己的颜色"，
/// 于是跟着全站走，而不是退成一块纯灰——成片的中性色发灰，与主色也不像一家人。
fn fallback_hue() -> f32 {
    to_hsl(RGBColor(31, 99, 80)).0
}

/// 实色条的目标亮度。**这是 WCAG 的相对亮度，不是 HSL 的明度。**
///
/// HSL 的明度不是视觉亮度：同一条 HSL 明度带（l=0.43, s=0.34）上，黄的实际亮度是
/// 0.275，紫是 0.101，差将近三倍。于是二十行的榜上，黄绿那几行永远比蓝紫那几行扎眼，
/// 整张图的"重量"忽轻忽重——看久了累，就是这么来的。
///
/// M3 的 tonal palette 用感知明度（HCT 的 tone）解决同一件事：同一个 tone 上的所有
/// 色相分量一样重，差别只剩色相。这里用 WCAG 的相对亮度做同样的归一。
const BAR_LUMINANCE: f32 = 0.16;

/// 淡色轨道的目标亮度。与 `BAR_LUMINANCE` 的对比度是 (0.68+0.05)/(0.16+0.05) ≈ 3.5∶1，
/// 过 WCAG 2.2 非文字元素的 3∶1——条尾在哪要看得出来，那是这张图的主要信息。
/// 从前轨道是「条色混一半白」，比例固定而对比度不固定：浅黄那一支只有 1.50∶1。
const TRACK_LUMINANCE: f32 = 0.68;

/// 彩度上限。亮度归一之后，各行之间剩下的差别只有色相与彩度；上限压到 0.30，
/// 二十行连起来才是一套调子，不是一道彩虹。
const MAX_SATURATION: f32 = 0.30;
const MIN_SATURATION: f32 = 0.16;

/// 定住色相与饱和度，把明度推到指定的相对亮度上。
///
/// 相对亮度对 HSL 明度单调，二分即可。任何色相都到得了 0—1 之间的任何亮度
/// （l=0 是黑，l=1 是白），所以这里不会失败。
fn at_luminance(h: f32, s: f32, target: f32) -> RGBColor {
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if relative_luminance(from_hsl(h, s, mid)) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    from_hsl(h, s, (lo + hi) / 2.0)
}

/// 主题色只留色相，彩度收进窄带，亮度归一到 `BAR_LUMINANCE`。
pub fn harmonize_theme(color: RGBColor) -> RGBColor {
    let (h, s, _) = to_hsl(color);
    let chroma =
        (color.0.max(color.1).max(color.2) - color.0.min(color.1).min(color.2)) as f32 / 255.0;
    // 彩度低到读不出方向的头像（纯灰的线稿、雪白、近黑）退到固定的回退色相，
    // 但**彩度压到窄带之下**：一张本来就没有颜色的头像，不该因为"没有颜色"
    // 反而成为整张榜上最扎眼的一条。它看着仍然是一块灰，只是带着系统那一点绿，
    // 不是纯灰——成片的中性色发灰，和主色也不像一家人。
    if chroma < HUE_NOISE_FLOOR {
        return at_luminance(fallback_hue(), 0.08, BAR_LUMINANCE);
    }
    at_luminance(h, s.clamp(MIN_SATURATION, MAX_SATURATION), BAR_LUMINANCE)
}

/// 这一行的淡色轨道：同一支色相，亮度归一到 `TRACK_LUMINANCE`。
///
/// 也归一，是因为「混一半白」得到的是固定的**比例**，不是固定的**对比度**：
/// 一支本来就亮的黄，混一半白之后与自己只差 1.50∶1，条尾在哪根本看不出来。
pub fn track_tone(bar: RGBColor) -> RGBColor {
    let (h, s, _) = to_hsl(bar);
    at_luminance(h, s, TRACK_LUMINANCE)
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

/// 这块底上该写深字还是浅字：黑与白各量一次，谁的对比度高就往谁那边走。
fn prefers_dark_ink(bg: RGBColor) -> bool {
    contrast_ratio(RGBColor(0, 0, 0), bg) >= contrast_ratio(RGBColor(255, 255, 255), bg)
}

/// 实色条上的字色。纯白/纯黑盖在彩色上像两片贴纸；取同色相的极浅调或极深调，
/// 对比度一样够，字却像是从这块颜色里长出来的。
///
/// 起手的那一档只是个起点：条色来自头像，什么色相都可能，总有那么一两支
/// 刚好卡在 4.5∶1 的线下（荧光粉就是），所以最后一律过一遍阈值再交出去。
pub fn get_contrast_color(bg_color: RGBColor) -> RGBColor {
    let seed = if prefers_dark_ink(bg_color) {
        deep_tone(bg_color, 0.26)
    } else {
        mix_with_white(bg_color, 0.10)
    };
    ensure_contrast(seed, bg_color, 4.5)
}

// ================= 对比度 =================
//
// 对比度是可达性，不是风格：阈值由 WCAG 2.2 定，正文 4.5∶1，大字与图形元素 3∶1。
// 条色来自头像，什么都可能，所以「同一支色相的深调」这种算法给出来的字色得逐对量过
// 才敢用——不量，总有那么一两支色相的字糊在自己的底上。

/// WCAG 2.2 的相对亮度。
fn relative_luminance(c: RGBColor) -> f32 {
    let channel = |v: u8| {
        let s = v as f32 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(c.0) + 0.7152 * channel(c.1) + 0.0722 * channel(c.2)
}

/// 两色之间的对比度，1—21。
pub fn contrast_ratio(a: RGBColor, b: RGBColor) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// 把前景一档档推开，直到它在 `bg` 上够 `min_ratio`。色相不动，只动明度——
/// 明度节奏是层级的载体，但这里让位给可达性那一条：底线过不去就不发。
///
/// 往哪边推由底色定，而且是**量出来**的：黑与白各在这块底上算一次对比度，
/// 谁高往谁推。用明度阈值（YIQ 128 之类）在交界一带会选错边——`#AC6553` 那样
/// 的砖红按 YIQ 算是"深底"，推到全白只有 4.43∶1，推到全黑却有 4.74∶1。
/// 两端里高的那个至少是 4.58∶1（正好落在交界的那支色），所以 4.5 总够得着。
pub fn ensure_contrast(fg: RGBColor, bg: RGBColor, min_ratio: f32) -> RGBColor {
    let target = if prefers_dark_ink(bg) {
        RGBColor(0, 0, 0)
    } else {
        RGBColor(255, 255, 255)
    };
    // 每档走掉剩余距离的 6%，且至少走一格：单纯按比例混色到了两端会因为取整
    // 原地打转，字色就停在离阈值一线的地方——这正是从前荧光粉那一行的毛病。
    let step = |v: u8, t: u8| -> u8 {
        let (v, t) = (v as f32, t as f32);
        let moved = v + (t - v) * 0.06;
        if t > v {
            moved.ceil().min(t) as u8
        } else if t < v {
            moved.floor().max(t) as u8
        } else {
            v as u8
        }
    };
    let mut c = fg;
    for _ in 0..255 {
        if contrast_ratio(c, bg) >= min_ratio || (c.0, c.1, c.2) == (target.0, target.1, target.2) {
            break;
        }
        c = RGBColor(
            step(c.0, target.0),
            step(c.1, target.1),
            step(c.2, target.2),
        );
    }
    c
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

/// 头像的均色，按 alpha 加权。
///
/// 头像在这之前已经被裁成圆的，方图的四角是全透明的像素——而 `RgbaImage::new`
/// 给的透明像素 RGB 是 `(0,0,0)`。不看 alpha 地把它们一起平均，等于给每张头像
/// 掺进两成纯黑：均色整体压暗，彩度也跟着被稀释掉约三成，本来就不多的色相方向
/// 于是更容易掉到噪声线以下。
///
/// 全透明或空图取主色兜底（从前是一支与全站无关的钢蓝）。
pub fn get_average_color(img: &RgbaImage) -> RGBColor {
    let mut r_sum = 0u64;
    let mut g_sum = 0u64;
    let mut b_sum = 0u64;
    let mut weight = 0u64;

    for p in img.pixels() {
        let a = p[3] as u64;
        if a == 0 {
            continue;
        }
        r_sum += p[0] as u64 * a;
        g_sum += p[1] as u64 * a;
        b_sum += p[2] as u64 * a;
        weight += a;
    }

    if weight == 0 {
        return ColorScheme::default().primary;
    }

    RGBColor(
        (r_sum / weight) as u8,
        (g_sum / weight) as u8,
        (b_sum / weight) as u8,
    )
}

/// 头像的调子：**明度取整张图的均色，色相取「有颜色的那部分」的均色。**
///
/// 均色本身是对的——一张头像给一支色，整张图都算数，不挑不猜。问题只在于把背景
/// 也算进了色相：大半头像是「大片白底/灰底 + 中间一小块彩色」，白底一平均就把那
/// 一小块的方向稀释到快没有了。中位彩度只有 0.082 就是这么来的，于是「多低算读不出
/// 色相」这条线不得不定得很低，稍微定高一点就会大片退成同一支色。
///
/// 所以这里仍然是平均，只是给每个像素按它自己的彩度加一份权重（`+0.04` 的底让
/// 纯灰的头像退化成原来那种朴素平均）。白底不再有发言权，色相回到那一小块彩色上；
/// 明度仍旧按整张图算，否则一张暗底亮标的头像会被那一点亮色带偏。
///
/// 这不是换一套逻辑，是把同一套平均算得准一点。
pub fn avatar_theme_color(img: &RgbaImage) -> RGBColor {
    let plain = get_average_color(img);

    let (mut r, mut g, mut b, mut weight) = (0f64, 0f64, 0f64, 0f64);
    for p in img.pixels() {
        if p[3] == 0 {
            continue;
        }
        let chroma = (p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2])) as f64 / 255.0;
        let w = (p[3] as f64 / 255.0) * (chroma + 0.04);
        r += p[0] as f64 * w;
        g += p[1] as f64 * w;
        b += p[2] as f64 * w;
        weight += w;
    }
    if weight <= 0.0 {
        return plain;
    }
    let tinted = RGBColor(
        (r / weight).round() as u8,
        (g / weight).round() as u8,
        (b / weight).round() as u8,
    );

    // 色相与饱和度来自加权的那一版，明度来自整张图。
    //
    // 明度**在收调的窄带里**还原，不用原样的那个值：一张白底头像的均色明度贴着顶，
    // 在那个明度上 HSL 根本表达不出多少彩度（`l=0.9` 时上限只剩两成），刚捞回来的
    // 色相会被重新压扁，连带把「读不读得出色相」那道门槛也骗过去。窄带就是
    // `harmonize_theme` 随后要 clamp 到的那一段，提前落进去不改变任何结果。
    let (h, s, _) = to_hsl(tinted);
    let (_, _, l) = to_hsl(plain);
    from_hsl(h, s, l.clamp(0.36, 0.50))
}

#[cfg(test)]
pub(crate) fn to_hsl_for_test(c: RGBColor) -> (f32, f32, f32) {
    to_hsl(c)
}

#[cfg(test)]
pub(crate) fn from_hsl_for_test(h: f32, s: f32, l: f32) -> RGBColor {
    from_hsl(h, s, l)
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
            let track = track_tone(bar);
            let ink = deep_tone(bar, 0.34);

            assert!(relative_luminance(track) > relative_luminance(bar), "轨道要比条浅");
            assert!(contrast_ratio(ink, track) >= 4.5, "数字压不住轨道");
        }

        // **这才是「同一个调子」的真正含义**：把整个色相环扫一遍，每一支的实际亮度
        // 都落在同一个点上。从前收的是 HSL 的明度，而 HSL 明度不是视觉亮度——同一条
        // 明度带上黄比紫亮将近三倍，于是黄绿那几行在榜上永远比别人扎眼，二十行的
        // 重量忽轻忽重。M3 的 tonal palette 用感知明度解决这件事，这里用相对亮度。
        let mut bars = Vec::new();
        let mut tracks = Vec::new();
        for h in 0..360 {
            for s in [0.0f32, 0.35, 0.7, 1.0] {
                for l in [0.05f32, 0.25, 0.5, 0.75, 0.95] {
                    let bar = harmonize_theme(from_hsl(h as f32, s, l));
                    bars.push(relative_luminance(bar));
                    tracks.push(relative_luminance(track_tone(bar)));
                }
            }
        }
        let spread = |v: &[f32]| {
            let (mut lo, mut hi) = (f32::MAX, f32::MIN);
            for &x in v {
                lo = lo.min(x);
                hi = hi.max(x);
            }
            (lo, hi)
        };
        // 留 0.02 的余量给 8 位量化：亮度是二分解出来的，最后要落回整数 RGB，
        // 越亮的颜色一格的跨度越大（轨道比条明显）。从前这个区间是 0.10—0.28。
        let (blo, bhi) = spread(&bars);
        assert!(
            bhi - blo < 0.02,
            "条色的亮度应当齐平，实测 {blo:.3}—{bhi:.3}"
        );
        let (tlo, thi) = spread(&tracks);
        assert!(
            thi - tlo < 0.02,
            "轨道的亮度应当齐平，实测 {tlo:.3}—{thi:.3}"
        );

        // 三分量几乎不差的头像——纯灰、近黑、冷灰——退到同一支回退色相。
        // 「不从噪声里读一个方向出来」，多低才算噪声由实测定，见 HUE_NOISE_FLOOR。
        // 三个样本都取自真实的头像缓存，不是编出来的极端值——编出来的数落在
        // 阈值哪一侧全凭手感，量出来的才说明问题。
        let fallback = harmonize_theme(RGBColor(140, 140, 140));
        for raw in [
            RGBColor(234, 234, 233),
            RGBColor(41, 42, 44),
            RGBColor(222, 223, 227),
        ] {
            let (h, _, _) = to_hsl(harmonize_theme(raw));
            let (fh, _, _) = to_hsl(fallback);
            assert!(
                (h - fh).abs() < 1.0,
                "{raw:?} 的彩度读不出方向，应当退到回退色相"
            );
        }
        // 回退色相就是主色那一支，不是一块纯灰
        assert!(fallback.0 != fallback.1 || fallback.1 != fallback.2);
        let (fh, _, _) = to_hsl(fallback);
        let (ph, _, _) = to_hsl(RGBColor(31, 99, 80));
        assert!((fh - ph).abs() < 1.0, "回退色相取的是主色那一支");

        // 本来就有色相的头像不受影响。雪白的自拍 `(238,234,228)` 也在此列：
        // 三分量差 10 格，是一张真的偏暖的照片，收调之后是一支克制的暖调，
        // 不是从前那条按 HSL 的 0.23 饱和度染出来的橘色。
        for raw in [RGBColor(255, 64, 160), RGBColor(238, 234, 228)] {
            let (h_in, _, _) = to_hsl(raw);
            let (h_out, _, _) = to_hsl(harmonize_theme(raw));
            assert!((h_in - h_out).abs() < 3.0, "{raw:?} 应当保留自己的色相");
        }
    }

    /// 图上每一处文字都要逐对量过对比度：阈值是 WCAG 2.2 的，不是眼睛觉得够。
    #[test]
    fn every_ink_clears_the_wcag_threshold_on_its_own_ground() {
        let colors = ColorScheme::default();
        let paper = colors.card_background;

        // 系统自己的几支墨：正文 4.5∶1
        for (name, ink) in [
            ("on-surface", colors.text_primary),
            ("on-surface-variant", colors.text_secondary),
            ("readable_faint", colors.readable_faint()),
        ] {
            let ratio = contrast_ratio(ink, paper);
            assert!(ratio >= 4.5, "{name} 在卡面上只有 {ratio:.2}∶1");
        }
        // 令牌原值差一线，`readable_faint` 就是为这条差额存在的；哪天卡片那侧把令牌
        // 改深了，这里会失败，那时删掉这一行与那个方法即可
        assert!(
            contrast_ratio(colors.text_faint, paper) < 4.5,
            "on-surface-faint 已经够正文对比度了，readable_faint 可以退休"
        );
        // 奖牌色也一样要能读
        for (i, medal) in MEDALS.iter().enumerate() {
            let ratio = contrast_ratio(*medal, paper);
            assert!(ratio >= 4.5, "第 {} 名的名次色只有 {ratio:.2}∶1", i + 1);
        }
        // 描边按图形元素的 3∶1 设计，因此它**不**够写字——这条钉住「弱化的文字
        // 不用描边色」那一条，免得哪天有人图省事拿它当灰字用
        assert!(contrast_ratio(colors.outline, paper) < 4.5);

        // 条色来自头像，什么色相都可能——所以不挑几个样本，把整个色相环扫一遍：
        // 收调之后条色落在一条窄带里，带子里的每一支都得写得下字。
        for h in 0..360 {
            for s in [0.0f32, 0.35, 0.7, 1.0] {
                for l in [0.05f32, 0.25, 0.5, 0.75, 0.95] {
                    let bar = harmonize_theme(from_hsl(h as f32, s, l));
                    let on_bar = get_contrast_color(bar);
                    let ratio = contrast_ratio(on_bar, bar);
                    assert!(
                        ratio >= 4.5,
                        "条上的名字在 {bar:?}（源 h={h} s={s} l={l}）上只有 {ratio:.2}∶1"
                    );
                    // 条外的数值有两种底：跟着条尾时压在淡色轨道上，排成右对齐的
                    // 一列时落在纸上。两种版式各按自己的底收墨，都得过 4.5∶1。
                    for ground in [track_tone(bar), paper] {
                        let value = ensure_contrast(deep_tone(bar, 0.34), ground, 4.5);
                        assert!(
                            contrast_ratio(value, ground) >= 4.5,
                            "数值在 {ground:?} 上只有 {:.2}∶1",
                            contrast_ratio(value, ground)
                        );
                        let pct = ensure_contrast(
                            mix_with_color(value, ground, 0.62),
                            ground,
                            4.5,
                        );
                        assert!(contrast_ratio(pct, ground) >= 4.5);
                    }
                }
            }
        }
    }

    /// 真头像的均色是一片洗过的灰调，不是样张里那种鲜明的色块。
    ///
    /// 这条钉住「多低才算读不出色相」：按本机 142 张缓存头像量，均色的中位彩度
    /// 只有 0.08，四分之一在 0.04 以下。门槛定高一格，一整张榜就会大片变成同一条
    /// 回退绿——合成样张看不出来，因为假头像的彩度比真头像高得多。
    #[test]
    fn a_washed_out_avatar_still_keeps_its_own_hue() {
        // 左边是真头像缓存里量到的均色，右边是它该不该保住自己的色相
        let samples = [
            ((119u8, 109, 98), true),  // 中位数那张：暖调，彩度 0.08
            ((145, 160, 171), true),   // 偏蓝的合影
            ((190, 180, 184), true),   // 淡粉灰，彩度 0.039
            ((89, 97, 96), true),      // 偏青的暗调，彩度 0.031
            ((238, 233, 230), true),   // 暖白的自拍，彩度 0.031
            ((222, 223, 227), false),  // 冷灰，彩度 0.020——到线上了
            ((220, 220, 220), false),  // 纯灰
            ((234, 234, 233), false),  // 近白的灰
            ((41, 42, 44), false),     // 近黑的剪影
        ];
        let fallback_hue = to_hsl(harmonize_theme(RGBColor(128, 128, 128))).0;
        for ((r, g, b), keeps_hue) in samples {
            let raw = RGBColor(r, g, b);
            let (own, _, _) = to_hsl(raw);
            let (out, _, _) = to_hsl(harmonize_theme(raw));
            if keeps_hue {
                assert!(
                    (out - own).abs() < 3.0,
                    "{raw:?} 的色相还读得出来，不该被推成回退色（{out} vs {own}）"
                );
            } else {
                assert!(
                    (out - fallback_hue).abs() < 1.0,
                    "{raw:?} 彩度已是噪声，应当退到回退色相"
                );
            }
        }
    }

    /// 大半头像是「一大片白底 + 中间一小块彩色」。朴素平均等于让白底投票决定色相，
    /// 那一小块的方向被稀释到快没有了。
    #[test]
    fn a_white_background_does_not_get_a_vote_on_the_hue() {
        // 八成白底 + 两成正红
        let mut img = RgbaImage::new(100, 100);
        for y in 0..100u32 {
            for x in 0..100u32 {
                let red = y >= 80;
                let px = if red {
                    Rgba([200, 30, 30, 255])
                } else {
                    Rgba([250, 250, 250, 255])
                };
                img.put_pixel(x, y, px);
            }
        }

        let plain = get_average_color(&img);
        let weighted = avatar_theme_color(&img);
        let chroma =
            |c: RGBColor| (c.0.max(c.1).max(c.2) - c.0.min(c.1).min(c.2)) as f32 / 255.0;

        // 两者的色相一致（白底不带色相，稀释的是强度不是方向）
        assert!((to_hsl(plain).0 - to_hsl(weighted).0).abs() < 6.0);
        // 但朴素平均被白底压到快读不出来，加权之后回到那一小块红上
        assert!(chroma(plain) < 0.14, "朴素平均的彩度 {}", chroma(plain));
        assert!(
            chroma(weighted) > chroma(plain) * 2.0,
            "加权之后应当把那一小块红捞回来：{} -> {}",
            chroma(plain),
            chroma(weighted)
        );
        // 明度仍然按整张图算（落在收调的窄带里）：这是一张亮头像，
        // 不因为那一块红就变暗，所以顶在窄带的上沿
        assert!((to_hsl(plain).2.clamp(0.36, 0.50) - to_hsl(weighted).2).abs() < 0.02);
        assert!(to_hsl(weighted).2 > 0.49, "亮头像应当落在窄带的上沿");

        // 整张纯灰的头像没有可加权的东西：色相仍然读不出来，交给回退那一条
        let mut flat = RgbaImage::new(20, 20);
        for p in flat.pixels_mut() {
            *p = Rgba([140, 140, 140, 255]);
        }
        let flat_tone = avatar_theme_color(&flat);
        assert_eq!(chroma(flat_tone), 0.0);
    }

    /// 读不出色相的头像退到回退色相，但**不该是整张榜上最扎眼的一条**：
    /// 它本来就没有颜色，收出来该是一块带着系统色的灰。
    #[test]
    fn a_colourless_avatar_stays_quiet() {
        let gray = harmonize_theme(RGBColor(150, 150, 150));
        let colourful = harmonize_theme(RGBColor(200, 30, 30));
        let sat = |c: RGBColor| to_hsl(c).1;
        assert!(
            sat(gray) < sat(colourful) / 1.5,
            "灰头像的条 {gray:?}（S={:.2}）不该比有颜色的 {colourful:?}（S={:.2}）还艳",
            sat(gray),
            sat(colourful)
        );
        // 但也不是纯灰
        assert!(gray.0 != gray.1 || gray.1 != gray.2);
    }

    /// 圆头像的四角是全透明的，`RgbaImage` 给它们的 RGB 是纯黑。
    /// 不看 alpha 地平均，等于往每张头像里掺两成黑。
    #[test]
    fn the_transparent_corners_do_not_darken_the_average() {
        let size = 100u32;
        let mut img = RgbaImage::new(size, size);
        let center = size as f32 / 2.0;
        let tint = [200u8, 150, 90];
        for y in 0..size {
            for x in 0..size {
                let (dx, dy) = (x as f32 - center + 0.5, y as f32 - center + 0.5);
                if (dx * dx + dy * dy).sqrt() <= center - 1.0 {
                    img.put_pixel(x, y, Rgba([tint[0], tint[1], tint[2], 255]));
                }
            }
        }
        let avg = get_average_color(&img);
        assert_eq!(
            (avg.0, avg.1, avg.2),
            (tint[0], tint[1], tint[2]),
            "一张纯色的圆头像，均色就该是那个纯色"
        );

        // 全透明的图没有颜色可取，兜底给主色，不给一支系统外的蓝
        let blank = RgbaImage::new(8, 8);
        assert_eq!(get_average_color(&blank), ColorScheme::default().primary);
    }

    /// 条的长度是「必须看得懂才读得出内容」的图形元素，它与自己那条轨道之间
    /// 按 WCAG 2.2 的非文字阈值要够 3∶1——不然条尾在哪就只能靠猜。
    #[test]
    fn the_bar_stands_out_from_its_own_track() {
        let mut worst = (99.0f32, RGBColor(0, 0, 0));
        for h in 0..360 {
            for s in [0.0f32, 0.35, 0.7, 1.0] {
                for l in [0.05f32, 0.25, 0.5, 0.75, 0.95] {
                    let bar = harmonize_theme(from_hsl(h as f32, s, l));
                    let track = track_tone(bar);
                    let r = contrast_ratio(bar, track);
                    if r < worst.0 {
                        worst = (r, bar);
                    }
                }
            }
        }
        println!("条与轨道最差的一支：{:?} {:.2}∶1", worst.1, worst.0);
        assert!(worst.0 >= 3.0, "条与轨道只有 {:.2}∶1", worst.0);
    }

    #[test]
    fn medals_are_fixed_and_the_rest_fall_back_to_faint() {
        let faint = ColorScheme::default().text_faint;
        assert_eq!(rank_ink(1, faint), MEDALS[0]);
        assert_eq!(rank_ink(3, faint), MEDALS[2]);
        assert_eq!(rank_ink(4, faint), faint);
        assert_eq!(rank_ink(20, faint), faint);
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
