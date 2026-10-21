//! 二维码：识别与生成两个方向，同一组指令。
//!
//! **识别**：指令后附图，或引用一张图再发指令，把图里**全部**二维码转成文字与链接。
//! 一个码就一行标题加内容，原样放在单独一行，长按可复制、QQ 自己会认出链接；
//! 多个码按阅读顺序编号，并在原图上把每个框出来、标上同样的序号。展示用文字而不是
//! 图片——图里的链接复制不了，也就失去了「二维码转链接」的意义（见 [`content`]）。
//!
//! 不发「正在识别」：结果本身就是回应，发出时引用请求那条。只有显式指令用错了、
//! 或者什么都没识别出来时才说话，并给出下一步。
//!
//! 只响应指令，不去扫群里每一张图：那样既费电又会在不相干的群里冒出来。
//!
//! **生成**（[`generate`]）：`/生成二维码 内容`，或引用一条消息再发 `/生成二维码`，回一张好扫的图。
//! `/二维码`、`/qr` 两个两头都沾的指令词看手里有什么：有图就识别，没图而有文字就生成——
//! 人说「二维码 github.com」时，要的显然不是去找图。

use crate::adapters::satori::{LockedWriter, api, delivery_uncertain, send_msg};
use crate::command::{self, get_prefixes};
use crate::event::Context;
use crate::http::download_bytes;
use crate::message::Message;
use crate::plugins::{PluginConfig, PluginError, get_config_or_default};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::future::BoxFuture;
use image::{DynamicImage, ImageDecoder, RgbImage};
use serde::{Deserialize, Serialize};
use simd_json::OwnedValue;
use simd_json::base::ValueAsScalar;
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsArray, ValueObjectAccessAsScalar};
use std::io::Cursor;
use std::time::{Duration, Instant};

pub mod annotate;
pub mod content;
pub mod generate;
pub mod scan;

const LOG_TARGET: &str = "Plugin/QrScan";

/// 指令要做哪个方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Intent {
    /// 只认图：把图里的二维码转成文字。
    Scan,
    /// 只认文字：把内容转成二维码图。
    Generate,
    /// 有图就识别；没图而有文字就生成。
    Either,
}

/// 指令词。`扫码` 最顺口，`qr` 给英文输入法；词后须有空白或结尾，
/// 所以「扫码连热点」「二维码怎么用」这样的话不会触发。
const COMMANDS: [(&str, Intent); 10] = [
    ("扫码", Intent::Scan),
    ("识别二维码", Intent::Scan),
    ("二维码", Intent::Either),
    ("qr", Intent::Either),
    ("qrcode", Intent::Either),
    ("生成二维码", Intent::Generate),
    ("转二维码", Intent::Generate),
    ("做二维码", Intent::Generate),
    ("二维码生成", Intent::Generate),
    ("qrgen", Intent::Generate),
];

/// 单张图下载后的体积上限。QQ 里发的图远小于它；挡的是别处来的怪文件。
const MAX_IMAGE_BYTES: usize = 30 * 1024 * 1024;

// ================= 配置定义 =================

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    enabled: bool,
    /// 一次最多识别几张图（指令里附的，或被引用那条里的）。
    max_images: usize,
    /// 一张图最多找几个码，也是一条回复最多列几个。
    max_codes: usize,
    /// 一共不止一个码时，把原图发回来，每个码框出并标上与清单对应的序号。
    annotate: bool,
    /// 下载加识别的总预算（秒）；到点用已经找到的结果收工。
    timeout_seconds: u64,
    /// 是否提供「文字转二维码」。关掉后只剩识别，`/二维码` 等指令词也只当识别用。
    generate: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            max_images: 4,
            max_codes: 12,
            annotate: true,
            timeout_seconds: 30,
            generate: true,
        }
    }
}

impl PluginConfig for Config {
    const NAME: &'static str = "qr_scan";

    fn check(&self) -> Result<(), String> {
        if !(1..=8).contains(&self.max_images) {
            return Err("max_images 需在 1—8 之间".into());
        }
        if !(1..=50).contains(&self.max_codes) {
            return Err("max_codes 需在 1—50 之间".into());
        }
        if !(5..=120).contains(&self.timeout_seconds) {
            return Err("timeout_seconds 需在 5—120 之间".into());
        }
        Ok(())
    }
}


// ================= 取指令与图片 =================

/// 一条触发了本插件的消息：指令前后附的图、指令词后面的文字，与被引用的那条。
#[derive(Debug, PartialEq)]
struct Request {
    intent: Intent,
    image_urls: Vec<String>,
    /// 指令词后面的文字（已去首尾空白）。识别时只是参数、不看；生成时就是要转的内容。
    text: String,
    reply_id: Option<String>,
}

/// 消息开头（引用、@ 与图片之后的第一段文字）是不是这个插件的指令。
///
/// 通用的指令匹配遇到图片就停手，而 QQ 里「先贴图、再打字」是常见的发法，
/// 所以这里自己走一遍：引用、@、图片都跳过，第一段有内容的文字必须以
/// 前缀加指令词开头，其后是空白或结尾；词后的文字原样收下当参数。
///
/// `generate` 为假时，生成类的指令词不认，两头都沾的当识别用。
fn parse_request(segments: &[OwnedValue], prefixes: &[String], generate: bool) -> Option<Request> {
    let mut reply_id = None;
    let mut image_urls = Vec::new();
    let mut intent = None;
    let mut text = String::new();
    // 上一段是 @：引用回复时 QQ 会在 `<at/>` 后面紧跟一段「@昵称 」的普通文字，指令在它后面。
    let mut after_at = false;
    for segment in segments {
        let data = segment.get("data");
        match segment.get_str("type") {
            Some("reply") => {
                if reply_id.is_none() {
                    reply_id = data.and_then(|data| {
                        data.get_str("id")
                            .map(String::from)
                            .or_else(|| data.get_i64("id").map(|id| id.to_string()))
                            .or_else(|| data.get_u64("id").map(|id| id.to_string()))
                    });
                }
            }
            Some("at") => after_at = true,
            Some("image") => image_urls.extend(data.and_then(image_url_of)),
            Some("text") if intent.is_none() => {
                let head = data.and_then(|data| data.get_str("text")).unwrap_or("");
                let head = head.trim_start();
                if head.is_empty() {
                    continue;
                }
                let mut candidates = vec![head];
                if after_at {
                    candidates.extend(command::mention_tails(head));
                }
                let (found, rest) = candidates
                    .into_iter()
                    .find_map(|candidate| match_command_word(candidate, prefixes, generate))?;
                intent = Some(found);
                text.push_str(rest);
            }
            Some("text") => text.push_str(data.and_then(|data| data.get_str("text")).unwrap_or("")),
            // 指令之前出现别的东西（表情、文件……）：这不是在对机器人说话。
            _ if intent.is_none() => return None,
            _ => {}
        }
    }
    Some(Request {
        intent: intent?,
        image_urls,
        text: text.trim().to_string(),
        reply_id,
    })
}

/// 文字是不是以「前缀 + 指令词」开头、词后是空白或结尾；是就给出意图和词后面的那段。
fn match_command_word<'a>(
    text: &'a str,
    prefixes: &[String],
    generate: bool,
) -> Option<(Intent, &'a str)> {
    for prefix in prefixes {
        for (word, intent) in COMMANDS {
            let intent = match (intent, generate) {
                (Intent::Generate, false) => continue,
                (Intent::Either, false) => Intent::Scan,
                (intent, _) => intent,
            };
            let target = format!("{prefix}{word}");
            let Some(head) = text.get(..target.len()) else {
                continue;
            };
            let rest = &text[target.len()..];
            if head.eq_ignore_ascii_case(&target) && rest.chars().next().is_none_or(char::is_whitespace)
            {
                return Some((intent, rest));
            }
        }
    }
    None
}

fn image_url_of(data: &OwnedValue) -> Option<String> {
    data.get_str("url")
        .or_else(|| data.get_str("file").filter(|file| file.starts_with("http")))
        .filter(|url| !url.is_empty())
        .map(String::from)
}

/// 被引用的那条消息里能用的东西：图片地址与文字。
#[derive(Debug, Default)]
struct Quoted {
    image_urls: Vec<String>,
    text: String,
}

impl Quoted {
    fn of(message: &Message) -> Self {
        let mut quoted = Quoted::default();
        for segment in &message.0 {
            let value = |key: &str| {
                segment
                    .data
                    .get(key)
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.is_empty())
            };
            match segment.type_.as_str() {
                "image" => quoted.image_urls.extend(value("url").map(String::from)),
                "text" => quoted.text.push_str(value("text").unwrap_or("")),
                _ => {}
            }
        }
        quoted.text = quoted.text.trim().to_string();
        quoted
    }
}

// ================= 识别 =================

/// 一张图识别完的结果。
struct Scanned {
    codes: Vec<scan::Decoded>,
    /// 标记用的预览图与缩放比例；没找到码或不标注时没有。
    preview: Option<(RgbImage, f32)>,
}

/// 解码图片（按 EXIF 转正）、找码；找到了且要标注才缩出预览。
fn scan_image(bytes: Vec<u8>, limits: scan::Limits, with_preview: bool) -> Result<Scanned, String> {
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|_| "不是能打开的图片".to_string())?;
    let mut image_limits = image::Limits::default();
    image_limits.max_image_width = Some(20_000);
    image_limits.max_image_height = Some(20_000);
    image_limits.max_alloc = Some(384 * 1024 * 1024);
    decoder
        .set_limits(image_limits)
        .map_err(|_| "图片尺寸太大，没法识别".to_string())?;
    let orientation = decoder.orientation().ok();
    let mut image = DynamicImage::from_decoder(decoder).map_err(|_| "图片解码失败".to_string())?;
    if let Some(orientation) = orientation {
        image.apply_orientation(orientation);
    }

    let codes = scan::scan(&scan::to_gray(&image), limits);
    let preview = (with_preview && !codes.is_empty()).then(|| annotate::preview_of(&image));
    Ok(Scanned { codes, preview })
}

/// 下载并识别一张图；失败时返回给人看的原因。
async fn fetch_and_scan(
    url: &str,
    limits: scan::Limits,
    with_preview: bool,
) -> Result<Scanned, String> {
    let remaining = limits.deadline.saturating_duration_since(Instant::now());
    let bytes = tokio::time::timeout(remaining.max(Duration::from_secs(5)), download_bytes(url))
        .await
        .map_err(|_| "下载超时".to_string())?
        .map_err(|e| format!("下载失败（{e}）"))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!("图片有 {} MB，太大了", bytes.len() / 1024 / 1024));
    }
    crate::render::worker::run(move || scan_image(bytes, limits, with_preview))
        .await
        .map_err(|e| {
            error!(target: LOG_TARGET, "识别任务异常：{}", e);
            "识别过程出错".to_string()
        })?
}

// ================= 摆放结果 =================

/// 一条回复要摆的东西。
#[derive(Debug)]
struct Arrangement {
    entries: Vec<content::Entry>,
    /// 每张图上要标的码（序号与角点，已换算到预览图坐标），与输入的图一一对应。
    marks: Vec<Vec<annotate::Mark>>,
    /// 超出上限没列的个数。
    omitted: usize,
    /// 一个码都没有的图，序号从 1 起，只在附了不止一张图时有意义。
    empty_images: Vec<usize>,
}

/// 把各张图的结果按顺序编号：序号跨图连续，总数不超过 `max_codes`。
fn arrange(scanned: &[Scanned], max_codes: usize) -> Arrangement {
    let mut entries = Vec::new();
    let mut marks = Vec::new();
    let mut omitted = 0;
    let mut empty_images = Vec::new();
    for (index, shot) in scanned.iter().enumerate() {
        if shot.codes.is_empty() {
            empty_images.push(index + 1);
        }
        let ratio = shot.preview.as_ref().map_or(1.0, |(_, ratio)| *ratio);
        let mut image_marks = Vec::new();
        for code in &shot.codes {
            if entries.len() >= max_codes {
                omitted += 1;
                continue;
            }
            entries.push(content::describe(&code.text));
            image_marks.push(annotate::Mark {
                number: entries.len(),
                corners: code.corners.map(|(x, y)| (x * ratio, y * ratio)),
            });
        }
        marks.push(image_marks);
    }
    Arrangement {
        entries,
        marks,
        omitted,
        empty_images,
    }
}

/// 回复正文：清单，外加「哪几张图里没有」「有几张图没读到」的一句补充。
fn reply_text(arrangement: &Arrangement, image_count: usize, failures: &[String]) -> String {
    let mut text = content::render(&arrangement.entries, arrangement.omitted);
    if image_count > 1 && !arrangement.empty_images.is_empty() {
        let which = arrangement
            .empty_images
            .iter()
            .map(|number| format!("第 {number} 张"))
            .collect::<Vec<_>>()
            .join("、");
        text.push_str(&format!("\n\n{which}图里没有识别到二维码"));
    }
    if !failures.is_empty() {
        text.push_str(&format!(
            "\n\n有 {} 张图没能读取：{}",
            failures.len(),
            failures.join("；")
        ));
    }
    text
}

fn failure_text(title: &str, hint: &str) -> String {
    format!("❌ {title}\n{hint}")
}

const USAGE_SCAN: &str = "❌ 没有找到图片\n发「/扫码」时附上图片，或引用一张图片再发「/扫码」";
const USAGE_GENERATE: &str =
    "❌ 没有可以转成二维码的内容\n发「/生成二维码 内容」，或引用一条文字消息再发「/生成二维码」";
/// 要生成的内容为空，而手里却有图：多半是把识别和生成弄混了。
const USAGE_GENERATE_WITH_IMAGE: &str =
    "❌ 没有可以转成二维码的内容\n要识别图片里的二维码，请发「/扫码」";
const USAGE_EITHER: &str = "❌ 没有找到图片或文字\n识别：发「/二维码」时附上图片，或引用一张图片再发\n生成：发「/二维码 内容」，或引用一条文字消息再发";

/// 没读到被引用的消息时，告诉人怎么绕过去。
fn quote_failure_hint(intent: Intent) -> &'static str {
    match intent {
        Intent::Scan => "请把图片直接附在「/扫码」后面发送",
        Intent::Generate => "请把内容直接写在「/生成二维码」后面",
        Intent::Either => "请把图片附在「/二维码」后面，或把要转的内容直接写在后面",
    }
}

// ================= 插件入口 =================

pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let Some(event) = ctx.as_message() else {
            return Ok(Some(ctx));
        };
        let Some(segments) = event.0.get_array("message") else {
            return Ok(Some(ctx));
        };
        let config: Config = get_config_or_default(&ctx);
        let prefixes = get_prefixes(&ctx);
        let Some(request) = parse_request(segments, &prefixes, config.generate) else {
            return Ok(Some(ctx));
        };
        let group_id = event.group_id().map(str::to_string);
        let user_id = event.user_id().to_string();
        let message_id = event.message_id().to_string();

        respond(
            &ctx,
            writer,
            &config,
            request,
            group_id.as_deref(),
            &user_id,
            &message_id,
        )
        .await?;
        Ok(None)
    })
}

/// 这一次回复要往哪去：识别哪几张图、转哪段文字，或者告诉人缺了什么。
#[derive(Debug, PartialEq)]
enum Plan {
    Scan(Vec<String>),
    Generate(String),
    Usage(&'static str),
}

/// 手里有什么、指令是哪个方向，决定做什么。图与文字都是「指令里自带的优先，没有再用被引用那条的」。
fn plan(intent: Intent, request_images: Vec<String>, request_text: String, quoted: Quoted) -> Plan {
    let images = if request_images.is_empty() {
        quoted.image_urls
    } else {
        request_images
    };
    let text = if request_text.is_empty() {
        quoted.text
    } else {
        request_text
    };
    match intent {
        Intent::Scan if images.is_empty() => Plan::Usage(USAGE_SCAN),
        Intent::Scan => Plan::Scan(images),
        Intent::Either if !images.is_empty() => Plan::Scan(images),
        Intent::Either | Intent::Generate if !text.is_empty() => Plan::Generate(text),
        Intent::Generate if !images.is_empty() => Plan::Usage(USAGE_GENERATE_WITH_IMAGE),
        Intent::Generate => Plan::Usage(USAGE_GENERATE),
        Intent::Either => Plan::Usage(USAGE_EITHER),
    }
}

async fn respond(
    ctx: &Context,
    writer: LockedWriter,
    config: &Config,
    request: Request,
    group_id: Option<&str>,
    user_id: &str,
    message_id: &str,
) -> Result<(), PluginError> {
    let reply = |text: String| Message::new().reply(message_id).text(text);
    let Request {
        intent,
        image_urls,
        text,
        reply_id,
    } = request;

    // 1. 被引用的那条：指令里没给够东西时才去读。
    let mut quoted = Quoted::default();
    let lacking = match intent {
        Intent::Generate => text.is_empty(),
        Intent::Scan | Intent::Either => image_urls.is_empty(),
    };
    if lacking && let Some(reply_id) = &reply_id {
        match api::get_msg(ctx, writer.clone(), reply_id).await {
            Ok(message) => quoted = Quoted::of(&message.message),
            // 生成不需要被引用的那条：指令后面自己写了内容就接着做。
            Err(e) if intent == Intent::Generate && !text.is_empty() => {
                warn!(target: LOG_TARGET, "读取被引用的消息失败（改用指令里的内容）: {}", e);
            }
            Err(e) => {
                warn!(target: LOG_TARGET, "读取被引用的消息失败: {}", e);
                let text = failure_text("没读到被引用的消息", quote_failure_hint(intent));
                send_msg(ctx, writer, group_id, Some(user_id), reply(text)).await?;
                return Ok(());
            }
        }
    }

    match plan(intent, image_urls, text, quoted) {
        Plan::Usage(usage) => {
            send_msg(ctx, writer, group_id, Some(user_id), reply(usage.to_string())).await
        }
        Plan::Scan(urls) => {
            respond_scan(ctx, writer, config, urls, group_id, user_id, message_id).await
        }
        Plan::Generate(content) => {
            respond_generate(ctx, writer, &content, group_id, user_id, message_id).await
        }
    }
}

// ================= 生成 =================

/// 回复里随图带的说明：补了协议头要交代，码太密要提醒；都没有就只发图。
fn generate_caption(prepared: &generate::Prepared, rendered: &generate::Rendered) -> Option<String> {
    let mut notes = Vec::new();
    if prepared.completed {
        notes.push(format!("🔗 已补全协议头\n{}", prepared.text));
    }
    if rendered.is_dense() {
        notes.push("⚠️ 内容较多，二维码比较密\n扫不出来时，先把图片放大再扫，或缩短内容".to_string());
    }
    (!notes.is_empty()).then(|| notes.join("\n\n"))
}

fn too_long_text(chars: usize) -> String {
    failure_text(
        "内容太长，一个二维码装不下",
        &format!(
            "这段有 {chars} 个字，一个码最多装约 980 个汉字或 2900 个英文字符\n长内容可以先放到网页或文档里，再用链接生成"
        ),
    )
}

async fn respond_generate(
    ctx: &Context,
    writer: LockedWriter,
    content: &str,
    group_id: Option<&str>,
    user_id: &str,
    message_id: &str,
) -> Result<(), PluginError> {
    let reply = |text: String| Message::new().reply(message_id).text(text);
    let prepared = generate::prepare(content);
    let to_encode = prepared.text.clone();
    // 编一张码是毫秒级的事，不去排渲染池的队：别人的长图识别占着池子时，生成不该跟着等。
    let rendered = match tokio::task::spawn_blocking(move || generate::render(&to_encode)).await {
        Ok(Ok(rendered)) => rendered,
        Ok(Err(generate::TooLong { chars })) => {
            send_msg(ctx, writer, group_id, Some(user_id), reply(too_long_text(chars))).await?;
            return Ok(());
        }
        Err(e) => {
            error!(target: LOG_TARGET, "生成任务异常：{}", e);
            let text = failure_text("二维码没能生成", "稍后再试一次");
            send_msg(ctx, writer, group_id, Some(user_id), reply(text)).await?;
            return Ok(());
        }
    };
    // 内容可能是密码、密钥，日志只记规模。
    info!(
        target: LOG_TARGET,
        "生成二维码：{} 字，{} 格",
        prepared.text.chars().count(),
        rendered.modules
    );

    let mut message = Message::new().reply(message_id).image_described(
        format!("base64://{}", STANDARD.encode(&rendered.png)),
        "二维码",
    );
    if let Some(caption) = generate_caption(&prepared, &rendered) {
        message = message.text(caption);
    }
    match send_msg(ctx, writer.clone(), group_id, Some(user_id), message).await {
        Ok(()) => Ok(()),
        // 结果未知的不能再发一遍：那一条可能晚到，会重复。
        Err(e) if delivery_uncertain(&*e) => Err(e),
        Err(e) => {
            warn!(target: LOG_TARGET, "二维码图片发送失败: {}", e);
            let text = failure_text("二维码图片没能发出", "稍后再试一次");
            send_msg(ctx, writer, group_id, Some(user_id), reply(text)).await
        }
    }
}

// ================= 识别并回复 =================

async fn respond_scan(
    ctx: &Context,
    writer: LockedWriter,
    config: &Config,
    mut urls: Vec<String>,
    group_id: Option<&str>,
    user_id: &str,
    message_id: &str,
) -> Result<(), PluginError> {
    let reply = |text: String| Message::new().reply(message_id).text(text);
    urls.truncate(config.max_images.clamp(1, 8));

    // 逐张下载、识别。一张失败不连坐别的。
    let limits = scan::Limits {
        max_codes: config.max_codes.clamp(1, 50),
        deadline: Instant::now() + Duration::from_secs(config.timeout_seconds.clamp(5, 120)),
    };
    let mut scanned = Vec::new();
    let mut failures = Vec::new();
    for (index, url) in urls.iter().enumerate() {
        match fetch_and_scan(url, limits, config.annotate).await {
            Ok(shot) => scanned.push(shot),
            Err(reason) => {
                warn!(target: LOG_TARGET, "第 {} 张图失败：{}", index + 1, reason);
                failures.push(if urls.len() > 1 {
                    format!("第 {} 张{}", index + 1, reason)
                } else {
                    reason
                });
            }
        }
    }

    let arrangement = arrange(&scanned, config.max_codes.clamp(1, 50));
    if arrangement.entries.is_empty() {
        let text = if scanned.is_empty() {
            failure_text("图片没能读取", &failures.join("；"))
        } else {
            failure_text(
                "没有识别到二维码",
                "换一张更清晰、二维码完整入镜的原图再试试",
            )
        };
        send_msg(ctx, writer, group_id, Some(user_id), reply(text)).await?;
        return Ok(());
    }
    info!(
        target: LOG_TARGET,
        "识别到 {} 个二维码（{} 张图）",
        arrangement.entries.len() + arrangement.omitted,
        urls.len()
    );

    // 发送。多个码时先放标好序号的原图，再放清单；图发不出去就只发清单。
    let text = reply_text(&arrangement, urls.len(), &failures);
    let annotated = if config.annotate && arrangement.entries.len() + arrangement.omitted > 1 {
        annotated_images(&scanned, &arrangement)
    } else {
        Vec::new()
    };
    if !annotated.is_empty() {
        let mut message = Message::new().reply(message_id);
        for jpeg in &annotated {
            message = message.image(format!("base64://{}", STANDARD.encode(jpeg)));
        }
        match send_msg(
            ctx,
            writer.clone(),
            group_id,
            Some(user_id),
            message.text(text.clone()),
        )
        .await
        {
            Ok(()) => return Ok(()),
            // 结果未知的不能再发一遍：那一条可能晚到，会重复。
            Err(e) if delivery_uncertain(&*e) => return Err(e),
            Err(e) => warn!(target: LOG_TARGET, "带图发送失败，改发纯文字: {}", e),
        }
    }
    send_msg(ctx, writer, group_id, Some(user_id), reply(text)).await?;
    Ok(())
}

/// 每张有码的图画一张标好序号的预览，按图的顺序。
fn annotated_images(scanned: &[Scanned], arrangement: &Arrangement) -> Vec<Vec<u8>> {
    scanned
        .iter()
        .zip(&arrangement.marks)
        .filter(|(_, marks)| !marks.is_empty())
        .filter_map(|(shot, marks)| {
            let (preview, _) = shot.preview.as_ref()?;
            annotate::annotate(preview, marks)
        })
        .collect()
}
