//! 回复里的内嵌图片：模型写 `![说明](地址)`，卡片把图按位置画进正文。
//!
//! 地址可以是公网链接、`data:` 内联图，或本机上的图片文件（模型刚用 bash 画出来的
//! 图表、截图）。三种都在这里读成一份大小受控的 data URL，随回复一起带出去——
//! 卡片的 CSP 只放行 `data:`，浏览器不替群友取任何地址。
//!
//! **为什么在 agent 层收尾时就取好，而不是等到渲染那一层**：房间每轮都有一个独占的
//! 临时目录，模型生成的东西多半落在那里，`conversation` 一返回它就被清掉了。等回复
//! 回到渲染那边，文件早已不在。回复正文（写进房间历史的那份）原样不动，图只在
//! [`Figures`] 里。
//!
//! 取不到的图不是静默丢掉：[`gather`] 会把失败原因交回给 agent 循环，让模型有一次
//! 机会改路径或删掉，再给最终回复；真到渲染时仍取不到的，卡片里留一个说明占位。

use crate::plugins::oai::LOG_TARGET;
use base64::Engine as _;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd};
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 一条回复最多嵌几张。再多的卡片会长到发不出去，也没有哪个问题真需要这么多图。
pub(crate) const MAX_FIGURES: usize = 6;

/// 原图的体积上限。
const MAX_SOURCE_BYTES: usize = 20 * 1024 * 1024;

/// 嵌进卡片的图，长边的像素上限。卡片版心 520 CSS px，四倍出图也只要 ~1900；
/// 1600 对默认的两倍已经绰绰有余，再大只是徒增体积。
const MAX_EDGE: u32 = 1600;

/// 不超过这个体积的 PNG / JPEG 原样放行（尺寸也在上限内），不重新编码。
const PASS_THROUGH_BYTES: usize = 1_500_000;

/// 一整条回复里所有图的 data URL 加起来的上限。
const MAX_TOTAL_BYTES: usize = 10 * 1024 * 1024;

/// 解码前先看头部给出的尺寸：像素数超过它就不解（全解开要 4 字节一个像素，手机上扛不住）。
const MAX_PIXELS: u64 = 36_000_000;

/// SVG 是文本，不经解码，只按体积限一道。
const MAX_SVG_BYTES: usize = 2 * 1024 * 1024;

/// 取一张图的总时限（含下载与转码）。
const LOAD_TIMEOUT: Duration = Duration::from_secs(25);

/// 重新编码 JPEG 的质量。
const JPEG_QUALITY: u8 = 88;

/// 卡片与取图必须按同一套规则解析 Markdown，否则「取到的图」与「卡片里要放的图」会对不上。
pub(crate) fn options() -> Options {
    Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
}

/// 这一条回复里取到的图：地址（原样，和正文里写的一致）→ data URL。
#[derive(Debug, Default, Clone)]
pub(crate) struct Figures {
    loaded: HashMap<String, String>,
    /// 没取到的图和原因，按正文里出现的顺序。
    failed: Vec<(String, String)>,
}

impl Figures {
    pub(crate) fn get(&self, source: &str) -> Option<&str> {
        self.loaded.get(source).map(String::as_str)
    }

    /// 没取到的图（含超出张数上限的）：`(地址, 原因)`。
    pub(crate) fn failures(&self) -> &[(String, String)] {
        &self.failed
    }

    #[cfg(test)]
    pub(crate) fn with(mut self, source: &str, data_url: &str) -> Self {
        self.loaded.insert(source.to_string(), data_url.to_string());
        self
    }
}

/// 正文里所有图片的地址，按首次出现的顺序去重。围栏代码块里的 `![](…)` 不算。
pub(crate) fn sources(markdown: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    Parser::new_ext(markdown, options())
        .filter_map(|event| match event {
            Event::Start(Tag::Image { dest_url, .. }) => Some(dest_url.to_string()),
            _ => None,
        })
        .filter(|source| !source.is_empty() && seen.insert(source.clone()))
        .collect()
}

/// 去掉正文里的图片（纯文本回退用）：有说明文字的留作 `〔图：说明〕`，没有的整个去掉。
pub(crate) fn without_images(markdown: &str) -> String {
    let mut out = String::new();
    let mut last = 0;
    let mut alt = String::new();
    let mut start = None;
    for (event, range) in Parser::new_ext(markdown, options()).into_offset_iter() {
        match event {
            Event::Start(Tag::Image { .. }) => {
                start = Some(range.start);
                alt.clear();
            }
            Event::Text(text) | Event::Code(text) if start.is_some() => alt.push_str(&text),
            Event::End(TagEnd::Image) => {
                if let Some(from) = start.take() {
                    out.push_str(&markdown[last..from]);
                    if !alt.trim().is_empty() {
                        out.push_str(&format!("〔图：{}〕", alt.trim()));
                    }
                    last = range.end;
                }
            }
            _ => {}
        }
    }
    out.push_str(&markdown[last..]);
    out
}

/// 把正文里的图片逐张取好。`roots` 是相对路径的查找根（先到先得）。
pub(crate) async fn gather(markdown: &str, roots: &[&Path]) -> Figures {
    let wanted = sources(markdown);
    let mut figures = Figures::default();
    if wanted.is_empty() {
        return figures;
    }
    let (take, over) = wanted.split_at(wanted.len().min(MAX_FIGURES));
    let results = futures_util::future::join_all(
        take.iter()
            .map(|source| async move { (source.clone(), load_one(source, roots).await) }),
    )
    .await;

    let mut total = 0;
    for (source, result) in results {
        match result {
            Ok(data_url) if total + data_url.len() <= MAX_TOTAL_BYTES => {
                total += data_url.len();
                figures.loaded.insert(source, data_url);
            }
            Ok(_) => figures
                .failed
                .push((source, "这条回复里的图加起来太大了".to_string())),
            Err(error) => {
                warn!(target: LOG_TARGET, "回复里的图片取不到 {}：{error:#}", shorten(&source));
                figures.failed.push((source, format!("{error:#}")));
            }
        }
    }
    for source in over {
        figures
            .failed
            .push((source.clone(), format!("一条回复最多嵌 {MAX_FIGURES} 张")));
    }
    figures
}

/// 取一张图：本机文件、公网链接或 `data:` 都行，带总时限。
///
/// 回复里的嵌图与 agent 自己的 `view_image` 工具走同一道准入与转码，所以「能嵌进卡片的」
/// 与「agent 能看见的」永远是同一批图。
pub(crate) async fn load_one(source: &str, roots: &[&Path]) -> anyhow::Result<String> {
    tokio::time::timeout(LOAD_TIMEOUT, load(source, roots))
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("{} 秒内没取完", LOAD_TIMEOUT.as_secs())))
}

/// 日志里的地址：data URL 可能有几 MB。
fn shorten(source: &str) -> String {
    crate::plugins::oai::utils::truncate_str(source, 80)
}

async fn load(source: &str, roots: &[&Path]) -> anyhow::Result<String> {
    let bytes = if let Some(rest) = source.strip_prefix("data:") {
        let (_, payload) = rest
            .split_once(";base64,")
            .ok_or_else(|| anyhow::anyhow!("只认 base64 的 data: 图片"))?;
        base64::engine::general_purpose::STANDARD
            .decode(payload.trim())
            .map_err(|_| anyhow::anyhow!("data: 图片的 base64 不合法"))?
    } else if source.starts_with("http://") || source.starts_with("https://") {
        crate::plugins::oai::search::fetch_public_bytes(
            source,
            MAX_SOURCE_BYTES,
            Duration::from_secs(20),
        )
        .await?
    } else {
        read_local(source, roots).await?
    };
    // 解码与缩放是 CPU 活，走共用的两条执行槽，别占住异步线程。
    crate::render::worker::run(move || normalize(&bytes))
        .await
        .map_err(|error| anyhow::anyhow!("图片处理中断：{error}"))?
}

/// 本机文件：`file://`、`~/`、绝对路径，或相对 `roots` 的路径；都找不到再试一遍百分号解码。
async fn read_local(source: &str, roots: &[&Path]) -> anyhow::Result<Vec<u8>> {
    let trimmed = source.strip_prefix("file://").unwrap_or(source);
    let mut spellings = vec![trimmed.to_string()];
    let decoded = percent_decode(trimmed);
    if decoded != trimmed {
        spellings.push(decoded);
    }
    let mut tried = Vec::new();
    for spelling in spellings {
        for candidate in candidates(&spelling, roots) {
            let Ok(meta) = tokio::fs::metadata(&candidate).await else {
                tried.push(candidate);
                continue;
            };
            if !meta.is_file() {
                anyhow::bail!("{} 不是文件", candidate.display());
            }
            if meta.len() > MAX_SOURCE_BYTES as u64 {
                anyhow::bail!("{} 超过 {} MB，太大了", candidate.display(), MAX_SOURCE_BYTES >> 20);
            }
            return tokio::fs::read(&candidate)
                .await
                .map_err(|error| anyhow::anyhow!("读取 {} 失败：{error}", candidate.display()));
        }
    }
    match tried.first() {
        Some(path) => anyhow::bail!("找不到文件 {}", path.display()),
        None => anyhow::bail!("找不到文件 {source}"),
    }
}

fn candidates(path: &str, roots: &[&Path]) -> Vec<PathBuf> {
    if let Some(rest) = path.strip_prefix("~/") {
        return std::env::var_os("HOME")
            .map(|home| vec![PathBuf::from(home).join(rest)])
            .unwrap_or_default();
    }
    let path = Path::new(path);
    if path.is_absolute() {
        return vec![path.to_path_buf()];
    }
    roots.iter().map(|root| root.join(path)).collect()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |at: usize| {
            bytes
                .get(at)
                .and_then(|byte| (*byte as char).to_digit(16))
        };
        match (bytes[i], hex(i + 1), hex(i + 2)) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                i += 3;
            }
            (byte, ..) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 原始字节 → 能放进卡片的 data URL。
///
/// 只认真正的图片：先按内容识别格式并解码，所以 `![](~/.ssh/id_rsa)` 这类写法读出来
/// 也只会得到一句「不是图片」——文件内容不会跟着卡片发出去。
fn normalize(bytes: &[u8]) -> anyhow::Result<String> {
    if looks_like_svg(bytes) {
        if bytes.len() > MAX_SVG_BYTES {
            anyhow::bail!("SVG 超过 {} MB", MAX_SVG_BYTES >> 20);
        }
        return Ok(data_url("image/svg+xml", bytes));
    }

    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| anyhow::anyhow!("读不了：{error}"))?;
    let format = reader
        .format()
        .ok_or_else(|| anyhow::anyhow!("不是图片（认不出格式）"))?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|error| anyhow::anyhow!("不是可用的图片：{error}"))?;
    // 解码炸弹：像素数先于分配被拦下，一张几万像素宽的小文件不能吃光内存。
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        anyhow::bail!("图片尺寸 {width}×{height} 太大");
    }
    let orientation = decoder.orientation().ok();
    let mut image = DynamicImage::from_decoder(decoder)
        .map_err(|error| anyhow::anyhow!("解码失败：{error}"))?;

    let fits = width.max(height) <= MAX_EDGE;
    if fits
        && bytes.len() <= PASS_THROUGH_BYTES
        && matches!(format, ImageFormat::Png | ImageFormat::Jpeg)
        // 带朝向标记的 JPEG 要转正后重新编码，不能指望每个浏览器都认。
        && orientation.is_none_or(|o| o == image::metadata::Orientation::NoTransforms)
    {
        return Ok(data_url(format.to_mime_type(), bytes));
    }

    if let Some(orientation) = orientation {
        image.apply_orientation(orientation);
    }
    if image.width().max(image.height()) > MAX_EDGE {
        image = image.resize(MAX_EDGE, MAX_EDGE, image::imageops::FilterType::Lanczos3);
    }
    let mut out = Vec::new();
    if has_transparency(&image) {
        image
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .map_err(|error| anyhow::anyhow!("编码失败：{error}"))?;
        Ok(data_url("image/png", &out))
    } else {
        let rgb = image.to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
            .encode_image(&rgb)
            .map_err(|error| anyhow::anyhow!("编码失败：{error}"))?;
        Ok(data_url("image/jpeg", &out))
    }
}

fn has_transparency(image: &DynamicImage) -> bool {
    match image {
        DynamicImage::ImageRgba8(pixels) => pixels.pixels().any(|pixel| pixel[3] < 255),
        other if other.color().has_alpha() => other.to_rgba8().pixels().any(|pixel| pixel[3] < 255),
        _ => false,
    }
}

/// 去掉 BOM 与空白后以 `<svg` 或 `<?xml` 起头，且前 2 KB 里真有 `<svg`。
fn looks_like_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(2048)];
    let Ok(text) = std::str::from_utf8(head).or_else(|error| std::str::from_utf8(&head[..error.valid_up_to()])) else {
        return false;
    };
    let text = text.trim_start_matches('\u{feff}').trim_start();
    (text.starts_with("<svg") || text.starts_with("<?xml") || text.starts_with("<!--") || text.starts_with("<!DOCTYPE svg"))
        && text.contains("<svg")
}

fn data_url(mime: &str, bytes: &[u8]) -> String {
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// 把图片放进事件流：独占一段的图是带图注的 `<figure>`，行内的图是行内 `<img>`，
/// 没取到的换成说明占位——**任何情况下都不让外部地址进卡片**。
pub(crate) fn place<'a>(events: Vec<Event<'a>>, figures: &Figures) -> Vec<Event<'a>> {
    let mut out = Vec::with_capacity(events.len());
    let mut i = 0;
    while i < events.len() {
        let Event::Start(Tag::Image { dest_url, .. }) = &events[i] else {
            out.push(events[i].clone());
            i += 1;
            continue;
        };
        // 找到配对的 End(Image)；图片不嵌套，说明文字里只有文本类事件。
        let mut alt = String::new();
        let mut end = i + 1;
        while end < events.len() && !matches!(events[end], Event::End(TagEnd::Image)) {
            if let Event::Text(text) | Event::Code(text) = &events[end] {
                alt.push_str(text);
            }
            end += 1;
        }
        let alone = matches!(out.last(), Some(Event::Start(Tag::Paragraph)))
            && matches!(events.get(end + 1), Some(Event::End(TagEnd::Paragraph)));
        let html = markup(figures.get(dest_url), alt.trim(), alone);
        if alone {
            // 去掉已经推出去的 `<p>`，连同后面的 `</p>` 一并换成一个块。
            out.pop();
            end += 1;
        }
        out.push(Event::Html(CowStr::from(html)));
        i = end + 1;
    }
    out
}

fn markup(data_url: Option<&str>, alt: &str, alone: bool) -> String {
    use crate::render::web::esc;
    match (data_url, alone) {
        (Some(src), true) => {
            let caption = if alt.is_empty() {
                String::new()
            } else {
                format!("<figcaption>{}</figcaption>", esc(alt))
            };
            format!(
                r#"<figure class="fig"><img src="{src}" alt="{}">{caption}</figure>"#,
                esc(alt)
            )
        }
        (Some(src), false) => format!(r#"<img class="fig-inline" src="{src}" alt="{}">"#, esc(alt)),
        (None, alone) => {
            let label = if alt.is_empty() {
                "图片没能加载".to_string()
            } else {
                format!("图：{alt}")
            };
            let tag = if alone { "div" } else { "span" };
            format!(r#"<{tag} class="fig-miss">〔{}〕</{tag}>"#, esc(&label))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([200, 60, 60, 255]));
        let mut out = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn sources_skip_code_blocks_and_dedupe() {
        let md = "![a](x.png) 文字 ![b](y.png) ![c](x.png)\n\n```\n![no](z.png)\n```\n";
        assert_eq!(sources(md), ["x.png", "y.png"]);
    }

    #[test]
    fn without_images_keeps_alt_as_text() {
        assert_eq!(
            without_images("前 ![折线图](a.png) 后 ![](b.png) 末"),
            "前 〔图：折线图〕 后  末"
        );
        // 代码块里的写法原样保留。
        assert!(without_images("```\n![x](y.png)\n```").contains("![x](y.png)"));
    }

    #[test]
    fn small_png_passes_through_unchanged() {
        let bytes = png(40, 30);
        let url = normalize(&bytes).unwrap();
        assert_eq!(url, data_url("image/png", &bytes));
    }

    #[test]
    fn huge_image_is_scaled_to_the_edge_limit() {
        let url = normalize(&png(3200, 800)).unwrap();
        let payload = url.split_once(";base64,").unwrap().1;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (MAX_EDGE, MAX_EDGE / 4));
        assert!(url.starts_with("data:image/jpeg"), "不透明的大图改存 JPEG");
    }

    #[test]
    fn transparent_image_stays_png_when_it_must_be_reencoded() {
        let image = image::RgbaImage::from_pixel(2000, 100, image::Rgba([0, 0, 0, 0]));
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        assert!(normalize(&bytes).unwrap().starts_with("data:image/png"));
    }

    #[test]
    fn only_real_images_get_through() {
        assert!(normalize(b"-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n").is_err());
        assert!(normalize(b"<html><body>hi</body></html>").is_err());
        assert!(normalize(b"").is_err());
        let svg = br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"/>"#;
        assert!(normalize(svg).unwrap().starts_with("data:image/svg+xml"));
    }

    #[test]
    fn pixel_bombs_are_refused_before_allocation() {
        // 宽高头写成 20000×20000 的 PNG，但没有像素数据。
        let mut bytes = png(1, 1);
        bytes[16..20].copy_from_slice(&20_000u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&20_000u32.to_be_bytes());
        let error = normalize(&bytes).unwrap_err().to_string();
        assert!(error.contains("太大") || error.contains("不是可用"), "{error}");
    }

    #[test]
    fn paths_resolve_against_each_root_and_percent_decoding() {
        let roots = [Path::new("/a"), Path::new("/b")];
        assert_eq!(
            candidates("c.png", &roots),
            [PathBuf::from("/a/c.png"), PathBuf::from("/b/c.png")]
        );
        assert_eq!(candidates("/x/y.png", &roots), [PathBuf::from("/x/y.png")]);
        assert_eq!(percent_decode("%E5%9B%BE%201.png"), "图 1.png");
        assert_eq!(percent_decode("a+b.png"), "a+b.png");
    }

    #[tokio::test]
    async fn gather_loads_local_files_and_reports_the_rest() {
        let dir = std::env::temp_dir().join(format!("acumen-figures-{}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ok.png"), png(10, 10)).unwrap();
        std::fs::write(dir.join("note.txt"), "不是图").unwrap();
        let md = "![好](ok.png) ![坏](nope.png) ![文](note.txt) ![内网](http://127.0.0.1:9/x.png)";
        let figures = gather(md, &[&dir]).await;
        assert!(figures.get("ok.png").is_some());
        let failed: Vec<&str> = figures.failures().iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(failed, ["nope.png", "note.txt", "http://127.0.0.1:9/x.png"]);
        assert!(figures.failures()[2].1.contains("内网"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn gather_caps_the_number_of_figures() {
        let md: String = (0..8).map(|i| format!("![](n{i}.png)\n\n")).collect();
        let figures = gather(&md, &[]).await;
        assert_eq!(figures.failures().len(), 8);
        assert!(figures.failures()[7].1.contains("最多"));
    }

    fn html_of(md: &str, figures: &Figures) -> String {
        let events: Vec<Event> = Parser::new_ext(md, options()).collect();
        let mut html = String::new();
        pulldown_cmark::html::push_html(&mut html, place(events, figures).into_iter());
        html
    }

    #[test]
    fn standalone_image_becomes_a_captioned_figure() {
        let figures = Figures::default().with("a.png", "data:image/png;base64,AAAA");
        let html = html_of("前\n\n![走势图](a.png)\n\n后", &figures);
        assert!(html.contains(r#"<figure class="fig"><img src="data:image/png;base64,AAAA" alt="走势图"><figcaption>走势图</figcaption></figure>"#), "{html}");
        assert!(!html.contains("<p><figure"), "{html}");
    }

    #[test]
    fn inline_and_missing_images_never_emit_a_foreign_src() {
        let figures = Figures::default().with("a.png", "data:image/png;base64,AAAA");
        let html = html_of("字 ![小](a.png) 字 ![丢了](http://x/y.png)\n\n![](nope.png)", &figures);
        assert!(html.contains(r#"<img class="fig-inline" src="data:"#), "{html}");
        assert!(html.contains("〔图：丢了〕"), "{html}");
        assert!(html.contains(r#"<div class="fig-miss">〔图片没能加载〕</div>"#), "{html}");
        assert!(!html.contains("http://x/y.png"), "{html}");
    }

    #[test]
    fn alt_text_is_escaped() {
        let figures = Figures::default().with("a.png", "data:image/png;base64,AAAA");
        let html = html_of(r#"![<b>"x"</b>](a.png)"#, &figures);
        assert!(!html.contains("<b>"), "{html}");
    }
}
