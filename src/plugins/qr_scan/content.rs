//! 二维码里的内容长什么样，怎么摆给人看。
//!
//! 二维码只是一串字：绝大多数是链接，也有 Wi-Fi、电话、名片、两步验证密钥……这里先认出
//! 是哪一类，再摆成「一行说是什么、下面是内容」的样子。内容本身**原样放在单独一行**，
//! 方便长按复制、QQ 也能自己认出链接——所以展示用文字而不是图片：图里的链接复制不了，
//! 那就失去了「转链接」的意义。
//!
//! 两处刻意的取舍：
//! - 两步验证（`otpauth://`）的二维码里就是密钥本身，**不回显**，只说它是什么。
//!   群里贴一个这样的码，机器人若原样转成文字，等于替人把密钥公开了第二遍。
//! - `javascript:`、`data:`、`file:` 这类不是网页的「链接」单独标出来，别让人顺手点开。

use url::Url;

/// 一个二维码摆出来的样子。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub icon: &'static str,
    /// 一行标题：是什么、来自哪。
    pub title: String,
    /// 正文，一行一条；链接与文字原样一行。
    pub lines: Vec<String>,
}

/// 单条内容最多摆多少字，超出截断并写明全长。
const MAX_CHARS: usize = 800;

/// 认出内容的类别，摆成条目。
pub fn describe(raw: &str) -> Entry {
    let text = raw.trim();
    if text.is_empty() {
        return plain("📝", "文本", vec!["（内容为空）".into()]);
    }
    let lower = text.to_ascii_lowercase();

    if let Some(entry) = web_link(text, &lower) {
        return entry;
    }
    if lower.starts_with("wifi:") {
        return wifi(&text[5..]);
    }
    if lower.starts_with("otpauth://") || lower.starts_with("otpauth-migration://") {
        return otp(text, &lower);
    }
    if lower.starts_with("begin:vcard") {
        return vcard(text);
    }
    if lower.starts_with("mecard:") {
        return mecard(&text[7..]);
    }
    if lower.starts_with("begin:vcalendar") || lower.starts_with("begin:vevent") {
        return event(text);
    }
    if let Some(number) = strip_prefix_ci(text, "tel:") {
        return plain("📞", "电话", vec![number.trim().to_string()]);
    }
    if lower.starts_with("sms:") || lower.starts_with("smsto:") {
        return sms(text);
    }
    if lower.starts_with("mailto:") {
        return mail(text);
    }
    if let Some(coordinates) = strip_prefix_ci(text, "geo:") {
        return geo(coordinates);
    }
    if lower.starts_with("wxp://") {
        return plain("💳", "微信收款码", vec![limit(text, MAX_CHARS)]);
    }
    if is_unsafe_scheme(&lower) {
        let scheme = lower.split(':').next().unwrap_or("");
        return plain(
            "⚠️",
            format!("不是网页 · {scheme}"),
            vec![
                limit(text, MAX_CHARS),
                "这类内容会在点开时执行，别直接打开".into(),
            ],
        );
    }
    if let Some(scheme) = app_scheme(&lower) {
        return plain(
            "📲",
            format!("应用链接 · {scheme}"),
            vec![limit(text, MAX_CHARS)],
        );
    }
    plain("📝", "文本", vec![limit(text, MAX_CHARS)])
}

fn plain(icon: &'static str, title: impl Into<String>, lines: Vec<String>) -> Entry {
    Entry {
        icon,
        title: title.into(),
        lines,
    }
}

fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &text[prefix.len()..])
}

/// 超出上限就截断，并写明一共多少字。
fn limit(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}…（共 {count} 字）")
}

// ================= 链接 =================

/// 这些协议点开就会执行或读本机文件，不是「网页」。
fn is_unsafe_scheme(lower: &str) -> bool {
    ["javascript:", "data:", "file:", "vbscript:", "blob:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
}

/// `weixin://`、`alipays://` 这类唤起应用的自定义协议，返回协议名。
fn app_scheme(lower: &str) -> Option<&str> {
    let (scheme, rest) = lower.split_once("://")?;
    // 网页协议轮不到这里：能当链接的已经在前面处理了，剩下的（夹着空白的）是文字。
    let valid = !matches!(scheme, "http" | "https")
        && !scheme.is_empty()
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
    (valid && !rest.is_empty()).then_some(scheme)
}

/// 已知站点的名字：只收那些光看域名认不出来的（短链、应用落地页）。
const KNOWN_HOSTS: &[(&str, &str)] = &[
    ("u.wechat.com", "微信名片"),
    ("mp.weixin.qq.com", "微信公众号文章"),
    ("wxaurl.cn", "微信小程序"),
    ("payapp.weixin.qq.com", "微信支付"),
    ("wx.tenpay.com", "微信支付"),
    ("qm.qq.com", "QQ 名片 / 群"),
    ("qun.qq.com", "QQ 群"),
    ("jq.qq.com", "QQ 群"),
    ("qr.alipay.com", "支付宝"),
    ("ds.alipay.com", "支付宝"),
    ("render.alipay.com", "支付宝"),
    ("b23.tv", "哔哩哔哩短链"),
    ("v.douyin.com", "抖音短链"),
    ("xhslink.com", "小红书短链"),
    ("m.tb.cn", "淘宝短链"),
    ("e.tb.cn", "淘宝短链"),
    ("u.jd.com", "京东短链"),
    ("t.cn", "微博短链"),
    ("url.cn", "腾讯短链"),
    ("dwz.cn", "百度短链"),
];

/// 域名后缀匹配：`a.b.example.com` 命中 `example.com`，`notexample.com` 不命中。
fn host_matches(host: &str, rule: &str) -> bool {
    host == rule || host.ends_with(&format!(".{rule}"))
}

fn known_label(host: &str, path: &str) -> Option<&'static str> {
    // 微信官网的短路径按用途分：群、公众号。
    if host_matches(host, "weixin.qq.com") && host != "mp.weixin.qq.com" {
        if path.starts_with("/g/") {
            return Some("微信群");
        }
        if path.starts_with("/r/") {
            return Some("微信公众号");
        }
    }
    KNOWN_HOSTS
        .iter()
        .find(|(rule, _)| host_matches(host, rule))
        .map(|(_, label)| *label)
}

fn web_link(text: &str, lower: &str) -> Option<Entry> {
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return None;
    }
    // 链接里夹着空白、后面还有一大段话的，是「带链接的文字」，按文字摆。
    if text.chars().any(char::is_whitespace) {
        return None;
    }
    let parsed = Url::parse(text).ok()?;
    let host = parsed.host_str()?.trim_start_matches("www.").to_string();
    let label = known_label(parsed.host_str()?, parsed.path());
    let title = match label {
        Some(label) => format!("链接 · {label}"),
        None => format!("链接 · {host}"),
    };
    Some(plain(
        "🔗",
        title,
        vec![limit(&normalize_origin(text), 1500)],
    ))
}

/// 把 `HTTPS://QR.ALIPAY.COM/FKX…` 的协议与域名转小写，路径一个字都不动（路径区分大小写）。
/// 支付宝收款码就是整串大写的协议头，不转的话 QQ 不一定认成链接。
fn normalize_origin(text: &str) -> String {
    let Some(scheme_end) = text.find("://") else {
        return text.to_string();
    };
    let authority_start = scheme_end + 3;
    let authority_end = text[authority_start..]
        .find(['/', '?', '#'])
        .map_or(text.len(), |offset| authority_start + offset);
    format!(
        "{}{}",
        text[..authority_end].to_ascii_lowercase(),
        &text[authority_end..]
    )
}

// ================= Wi-Fi =================

/// 按 `;` 切字段，`\` 转义下一个字符；字段里第一个未转义的 `:` 分开键与值。
fn split_fields(body: &str) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    let mut key = String::new();
    let mut value = String::new();
    let mut in_value = false;
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    if in_value { &mut value } else { &mut key }.push(next);
                }
            }
            ':' if !in_value => in_value = true,
            ';' => {
                if !key.is_empty() || !value.is_empty() {
                    fields.push((std::mem::take(&mut key), std::mem::take(&mut value)));
                }
                in_value = false;
            }
            _ => {
                if in_value { &mut value } else { &mut key }.push(c);
            }
        }
    }
    if !key.is_empty() || !value.is_empty() {
        fields.push((key, value));
    }
    fields
}

fn field<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// 十六进制串被引号包起来是在说「这是文本，不是十六进制」，去掉引号。
fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(value)
}

fn wifi(body: &str) -> Entry {
    let fields = split_fields(body);
    let ssid = unquote(field(&fields, "S").unwrap_or("")).to_string();
    let encryption = field(&fields, "T").unwrap_or("").trim().to_string();
    let password = unquote(field(&fields, "P").unwrap_or("")).to_string();
    let hidden = field(&fields, "H").is_some_and(|value| value.eq_ignore_ascii_case("true"));

    let open =
        encryption.eq_ignore_ascii_case("nopass") || (encryption.is_empty() && password.is_empty());
    let mut lines = Vec::new();
    if open {
        lines.push("开放网络，无密码".to_string());
    } else {
        if !password.is_empty() {
            lines.push(format!("密码 {password}"));
        }
        if !encryption.is_empty() {
            lines.push(format!("加密 {}", encryption.to_ascii_uppercase()));
        }
    }
    if hidden {
        lines.push("隐藏网络".to_string());
    }
    let title = if ssid.is_empty() {
        "Wi-Fi".to_string()
    } else {
        format!("Wi-Fi · {ssid}")
    };
    plain("📶", title, lines)
}

// ================= 两步验证 =================

/// 两步验证二维码：只说是什么，不回显密钥。
fn otp(text: &str, lower: &str) -> Entry {
    if lower.starts_with("otpauth-migration://") {
        return plain(
            "🔐",
            "验证器迁移码",
            vec!["里面是整批账号的两步验证密钥，已隐藏；别发给任何人".into()],
        );
    }
    let parsed = Url::parse(text).ok();
    let label = parsed
        .as_ref()
        .map(|url| percent_decode(url.path().trim_start_matches('/')))
        .unwrap_or_default();
    let issuer = parsed.as_ref().and_then(|url| {
        url.query_pairs()
            .find(|(key, _)| key == "issuer")
            .map(|(_, value)| value.into_owned())
    });
    let title = match issuer.or_else(|| {
        label
            .split(':')
            .next()
            .filter(|_| label.contains(':'))
            .map(str::to_string)
    }) {
        Some(issuer) if !issuer.is_empty() => format!("两步验证 · {issuer}"),
        _ => "两步验证".to_string(),
    };
    let mut lines = Vec::new();
    if !label.is_empty() {
        lines.push(format!(
            "账号 {}",
            label.rsplit(':').next().unwrap_or(&label).trim()
        ));
    }
    lines.push("里面是两步验证的密钥，已隐藏；别发给任何人".into());
    plain("🔐", title, lines)
}

fn percent_decode(text: &str) -> String {
    url::form_urlencoded::parse(text.as_bytes())
        .map(|(key, value)| {
            if value.is_empty() {
                key.into_owned()
            } else {
                format!("{key}={value}")
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

// ================= 名片与日程 =================

fn vcard(text: &str) -> Entry {
    let mut name = String::new();
    let mut structured_name = String::new();
    let mut phones = Vec::new();
    let mut emails = Vec::new();
    let mut org = String::new();
    let mut job = String::new();
    for line in unfold(text) {
        let Some((head, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let key = head.split(';').next().unwrap_or("").to_ascii_uppercase();
        match key.as_str() {
            "FN" => name = value.to_string(),
            "N" => {
                // 姓;名;…：中文直接拼，西文「名 姓」。
                let parts: Vec<&str> = value.split(';').collect();
                let (family, given) = (
                    parts.first().copied().unwrap_or(""),
                    parts.get(1).copied().unwrap_or(""),
                );
                structured_name = join_name(family, given);
            }
            "TEL" => phones.push(value.to_string()),
            "EMAIL" => emails.push(value.to_string()),
            "ORG" => org = value.replace(';', " "),
            "TITLE" => job = value.to_string(),
            _ => {}
        }
    }
    let name = if name.is_empty() {
        structured_name
    } else {
        name
    };
    contact(name, phones, emails, org, job)
}

/// vCard 的折行：下一行以空格或制表符开头，是上一行的续写。
fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        match line.strip_prefix([' ', '\t']) {
            Some(rest) if !lines.is_empty() => lines.last_mut().unwrap().push_str(rest),
            _ => lines.push(line.trim_end().to_string()),
        }
    }
    lines
}

fn join_name(family: &str, given: &str) -> String {
    let (family, given) = (family.trim(), given.trim());
    if family.is_ascii() && given.is_ascii() && !family.is_empty() && !given.is_empty() {
        format!("{given} {family}")
    } else {
        format!("{family}{given}")
    }
}

fn mecard(body: &str) -> Entry {
    let fields = split_fields(body);
    let mut phones = Vec::new();
    let mut emails = Vec::new();
    for (key, value) in &fields {
        match key.to_ascii_uppercase().as_str() {
            "TEL" => phones.push(value.clone()),
            "EMAIL" => emails.push(value.clone()),
            _ => {}
        }
    }
    let name = field(&fields, "N")
        .map(|value| {
            let (family, given) = value.split_once(',').unwrap_or((value, ""));
            join_name(family, given)
        })
        .unwrap_or_default();
    contact(
        name,
        phones,
        emails,
        field(&fields, "ORG").unwrap_or("").to_string(),
        String::new(),
    )
}

fn contact(
    name: String,
    phones: Vec<String>,
    emails: Vec<String>,
    org: String,
    job: String,
) -> Entry {
    let mut lines = Vec::new();
    for phone in phones {
        lines.push(format!("电话 {phone}"));
    }
    for email in emails {
        lines.push(format!("邮箱 {email}"));
    }
    let workplace = [org.trim(), job.trim()]
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ");
    if !workplace.is_empty() {
        lines.push(format!("单位 {workplace}"));
    }
    let title = if name.trim().is_empty() {
        "名片".to_string()
    } else {
        format!("名片 · {}", name.trim())
    };
    plain("👤", title, lines)
}

fn event(text: &str) -> Entry {
    let mut summary = String::new();
    let mut start = String::new();
    let mut place = String::new();
    for line in unfold(text) {
        let Some((head, value)) = line.split_once(':') else {
            continue;
        };
        match head
            .split(';')
            .next()
            .unwrap_or("")
            .to_ascii_uppercase()
            .as_str()
        {
            "SUMMARY" if summary.is_empty() => summary = value.trim().to_string(),
            "DTSTART" if start.is_empty() => start = readable_time(value.trim()),
            "LOCATION" if place.is_empty() => place = value.trim().to_string(),
            _ => {}
        }
    }
    let mut lines = Vec::new();
    if !start.is_empty() {
        lines.push(format!("时间 {start}"));
    }
    if !place.is_empty() {
        lines.push(format!("地点 {place}"));
    }
    let title = if summary.is_empty() {
        "日程".to_string()
    } else {
        format!("日程 · {summary}")
    };
    plain("📅", title, lines)
}

/// `20261003T093000Z` → `2026-10-03 09:30`；认不出来的原样给。
fn readable_time(value: &str) -> String {
    let digits: Vec<char> = value.chars().filter(char::is_ascii_digit).collect();
    if digits.len() >= 8 {
        let pick = |range: std::ops::Range<usize>| digits[range].iter().collect::<String>();
        let date = format!("{}-{}-{}", pick(0..4), pick(4..6), pick(6..8));
        if digits.len() >= 12 {
            return format!("{date} {}:{}", pick(8..10), pick(10..12));
        }
        return date;
    }
    value.to_string()
}

// ================= 短信、邮件、位置 =================

fn sms(text: &str) -> Entry {
    // `smsto:号码:内容`，或 `sms:号码?body=内容`。
    let rest = text.split_once(':').map_or("", |(_, rest)| rest);
    let (number, body) = if text.to_ascii_lowercase().starts_with("smsto:") {
        rest.split_once(':').unwrap_or((rest, ""))
    } else {
        let (number, query) = rest.split_once('?').unwrap_or((rest, ""));
        let body = url::form_urlencoded::parse(query.as_bytes())
            .find(|(key, _)| key == "body")
            .map(|(_, value)| value.into_owned())
            .unwrap_or_default();
        return build_sms(number, &body);
    };
    build_sms(number, body)
}

fn build_sms(number: &str, body: &str) -> Entry {
    let mut lines = Vec::new();
    if !body.trim().is_empty() {
        lines.push(limit(body.trim(), MAX_CHARS));
    }
    let title = if number.trim().is_empty() {
        "短信".to_string()
    } else {
        format!("短信 · {}", number.trim())
    };
    plain("💬", title, lines)
}

fn mail(text: &str) -> Entry {
    let rest = &text[7..];
    let (address, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut lines = Vec::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "subject" if !value.is_empty() => lines.push(format!("主题 {value}")),
            "body" if !value.is_empty() => lines.push(limit(&value, MAX_CHARS)),
            _ => {}
        }
    }
    plain("✉️", format!("邮件 · {}", address.trim()), lines)
}

fn geo(coordinates: &str) -> Entry {
    let head = coordinates.split(['?', ';']).next().unwrap_or("").trim();
    let pretty = head.replace(',', "，");
    plain("📍", "位置", vec![format!("纬度，经度 {pretty}")])
}

// ================= 摆成消息 =================

/// 圈起来的序号；超出 20 的退回「21.」。
fn ordinal(number: usize) -> String {
    match number {
        1..=20 => char::from_u32(0x2460 + number as u32 - 1)
            .map_or_else(|| format!("{number}."), String::from),
        _ => format!("{number}."),
    }
}

/// 把条目摆成一条消息。
///
/// 一个码：一行标题、下面是内容，不编号。多个码：先说一共几个，再按序号逐个列，
/// 序号与图上标的一致。
pub fn render(entries: &[Entry], omitted: usize) -> String {
    let mut out = String::new();
    if entries.len() > 1 || omitted > 0 {
        let total = entries.len() + omitted;
        out.push_str(&format!("识别到 {total} 个二维码\n"));
    }
    for (index, entry) in entries.iter().enumerate() {
        if !out.is_empty() {
            out.push('\n');
        }
        if entries.len() > 1 || omitted > 0 {
            out.push_str(&format!("{} ", ordinal(index + 1)));
        }
        out.push_str(&format!("{} {}", entry.icon, entry.title));
        for line in &entry.lines {
            out.push('\n');
            out.push_str(line);
        }
        if index + 1 < entries.len() {
            out.push('\n');
        }
    }
    if omitted > 0 {
        out.push_str(&format!("\n\n另有 {omitted} 个没有列出"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title(text: &str) -> String {
        describe(text).title
    }

    #[test]
    fn web_links_show_the_host_or_a_known_name() {
        let entry = describe("https://github.com/araea/acumen");
        assert_eq!(entry.icon, "🔗");
        assert_eq!(entry.title, "链接 · github.com");
        assert_eq!(entry.lines, ["https://github.com/araea/acumen"]);
        assert_eq!(title("http://www.example.com/a"), "链接 · example.com");
        // 光看域名认不出的：短链与应用落地页写出名字。
        assert_eq!(title("https://u.wechat.com/MPabcd"), "链接 · 微信名片");
        assert_eq!(title("https://b23.tv/AbCdEf"), "链接 · 哔哩哔哩短链");
        assert_eq!(
            title("https://qm.qq.com/cgi-bin/qm/qr?k=abc"),
            "链接 · QQ 名片 / 群"
        );
        assert_eq!(title("https://weixin.qq.com/g/AwYAAA"), "链接 · 微信群");
        assert_eq!(title("https://weixin.qq.com/r/AbC"), "链接 · 微信公众号");
        assert_eq!(
            title("https://mp.weixin.qq.com/s/abc"),
            "链接 · 微信公众号文章"
        );
        // 后缀匹配不能误伤：notb23.tv 不是 b23.tv。
        assert_eq!(title("https://notb23.tv/x"), "链接 · notb23.tv");
    }

    /// 支付宝收款码整串大写协议头；协议与域名转小写，路径一个字都不能动。
    #[test]
    fn upper_case_origin_is_lowered_but_the_path_is_kept() {
        let entry = describe("HTTPS://QR.ALIPAY.COM/FKX01234AbCdEf?x=Y");
        assert_eq!(entry.title, "链接 · 支付宝");
        assert_eq!(entry.lines, ["https://qr.alipay.com/FKX01234AbCdEf?x=Y"]);
    }

    /// 带空白的是「夹着链接的文字」，按文字摆，不装成链接。
    #[test]
    fn text_that_merely_contains_a_link_stays_text() {
        let entry = describe("扫码领券 https://a.com/x");
        assert_eq!(entry.title, "文本");
        assert_eq!(describe("https://a.com/x y").title, "文本");
        // 不是合法链接。
        assert_eq!(describe("https://").title, "文本");
    }

    #[test]
    fn wifi_codes_show_ssid_password_and_encryption() {
        let entry = describe("WIFI:T:WPA;S:MyHome;P:secret123;;");
        assert_eq!(entry.icon, "📶");
        assert_eq!(entry.title, "Wi-Fi · MyHome");
        assert_eq!(entry.lines, ["密码 secret123", "加密 WPA"]);

        // 转义的分号与冒号，以及隐藏网络。
        let escaped = describe(r"WIFI:S:a\;b\:c;T:WPA2;P:p\\w;H:true;;");
        assert_eq!(escaped.title, "Wi-Fi · a;b:c");
        assert_eq!(escaped.lines, [r"密码 p\w", "加密 WPA2", "隐藏网络"]);

        let open = describe("WIFI:T:nopass;S:Cafe;;");
        assert_eq!(open.lines, ["开放网络，无密码"]);
        // 引号包起来的十六进制样子的名字，去掉引号。
        assert_eq!(
            describe("WIFI:S:\"1234\";T:WPA;P:abcdefgh;;").title,
            "Wi-Fi · 1234"
        );
    }

    #[test]
    fn phone_sms_mail_and_geo_are_recognised() {
        assert_eq!(describe("tel:+8613800138000").lines, ["+8613800138000"]);
        assert_eq!(describe("TEL:10086").title, "电话");

        let sms = describe("SMSTO:10086:查话费");
        assert_eq!(sms.title, "短信 · 10086");
        assert_eq!(sms.lines, ["查话费"]);
        assert_eq!(describe("sms:10086?body=cxll").lines, ["cxll"]);

        let mail = describe("mailto:me@example.com?subject=Hello%20World&body=Hi");
        assert_eq!(mail.title, "邮件 · me@example.com");
        assert_eq!(mail.lines, ["主题 Hello World", "Hi"]);

        assert_eq!(
            describe("geo:39.9087,116.3975").lines,
            ["纬度，经度 39.9087，116.3975"]
        );
    }

    #[test]
    fn contact_cards_list_name_phones_emails_and_workplace() {
        let vcard = describe(
            "BEGIN:VCARD\nVERSION:3.0\nN:张;三;;;\nFN:张三\nORG:示例公司\nTITLE:工程师\nTEL;TYPE=CELL:13800138000\nEMAIL:zs@example.com\nEND:VCARD",
        );
        assert_eq!(vcard.title, "名片 · 张三");
        assert_eq!(
            vcard.lines,
            [
                "电话 13800138000",
                "邮箱 zs@example.com",
                "单位 示例公司 · 工程师"
            ]
        );

        // 没有 FN 就用 N：西文是「名 姓」，中文直接拼。
        assert_eq!(
            describe("BEGIN:VCARD\nN:Smith;John\nEND:VCARD").title,
            "名片 · John Smith"
        );
        assert_eq!(
            describe("BEGIN:VCARD\nN:李;四\nEND:VCARD").title,
            "名片 · 李四"
        );

        let mecard = describe("MECARD:N:Smith,John;TEL:123456;EMAIL:j@x.com;ORG:Acme;;");
        assert_eq!(mecard.title, "名片 · John Smith");
        assert_eq!(mecard.lines, ["电话 123456", "邮箱 j@x.com", "单位 Acme"]);
    }

    #[test]
    fn calendar_events_show_summary_time_and_place() {
        let entry = describe(
            "BEGIN:VEVENT\nSUMMARY:评审会\nDTSTART:20261003T093000Z\nLOCATION:3 号会议室\nEND:VEVENT",
        );
        assert_eq!(entry.title, "日程 · 评审会");
        assert_eq!(entry.lines, ["时间 2026-10-03 09:30", "地点 3 号会议室"]);
        assert_eq!(readable_time("20261003"), "2026-10-03");
        assert_eq!(readable_time("明天"), "明天");
    }

    /// 两步验证的二维码里就是密钥：说清是什么，但一个字符的密钥都不能回显。
    #[test]
    fn otp_secrets_are_never_echoed() {
        let entry = describe("otpauth://totp/GitHub:araea?secret=JBSWY3DPEHPK3PXP&issuer=GitHub");
        assert_eq!(entry.icon, "🔐");
        assert_eq!(entry.title, "两步验证 · GitHub");
        let shown = format!("{} {}", entry.title, entry.lines.join("\n"));
        assert!(!shown.contains("JBSWY3DPEHPK3PXP"), "密钥泄露了：{shown}");
        assert!(entry.lines[0].contains("araea"));

        let migration =
            describe("otpauth-migration://offline?data=CjEKCkhlbGxvId6tvu8SGAoOTXlTZWNyZXQ");
        assert_eq!(migration.title, "验证器迁移码");
        assert!(!migration.lines.join("").contains("CjEKCkhl"));
    }

    #[test]
    fn non_web_schemes_are_labelled_not_passed_off_as_links() {
        let script = describe("javascript:alert(1)");
        assert_eq!(script.icon, "⚠️");
        assert!(script.title.contains("javascript"));
        assert!(script.lines.iter().any(|line| line.contains("别直接打开")));
        for raw in [
            "data:text/html;base64,PHNjcmlwdD4=",
            "FILE:///etc/passwd",
            "vbscript:x",
        ] {
            assert_eq!(describe(raw).icon, "⚠️", "{raw}");
        }
        // 唤起应用的自定义协议：普通的应用链接。
        let app = describe("weixin://dl/business/?t=abc");
        assert_eq!((app.icon, app.title.as_str()), ("📲", "应用链接 · weixin"));
        assert_eq!(describe("wxp://f2f0abc").title, "微信收款码");
    }

    #[test]
    fn plain_text_and_empty_codes_are_shown_as_text() {
        assert_eq!(describe("你好，世界").title, "文本");
        assert_eq!(describe("  \n ").lines, ["（内容为空）"]);
        assert_eq!(describe("12345678").title, "文本");
    }

    #[test]
    fn long_content_is_cut_with_its_full_length_stated() {
        let entry = describe(&"字".repeat(2000));
        assert!(entry.lines[0].starts_with(&"字".repeat(MAX_CHARS)));
        assert!(entry.lines[0].ends_with("…（共 2000 字）"));
        assert_eq!(limit("短", 10), "短");
    }

    #[test]
    fn a_single_code_is_shown_without_numbering() {
        let entry = describe("https://github.com/araea/acumen");
        assert_eq!(
            render(&[entry], 0),
            "🔗 链接 · github.com\nhttps://github.com/araea/acumen"
        );
    }

    #[test]
    fn several_codes_are_numbered_with_a_header() {
        let entries = [
            describe("https://github.com/araea/acumen"),
            describe("WIFI:T:WPA;S:MyHome;P:secret123;;"),
            describe("你好"),
        ];
        assert_eq!(
            render(&entries, 0),
            "识别到 3 个二维码\n\
             \n\
             ① 🔗 链接 · github.com\n\
             https://github.com/araea/acumen\n\
             \n\
             ② 📶 Wi-Fi · MyHome\n\
             密码 secret123\n\
             加密 WPA\n\
             \n\
             ③ 📝 文本\n\
             你好"
        );
    }

    #[test]
    fn omitted_codes_are_counted_in_the_header_and_mentioned_at_the_end() {
        let rendered = render(&[describe("甲")], 4);
        assert!(rendered.starts_with("识别到 5 个二维码\n"));
        assert!(rendered.contains("① 📝 文本\n甲"));
        assert!(rendered.ends_with("另有 4 个没有列出"));
    }

    #[test]
    fn ordinals_fall_back_after_twenty() {
        assert_eq!(ordinal(1), "①");
        assert_eq!(ordinal(10), "⑩");
        assert_eq!(ordinal(20), "⑳");
        assert_eq!(ordinal(21), "21.");
    }
}
