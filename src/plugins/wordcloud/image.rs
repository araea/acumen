use super::stopwords::get_stop_words;
use araea_wordcloud::{WordCloudBuilder, WordInput};
use base64::{Engine as _, engine::general_purpose};
use rand::RngExt;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Instant;

/// 词云的纸色。取设计系统的卡面（`res/cards/m3e.css` 的 `scheme-manual`），
/// 与六张卡片、与统计图同一张纸——词云常常就插在一张统计卡片后面。
const PAPER: &str = "#FFFEFA";

/// 词的五个色相。
///
/// 五个都取自设计系统那张色表：主色、画像种子的靛与紫、控制卡的三级橄榄、警告赭金。
/// 选它们是因为在这张纸上**彼此分得开**——云里相邻的词常常不同色，色相挨太近就糊成
/// 一片；同时又都在同一个低彩度的家族里，不会像从前那样冒出一支与全站无关的蓝。
/// 换个说法：不是新造一套配色，是把系统里已有的色按「能分辨」这个唯一标准挑五个。
/// `the_word_hues_come_from_the_design_system` 那条单测钉着它们都还在样式表里。
const WORD_COLORS: [&str; 5] = [
    "#1F6350", // 主色（scheme-manual 的 primary）
    "#3E4E9E", // 画像种子 indigo
    "#5F3A96", // 画像种子 violet
    "#4A5B3A", // 控制卡的三级色（橄榄）
    "#7A5300", // 警告赭金
];

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

    if freq_map.is_empty() {
        return Err("有效词汇为空（可能被过滤）".to_string());
    }

    let mut word_vec: Vec<(String, f64)> = freq_map.into_iter().collect();
    word_vec.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

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
        .padding(4);

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
    info!(target: super::LOG_TARGET, "Generated in {:?}", start.elapsed());

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

#[cfg(test)]
mod tests {
    use super::*;
    /// 词云用的六个色值（纸色 + 五个色相）都得在设计系统的样式表里找得到。
    ///
    /// 这条测试挡的是「顺手加一个好看的颜色」：加了就不再是同一套系统，
    /// 而词云与卡片经常同时出现在一条消息里，一眼能看出不是一个人做的。
    #[test]
    fn the_word_hues_come_from_the_design_system() {
        let sheet = crate::render::web::DESIGN_SYSTEM.to_ascii_uppercase();
        for color in std::iter::once(PAPER).chain(WORD_COLORS) {
            assert!(
                sheet.contains(color),
                "{color} 不在 res/cards/m3e.css 里：词云的配色要取自系统那张色表"
            );
        }
    }

    #[test]
    fn rejects_oversized_cloud_before_allocating() {
        assert!(generate_word_cloud(vec![], None, None, 50, u32::MAX, 600).is_err());
    }
    #[test]
    #[ignore = "生成本地词云样张"]
    fn dump_sample_cards() {
        let Ok(dir) = std::env::var("WORDCLOUD_CARD_DUMP") else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();
        let words = [
            "生活", "阅读", "设计", "分享", "周末", "音乐", "天气", "咖啡", "散步", "编程", "朋友",
            "电影", "旅行", "日常", "摄影", "星空", "故事", "灵感", "晚安", "城市",
        ];
        let corpus = words
            .iter()
            .enumerate()
            .flat_map(|(i, w)| std::iter::repeat_n(w.to_string(), 24 - i))
            .collect();
        let out = generate_word_cloud(corpus, None, None, 50, 800, 600).unwrap();
        let bytes = general_purpose::STANDARD
            .decode(out.trim_start_matches("base64://"))
            .unwrap();
        assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 1600);
        std::fs::write(format!("{dir}/wordcloud.png"), bytes).unwrap();
    }
}
