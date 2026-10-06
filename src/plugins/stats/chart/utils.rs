use crate::plugins::stats::StatsConfig;
use base64::{Engine as _, engine::general_purpose};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use plotters::prelude::*;
use plotters::style::{FontStyle, register_font};
use std::path::Path;
use std::sync::OnceLock;

use crate::plugins::stats::LOG_TARGET;

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

/// 连族名都查不到时的兜底字体文件（Android 自带的 CJK 字体）。
/// `.ttc` 取第 0 号 face，够用来渲染中日韩汉字。
const CJK_FALLBACK_FILES: &[&str] = &[
    "/system/fonts/NotoSansCJK-Regular.ttc",
    "/system/fonts/NotoSerifCJK-Regular.ttc",
    "/system/fonts/DroidSansFallbackFull.ttf",
    "/system/fonts/DroidSansFallback.ttf",
];

static RESOLVED_FONT: OnceLock<String> = OnceLock::new();

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
        warn!(target: LOG_TARGET, "字体路径不存在或不是文件: {}", path);
        return false;
    }
    match std::fs::read(p) {
        Ok(bytes) => {
            if register_bytes(bytes) {
                info!(target: LOG_TARGET, "已加载字体文件: {}", path);
                true
            } else {
                warn!(target: LOG_TARGET, "字体文件无法被解析: {}", path);
                false
            }
        }
        Err(e) => {
            warn!(target: LOG_TARGET, "字体文件读取失败 {}: {}", path, e);
            false
        }
    }
}

/// 通过 fontdb 在系统字体中查找 family 并注册。
fn try_load_family(family: &str) -> bool {
    let db = crate::render::font::database();
    let query = fontdb::Query {
        families: &[fontdb::Family::Name(family)],
        ..Default::default()
    };
    let Some(id) = db.query(&query) else { return false };
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
                info!(target: LOG_TARGET, "已加载字体族: {}", family);
                return CHART_FONT_NAME.to_string();
            }
            warn!(
                target: LOG_TARGET,
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
                warn!(target: LOG_TARGET, "使用回退字体族: {}", fb);
                return CHART_FONT_NAME.to_string();
            }
        }

        // 4. 按文件路径兜底：Android 的系统字体没有可查询的 fontconfig 索引
        for &file in CJK_FALLBACK_FILES {
            if Path::new(file).is_file() && try_load_path(file) {
                warn!(target: LOG_TARGET, "使用回退字体文件: {}", file);
                return CHART_FONT_NAME.to_string();
            }
        }

        warn!(
            target: LOG_TARGET,
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
    /// 弱化文本映射到共享的 on-surface-variant；保留对自定义底色的对比度兜底。
    pub fn readable_faint(&self) -> RGBColor {
        ensure_contrast(self.text_faint, self.card_background, 4.5)
    }
}

impl Default for ColorScheme {
    /// 位图直接使用共享生成器的 Rust 令牌，不再维护手抄色表。
    fn default() -> Self {
        use crate::render::tokens as t;
        Self {
            background: t::SURFACE_DIM,
            card_background: t::SURFACE,
            container: t::SURFACE_CONTAINER,
            container_high: t::SURFACE_CONTAINER_HIGH,
            primary: t::PRIMARY,
            text_primary: t::ON_SURFACE,
            text_secondary: t::ON_SURFACE_VARIANT,
            text_faint: t::ON_SURFACE_VARIANT,
            outline: t::OUTLINE,
            grid_line: t::OUTLINE_VARIANT,
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
        format!("{rounded}%")
    }
}

// ================= 同一支色相里的调子 =================
//
// 条色是从头像里取的平均色，什么都有：雪白的自拍、全黑的剪影、荧光的二次元图。
// 直接拿来铺条，一张二十行的榜就是二十种互不相干的颜色，字色也只能碰运气。
// 这里按 Material 3 的 tonal 思路收一道：色相留给个人，饱和度与明度收进一条窄带，
// 条是浅而有色的容器，条上条下的字都取同一支色相的深调——底淡字深，对比稳定，通篇一套调子。

/// RGB → HSL，H 为 0—360，S/L 为 0—1。
fn to_hsl(c: RGBColor) -> (f32, f32, f32) {
    let (r, g, b) = (c.0 as f32 / 255.0, c.1 as f32 / 255.0, c.2 as f32 / 255.0);
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
/// 重新量：拿 `avatar_theme_color` 过一遍缓存目录里的头像、看彩度分布
/// （当时的量法见提交 e7cf322 的 `cache_survey`，内联测试清理时已移除）。
const HUE_NOISE_FLOOR: f32 = 0.02;

/// 回退色相：系统主色的那一支。灰头像不是"没有颜色"，是"没有自己的颜色"，
/// 于是跟着全站走，而不是退成一块纯灰——成片的中性色发灰，与主色也不像一家人。
fn fallback_hue() -> f32 {
    to_hsl(crate::render::tokens::PRIMARY).0
}

// ---- 一行的五个调子 ----
//
// 调子按 M3 的 **tone**（HCT 的感知明度，就是 CIELAB 的 L*）来定。L* 只是相对亮度
// Y 的函数，所以「定住色相、把 tone 推到 T」与「把 WCAG 相对亮度推到 Y(T)」是
// 同一件事，下面的 `at_luminance` 直接复用；任意两个 tone 之间的对比度也就与色相
// 无关，可以在这里一次算清。
//
// 从前条是 T47 的实色、名字是近白：二十根一指厚的深色块摞在一起，整张图很"实"，
// 而且长段的浅字压深底本就比深字压浅底费眼。现在改成 M3 的 container / on-container
// 那一对——条是浅而有色的容器，名字是同一支色相的深调，像从这块颜色里长出来的。
//
//     轨道   T94   条尾之后那一截，淡到只剩一点色相
//     条     T84   容器色：浅，但色相读得出来
//     标记   T40   条尾的一道竖向把手（M3E 滑块的 handle），与条 4.3∶1、与轨道 5.6∶1
//     墨     T30   条上的名字、条外的数值（on-container），在条上 6.2∶1、在轨道上 8.1∶1
//
// 条与轨道之间只差 1.3∶1，这是有意的：条尾在哪由那道把手交代（WCAG 2.2 的 1.4.11
// 要求 3∶1 的是"看懂内容所必需的图形"，把手对两侧都过线），条本身只负责轻轻地
// 铺出长度，不必再是一块压着人的深色。

/// M3 tone（L*，0—100）→ WCAG 相对亮度。
fn tone_luminance(tone: f32) -> f32 {
    let f = (tone + 16.0) / 116.0;
    if tone > 8.0 { f * f * f } else { tone / 903.3 }
}

const TRACK_TONE: f32 = 94.0;
const BAR_TONE: f32 = 84.0;
const HANDLE_TONE: f32 = 40.0;
const INK_TONE: f32 = 30.0;

/// 条色的饱和度窄带。浅调上同样的 HSL 饱和度给出的彩度只有中间调的一半不到，
/// 所以这条带比从前（0.16—0.30，配 T47）整体往上挪；上限仍然压着，二十行连起来
/// 是一套调子，不是一道彩虹。
const MAX_SATURATION: f32 = 0.62;
const MIN_SATURATION: f32 = 0.36;
/// 读不出色相的头像只带一点主色，比窄带的下限还低：它不该因为"没有颜色"
/// 反而成为整张榜上最扎眼的一条。
const FALLBACK_SATURATION: f32 = 0.18;

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

/// 同一支色相、同一档饱和度，换到另一个 tone。
fn at_tone(color: RGBColor, tone: f32) -> RGBColor {
    let (h, s, _) = to_hsl(color);
    at_luminance(h, s, tone_luminance(tone))
}

/// 主题色只留色相，饱和度收进窄带，明度归一到 `BAR_TONE`。
pub fn harmonize_theme(color: RGBColor) -> RGBColor {
    let (h, s, _) = to_hsl(color);
    let chroma =
        (color.0.max(color.1).max(color.2) - color.0.min(color.1).min(color.2)) as f32 / 255.0;
    // 彩度低到读不出方向的头像（纯灰的线稿、雪白、近黑）退到固定的回退色相，
    // 饱和度压到窄带之下：看着仍然是一块灰，只是带着系统主色那一点蓝紫。
    if chroma < HUE_NOISE_FLOOR {
        return at_luminance(
            fallback_hue(),
            FALLBACK_SATURATION,
            tone_luminance(BAR_TONE),
        );
    }
    at_luminance(
        h,
        s.clamp(MIN_SATURATION, MAX_SATURATION),
        tone_luminance(BAR_TONE),
    )
}

/// 这一行的淡色轨道：同一支色相，推到 `TRACK_TONE`。
pub fn track_tone(bar: RGBColor) -> RGBColor {
    at_tone(bar, TRACK_TONE)
}

/// 条尾的把手：同一支色相的中深调，对条与轨道都过非文字的 3∶1。
pub fn handle_tone(bar: RGBColor) -> RGBColor {
    at_tone(bar, HANDLE_TONE)
}

/// 条上、条旁的字色（on-container）：同一支色相的深调，名字跟着条色走。
///
/// tone 之间的对比度与色相无关，T30 在 T84 上恒为 6.2∶1；最后仍过一遍
/// `ensure_contrast`，挡住 8 位取整把哪一支推到线下的万一。
pub fn on_bar_ink(bar: RGBColor) -> RGBColor {
    ensure_contrast(at_tone(bar, INK_TONE), bar, 4.5)
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
    root.draw(&Rectangle::new(
        [(x0, y0 + r), (x0 + r, y1 - r)],
        color.filled(),
    ))
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
        .map_err(|e| format!("图片编码失败：{e}"))?;
    let b64 = general_purpose::STANDARD.encode(cursor.into_inner());
    Ok(format!("base64://{b64}"))
}

pub fn truncate_text_to_fit(
    font: &FontDesc,
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
        let candidate = format!("{s}…");
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
