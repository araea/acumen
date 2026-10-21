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
