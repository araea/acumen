use super::data::Manager;
use super::parser::{Action, Command, Scope};
use super::types::{Agent, ChatMessage};
use super::utils::{escape_markdown_special, format_export_txt, format_history};
use crate::adapters::satori::{LockedWriter, api, send_msg};
use crate::event::{Context, MessageEvent};
use crate::message::Message;
use regex::Regex;
use rig_core::completion::message::{DocumentSourceKind, Image, Text, UserContent};
use rig_core::completion::Message as LlmMessage;
use std::{fs::File, io::Write, sync::Arc};

use super::LOG_TARGET;

/// 「正在处理」的状态表情：贴在触发消息上，处理完再揭掉。
const WORKING_EMOJI: &str = "124";

/// 「已并入正在进行的这一轮」的回执表情（赞）：贴上就不揭，让人知道这句话没被丢掉。
const STEERED_EMOJI: &str = "76";

/// 给触发消息贴上或揭掉「正在处理」表情。机器人自己手动发的指令不贴；
/// 贴不上不影响处理，只记一行调试日志。
pub(crate) async fn mark_working(
    ctx: &Context,
    writer: &LockedWriter,
    event: &MessageEvent<'_>,
    on: bool,
) {
    if event.is_manual_self() {
        return;
    }
    if let Err(error) =
        api::set_msg_emoji_like(ctx, writer.clone(), event.message_id(), WORKING_EMOJI, on).await
    {
        debug!(target: LOG_TARGET, "「正在处理」表情{}失败：{error}", if on { "贴上" } else { "揭掉" });
    }
}

/// 「还在生成」：这句话带信息（你这句没被接住），但同一个人这一轮里只说一次。
/// 再催的几句静默放过——第一次已经告诉过怎么停了。
async fn busy_notice(
    ctx: &Context,
    writer: &LockedWriter,
    event: &MessageEvent<'_>,
    mgr: &Manager,
    name: &str,
    private: bool,
    uid: &str,
) {
    let who = event.user_id().to_string();
    if !mgr.generating.write().await.first_notice(name, private, uid, &who) {
        debug!(target: LOG_TARGET, "{name} 还在生成，{who} 这一轮已经提醒过，不再重复");
        return;
    }
    reply_text(ctx, writer, event, format!("⏳ {name} 还在回上一句，这句没接上；等它说完再发，或发「{name}!」停下")).await;
}

pub(crate) async fn reply_text(
    ctx: &Context,
    writer: &LockedWriter,
    event: &MessageEvent<'_>,
    text: impl Into<String>,
) {
    if let Err(error) = crate::adapters::satori::send_text_chunks(
        ctx,
        writer.clone(),
        event.group_id(),
        Some(event.user_id()),
        &text.into(),
    )
    .await
    {
        warn!(target: LOG_TARGET, "回复发送失败：{error}");
    }
}

/// 把一张卡片图作为对触发消息的引用回复发出。发不出去只记日志——调用方此时已无别的退路。
async fn reply_image(
    ctx: &Context,
    writer: &LockedWriter,
    event: &MessageEvent<'_>,
    b64: &str,
    header: &str,
) {
    let message = Message::new()
        .reply(event.message_id())
        .image_described(format!("base64://{b64}"), header);
    if let Err(error) = send_msg(
        ctx,
        writer.clone(),
        event.group_id(),
        Some(event.user_id()),
        message,
    )
    .await
    {
        warn!(target: LOG_TARGET, "回复卡片发送失败：{error}");
    }
}

async fn reply(
    ctx: &Context,
    writer: &LockedWriter,
    event: &MessageEvent<'_>,
    text: &str,
    text_mode: bool,
    header: &str,
) {
    let figures = super::agent::figures::Figures::default();
    reply_card(ctx, writer, event, text, text_mode, header, &[], &figures, None).await;
}

/// 把回复渲染成卡片图片发出；`text_mode`、关掉出图、或渲染失败时退回纯文本。
/// 返回卡片有没有发出去——正文里内嵌的图跟着卡片走，没发出去的话调用方要把图另发。
///
/// 关图开关在这里读一次就够——全部卡片输出都汇到这一个函数，
/// 调用方不必各自判断。
#[allow(clippy::too_many_arguments)]
async fn reply_card(
    ctx: &Context,
    writer: &LockedWriter,
    event: &MessageEvent<'_>,
    text: &str,
    text_mode: bool,
    header: &str,
    sources: &[super::types::Source],
    figures: &super::agent::figures::Figures,
    footer: Option<super::render::Footer>,
) -> bool {
    let oai = crate::plugins::get_config_or_default::<super::OaiConfig>(ctx);
    let re = Regex::new(r"!\[.*?\]\((data:image/[^\s\)]+)\)").unwrap();
    // 文本回退里没有图的位置：有说明的留成〔图：说明〕，其余去掉。
    let spoken = super::agent::figures::without_images(text);
    let mut accessible = re.replace_all(&spoken, "[内嵌图片]").to_string();
    if !sources.is_empty() {
        accessible.push_str("\n\n参考来源：");
        for (index, source) in sources.iter().enumerate() {
            accessible.push_str(&format!(
                "\n{}. {}\n{}",
                index + 1,
                source.title,
                source.url
            ));
        }
    }
    if let Some(footer) = &footer {
        if !footer.meta.is_empty() {
            accessible.push_str(&format!("\n\n{}", footer.meta));
        }
        for step in &footer.trace {
            accessible.push_str(&format!(
                "\n工具：{} {} ×{}",
                step.name, step.detail, step.repeats
            ));
        }
        if footer.trace_overflow > 0 {
            accessible.push_str(&format!("\n另有 {} 次工具调用", footer.trace_overflow));
        }
    }
    let mut illustrated = false;
    if !text_mode && oai.image_enabled() {
        let card = super::render::Card {
            title: header,
            markdown: text,
            sources,
            figures,
            footer,
        };
        match super::render::render_card(card, oai.image_scale()).await {
            Ok(b64) => {
                illustrated = true;
                reply_image(ctx, writer, event, &b64, header).await;
            }
            Err(error) => warn!(target: LOG_TARGET, "回复卡片渲染失败，退回文本：{error:#}"),
        }
    }
    // 出图成功后不再补发等价文本：卡片已经把正文、来源与页脚都画进去了，
    // 再发一遍只是在群里刷屏。只有没出图时才退回文本。
    if !illustrated {
        reply_text(ctx, writer, event, accessible).await;
    }
    illustrated
}

/// 把一份列表渲染成卡片图发出；`text_mode`、关掉出图、或渲染失败时退回同一份数据的文本。
///
/// 与 [`reply_card`] 的分工：回复卡渲染的是模型写的 Markdown（要转义、要防注入、带来源与页脚），
/// 列表是插件自己整理的结构化数据，直接走手册卡 / 控制卡那套文档模型。
async fn reply_list(
    ctx: &Context,
    writer: &LockedWriter,
    event: &MessageEvent<'_>,
    listing: &super::lists::Listing,
    text_mode: bool,
    header: &str,
) {
    let oai = crate::plugins::get_config_or_default::<super::OaiConfig>(ctx);
    if !text_mode && oai.image_enabled() {
        let browser_path = ctx.browser_path();
        match crate::render::web::capture(
            &listing.doc(),
            oai.image_scale(),
            browser_path.as_deref(),
        )
        .await
        {
            Ok(b64) => {
                reply_image(ctx, writer, event, &b64, header).await;
                return;
            }
            Err(error) => warn!(target: LOG_TARGET, "列表卡片渲染失败，退回文本：{error:#}"),
        }
    }
    reply_text(ctx, writer, event, listing.markdown()).await;
}

fn extract_image_urls(content: &str) -> Vec<String> {
    let re = Regex::new(r"!\[.*?\]\(((?:https?://|data:image/)[^\s\)]+)\)|(?:https?://[^\s]+\.(?:png|jpg|jpeg|gif|webp|bmp))").unwrap();
    let mut urls: Vec<String> = re
        .captures_iter(content)
        .filter_map(|cap| cap.get(1).or(cap.get(0)).map(|m| m.as_str().to_string()))
        .collect();
    let mut seen = std::collections::HashSet::new();
    urls.retain(|url| seen.insert(url.clone()));
    urls
}

/// 去掉正文里的 markdown 图片；图片另作真图发出，文字部分留给卡片或文本。
fn strip_markdown_images(content: &str) -> String {
    let re = Regex::new(r"!\[.*?\]\(((?:https?://|data:image/)[^\s\)]+)\)").unwrap();
    let stripped = re.replace_all(content, "");
    let blank_runs = Regex::new(r"\n{3,}").unwrap();
    blank_runs.replace_all(stripped.trim(), "\n\n").into_owned()
}

/// 卡片没发出去时，正文里的图改作真图另发：取好的用取好的那份，远程直链交给实现端自己拉；
/// 本机文件取不到就没有办法了，那一张只能丢。
fn loose_images(content: &str, figures: &super::agent::figures::Figures) -> Vec<String> {
    super::agent::figures::sources(content)
        .into_iter()
        .filter_map(|source| match figures.get(&source) {
            Some(data_url) => Some(data_url.to_string()),
            None if source.starts_with("http://")
                || source.starts_with("https://")
                || source.starts_with("data:") =>
            {
                Some(source)
            }
            None => None,
        })
        .collect()
}

/// 把图片逐张作为真图发出，返回没发出去的那些。`quote` 为真时第一张引用用户那句话。
async fn send_images<'a>(
    ctx: &Context,
    writer: &LockedWriter,
    event: &MessageEvent<'_>,
    urls: &'a [String],
    quote: bool,
) -> Vec<&'a str> {
    let mut failed = Vec::new();
    for (index, url) in urls.iter().enumerate() {
        let file = match url.strip_prefix("data:") {
            Some(data) => if let Some((_, base64_data)) = data.split_once(',') { format!("base64://{base64_data}") } else {
                failed.push(url.as_str());
                continue;
            },
            None => url.clone(),
        };
        let mut message = Message::new();
        if quote && index == 0 {
            message = message.reply(event.message_id());
        }
        if let Err(error) = send_msg(
            ctx,
            writer.clone(),
            event.group_id(),
            Some(event.user_id()),
            message.image(file),
        )
        .await
        {
            warn!(target: LOG_TARGET, "图片发送失败 {}: {error}", crate::storage::truncate_chars(url, 80));
            failed.push(url.as_str());
        }
    }
    failed
}

/// 图片没发出去时的说明。生成一次要花钱，远程直链至少留给人自己去打开。
fn undelivered_notice(failed: &[&str]) -> String {
    let links: Vec<&str> = failed
        .iter()
        .copied()
        .filter(|url| !url.starts_with("data:"))
        .collect();
    if links.is_empty() {
        format!("❌ 有 {} 张图片没发出去", failed.len())
    } else {
        format!("❌ 图片没发出去，原图链接：\n{}", links.join("\n"))
    }
}

fn extract_video_urls(content: &str) -> Vec<String> {
    let re = Regex::new(r"\[download video\]\((https?://[^\s\)]+)\)").unwrap();
    re.captures_iter(content)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
        .collect()
}

/// 把图片地址转成多模态模型可内联的 data URL。
/// 已经是 `data:` 的保持不变；其余（QQ 图片 / 头像等远程 URL）下载后转 base64，
/// 避免 QQ 图片防盗链导致服务端拉不到图。下载失败则回退原 URL，由服务端自行尝试。
pub(crate) async fn to_data_url(url: &str) -> String {
    if url.starts_with("data:") {
        return url.to_string();
    }
    // 历史里的图片每一轮都会重新入参，没有缓存就意味着每轮重下一遍：多图会话里
    // 这部分等待往往比模型本身还久。
    if let Some(cached) = cached_data_url(url) {
        return cached;
    }
    match download_image_to_data_url(url).await {
        Some(data_url) => {
            remember_data_url(url, &data_url);
            data_url
        }
        None => url.to_string(),
    }
}

/// 远程图片 → data URL 的进程内缓存。
///
/// QQ 图片链接带签名且很快失效，缓存同时兼作「这张图还能用」的保底：链接过期后
/// 历史里的图仍然可以继续参与对话。容量到顶时整体清空——命中率远比精确淘汰重要，
/// 也省下维护 LRU 链表的复杂度。
fn data_url_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, String>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, String>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// 缓存条目上限，按每张图数 MB 的量级留出余量。
const DATA_URL_CACHE_CAPACITY: usize = 64;

fn cached_data_url(url: &str) -> Option<String> {
    data_url_cache().lock().ok()?.get(url).cloned()
}

fn remember_data_url(url: &str, data_url: &str) {
    if let Ok(mut cache) = data_url_cache().lock() {
        if cache.len() >= DATA_URL_CACHE_CAPACITY {
            cache.clear();
        }
        cache.insert(url.to_string(), data_url.to_string());
    }
}

async fn download_image_to_data_url(url: &str) -> Option<String> {
    const MAX_BYTES: usize = 20 * 1024 * 1024;

    let resp = crate::http::client()
        .get(url)
        .header(
            reqwest::header::USER_AGENT,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36",
        )
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);

    let bytes = resp.bytes().await.ok()?;
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return None;
    }

    let mime = content_type
        .filter(|ct| ct.starts_with("image/"))
        .unwrap_or_else(|| sniff_image_mime(&bytes).to_string());

    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(format!("data:{mime};base64,{b64}"))
}

fn sniff_image_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(b"BM") {
        "image/bmp"
    } else {
        "image/jpeg"
    }
}

fn prepare_history(
    history: &[ChatMessage],
    prompt: &str,
    imgs: &[String],
    regen: bool,
    from: &str,
) -> Vec<ChatMessage> {
    let mut hist = history.to_vec();
    if regen {
        if hist.last().is_some_and(|m| m.role == "assistant") {
            hist.pop();
        }
        if !prompt.is_empty() || !imgs.is_empty() {
            if hist.last().is_some_and(|m| m.role == "user") {
                hist.pop();
            }
            hist.push(ChatMessage::new("user", prompt, imgs.to_vec()).with_from(from));
        }
    } else {
        hist.push(ChatMessage::new("user", prompt, imgs.to_vec()).with_from(from));
    }
    hist
}

/// 房间的模型落到哪个接口：返回（接口基址、密钥、发给接口的模型 id）；失败时是一句可以直接回给用户的话。
///
/// 内置 agent 房间可以只写 `agent`（或干脆留空）表示「用默认模型」：先看 `[oai] agent_default_model`，
/// 再退到 oai config.json 里的 default_model；普通房间没有这层回退——模型是建房时就定下的。
/// 房间模型可以写成 `供应商/模型`，前缀决定打哪个接口，剥掉前缀的部分才是发给接口的模型 id。
/// 定时任务到点开一轮时走的是同一条路，所以收在这里一处。
pub(super) async fn room_endpoint(
    agent: &Agent,
    oai: &super::OaiConfig,
    mgr: &Manager,
    api: &(String, String),
) -> Result<(String, String, String), String> {
    let spec = if agent.uses_agent() && super::agent::uses_default_model(&agent.model) {
        let fallback = mgr.config.read().await.default_model.clone();
        oai.agent_default_model().unwrap_or(fallback.as_str()).to_string()
    } else {
        agent.model.clone()
    };
    if spec.trim().is_empty() {
        return Err(format!(
            "❌ {} 还没指定模型：用 `{}%供应商/模型` 设一个，或配置 [oai] agent_default_model",
            agent.name, agent.name
        ));
    }
    let (provider, chat_model) = super::utils::split_provider(&spec);
    let Some((api_base, api_key)) = super::resolve_endpoint(&oai.providers, &api.0, &api.1, provider.as_deref()) else {
        let name = provider.as_deref().unwrap_or_default();
        return Err(format!(
            "❌ {}用的是 {spec}，但不认得供应商「{name}」。\n自己的中转站发：oai {name} 地址 密钥\n认得的供应商发 `oai` 查看。",
            agent.name
        ));
    };
    if api_base.is_empty() || api_key.is_empty() {
        let mut text = match provider.as_deref().filter(|name| !name.eq_ignore_ascii_case("apilio")) {
            Some(name) => format!("❌ {}用的是 {spec}，但 {}", agent.name, super::catalog::missing_key_hint(name)),
            None => format!(
                "❌ {}用的是 {spec}，它走默认接口，而默认接口还没配。\n· 接中转站：oai 地址 密钥\n· 或换成官方供应商的模型：{}%deepseek/deepseek-flash",
                agent.name, agent.name
            ),
        };
        // 别家已经接好了：直接给一条能抄的换法。
        if let Some((name, Some(known))) = super::catalog::ready(&oai.providers)
            .into_iter()
            .find(|(_, known)| known.is_some_and(|known| !known.model.is_empty()))
        {
            text.push_str(&format!("\n已经接好的 {} 也能用：{}%{name}/{}", known.name, agent.name, known.model));
        }
        return Err(text);
    }
    Ok((api_base, api_key, chat_model))
}

/// 画图、MJ、写歌、拍片这些模型走各自的专用接口，只有普通房间会把话转给它们；
/// 内置智能体只会聊天补全，接上这种模型只会在第一轮就报错。
fn is_media_model(model: &str, oai: &super::OaiConfig) -> bool {
    let (_, id) = super::utils::split_provider(model);
    super::mj::is_mj_model(&id)
        || super::images::is_images_model(&id, &oai.image_models)
        || super::music::is_music_model(&id, &oai.music_models)
        || super::video::is_video_model(&id, &oai.video_models)
}

/// `chat`、`chat 模型`、`chat/模型`：把房间转成普通对话房间（不调工具）。写法同 [`super::agent::parse_agent_spec`]。
fn parse_chat_spec(spec: &str) -> Option<String> {
    const KEYWORD: &str = "chat";
    let spec = spec.trim();
    let tail = spec
        .get(..KEYWORD.len())
        .filter(|head| head.eq_ignore_ascii_case(KEYWORD))
        .map(|_| &spec[KEYWORD.len()..])?;
    let mut chars = tail.chars();
    match chars.next() {
        None => Some(String::new()),
        Some('/' | ':' | '：' | ' ' | '\t') => Some(chars.as_str().trim().to_string()),
        Some(_) => None,
    }
}

/// 一个模型写法落成（引擎, 模型）。
///
/// - `agent [模型]` / `chat [模型]` 明说引擎；
/// - 其余只换模型、**不换引擎**：管家大人写 `管家大人%deepseek-flash` 仍是内置智能体。
///   从前 `%` 一律把房间转回普通对话，内置智能体换个模型就悄悄丢了全部工具；
/// - 画图、写歌、拍片这类模型只能跑在普通房间里，写给智能体房间时自动转过去（回话里会说）。
///
/// 裸模型名在 `/%` 那份完整列表里找（序号、全名、只写模型名都行），找不到就原样用。
fn resolve_room_model(
    spec: &str,
    room_is_agent: bool,
    models: &[String],
    mgr: &Manager,
    oai: &super::OaiConfig,
) -> (&'static str, String) {
    let resolve = |text: &str| -> String {
        if text.is_empty() {
            return String::new();
        }
        match super::utils::split_provider(text) {
            // 带供应商前缀时不做列表匹配，原样保留前缀交给路由。
            (Some(_), _) => text.to_string(),
            (None, bare) => mgr.resolve_model(&bare, models).unwrap_or(bare),
        }
    };
    if let Some(model) = super::agent::parse_agent_spec(spec) {
        return (super::types::ENGINE_AGENT, resolve(&model));
    }
    if let Some(model) = parse_chat_spec(spec) {
        return (super::types::ENGINE_CHAT, resolve(&model));
    }
    let model = resolve(spec);
    let engine = if room_is_agent && !is_media_model(&model, oai) {
        super::types::ENGINE_AGENT
    } else {
        super::types::ENGINE_CHAT
    };
    (engine, model)
}

/// `agent_owner_only` 开着时，别人叫智能体房间（对话、重来、停、导出……一概）就挡下，返回真。
/// 在取引用、收文件之前判，客人发来的文件不会先落进智能体的收件箱。
pub(crate) async fn owner_only_denied(ctx: &Context, writer: &LockedWriter, name: &str, mgr: &Manager) -> bool {
    let oai = crate::plugins::get_config_or_default::<super::OaiConfig>(ctx);
    if !oai.agent_owner_only || crate::access::is_manager(ctx) {
        return false;
    }
    let is_agent = mgr.config.read().await.agents.iter().any(|a| a.name == name && a.uses_agent());
    if !is_agent {
        return false;
    }
    if let Some(event) = ctx.as_message()
        && first_denial(event.user_id(), name)
    {
        reply_text(ctx, writer, &event, format!("🔒 {name} 只对号主与机器人管理员开放")).await;
    }
    true
}

/// 被挡下的人，同一间房十分钟内只告诉一次：再叫只是没回应，不在群里来回刷「不开放」。
fn first_denial(user: &str, room: &str) -> bool {
    type Told = std::collections::HashMap<(String, String), std::time::Instant>;
    static TOLD: std::sync::LazyLock<std::sync::Mutex<Told>> = std::sync::LazyLock::new(Default::default);
    let mut told = TOLD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    told.retain(|_, at| at.elapsed() < std::time::Duration::from_mins(10));
    told.insert((user.to_string(), room.to_string()), std::time::Instant::now()).is_none()
}

#[allow(clippy::too_many_arguments)]
async fn chat(
    name: &str,
    prompt: &str,
    imgs: Vec<String>,
    regen: bool,
    cmd: &Command,
    ctx: &Context,
    writer: &LockedWriter,
    mgr: &Arc<Manager>,
) {
    let Some(event) = ctx.as_message() else { return };
    let (agent, api) = {
        let c = mgr.config.read().await;
        let a = c.agents.iter().find(|a| a.name == name).cloned();
        (a, (c.api_base.clone(), c.api_key.clone()))
    };

    let Some(agent) = agent else {
        reply_text(ctx, writer, &event, format!("❌ 智能体 {name} 不存在")).await;
        return;
    };

    let use_agent = agent.uses_agent();
    let oai = crate::plugins::get_config_or_default::<super::OaiConfig>(ctx);
    let (api_base, api_key, chat_model) = match room_endpoint(&agent, &oai, mgr, &api).await {
        Ok(endpoint) => endpoint,
        Err(message) => {
            reply_text(ctx, writer, &event, message).await;
            return;
        }
    };
    // 按剥掉供应商前缀后的名字判家族，与后面的图像模型判断保持一致。
    if !use_agent && super::mj::is_mj_model(&chat_model) {
        // MJ 房间天生是无历史任务流；引用文字也不应混入绘图提示词。
        super::mj::handle_agent(&agent, &cmd.args, imgs, ctx, writer, mgr).await;
        return;
    }

    let is_priv_ctx = cmd.private_reply;
    let uid = event.user_id().to_string();
    let temp_mode = cmd.temp_mode;
    let asker = super::agent::Asker::from_event(ctx, &event);
    // 公共历史是大家共用的：智能体房间里把说话人记在每句话上，下一轮展开时才不会张冠李戴。
    let from = if use_agent && !is_priv_ctx { asker.tag() } else { String::new() };

    if !temp_mode {
        let busy = mgr.generating.read().await.is_generating(name, is_priv_ctx, &uid);
        if busy {
            // 智能体房间里，发起这一轮的人又发了话：并进正在跑的这一轮（贴个赞表示收到），
            // 而不是让他等完再重发。带图的补充带不进去，仍按「正在生成」回。
            // 机器人自己发出去的话作为回声回来时，发送者也是账号自己：那不是号主在补充。
            let echo = uid == ctx.bot.self_id() && !event.is_manual_self();
            if use_agent
                && !echo
                && !regen
                && imgs.is_empty()
                && !prompt.is_empty()
                && mgr.steer(name, is_priv_ctx, &uid, prompt.to_string())
            {
                if let Err(error) =
                    api::set_msg_emoji_like(ctx, writer.clone(), event.message_id(), STEERED_EMOJI, true).await
                {
                    debug!(target: LOG_TARGET, "「已追加」表情贴不上：{error}");
                }
                return;
            }
            busy_notice(ctx, writer, &event, mgr, name, is_priv_ctx, &uid).await;
            return;
        }
    }

    if !regen && prompt.is_empty() && imgs.is_empty() {
        reply_text(ctx, writer, &event, format!("💡 {name} 后面接上要说的话，或引用一条消息再叫它")).await;
        return;
    }
    let (hist, gen_id) = if temp_mode {
        (prepare_history(&[], prompt, &imgs, regen, ""), 0)
    } else {
        let mut config = mgr.config.write().await;
        let Some(current) = config.agents.iter_mut().find(|a| a.name == name) else {
            return;
        };
        let reservation = mgr.generating.write().await.begin(name, is_priv_ctx, &uid);
        let Some(id) = reservation else {
            drop(config);
            busy_notice(ctx, writer, &event, mgr, name, is_priv_ctx, &uid).await;
            return;
        };
        let hist = prepare_history(current.history(is_priv_ctx, &uid), prompt, &imgs, regen, &from);
        *current.history_mut(is_priv_ctx, &uid) = hist.clone();
        if let Err(error) = mgr.save(&mut config) {
            mgr.generating
                .write()
                .await
                .set_generating(name, is_priv_ctx, &uid, false);
            reply_text(
                ctx,
                writer,
                &event,
                format!("保存失败，已恢复原配置。请检查磁盘空间与目录权限后重试：{error}"),
            )
            .await;
            return;
        }
        (hist, id)
    };

    let base = super::utils::openai_api_base(&api_base);

    mark_working(ctx, writer, &event, true).await;

    let started = std::time::Instant::now();
    // 内置智能体房间这一轮的办事台：谁在问、在哪儿、够不够得着机器人本体（见 `agent::Seat`）。
    let seat = use_agent.then(|| super::agent::Seat {
        room: name.to_string(),
        asker: asker.clone(),
        ctx: ctx.clone(),
        writer: writer.clone(),
        group: event.group_id().map(str::to_string),
        private: is_priv_ctx,
        origin: super::agent::Origin::Asked,
        public: false,
    });
    // 这一轮进行当中，发起人又发来的话排在这个信箱里（临时对话不占会话，不开）。
    let steering = (use_agent && !temp_mode).then(|| mgr.open_steering(name, is_priv_ctx, &uid));

    // 出结果与到总预算在同一个任务里赛跑。用 select 而不是另起任务，是因为发消息
    // 要用借来的 ctx/event，搬进 spawn 就得整套克隆一遍。把工作 Future 放进独立
    // 作用域，超时/停止后立即 drop 并终止工具派生的子进程。
    //
    // 中途不发任何「还在处理」提示：等待本身是隐式的，一条进度播报换不来更快的
    // 回复，只会在群里插进一段与上下文无关的噪音。
    // 图像 / 音乐 / 视频房间各自走专用接口，其余房间继续走聊天补全 / 内置 agent。
    // 内置 agent 房间的模型是交给执行层解析的，不能拿它去撞中转站的这些模型关键字。
    let draw = !use_agent && super::images::is_images_model(&chat_model, &oai.image_models);
    let music = !use_agent && super::music::is_music_model(&chat_model, &oai.music_models);
    let video = !use_agent && super::video::is_video_model(&chat_model, &oai.video_models);
    // 视频与一首歌都是异步任务，普通的 5 分钟预算不够它们出成品。
    let media_room = music || video;

    let mut outcome = {
        let work = async {
            if music {
                super::music::generate_reply(&api_base, &api_key, &agent, &hist, &oai).await
            } else if video {
                super::video::generate_reply(&api_base, &api_key, &agent, &hist, &oai).await
            } else if draw {
                let mut draw_agent = agent.clone();
                draw_agent.model = chat_model.clone();
                super::images::generate_reply(&base, &api_key, &draw_agent, &hist).await
            } else {
                // 在群里说话的房间接上群聊能力层：群资料、翻旧账、发消息、画图写歌
                // 那一套从此也在房间里可用。私聊里的房间没有这一份。
                let chat = event.group_id().map(|group| {
                    let trigger = super::chat::trigger::Trigger::from_event(&event);
                    super::agent::ChatContext {
                        ctx: ctx.clone(),
                        writer: writer.clone(),
                        group: group.to_string(),
                        config: oai.chat.clone(),
                        tools: Some(room_tools(&oai.chat)),
                        authority: super::chat::Authority::of(asker.owner, &trigger.role),
                        trigger: Some(trigger),
                    }
                });
                respond(
                    &api_base,
                    &api_key,
                    &agent,
                    &chat_model,
                    &hist,
                    &oai,
                    mgr.path.parent().unwrap_or(&mgr.path),
                    seat,
                    steering.as_deref(),
                    chat,
                )
                .await
            }
        };
        let mut work = std::pin::pin!(work);
        let mut budget = std::pin::pin!(tokio::time::sleep(if media_room {
            oai.media_timeout()
        } else {
            oai.request_timeout()
        }));

        let mut cancellation = tokio::time::interval(std::time::Duration::from_millis(200));
        loop {
            tokio::select! {
                result = &mut work => break Some(result.map(Some)),
                _ = &mut budget => break None,
                _ = cancellation.tick(), if !temp_mode => {
                    let config = mgr.config.read().await;
                    if !config.agents.iter().any(|a| a.name == name)
                        || !mgr.generating.read().await.is_current(name, is_priv_ctx, &uid, gen_id) {
                        break Some(Ok(None));
                    }
                }
            }
        }
    };

    // 撤掉追加信箱：此后的话照常回「正在生成中」；没来得及处理的话在下面记进历史或告诉用户。
    let leftover = mgr.close_steering(name, is_priv_ctx, &uid);

    // 在保存历史前再次验证请求身份；旧请求不能清掉新请求的占用状态。
    if !temp_mode {
        let mut config = mgr.config.write().await;
        let mut generating = mgr.generating.write().await;
        if generating.is_current(name, is_priv_ctx, &uid, gen_id) {
            generating.set_generating(name, is_priv_ctx, &uid, false);
            if let Some(Ok(Some(reply))) = &outcome {
                if let Some(agent) = config.agents.iter_mut().find(|a| a.name == name) {
                    let history = agent.history_mut(is_priv_ctx, &uid);
                    // 并进这一轮的追加话先记，再是回复：历史里的先后就是对话实际的先后。
                    for extra in &reply.steered {
                        history.push(ChatMessage::new("user", extra, vec![]).with_from(&from));
                    }
                    history.push(
                        ChatMessage::new("assistant", &reply.text, vec![])
                            .with_work(super::agent::run::work_digest(&reply.trace, reply.trace_overflow)),
                    );
                    // 赶在收口前后脚到的话没人回应，记成用户消息，下一轮自然接上。
                    for extra in &leftover {
                        history.push(ChatMessage::new("user", extra, vec![]).with_from(&from));
                    }
                    if let Err(error) = mgr.save(&mut config) {
                        reply_text(
                            ctx,
                            writer,
                            &event,
                            format!(
                                "保存失败，已恢复原配置。请检查磁盘空间与目录权限后重试：{error}"
                            ),
                        )
                        .await;
                        return;
                    }
                } else {
                    outcome = Some(Ok(None));
                }
            }
        } else {
            outcome = Some(Ok(None));
        }
    }

    if !leftover.is_empty() && !matches!(outcome, Some(Ok(Some(_)))) {
        reply_text(
            ctx,
            writer,
            &event,
            format!("这一轮没能做完，你中途追加的 {} 句话也没处理上，请重新说一遍", leftover.len()),
        )
        .await;
    }

    match outcome {
        None => {
            let budget = if media_room {
                oai.media_timeout()
            } else {
                oai.request_timeout()
            };
            reply_text(
                ctx,
                writer,
                &event,
                format!(
                    "⏳ 请求超时：模型响应超过 {} 秒，已强制停止",
                    budget.as_secs()
                ),
            )
            .await;
        }
        Some(Err(error)) => {
            reply_text(ctx, writer, &event, format!("❌ 对话失败：{error:#}")).await;
        }
        Some(Ok(None)) => {}
        Some(Ok(Some(reply_data))) => {
            let content = reply_data.text;

            let msg_index = if temp_mode {
                0
            } else {
                let c = mgr.config.read().await;
                c.agents
                    .iter()
                    .find(|a| a.name == name)
                    .map_or(0, |a| a.history(is_priv_ctx, &uid).len())
            };

            let image_urls = extract_image_urls(&content);
            let header = if temp_mode {
                format!("{} (临时会话)", agent.name)
            } else {
                format!(
                    "{} #{}回复{}",
                    agent.name,
                    msg_index,
                    if cmd.private_reply { " (私有)" } else { "" }
                )
            };

            // 图片的去向有两种。
            //
            // **内嵌**（智能体房间、回复要渲染成卡片、且正文里确实写了图）：图按位置画进卡片，
            // 取图已经在 agent 层做完（见 `agent::figures`）。卡片没发出去才把图另发。
            //
            // **另发**（其余所有情形）：正文里只留文字，图作为真图单独发出——卡片只放行
            // `data:` 图片，远程直链在卡里是个空框；把图再塞进卡片或附一串链接只是重复。
            // 画图房间的正文是「加粗的提示词 + 图片链接」，那是留给历史记录与垫图回放
            // 的，不是给人看的：只发成品图，引用着用户那句话。
            let inline = agent.uses_agent()
                && !draw
                && !cmd.text_mode
                && oai.image_enabled()
                && !super::agent::figures::sources(&content).is_empty();
            let prose = if inline {
                content.clone()
            } else if draw && !image_urls.is_empty() {
                String::new()
            } else {
                strip_markdown_images(&content)
            };

            let mut spoke = false;
            let mut carded = false;
            if !prose.trim().is_empty() || !reply_data.sources.is_empty() {
                // 一两句话没必要走一次浏览器截图：文本更快，也方便直接复制。
                let plain = cmd.text_mode
                    || reply_data.plain
                    || (reply_data.sources.is_empty()
                        && is_plain_enough(&prose, oai.plain_text_max_chars()));
                let footer = (oai.show_trace_footer() && !plain).then(|| super::render::Footer {
                    meta: format!(
                        "{} · {}",
                        reply_data.model.as_deref().unwrap_or(&agent.model),
                        super::utils::format_elapsed(started)
                    ),
                    trace: reply_data.trace.clone(),
                    trace_overflow: reply_data.trace_overflow,
                });

                carded = reply_card(
                    ctx,
                    writer,
                    &event,
                    &prose,
                    plain,
                    &header,
                    &reply_data.sources,
                    &reply_data.figures,
                    footer,
                )
                .await;
                spoke = true;
            }

            // 正文没说话时由第一张图引住用户那句话，免得群里分不清是给谁画的。
            let loose = match (inline, carded) {
                (true, true) => Vec::new(),
                (true, false) => loose_images(&content, &reply_data.figures),
                (false, _) => image_urls,
            };
            let failed = send_images(ctx, writer, &event, &loose, !spoke).await;
            if !failed.is_empty() {
                reply_text(ctx, writer, &event, undelivered_notice(&failed)).await;
            }

            for url in extract_video_urls(&content) {
                if let Err(error) = send_msg(
                    ctx,
                    writer.clone(),
                    event.group_id(),
                    Some(event.user_id()),
                    Message::new().video(&url),
                )
                .await
                {
                    warn!(target: LOG_TARGET, "视频 {url} 发送失败：{error}");
                }
            }

            // 音乐、视频这类成品由插件附在正文外，按段发出去。一条媒体消息里的片段
            // 一起发：失败只影响这一条，已经发出去的封面/文件不受牵连。
            //
            // 播放用的那条（语音 / 视频气泡）由 `media_messages` 排在同文件前面：
            // 有些群不让普通成员发群文件，那条腿失败时不该把能播的那条一起拖住。
            // 这里**不留引用**：正文卡片已经把用户那句话引住了；顺媒体也不接受引用
            // （语音、视频、群文件必须单独成条，带了别的段落实现端就得替我们拆开）。
            for message in reply_data.media {
                let mut msg = Message::new();
                for segment in message.segments {
                    msg = match segment {
                        Media::Image { url } => msg.image(url),
                        Media::Audio { url } => msg.record(url),
                        Media::Video { url } => msg.video(url),
                        Media::File { url, name } => msg.file(url, Some(name)),
                    };
                }
                if let Err(error) = send_msg(
                    ctx,
                    writer.clone(),
                    event.group_id(),
                    Some(event.user_id()),
                    msg,
                )
                .await
                {
                    warn!(target: LOG_TARGET, "媒体消息发送失败: {error}");
                }
            }
        }
    }

    mark_working(ctx, writer, &event, false).await;
}

/// 回执里那句「会打到哪些后端」，优先的在前面。
///
/// 只在这一轮真的能联网时才显示：链空着说明没配出任何可用后端，这时把话说破，
/// 省得管理员以为开关打开了就一定搜得到。
fn search_backends(config: &super::search::SearchConfig) -> String {
    let chain = config.chain();
    if chain.is_empty() {
        return "（没有可用的后端，去 [oai.search.backends] 配密钥）".to_string();
    }
    chain.join(" → ")
}

/// 房间里显示的联网状态：区分「这间房自己定的」和「跟着全局走」。
fn search_state(agent: &Agent, global: bool) -> String {
    if !agent.uses_agent() {
        return "不适用（普通房间）".to_string();
    }
    // 两件事分开说：**现在开没开**（有效值）与**这个值是谁定的**。
    // 只写「跟随全局」的话，读者看不出当下到底是开还是关——而那是这一行最要紧的
    // 信息，不能为了让词表短一档就丢掉。
    match (agent.web_search(global), agent.search) {
        (true, Some(true)) => "已打开（本房间设置）".to_string(),
        (true, _) => "已打开（跟随全局）".to_string(),
        (false, Some(false)) => "已关闭（本房间设置）".to_string(),
        (false, _) => "已关闭（跟随全局）".to_string(),
    }
}

/// 房间列表和回执里显示的模型：内置 agent 房间前面挂上引擎，一眼能看出这间屋子
/// 谁在跑；设了思考强度就一并标出。
///
/// 内置智能体房间没写模型时，把实际用的默认模型（`default`）写出来，
/// 不然「内置 · 默认模型」说了等于没说。
pub(super) fn room_model_label_with(agent: &Agent, default: Option<&str>) -> String {
    let base = if !agent.uses_agent() {
        agent.model.clone()
    } else if super::agent::uses_default_model(&agent.model) {
        match default {
            Some(model) => format!("内置 · {model}（默认）"),
            None => "内置 · 默认模型".to_string(),
        }
    } else {
        format!("内置 · {}", agent.model)
    };
    match agent.effective_thinking() {
        Some(level) => format!("{base} · 思考:{level}"),
        None => base,
    }
}

/// 一次成功回复的产物。
pub(super) struct Reply {
    pub(super) text: String,
    pub(super) sources: Vec<super::types::Source>,
    pub(super) trace: Vec<super::types::TraceStep>,
    /// 超出页脚保留上限、只计数的调用次数。
    pub(super) trace_overflow: usize,
    pub(super) model: Option<String>,
    /// 这一轮只发纯文本，不渲染卡片。
    ///
    /// 音乐房间的正文是歌名、标签加几行歌词，本来就该能选中复制；渲染成卡片图反而
    /// 只剩一张图。视频房间的正文只有一行，走 `is_plain_enough` 也是纯文本，这里写
    /// 死只是省掉那一次判断。
    pub(super) plain: bool,
    /// 正文之外要单独发出去的媒体（音乐、视频、封面）。
    ///
    /// 图片走正文里的 markdown 链接（`extract_image_urls`）、视频走
    /// `[download video](url)`，那两条是模型能自己写的；音乐这类「房间产出的成品」
    /// 模型根本碰不到，只能由插件在正文外附上，于是有了这个字段。
    pub(super) media: Vec<MediaMessage>,
    /// 正文里内嵌的图（智能体房间才有），卡片按位置画进去。
    pub(super) figures: super::agent::figures::Figures,
    /// 这一轮进行当中并进来的追加话（智能体房间才有），历史里要记在回复之前。
    pub(super) steered: Vec<String>,
}

/// 房间在等一句补充时回的那条。
///
/// 这不是失败——用户只是还没说要什么，`❌ 对话失败：…` 会把一句引导说成故障。
/// 走 💡 并保持纯文本。
pub(super) fn guidance(text: impl Into<String>) -> Reply {
    Reply {
        text: text.into(),
        sources: Vec::new(),
        trace: Vec::new(),
        trace_overflow: 0,
        model: None,
        plain: true,
        media: Vec::new(),
        figures: super::agent::figures::Figures::default(),
        steered: Vec::new(),
    }
}

/// 一条媒体消息里的单个片段。
pub(super) enum Media {
    Image { url: String },
    Audio { url: String },
    Video { url: String },
    File { url: String, name: String },
}

/// 一条要单独发出去的媒体消息（封面 + 音频这种拼法各段一起交给实现端）。
///
/// 顺媒体（语音、视频、群文件）在 QQ 里必须单独成条，与图片、引用同条时实现端会
/// 按原顺序拆开发（见 satori-qq 的 `docs/SATORI_SUPPORT.md`）——所以「封面 + 语音」
/// 到了群里是两条：封面那张图，再是语音。
pub(super) struct MediaMessage {
    pub(super) segments: Vec<Media>,
}

/// 房间里这一轮的工具白名单。
///
/// 本机那几件按 `[oai.chat] tools` 来（留空就是全给）；群聊那一套按额度收敛，
/// 与搭话用同一份清单（[`chat::tool_names`]），于是「模型看得到的工具」与
/// 「它真按得动的按钮」在两边都永远对得上。
pub(super) fn room_tools(config: &super::chat::ChatConfig) -> String {
    let mut names: Vec<String> = if config.tools.trim().is_empty() {
        super::agent::tools::local_names()
            .into_iter()
            .map(str::to_string)
            .collect()
    } else {
        config
            .tools
            .split(',')
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect()
    };
    // 管理动作写在 `satori_action` 里，按群授权（`[oai.chat] management_groups`），
    // 白名单这一层不必再分；这里补的是「按额度该不该出现」。
    names.extend(
        super::chat::tool_names(config)
            .into_iter()
            .map(str::to_string),
    );
    // 出网工具写不写都行：这一轮没开联网时 `definitions` 会把它们滤掉（白名单是
    // 过滤器）。写上是因为房间确实该有它们——漏写就等于把联网从房间里摘掉了。
    names.extend(["web_search", "web_fetch"].map(str::to_string));
    // 办事台的三件（插件管控、提醒、手记）同理：这一轮有没有办事台、是不是号主，由 `definitions` 滤。
    names.extend(
        [
            super::agent::control::NAME,
            super::agent::schedule::NAME,
            super::agent::notes::NAME,
            super::agent::share::NAME,
        ]
        .map(str::to_string),
    );
    names.join(",")
}

/// 房间里这一轮的工具块：名字清单、出网工具在不在，以及它的体量。
///
/// 工具定义跟着每一次请求走（一轮最多 24 次），它涨一点就是每一次请求都涨。
/// 2026-09-17 量的：私聊 1.9k 字；群里 15.4k 字；加联网 16.2k 字。其中
/// `satori_action` 一份 6.9k——19 个动作变体的严格结构，换的是「一次填对参数」。
/// 2026-10-17 联网两件工具从 0.8k 涨到 1.7k（站点/日期/语种/正文/翻页的参数与用法），
/// 加联网约 17.1k 字；换来的是一次搜索就能带准条件，少绕几轮。
/// 内置 agent 房间走带工具的多轮循环，普通房间走单轮 Chat Completions。
///
/// `api_base` / `api_key` 已按房间模型的 `供应商/` 前缀解析好；
/// `chat_model` 是真正要发给接口的模型 id（已剥掉前缀）。
#[allow(clippy::too_many_arguments)]
pub(super) async fn respond(
    api_base: &str,
    api_key: &str,
    agent: &Agent,
    chat_model: &str,
    hist: &[ChatMessage],
    oai: &super::OaiConfig,
    data_dir: &std::path::Path,
    seat: Option<super::agent::Seat>,
    steering: Option<&super::agent::Steering>,
    // 群聊那一路的接口与额度；私聊里的房间没有这一份（没有群也就没有群动作）。
    chat: Option<super::agent::ChatContext>,
) -> anyhow::Result<Reply> {
    let thinking = agent.effective_thinking();
    if agent.uses_agent() {
        // 联网开关是房间自己的选择优先，没写过才跟 `[oai.search].enabled`。
        // 工具只是挂上去，搜不搜由模型按需决定，没有任何一轮是强制的。
        let search = super::search::SearchConfig {
            enabled: agent.web_search(oai.search.enabled),
            ..oai.search_config()
        };
        let result = super::agent::conversation(
            api_base,
            api_key,
            data_dir,
            &agent.system_prompt,
            chat_model,
            thinking.as_deref(),
            oai.request_stall(),
            hist,
            seat,
            steering,
            &search,
            chat,
            oai.request_timeout(),
        )
        .await?;
        return Ok(Reply {
            text: result.text,
            sources: result.sources,
            trace: result.trace,
            trace_overflow: result.trace_overflow,
            // 页脚显示房间写的那份（`供应商/模型`）；房间没写模型时显示解析出来的默认模型。
            model: Some(if super::agent::uses_default_model(&agent.model) {
                chat_model.to_string()
            } else {
                agent.model.clone()
            }),
            plain: false,
            media: Vec::new(),
            figures: result.figures,
            steered: result.steered,
        });
    }
    let msgs = build_chat_messages(agent, hist).await;
    Ok(Reply {
        text: super::llm::complete(api_base, api_key, chat_model, msgs, thinking.as_deref())
            .await?,
        sources: Vec::new(),
        trace: Vec::new(),
        trace_overflow: 0,
        model: Some(chat_model.to_string()),
        plain: false,
        media: Vec::new(),
        figures: super::agent::figures::Figures::default(),
        steered: Vec::new(),
    })
}

/// 把房间历史转成 Chat Completions 消息。
async fn build_chat_messages(agent: &Agent, hist: &[ChatMessage]) -> Vec<LlmMessage> {
    let mut msgs: Vec<LlmMessage> = Vec::new();

    // 少数图像模型不接受 system 角色，只能把提示词并进首条用户消息。
    let model_lower = agent.model.to_lowercase();
    let force_user_role_for_system = [
        "nano-banana",
        "gemini-2.5-flash-image",
        "gemini-3-pro-image",
    ]
    .iter()
    .any(|kw| model_lower.contains(kw));

    let mut pending_sys_prompt =
        (!agent.system_prompt.is_empty()).then(|| agent.system_prompt.clone());

    if !force_user_role_for_system && let Some(sp) = pending_sys_prompt.take() {
        msgs.push(LlmMessage::System { content: sp });
    }

    let re = Regex::new(r"!\[.*?\]\((data:image/[^\s\)]+)\)").unwrap();
    for m in hist {
        if m.role == "user" {
            let mut parts = Vec::new();

            if let Some(sp) = pending_sys_prompt.take() {
                parts.push(UserContent::Text(Text::new(sp)));
            }

            if !m.content.is_empty() {
                parts.push(UserContent::Text(Text::new(m.content.clone())));
            }
            for data_url in resolve_images(&m.images).await {
                parts.push(image_content(data_url));
            }
            if parts.is_empty() {
                continue;
            }
            msgs.push(LlmMessage::User { content: parts });
        } else if m.role == "assistant" {
            let clean_content = re.replace_all(&m.content, "[Image Created]").to_string();
            // 空内容的 assistant 消息会被请求校验拒掉（provider 侧同样报错），跳过。
            if !clean_content.trim().is_empty() {
                msgs.push(LlmMessage::assistant(clean_content));
            }
            let gen_imgs = extract_image_urls(&m.content);
            if !gen_imgs.is_empty() {
                msgs.push(LlmMessage::User {
                    content: gen_imgs.into_iter().map(image_content).collect(),
                });
            }
        }
    }

    if let Some(sp) = pending_sys_prompt {
        msgs.push(LlmMessage::User {
            content: vec![UserContent::Text(Text::new(sp))],
        });
    }
    msgs
}

/// 图片地址是多模态模型可内联的 data URL 时原样交给 provider。
fn image_content(url: String) -> UserContent {
    UserContent::Image(Image {
        data: DocumentSourceKind::Url(url),
        media_type: None,
        detail: None,
        native: None,
    })
}

/// 并发解析一条消息里的全部图片地址。
///
/// 逐张串行下载会让多图历史在每一轮都白等一次网络往返；缓存则让同一张图在整个
/// 会话里只下载一次。
async fn resolve_images(urls: &[String]) -> Vec<String> {
    futures_util::future::join_all(urls.iter().map(|url| to_data_url(url))).await
}

/// 短回复能否直接以纯文本发送。
///
/// 只要出现标题、列表、表格、代码块或链接，排版就有信息量，仍旧渲染卡片。
pub(super) fn is_plain_enough(text: &str, max_chars: usize) -> bool {
    if max_chars == 0 || text.chars().count() > max_chars {
        return false;
    }
    if text.contains("](") || text.contains("```") || text.contains('|') {
        return false;
    }
    !text.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with('#')
            || line.starts_with("- ")
            || line.starts_with("* ")
            || line.starts_with("> ")
            || line.split_once(". ").is_some_and(|(head, _)| {
                !head.is_empty() && head.chars().all(|c| c.is_ascii_digit())
            })
    })
}

pub async fn execute(
    cmd: Command,
    prompt: String,
    imgs: Vec<String>,
    ctx: &Context,
    writer: &LockedWriter,
    mgr: &Arc<Manager>,
) {
    let Some(msg_event) = ctx.as_message() else { return };
    let name = &cmd.agent;
    let uid = msg_event.user_id().to_string();

    if matches!(
        cmd.action,
        Action::Delete
            | Action::DeleteAt(_)
            | Action::ClearHistory(_)
            | Action::ClearAllPublic
            | Action::ClearEverything
    ) {
        let config = mgr.config.read().await;
        let fingerprint = format!(
            "{:x}",
            md5::compute(serde_json::to_vec(&*config).unwrap_or_default())
        );
        let key = format!("{:?}:{}:{:?}", msg_event.group_id(), uid, cmd);
        let count = if name.is_empty() {
            config.agents.len()
        } else {
            usize::from(config.agents.iter().any(|agent| agent.name == *name))
        };
        if !confirm_destructive(key, fingerprint) {
            let scope = match cmd.action {
                Action::Delete => "删除智能体及其全部历史",
                Action::ClearEverything => "清空所有智能体的公开和私有历史（保留智能体）",
                Action::ClearAllPublic => "清空所有智能体的公开历史（保留智能体）",
                Action::ClearHistory(Scope::Private) | Action::DeleteAt(Scope::Private) => {
                    "删除你在该智能体中的私有历史"
                }
                _ => "删除该智能体的公开历史",
            };
            reply_text(ctx, writer, &msg_event, format!("即将{scope}，涉及 {count} 个智能体。此操作不可撤销。请在 60 秒内再次发送同一条指令确认；不再发送即可取消。数据有变化时需重新确认。")).await;
            return;
        }
    }

    match cmd.action {
        Action::Connect(request) => {
            super::setup::connect(request, ctx, writer, mgr).await;
        }
        Action::Chat => {
            chat(name, &prompt, imgs, false, &cmd, ctx, writer, mgr).await;
        }
        Action::Regenerate => {
            chat(name, &cmd.args, imgs, true, &cmd, ctx, writer, mgr).await;
        }
        Action::Stop => {
            let is_priv_ctx = cmd.private_reply;
            {
                mgr.generating
                    .write()
                    .await
                    .set_generating(name, is_priv_ctx, &uid, false);
            }
            let mut c = mgr.config.write().await;
            if let Some(a) = c.agents.iter_mut().find(|a| a.name == *name) {
                a.generation_id += 1;
                if let Err(error) = mgr.save(&mut c) {
                    reply_text(
                        ctx,
                        writer,
                        &msg_event,
                        format!("保存失败，已恢复原配置。请检查磁盘空间与目录权限后重试：{error}"),
                    )
                    .await;
                    return;
                }
                reply_text(ctx, writer, &msg_event, "已停止").await;
            } else {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("❌ 智能体 {name} 不存在"),
                )
                .await;
            }
        }
        Action::Copy => {
            if cmd.args.is_empty() {
                reply_text(ctx, writer, &msg_event, "❌ 请指定新名称：智能体~#新名称").await;
                return;
            }
            if !super::parser::valid_agent_name(&cmd.args) {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    "❌ 名称限制：最多7字且不能包含指令符号",
                )
                .await;
                return;
            }
            let mut c = mgr.config.write().await;
            if c.agents.iter().any(|a| a.name == cmd.args) {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("❌ 智能体 {} 已存在", cmd.args),
                )
                .await;
                return;
            }
            if let Some(src) = c.agents.iter().find(|a| a.name == *name).cloned() {
                let mut new_agent = Agent::new(
                    &cmd.args,
                    &src.model,
                    &src.system_prompt,
                    &format!("复制自 {name}"),
                );
                // 副本要连引擎一起带走：新名字不再能推断出「这是一间 Agent 房间」。
                new_agent.set_engine(&src.engine, &src.model);
                new_agent.description = src.description.clone();
                c.agents.push(new_agent);
                if let Err(error) = mgr.save(&mut c) {
                    reply_text(
                        ctx,
                        writer,
                        &msg_event,
                        format!("保存失败，已恢复原配置。请检查磁盘空间与目录权限后重试：{error}"),
                    )
                    .await;
                    return;
                }
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("已复制 {} → {}", name, cmd.args),
                )
                .await;
            } else {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("❌ 智能体 {name} 不存在"),
                )
                .await;
            }
        }
        Action::Rename => {
            if cmd.args.is_empty() {
                reply_text(ctx, writer, &msg_event, "❌ 请指定新名称：智能体~=新名称").await;
                return;
            }
            if !super::parser::valid_agent_name(&cmd.args) {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    "❌ 名称限制：最多7字且不能包含指令符号",
                )
                .await;
                return;
            }
            let mut c = mgr.config.write().await;
            if c.agents.iter().any(|a| a.name == cmd.args) {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("❌ 智能体 {} 已存在", cmd.args),
                )
                .await;
                return;
            }
            let idx_opt = c.agents.iter().position(|a| a.name == *name);
            if let Some(idx) = idx_opt {
                mgr.generating.write().await.cancel_room(name);
                c.agents[idx].name = cmd.args.clone();
                if let Err(error) = mgr.save(&mut c) {
                    reply_text(
                        ctx,
                        writer,
                        &msg_event,
                        format!("保存失败，已恢复原配置。请检查磁盘空间与目录权限后重试：{error}"),
                    )
                    .await;
                    return;
                }
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("已重命名 {} → {}", name, cmd.args),
                )
                .await;
            } else {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("❌ 智能体 {name} 不存在"),
                )
                .await;
            }
        }
        Action::SetDesc => {
            if cmd.args.is_empty() {
                reply_text(ctx, writer, &msg_event, "❌ 请提供描述：智能体:描述内容").await;
                return;
            }
            let mut c = mgr.config.write().await;
            if let Some(a) = c.agents.iter_mut().find(|a| a.name == *name) {
                a.description = cmd.args.clone();
                if let Err(error) = mgr.save(&mut c) {
                    reply_text(
                        ctx,
                        writer,
                        &msg_event,
                        format!("保存失败，已恢复原配置。请检查磁盘空间与目录权限后重试：{error}"),
                    )
                    .await;
                    return;
                }
                reply_text(ctx, writer, &msg_event, format!("{name} 描述已更新")).await;
            } else {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("❌ 智能体 {name} 不存在"),
                )
                .await;
            }
        }
        // `%` 换模型；引擎只在明写 `agent` / `chat` 时换（见 [`resolve_room_model`]）。
        Action::SetModel => {
            if cmd.args.is_empty() {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    "❌ 请指定模型：`房间%模型名`（`/%` 看能用的）；转成内置智能体用 `房间%agent`，转成普通对话用 `房间%chat`",
                )
                .await;
                return;
            }
            let oai = crate::plugins::get_config_or_default::<super::OaiConfig>(ctx);
            let mut c = mgr.config.write().await;
            let models = Manager::all_models(&c);
            // 模型串支持 `:强度` 后缀；先摘掉它，剩下的再判引擎关键字 / 供应商前缀。
            let (spec, thinking) = super::utils::split_thinking(&cmd.args);
            let Some(a) = c.agents.iter_mut().find(|a| a.name == *name) else {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("❌ 智能体 {name} 不存在"),
                )
                .await;
                return;
            };
            let was_agent = a.uses_agent();
            let (engine, mut model) = resolve_room_model(&spec, was_agent, &models, mgr, &oai);
            // `房间%chat` 没写模型：沿用房间现在实际在用的那个，普通房间可没有「默认模型」。
            if engine == super::types::ENGINE_CHAT && model.is_empty() {
                model = if super::agent::uses_default_model(&a.model) {
                    oai.agent_default_model().unwrap_or_default().to_string()
                } else {
                    a.model.clone()
                };
            }
            if engine == super::types::ENGINE_CHAT && model.is_empty() {
                reply_text(ctx, writer, &msg_event, "❌ 普通对话房间要写明模型：`房间%chat 供应商/模型`").await;
                return;
            }
            let old = room_model_label_with(a, oai.agent_default_model());
            a.set_engine(engine, &model);
            // 没写 `:强度` 就保留房间原来的档位，只换模型时不必重报思考强度。
            if let Some(level) = thinking {
                a.thinking = level;
            }
            let new = room_model_label_with(a, oai.agent_default_model());
            let now_agent = a.uses_agent();
            if let Err(error) = mgr.save(&mut c) {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("保存失败，已恢复原配置。请检查磁盘空间与目录权限后重试：{error}"),
                )
                .await;
                return;
            }
            let mut text = format!("{name} 模型：{old} → {new}");
            match (was_agent, now_agent) {
                (true, false) if parse_chat_spec(&spec).is_none() => {
                    text.push_str("\n这是画图/音乐/视频模型，只能在普通房间里用，已转成普通房间（不再调工具）。转回：`房间%agent`");
                }
                (true, false) => text.push_str("\n已转成普通对话房间：只聊天，不调工具。"),
                (false, true) => text.push_str("\n已转成内置智能体：会自己用终端、联网和群聊工具。"),
                _ => {}
            }
            reply_text(ctx, writer, &msg_event, text).await;
        }
        // `房间?` 管这间房要不要联网：不带词就换一边，`开`/`关` 明确指定，
        // `默认` 交回 [oai.search].enabled。房间自己的选择优先于全局。
        Action::SetSearch => {
            let oai = crate::plugins::get_config_or_default::<super::OaiConfig>(ctx);
            let global = oai.search.enabled;
            let mut c = mgr.config.write().await;
            let Some(a) = c.agents.iter_mut().find(|a| a.name == *name) else {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("❌ 智能体 {name} 不存在"),
                )
                .await;
                return;
            };
            if !a.uses_agent() {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!(
                        "❌ {name} 是普通房间，联网搜索只挂在内置智能体上；先 `{name}%agent` 把它转过来"
                    ),
                )
                .await;
                return;
            }
            let Some(choice) = super::parser::search_choice(&cmd.args, a.web_search(global)) else {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    "❌ 不认识这个写法\n`房间?` 换一边，`房间?开` / `房间?关` 明确开关，`房间?默认` 跟随全局",
                )
                .await;
                return;
            };
            a.search = choice;
            let now = a.web_search(global);
            if let Err(error) = mgr.save(&mut c) {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("保存失败，已恢复原配置。请检查磁盘空间与目录权限后重试：{error}"),
                )
                .await;
                return;
            }
            let state = match choice {
                Some(true) => "已打开（按需触发）",
                Some(false) => "已关闭",
                None if now => "已打开（跟随全局）",
                None => "已关闭（跟随全局）",
            };
            // 打开时顺带说一句会打到哪个后端，省得去翻配置确认密钥有没有生效。
            let backends = if now {
                format!("　后端 {}", search_backends(&oai.search_config()))
            } else {
                String::new()
            };
            reply_text(
                ctx,
                writer,
                &msg_event,
                format!("{name} 联网搜索：{state}{backends}"),
            )
            .await;
        }
        Action::SetPrompt => {
            let mut c = mgr.config.write().await;
            if let Some(a) = c.agents.iter_mut().find(|a| a.name == *name) {
                a.system_prompt = cmd.args.clone();
                if let Err(error) = mgr.save(&mut c) {
                    reply_text(
                        ctx,
                        writer,
                        &msg_event,
                        format!("保存失败，已恢复原配置。请检查磁盘空间与目录权限后重试：{error}"),
                    )
                    .await;
                    return;
                }
                if cmd.args.is_empty() {
                    reply_text(ctx, writer, &msg_event, format!("{name} 提示词已清空")).await;
                } else {
                    reply_text(ctx, writer, &msg_event, format!("{name} 提示词已更新")).await;
                }
            } else {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("❌ 智能体 {name} 不存在"),
                )
                .await;
            }
        }
        Action::ViewPrompt => {
            let c = mgr.config.read().await;
            if let Some(a) = c.agents.iter().find(|a| a.name == *name) {
                if cmd.text_mode {
                    reply_text(ctx, writer, &msg_event, &a.system_prompt).await;
                    return;
                }
                let prompt_display = if a.system_prompt.is_empty() {
                    "(空)".to_string()
                } else {
                    escape_markdown_special(&a.system_prompt)
                };
                let oai = crate::plugins::get_config_or_default::<super::OaiConfig>(ctx);
                let search_default = oai.search.enabled;
                let content = format!(
                    "**模型**: `{}`\n\n**联网搜索**: {}\n\n**提示词**:\n```\n{}\n```",
                    room_model_label_with(a, oai.agent_default_model()),
                    search_state(a, search_default),
                    prompt_display
                );
                reply(
                    ctx,
                    writer,
                    &msg_event,
                    &content,
                    cmd.text_mode,
                    &format!("{} 系统提示词", a.name),
                )
                .await;
            } else {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("❌ 智能体 {name} 不存在"),
                )
                .await;
            }
        }
        Action::List => {
            let oai = crate::plugins::get_config_or_default::<super::OaiConfig>(ctx);
            let search_default = oai.search.enabled;
            let c = mgr.config.read().await;
            if c.agents.is_empty() {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    "📭 暂无智能体，使用 ##名称 模型 提示词 创建",
                )
                .await;
                return;
            }
            let listing = super::lists::Listing::Rooms(super::lists::Rooms::new(
                &c.agents,
                search_default,
                oai.agent_default_model(),
            ));
            reply_list(
                ctx,
                writer,
                &msg_event,
                &listing,
                cmd.text_mode,
                &format!("智能体列表 (共{}个)", c.agents.len()),
            )
            .await;
        }
        Action::Delete => {
            let mut c = mgr.config.write().await;
            if let Some(idx) = c.agents.iter().position(|a| a.name == *name) {
                mgr.generating.write().await.cancel_room(name);
                c.agents.remove(idx);
                if let Err(error) = mgr.save(&mut c) {
                    reply_text(
                        ctx,
                        writer,
                        &msg_event,
                        format!("保存失败，已恢复原配置。请检查磁盘空间与目录权限后重试：{error}"),
                    )
                    .await;
                    return;
                }
                reply_text(ctx, writer, &msg_event, format!("已删除 {name}")).await;
            } else {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("❌ 智能体 {name} 不存在"),
                )
                .await;
            }
        }
        Action::ListModels => {
            // 每次查看都强制刷新，确保能获取最新模型；刷新不慢，不先发提示
            // 尝试获取，如果失败则仅提示警告，后续继续尝试展示缓存
            let oai = crate::plugins::get_config_or_default::<super::OaiConfig>(ctx);
            if let Err(e) = mgr.fetch_models(&oai.model_filter, &oai.providers).await {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("⚠️ 刷新失败，将展示缓存列表：{e}"),
                )
                .await;
            }

            let c = mgr.config.read().await;
            let models = Manager::all_models(&c);
            if models.is_empty() {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    "📭 还没有可用模型：发 `oai deepseek 密钥` 接上一家（`oai` 看认得的供应商）；中转站列表为空时检查 [oai] model_filter",
                )
                .await;
                return;
            }

            // 各家官方供应商单独成组，标题用它的名字；默认接口那份照旧按厂商分。
            let providers: Vec<(String, String)> = c
                .provider_models
                .keys()
                .map(|name| {
                    let title = super::catalog::find(name).map_or(name.clone(), |known| known.name.to_string());
                    (name.clone(), title)
                })
                .collect();
            let listing = super::lists::Listing::Models(super::lists::Models::new(
                &models,
                &providers,
                &c.agents,
                &c.default_model,
            ));
            reply_list(
                ctx,
                writer,
                &msg_event,
                &listing,
                cmd.text_mode,
                &format!("模型列表 (共{}个)", models.len()),
            )
            .await;
        }
        Action::ViewAll(scope) => {
            let c = mgr.config.read().await;
            if let Some(a) = c.agents.iter().find(|a| a.name == *name) {
                let priv_scope = matches!(scope, Scope::Private);
                let hist = a.history(priv_scope, &uid);
                if hist.is_empty() {
                    let s = if priv_scope { "私有" } else { "公有" };
                    reply_text(
                        ctx,
                        writer,
                        &msg_event,
                        format!("📭 {name} {s}历史为空"),
                    )
                    .await;
                    return;
                }
                let content = format_history(hist, 0, cmd.text_mode);
                let header = format!(
                    "{} {}历史 ({} 条)",
                    name,
                    if priv_scope { "私有" } else { "公有" },
                    hist.len()
                );
                reply(ctx, writer, &msg_event, &content, cmd.text_mode, &header).await;
            } else {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("❌ 智能体 {name} 不存在"),
                )
                .await;
            }
        }
        Action::ViewAt(scope) => {
            if cmd.indices.is_empty() {
                reply_text(ctx, writer, &msg_event, "❌ 请指定索引：智能体/索引").await;
                return;
            }
            let c = mgr.config.read().await;
            if let Some(a) = c.agents.iter().find(|a| a.name == *name) {
                let priv_scope = matches!(scope, Scope::Private);
                let hist = a.history(priv_scope, &uid);
                let mut results = Vec::new();
                let mut extra_images = Vec::new();
                let re = Regex::new(r"!\[.*?\]\(((?:https?://|data:image/)[^\s\)]+)\)").unwrap();

                for i in &cmd.indices {
                    if *i > 0 && *i <= hist.len() {
                        let m = &hist[i - 1];
                        let role_label = match m.role.as_str() {
                            "user" => "用户",
                            "assistant" => "助手",
                            _ => "未知",
                        };
                        let mut content = m.content.clone();
                        let mut msg_imgs = extract_image_urls(&content);
                        msg_imgs.extend(m.images.clone());
                        if cmd.text_mode {
                            content = re
                                .replace_all(&content, |caps: &regex::Captures| {
                                    let url = &caps[1];
                                    if url.starts_with("data:") {
                                        "[图片]".to_string()
                                    } else {
                                        url.to_string()
                                    }
                                })
                                .to_string();
                        }
                        if !m.images.is_empty() {
                            if !content.is_empty() {
                                content.push_str("\n\n");
                            }
                            for url in &m.images {
                                if cmd.text_mode {
                                    if url.starts_with("data:") {
                                        content.push_str("\n- [Base64 Image]");
                                    } else {
                                        content.push_str(&format!("\n- {url}"));
                                    }
                                } else {
                                    content.push_str(&format!("\n![image]({url})"));
                                }
                            }
                        }
                        extra_images.extend(msg_imgs);
                        results.push(format!("**#{i} {role_label}**\n{content}"));
                    }
                }
                if results.is_empty() {
                    reply_text(ctx, writer, &msg_event, "❌ 索引无效").await;
                } else {
                    reply(
                        ctx,
                        writer,
                        &msg_event,
                        &results.join("\n\n---\n\n"),
                        cmd.text_mode,
                        &format!("{name} 历史记录"),
                    )
                    .await;
                    send_images(ctx, writer, &msg_event, &extra_images, false).await;
                }
            } else {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("❌ 智能体 {name} 不存在"),
                )
                .await;
            }
        }
        Action::Export(scope) => {
            let c = mgr.config.read().await;
            if let Some(a) = c.agents.iter().find(|a| a.name == *name) {
                let priv_scope = matches!(scope, Scope::Private);
                let hist = a.history(priv_scope, &uid);
                if hist.is_empty() {
                    reply_text(ctx, writer, &msg_event, "📭 历史为空").await;
                    return;
                }
                let scope_str = if priv_scope { "私有" } else { "公有" };
                let content = format_export_txt(name, &a.model, scope_str, hist);
                let scope_file = if priv_scope { "private" } else { "public" };
                let fname = format!(
                    "{}_{}_{}_{}.txt",
                    name,
                    scope_file,
                    uid,
                    chrono::Local::now().format("%Y%m%d%H%M%S")
                );
                let dir = mgr.path.parent().unwrap_or(&mgr.path).to_path_buf();
                let path = dir.join(&fname);
                match File::create(&path) {
                    Ok(mut f) => {
                        if f.write_all(content.as_bytes()).is_ok() {
                            let path_str = path.to_string_lossy().to_string();
                            let result = api::upload_file(
                                ctx,
                                writer.clone(),
                                msg_event.group_id(),
                                Some(msg_event.user_id()),
                                &path_str,
                                &fname,
                            )
                            .await;
                            match result {
                                Ok(_) => {
                                    reply_text(
                                        ctx,
                                        writer,
                                        &msg_event,
                                        format!("已导出：{fname}"),
                                    )
                                    .await;
                                }
                                Err(e) => {
                                    reply_text(
                                        ctx,
                                        writer,
                                        &msg_event,
                                        format!("❌ 上传失败：{e}"),
                                    )
                                    .await;
                                }
                            }
                        } else {
                            reply_text(ctx, writer, &msg_event, "❌ 写入失败").await;
                        }
                    }
                    Err(e) => {
                        reply_text(ctx, writer, &msg_event, format!("❌ 创建文件失败：{e}")).await;
                    }
                }
            } else {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("❌ 智能体 {name} 不存在"),
                )
                .await;
            }
        }
        Action::EditAt(scope) => {
            if cmd.indices.is_empty() {
                reply_text(ctx, writer, &msg_event, "❌ 请指定索引：智能体'索引 新内容").await;
                return;
            }
            if cmd.args.is_empty() {
                reply_text(ctx, writer, &msg_event, "❌ 请提供新内容").await;
                return;
            }
            let idx = cmd.indices[0];
            let mut c = mgr.config.write().await;
            if let Some(a) = c.agents.iter_mut().find(|a| a.name == *name) {
                let priv_scope = matches!(scope, Scope::Private);
                if a.edit_at(priv_scope, &uid, idx, &cmd.args) {
                    mgr.generating
                        .write()
                        .await
                        .set_generating(name, priv_scope, &uid, false);
                    if let Err(error) = mgr.save(&mut c) {
                        reply_text(
                            ctx,
                            writer,
                            &msg_event,
                            format!(
                                "保存失败，已恢复原配置。请检查磁盘空间与目录权限后重试：{error}"
                            ),
                        )
                        .await;
                        return;
                    }
                    reply_text(ctx, writer, &msg_event, format!("已编辑第 {idx} 条")).await;
                } else {
                    reply_text(ctx, writer, &msg_event, format!("❌ 索引 {idx} 无效")).await;
                }
            } else {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("❌ 智能体 {name} 不存在"),
                )
                .await;
            }
        }
        Action::DeleteAt(scope) => {
            if cmd.indices.is_empty() {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    "❌ 请指定索引：智能体-索引（支持 1,3,5 或 1-5）",
                )
                .await;
                return;
            }
            let mut c = mgr.config.write().await;
            if let Some(a) = c.agents.iter_mut().find(|a| a.name == *name) {
                let priv_scope = matches!(scope, Scope::Private);
                let deleted = a.delete_at(priv_scope, &uid, &cmd.indices);
                if !deleted.is_empty() {
                    mgr.generating
                        .write()
                        .await
                        .set_generating(name, priv_scope, &uid, false);
                }
                if deleted.is_empty() {
                    reply_text(ctx, writer, &msg_event, "❌ 索引无效").await;
                } else {
                    if let Err(error) = mgr.save(&mut c) {
                        reply_text(
                            ctx,
                            writer,
                            &msg_event,
                            format!(
                                "保存失败，已恢复原配置。请检查磁盘空间与目录权限后重试：{error}"
                            ),
                        )
                        .await;
                        return;
                    }
                    let s = deleted
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ");
                    reply_text(
                        ctx,
                        writer,
                        &msg_event,
                        format!("已删除第 {} 条（共 {} 条）", s, deleted.len()),
                    )
                    .await;
                }
            } else {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("❌ 智能体 {name} 不存在"),
                )
                .await;
            }
        }
        Action::ClearHistory(scope) => {
            let is_priv_ctx = matches!(scope, Scope::Private);
            {
                mgr.generating
                    .write()
                    .await
                    .set_generating(name, is_priv_ctx, &uid, false);
            }
            let mut c = mgr.config.write().await;
            if let Some(a) = c.agents.iter_mut().find(|a| a.name == *name) {
                let priv_scope = matches!(scope, Scope::Private);
                let s = if priv_scope { "私有" } else { "公有" };
                a.clear_history(priv_scope, &uid);
                a.generation_id += 1;
                if let Err(error) = mgr.save(&mut c) {
                    reply_text(
                        ctx,
                        writer,
                        &msg_event,
                        format!("保存失败，已恢复原配置。请检查磁盘空间与目录权限后重试：{error}"),
                    )
                    .await;
                    return;
                }
                reply_text(ctx, writer, &msg_event, format!("{name} {s}历史已清空")).await;
            } else {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("❌ 智能体 {name} 不存在"),
                )
                .await;
            }
        }
        Action::ClearAllPublic => {
            {
                mgr.generating.write().await.public.clear();
            }
            let mut c = mgr.config.write().await;
            let cnt = c.agents.len();
            for a in &mut c.agents {
                a.public_history.clear();
                a.generation_id += 1;
            }
            if let Err(error) = mgr.save(&mut c) {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("保存失败，已恢复原配置。请检查磁盘空间与目录权限后重试：{error}"),
                )
                .await;
                return;
            }
            reply_text(
                ctx,
                writer,
                &msg_event,
                format!("已清空 {cnt} 个智能体的公有历史"),
            )
            .await;
        }
        Action::ClearEverything => {
            {
                let mut g = mgr.generating.write().await;
                g.public.clear();
                g.private.clear();
            }
            let mut c = mgr.config.write().await;
            let cnt = c.agents.len();
            for a in &mut c.agents {
                a.public_history.clear();
                a.private_histories.clear();
                a.generation_id += 1;
            }
            if let Err(error) = mgr.save(&mut c) {
                reply_text(
                    ctx,
                    writer,
                    &msg_event,
                    format!("保存失败，已恢复原配置。请检查磁盘空间与目录权限后重试：{error}"),
                )
                .await;
                return;
            }
            reply_text(
                ctx,
                writer,
                &msg_event,
                format!("✅ 已清空 {cnt} 个智能体的所有历史"),
            )
            .await;
        }
        Action::Help => {
            // 先说接入状态（管家大人能不能用、缺什么），再是要紧的指令：怎么聊、怎么管房间、历史怎么翻。
            let oai = crate::plugins::get_config_or_default::<super::OaiConfig>(ctx);
            let status = super::setup::status(&oai, mgr).await;
            let help = r#"## 对话
- `房间 内容` 和房间对话
- `~房间 内容` 一次性，不留历史
- `"房间 内容` 只回文字，不转图
- `&房间 内容` 用私有历史
- `房间~` 重新生成上一条，`房间!` 停止生成

前缀可叠加，如 `~"房间 内容`。房间名随便取，中文也行。

## 房间
- `/#` 房间列表，`/%` 模型列表（默认接口与各家供应商）
- `##名称 模型 提示词` 新建或更新（普通对话房间，只聊天）
- `##名称 agent 模型` 新建内置智能体房间：自己用工具、联网、做群聊动作；模型可省
- `房间%模型` 换模型，可写 `供应商/模型:思考强度`、序号或只写模型名；不改房间类型
- `房间%agent`、`房间%chat` 转成内置智能体 / 普通对话
- `房间$提示词` 改提示词，`房间$` 清空，`房间/$` 查看
- `房间?开`、`?关`、`?默认` 这间房的联网搜索
- `房间~=新名` 重命名，`房间~#新名` 复制
- `房间:描述` 写描述，`##:模型` 批量生成
- `-#名称` 删除房间

## 历史
- `房间/*` 查看，`/1` 一条，`/1-5` 一段
- `房间_*` 导出为 .txt
- `房间'1 内容` 改第 1 条
- `房间-1` 删第 1 条，`-1,3` 多条，`-*` 清空

前面加 `&` 操作私有历史：`&房间/*`。符号半角全角都认。

## 画图
- 建一间用图像模型的房间（`##画画 gpt-image-2.5`），`房间 提示词` 就出图
- 发图、引用图片，或在房间名后 @某人，那张图就当垫图
- 可加 `--size 1536x1024`、`--quality high`
- MJ 的四宫格引用后回 `1`–`4` 放大

## 慎用
`-*` 清空所有房间的公有历史，`-*!` 连私有历史一起清。
"#;
            let help = format!("{status}\n{help}");
            reply(
                ctx,
                writer,
                &msg_event,
                &help,
                cmd.text_mode,
                "OAI 符号指令帮助",
            )
            .await;
        }
        Action::AutoFillDescriptions(model_ref) => {
            let oai = crate::plugins::get_config_or_default::<super::OaiConfig>(ctx);
            // 没写模型就用管家大人那个：新装的机器人往往只接了一家官方供应商，默认接口是空的。
            let fallback = super::setup::butler_spec(&oai, mgr).await.1;
            let (target_agents, api_config, use_model) = {
                let c = mgr.config.read().await;
                let models = Manager::all_models(&c);
                let spec = if model_ref.is_empty() {
                    fallback
                } else {
                    mgr.resolve_model(&model_ref, &models).unwrap_or(model_ref)
                };
                let (provider, model_id) = super::utils::split_provider(&spec);
                let api = super::resolve_endpoint(&oai.providers, &c.api_base, &c.api_key, provider.as_deref())
                    .unwrap_or_default();
                let targets: Vec<(String, String)> = c
                    .agents
                    .iter()
                    .filter(|a| a.description.is_empty() || a.description == "新建智能体")
                    .map(|a| (a.name.clone(), a.system_prompt.clone()))
                    .collect();
                (targets, api, model_id)
            };

            if target_agents.is_empty() {
                reply_text(ctx, writer, &msg_event, "✅ 所有智能体均已有描述，无需处理").await;
                return;
            }
            if api_config.0.is_empty() || api_config.1.is_empty() {
                reply_text(ctx, writer, &msg_event, "❌ API 未配置").await;
                return;
            }

            reply_text(
                ctx,
                writer,
                &msg_event,
                format!(
                    "开始使用 [{}] 为 {} 个智能体生成描述…",
                    use_model,
                    target_agents.len()
                ),
            )
            .await;
            let mut success_count = 0;

            for (name, prompt) in target_agents {
                let gen_prompt = format!(
                    "请阅读以下角色的 System Prompt，为其生成一个极简短的中文功能描述（Role/Tag）。\n要求：\n1. 必须控制在 10 个字以内\n2. 不要包含任何标点符号\n3. 直接输出描述内容，不要解释\n\nSystem Prompt:\n{prompt}"
                );
                let msgs = vec![LlmMessage::User {
                    content: vec![UserContent::Text(Text::new(gen_prompt))],
                }];

                if let Ok(content) =
                    super::llm::complete(&api_config.0, &api_config.1, &use_model, msgs, None).await
                {
                    let new_desc = content.trim().replace(['"', '“', '”', '。', '.'], "");
                    let mut c = mgr.config.write().await;
                    if let Some(a) = c.agents.iter_mut().find(|a| a.name == name) {
                        a.description = new_desc.clone();
                        if let Err(error) = mgr.save(&mut c) {
                            reply_text(ctx, writer, &msg_event, format!("保存失败，已恢复原配置。请检查磁盘空间与目录权限后重试：{error}")).await;
                            return;
                        }
                        success_count += 1;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            reply_text(
                ctx,
                writer,
                &msg_event,
                format!("✅ 批量处理完成，已更新 {success_count} 个智能体的描述"),
            )
            .await;
        }
        Action::Create => {}
    }
}

pub async fn handle_create(
    name: &str,
    desc: &str,
    model: &str,
    prompt: &str,
    ctx: &Context,
    writer: &LockedWriter,
    mgr: &Arc<Manager>,
) {
    let Some(msg_event) = ctx.as_message() else { return };
    let oai = crate::plugins::get_config_or_default::<super::OaiConfig>(ctx);
    let mut c = mgr.config.write().await;
    let models = Manager::all_models(&c);
    // 建房时的模型位同样认引擎关键字与供应商前缀：
    // `##研究 agent`、`##研究 agent deepseek/deepseek-flash`、`##研究 deepseek/deepseek-flash:high`。
    // 已有的内置智能体房间重写一遍时不丢引擎，规则与 `%` 相同。
    let (spec, thinking) = super::utils::split_thinking(model);
    let was_agent = c.agents.iter().any(|a| a.name == name && a.uses_agent());
    let (engine, model) = resolve_room_model(&spec, was_agent, &models, mgr, &oai);
    // 不再默认填充「你是一个有帮助的助手」：没写提示词就留空，让模型用裸提示词。
    // 生图房间尤其不该被一句通用预设污染提示词；普通房间也保持中立（MJ 同样留空）。
    let prompt = prompt.to_string();

    if let Some(a) = c.agents.iter_mut().find(|a| a.name == name) {
        // 省略模型位时只改提示词和描述，保留这个房间原来的引擎。
        if !spec.trim().is_empty() {
            a.set_engine(engine, &model);
        }
        if let Some(level) = &thinking {
            a.thinking = level.clone();
        }
        a.system_prompt = prompt;
        if !desc.is_empty() {
            a.description = desc.to_string();
        }
        let updated_model = room_model_label_with(a, oai.agent_default_model());
        if let Err(error) = mgr.save(&mut c) {
            reply_text(
                ctx,
                writer,
                &msg_event,
                format!("保存失败，已恢复原配置。请检查磁盘空间与目录权限后重试：{error}"),
            )
            .await;
            return;
        }
        reply_text(
            ctx,
            writer,
            &msg_event,
            format!("已更新 {name}（模型：{updated_model}）"),
        )
        .await;
    } else {
        let description = if desc.is_empty() {
            "新建智能体".to_string()
        } else {
            desc.to_string()
        };
        let mut agent = Agent::new(name, &model, &prompt, &description);
        agent.set_engine(engine, &model);
        if let Some(level) = thinking {
            agent.thinking = level;
        }
        let label = room_model_label_with(&agent, oai.agent_default_model());
        c.agents.push(agent);
        if let Err(error) = mgr.save(&mut c) {
            reply_text(
                ctx,
                writer,
                &msg_event,
                format!("保存失败，已恢复原配置。请检查磁盘空间与目录权限后重试：{error}"),
            )
            .await;
            return;
        }
        let tail = if engine == super::types::ENGINE_AGENT {
            "\n内置智能体：会自己用终端、联网和群聊工具。"
        } else if is_media_model(&model, &oai) || model.is_empty() {
            ""
        } else {
            "\n普通对话房间：只聊天，不调工具；要工具就建成 `##名称 agent 模型`。"
        };
        reply_text(
            ctx,
            writer,
            &msg_event,
            format!("已创建 {name}（模型：{label}）{tail}"),
        )
        .await;
    }
}

fn confirm_destructive(key: String, fingerprint: String) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    static PENDING: OnceLock<Mutex<HashMap<String, (String, Instant)>>> = OnceLock::new();
    let now = Instant::now();
    let mut pending = PENDING.get_or_init(Default::default).lock().unwrap();
    pending.retain(|_, (_, at)| now.duration_since(*at) < Duration::from_secs(60));
    if let Some((previous, _)) = pending.remove(&key)
        && previous == fingerprint
    {
        return true;
    }
    if pending.len() >= 256 {
        pending.clear();
    }
    pending.insert(key, (fingerprint, now));
    false
}
