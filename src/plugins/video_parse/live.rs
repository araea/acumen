//! 取片的真机验证。
//!
//! 两条都默认 `ignored`：一条只连 B 站拉稿件信息与取流地址，不碰群；
//! 另一条要显式给沙盒群号，会真的往群里发一条视频（正文 + 群文件 + 气泡）再撤回。
//!
//! ```sh
//! cargo test --bin ayjx live_reads_the_metadata -- --ignored --nocapture
//! AYJX_VIDEO_PARSE_LIVE_GROUP=280183116 \
//!   cargo test --bin ayjx live_takes_the_video -- --ignored --nocapture
//! ```
//!
//! 「发出去了没有」看实现端记下来的消息（`message.list`），不看 ayjx 的日志：
//! `[Chat] 发送 ->` 只说明打算发，成功失败它不知道。

use super::*;
use crate::adapters::satori::SatoriClient;
use crate::config::AppConfig;
use crate::event::{BotStatus, EventType, LoginUser};
use crate::matcher::Matcher;
use crate::scheduler::Scheduler;
use sea_orm::Database;
use serde_json::json;
use std::sync::{Arc, RwLock};
use tokio::sync::Mutex as AsyncMutex;

/// 最省事的一条样品：3 分 33 秒，720P/360P 两档，360P 约 10 MB。
const SAMPLE: &str = "https://www.bilibili.com/video/BV1GJ411x7h7";
const SAMPLE_CAP: u64 = 20 * 1_048_576;

/// 拉一次样品的信息，并取 360P 档——真机上跑的是手机网络，一条自检别下几十兆。
async fn sample() -> (bilibili::Video, bilibili::Streams) {
    let reference = bilibili::reference(&Url::parse(SAMPLE).unwrap()).unwrap();
    let video = bilibili::info(&reference, "", API_TIMEOUT)
        .await
        .expect("拉不到稿件信息");
    let streams = bilibili::plan(&video.bvid, video.cid, 16, SAMPLE_CAP, "", API_TIMEOUT)
        .await
        .expect("取不到 360P 的流");
    (video, streams)
}

#[tokio::test]
#[ignore = "真连 B 站：拉稿件信息与取流地址，不发群消息"]
async fn live_reads_the_metadata_and_the_stream_plan() {
    let (video, streams) = sample().await;
    println!(
        "{} / {} / UP {} / {}s / {} / {:.1} MB / {} 段",
        video.bvid,
        video.title,
        video.owner,
        video.duration,
        bilibili::quality_label(streams.quality),
        streams.size as f64 / 1_048_576.0,
        streams.urls.len(),
    );

    assert_eq!(video.bvid, "BV1GJ411x7h7");
    assert!(video.title.contains("Never Gonna Give You Up"), "{}", video.title);
    assert!(video.cover.is_some(), "封面没拿到");
    assert!(streams.size > 0 && !streams.urls.is_empty());

    // 分享短链要真的跟过去才知道落到哪个页面。
    let shared = resolve_link("https://b23.tv/BV1GJ411x7h7").await.unwrap();
    assert_eq!(
        bilibili::reference(&shared).unwrap().bvid.as_deref(),
        Some("BV1GJ411x7h7"),
        "短链没跳到稿件页：{shared}"
    );

    // 上限比最小一档还小时，报的是「最小的一档也有多大」，而不是别的错。
    let error = bilibili::plan(&video.bvid, video.cid, 16, 1024, "", API_TIMEOUT)
        .await
        .expect_err("1024 字节的上限不该挑得出画质");
    assert!(error.to_string().contains("也有"), "{error}");
}

#[tokio::test]
#[ignore = "AYJX_VIDEO_PARSE_LIVE_GROUP=280183116；会真的往沙盒群发一条视频（正文 + 群文件 + 气泡）并撤回"]
async fn live_takes_the_video_into_the_sandbox_group() {
    let group: i64 = std::env::var("AYJX_VIDEO_PARSE_LIVE_GROUP")
        .ok()
        .and_then(|value| value.parse().ok())
        .expect("先给 AYJX_VIDEO_PARSE_LIVE_GROUP=<沙盒群号>");
    let (ctx, writer) = live_context().await;
    let (video, streams) = sample().await;

    let dir = get_data_dir("video_parse").await.unwrap();
    let path = dir.join("live-test.part");
    let size = download(&streams.urls, &path, SAMPLE_CAP, Duration::from_secs(300))
        .await
        .expect("下载失败");
    assert_eq!(size, streams.size, "落盘的字节数与站点报的对不上");

    // 真机上是用户引用预览再回复，这里只要引用段指向一条真实存在的消息。
    let quoted: serde_json::Value = writer
        .call(
            &ctx,
            "message.create",
            json!({"channel_id": group.to_string(), "content": "视频解析自检"}),
        )
        .await
        .unwrap();
    let quote_id: i64 = quoted[0]["id"].as_str().unwrap().parse().unwrap();

    let preview = state::Preview {
        target_id: group,
        message_id: quote_id.to_string(),
        created_ts: chrono::Utc::now().timestamp(),
        url: SAMPLE.to_string(),
        bvid: video.bvid.clone(),
        cid: video.cid,
        page: video.page,
        title: video.title.clone(),
        duration: video.duration,
        extracted: false,
    };
    let config = Config::default();
    let result = deliver(
        &ctx,
        &writer,
        &config,
        &preview,
        &streams,
        size,
        &path,
        Some(group),
        0,
        quote_id,
    )
    .await;
    let _ = tokio::fs::remove_file(&path).await;
    result.expect("取片成品没发出去");

    // 群里到底有没有：看实现端记下来的内容，不看日志。
    let listed: serde_json::Value = writer
        .call(&ctx, "message.list", json!({"channel_id": group.to_string()}))
        .await
        .unwrap();
    let messages = listed["data"].as_array().cloned().unwrap_or_default();
    let find = |marker: &str| {
        messages
            .iter()
            .find(|message| {
                message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains(marker))
            })
            .map(|message| {
                println!("{}", message["content"].as_str().unwrap_or_default());
                message["id"].as_str().unwrap_or_default().to_string()
            })
    };
    let caption = find(" MB");
    let file = find("<file");
    let bubble = find("<video");
    assert!(caption.is_some(), "正文没到群里");
    assert!(file.is_some(), "群文件没到群里");
    assert!(bubble.is_some(), "视频气泡没到群里");

    // 收工：这次发出去的连同引用目标一起撤回。
    for id in [Some(quote_id.to_string()), caption, file, bubble]
        .into_iter()
        .flatten()
    {
        let deleted: serde_json::Value = writer
            .call(
                &ctx,
                "message.delete",
                json!({"channel_id": group.to_string(), "message_id": id}),
            )
            .await
            .unwrap();
        println!("撤回 {id}: {deleted}");
    }
}

#[tokio::test]
#[ignore = "AYJX_VIDEO_PARSE_LIVE_GROUP=280183116；会真的往沙盒群发一条预览并撤回"]
async fn live_sends_the_preview_into_the_sandbox_group() {
    let group: i64 = std::env::var("AYJX_VIDEO_PARSE_LIVE_GROUP")
        .ok()
        .and_then(|value| value.parse().ok())
        .expect("先给 AYJX_VIDEO_PARSE_LIVE_GROUP=<沙盒群号>");
    let (ctx, writer) = live_context().await;

    // 真机上是用户发的那条链接消息，这里造一条真实的，引用段要指着它。
    let trigger: serde_json::Value = writer
        .call(
            &ctx,
            "message.create",
            json!({"channel_id": group.to_string(), "content": SAMPLE}),
        )
        .await
        .unwrap();
    let trigger_id: i64 = trigger[0]["id"].as_str().unwrap().parse().unwrap();

    let config = Config::default();
    let record = preview(
        &ctx,
        &writer,
        &config,
        SAMPLE,
        Some(group),
        0,
        trigger_id,
    )
    .await
    .expect("预览没发出去");
    println!("预览消息 ID：{}", record.message_id);

    // 引用取片靠的就是这条对应关系；记不下就等于取不到片。
    let claimed = state::claim(group, &record.message_id).await;
    assert!(
        matches!(claimed, state::Claim::Ready(_)),
        "预览发出去了却没有记下对应关系"
    );
    state::release(group, &record.message_id).await;

    // 群里那一条的完整元素。`message.list` 回的是文本快照，认不出图片，
    // 要看封面得用 `message.get`。
    let sent: serde_json::Value = writer
        .call(
            &ctx,
            "message.get",
            json!({"channel_id": group.to_string(), "message_id": record.message_id}),
        )
        .await
        .unwrap();
    let content = sent["content"].as_str().unwrap_or_default().to_string();
    println!("{content}");
    assert!(content.contains("索尼音乐中国"), "预览里没有 UP 主");
    assert!(content.contains(HINT_LINE), "预览里没有取片提示");
    assert!(
        content.contains(&format!("<quote id=\"{trigger_id}\"/>")),
        "预览没有引用用户那条链接"
    );
    // 封面是让实现端自己去 hdslb 取的：落到 asset 路由上说明它真的取到了。
    assert!(content.contains("<img src="), "预览里没有封面");

    let _: serde_json::Value = writer
        .call(
            &ctx,
            "message.delete",
            json!({"channel_id": group.to_string(), "message_id": record.message_id}),
        )
        .await
        .unwrap();
    let _: serde_json::Value = writer
        .call(
            &ctx,
            "message.delete",
            json!({"channel_id": group.to_string(), "message_id": trigger_id.to_string()}),
        )
        .await
        .unwrap();
}

/// 群里发的是卡片（QQ 小程序卡 / 分享卡）时走通同一条路：从 `json` 段取落地地址、
/// 跟短链、拉稿件信息、回预览。载荷照真机收到的形状造，只有里面的 b23 短链换成
/// 样品那条——真卡片指向的稿件随时可能被删，自检不能靠它。
#[tokio::test]
#[ignore = "AYJX_VIDEO_PARSE_LIVE_GROUP=280183116；会真的往沙盒群发一条预览并撤回"]
async fn live_reads_a_card_from_the_sandbox_group() {
    let group: i64 = std::env::var("AYJX_VIDEO_PARSE_LIVE_GROUP")
        .ok()
        .and_then(|value| value.parse().ok())
        .expect("先给 AYJX_VIDEO_PARSE_LIVE_GROUP=<沙盒群号>");
    let (ctx, writer) = live_context().await;

    // 卡片必须引用一条真实存在的消息，预览才建立得起来。
    let trigger: serde_json::Value = writer
        .call(
            &ctx,
            "message.create",
            json!({"channel_id": group.to_string(), "content": "[分享]视频"}),
        )
        .await
        .unwrap();
    let trigger_id: i64 = trigger[0]["id"].as_str().unwrap().parse().unwrap();

    // 封面地址是不转义的（正则先撞上它），落地地址是转义的（正则撞不上）——
    // 所以这条测试只有在卡片地址真的被取到时才会通过。
    let payload = r#"{"ver":"1.0.0.19","prompt":"[QQ小程序]测试稿件","app":"com.tencent.miniapp_01",
        "meta":{"detail_1":{"title":"哔哩哔哩","appid":"1109937557",
        "icon":"http://miniapp.gtimg.cn/public/appicon/test.jpg",
        "preview":"https://qq.ugcimg.cn/v1/test",
        "url":"m.q.qq.com/a/s/test",
        "qqdocurl":"https:\/\/b23.tv\/BV1GJ411x7h7"}}}"#;
    let (ctx, writer) = card_context(ctx, writer, group, trigger_id, payload).await;

    let consumed = handle(ctx.clone(), writer.clone()).await.unwrap();
    assert!(consumed.is_none(), "卡片该由本插件吃掉");

    // 群里到底有没有：看实现端记下来的内容，不看日志。
    let listed: serde_json::Value = writer
        .call(&ctx, "message.list", json!({"channel_id": group.to_string()}))
        .await
        .unwrap();
    let messages = listed["data"].as_array().cloned().unwrap_or_default();
    let preview = messages
        .iter()
        .find(|message| {
            message["content"]
                .as_str()
                .is_some_and(|content| content.contains(HINT_LINE))
        })
        .expect("卡片没有换来预览");
    let content = preview["content"].as_str().unwrap_or_default().to_string();
    println!("{content}");
    assert!(content.contains("索尼音乐中国"), "预览里没有 UP 主");
    assert!(
        content.contains(&format!("<quote id=\"{trigger_id}\"/>")),
        "预览没有引用那条卡片"
    );

    // 收工：这次发出去的连同引用目标一起撤回。
    for id in [trigger_id.to_string(), preview["id"].as_str().unwrap().to_string()] {
        let _: serde_json::Value = writer
            .call(
                &ctx,
                "message.delete",
                json!({"channel_id": group.to_string(), "message_id": id}),
            )
            .await
            .unwrap();
    }
    state::release(group, preview["id"].as_str().unwrap()).await;
}

/// 把[`live_context`]给的上下文换成「一条卡片消息」：正文为空，`json` 段带着载荷，
/// `raw_message` 是与实现端一致的 CQ 形态（`[CQ:json,data=…]`）。
async fn card_context(
    mut ctx: Context,
    writer: LockedWriter,
    group: i64,
    message_id: i64,
    payload: &str,
) -> (Context, LockedWriter) {
    ctx.event = EventType::Satori(
        simd_json::serde::to_owned_value(json!({
            "post_type": "message",
            "satori_type": "message-created",
            "message_type": "group",
            "group_id": group,
            "user_id": 1,
            "message_id": message_id,
            "raw_message": format!("[CQ:json,data={payload}]"),
            "sender": {"nickname": "自检", "role": "member"},
            "message": [{"type": "json", "data": {"data": payload}}]
        }))
        .unwrap(),
    );
    (ctx, writer)
}

/// 一个够用的 Context：配置、数据库与登录账号，事件本身用不上。
async fn live_context() -> (Context, LockedWriter) {    let db = Database::connect("sqlite::memory:").await.unwrap();
    let text = tokio::fs::read_to_string("config.toml")
        .await
        .expect("要在仓库根目录跑");
    let disk: toml::Value = toml::from_str(&text).unwrap();
    let connection = disk["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|bot| bot["protocol"].as_str() == Some("satori"))
        .unwrap();
    let endpoint = connection["url"].as_str().unwrap().trim_end_matches('/');
    let token = std::env::var("AYJX_SATORI_TOKEN").ok().or_else(|| {
        connection
            .get("access_token")
            .and_then(toml::Value::as_str)
            .map(str::to_string)
    });

    let mut app = AppConfig::default();
    for plugin in crate::plugins::get_plugins() {
        app.plugins
            .insert(plugin.name.to_string(), (plugin.default_config)());
    }
    let ctx = Context {
        event: EventType::Satori(
            simd_json::serde::to_owned_value(json!({
                "post_type": "message",
                "satori_type": "message-created",
                "message_type": "group",
                "group_id": 0,
                "user_id": 1,
                "message_id": 1,
                "sender": {"nickname": "自检", "role": "member"},
                "message": [{"type": "text", "data": {"text": "自检"}}]
            }))
            .unwrap(),
        ),
        config: Arc::new(RwLock::new(app)),
        config_save_lock: Arc::new(AsyncMutex::new(())),
        db,
        scheduler: Arc::new(Scheduler::new()),
        matcher: Arc::new(Matcher::new()),
        config_path: Arc::from("config.toml"),
        bot: Arc::new(BotStatus {
            adapter: "satori-qq".to_string(),
            platform: "red".to_string(),
            login_user: LoginUser::default().into(),
        }),
    };
    let writer: LockedWriter = Arc::new(SatoriClient::new(endpoint.to_string(), token));
    // 出站选择器要跟实现端当前认定的登录一致，否则请求会被判成别的账号。
    let login: serde_json::Value = writer.call(&ctx, "login.get", json!({})).await.unwrap();
    ctx.bot.login_user.set(LoginUser {
        id: login["user"]["id"].as_str().unwrap_or_default().to_string(),
        name: login["user"]["name"].as_str().map(str::to_string),
        ..Default::default()
    });
    (ctx, writer)
}
