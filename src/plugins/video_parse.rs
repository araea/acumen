//! 视频站链接：群里出现一条能解析的链接，就地把原片取进群。
//!
//! 视频站的链接不适合交给截图：抖音那类本来就只有登录墙，B 站这类要等播放器起
//! 画面，截出来又慢又没什么信息。所以这些链接改由本插件接，一步到底：
//! 链接进来、片子出去，不先回预览，不等引用，也没有要用户记住的指令。
//!
//! 触发面覆盖消息里的三种形态，判据都是「点开它会去哪」：
//!
//! - 正文里的链接（[`crate::command::message_links`] 认得的都算）；
//! - QQ 的小程序卡与分享卡——链接在那段 `json` 载荷里，正文是空的。
//!
//! 群里只说成品那一条消息：不补一句「正在取片」，也**不报错**——取不到就当这条链接
//! 没被接住，原因写进日志给人看。成品自己就是那条消息，不再另发一条正文，**也不带
//! 引用**——视频在 QQ 里是「顺媒体」，与引用放在一条消息里就显示不出来（见 [`send`]）。
//! 群里等得着的只有片子，中途冒出来的每一句都是打扰。
//! 同一个人在同一会话里十分钟内重贴同一条不会下第二遍（[`state`]）。
//!
//! 链接准入与 `webshot` 共用一个判据（[`is_video_link`]）：本插件负责的链接，
//! 截图那边直接跳过，两处不会各截一次又取一次。

mod bilibili;
mod state;

#[cfg(test)]
mod live;

use crate::adapters::satori::{LockedWriter, send_msg};
use crate::command::message_links;
use crate::config::build_config;
use crate::event::Context;
use crate::message::Message;
use crate::plugins::oai::utils::safe_file_name;
use crate::plugins::oai::video::SendMode;
use crate::plugins::{ChannelConfig, PluginError, get_config_or_default, get_data_dir};
use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;
use tokio::time;
use toml::Value;
use url::Url;

const LOG_TARGET: &str = "Plugin/VideoParse";

/// 认稿件与取流的接口都很轻，10 秒足够。
const API_TIMEOUT: Duration = Duration::from_secs(10);

/// 同时进行的取片上限。任意群友贴一条链接就能让我们下几十兆，没有闸门时
/// 一个人连贴五条链接就是五份并发下载，手机的网络与内存都吃不下。
/// 排队排在下载预算之外：等闸门的时间不算进单片的超时，也不在群里冒出一句状态。
static TAKE_GATE: Semaphore = Semaphore::const_new(2);

// ================= Config =================

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    /// 取片时最多下载多大（MB）。按它挑画质；最小一档也超了就放弃这条链接，
    /// 只在日志里写一句说明。
    pub max_size_mb: u64,
    /// 想要的最高画质：80=1080P / 64=720P / 32=480P / 16=360P。
    /// 未登录时 B 站只放 360P—720P，想要更高要在 `cookie` 里填登录态。
    pub prefer_quality: u32,
    /// 成品怎么发：bubble（只发视频气泡，默认）/ file（只发群文件）/ both。
    /// 默认只发气泡，一份成品一条消息，不让群文件白占一份空间；要群文件的群再开
    /// `file` 或 `both`——`both` 也是安全的：气泡先发，文件那条腿失败只少一个文件。
    pub send: String,
    /// 一次取片的总预算（秒），含挑画质、下载与上传。
    pub timeout_seconds: u64,
    /// B 站登录 Cookie，留空即匿名。
    pub cookie: String,
    /// 群名单：配了黑名单就对名单外的所有群生效，配了白名单则只对名单内的群生效。
    pub channel: ChannelConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            max_size_mb: 80,
            prefer_quality: 64,
            send: "bubble".to_string(),
            timeout_seconds: 300,
            cookie: String::new(),
            channel: ChannelConfig::default(),
        }
    }
}

pub fn default_config() -> Value {
    build_config(Config::default())
}

// ================= 链接准入 =================

/// 链接是否属于本插件负责的视频站。
///
/// `webshot` 也用这个判据：两边必须认同一份名单，否则同一条链接会被截一次图、
/// 又取一遍片。
pub(crate) fn is_video_link(raw: &str) -> bool {
    let Ok(url) = Url::parse(raw) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    // 短链只有跟过去才知道落到哪个页面；分享出来基本都是稿件，先认下。
    bilibili::is_short_host(host) || bilibili::reference(&url).is_some()
}

/// 短链先跟着跳一次拿到真正的地址，长链原样返回。
async fn resolve_link(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).map_err(|_| anyhow!("无法解析的链接"))?;
    let short = url.host_str().is_some_and(bilibili::is_short_host);
    if !short {
        return Ok(url);
    }
    let response = crate::http::client()
        .get(url)
        .header(reqwest::header::USER_AGENT, bilibili::UA)
        .timeout(API_TIMEOUT)
        .send()
        .await?;
    // 实现端不一定给短链配了跳转；落回原地址时交给下游报错。
    Ok(response.url().clone())
}

// ================= Main Handler =================

pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let Some(msg) = ctx.as_message() else {
            return Ok(Some(ctx));
        };

        let config: Config = get_config_or_default(&ctx, "video_parse");
        if !config.enabled {
            return Ok(Some(ctx));
        }
        let group_id = msg.group_id();
        if !config.channel.allows(group_id) {
            return Ok(Some(ctx));
        }
        let user_id = msg.user_id();
        // 号主与机器人共用同一个 QQ 号：他自己手打的消息也带着这个号进来，
        // 靠 `manual_self` 分辨。只跳过机器人自己发出去的那一份回声，
        // 别把号主贴的链接一起跳掉。
        let self_id = ctx.bot.login_user.get().id.parse::<i64>().unwrap_or(0);
        if user_id == self_id && !msg.is_manual_self() {
            return Ok(Some(ctx));
        }

        // 链接不一定写在正文里：QQ 的小程序卡与分享卡是一段落在 `json` 元素里的
        // 载荷，落地地址要从里面取（见 `message_links`）。两种来源都收，取第一个
        // 认得的稿件页。
        let Some(candidate) = take_candidate(&ctx) else {
            return Ok(Some(ctx));
        };

        if let Err(error) = take(&ctx, &writer, &config, &candidate, group_id, user_id).await {
            // 取不到就静默收场：群里只当这条链接没被接住，原因留给日志。
            // 一条「没取到」在群里就是一次打扰，而贴链接的人自己看得出来没片子。
            warn!(target: LOG_TARGET, "取片失败（{}）：{}", candidate, error);
        }
        // 视频站的链接归本插件，后面几个链接类插件（截图）不必再看一眼。
        Ok(None)
    })
}

/// 这条消息里第一个本插件接得住的链接。
///
/// [`message_links`] 把卡片里的落地地址排在正文前面，这里逐个过一遍
/// [`is_video_link`]——卡片载荷里还混着封面与图标的地址，第一个能用的才作数。
fn take_candidate(ctx: &Context) -> Option<String> {
    message_links(ctx)
        .into_iter()
        .find(|url| is_video_link(url))
}

/// 会话标识：群聊用群号，私聊取用户号的负数。
fn target_key(group_id: Option<i64>, user_id: i64) -> i64 {
    group_id.filter(|id| *id != 0).unwrap_or(-user_id.max(1))
}

/// 插件边界上的错误（`BotError` / `PluginError`）进不了 anyhow，这里翻一层，
/// 好让取片流程里的每一步失败都能带着一句人话往上传。
fn failed(error: Box<dyn std::error::Error + Send + Sync>) -> anyhow::Error {
    anyhow!("{error}")
}

// ================= 取片 =================

/// 一条链接的完整处理：认稿件、拉信息、占名额、下载、发出去。
async fn take(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    raw_url: &str,
    group_id: Option<i64>,
    user_id: i64,
) -> Result<()> {
    let url = resolve_link(raw_url).await?;
    let reference = bilibili::reference(&url).ok_or_else(|| anyhow!("链接没落到稿件页"))?;
    let video = bilibili::info(&reference, &config.cookie, API_TIMEOUT).await?;

    // 同一个人在同一个会话里刚贴过同一条：片子已经在群里，或者正在取，不必再来一遍。
    // 判在拉完稿件信息之后，是因为判据要用上游认出来的 `bvid`——同一条链接
    // 一次是短链、一次是长链时，光比地址认不出来。
    let target = target_key(group_id, user_id);
    let now = chrono::Utc::now().timestamp();
    if state::claim(target, user_id, &video.bvid, now).await == state::Claim::Recent {
        info!(target: LOG_TARGET, "同一条刚由同一个人贴过，不再取第二遍：{}", video.bvid);
        return Ok(());
    }

    let result = extract(ctx, writer, config, &video, group_id, user_id).await;
    if result.is_err() {
        // 没取到就把名额放回去：他重贴一次还能再来。
        state::release(target, user_id, &video.bvid).await;
    }
    result
}

/// 挑画质、下载到本地、发进群，最后把本地那份删掉。
///
/// 顺序是刻意的：先把分片落到 `data/video_parse/` 下的临时文件，再一次性
/// `upload.create` 给实现端。B 站的分片 CDN 查 Referer，QQ 进程自己去取会 403；
/// 而落在 Termux 私有目录里的文件 QQ 也读不到，只能这样过一道手。
async fn extract(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    video: &bilibili::Video,
    group_id: Option<i64>,
    user_id: i64,
) -> Result<()> {
    let cap = config.max_size_mb.clamp(1, 2048) * 1_048_576;
    let budget = Duration::from_secs(config.timeout_seconds.clamp(30, 1800));
    let streams = bilibili::plan(
        &video.bvid,
        video.cid,
        config.prefer_quality,
        cap,
        &config.cookie,
        API_TIMEOUT,
    )
    .await?;

    let dir = get_data_dir("video_parse").await.map_err(failed)?;
    discard_stale_parts(&dir).await;
    let scratch = Scratch::new(&dir, &video.bvid);

    // 闸门在下载预算之外：排在前头那单的时间里不算这一单的超时。
    let _permit = TAKE_GATE
        .acquire()
        .await
        .map_err(|_| anyhow!("取片闸门不可用"))?;
    let size = download(&streams.urls, scratch.path(), cap, budget).await?;
    send(
        ctx,
        writer,
        config,
        video,
        &streams,
        size,
        scratch.path(),
        group_id,
        user_id,
    )
    .await
}

/// 一次取片落在本地的那一份片子，析构时删掉。
///
/// 下载超时、超过大小上限、上传失败，每一条都是提前返回；把 `remove_file` 写在
/// 末尾，迟早会漏掉其中一条——下载失败那条就漏过。清理挂在值的生命周期上，返回
/// 路径怎么写都不会漏。只有进程被 SIGKILL 那一下不走，留给 [`discard_stale_parts`]
/// 在下次取片开头兜底。
///
/// 名字在稿件号之外带一段随机数：去重判据是「同一个人在同一会话里贴过没有」，
/// 同一个群里的第二个人、或另一个群同时贴同一条稿件都是两份并发取片，只按稿件号
/// 命名会让它们写进同一个文件——互相截断，先跑完的那份还会把另一份的文件删掉。
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(dir: &Path, bvid: &str) -> Self {
        Self {
            path: dir.join(format!(
                "{}-{:032x}.part",
                safe_file_name(bvid),
                rand::random::<u128>()
            )),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // 析构里不能 await；一次 unlink 是微秒级，直接同步做掉。
        let _ = std::fs::remove_file(&self.path);
    }
}

/// 把分片按顺序写进同一个文件，边写边按上限收口。
///
/// 老稿件的 `durl` 会切成好几段，它们本来就是同一路 mp4 的连续分片，顺序拼起来
/// 即可。上限在看之前先按站点给的字节数筛过一道，这里再兜一次——免得上游少报了
/// 体积，把手机写满。
async fn download(urls: &[String], path: &Path, cap: u64, budget: Duration) -> Result<u64> {
    let client = crate::http::client();
    let mut file = tokio::fs::File::create(path).await?;
    let mut total = 0u64;

    let attempt = async {
        for url in urls {
            let response = client
                .get(url)
                .header(reqwest::header::USER_AGENT, bilibili::UA)
                .header(reqwest::header::REFERER, bilibili::REFERER)
                .send()
                .await?
                .error_for_status()?;
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                total += chunk.len() as u64;
                if total > cap {
                    return Err(anyhow!("视频超过大小上限"));
                }
                file.write_all(&chunk).await?;
            }
        }
        file.flush().await?;
        Ok(total)
    };

    match time::timeout(budget, attempt).await {
        Ok(result) => result,
        Err(_) => Err(anyhow!("下载超时")),
    }
}

/// 上传一份、发出去。
///
/// 视频气泡与群文件共用同一次上传。一条成品只有成品本身，不再另发一条正文：
/// 用户在群里等的是这条片子，不是关于它的说明。
///
/// **两条腿都是纯媒体消息**（不带引用，也不带 @ 与文字）：QQ 里语音、视频、群文件
/// 必须单独成条，同条的引用与文字会把它顶成空气泡（2026-09-17 实测）。成品要的就是
/// 一段干净的视频，不该为了带引用被拆成两条；群里同时有好几条链接时靠时间顺序对上，
/// 成品就挨在触发它的那条后面。实现端对「顺媒体 + 别的段落」也会兜底拆开，见
/// `docs/SATORI_SUPPORT.md`——这里不靠它。
#[allow(clippy::too_many_arguments)]
async fn send(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    video: &bilibili::Video,
    streams: &bilibili::Streams,
    size: u64,
    path: &Path,
    group_id: Option<i64>,
    user_id: i64,
) -> Result<()> {
    let name = format!("{}.mp4", safe_file_name(&video.title));
    let bytes = tokio::fs::read(path).await?;
    let uploaded = writer
        .upload(ctx, bytes, &name, "video/mp4")
        .await
        .map_err(failed)?;
    let resource = uploaded
        .get("file")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("上传没有返回资源"))?
        .to_string();

    let send = SendMode::parse(&config.send);
    // 先发能点开就播的那条（视频气泡），再补群文件。
    //
    // 两件事决定了这个顺序：① 实现端对上传失败的富媒体会就地重试到超预算（默认 2 次 /
    // 45 秒），而有些群不让普通成员发群文件——文件那条腿注定失败，不该让它挡着气泡；
    // ② 两条腿各自兜错，`both` 时一条成了就算送到（另一条只留一行 warn），只配一条时
    // 它自己的失败照旧往上报，最后由 [`handle`] 写进日志，群里不开口。
    let mut sent = false;
    let mut failure = None;
    if matches!(send, SendMode::Bubble | SendMode::Both) {
        let bubble = Message::new().video(resource.clone());
        match send_msg(ctx, writer.clone(), group_id, Some(user_id), bubble).await {
            Ok(()) => {
                sent = true;
            }
            Err(error) => {
                warn!(target: LOG_TARGET, "视频气泡没发出去：{}", error);
                failure = Some(failed(error));
            }
        }
    }
    if matches!(send, SendMode::File | SendMode::Both) {
        let file = Message::new().file(resource, Some(name.clone()));
        match send_msg(ctx, writer.clone(), group_id, Some(user_id), file).await {
            Ok(()) => {
                sent = true;
            }
            Err(error) => {
                warn!(target: LOG_TARGET, "群文件没发出去（{}）：{}", name, error);
                failure = Some(failed(error));
            }
        }
    }
    if !sent {
        return Err(failure.unwrap_or_else(|| anyhow!("成品没有发出去")));
    }

    info!(
        target: LOG_TARGET,
        "已取片：{}（{} · P{}/{} · {} · {:.1} MB）",
        video.title,
        video.bvid,
        video.page,
        video.pages,
        bilibili::quality_label(streams.quality),
        size as f64 / 1_048_576.0,
    );
    Ok(())
}

/// 上次取片被打断留下的残片。手机上的空间经不起一条几十兆的 `.part` 常驻，
/// 每次动手前顺手清掉放旧的。
async fn discard_stale_parts(dir: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    let cutoff = std::time::SystemTime::now() - Duration::from_secs(6 * 3600);
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("part") {
            continue;
        }
        let stale = entry
            .metadata()
            .await
            .and_then(|metadata| metadata.modified())
            .map(|modified| modified < cutoff)
            .unwrap_or(false);
        if stale {
            let _ = tokio::fs::remove_file(&path).await;
        }
    }
}

/// Validate control edits against the plugin's actual configuration type.
pub fn validate_config(value: &toml::Value) -> Result<(), String> {
    <Config as serde::Deserialize>::deserialize(value.clone())
        .map(|_| ())
        .map_err(|_| "配置类型不匹配（请检查数组元素、字段类型及整数范围）".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::satori::SatoriClient;
    use crate::config::AppConfig;
    use crate::event::{BotStatus, EventType, LoginUser};
    use crate::matcher::Matcher;
    use crate::scheduler::Scheduler;
    use sea_orm::Database;
    use std::sync::{Arc, RwLock};
    use tokio::sync::Mutex as AsyncMutex;

    const SAMPLE: &str = "https://www.bilibili.com/video/BV1GJ411x7h7";
    const GROUP: i64 = 1000;

    #[test]
    fn only_the_video_pages_we_can_parse_are_taken_over() {
        for raw in [
            "https://www.bilibili.com/video/BV1GJ411x7h7",
            "https://b23.tv/BV1GJ411x7h7",
            "https://bili2233.cn/abc",
            "https://m.bilibili.com/video/av80433022",
        ] {
            assert!(is_video_link(raw), "{raw} 应由本插件接");
        }
        // 番剧、专栏、直播、空间这些截个图比取片有用，留给 webshot。
        for raw in [
            "https://www.bilibili.com/bangumi/play/ep307580",
            "https://www.bilibili.com/read/cv123456",
            "https://live.bilibili.com/12345",
            "https://space.bilibili.com/486906719",
            "https://example.com/video/BV1GJ411x7h7",
            "https://v.douyin.com/c9EJkQ5hNz0/",
            "随便一句话",
        ] {
            assert!(!is_video_link(raw), "{raw} 不该由本插件接");
        }
    }

    /// 默认只发视频气泡：有些群不让普通成员发群文件，那条腿必定失败，实现端还会为它
    /// 重试到超预算，把整条取片流程一起拖垮。要群文件的群再单独开。
    #[test]
    fn the_take_goes_out_as_a_bubble_unless_the_group_asks_for_the_file() {
        let send = Config::default().send;
        assert_eq!(send, "bubble", "成品默认发法不该变：{send}");
        assert!(matches!(SendMode::parse(&send), SendMode::Bubble));
    }

    #[test]
    fn a_group_and_a_private_chat_do_not_share_a_key() {
        assert_eq!(target_key(Some(42), 7), 42);
        assert_eq!(target_key(None, 7), -7);
        assert_eq!(target_key(Some(0), 7), -7);
    }

    // ============ 分流 ============

    /// 一条群消息，可选地带一个引用段；写回执走控制台适配器，不发真群。
    async fn event(text: &str, reply: Option<&str>) -> (Context, LockedWriter) {
        event_from(42, false, text, reply).await
    }

    /// 同上，但要指定发送者与 `manual_self`——号主手打的消息是后者为真的那种。
    async fn event_from(
        user_id: i64,
        manual_self: bool,
        text: &str,
        reply: Option<&str>,
    ) -> (Context, LockedWriter) {
        let mut message = Vec::new();
        if let Some(id) = reply {
            message.push(serde_json::json!({"type": "reply", "data": {"id": id}}));
        }
        message.push(serde_json::json!({"type": "text", "data": {"text": text}}));
        stage(serde_json::json!({
            "post_type": "message",
            "satori_type": "message-created",
            "message_type": "group",
            "group_id": GROUP,
            "user_id": user_id,
            "manual_self": manual_self,
            "message_id": 9001,
            "raw_message": text,
            "sender": {"nickname": "群友", "role": "member"},
            "message": message
        }))
        .await
    }

    /// 一条卡片消息：正文是空的，链接在 `json` 段里，`raw_message` 是与实现端一致的
    /// CQ 形态（`[CQ:json,data=…]`）。
    async fn card_event(payload: &str) -> (Context, LockedWriter) {
        stage(serde_json::json!({
            "post_type": "message",
            "satori_type": "message-created",
            "message_type": "group",
            "group_id": GROUP,
            "user_id": 42,
            "message_id": 9002,
            "raw_message": format!("[CQ:json,data={payload}]"),
            "sender": {"nickname": "群友", "role": "member"},
            "message": [{"type": "json", "data": {"data": payload}}]
        }))
        .await
    }

    /// 拿一份事件造一个可用的上下文；写回执走控制台适配器，不发真群。
    async fn stage(event: serde_json::Value) -> (Context, LockedWriter) {
        let event = simd_json::serde::to_owned_value(event).unwrap();

        let mut config = AppConfig::default();
        for plugin in crate::plugins::get_plugins() {
            config
                .plugins
                .insert(plugin.name.to_string(), (plugin.default_config)());
        }
        let ctx = Context {
            event: EventType::Satori(event),
            config: Arc::new(RwLock::new(config)),
            config_save_lock: Arc::new(AsyncMutex::new(())),
            db: Database::connect("sqlite::memory:").await.unwrap(),
            scheduler: Arc::new(Scheduler::new()),
            matcher: Arc::new(Matcher::new()),
            config_path: Arc::from("video-parse-test.toml"),
            bot: Arc::new(BotStatus {
                adapter: "satori-qq".to_string(),
                platform: "red".to_string(),
                login_user: LoginUser {
                    id: "7".to_string(),
                    ..Default::default()
                }
                .into(),
            }),
        };
        (ctx, Arc::new(SatoriClient::console()))
    }

    /// 没有本插件接得住的链接时，事件原样放行——普通消息、别家的链接、
    /// 指向别处的卡片都不该被吃掉。这条不碰网络：放行的那几条都轮不到取片。
    #[tokio::test]
    async fn a_message_without_a_video_link_is_left_to_the_other_plugins() {
        for text in [
            "随便一句话",
            "https://example.com/some/page",
            "https://mp.weixin.qq.com/s/abc",
            "https://www.bilibili.com/bangumi/play/ep307580",
        ] {
            let (ctx, writer) = event(text, None).await;
            assert!(
                handle(ctx, writer).await.unwrap().is_some(),
                "{text} 不该被本插件吃掉"
            );
        }
    }

    /// 号主与机器人共用同一个 QQ 号，他自己的消息同样带着这个号进来。靠 `manual_self`
    /// 分辨：机器人自己发出去的那份回声连链接都不取（放行给后面的插件），
    /// 号主贴的照常取。取片那条真机路走 live 用例，这里钉的是回声那一半。
    #[tokio::test]
    async fn the_bots_own_echo_is_left_alone() {
        let (ctx, writer) = event_from(7, false, SAMPLE, None).await;
        assert!(
            handle(ctx, writer).await.unwrap().is_some(),
            "机器人自己的回声不该被处理"
        );
    }

    /// 群名单拦下时一声不出，也不取片。
    #[tokio::test]
    async fn a_blocked_group_is_never_served() {
        let (ctx, writer) = event(SAMPLE, None).await;
        {
            let mut config = ctx.config.write().unwrap();
            let value = config.plugins.get_mut("video_parse").unwrap();
            value["channel"]["black"] = toml::Value::Array(vec![toml::Value::Integer(GROUP)]);
        }
        assert!(
            handle(ctx, writer).await.unwrap().is_some(),
            "名单外的群该原样放行"
        );
    }

    /// 卡片消息：正文是空的，链接在 `json` 段里——QQ 的小程序卡与分享卡都是这个
    /// 形态。载荷里封面与图标的地址排在前面，不能让它把卡片真正打开的那页顶掉。
    #[tokio::test]
    async fn a_card_is_read_for_the_page_it_opens() {
        // 小程序卡：地址是转义过的（`https:\/\/`），正则抓不到。
        let miniapp = r#"{"app":"com.tencent.miniapp_01","prompt":"[QQ小程序]琵琶曲",
            "meta":{"detail_1":{"title":"哔哩哔哩",
            "icon":"http:\/\/miniapp.gtimg.cn\/public\/appicon\/432b.jpg",
            "preview":"https:\/\/qq.ugcimg.cn\/v1\/gio99kjvll3gl6baq",
            "qqdocurl":"https:\/\/b23.tv\/czQoMIg?share_medium=android&share_source=qq"}}}"#;
        let (ctx, _writer) = card_event(miniapp).await;
        assert_eq!(
            take_candidate(&ctx).as_deref(),
            Some("https://b23.tv/czQoMIg?share_medium=android&share_source=qq")
        );

        // 分享卡：落地地址在 `meta.news.jumpUrl`。
        let news = r#"{"app":"com.tencent.tuwen.lua","view":"news",
            "meta":{"news":{"jumpUrl":"https://b23.tv/DONRtWF",
            "preview":"https://qq.ugcimg.cn/v1/odu6is84rije659prqcbgoornfg14q"}}}"#;
        let (ctx, _writer) = card_event(news).await;
        assert_eq!(
            take_candidate(&ctx).as_deref(),
            Some("https://b23.tv/DONRtWF")
        );

        // 卡片指向的页面不归本插件，事件原样放行给后面的插件。
        let other = r#"{"app":"com.tencent.structmsg","view":"news",
            "meta":{"news":{"jumpUrl":"https://mp.weixin.qq.com/s/abc"}}}"#;
        let (ctx, _writer) = card_event(other).await;
        assert_eq!(take_candidate(&ctx), None);
    }

    // ============ 临时文件 ============

    /// 下载半路失败（超时、超过大小上限、连不上）也该把本地那份删掉。清理挂在
    /// `Scratch` 的析构上，不是末尾写一句 `remove_file`——曾经就是那样，下载失败
    /// 提前返回，几十兆的 `.part` 留在手机里，要等六小时后下次取片才被扫走。
    #[tokio::test]
    async fn an_unfinished_take_leaves_no_part_behind() {
        let dir = std::env::temp_dir().join(format!("acumen-part-{:032x}", rand::random::<u128>()));
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let path = {
            let scratch = Scratch::new(&dir, "BV1GJ411x7h7");
            tokio::fs::write(scratch.path(), b"half a video")
                .await
                .unwrap();
            scratch.path().to_path_buf()
        };
        assert!(!path.exists(), "析构后本地那份不该留下");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// 同一个群里两个人、或两个群同时贴同一条稿件，是两份并发取片，不能落到
    /// 同一个文件上——只按稿件号命名时它们会互相截断。
    #[test]
    fn two_takes_of_one_video_do_not_share_a_part_file() {
        let dir = std::env::temp_dir();
        let first = Scratch::new(&dir, "BV1GJ411x7h7");
        let second = Scratch::new(&dir, "BV1GJ411x7h7");

        assert_ne!(first.path(), second.path());
        assert!(
            first.path().to_string_lossy().contains("BV1GJ411x7h7"),
            "文件名里留着稿件号，便于对日志：{:?}",
            first.path()
        );
    }
}
