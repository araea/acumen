use super::stopwords::get_stop_words;
use araea_wordcloud::{WordCloudBuilder, WordInput};
use base64::{Engine as _, engine::general_purpose};
use image::{ImageFormat, RgbImage};
use rand::RngExt;
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::OnceLock;
use std::time::Instant;

/// 词云的纸色。取设计系统的卡面（`res/cards/m3e.css` 的 `scheme-manual`），
/// 与六张卡片、与统计图同一张纸——词云常常就插在一张统计卡片后面。
const PAPER: &str = "#FFFEFA";

/// 纸色的三个通道。裁留白要拿它当「空」，写成 const 是为了不在扫描循环里解析字符串。
const PAPER_RGB: [u8; 3] = hex_rgb(PAPER);

const fn hex_rgb(hex: &str) -> [u8; 3] {
    let bytes = hex.as_bytes();
    let mut out = [0u8; 3];
    let mut i = 0;
    while i < 3 {
        out[i] = nibble(bytes[1 + i * 2]) * 16 + nibble(bytes[2 + i * 2]);
        i += 1;
    }
    out
}

const fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 0,
    }
}

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

    let trimmed = trim_paper_ring(&png_data);
    let final_data = match &trimmed {
        Ok(Some(bytes)) => bytes.as_slice(),
        Ok(None) => png_data.as_slice(),
        // 裁切只是观感上的补救，失败了就发原图，不该让一条词云整个变成报错。
        Err(e) => {
            warn!(target: super::LOG_TARGET, "裁留白失败，改发原图：{}", e);
            png_data.as_slice()
        }
    };

    let b64_str = general_purpose::STANDARD.encode(final_data);
    info!(
        target: super::LOG_TARGET,
        "Generated in {:?} ({} bytes)",
        start.elapsed(),
        final_data.len()
    );

    Ok(format!("base64://{}", b64_str))
}

/// 内容与画布边之间留的呼吸位：词云的词是按 4px 的空隙排的，裁完补上两倍于它的一圈，
/// 让贴边的字不至于顶在图片边框上。以内容短边为基准取 1.5%，再夹在 12—48 像素之间——
/// 下限管小图（不要让裁完的图边上一点余量都没有），上限管大图（不要又撑出一圈白）。
fn trim_margin(content_short_side: u32) -> u32 {
    ((content_short_side as f32 * 0.015) as u32).clamp(12, 48)
}

/// 这个像素算不算「有东西」。
///
/// 阈值 12 是给抗锯齿留的：字缘从纸色渐变成词色，覆盖度一成多的像素已经看不见了，
/// 把它们留在边界外，才不会有肉眼可见的一行被切掉；同时纸色（#FFFEFA）与五个色相
/// 相距都在 90 以上，12 不会把空白误判成内容。
fn is_ink(px: &[u8; 3], paper: [u8; 3]) -> bool {
    px[0].abs_diff(paper[0])
        .max(px[1].abs_diff(paper[1]))
        .max(px[2].abs_diff(paper[2]))
        > 12
}

/// 内容的最小外接矩形 `(x, y, w, h)`；整张都是纸色时返回 `None`。
fn content_bounds(img: &RgbImage) -> Option<(u32, u32, u32, u32)> {
    let (width, height) = img.dimensions();
    let paper = PAPER_RGB;
    let stride = width as usize * 3;
    let raw = img.as_raw();
    let (mut min_x, mut min_y) = (width, height);
    let (mut max_x, mut max_y) = (0u32, 0u32);

    for y in 0..height as usize {
        let row = &raw[y * stride..(y + 1) * stride];
        let mut first = None;
        let mut last = 0u32;
        for (x, px) in row.as_chunks::<3>().0.iter().enumerate() {
            if is_ink(px, paper) {
                if first.is_none() {
                    first = Some(x as u32);
                }
                last = x as u32;
            }
        }
        let Some(first) = first else { continue };
        min_x = min_x.min(first);
        max_x = max_x.max(last);
        min_y = min_y.min(y as u32);
        max_y = y as u32;
    }

    (min_x <= max_x).then(|| (min_x, min_y, max_x - min_x + 1, max_y - min_y + 1))
}

/// 把画布四周的纸色裁掉，只留 `trim_margin` 那么厚的一圈。
///
/// 词是绕着画布中心沿一条螺旋线摆开的，内容天然是一团靠中间的块：词少的时候
/// 800×600 的画布上能空出近一半的暖白，在群里刷过去就是「一张白图中间挤着几个字」。
/// 裁到内容边界之后，成图多大由内容决定，画布只决定词排得开不开。
///
/// 返回 `None` 表示四周的留白本来就够厚（或者内容铺满了画布），调用方直接用原图。
fn trim_paper_ring(png_data: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let img = image::load_from_memory(png_data)
        .map_err(|e| format!("解码失败：{}", e))?
        .to_rgb8();

    let Some(img) = trim_blank(&img) else {
        return Ok(None);
    };

    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png)
        .map_err(|e| format!("重新编码失败：{}", e))?;
    Ok(Some(out.into_inner()))
}

/// 裁留白本身：内容多小就留多小，四周补一圈纸色。
///
/// 补不出来的那一侧（内容本来就快贴到画布边了）就保持原样：这一刀只在画布内挪，
/// 成图不会比设定的画布还大，也不会把已经排好的词挤出去。
fn trim_blank(img: &RgbImage) -> Option<RgbImage> {
    let (x, y, w, h) = content_bounds(img)?;
    let margin = trim_margin(w.min(h));

    let left = x.saturating_sub(margin);
    let top = y.saturating_sub(margin);
    let right = (x + w + margin).min(img.width());
    let bottom = (y + h + margin).min(img.height());

    // 四周本来就有一圈够厚的留白：不必解码再编码一趟。
    if (left, top, right, bottom) == (0, 0, img.width(), img.height()) {
        return None;
    }

    Some(image::imageops::crop_imm(img, left, top, right - left, bottom - top).to_image())
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
    use image::{GenericImageView, Rgb};

    /// 五十个词，权重递减。用作样张，以及「词多到快铺满画布」那一侧的样本。
    const SAMPLE_WORDS: [&str; 50] = [
        "生活", "阅读", "设计", "分享", "周末", "音乐", "天气", "咖啡", "散步", "编程", "朋友",
        "电影", "旅行", "日常", "摄影", "星空", "故事", "灵感", "晚安", "城市", "考试", "加班",
        "开会", "外卖", "快递", "游戏", "猫", "狗", "地铁", "机票", "医院", "作业", "论文", "面试",
        "房租", "超市", "火锅", "奶茶", "健身", "旅游", "动画", "漫画", "耳机", "键盘", "显示器",
        "显卡", "手机", "充电", "雨伞", "口罩",
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
                sheet.contains(color),
                "{color} 不在 res/cards/m3e.css 里：词云的配色要取自系统那张色表"
            );
        }
        // 纸色被 const 解析过一次，裁切拿的就是它；解析错了整张图的边界就全错。
        assert_eq!(PAPER_RGB, [0xFF, 0xFE, 0xFA]);
    }

    #[test]
    fn rejects_oversized_cloud_before_allocating() {
        assert!(generate_word_cloud(vec![], None, None, 50, u32::MAX, 600).is_err());
    }

    /// 内容边界就是墨迹本身，纸色一点不算。
    #[test]
    fn the_bounds_are_the_ink_extent() {
        let mut img = RgbImage::from_pixel(100, 80, Rgb(PAPER_RGB));
        for y in 30..33 {
            for x in 40..44 {
                img.put_pixel(x, y, Rgb([0x1F, 0x63, 0x50]));
            }
        }
        assert_eq!(content_bounds(&img), Some((40, 30, 4, 3)));
        assert_eq!(
            content_bounds(&RgbImage::from_pixel(10, 10, Rgb(PAPER_RGB))),
            None
        );
    }

    /// 裁完的结果是「内容 + 一圈固定厚度的纸」，而不是把内容顶到边上。
    #[test]
    fn trimming_leaves_a_margin_of_paper() {
        let mut img = RgbImage::from_pixel(100, 80, Rgb(PAPER_RGB));
        for y in 30..33 {
            for x in 40..44 {
                img.put_pixel(x, y, Rgb([0x1F, 0x63, 0x50]));
            }
        }

        let margin = trim_margin(3);
        let trimmed = trim_blank(&img).expect("四周都是纸，该裁");
        assert_eq!(trimmed.dimensions(), (4 + margin * 2, 3 + margin * 2));
        assert_eq!(content_bounds(&trimmed), Some((margin, margin, 4, 3)));
    }

    /// 内容铺满画布时不动它：没有可裁的，也不该平白多出一圈白。
    #[test]
    fn a_full_canvas_is_left_alone() {
        let img = RgbImage::from_pixel(64, 64, Rgb([0x1F, 0x63, 0x50]));
        assert!(trim_blank(&img).is_none());
    }

    /// 内容已经贴着画布边时，这一侧补不出留白，但也不许把墨迹切掉。
    #[test]
    fn a_side_that_has_no_room_keeps_its_pixels() {
        let mut img = RgbImage::from_pixel(100, 80, Rgb(PAPER_RGB));
        for y in 0..4 {
            for x in 0..3 {
                img.put_pixel(x, y, Rgb([0x1F, 0x63, 0x50]));
            }
        }

        let trimmed = trim_blank(&img).expect("右下有大片留白，该裁");
        assert_eq!(content_bounds(&trimmed), Some((0, 0, 3, 4)));
        assert!(
            trimmed.width() <= 100 && trimmed.height() <= 80,
            "成图不该比画布还大：{:?}",
            trimmed.dimensions()
        );
    }

    /// 出图之后四周不该再有一大圈纸色：裁完的墨迹必须恰好离四边 `trim_margin`。
    ///
    /// 这就是用户看到的那件事——词少的时候画布大半是空的，群里刷过去是一张白图；
    /// 裁完之后同样十个词，成图跟着内容缩到内容大小。
    #[test]
    fn a_sparse_cloud_comes_back_without_the_paper_ring() {
        let out = generate_word_cloud(corpus_of(&SAMPLE_WORDS[..10]), None, None, 50, 800, 600)
            .unwrap();

        let img = image::load_from_memory(&decode(&out)).unwrap().to_rgb8();
        assert!(
            img.width() < 1600 && img.height() < 1200,
            "十个词不该铺满 1600×1200：{:?}",
            img.dimensions()
        );

        let (x, y, w, h) = content_bounds(&img).expect("图里得有词");
        let margin = trim_margin(w.min(h));
        assert_eq!((x, y), (margin, margin), "四边留白不等厚");
        assert_eq!(img.dimensions(), (w + margin * 2, h + margin * 2));
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
