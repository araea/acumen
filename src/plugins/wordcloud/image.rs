use super::stopwords::get_stop_words;
use araea_wordcloud::{WordCloudBuilder, WordInput};
use base64::{Engine as _, engine::general_purpose};
use rand::RngExt;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Instant;

/// 词云的纸色。取设计系统的卡面（`res/cards/m3e.css` 的 `scheme-manual`），
/// 与六张卡片、与统计图同一张纸——词云常常就插在一张统计卡片后面。
const PAPER: &str = crate::render::tokens::SURFACE_HEX;

/// 词与词之间的碰撞间距，同时也是内容边界外自带的一圈留白（画布像素）。
const PADDING: u32 = 4;

/// 裁到内容边界后，四周再留的纸色边距（画布像素）。
///
/// 出图会按 `to_png` 的缩放一起放大，`to_png(2.0)` 下就是 16px；加上掩码自带的
/// `PADDING`，墨迹离图片边框约 24px。从前这一步是自己解码 PNG、扫一遍内容边界、
/// 按内容短边取 1.5% 再夹到 12—48px 后重新编码；现在由 araea-wordcloud 在布局层
/// 算，边距给一个定值即可。
const TRIM_MARGIN: u32 = 8;

/// 由共享的 M3 令牌生成，不再手动复制卡片色值。
const WORD_COLORS: [&str; 5] = crate::render::tokens::WORD_COLORS;

static FONT_DB: OnceLock<fontdb::Database> = OnceLock::new();

fn get_font_db() -> &'static fontdb::Database {
    FONT_DB.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts(); // 扫描系统字体
        db
    })
}

pub fn generate_word_cloud(
    corpus: Vec<String>,
    font_path: Option<String>,
    font_family: Option<String>,
    limit: usize,
    width: u32,
    height: u32,
) -> Result<String, String> {
    let start = Instant::now();
    if !(64..=2048).contains(&width)
        || !(64..=2048).contains(&height)
        || u64::from(width) * u64::from(height) > 4_000_000
    {
        return Err("词云尺寸须在 64—2048 像素之间，总面积不超过 400 万像素".into());
    }

    let freq_map = frequencies(&corpus);

    if freq_map.is_empty() {
        return Err("有效词汇为空（可能被过滤）".to_string());
    }

    let mut word_vec: Vec<(String, f64)> = freq_map.into_iter().collect();
    word_vec.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let top_words: Vec<WordInput> = word_vec
        .into_iter()
        .take(limit.clamp(1, 200))
        .map(|(text, size)| WordInput::new(text, size as f32))
        .collect();

    let mut rng = rand::rng();
    let mut builder = WordCloudBuilder::new()
        .size(width, height)
        .seed(rng.random())
        .background(PAPER)
        .colors(WORD_COLORS)
        .padding(PADDING)
        // 画布只决定词排得开不开：成图交给库裁到内容边界，大小跟着词量走。
        .trim(true)
        .trim_margin(TRIM_MARGIN);

    // 字体加载逻辑
    if let Some(path) = font_path {
        match std::fs::read(&path) {
            Ok(font_data) => {
                builder = builder.font(font_data);
            }
            Err(e) => {
                return Err(format!("加载字体文件失败：{}（{}）", path, e));
            }
        }
    } else if let Some(family) = font_family {
        match load_font_by_family(&family) {
            Ok(font_data) => {
                builder = builder.font(font_data);
            }
            Err(e) => {
                warn!(target: super::LOG_TARGET, "加载系统字体 [{}] 失败: {}，将尝试默认方案", family, e);
            }
        }
    }

    let wordcloud = builder
        .angles(vec![0.0])
        .vertical_writing(false)
        .build(&top_words)
        .map_err(|e| format!("词云布局失败：{}", e))?;

    let png_data = wordcloud
        .to_png(2.0)
        .map_err(|e| format!("PNG 编码失败：{}", e))?;

    let b64_str = general_purpose::STANDARD.encode(&png_data);
    info!(
        target: super::LOG_TARGET,
        "Generated in {:?} ({} bytes)",
        start.elapsed(),
        png_data.len()
    );

    Ok(format!("base64://{}", b64_str))
}

/// 查找并读取字体数据
fn load_font_by_family(family: &str) -> Result<Vec<u8>, String> {
    let db = get_font_db();
    let query = fontdb::Query {
        families: &[fontdb::Family::Name(family), fontdb::Family::SansSerif],
        weight: fontdb::Weight::NORMAL,
        stretch: fontdb::Stretch::Normal,
        style: fontdb::Style::Normal,
    };

    let id = db
        .query(&query)
        .ok_or_else(|| format!("未找到匹配的字体族：{}", family))?;

    // with_face_data 会自动处理文件 IO 或内存引用，并返回闭包的结果
    db.with_face_data(id, |data, _face_index| data.to_vec())
        .ok_or_else(|| "无法获取字体数据".to_string())
}

pub fn frequencies(corpus: &[String]) -> HashMap<String, f64> {
    let stop_words = get_stop_words();
    let mut freq_map: HashMap<String, f64> = HashMap::new();

    for line in corpus {
        let words = line.split_whitespace();
        for w in words {
            let w_trim = w.trim();
            if w_trim.chars().count() > 1
                && !stop_words.contains(w_trim)
                && !w_trim
                    .chars()
                    .all(|c| c.is_numeric() || c.is_ascii_punctuation())
            {
                *freq_map.entry(w_trim.to_string()).or_insert(0.0) += 1.0;
            }
        }
    }

    freq_map
}
