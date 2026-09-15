use super::stopwords::get_stop_words;
use araea_wordcloud::{WordCloudBuilder, WordInput};
use base64::{Engine as _, engine::general_purpose};
use rand::RngExt;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Instant;

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
        return Err("词云尺寸应在 64—2048 像素之间，面积不超过 400 万像素".into());
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
        .background("#FFFEFA")
        .colors(["#285E53", "#3E6578", "#785C40", "#536747", "#655A7B"])
        .padding(4);

    // 字体加载逻辑
    if let Some(path) = font_path {
        match std::fs::read(&path) {
            Ok(font_data) => {
                builder = builder.font(font_data);
            }
            Err(e) => {
                return Err(format!("加载字体文件失败: {} - {}", path, e));
            }
        }
    } else if let Some(family) = font_family {
        match load_font_by_family(&family) {
            Ok(font_data) => {
                builder = builder.font(font_data);
            }
            Err(e) => {
                warn!(target: "Plugin/WordCloud", "加载系统字体 [{}] 失败: {}，将尝试默认方案", family, e);
            }
        }
    }

    let wordcloud = builder
        .angles(vec![0.0])
        .vertical_writing(false)
        .build(&top_words)
        .map_err(|e| format!("Build Error: {}", e))?;

    let png_data = wordcloud
        .to_png(2.0)
        .map_err(|e| format!("PNG Encode Error: {}", e))?;

    let b64_str = general_purpose::STANDARD.encode(&png_data);
    info!(target: "Plugin/WordCloud", "Generated in {:?}", start.elapsed());

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
        .ok_or_else(|| format!("未找到匹配的字体族: {}", family))?;

    // with_face_data 会自动处理文件 IO 或内存引用，并返回闭包的结果
    db.with_face_data(id, |data, _face_index| data.to_vec())
        .ok_or_else(|| "无法获取字体数据".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
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
