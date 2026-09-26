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

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GenericImageView, RgbImage};

    /// 纸色的通道值：测试拿它当「空」，用来量墨迹到图边的距离。
    const PAPER_RGB: [u8; 3] = [0xFF, 0xFE, 0xFA];

    /// 图里墨迹的最小外接矩形 `(x, y, w, h)`；整张都是纸色时返回 `None`。
    ///
    /// 裁切已交给 araea-wordcloud 在布局层做，这里只留一把量尺，验证成图四周
    /// 没有多余的纸环。
    fn ink_extent(img: &RgbImage) -> Option<(u32, u32, u32, u32)> {
        let (width, height) = img.dimensions();
        let (mut min_x, mut min_y) = (width, height);
        let (mut max_x, mut max_y) = (0u32, 0u32);

        for y in 0..height {
            for x in 0..width {
                let px = img.get_pixel(x, y).0;
                let diff = px[0]
                    .abs_diff(PAPER_RGB[0])
                    .max(px[1].abs_diff(PAPER_RGB[1]))
                    .max(px[2].abs_diff(PAPER_RGB[2]));
                if diff > 12 {
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
        }

        (min_x <= max_x).then(|| (min_x, min_y, max_x - min_x + 1, max_y - min_y + 1))
    }

    /// 五十个词，权重递减。用作样张，以及「词多到快铺满画布」那一侧的样本。
    const SAMPLE_WORDS: [&str; 50] = [
        "生活",
        "阅读",
        "设计",
        "分享",
        "周末",
        "音乐",
        "天气",
        "咖啡",
        "散步",
        "编程",
        "朋友",
        "电影",
        "旅行",
        "日常",
        "摄影",
        "星空",
        "故事",
        "灵感",
        "晚安",
        "城市",
        "考试",
        "加班",
        "开会",
        "外卖",
        "快递",
        "游戏",
        "猫",
        "狗",
        "地铁",
        "机票",
        "医院",
        "作业",
        "论文",
        "面试",
        "房租",
        "超市",
        "火锅",
        "奶茶",
        "健身",
        "旅游",
        "动画",
        "漫画",
        "耳机",
        "键盘",
        "显示器",
        "显卡",
        "手机",
        "充电",
        "雨伞",
        "口罩",
    ];

    /// 第 i 个词出现 `len - i` 次：大字小字都有，和群里的长尾分布差不多。
    fn corpus_of(words: &[&str]) -> Vec<String> {
        words
            .iter()
            .enumerate()
            .flat_map(|(i, w)| std::iter::repeat_n(w.to_string(), words.len() - i))
            .collect()
    }

    fn decode(out: &str) -> Vec<u8> {
        general_purpose::STANDARD
            .decode(out.trim_start_matches("base64://"))
            .unwrap()
    }

    /// 词云用的六个色值（纸色 + 五个色相）都得在设计系统的样式表里找得到。
    ///
    /// 这条测试挡的是「顺手加一个好看的颜色」：加了就不再是同一套系统，
    /// 而词云与卡片经常同时出现在一条消息里，一眼能看出不是一个人做的。
    #[test]
    fn the_word_hues_come_from_the_design_system() {
        let sheet = crate::render::web::DESIGN_SYSTEM.to_ascii_uppercase();
        for color in std::iter::once(PAPER).chain(WORD_COLORS) {
            assert!(
                sheet.contains(&color.to_ascii_uppercase()),
                "{color} 不在 res/cards/m3e.css 里：词云的配色要取自系统那张色表"
            );
        }
    }

    #[test]
    fn rejects_oversized_cloud_before_allocating() {
        assert!(generate_word_cloud(vec![], None, None, 50, u32::MAX, 600).is_err());
    }

    /// 出图之后四周不该再有一大圈纸色：墨迹到四边就是一个 `PADDING + TRIM_MARGIN`。
    ///
    /// 这就是用户看到的那件事——词少的时候画布大半是空的，群里刷过去是一张白图；
    /// 裁完之后同样十个词，成图跟着内容缩到内容大小。
    #[test]
    fn a_sparse_cloud_comes_back_without_the_paper_ring() {
        let out =
            generate_word_cloud(corpus_of(&SAMPLE_WORDS[..10]), None, None, 50, 800, 600).unwrap();

        let img = image::load_from_memory(&decode(&out)).unwrap().to_rgb8();
        assert!(
            img.width() < 1600 && img.height() < 1200,
            "十个词不该铺满 1600×1200：{:?}",
            img.dimensions()
        );

        // 裁切在画布上按 `PADDING + TRIM_MARGIN` 算，出图放大两倍；字缘的抗锯齿让
        // 最外一圈墨迹有一两个像素的出入。
        let expected = (PADDING + TRIM_MARGIN) * 2;
        let (x, y, w, h) = ink_extent(&img).expect("图里得有词");
        let edges = [x, y, img.width() - (x + w), img.height() - (y + h)];
        for edge in edges {
            assert!(
                (PADDING * 2 - 3..=expected + 3).contains(&edge),
                "墨迹离边 {edge}px，预期 {expected}px 上下；四边为 {edges:?}"
            );
        }
    }

    #[test]
    #[ignore = "生成本地词云样张"]
    fn dump_sample_cards() {
        let Ok(dir) = std::env::var("WORDCLOUD_CARD_DUMP") else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();

        // 两张：五十个词的满档，与十个词的稀疏档。后者从前是一张中间一小团字的白图，
        // 放在一起，「留白裁掉没有」一眼就能看出来。
        for (name, words) in [
            ("wordcloud.png", &SAMPLE_WORDS[..]),
            ("wordcloud-sparse.png", &SAMPLE_WORDS[..10]),
        ] {
            let out = generate_word_cloud(corpus_of(words), None, None, 50, 800, 600).unwrap();
            let bytes = decode(&out);
            let img = image::load_from_memory(&bytes).unwrap();
            assert!(
                img.width() <= 1600 && img.height() <= 1200,
                "样张不该比画布大：{:?}",
                img.dimensions()
            );
            std::fs::write(format!("{dir}/{name}"), bytes).unwrap();
        }
    }
}
