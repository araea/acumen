use crate::message::{Message, Segment};
use quick_xml::Reader;
use quick_xml::events::Event as XmlEvent;
use simd_json::OwnedValue;
use simd_json::base::{ValueAsArray, ValueAsObject, ValueAsScalar};
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsScalar};
use simd_json::owned::Object;
use std::sync::{Arc, OnceLock};

/// QQ 把骰子和猜拳实现为两个「魔法表情」，没有独立的消息元素。
pub const DICE_FACE_ID: u32 = 358;
pub const RPS_FACE_ID: u32 = 359;

#[derive(Default)]
struct Element {
    name: String,
    attrs: Object,
    children: Vec<Element>,
    text: String,
}

/// 按 Satori 的[资源链接](https://satori.js.org/zh-CN/advanced/resource.html)规范，
/// 把元素里的 `src` 解析成插件可以直接 GET 的地址。
///
/// `internal:` 链接和 `proxy_urls` 命中的平台链接都要走实现端的 `/v1/proxy/{url}`
/// 路由——前者应用侧根本取不到，后者可能有防盗链或时效。该路由不需要鉴权头。
#[derive(Clone, Default)]
pub struct ResourceProxy {
    endpoint: String,
    proxy_urls: Arc<Vec<String>>,
}

impl ResourceProxy {
    pub fn new(endpoint: String, proxy_urls: Arc<Vec<String>>) -> Self {
        Self {
            endpoint,
            proxy_urls,
        }
    }

    /// 返回可直接下载的 URL；`data:`、`file:` 和本地路径没有下载地址，返回 `None`。
    fn fetchable(&self, src: &str) -> Option<String> {
        let direct = src.starts_with("http://") || src.starts_with("https://");
        let proxied = src.starts_with("internal:")
            || (direct
                && self
                    .proxy_urls
                    .iter()
                    .any(|prefix| src.starts_with(prefix.as_str())));
        if proxied && !self.endpoint.is_empty() {
            return Some(format!(
                "{}/v1/proxy/{}",
                self.endpoint,
                encode_proxy_url(src)
            ));
        }
        direct.then(|| src.to_string())
    }
}

/// 代理路由把 URL 直接拼在路径里，只转义会破坏路径解析的字符。
/// `internal:` 链接不含这些字符，因此始终保持原样。
fn encode_proxy_url(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for ch in src.chars() {
        match ch {
            '%' => out.push_str("%25"),
            '+' => out.push_str("%2B"),
            '?' => out.push_str("%3F"),
            '#' => out.push_str("%23"),
            ' ' => out.push_str("%20"),
            _ => out.push(ch),
        }
    }
    out
}

/// 将 Satori 元素串转为框架内部的消息段，按实现端下发的代理路由解析资源地址。
pub fn from_content_with(content: &str, proxy: &ResourceProxy) -> Message {
    let normalized = normalize_boolean_attrs(content);
    let mut reader = Reader::from_str(&normalized);
    reader.config_mut().trim_text(false);
    let mut roots = Vec::new();
    let mut stack: Vec<Element> = Vec::new();

    loop {
        match reader.read_event() {
            Ok(XmlEvent::Start(start)) => {
                stack.push(Element {
                    name: start.name().as_ref().to_string(),
                    attrs: attrs(&start),
                    ..Default::default()
                });
            }
            Ok(XmlEvent::Empty(start)) => {
                let element = Element {
                    name: start.name().as_ref().to_string(),
                    attrs: attrs(&start),
                    ..Default::default()
                };
                push_element(&mut roots, &mut stack, element);
            }
            Ok(XmlEvent::Text(text)) => {
                let decoded = text.into_inner();
                let value = match quick_xml::escape::unescape(&decoded) {
                    Ok(value) => value.into_owned(),
                    Err(_) => decoded.into_owned(),
                };
                push_text(&mut roots, &mut stack, &value);
            }
            Ok(XmlEvent::CData(text)) => {
                let value = text.into_inner();
                push_text(&mut roots, &mut stack, &value);
            }
            Ok(XmlEvent::GeneralRef(reference)) => {
                let name = reference.into_inner();
                let value = if let Some(number) = name.strip_prefix("#x") {
                    u32::from_str_radix(number, 16)
                        .ok()
                        .and_then(char::from_u32)
                } else if let Some(number) = name.strip_prefix('#') {
                    number.parse::<u32>().ok().and_then(char::from_u32)
                } else {
                    None
                }
                .map(|value| value.to_string())
                .or_else(|| quick_xml::escape::resolve_xml_entity(&name).map(str::to_string))
                .unwrap_or_else(|| format!("&{name};"));
                push_text(&mut roots, &mut stack, &value);
            }
            Ok(XmlEvent::End(_)) => {
                if let Some(element) = stack.pop() {
                    push_element(&mut roots, &mut stack, element);
                }
            }
            Ok(XmlEvent::Eof) => break,
            Err(_) => return Message::new().text(content),
            _ => {}
        }
    }

    let mut out = Message::new();
    for root in roots {
        append_element(&mut out, root, proxy);
    }
    out
}

/// Satori 允许 `<message forward>` 这样的布尔属性；补成严格 XML 交给 quick-xml。
fn normalize_boolean_attrs(content: &str) -> String {
    static TAG: OnceLock<regex::Regex> = OnceLock::new();
    static ATTR: OnceLock<regex::Regex> = OnceLock::new();
    let tag = TAG.get_or_init(|| {
        regex::Regex::new(r#"<([a-z][a-z0-9-]*(?::[a-z][a-z0-9-]*)?)([^<>]*?)(/?)>"#)
            .expect("valid tag regex")
    });
    let attr = ATTR.get_or_init(|| {
        regex::Regex::new(r#"([^\s=]+)(?:=(?:"[^"]*"|'[^']*'))?"#).expect("valid attr regex")
    });
    tag.replace_all(content, |caps: &regex::Captures<'_>| {
        let name = caps.get(1).map(|value| value.as_str()).unwrap_or("");
        let raw_attrs = caps.get(2).map(|value| value.as_str()).unwrap_or("");
        let slash = caps.get(3).map(|value| value.as_str()).unwrap_or("");
        let mut attrs = String::new();
        for found in attr.find_iter(raw_attrs) {
            let raw = found.as_str();
            if raw.contains('=') {
                attrs.push(' ');
                attrs.push_str(raw);
            } else if let Some(key) = raw.strip_prefix("no-") {
                attrs.push_str(&format!(" {key}=\"false\""));
            } else {
                attrs.push_str(&format!(" {raw}=\"true\""));
            }
        }
        format!("<{name}{attrs}{slash}>")
    })
    .into_owned()
}

fn push_text(roots: &mut Vec<Element>, stack: &mut [Element], value: &str) {
    if value.is_empty() {
        return;
    }
    let target = if let Some(parent) = stack.last_mut() {
        &mut parent.children
    } else {
        roots
    };
    if let Some(last) = target.last_mut()
        && last.name == "text"
    {
        last.text.push_str(value);
        return;
    }
    target.push(Element {
        name: "text".to_string(),
        text: value.to_string(),
        ..Default::default()
    });
}

fn attrs(start: &quick_xml::events::BytesStart<'_>) -> Object {
    let mut out = Object::new();
    for attr in start.attributes().with_checks(false).flatten() {
        let key = attr.key.as_ref().to_string();
        // 这些元素片段没有 XML 声明，按隐式 1.0 归一属性值（含实体解码）。
        let value = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map(|v| v.into_owned())
            .unwrap_or_default();
        out.insert(key, OwnedValue::from(value));
    }
    out
}

fn push_element(roots: &mut Vec<Element>, stack: &mut [Element], element: Element) {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(element);
    } else {
        roots.push(element);
    }
}

fn attr(element: &Element, key: &str) -> String {
    element
        .attrs
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn append_element(out: &mut Message, mut element: Element, proxy: &ResourceProxy) {
    // 平台原生元素带适配器名前缀（`satori-qq:json`、`satori-wx:…`）；插件按不带前缀的名字认。
    if let Some((_, name)) = element.name.split_once(':') {
        element.name = name.to_string();
    }
    let mut data = Object::new();
    match element.name.as_str() {
        "text" => push(out, "text", "text", element.text),
        "at" => {
            let id = attr(&element, "id");
            let at_type = attr(&element, "type");
            let role = attr(&element, "role");
            let target = if id.eq_ignore_ascii_case("all")
                || at_type.eq_ignore_ascii_case("all")
                || (id.is_empty() && role.eq_ignore_ascii_case("all"))
            {
                "all".to_string()
            } else {
                id
            };
            if target.is_empty() {
                let fallback = if at_type.eq_ignore_ascii_case("here") {
                    "在线成员".to_string()
                } else {
                    [attr(&element, "name"), role, at_type]
                        .into_iter()
                        .find(|value| !value.is_empty())
                        .unwrap_or_default()
                };
                if !fallback.is_empty() {
                    push(out, "text", "text", format!("@{fallback}"));
                }
                return;
            }
            data.insert("qq".into(), OwnedValue::from(target));
            let name = attr(&element, "name");
            if !name.is_empty() {
                data.insert("name".into(), OwnedValue::from(name));
            }
            out.0.push(Segment::new("at", data));
        }
        "sharp" => {
            let label = [attr(&element, "name"), attr(&element, "id")]
                .into_iter()
                .find(|value| !value.is_empty())
                .unwrap_or_default();
            if !label.is_empty() {
                push(out, "text", "text", format!("#{label}"));
            }
        }
        "quote" => {
            let id = [
                attr(&element, "id"),
                attr(&element, "message-id"),
                attr(&element, "messageId"),
            ]
            .into_iter()
            .find(|value| !value.is_empty())
            .unwrap_or_default();
            push(out, "reply", "id", id);
        }
        "face" | "emoji" => push(out, "face", "id", attr(&element, "id")),
        "img" | "image" => resource(out, "image", element, proxy),
        "audio" | "record" => resource(out, "record", element, proxy),
        "video" => resource(out, "video", element, proxy),
        "file" => resource(out, "file", element, proxy),
        "json" => {
            let value = if attr(&element, "data").is_empty() {
                joined_text(&element)
            } else {
                attr(&element, "data")
            };
            push(out, "json", "data", value);
        }
        "mface" | "poke" => {
            let data = element
                .attrs
                .into_iter()
                .map(|(key, value)| {
                    let key = key.replace('-', "_");
                    let value = if key == "sub_type" {
                        value
                            .as_str()
                            .and_then(|value| value.parse::<i64>().ok())
                            .map(OwnedValue::from)
                            .unwrap_or(value)
                    } else {
                        value
                    };
                    (key, value)
                })
                .collect();
            out.0.push(Segment::new(&element.name, data));
        }
        "br" => push(out, "text", "text", "\n".to_string()),
        "p" => {
            append_children(out, element.children, proxy);
            push(out, "text", "text", "\n".to_string());
        }
        "a" => {
            let href = attr(&element, "href");
            let label = joined_text(&element);
            append_children(out, element.children, proxy);
            if !href.is_empty() && href != label {
                let suffix = if label.is_empty() {
                    href
                } else {
                    format!(" ({href})")
                };
                push(out, "text", "text", suffix);
            }
        }
        "message" if attr(&element, "forward") == "true" => {
            let id = attr(&element, "id");
            if !id.is_empty() {
                push(out, "forward", "id", id);
            } else {
                for child in element.children {
                    if child.name != "message" {
                        continue;
                    }
                    let mut node = Object::new();
                    let mut body = Message::new();
                    for part in child.children {
                        if part.name == "author" {
                            node.insert("user_id".into(), OwnedValue::from(attr(&part, "id")));
                            node.insert("nickname".into(), OwnedValue::from(attr(&part, "name")));
                        } else {
                            append_element(&mut body, part, proxy);
                        }
                    }
                    node.insert(
                        "content".into(),
                        simd_json::serde::to_owned_value(body).unwrap_or_default(),
                    );
                    out.0.push(Segment::new("node", node));
                }
            }
        }
        _ => {
            if !element.text.is_empty() {
                push(out, "text", "text", element.text);
            }
            append_children(out, element.children, proxy);
        }
    }
}

fn joined_text(element: &Element) -> String {
    let mut text = element.text.clone();
    for child in &element.children {
        text.push_str(&joined_text(child));
    }
    text
}

fn append_children(out: &mut Message, children: Vec<Element>, proxy: &ResourceProxy) {
    for child in children {
        append_element(out, child, proxy);
    }
}

fn push(out: &mut Message, kind: &str, key: &str, value: String) {
    let mut data = Object::new();
    data.insert(key.into(), OwnedValue::from(value));
    out.0.push(Segment::new(kind, data));
}

fn resource(out: &mut Message, kind: &str, element: Element, proxy: &ResourceProxy) {
    let mut data = Object::new();
    let src = attr(&element, "src");
    if let Some(url) = proxy.fetchable(&src) {
        data.insert("url".into(), OwnedValue::from(url));
    }
    data.insert("file".into(), OwnedValue::from(src));
    let title = attr(&element, "title");
    if !title.is_empty() {
        data.insert("name".into(), OwnedValue::from(title));
    }
    for (source, target) in [
        ("file-size", "file_size"),
        ("fileSize", "file_size"),
        ("width", "width"),
        ("height", "height"),
        ("duration", "duration"),
        ("poster", "poster"),
        ("summary", "summary"),
    ] {
        let value = attr(&element, source);
        if !value.is_empty() {
            data.insert(target.into(), OwnedValue::from(value));
        }
    }
    // satori-qq 把 QQ 的图片子类型带在 `sub-type` 上：1 是收藏/自定义表情，0 或缺省是
    // 普通图片。按数字存，与商城表情的 `sub_type` 同一种写法，录制器也按数字认。
    if let Ok(sub_type) = attr(&element, "sub-type").parse::<i64>()
        && sub_type != 0
    {
        data.insert("sub_type".into(), OwnedValue::from(sub_type));
    }
    out.0.push(Segment::new(kind, data));
}

/// 将框架内部消息段序列化为 Satori 元素串。
pub fn to_content(value: &OwnedValue) -> String {
    if let Some(text) = value.as_str() {
        return escape_text(text);
    }
    let Some(segments) = value.as_array() else {
        return String::new();
    };
    let all_nodes = !segments.is_empty()
        && segments
            .iter()
            .all(|segment| segment.get_str("type") == Some("node"));
    let body = segments.iter().map(segment_to_content).collect::<String>();
    if all_nodes {
        format!("<message forward>{body}</message>")
    } else {
        body
    }
}

fn segment_to_content(segment: &OwnedValue) -> String {
    let kind = segment.get_str("type").unwrap_or("text");
    let data = segment.get("data").unwrap_or(segment);
    match kind {
        "text" => escape_text(data.get_str("text").unwrap_or("")),
        "at" => {
            let id = scalar(data.get("qq"));
            if id.eq_ignore_ascii_case("all") {
                "<at type=\"all\"/>".to_string()
            } else {
                format!("<at id=\"{}\"/>", escape_attr(&id))
            }
        }
        "face" => tag("emoji", &[("id", scalar(data.get("id")))]),
        "reply" => tag("quote", &[("id", scalar(data.get("id")))]),
        "image" => resource_tag("img", data),
        "record" => resource_tag("audio", data),
        "video" => resource_tag("video", data),
        "file" => resource_tag("file", data),
        "json" | "lightapp" => tag(
            "satori-qq:json",
            &[(
                "data",
                data.get_str("data")
                    .or_else(|| data.get_str("content"))
                    .unwrap_or("")
                    .to_string(),
            )],
        ),
        "mface" | "poke" => tag_from_data(&format!("satori-qq:{kind}"), data),
        // 骰子和猜拳在 QQ 里就是两个特殊表情；`<dice/>`/`<rps/>` 不在 Satori 元素表里，
        // 发出去只会被适配器整段丢掉（没有报错，消息直接变空）。
        "dice" => tag("emoji", &[("id", DICE_FACE_ID.to_string())]),
        "rps" => tag("emoji", &[("id", RPS_FACE_ID.to_string())]),
        "markdown" => escape_text(data.get_str("content").unwrap_or("")),
        "node" => {
            if let Some(id) = data.get_str("id") {
                return format!("<message id=\"{}\"/>", escape_attr(id));
            }
            let uid = scalar(data.get("user_id"));
            let nick = data.get_str("nickname").unwrap_or("");
            let content = data.get("content").map(to_content).unwrap_or_default();
            format!(
                "<message><author id=\"{}\" name=\"{}\"/>{}</message>",
                escape_attr(&uid),
                escape_attr(nick),
                content
            )
        }
        "forward" => tag(
            "message",
            &[
                ("forward", "true".to_string()),
                ("id", scalar(data.get("id"))),
            ],
        ),
        _ => escape_text(data.get_str("text").unwrap_or("")),
    }
}

fn resource_tag(kind: &str, data: &OwnedValue) -> String {
    let src = data
        .get_str("file")
        .or_else(|| data.get_str("url"))
        .unwrap_or("");
    let mut attrs = vec![("src", src.to_string())];
    if let Some(name) = data.get_str("name") {
        attrs.push(("title", name.to_string()));
    }
    // 图片子类型原样带回去：偷来的表情包发出去还是表情包的样子，不变成一张大图。
    if let Some(sub_type) = data
        .get_i64("sub_type")
        .or_else(|| data.get_str("sub_type").and_then(|v| v.parse().ok()))
        .filter(|sub_type| *sub_type != 0)
    {
        attrs.push(("sub-type", sub_type.to_string()));
    }
    if let Some(summary) = data
        .get_str("summary")
        .filter(|summary| !summary.is_empty())
    {
        attrs.push(("summary", summary.to_string()));
    }
    tag(kind, &attrs)
}

fn tag_from_data(kind: &str, data: &OwnedValue) -> String {
    let Some(object) = data.as_object() else {
        return format!("<{kind}/>");
    };
    let attrs = object
        .iter()
        .map(|(key, value)| (key.as_str(), scalar(Some(value))))
        .collect::<Vec<_>>();
    tag(kind, &attrs)
}

fn tag(kind: &str, attrs: &[(&str, String)]) -> String {
    let attrs = attrs
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(key, value)| format!(" {key}=\"{}\"", escape_attr(value)))
        .collect::<String>();
    format!("<{kind}{attrs}/>")
}

fn scalar(value: Option<&OwnedValue>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    if let Some(value) = value.as_str() {
        value.to_string()
    } else if let Some(value) = value.as_i64() {
        value.to_string()
    } else if let Some(value) = value.as_u64() {
        value.to_string()
    } else if let Some(value) = value.as_bool() {
        value.to_string()
    } else {
        String::new()
    }
}

/// 只转义 `< > &`：实现端的反转义不认 `&apos;`，全量转义会把
/// `don&apos;t` 这种半成品原样发进 QQ 消息。
fn escape_plain(value: &str) -> String {
    quick_xml::escape::partial_escape(value).into_owned()
}

/// Satori 规范把文本段首尾「含换行的空白」当排版空白裁掉，首尾换行因此改用
/// `<br/>` 表达，否则 `@某人\n[图片]` 之类的换行会被实现端吃掉。
fn escape_text(value: &str) -> String {
    let Some(start) = value.find(|ch: char| !ch.is_whitespace()) else {
        return boundary_whitespace(value);
    };
    let end = value
        .rfind(|ch: char| !ch.is_whitespace())
        .map(|idx| idx + value[idx..].chars().next().map_or(0, char::len_utf8))
        .unwrap_or(value.len());
    format!(
        "{}{}{}",
        boundary_whitespace(&value[..start]),
        escape_plain(&value[start..end]),
        boundary_whitespace(&value[end..])
    )
}

/// 首尾空白里的换行转成 `<br/>`；`\r` 会让实现端把整段空白判成排版空白，直接丢掉。
fn boundary_whitespace(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '\n' => out.push_str("<br/>"),
            '\r' => {}
            _ => out.push(ch),
        }
    }
    out
}

fn escape_attr(value: &str) -> String {
    escape_plain(value).replace('"', "&quot;")
}
