#![allow(dead_code)]

use crate::adapters::satori::{LockedWriter, api};
use crate::event::Context;
use regex::Regex;
use simd_json::OwnedValue;
use simd_json::base::{ValueAsObject, ValueAsScalar};
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsArray, ValueObjectAccessAsScalar};
use std::sync::OnceLock;

pub struct CommandMatch {
    /// 匹配后的参数列表（剩余的消息段）
    pub args: Vec<OwnedValue>,
    /// 被过滤掉的引用回复 ID
    pub reply_id: Option<String>,
    /// 被过滤掉的 AT 用户 ID 列表
    pub at_ids: Vec<String>,
}

pub fn get_prefixes(ctx: &Context) -> Vec<String> {
    let prefixes = ctx.config.read().unwrap().command_prefix.clone();
    if prefixes.is_empty() {
        vec![String::new()]
    } else {
        prefixes
    }
}

/// 在多条候选指令中返回第一个命中的匹配
pub fn first_command_match(ctx: &Context, commands: &[&str]) -> Option<CommandMatch> {
    commands.iter().find_map(|cmd| match_command(ctx, cmd))
}

/// 把 CommandMatch 的参数拼接为纯文本
///
/// 段与段之间补一个空格，避免多段文本参数粘连成一个词。
pub fn extract_text_arg(args: &[OwnedValue]) -> String {
    let mut buf = String::new();
    for seg in args {
        if seg.get_str("type") == Some("text")
            && let Some(text) = seg.get("data").and_then(|d| d.get_str("text"))
        {
            buf.push_str(text);
            buf.push(' ');
        }
    }
    buf.trim().to_string()
}

/// 剥离消息前缀：配置了前缀则必须命中其一，未配置前缀则原样放行
pub fn strip_prefix<'a>(ctx: &Context, text: &'a str) -> Option<&'a str> {
    let text = text.trim();
    let prefixes = get_prefixes(ctx);
    if prefixes.is_empty() {
        return Some(text);
    }
    prefixes
        .iter()
        .find_map(|p| text.strip_prefix(p.as_str()).map(str::trim_start))
}

/// 正文里能拿去认的每一截：先是整段，正文以 @ 开头时再补上 @ 之后逐词往后的每一截。
///
/// 平台会把 @ 的**显示名**也写进正文——`at` 段后面跟着一段「@名字 正文」（`at` 段自己
/// 不写名字，那个名字是 QQ 客户端显示出来的样子）；适配器拼 `raw_message` 时只取文本段，
/// 于是引用回复（QQ 会自动补一个 @）进来的正文长这样：「@A宝好腻害！ 2」或
/// 「@A宝好腻害！ 视频」。名字多长没法猜、昵称里还可能带空格，所以这里只给候选，
/// 由调用方挑认得出的那一截；正文不带 @ 时只有整段一个候选，与从前一样。
pub fn spoken_bodies(text: &str) -> Vec<&str> {
    let mut bodies = vec![text.trim()];
    let Some(mut tail) = bodies[0].strip_prefix('@').map(str::trim_start) else {
        return bodies;
    };
    loop {
        bodies.push(tail);
        match tail.split_once(char::is_whitespace) {
            Some((_, next)) => tail = next.trim_start(),
            None => return bodies,
        }
    }
}

/// 取消息里的引用回复 ID（`reply` 段的 `id`）。
///
/// 「引用某条消息再回复」这类隐式交互（AI 资讯的序号提取）从这一处取被引消息的
/// ID，实现端把它写成字符串还是数字都认。
pub fn message_reply_id(ctx: &Context) -> Option<String> {
    let arr = ctx.as_message()?.0.get_array("message")?;
    for segment in arr {
        if segment.get_str("type") != Some("reply") {
            continue;
        }
        let data = segment.get("data")?;
        return data
            .get_str("id")
            .map(String::from)
            .or_else(|| data.get_i64("id").map(|v| v.to_string()))
            .or_else(|| data.get_u64("id").map(|v| v.to_string()));
    }
    None
}

/// 提取文本里所有 http(s) URL，按出现顺序去重。
///
/// 群聊里的链接几乎从不独占一行：前后粘着中文，后面跟着全角逗号、句号、引号或者
/// 一对括号。「避雷这个中转站https://platform.deepseek.com，pro 模型路由到 flash」
/// 里真正的地址到 `.com` 为止，之前只排除汉字的写法会把「，pro」也算进去。
///
/// 所以这里只认 RFC 3986 允许的那些 ASCII 字符——中文、全角标点、书名号、引号
/// 都不在其中，自然断开；再把结尾那几个几乎不可能属于地址的半角标点剥掉，
/// 包括与地址内部不成对的那半个括号（`(https://example.com)` 里的右括号是外面的）。
///
/// 一条消息可以贴好几条链接（「一句话提示词生成」那类分享常见），所以这里收全量，
/// 由调用方决定怎么用：截图会逐条截、合成一条消息发出去。
pub fn find_urls(text: &str) -> Vec<String> {
    let re = url_regex();
    let mut urls: Vec<String> = Vec::new();
    for matched in re.find_iter(text) {
        let url = trim_tail(matched.as_str());
        // 剥完之后至少还得剩个主机名，`见 https://。` 这种不算链接。
        let host = url.split_once("//").map_or("", |(_, rest)| rest);
        if host.is_empty() {
            continue;
        }
        if !urls.iter().any(|seen| seen == url) {
            urls.push(url.to_string());
        }
    }
    urls
}

/// 提取文本中第一个 http(s) URL。取法见 [`find_urls`]。
pub fn find_url(text: &str) -> Option<String> {
    find_urls(text).into_iter().next()
}

fn url_regex() -> &'static Regex {
    static URL_REGEX: OnceLock<Regex> = OnceLock::new();
    URL_REGEX.get_or_init(|| {
        Regex::new(r"https?://[A-Za-z0-9\-._~:/?#\[\]@!$&'()*+,;=%]+").expect("Invalid Regex")
    })
}

/// 去掉结尾那些属于句子而不属于地址的标点。
fn trim_tail(url: &str) -> &str {
    let mut end = url.len();
    while end > 0 {
        let keep = match url.as_bytes()[end - 1] {
            b'.' | b',' | b';' | b':' | b'!' | b'?' | b'\'' | b'"' => false,
            b')' => balanced(&url[..end], b'(', b')'),
            b']' => balanced(&url[..end], b'[', b']'),
            _ => true,
        };
        if keep {
            break;
        }
        end -= 1;
    }
    &url[..end]
}

/// 括号在这段地址里是否配平——不配平就说明右括号是外面那对的。
fn balanced(url: &str, open: u8, close: u8) -> bool {
    let count = |target: u8| url.bytes().filter(|byte| *byte == target).count();
    count(open) >= count(close)
}

/// 卡片载荷里「点开这张卡会去哪」的地址。
///
/// 群里的链接有一半不是以正文出现的：QQ 的小程序卡与分享卡都是一段 JSON，
/// 整段落在 `<json>` 元素里。载荷里的斜杠常被转义成 `\/`，所以这里按 JSON 解析，
/// 不在字符串上找字面的 `https://`。
///
/// 字段按可信度取：`qqdocurl` 是小程序真正打开的那个页面，`jumpUrl` 是分享卡的
/// 落地地址；载荷里那些 `icon` / `preview` 只是图，不取。已知路径都没命中时，
/// 再按字段名在整段载荷里找一遍——卡片的外层结构换得勤。
pub fn card_target_url(payload: &str) -> Option<String> {
    /// 已知的落地地址路径，按可信度排列（`meta` 底下那几种外层都见过）。
    const PATHS: &[&[&str]] = &[
        &["meta", "detail_1", "qqdocurl"],
        &["meta", "detail", "qqdocurl"],
        &["meta", "news", "qqdocurl"],
        &["meta", "miniapp", "qqdocurl"],
        &["meta", "detail_1", "jumpUrl"],
        &["meta", "detail", "jumpUrl"],
        &["meta", "news", "jumpUrl"],
        &["meta", "miniapp", "jumpUrl"],
        &["qqdocurl"],
        &["jumpUrl"],
    ];
    /// 兜底按字段名找时认的键，同样分先后。
    const KEYS: &[&str] = &["qqdocurl", "jumpUrl"];

    let mut bytes = payload.as_bytes().to_vec();
    let value = simd_json::to_owned_value(&mut bytes).ok()?;
    PATHS
        .iter()
        .find_map(|path| nested_str(&value, path))
        .or_else(|| KEYS.iter().find_map(|key| search_card_key(&value, key, 0)))
}

/// 按路径取值，中间断在哪一层都算没取到。
fn nested_str(value: &OwnedValue, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    non_empty(current)
}

/// 从外往里找第一个同名的字符串字段。层数有个上限，免得碰上畸形载荷把栈走深。
fn search_card_key(value: &OwnedValue, key: &str, depth: usize) -> Option<String> {
    const MAX_DEPTH: usize = 6;
    if depth > MAX_DEPTH {
        return None;
    }
    let object = value.as_object()?;
    if let Some(found) = object.get(key).and_then(non_empty) {
        return Some(found);
    }
    object
        .values()
        .find_map(|child| search_card_key(child, key, depth + 1))
}

fn non_empty(value: &OwnedValue) -> Option<String> {
    let text = value.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// 一条消息里的链接候选，按可信度排序。
///
/// 卡片给出的落地地址排在正文前面：它是「点开卡片会去哪」，而 `raw_message`
/// 里那串载荷还混着封面与图标的地址，正则先抓到的多半不是它。
///
/// 正文只取第一个，与 [`find_url`] 在同一处；卡片可以有好几张，按出现顺序排。
pub fn message_links(ctx: &Context) -> Vec<String> {
    let mut links = Vec::new();
    let Some(message) = ctx.as_message() else {
        return links;
    };
    if let Some(segments) = message.0.get_array("message") {
        for segment in segments {
            if segment.get_str("type") != Some("json") {
                continue;
            }
            let Some(data) = segment.get("data") else {
                continue;
            };
            let payload = data
                .get_str("data")
                .or_else(|| data.get_str("content"))
                .unwrap_or("");
            if let Some(url) = card_target_url(payload) {
                push_unique(&mut links, url);
            }
        }
    }
    if let Some(url) = find_url(message.text()) {
        push_unique(&mut links, url);
    }
    links
}

fn push_unique(links: &mut Vec<String>, url: String) {
    if !links.iter().any(|seen| seen == &url) {
        links.push(url);
    }
}

/// 从指令参数或引用回复中提取第一张图片的 URL
pub async fn get_image_url(
    ctx: &Context,
    writer: LockedWriter,
    args: &[OwnedValue],
    reply_id: Option<&String>,
) -> Option<String> {
    // 1. 指令参数中直接携带图片
    for seg in args {
        if seg.get_str("type") == Some("image")
            && let Some(data) = seg.get("data")
            && let Some(url) = data.get_str("url")
        {
            return Some(url.to_string());
        }
    }

    // 2. 引用回复中的图片
    let resp = api::get_msg(ctx, writer, reply_id?).await.ok()?;
    resp.message.0.iter().find_map(|seg| {
        if seg.type_ == "image"
            && let Some(url) = seg.data.get("url").and_then(|v| v.as_str())
        {
            Some(url.to_string())
        } else {
            None
        }
    })
}

/// 解析指令：自动过滤头部的 Reply/At/空白，匹配 [Prefix][Command]，返回参数及引用信息
pub fn match_command(ctx: &Context, command_name: &str) -> Option<CommandMatch> {
    match_command_inner(ctx, command_name, false)
}

/// Word commands require whitespace or end-of-message after their name.
pub fn match_word_command(ctx: &Context, command_name: &str) -> Option<CommandMatch> {
    match_command_inner(ctx, command_name, true)
}

fn match_command_inner(ctx: &Context, command_name: &str, strict: bool) -> Option<CommandMatch> {
    let prefixes = get_prefixes(ctx);
    // 仅处理 MessageEvent
    let msg_arr = ctx.as_message()?.0.get_array("message")?;
    match_segments(msg_arr, &prefixes, command_name, strict)
}

/// 紧跟在 `at` 元素后面的那串显示文字之后的候选起点。
///
/// QQ 在引用回复（或手动 @）时，除了 `<at id="…"/>` 之外，还把「@昵称 」当作**普通文字**
/// 紧跟在后面：`<quote/><at id="3844710092"/>@lary /扫码`。指令因此不在文字开头，
/// 要先越过这段显示名才找得到。昵称里可以有空格（「@汽修二班 阿洛」），所以按空白依次
/// 试：跳过 1 到 3 个词之后剩下的部分都是候选，至多三个——名字再长就不是在叫人，是在说话。
/// 不以 `@` 开头的文字没有这回事，返回空。
pub fn mention_tails(text: &str) -> Vec<&str> {
    let text = text.trim_start();
    if !text.starts_with('@') {
        return Vec::new();
    }
    let mut tails = Vec::new();
    let mut rest = text;
    for _ in 0..3 {
        let Some(end) = rest.find(char::is_whitespace) else {
            break;
        };
        rest = rest[end..].trim_start();
        if rest.is_empty() {
            break;
        }
        tails.push(rest);
    }
    tails
}

/// [`match_command_inner`] 里不依赖上下文的那一半：在消息段里找指令，便于单测。
fn match_segments(
    msg_arr: &[OwnedValue],
    prefixes: &[String],
    command_name: &str,
    strict: bool,
) -> Option<CommandMatch> {
    let mut reply_id = None;
    let mut at_ids = Vec::new();
    // 上一段是 `at`：它后面的文字开头可能是那个 @ 的显示名，见 [`mention_tails`]。
    let mut after_at = false;
    for (i, segment) in msg_arr.iter().enumerate() {
        let type_ = segment.get_str("type")?;
        let data = segment.get("data")?;

        match type_ {
            "reply" => {
                if reply_id.is_none() {
                    // 尝试获取 id (可能是字符串或数字)
                    let id_str = data
                        .get_str("id")
                        .map(String::from)
                        .or_else(|| data.get_i64("id").map(|v| v.to_string()))
                        .or_else(|| data.get_u64("id").map(|v| v.to_string()));
                    reply_id = id_str;
                }
            }
            "at" => {
                let qq_str = data
                    .get_str("qq")
                    .map(String::from)
                    .or_else(|| data.get_i64("qq").map(|v| v.to_string()))
                    .or_else(|| data.get_u64("qq").map(|v| v.to_string()));
                if let Some(qq) = qq_str {
                    at_ids.push(qq);
                }
                after_at = true;
            }
            "text" => {
                let raw_text = data.get_str("text").unwrap_or("");
                // 跳过首部纯空白文本
                let trimmed_start = raw_text.trim_start();
                if trimmed_start.is_empty() {
                    continue;
                }

                // 找到第一个有效文本节点，尝试匹配：先按原文，紧跟在 @ 后面的再试着
                // 越过那段显示名（引用回复时 QQ 自带的「@昵称 」）。
                let mut candidates = vec![trimmed_start];
                if after_at {
                    candidates.extend(mention_tails(trimmed_start));
                }
                for candidate in candidates {
                    for prefix in prefixes {
                        let target = format!("{prefix}{command_name}");
                        if candidate.starts_with(&target) {
                            // 匹配成功
                            let mut args = Vec::new();

                            // 处理当前文本节点剩余部分
                            let rest_of_text = &candidate[target.len()..];
                            // 指令后通常有空格，作为参数时去除左侧空格
                            if strict
                                && rest_of_text
                                    .chars()
                                    .next()
                                    .is_some_and(|c| !c.is_whitespace())
                            {
                                continue;
                            }
                            let args_text = rest_of_text.trim_start();

                            if !args_text.is_empty() {
                                let mut new_seg = segment.clone();
                                new_seg["data"]["text"] = OwnedValue::from(args_text);
                                args.push(new_seg);
                            }

                            // 将后续所有节点加入 args
                            for seg in msg_arr.iter().skip(i + 1) {
                                args.push(seg.clone());
                            }

                            return Some(CommandMatch {
                                reply_id,
                                at_ids,
                                args,
                            });
                        }
                    }
                }
                // 如果遇到第一个有效文本但未匹配成功，则视为匹配失败
                return None;
            }
            // 遇到其他类型（如图片）且未匹配到指令，停止
            _ => return None,
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use simd_json::base::ValueAsArray;

    fn segments(value: serde_json::Value) -> Vec<OwnedValue> {
        simd_json::serde::to_owned_value(value)
            .unwrap()
            .as_array()
            .unwrap()
            .clone()
    }

    fn slash() -> Vec<String> {
        vec!["/".to_string()]
    }

    fn text(value: &str) -> serde_json::Value {
        serde_json::json!({"type": "text", "data": {"text": value}})
    }

    fn at(id: &str) -> serde_json::Value {
        serde_json::json!({"type": "at", "data": {"qq": id}})
    }

    /// 线上抓到的形状：引用回复时 QQ 自带 `<at/>`，并把「@lary 」当普通文字紧跟在后面。
    #[test]
    fn a_quoted_reply_with_the_auto_mention_still_matches() {
        let message = segments(serde_json::json!([
            {"type": "reply", "data": {"id": "7692256523120004036"}},
            at("3844710092"),
            text("@lary /扫码"),
        ]));
        let matched = match_segments(&message, &slash(), "扫码", true).expect("应当认出指令");
        assert_eq!(matched.reply_id.as_deref(), Some("7692256523120004036"));
        assert_eq!(matched.at_ids, ["3844710092"]);
        assert!(matched.args.is_empty());
    }

    #[test]
    fn the_mention_may_have_spaces_and_the_command_may_have_arguments() {
        let message = segments(serde_json::json!([
            at("1"),
            text("@汽修二班 阿洛 /md # 标题")
        ]));
        let matched = match_segments(&message, &slash(), "md", true).unwrap();
        assert_eq!(extract_text_arg(&matched.args), "# 标题");
        // 后面跟的图片等段落照常进参数。
        let with_image = segments(serde_json::json!([
            at("1"),
            text("@lary /扫码 "),
            {"type": "image", "data": {"url": "http://a/1.png"}},
        ]));
        let matched = match_segments(&with_image, &slash(), "扫码", true).unwrap();
        assert_eq!(matched.args.len(), 1);
    }

    /// 越过显示名只在 @ 之后、只试前三个词：普通聊天里夹着指令词不触发。
    #[test]
    fn mid_sentence_mentions_do_not_trigger() {
        for message in [
            // 没有 at 段：文字再像也不越过。
            serde_json::json!([text("@lary /扫码")]),
            // 名字不会有四个词那么长。
            serde_json::json!([at("1"), text("@a b c d /扫码")]),
            // 指令词后紧跟别的字（strict）。
            serde_json::json!([at("1"), text("@lary /扫码连热点")]),
            // at 之后的文字不以 @ 开头：照旧要求一上来就是指令。
            serde_json::json!([at("1"), text("你看 /扫码")]),
        ] {
            let message_segments = segments(message.clone());
            assert!(
                match_segments(&message_segments, &slash(), "扫码", true).is_none(),
                "{message}"
            );
        }
    }

    #[test]
    fn mention_tails_skip_one_to_three_words() {
        assert_eq!(mention_tails("@lary /扫码"), ["/扫码"]);
        assert_eq!(
            mention_tails("@汽修二班 阿洛 /扫码"),
            ["阿洛 /扫码", "/扫码"]
        );
        assert_eq!(mention_tails("@a b c /x y").len(), 3);
        assert!(mention_tails("没有艾特 /扫码").is_empty());
        assert!(mention_tails("@lary").is_empty());
        assert!(mention_tails("@lary   ").is_empty());
    }

    /// 原有的匹配不受影响：没有 @ 时与从前一样。
    #[test]
    fn plain_commands_match_as_before() {
        let message = segments(serde_json::json!([text("/echo 你好")]));
        let matched = match_segments(&message, &slash(), "echo", false).unwrap();
        assert_eq!(extract_text_arg(&matched.args), "你好");
        assert!(match_segments(&message, &slash(), "ech0", false).is_none());
        let leading_at = segments(serde_json::json!([at("1"), text(" /echo 你好")]));
        assert!(match_segments(&leading_at, &slash(), "echo", false).is_some());
    }
}
