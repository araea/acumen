//! 抖音开播提醒：盯着几个抖音主播，开播、下播各往指定群发一条短提醒。
//!
//! 只查直播状态，不取流、不登录、不进直播间。抖音网页端有个公开的房间状态接口
//! （`webcast/room/web/enter`），带上先访问一次直播间首页拿到的 `ttwid` cookie 即可，
//! 返回里 `data.data[0].status == 2` 是在播，4 是没播。参考了 DouyinLiveRecorder 的
//! 抖音状态查询；它为了取流还算 `a_bogus`，这里只认状态，实测不需要那层签名。
//!
//! 配置按群分：`[[live_remind.targets]]` 写一个群，群里挂要盯的主播（名字 + 直播间号）。
//! 直播间号就是 `live.douyin.com/<数字>` 里的数字。首次轮询只记基线不推送，
//! 免得刚上线时把已经在播的当成「刚开播」；之后只在状态真的翻转时发一条。
//!
//! 文案只有 `{name}` 一个占位符，默认就是「开播了 / 下播了」一句，纯提醒、不 @ 人、
//! 不刷屏；两个群盯同一个主播时各自按自己的文案发，状态只查一次。

use crate::adapters::satori::{LockedWriter, send_msg};
use crate::event::Context;
use crate::plugins::{PluginConfig, PluginError, get_config_or_default};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const LOG_TARGET: &str = "Plugin/LiveRemind";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// 调度器的最小检查粒度。真正的查询间隔由配置决定，在 `tick` 里用时间戳判断，
/// 这样改 `poll_seconds` 不必重启。
const TICK_SECONDS: u64 = 15;
/// 再密也没意义：主播开播的粒度就是分钟级，别给抖音添麻烦。
const MIN_POLL_SECONDS: u64 = 30;

// ==================== 配置 ====================

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    enabled: bool,
    /// 多久查一次（秒，最小 30）。
    poll_seconds: u64,
    /// 要推送的群，每个群各自挂要盯的主播。
    targets: Vec<Target>,
    /// 开播文案；可用 {name}。
    start_text: String,
    /// 下播文案；可用 {name}。
    end_text: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Target {
    /// 群号。
    group: String,
    streamers: Vec<Streamer>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Streamer {
    /// 提醒里显示的名字，随便写，比如「大司马解说」。
    name: String,
    /// 直播间号，`live.douyin.com/<rid>` 里的那串数字。
    rid: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            poll_seconds: 60,
            targets: Vec::new(),
            start_text: "🔴 {name} 开播了".into(),
            end_text: "⚪ {name} 下播了".into(),
        }
    }
}

impl Default for Target {
    fn default() -> Self {
        Self {
            group: String::new(),
            streamers: Vec::new(),
        }
    }
}

impl Default for Streamer {
    fn default() -> Self {
        Self {
            name: String::new(),
            rid: String::new(),
        }
    }
}

impl PluginConfig for Config {
    const NAME: &'static str = "live_remind";

    fn check(&self) -> Result<(), String> {
        for target in &self.targets {
            if target.group.trim().is_empty() {
                return Err("开播提醒：targets 里的群号不能留空".into());
            }
            for streamer in &target.streamers {
                if streamer.rid.trim().is_empty() {
                    return Err(format!("开播提醒：{} 的直播间号不能留空", streamer.name));
                }
                if !streamer.rid.chars().all(|c| c.is_ascii_digit()) {
                    return Err(format!(
                        "开播提醒：直播间号只能是数字（{} = {}）",
                        streamer.name, streamer.rid
                    ));
                }
            }
        }
        Ok(())
    }
}

fn load_config(ctx: &Context) -> Config {
    get_config_or_default::<Config>(ctx)
}

fn render(template: &str, name: &str) -> String {
    template.replace("{name}", name)
}

/// 后台插件，没有群指令；开关走 `ctl on/off live_remind`。
pub fn handle(
    ctx: Context,
    _writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async { Ok(Some(ctx)) })
}

// ==================== 抖音直播状态 ====================

/// 访问直播间首页拿到的 ttwid。抖音的风控会让它失效，失败或空响应时清掉重取。
static TTWID: Mutex<Option<String>> = Mutex::new(None);

async fn refresh_ttwid() -> Result<String, PluginError> {
    let resp = crate::http::client()
        .get("https://live.douyin.com/")
        .header(reqwest::header::USER_AGENT, UA)
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9")
        .send()
        .await?;
    let mut found = None;
    for value in resp.headers().get_all(reqwest::header::SET_COOKIE).iter() {
        let Ok(header) = value.to_str() else {
            continue;
        };
        for cookie in header.split(';') {
            if let Some((key, value)) = cookie.trim().split_once('=')
                && key == "ttwid"
            {
                found = Some(value.to_string());
            }
        }
    }
    found.ok_or_else(|| "抖音首页未返回 ttwid".into())
}

fn clear_ttwid() {
    *TTWID.lock().unwrap() = None;
}

async fn current_ttwid() -> Result<String, PluginError> {
    if let Some(value) = TTWID.lock().unwrap().clone() {
        return Ok(value);
    }
    let value = refresh_ttwid().await?;
    *TTWID.lock().unwrap() = Some(value.clone());
    Ok(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Live {
    Yes,
    No,
}

/// 查一个直播间。`browser_*` 那几个参数不能少，少了抖音会回一个 200 空串。
async fn query_live(rid: &str) -> Result<Live, PluginError> {
    for _ in 0..2 {
        let cookie = current_ttwid().await?;
        let url = format!(
            "https://live.douyin.com/webcast/room/web/enter/?aid=6383&app_name=douyin_web&live_id=1&device_platform=web&language=zh-CN&browser_language=zh-CN&browser_platform=Win32&browser_name=Chrome&browser_version=120.0.0.0&enter_from=web_page&web_rid={rid}"
        );
        let resp = crate::http::client()
            .get(&url)
            .header(reqwest::header::USER_AGENT, UA)
            .header(reqwest::header::REFERER, "https://live.douyin.com/")
            .header(reqwest::header::COOKIE, format!("ttwid={cookie}"))
            .send()
            .await?;
        let bytes = resp.error_for_status()?.bytes().await?;
        if bytes.is_empty() {
            clear_ttwid();
            continue;
        }
        let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            clear_ttwid();
            continue;
        };
        if json.get("status_code").and_then(|v| v.as_i64()) != Some(0) {
            clear_ttwid();
            continue;
        }
        let Some(room) = json
            .get("data")
            .and_then(|d| d.get("data"))
            .and_then(|rooms| rooms.get(0))
        else {
            return Err(format!("直播间 {rid} 没有返回房间数据").into());
        };
        let status = room.get("status").and_then(|v| v.as_i64()).unwrap_or(0);
        return Ok(if status == 2 { Live::Yes } else { Live::No });
    }
    Err(format!("直播间 {rid} 的状态查询连续两次失败").into())
}

// ==================== 状态与轮询 ====================

/// 每个直播间上一次的状态。键是直播间号，所以两个群盯同一个人也只记一份。
static STATES: OnceLock<Mutex<HashMap<String, Live>>> = OnceLock::new();

fn states() -> &'static Mutex<HashMap<String, Live>> {
    STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

static SCHEDULED: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);
static LAST_POLL: AtomicI64 = AtomicI64::new(0);

struct RunGuard;

impl Drop for RunGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

pub fn on_connected(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        if SCHEDULED.swap(true, Ordering::SeqCst) {
            return Ok(Some(ctx));
        }
        let config = load_config(&ctx);
        info!(
            target: LOG_TARGET,
            "开播提醒已就位：每 {} 秒查一次，推送目标 {} 个群",
            config.poll_seconds.max(MIN_POLL_SECONDS),
            config.targets.len()
        );

        let poll_ctx = ctx.clone();
        let poll_writer = writer.clone();
        ctx.scheduler
            .clone()
            .add_interval(Duration::from_secs(TICK_SECONDS), move || {
                let ctx = poll_ctx.clone();
                let writer = poll_writer.clone();
                async move { tick(ctx, writer).await }
            });
        Ok(Some(ctx))
    })
}

async fn tick(ctx: Context, writer: LockedWriter) {
    let config = load_config(&ctx);
    if !config.enabled || config.targets.is_empty() {
        return;
    }

    let now = chrono::Utc::now().timestamp();
    if now - LAST_POLL.load(Ordering::Relaxed) < config.poll_seconds.max(MIN_POLL_SECONDS) as i64 {
        return;
    }
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let _guard = RunGuard;
    LAST_POLL.store(now, Ordering::Relaxed);

    // 同一个 rid 只查一次，多个群共用结果。
    let mut results: HashMap<String, Live> = HashMap::new();
    for target in &config.targets {
        for streamer in &target.streamers {
            if results.contains_key(&streamer.rid) {
                continue;
            }
            match query_live(&streamer.rid).await {
                Ok(live) => {
                    results.insert(streamer.rid.clone(), live);
                }
                Err(error) => {
                    warn!(
                        target: LOG_TARGET,
                        "查询「{}」({}) 失败：{}",
                        streamer.name, streamer.rid, error
                    );
                }
            }
        }
    }
    if results.is_empty() {
        return;
    }

    struct Notice {
        group: String,
        text: String,
        name: String,
    }
    let mut notices = Vec::new();
    {
        let mut state = states().lock().unwrap();
        for target in &config.targets {
            for streamer in &target.streamers {
                let Some(&live) = results.get(&streamer.rid) else {
                    continue;
                };
                let previous = state.insert(streamer.rid.clone(), live);
                // 第一次看到只记基线：刚上线时已经在播的不算「刚开播」。
                if previous.is_none() || previous == Some(live) {
                    continue;
                }
                let template = if live == Live::Yes {
                    &config.start_text
                } else {
                    &config.end_text
                };
                let text = render(template, &streamer.name);
                if !text.trim().is_empty() {
                    notices.push(Notice {
                        group: target.group.clone(),
                        text,
                        name: streamer.name.clone(),
                    });
                }
            }
        }
    }

    for notice in notices {
        if let Err(error) = send_msg(
            &ctx,
            writer.clone(),
            Some(&notice.group),
            None,
            notice.text,
        )
        .await
        {
            warn!(
                target: LOG_TARGET,
                "向群 {} 推送「{}」失败：{}",
                notice.group, notice.name, error
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_text_replaces_name() {
        assert_eq!(render("🔴 {name} 开播了", "大司马解说"), "🔴 大司马解说 开播了");
        assert_eq!(render("", "大司马"), "");
    }

    #[test]
    fn config_rejects_non_numeric_rid() {
        let config = Config {
            enabled: true,
            targets: vec![Target {
                group: "123".into(),
                streamers: vec![Streamer {
                    name: "主播".into(),
                    rid: "abc".into(),
                }],
            }],
            ..Config::default()
        };
        assert!(config.check().is_err());
    }

    #[test]
    fn status_code_two_means_live() {
        let json = serde_json::json!({ "data": { "data": [{ "status": 2 }] } });
        let room = json["data"]["data"][0].as_object().unwrap();
        assert_eq!(room["status"].as_i64(), Some(2));
    }

    /// 真接口冒烟：默认不跑，`cargo test plugins::live_remind::tests::real_douyin_status -- --ignored --nocapture`。
    #[tokio::test]
    #[ignore = "需要访问抖音，手工跑"]
    async fn real_douyin_status() {
        for rid in ["627858614", "39719844720"] {
            let live = query_live(rid).await.expect("查抖音直播状态失败");
            println!("直播间 {rid} 状态：{live:?}");
        }
    }
}
