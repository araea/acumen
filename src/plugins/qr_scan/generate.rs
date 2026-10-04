//! 文字转二维码：把一段内容编成一张好扫的图。
//!
//! 取舍都朝「扫得出来」去：纯黑白、方格按整数像素放大（不插值、边缘不糊）、四周留足
//! 四格静区，大小按格数挑，小码不至于糊在一团、密码也不至于大到发不出去。不上色、不加 logo、
//! 不做圆点——花样每多一样，就有一种扫码器会读不出来。
//!
//! 唯一替人做的主意是把「裸域名」补上 `https://`：绝大多数人贴 `github.com/araea` 是想让别人
//! 扫了直接打开，而不少扫码器会把没有协议头的当成纯文字。补了会在回复里说一声，
//! 并把补完的整串原样放在单独一行，方便复制。

use image::{GrayImage, ImageFormat, Luma};
use qrcode::bits::Bits;
use qrcode::{Color, EcLevel, QrCode, Version};
use std::io::Cursor;

/// 静区格数。规范要求至少 4，少了就有扫码器找不到边。
const QUIET: u32 = 4;
/// 图的目标边长（像素）。微信、QQ 的聊天里看着舒服，放大到全屏也不糊。
const TARGET_SIDE: u32 = 720;
/// 每格至少几个像素。再小，聊天软件一压缩就糊成灰。
const MIN_MODULE: u32 = 6;
const MAX_MODULE: u32 = 28;
/// 格数到这个量级（约版本 17 往上）就算「密」：手机拍屏幕时要靠近、对准。
const DENSE_MODULES: usize = 85;

/// 常见的顶级域名。只在这些里才认「裸域名」，免得把 `a.sh`、`main.rs`、`notes.md`
/// 这类文件名当网址。
const TLDS: &[&str] = &[
    "com", "cn", "net", "org", "io", "cc", "me", "top", "xyz", "dev", "app", "ai", "co", "tv",
    "info", "edu", "gov", "biz", "site", "online", "tech", "club", "vip", "pro", "wang", "link",
    "ltd", "fun", "live", "store", "shop", "cloud", "art", "one", "icu", "ink", "ren", "work",
    "red", "mobi", "asia", "us", "uk", "jp", "kr", "hk", "tw", "de", "fr", "ru", "sg", "eu",
    "ly", "gg", "to", "im", "fm", "ws", "pw",
];

/// 整理过、真正要编码的内容。
#[derive(Debug, PartialEq, Eq)]
pub struct Prepared {
    pub text: String,
    /// 裸域名被补上了 `https://`。
    pub completed: bool,
}

/// 去掉首尾空白；单独一串的裸域名补上协议头。其余一个字不改。
pub fn prepare(raw: &str) -> Prepared {
    let text = raw.trim();
    if is_bare_domain(text) {
        return Prepared {
            text: format!("https://{text}"),
            completed: true,
        };
    }
    Prepared {
        text: text.to_string(),
        completed: false,
    }
}

/// `github.com`、`www.example.cn/a?b=1`、`example.com:8080/x` 这样没写协议头的网址。
fn is_bare_domain(text: &str) -> bool {
    if text.is_empty() || text.contains(char::is_whitespace) || text.contains('@') {
        return false;
    }
    // 域名（可带端口）到第一个 / ? # 为止；后面的路径与参数不看。
    let authority = text.split(['/', '?', '#']).next().unwrap_or("");
    let host = match authority.split_once(':') {
        // `mailto:`、`tel:`、`WIFI:T:…`、`http:` 都落在这里：冒号后不是纯数字端口。
        Some((host, port)) => {
            if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
                return false;
            }
            host
        }
        None => authority,
    };
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 || !labels.iter().all(|label| is_label(label)) {
        return false;
    }
    let tld = labels[labels.len() - 1].to_ascii_lowercase();
    if !tld.bytes().all(|b| b.is_ascii_alphabetic()) {
        return false;
    }
    TLDS.contains(&tld.as_str())
        || (labels.len() >= 3 && labels[0].eq_ignore_ascii_case("www") && tld.len() >= 2)
}

fn is_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// 编出来的图与它的规格。
#[derive(Debug)]
pub struct Rendered {
    pub png: Vec<u8>,
    /// 码每边几格（不含静区）。
    pub modules: usize,
}

impl Rendered {
    pub fn is_dense(&self) -> bool {
        self.modules >= DENSE_MODULES
    }
}

/// 内容太多，一个二维码装不下。
#[derive(Debug, PartialEq, Eq)]
pub struct TooLong {
    pub chars: usize,
}

/// 编成二维码图（PNG）。纠错先取 M（损一成多仍能读）；装不下再退到 L，换最大的容量。
pub fn render(text: &str) -> Result<Rendered, TooLong> {
    let code = [EcLevel::M, EcLevel::L]
        .into_iter()
        .find_map(|level| encode(text, level))
        .ok_or_else(|| TooLong {
            chars: text.chars().count(),
        })?;
    Ok(Rendered {
        modules: code.width(),
        png: draw(&code),
    })
}

/// 挑最小的版本把内容装进去。装不下返回 `None`。
///
/// **不能交给 `qrcode` 的自动分段**：它会把 UTF-8 里碰巧落在 Shift-JIS 汉字区的字节对
/// （中文几乎处处是）切成 Kanji 模式，扫码器按日文解码，中文就成了乱码——短到二十来个字
/// 就会发生。所以这里只用数字、字母数字、字节三种模式，整串挑一种。
fn encode(text: &str, level: EcLevel) -> Option<QrCode> {
    let data = text.as_bytes();
    let numeric = data.iter().all(u8::is_ascii_digit);
    let alphanumeric = data.iter().all(|&b| is_alphanumeric_mode(b));
    (1..=40).find_map(|version| {
        let mut bits = Bits::new(Version::Normal(version));
        if numeric {
            bits.push_numeric_data(data)
        } else if alphanumeric {
            bits.push_alphanumeric_data(data)
        } else {
            bits.push_byte_data(data)
        }
        .ok()?;
        bits.push_terminator(level).ok()?;
        QrCode::with_bits(bits, level).ok()
    })
}

/// 字母数字模式的字符集：数字、大写字母与 ` $%*+-./:`。
fn is_alphanumeric_mode(byte: u8) -> bool {
    byte.is_ascii_digit() || byte.is_ascii_uppercase() || b" $%*+-./:".contains(&byte)
}

fn draw(code: &QrCode) -> Vec<u8> {
    let n = code.width() as u32;
    let total = n + QUIET * 2;
    let module = (TARGET_SIDE as f32 / total as f32)
        .round()
        .clamp(MIN_MODULE as f32, MAX_MODULE as f32) as u32;
    let colors = code.to_colors();
    let side = total * module;
    let image = GrayImage::from_fn(side, side, |x, y| {
        let (cx, cy) = (x / module, y / module);
        let dark = cx >= QUIET
            && cy >= QUIET
            && cx < QUIET + n
            && cy < QUIET + n
            && colors[((cy - QUIET) * n + (cx - QUIET)) as usize] == Color::Dark;
        Luma([if dark { 0 } else { 255 }])
    });
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .expect("写入内存里的 PNG 不会失败");
    png
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::qr_scan::scan;
    use std::time::{Duration, Instant};

    fn decode(png: &[u8]) -> Vec<String> {
        let gray = image::load_from_memory(png).unwrap().to_luma8();
        let limits = scan::Limits {
            max_codes: 4,
            deadline: Instant::now() + Duration::from_secs(60),
        };
        scan::scan(&gray, limits)
            .into_iter()
            .map(|code| code.text)
            .collect()
    }

    /// 生成的图扫回来必须与输入一字不差：中文、emoji、换行、符号、网址里的 & 都是。
    #[test]
    fn what_goes_in_is_what_scans_out() {
        for text in [
            "hello",
            "https://github.com/araea/acumen",
            "https://example.com/search?q=二维码&lang=zh-CN#top",
            "你好，世界！这是一段中文。",
            "第一行\n第二行\n\n第四行",
            "emoji 🎉 与 ünïcödé",
            "WIFI:T:WPA;S:MyHome;P:secret123;;",
            "BEGIN:VCARD\nVERSION:3.0\nFN:张三\nTEL:13800000000\nEND:VCARD",
            "1234567890",
            "A",
        ] {
            let rendered = render(text).unwrap();
            assert_eq!(decode(&rendered.png), [text], "{text:?}");
        }
    }

    /// 回归：`qrcode` 自带的自动分段会把中文的 UTF-8 字节误判成 Shift-JIS 汉字，
    /// 扫出来前半段对、后半段是乱码。这条句子在 28 个字时就会中招。
    #[test]
    fn chinese_is_never_mistaken_for_kanji() {
        let text = "这是一段比较长的话，用来看二维码变密以后还扫不扫得出来。";
        assert_eq!(decode(&render(text).unwrap().png), [text]);
        for repeat in [3, 12, 30] {
            let text = text.repeat(repeat);
            let rendered = render(&text).unwrap();
            assert_eq!(decode(&rendered.png), [text], "重复 {repeat} 次");
        }
    }

    /// 常用汉字、日文假名、全角符号混排，长短不一，逐条读回——不靠某句碰巧过关。
    #[test]
    fn assorted_cjk_text_round_trips() {
        // 确定性的伪随机，免得测试时好时坏。
        let mut state = 20_261_004u32;
        let mut next = move |n: u32| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 8) % n
        };
        for length in [1, 2, 5, 9, 16, 28, 50, 90, 160, 300] {
            let text: String = (0..length)
                .map(|_| match next(10) {
                    0 => char::from_u32(0x3041 + next(80)).unwrap(),
                    1 => ['，', '。', '！', '？', '、', '：', '（', '）'][next(8) as usize],
                    2 => char::from_u32(0x20 + next(0x5f)).unwrap(),
                    _ => char::from_u32(0x4e00 + next(0x51a5)).unwrap(),
                })
                .collect();
            let rendered = render(&text).unwrap();
            assert_eq!(decode(&rendered.png), [text.clone()], "长 {length}：{text}");
        }
    }

    /// 纯数字与纯大写字母数字走更省地方的模式，格数比字节模式少。
    #[test]
    fn digits_and_capitals_use_the_denser_modes() {
        let digits = "1234567890".repeat(8);
        let capitals = "HELLO WORLD 2026 ".repeat(5).trim_end().to_string();
        let bytes = "hello world 2026 ".repeat(5).trim_end().to_string();
        let (d, c, b) = (render(&digits).unwrap(), render(&capitals).unwrap(), render(&bytes).unwrap());
        assert_eq!(decode(&d.png), [digits.clone()]);
        assert_eq!(decode(&c.png), [capitals.clone()]);
        assert!(d.modules < c.modules && c.modules < b.modules, "{} {} {}", d.modules, c.modules, b.modules);
    }

    /// 由 `prepare` 补过协议头的网址，扫出来的就是补完的整串。
    #[test]
    fn a_completed_link_scans_back_as_the_completed_link() {
        let prepared = prepare("  github.com/araea/acumen \n");
        let rendered = render(&prepared.text).unwrap();
        assert_eq!(decode(&rendered.png), ["https://github.com/araea/acumen"]);
    }

    #[test]
    fn small_and_dense_codes_are_both_sized_to_be_readable() {
        // 小码：放大到接近目标边长；密码：每格不少于下限。
        let small = image::load_from_memory(&render("A").unwrap().png).unwrap();
        assert!((600..=740).contains(&small.width()), "{}", small.width());
        assert_eq!(small.width(), small.height());
        let long = "https://example.com/".to_string() + &"a1b2c3d4e5".repeat(60);
        let dense = render(&long).unwrap();
        assert!(dense.is_dense());
        let image = image::load_from_memory(&dense.png).unwrap();
        let total = dense.modules as u32 + QUIET * 2;
        assert!(image.width() >= total * MIN_MODULE);
        assert_eq!(decode(&dense.png), [long]);
        // 一般长度的内容不算密。
        assert!(!render("https://github.com/araea/acumen").unwrap().is_dense());
    }

    /// 四周必须有整整四格的纯白静区，且只有黑白两色。
    #[test]
    fn the_quiet_zone_is_white_and_the_picture_is_black_and_white() {
        let rendered = render("quiet").unwrap();
        let image = image::load_from_memory(&rendered.png).unwrap().to_luma8();
        let total = rendered.modules as u32 + QUIET * 2;
        let module = image.width() / total;
        let margin = QUIET * module;
        for (x, y, pixel) in image.enumerate_pixels() {
            assert!(pixel.0[0] == 0 || pixel.0[0] == 255);
            if x < margin || y < margin || x >= image.width() - margin || y >= image.height() - margin {
                assert_eq!(pixel.0[0], 255, "({x},{y})");
            }
        }
    }

    #[test]
    fn the_largest_capacity_is_used_before_giving_up() {
        // 两千多字节：M 级装不下，退到 L 能装。
        let big = "x".repeat(2500);
        let rendered = render(&big).unwrap();
        assert!(rendered.modules > 150, "{}", rendered.modules);
        assert_eq!(decode(&rendered.png), [big]);
        // 再多就真装不下了。
        let too_big = "汉".repeat(1100);
        assert_eq!(render(&too_big).unwrap_err(), TooLong { chars: 1100 });
    }

    #[test]
    fn bare_domains_get_a_protocol_and_nothing_else_is_touched() {
        for (raw, expected) in [
            ("github.com", "https://github.com"),
            ("github.com/araea/acumen", "https://github.com/araea/acumen"),
            ("  www.baidu.com  ", "https://www.baidu.com"),
            ("Example.CN/a?b=1#c", "https://Example.CN/a?b=1#c"),
            ("example.com:8080/x", "https://example.com:8080/x"),
            ("b23.tv/AbCdEf", "https://b23.tv/AbCdEf"),
            ("a.b.c.example.com", "https://a.b.c.example.com"),
            ("www.some-site.xyz", "https://www.some-site.xyz"),
        ] {
            let prepared = prepare(raw);
            assert_eq!(prepared.text, expected, "{raw}");
            assert!(prepared.completed, "{raw}");
        }
        for raw in [
            "https://github.com",
            "http://example.com/a",
            "HTTPS://EXAMPLE.COM",
            "ftp://files.example.com",
            "mailto:me@example.com",
            "me@example.com",
            "tel:13800000000",
            "WIFI:T:WPA;S:a.com;P:x;;",
            "hello world",
            "你好.com 这是什么",
            "github.com 是个网站",
            "main.rs",
            "script.sh",
            "notes.md",
            "v1.2.3",
            "3.14",
            "localhost:8080",
            "-bad-.com",
            "a..com",
            ".com",
            "com",
            "",
            "   ",
        ] {
            let prepared = prepare(raw);
            assert_eq!(prepared.text, raw.trim(), "{raw}");
            assert!(!prepared.completed, "{raw}");
        }
    }

    /// 内容原样：只去首尾空白，中间的换行与空格不动。
    #[test]
    fn inner_whitespace_survives() {
        assert_eq!(prepare("\n  第一行\n  第二行  \n").text, "第一行\n  第二行");
    }
}
