//! 取片的真机验证。
//!
//! 第一条只连 B 站拉稿件信息与取流地址，不碰群；后两条要显式给沙盒群号，
//! 会真的往群里发一条视频（外加一份群文件）再撤回。
//!
//! ```sh
//! cargo test --bin acumen live_reads_the_metadata -- --ignored --nocapture
//! ACUMEN_VIDEO_PARSE_LIVE_GROUP=280183116 \
//!   cargo test --bin acumen live_takes_ -- --ignored --nocapture
//! ```
//!
//! 「发出去了没有」看实现端记下来的消息（`message.list`），不看 acumen 的日志：
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
const SAMPLE_BVID: &str = "BV1GJ411x7h7";
const SAMPLE_CAP: u64 = 20 * 1_048_576;

/// 自检用的是同一条样品、同一个贴链接的人（`user_id = 1`），而线上要的就是
/// 「同一个人十分钟内重贴同一条不取第二遍」——不清掉上一轮的名额，第二次跑会被
/// 自己的去重挡住。这条只清自检自己那一份，不动别的会话。
async fn forget_previous_take(group: i64) {
    state::release(group, 1, SAMPLE_BVID).await;
}

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
        "{} / {} / P{}/{} / {} / {:.1} MB / {} 段",
        video.bvid,
        video.title,
        video.page,
        video.pages,
        bilibili::quality_label(streams.quality),
        streams.size as f64 / 1_048_576.0,
        streams.urls.len(),
    );

    assert_eq!(video.bvid, "BV1GJ411x7h7");
    assert!(
        video.title.contains("Never Gonna Give You Up"),
        "{}",
        video.title
    );
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

/// 群友在群里贴一条链接，片子就该直接进群——不用引用，也不用再说一个词。
///
/// 这条会往群里发三四条（触发那条、成品那条气泡与文件），模块的
/// 出站闸门是全局 20 条/分钟，别把它跟别的沙盒用例挤在同一分钟里跑。
#[tokio::test]
#[ignore = "ACUMEN_VIDEO_PARSE_LIVE_GROUP=280183116；会真的往沙盒群发一条视频（气泡 + 群文件）并撤回"]
async fn live_takes_a_link_from_the_sandbox_group() {
    let group: i64 = std::env::var("ACUMEN_VIDEO_PARSE_LIVE_GROUP")
        .ok()
        .and_then(|value| value.parse().ok())
        .expect("先给 ACUMEN_VIDEO_PARSE_LIVE_GROUP=<沙盒群号>");
    let (ctx, writer) = live_context().await;
    forget_previous_take(group).await;
    // 真机上这条链接是群友发的，这里造一条真实存在的。
    let trigger = post(&ctx, &writer, group, SAMPLE).await;
    let (ctx, writer) = link_context(ctx, writer, group, trigger).await;
    // 只为自检省流量：360P、20 MB 上限；两条腿都验，所以发法显式开成 both
    // （线上默认只发气泡）。
    cheaper_take(&ctx);

    let consumed = handle(ctx.clone(), writer.clone()).await.unwrap();
    assert!(consumed.is_none(), "视频站链接该由本插件吃掉");

    // 群里到底有没有：看实现端记下来的内容，不看日志。成品**不带引用**（带引用时
    // 视频不显示，见 `send`），所以用「比触发那条新」认这一轮的成品。
    let listed = recent_messages(&ctx, &writer, group).await;
    let bubble = find_sent(&listed, "<video", trigger);
    let file = find_sent(&listed, "<file", trigger);
    assert!(bubble.is_some(), "视频气泡没到群里");
    assert!(file.is_some(), "群文件没到群里");
    // 成品里不该出现引用段：那正是视频显示不出来的成因。只看这一轮发出去的——
    // 群里可能有从前的成品留在那儿。
    for message in &listed {
        let newer = message["id"]
            .as_str()
            .and_then(|id| id.parse::<i64>().ok())
            .is_some_and(|id| id > trigger);
        let content = message["content"].as_str().unwrap_or_default();
        assert!(
            !(newer && content.contains("<quote") && content.contains("<video")),
            "成品带上了引用，视频会显示不出来：{content}"
        );
    }

    // 收工：这次发出去的连同触发那条一起撤回。
    let ids = [Some(trigger.to_string()), bubble, file];
    for id in ids.into_iter().flatten() {
        recall(&ctx, &writer, group, &id).await;
    }
}

/// 群里发的是卡片（QQ 小程序卡 / 分享卡）时走通同一条路：从 `json` 段取落地地址、
/// 跟短链、拉稿件信息、把原片发进群。载荷照真机收到的形状造，只有里面的 b23 短链换成
/// 样品那条——真卡片指向的稿件随时可能被删，自检不能靠它。
#[tokio::test]
#[ignore = "ACUMEN_VIDEO_PARSE_LIVE_GROUP=280183116；会真的往沙盒群发一条视频（气泡 + 群文件）并撤回"]
async fn live_reads_a_card_from_the_sandbox_group() {
    let group: i64 = std::env::var("ACUMEN_VIDEO_PARSE_LIVE_GROUP")
        .ok()
        .and_then(|value| value.parse().ok())
        .expect("先给 ACUMEN_VIDEO_PARSE_LIVE_GROUP=<沙盒群号>");
    let (ctx, writer) = live_context().await;
    forget_previous_take(group).await;
    let trigger = post(&ctx, &writer, group, "[分享]视频").await;
    cheaper_take(&ctx);

    // 封面地址是不转义的（正则先撞上它），落地地址是转义的（正则撞不上）——
    // 所以这条只有在卡片地址真的被取到时才会通过。
    let payload = r#"{"ver":"1.0.0.19","prompt":"[QQ小程序]测试稿件","app":"com.tencent.miniapp_01",
        "meta":{"detail_1":{"title":"哔哩哔哩","appid":"1109937557",
        "icon":"http://miniapp.gtimg.cn/public/appicon/test.jpg",
        "preview":"https://qq.ugcimg.cn/v1/test",
        "url":"m.q.qq.com/a/s/test",
        "qqdocurl":"https:\/\/b23.tv\/BV1GJ411x7h7"}}}"#;
    let (ctx, writer) = card_context(ctx, writer, group, trigger, payload).await;

    let consumed = handle(ctx.clone(), writer.clone()).await.unwrap();
    assert!(consumed.is_none(), "卡片该由本插件吃掉");

    let listed = recent_messages(&ctx, &writer, group).await;
    let bubble = find_sent(&listed, "<video", trigger);
    assert!(bubble.is_some(), "卡片没有换来原片");

    // 收工：这次发出去的连同触发那条一起撤回。
    let ids = [
        Some(trigger.to_string()),
        bubble,
        find_sent(&listed, "<file", trigger),
    ];
    for id in ids.into_iter().flatten() {
        recall(&ctx, &writer, group, &id).await;
    }
}

/// 撤回一条自检消息。
///
/// 撤回失败只打印，不带红这条用例：QQ 偶尔会对一条刚发出去的富媒体回 `code=5`，
/// 而这条用例要证明的是「片子到群里了没有」，不是「撤回一定成功」。
async fn recall(ctx: &Context, writer: &LockedWriter, group: i64, id: &str) {
    let body = json!({"channel_id": group.to_string(), "message_id": id});
    let deleted: Result<serde_json::Value, _> = writer.call(ctx, "message.delete", body).await;
    match deleted {
        Ok(result) => println!("撤回 {id}: {result}"),
        Err(error) => println!("撤回 {id} 失败：{error}"),
    }
}

/// 最近的消息，一路跟着 `next` 往回翻几页。
///
/// `message.list` 的第一页不保证含刚发出去的那几条：它按 seq 分页，实测过第一页只有
/// 最新一条、成品都落在下一页。
async fn recent_messages(
    ctx: &Context,
    writer: &LockedWriter,
    group: i64,
) -> Vec<serde_json::Value> {
    const PAGES: usize = 4;
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..PAGES {
        let mut body = json!({"channel_id": group.to_string()});
        if let Some(next) = &cursor {
            body["next"] = json!(next);
        }
        let listed: serde_json::Value = writer.call(ctx, "message.list", body).await.unwrap();
        out.extend(listed["data"].as_array().cloned().unwrap_or_default());
        match listed["next"].as_str() {
            Some(next) if !next.is_empty() => cursor = Some(next.to_string()),
            _ => break,
        }
    }
    out
}

/// 在这批消息里找带某个记号、且比 `after` 新的那一条，回它的 ID。
///
/// 成品不带引用，没法靠「引用的是这一轮的触发消息」认出自己那条；消息 ID 是递增的，
/// 比触发那条大就一定是这一轮发出去的（上一轮留下的成品比它小）。
fn find_sent(messages: &[serde_json::Value], marker: &str, after: i64) -> Option<String> {
    messages
        .iter()
        .filter(|message| {
            message["id"]
                .as_str()
                .and_then(|id| id.parse::<i64>().ok())
                .is_some_and(|id| id > after)
        })
        .find(|message| {
            message["content"]
                .as_str()
                .is_some_and(|content| content.contains(marker))
        })
        .map(|message| {
            println!("{}", message["content"].as_str().unwrap_or_default());
            message["id"].as_str().unwrap_or_default().to_string()
        })
}

/// 往沙盒群发一条真实存在的消息，拿它的 ID。
async fn post(ctx: &Context, writer: &LockedWriter, group: i64, content: &str) -> i64 {
    let sent: serde_json::Value = writer
        .call(
            ctx,
            "message.create",
            json!({"channel_id": group.to_string(), "content": content}),
        )
        .await
        .unwrap();
    sent[0]["id"].as_str().unwrap().parse().unwrap()
}

/// 自检用的取片参数：360P、20 MB 上限、两条腿都发（`Config::default()` 挑的是
/// 720P 与只发气泡，跟着线上走就要下几十兆，还验不到群文件那条腿）。
fn cheaper_take(ctx: &Context) {
    let mut config = ctx.config.write().unwrap();
    let Some(value) = config.plugins.get_mut("video_parse") else {
        panic!("插件配置不在");
    };
    value["prefer_quality"] = toml::Value::Integer(16);
    value["max_size_mb"] = toml::Value::Integer(20);
    value["send"] = toml::Value::String("both".to_string());
}

/// 把[`live_context`]给的上下文换成「群友贴了一条链接」的那条消息。
async fn link_context(
    mut ctx: Context,
    writer: LockedWriter,
    group: i64,
    message_id: i64,
) -> (Context, LockedWriter) {
    ctx.event = EventType::Satori(
        simd_json::serde::to_owned_value(json!({
            "post_type": "message",
            "satori_type": "message-created",
            "message_type": "group",
            "group_id": group,
            "user_id": 1,
            "message_id": message_id,
            "raw_message": SAMPLE,
            "sender": {"nickname": "自检", "role": "member"},
            "message": [{"type": "text", "data": {"text": SAMPLE}}]
        }))
        .unwrap(),
    );
    (ctx, writer)
}

/// 同上，换成「一条卡片消息」：正文为空，`json` 段带着载荷，
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
async fn live_context() -> (Context, LockedWriter) {
    let db = Database::connect("sqlite::memory:").await.unwrap();
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
    let token = std::env::var("ACUMEN_SATORI_TOKEN").ok().or_else(|| {
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
