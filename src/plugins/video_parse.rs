//! 视频站链接：先回一条预览，引用预览再开口时才把原片取进群。
//!
//! 视频站的链接不适合交给截图：抖音那类本来就只有登录墙，B 站这类要等播放器起
//! 画面，截出来又慢又没什么信息。所以这些链接改由本插件接：
//!
//! - 群里出现一条能解析的视频链接，只回一条预览——封面加上标题、UP 主、时长、
//!   播放量，一行提示说明怎么取片。这一步不下载任何视频；
//! - 用户**引用那条预览**并回复「视频」这类词，才真的去取片，取完按 `send`
//!   配的发法把成品发出去（默认群文件加视频气泡）。
//!
//! 「引用 + 回复」这套隐式交互与 AI 资讯的「引用卡片回复序号」是同一个设计：
//! 一级尽量轻，重的内容等用户开口。对应关系落在 [`state`]，取片细节见 [`bilibili`]。
//!
//! 链接准入与 `webshot` 共用一个判据（[`is_video_link`]）：本插件负责的链接，
//! 截图那边直接跳过，两处不会各截一次又取一次。

mod bilibili;
mod state;

#[cfg(test)]
mod live;

use crate::adapters::satori::{LockedWriter, send_msg, send_msg_id};
use crate::command::{find_url, message_reply_id};
use crate::config::build_config;
use crate::event::Context;
use crate::message::Message;
use crate::plugins::oai::utils::{safe_file_name, truncate_str};
use crate::plugins::oai::video::SendMode;
use crate::plugins::{ChannelConfig, PluginError, get_config_or_default, get_data_dir};
use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::Path;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::time;
use toml::Value;
use url::Url;

const LOG_TARGET: &str = "Plugin/VideoParse";

/// 认稿件与取流的接口都很轻，10 秒足够。
const API_TIMEOUT: Duration = Duration::from_secs(10);

/// 引用预览后可以回复的词。生效的前提是**先引用了本插件发的预览**，
/// 所以列宽一点不怕误伤普通消息。
const EXTRACT_WORDS: &[&str] = &[
    "视频", "原片", "原视频", "下载", "下载视频", "发视频", "取片", "文件", "video", "mp4",
];

const ALREADY_MESSAGE: &str = "这条的片子已经取过了。";
const HINT_LINE: &str = "引用本条并回复「视频」可取原片";

// ================= Config =================

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    /// 预览里那行「引用本条并回复…」。关掉就只发封面与信息。
    pub hint: bool,
    /// 取片时最多下载多大（MB）。按它挑画质；最小一档也超了就只回一句说明。
    pub max_size_mb: u64,
    /// 想要的最高画质：80=1080P / 64=720P / 32=480P / 16=360P。
    /// 未登录时 B 站只放 360P—720P，想要更高要在 `cookie` 里填登录态。
    pub prefer_quality: u32,
    /// 成品怎么发：both（群文件 + 视频气泡）/ file / bubble。
    pub send: String,
    /// 一次取片的总预算（秒），含挑画质、下载与上传。
    pub timeout_seconds: u64,
    /// 超过这么久还没取完，就先回一句「正在取片」。0 表示什么都不说。
    pub ack_after_seconds: u64,
    /// B 站登录 Cookie，留空即匿名。
    pub cookie: String,
    /// 群名单：配了黑名单就对名单外的所有群生效，配了白名单则只对名单内的群生效。
    pub channel: ChannelConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            hint: true,
            max_size_mb: 80,
            prefer_quality: 64,
            send: "both".to_string(),
            timeout_seconds: 300,
            ack_after_seconds: 20,
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
/// 又回一次预览。
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

/// 引用预览后回复的这句话，是不是在要片。
fn matches_extract_request(text: &str) -> bool {
    let cleaned = text
        .trim()
        .trim_end_matches(|c: char| "。.!！~～?？,，、".contains(c))
        .trim();
    !cleaned.is_empty() && EXTRACT_WORDS.contains(&cleaned.to_ascii_lowercase().as_str())
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
        let target = target_key(group_id, user_id);

        // 一、引用预览再回复：取片那一步。裸引用的消息不会被拦下——
        // 只有引用的是本插件发过的预览、回复的又正好是那几个词，才动手。
        if matches_extract_request(msg.text())
            && let Some(reply_id) = message_reply_id(&ctx)
        {
            match state::claim(target, &reply_id).await {
                state::Claim::Missing => {}
                state::Claim::AlreadyExtracted => {
                    let body = Message::new().reply(msg.message_id()).text(ALREADY_MESSAGE);
                    send_msg(&ctx, writer, group_id, Some(user_id), body).await?;
                    return Ok(None);
                }
                state::Claim::Ready(preview) => {
                    if let Err(error) = extract(
                        &ctx,
                        &writer,
                        &config,
                        &preview,
                        group_id,
                        user_id,
                        msg.message_id(),
                    )
                    .await
                    {
                        // 失败要把标记放回去，否则这条预览再点一次会被判成「已经取过」。
                        state::release(target, &reply_id).await;
                        warn!(
                            target: LOG_TARGET,
                            "取片失败（{}）：{}", preview.bvid, error
                        );
                        let body = Message::new()
                            .reply(msg.message_id())
                            .text(format!("没取到：{}", error));
                        send_msg(&ctx, writer, group_id, Some(user_id), body).await?;
                    }
                    return Ok(None);
                }
            }
        }

        // 二、群里出现视频站链接：只回一条预览，片子等用户开口。
        let Some(candidate) = find_url(msg.text()) else {
            return Ok(Some(ctx));
        };
        if !is_video_link(&candidate) {
            return Ok(Some(ctx));
        }
        match preview(&ctx, &writer, &config, &candidate, group_id, user_id, msg.message_id()).await
        {
            Ok(record) => {
                info!(target: LOG_TARGET, "已回预览：{}（{}）", record.title, record.bvid);
            }
            Err(error) => warn!(target: LOG_TARGET, "预览失败（{}）：{}", candidate, error),
        }
        Ok(None)
    })
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

// ================= 预览 =================

/// 拉一次稿件信息，回一条预览，并把「预览消息 → 稿件」记下来。
///
/// 记不下对应关系时返回错误：一张引用不回来的预览比不发更糟——用户会以为
/// 取片入口就在那儿。
async fn preview(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    raw_url: &str,
    group_id: Option<i64>,
    user_id: i64,
    request_id: i64,
) -> Result<state::Preview> {
    let url = resolve_link(raw_url).await?;
    let reference = bilibili::reference(&url).ok_or_else(|| anyhow!("链接没落到稿件页"))?;
    let video = bilibili::info(&reference, &config.cookie, API_TIMEOUT).await?;

    let mut message = Message::new().reply(request_id).text(preview_text(&video, config));
    if let Some(cover) = &video.cover {
        message = message.image(cover.clone());
    }

    let Some(message_id) =
        send_msg_id(ctx, writer.clone(), group_id, Some(user_id), message)
        .await
        .map_err(failed)?
    else {
        return Err(anyhow!("实现端没有回消息 ID，引用取片对不上"));
    };

    let record = state::Preview {
        target_id: target_key(group_id, user_id),
        message_id,
        created_ts: chrono::Utc::now().timestamp(),
        url: url.to_string(),
        bvid: video.bvid,
        cid: video.cid,
        page: video.page,
        title: video.title,
        duration: video.duration,
        extracted: false,
    };
    state::remember(record.clone()).await;
    Ok(record)
}

/// 预览正文：标题一行，UP 主 / 时长 / 播放量一行，再一行怎么取片。
fn preview_text(video: &bilibili::Video, config: &Config) -> String {
    let mut title = one_line(&video.title);
    if video.pages > 1 {
        title.push_str(&format!("（P{}/{}）", video.page, video.pages));
    }
    let mut text = format!("📺 {}", truncate_str(&title, 64));

    let mut meta = vec![
        one_line(&video.owner),
        duration_text(video.duration),
        format!("播放 {}", views_text(video.views)),
    ];
    meta.retain(|part| !part.trim().is_empty());
    text.push('\n');
    text.push_str(&meta.join(" · "));

    if config.hint {
        text.push('\n');
        text.push_str(HINT_LINE);
    }
    text
}

/// 取片成品的正文：片名与这一单的实际参数。
fn caption_text(preview: &state::Preview, quality: u32, size: u64) -> String {
    format!(
        "🎬 {}\n{} · {} · {:.1} MB",
        truncate_str(&one_line(&preview.title), 64),
        duration_text(preview.duration),
        bilibili::quality_label(quality),
        size as f64 / 1_048_576.0,
    )
}

/// 秒数写成 `3:33` / `1:02:03`。
fn duration_text(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// 播放量按中文习惯写成亿/万。
fn views_text(views: u64) -> String {
    let value = views as f64;
    if value >= 100_000_000.0 {
        format!("{:.1}亿", value / 100_000_000.0)
    } else if value >= 10_000.0 {
        format!("{:.1}万", value / 10_000.0)
    } else {
        views.to_string()
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ================= 取片 =================

/// 下载到本地、发进群，最后把本地那份删掉。
///
/// 顺序是刻意的：先把分片落到 `data/video_parse/` 下的临时文件，再一次性
/// `upload.create` 给实现端。B 站的分片 CDN 查 Referer，QQ 进程自己去取会 403；
/// 而落在 Termux 私有目录里的文件 QQ 也读不到，只能这样过一道手。
async fn extract(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    preview: &state::Preview,
    group_id: Option<i64>,
    user_id: i64,
    request_id: i64,
) -> Result<()> {
    let cap = config.max_size_mb.clamp(1, 2048) * 1_048_576;
    let budget = Duration::from_secs(config.timeout_seconds.clamp(30, 1800));
    let streams = bilibili::plan(
        &preview.bvid,
        preview.cid,
        config.prefer_quality,
        cap,
        &config.cookie,
        API_TIMEOUT,
    )
    .await?;

    let dir = get_data_dir("video_parse").await.map_err(failed)?;
    discard_stale_parts(&dir).await;
    let path = dir.join(format!("{}.part", safe_file_name(&preview.bvid)));

    let work = async {
        let size = download(&streams.urls, &path, cap, budget).await?;
        deliver(
            ctx,
            writer,
            config,
            preview,
            &streams,
            size,
            &path,
            group_id,
            user_id,
            request_id,
        )
        .await
    };
    let result = with_ack(ctx, writer, config, group_id, user_id, request_id, work).await;

    // 成品已经发出去（或者发失败），本地这份就没有用了。
    let _ = tokio::fs::remove_file(&path).await;
    result
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

/// 上限与画质挑完才动手，这里只负责把片子送出去。
#[allow(clippy::too_many_arguments)]
async fn deliver(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    preview: &state::Preview,
    streams: &bilibili::Streams,
    size: u64,
    path: &Path,
    group_id: Option<i64>,
    user_id: i64,
    request_id: i64,
) -> Result<()> {
    let caption = caption_text(preview, streams.quality, size);
    let body = Message::new().reply(request_id).text(caption);
    send_msg(ctx, writer.clone(), group_id, Some(user_id), body)
        .await
        .map_err(failed)?;

    // 上传一次，群文件与视频气泡共用同一份资源：一次取片只往实现端送一遍。
    let name = format!("{}.mp4", safe_file_name(&preview.title));
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
    if matches!(send, SendMode::File | SendMode::Both) {
        let file = Message::new().file(resource.clone(), Some(name.clone()));
        send_msg(ctx, writer.clone(), group_id, Some(user_id), file)
            .await
            .map_err(failed)?;
    }
    if matches!(send, SendMode::Bubble | SendMode::Both) {
        let bubble = Message::new().video(resource);
        send_msg(ctx, writer.clone(), group_id, Some(user_id), bubble)
            .await
            .map_err(failed)?;
    }
    Ok(())
}

/// 取片要下几十兆，群里等起来像是没反应。超过 `ack_after_seconds` 还没完，
/// 就先回一句说明；配 0 就什么都不说。
async fn with_ack<F>(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    group_id: Option<i64>,
    user_id: i64,
    request_id: i64,
    work: F,
) -> Result<()>
where
    F: Future<Output = Result<()>>,
{
    if config.ack_after_seconds == 0 {
        return work.await;
    }
    futures_util::pin_mut!(work);
    tokio::select! {
        result = &mut work => result,
        _ = time::sleep(Duration::from_secs(config.ack_after_seconds)) => {
            let body = Message::new().reply(request_id).text("正在取片，稍等…");
            if let Err(error) = send_msg(ctx, writer.clone(), group_id, Some(user_id), body).await {
                warn!(target: LOG_TARGET, "取片提示发送失败: {}", error);
            }
            work.await
        }
    }
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

    fn config() -> Config {
        Config::default()
    }

    fn video() -> bilibili::Video {
        bilibili::Video {
            bvid: "BV1GJ411x7h7".into(),
            cid: 137649199,
            page: 1,
            pages: 1,
            title: "【官方 MV】Never Gonna Give You Up - Rick Astley".into(),
            owner: "索尼音乐中国".into(),
            duration: 213,
            cover: Some("https://i1.hdslb.com/bfs/archive/a.jpg".into()),
            views: 105_703_453,
        }
    }

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

    #[test]
    fn extract_words_are_matched_exactly() {
        for text in ["视频", " 原片 ", "下载视频。", "VIDEO", "mp4", "文件！"] {
            assert!(matches_extract_request(text), "{text} 应该算取片请求");
        }
        for text in ["", "这个视频不错", "视频吗", "下载了", "转发"] {
            assert!(!matches_extract_request(text), "{text} 不该算取片请求");
        }
    }

    #[test]
    fn the_preview_names_the_video_and_how_to_get_it() {
        let text = preview_text(&video(), &config());
        assert!(text.starts_with("📺 【官方 MV】Never Gonna Give You Up - Rick Astley\n"));
        assert!(text.contains("索尼音乐中国 · 3:33 · 播放 1.1亿"));
        assert!(text.ends_with(HINT_LINE));

        let quiet = preview_text(&video(), &Config { hint: false, ..config() });
        assert!(!quiet.contains(HINT_LINE));
    }

    #[test]
    fn multi_part_videos_say_which_page() {
        let mut video = video();
        video.pages = 4;
        video.page = 3;
        assert!(preview_text(&video, &config()).contains("（P3/4）"));
    }

    /// 一个字的标题、没有 UP 主、零播放量都不该在预览里留下空行或者 ` · `。
    #[test]
    fn missing_fields_do_not_leave_holes() {
        let mut video = video();
        video.owner = String::new();
        video.views = 0;
        let text = preview_text(&video, &config());
        assert!(text.contains("\n3:33 · 播放 0\n"));
        assert!(!text.contains(" ·  · "));
    }

    #[test]
    fn durations_and_views_read_the_way_the_group_writes_them() {
        assert_eq!(duration_text(213), "3:33");
        assert_eq!(duration_text(3723), "1:02:03");
        assert_eq!(duration_text(59), "0:59");
        assert_eq!(views_text(0), "0");
        assert_eq!(views_text(9999), "9999");
        assert_eq!(views_text(10_000), "1.0万");
        assert_eq!(views_text(105_703_453), "1.1亿");
        assert_eq!(views_text(30_000), "3.0万");
    }

    #[test]
    fn the_caption_says_what_this_take_cost() {
        let preview = state::Preview {
            target_id: 1,
            message_id: "m1".into(),
            created_ts: 0,
            url: "https://b23.tv/abc".into(),
            bvid: "BV1GJ411x7h7".into(),
            cid: 137649199,
            page: 1,
            title: "测试稿件".into(),
            duration: 213,
            extracted: true,
        };
        let text = caption_text(&preview, 64, 25_847_808);
        assert_eq!(text, "🎬 测试稿件\n3:33 · 720P · 24.7 MB");
    }

    #[test]
    fn a_group_and_a_private_chat_do_not_share_a_key() {
        assert_eq!(target_key(Some(42), 7), 42);
        assert_eq!(target_key(None, 7), -7);
        assert_eq!(target_key(Some(0), 7), -7);
    }

    // ============ 分流 ============

    const PREVIEW_ID: &str = "7001";
    const GROUP: i64 = 1000;

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
        let event = simd_json::serde::to_owned_value(serde_json::json!({
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
        .unwrap();

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

    fn taken_preview(message_id: &str) -> state::Preview {
        state::Preview {
            target_id: GROUP,
            message_id: message_id.to_string(),
            created_ts: chrono::Utc::now().timestamp(),
            url: "https://b23.tv/abc".into(),
            bvid: "BV1GJ411x7h7".into(),
            cid: 137649199,
            page: 1,
            title: "测试稿件".into(),
            duration: 213,
            extracted: true,
        }
    }

    /// 取片那条路只认「引用本插件发过的预览 + 一句取片词」，别的一律放行，
    /// 免得把普通消息吃掉。
    #[tokio::test]
    async fn only_a_quoted_preview_asking_for_the_video_is_consumed() {
        state::remember(taken_preview(PREVIEW_ID)).await;

        // 引用预览说「视频」：走取片那条路。这条已经取过，回一句就吃掉事件。
        let (ctx, writer) = event("视频", Some(PREVIEW_ID)).await;
        assert!(
            handle(ctx, writer).await.unwrap().is_none(),
            "引用预览要片应该被本插件吃掉"
        );

        // 引用的不是本插件发过的消息。
        let (ctx, writer) = event("视频", Some("999999")).await;
        assert!(
            handle(ctx, writer).await.unwrap().is_some(),
            "引用别人的消息不该被吃掉"
        );

        // 引用了预览，但说的不是取片词。
        let (ctx, writer) = event("这条不错", Some(PREVIEW_ID)).await;
        assert!(
            handle(ctx, writer).await.unwrap().is_some(),
            "引用预览说别的话不该被吃掉"
        );

        // 没引用，光提了一句「视频」。
        let (ctx, writer) = event("视频", None).await;
        assert!(
            handle(ctx, writer).await.unwrap().is_some(),
            "没引用时不该被吃掉"
        );
    }

    /// 号主与机器人共用同一个 QQ 号，他自己的消息同样带着这个号进来。
    /// 靠 `manual_self` 分辨：号主贴的链接要照常接，机器人自己的回声才跳过。
    #[tokio::test]
    async fn the_owner_shares_the_account_but_still_gets_served() {
        let (ctx, writer) = event_from(7, true, "视频", Some(PREVIEW_ID)).await;
        assert!(
            handle(ctx, writer).await.unwrap().is_none(),
            "号主手打的消息应当照常处理"
        );

        let (ctx, writer) = event_from(7, false, "视频", Some(PREVIEW_ID)).await;
        assert!(
            handle(ctx, writer).await.unwrap().is_some(),
            "机器人自己的回声不该被处理"
        );
    }
}
