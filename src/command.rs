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
        .find_map(|p| text.strip_prefix(p.as_str()).map(|rest| rest.trim_start()))
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
    for segment in arr.iter() {
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

/// 提取文本中第一个 http(s) URL。
///
/// 群聊里的链接几乎从不独占一行：前后粘着中文，后面跟着全角逗号、句号、引号或者
/// 一对括号。「避雷这个中转站https://platform.deepseek.com，pro 模型路由到 flash」
/// 里真正的地址到 `.com` 为止，之前只排除汉字的写法会把「，pro」也算进去。
///
/// 所以这里只认 RFC 3986 允许的那些 ASCII 字符——中文、全角标点、书名号、引号
/// 都不在其中，自然断开；再把结尾那几个几乎不可能属于地址的半角标点剥掉，
/// 包括与地址内部不成对的那半个括号（`(https://example.com)` 里的右括号是外面的）。
pub fn find_url(text: &str) -> Option<String> {
    static URL_REGEX: OnceLock<Regex> = OnceLock::new();
    let re = URL_REGEX.get_or_init(|| {
        Regex::new(r"https?://[A-Za-z0-9\-._~:/?#\[\]@!$&'()*+,;=%]+").expect("Invalid Regex")
    });
    let url = trim_tail(re.find(text)?.as_str());
    // 剥完之后至少还得剩个主机名，`见 https://。` 这种不算链接。
    let host = url.split_once("//").map(|(_, rest)| rest).unwrap_or("");
    (!host.is_empty()).then(|| url.to_string())
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
    let Some(object) = value.as_object() else {
        return None;
    };
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
        for segment in segments.iter() {
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
    let rid = reply_id?.parse::<i64>().ok()?;
    let resp = api::get_msg(ctx, writer, rid).await.ok()?;
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

    let mut reply_id = None;
    let mut at_ids = Vec::new();

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
            }
            "text" => {
                let raw_text = data.get_str("text").unwrap_or("");
                // 跳过首部纯空白文本
                let trimmed_start = raw_text.trim_start();
                if trimmed_start.is_empty() {
                    continue;
                }

                // 找到第一个有效文本节点，尝试匹配
                for prefix in &prefixes {
                    let target = format!("{}{}", prefix, command_name);
                    if trimmed_start.starts_with(&target) {
                        // 匹配成功
                        let mut args = Vec::new();

                        // 处理当前文本节点剩余部分
                        let rest_of_text = &trimmed_start[target.len()..];
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
    use super::{card_target_url, find_url, spoken_bodies};

    /// 引用回复带着平台补的 @ 进来时，正文里能认的那一截要挑得出来。
    #[test]
    fn the_platform_at_leaves_a_body_worth_reading() {
        // 不带 @：只有整段一个候选。
        assert_eq!(spoken_bodies("2"), vec!["2"]);
        assert_eq!(spoken_bodies("  视频  "), vec!["视频"]);
        // 带 @：整段之后从名字后面逐词往后补。
        assert_eq!(
            spoken_bodies("@A宝好腻害！ 视频"),
            vec!["@A宝好腻害！ 视频", "A宝好腻害！ 视频", "视频"]
        );
        // 昵称里带空格、或者有两个 @，多补几截。
        assert_eq!(spoken_bodies("@对吧 A 宝最可爱啦～ 2").last(), Some(&"2"));
        assert_eq!(spoken_bodies("@小黑 @小白 3").last(), Some(&"3"));
        // 光 @ 了人、没有别的话：候选只剩名字，认不认由调用方定。
        assert_eq!(spoken_bodies("@A宝好腻害！").last(), Some(&"A宝好腻害！"));
    }

    #[test]
    fn urls_stop_where_the_sentence_resumes() {
        // 群里最常见的形态：中文、全角标点直接粘在地址后面。
        assert_eq!(
            find_url(
                "避雷这个中转站https://platform.deepseek.com，pro模型路由到flash，真的是脸都不要了"
            )
            .as_deref(),
            Some("https://platform.deepseek.com")
        );
        assert_eq!(
            find_url("看这个 https://example.com/a/b?x=1&y=2。然后呢").as_deref(),
            Some("https://example.com/a/b?x=1&y=2")
        );
        assert_eq!(
            find_url("链接是「https://example.com/路径」").as_deref(),
            Some("https://example.com/")
        );
        // 半角句尾标点同样不属于地址。
        assert_eq!(
            find_url("see https://example.com, and more").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            find_url("go to https://example.com/docs.").as_deref(),
            Some("https://example.com/docs")
        );
    }

    #[test]
    fn brackets_are_kept_only_when_they_belong_to_the_url() {
        assert_eq!(
            find_url("(https://example.com/a)").as_deref(),
            Some("https://example.com/a")
        );
        // 维基百科那种地址里本来就带括号，配平的就留着。
        assert_eq!(
            find_url("https://en.wikipedia.org/wiki/Rust_(programming_language) 挺好").as_deref(),
            Some("https://en.wikipedia.org/wiki/Rust_(programming_language)")
        );
    }

    #[test]
    fn only_real_links_come_back() {
        assert_eq!(
            find_url("先 http://127.0.0.1:6520/panel#tab 再说").as_deref(),
            Some("http://127.0.0.1:6520/panel#tab")
        );
        // 百分号编码的中文路径是完整的地址，不能在编码处断开。
        assert_eq!(
            find_url("https://zh.wikipedia.org/wiki/%E4%B8%AD%E6%96%87 这个").as_deref(),
            Some("https://zh.wikipedia.org/wiki/%E4%B8%AD%E6%96%87")
        );
        assert_eq!(find_url("没有链接的一句话"), None);
        assert_eq!(find_url("裸域名 example.com 不算"), None);
        assert_eq!(find_url("https://。"), None);
    }

    /// 样例取自真机收到的卡片（B 站小程序卡与 B 站分享卡各一份），只裁掉不影响
    /// 取地址的字段。
    #[test]
    fn a_card_gives_up_the_page_it_opens() {
        // 小程序卡：斜杠是转义的，正则抓不到 `https://`，只有按 JSON 解析才拿得到。
        // 拿 `qqdocurl` 而不是 `url`——后者是小程序自己的路由页。
        let miniapp = r#"{"ver":"1.0.0.19","prompt":"[QQ小程序]琵琶曲","app":"com.tencent.miniapp_01",
            "meta":{"detail_1":{"title":"哔哩哔哩","desc":"琵琶曲","appid":"1109937557",
            "icon":"http:\/\/miniapp.gtimg.cn\/public\/appicon\/432b.jpg",
            "preview":"https:\/\/qq.ugcimg.cn\/v1\/gio99kjvll3gl6baq",
            "url":"m.q.qq.com\/a\/s\/7cbaf275098703ebfccdf98701b6bcd3",
            "qqdocurl":"https:\/\/b23.tv\/czQoMIg?share_medium=android&share_source=qq"}}}"#;
        assert_eq!(
            card_target_url(miniapp).as_deref(),
            Some("https://b23.tv/czQoMIg?share_medium=android&share_source=qq")
        );

        // 分享卡：落地地址在 `meta.news.jumpUrl`，同一张卡里还有封面与图标两个地址。
        let news = r#"{"app":"com.tencent.tuwen.lua","prompt":"[分享]视频","view":"news",
            "meta":{"news":{"appid":100951776,"desc":"Agent 工作时，人类可以做什么",
            "jumpUrl":"https://b23.tv/DONRtWF",
            "preview":"https://qq.ugcimg.cn/v1/odu6is84rije659prqcbgoornfg14q",
            "tagIcon":"https://open.gtimg.cn/open/app_icon/00/95/17/76/x.png"}}}"#;
        assert_eq!(
            card_target_url(news).as_deref(),
            Some("https://b23.tv/DONRtWF")
        );
    }

    /// 外层结构换个名字也要认，但只要「点开会去哪」的那两个字段，不碰图片地址。
    #[test]
    fn a_card_is_searched_by_field_name_when_the_shape_is_new() {
        let unknown = r#"{"app":"com.tencent.other","meta":{"foo":{"bar":{
            "title":"哔哩哔哩","icon":"https://example.com/icon.png",
            "jumpUrl":"https://b23.tv/abc123"}}}}"#;
        assert_eq!(
            card_target_url(unknown).as_deref(),
            Some("https://b23.tv/abc123")
        );

        // 只有封面与图标的卡片不是链接卡，取不到地址。
        let plain = r#"{"app":"com.tencent.other","meta":{"detail_1":{
            "title":"没有链接","icon":"https://example.com/a.png",
            "preview":"https://example.com/b.png"}}}"#;
        assert_eq!(card_target_url(plain), None);
    }

    #[test]
    fn a_card_that_is_not_json_is_left_alone() {
        assert_eq!(card_target_url(""), None);
        assert_eq!(card_target_url("不是一个 JSON"), None);
        assert_eq!(card_target_url("<msg serviceID=\"1\"></msg>"), None);
        // 字段为空串同样不算数。
        assert_eq!(card_target_url(r#"{"meta":{"news":{"jumpUrl":""}}}"#), None);
    }
}
