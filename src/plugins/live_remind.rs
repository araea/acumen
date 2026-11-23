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
//!
//! 轮询任务注册在 `on_init`：这个钩子在插件「运行时被启用」时会被框架补跑一次
//! （`ctl on` / 控制台开关都走这条路），所以开着开关改完配置、或半路才打开插件都能立刻
//! 轮询，不必重启。发送不靠连接建立时抓来的 writer，而是在要发时从已连上的账号里现取
//! 一条线路（与潜水清理后台任务同法），于是运行时启用的插件也发得出去。
//!
//! 控制台有一块专用面板（`GET /api/live_remind` 等），按群管理主播、看开播状态、试发、
//! 改文案都在那里；本模块导出 [`panel_data`]、[`probe_room`]、[`snapshot`]、[`send_to`]
//! 供它调用。

use crate::adapters::satori::{LockedWriter, send_msg};
use crate::event::Context;
use crate::plugins::{PluginConfig, PluginError, get_config_or_default};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
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

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Target {
    /// 群号。
    group: String,
    streamers: Vec<Streamer>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
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

impl PluginConfig for Config {
    const NAME: &'static str = "live_remind";

    fn check(&self) -> Result<(), String> {
        for target in &self.targets {
            if target.group.trim().is_empty() {
                return Err("开播提醒：每个推送目标都要填群号".into());
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

/// 后台插件，没有群指令；开关走 `ctl on/off live_remind` 或控制台开关。
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
    for value in &resp.headers().get_all(reqwest::header::SET_COOKIE) {
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

/// 一次房间查询拿到的全部信息：在播状态、主播昵称、直播标题。
/// 控制台「检查开播状态 / 识别主播」用得到，轮询只取 `live`。
#[derive(Debug, Clone)]
pub struct RoomInfo {
    /// 是否正在直播。
    pub live: bool,
    /// 抖音上的主播昵称（用来自动填写提醒里的名字）。
    pub nickname: Option<String>,
    /// 当前直播标题。
    pub title: Option<String>,
}

/// 查一个直播间。`browser_*` 那几个参数不能少，少了抖音会回一个 200 空串。
/// 失败（风控 / 空响应 / 坏 JSON）时清掉 ttwid 重取，最多两次。
pub async fn probe_room(rid: &str) -> Result<RoomInfo, PluginError> {
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
        let Ok(json) = serde_json::from_slice::<Value>(&bytes) else {
            clear_ttwid();
            continue;
        };
        if json.get("status_code").and_then(Value::as_i64) != Some(0) {
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
        let live = room.get("status").and_then(Value::as_i64) == Some(2);
        let nickname = json
            .pointer("/data/user/nickname")
            .and_then(|v| v.as_str())
            .map(String::from);
        let title = room
            .get("title")
            .and_then(|v| v.as_str())
            .map(String::from);
        return Ok(RoomInfo {
            live,
            nickname,
            title,
        });
    }
    Err(format!("直播间 {rid} 的状态查询连续两次失败").into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Live {
    Yes,
    No,
}

async fn query_live(rid: &str) -> Result<Live, PluginError> {
    Ok(if probe_room(rid).await?.live {
        Live::Yes
    } else {
        Live::No
    })
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

/// 后台任务没有事件可回，借现成的一条登录线路发消息（与潜水清理同法）。
fn link(ctx: &Context) -> Option<(Context, LockedWriter)> {
    let (bot, writer) = crate::adapters::satori::logins().into_iter().next()?;
    let mut send_ctx = ctx.clone();
    send_ctx.bot = bot;
    Some((send_ctx, writer))
}

/// 往指定群发一条提醒。轮询与控制台「试发」共用，出去前统一过发送流水线与路由。
pub async fn send_to(ctx: &Context, group: &str, text: String) -> Result<(), PluginError> {
    let Some((send_ctx, writer)) = link(ctx) else {
        return Err("还没有连上的账号，提醒暂时发不出去".into());
    };
    send_msg(&send_ctx, writer, Some(group), None, text).await
}

/// 在 `on_init` 注册轮询任务，而不是 `on_connected`：
/// 后者只在连接建立时对「当时已启用」的插件触发一次，运行时才打开的插件不会补跑；
/// 前者由框架在运行时启用时（`ctl on` / 控制台开关）补跑，改配置或半路启用都能立即生效。
pub fn on_init(ctx: Context) -> BoxFuture<'static, Result<(), PluginError>> {
    Box::pin(async move {
        if SCHEDULED.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let config = load_config(&ctx);
        info!(
            target: LOG_TARGET,
            "开播提醒已就位：每 {} 秒查一次，推送目标 {} 个群",
            config.poll_seconds.max(MIN_POLL_SECONDS),
            config.targets.len()
        );

        let poll_ctx = ctx.clone();
        ctx.scheduler
            .clone()
            .add_interval(Duration::from_secs(TICK_SECONDS), move || {
                let ctx = poll_ctx.clone();
                async move { tick(ctx).await }
            });
        Ok(())
    })
}

async fn tick(ctx: Context) {
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

    // 先按「旧状态」算出提醒，再统一写回新状态。绝不能边遍历边写：两个群盯同一个
    // 主播时，第二个群会读到刚写进去的新状态，以为没变化，于是漏掉它那一份。
    let previous = {
        let mut state = states().lock().unwrap();
        let snapshot = state.clone();
        for (rid, &live) in &results {
            state.insert(rid.clone(), live);
        }
        snapshot
    };
    let notices = build_notices(&config, &results, &previous);

    for (group, text, name) in notices {
        if let Err(error) = send_to(&ctx, &group, text).await {
            warn!(
                target: LOG_TARGET,
                "向群 {} 推送「{}」失败：{}",
                group, name, error
            );
        }
    }
}

/// 依据「上次状态」与「本次结果」算出每个群该发的提醒。纯函数，便于单测：
/// 多个群盯同一个主播时，每个群都要拿到自己那份，状态只查一次就够了。
fn build_notices(
    config: &Config,
    results: &HashMap<String, Live>,
    previous: &HashMap<String, Live>,
) -> Vec<(String, String, String)> {
    struct Notice {
        group: String,
        text: String,
        name: String,
    }
    let mut notices = Vec::new();
    for target in &config.targets {
        for streamer in &target.streamers {
            let Some(&live) = results.get(&streamer.rid) else {
                continue;
            };
            let prev = previous.get(&streamer.rid).copied();
            // 第一次看到只记基线：刚上线时已经在播的不算「刚开播」。
            if prev.is_none() || prev == Some(live) {
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
    notices
        .into_iter()
        .map(|n| (n.group, n.text, n.name))
        .collect()
}

// ==================== 控制台接口 ====================

/// 控制台面板的读数：当前配置、默认值、上次轮询时间与各直播间上次查到的状态。
pub fn panel_data(ctx: &Context) -> Value {
    let config = load_config(ctx);
    let (last_poll, states) = snapshot();
    let defaults = Config::default();
    json!({
        "enabled": config.enabled,
        "poll_seconds": config.poll_seconds,
        "start_text": config.start_text,
        "end_text": config.end_text,
        "targets": config.targets,
        "min_poll_seconds": MIN_POLL_SECONDS,
        "tick_seconds": TICK_SECONDS,
        "defaults": {
            "poll_seconds": defaults.poll_seconds,
            "start_text": defaults.start_text,
            "end_text": defaults.end_text,
        },
        "sample_name": "大司马解说",
        "states": states,
        "last_poll": last_poll,
        "now": chrono::Utc::now().timestamp(),
    })
}

/// 每个直播间上次查到的状态，以及上次轮询时间。给面板画「现在的情况」用，不额外联网。
pub fn snapshot() -> (i64, Value) {
    let states = states().lock().unwrap();
    let map: serde_json::Map<String, Value> = states
        .iter()
        .map(|(rid, live)| {
            (
                rid.clone(),
                Value::from(if *live == Live::Yes { "live" } else { "off" }),
            )
        })
        .collect();
    (LAST_POLL.load(Ordering::Relaxed), Value::Object(map))
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

    /// 回归：两个群盯同一个主播，状态翻转时两个群都要拿到提醒（旧实现只发第一个群）。
    #[test]
    fn every_group_gets_its_own_notice_for_a_shared_streamer() {
        let streamer = Streamer {
            name: "大司马".into(),
            rid: "627".into(),
        };
        let config = Config {
            enabled: true,
            targets: vec![
                Target {
                    group: "群A".into(),
                    streamers: vec![streamer.clone()],
                },
                Target {
                    group: "群B".into(),
                    streamers: vec![streamer.clone()],
                },
            ],
            ..Config::default()
        };
        let mut previous = HashMap::new();
        previous.insert("627".to_string(), Live::No);
        let mut results = HashMap::new();
        results.insert("627".to_string(), Live::Yes);

        let notices = build_notices(&config, &results, &previous);
        let groups: Vec<&str> = notices.iter().map(|(group, _, _)| group.as_str()).collect();
        assert_eq!(groups, vec!["群A", "群B"]);
        assert!(notices
            .iter()
            .all(|(_, text, _)| text.contains("大司马") && text.contains("开播")));
    }

    /// 首次看到只记基线，不推送；状态没变也不推送。
    #[test]
    fn baseline_and_no_change_send_nothing() {
        let config = Config {
            enabled: true,
            targets: vec![Target {
                group: "群A".into(),
                streamers: vec![Streamer {
                    name: "大司马".into(),
                    rid: "627".into(),
                }],
            }],
            ..Config::default()
        };
        let mut results = HashMap::new();
        results.insert("627".to_string(), Live::Yes);
        assert!(build_notices(&config, &results, &HashMap::new()).is_empty());

        let mut previous = HashMap::new();
        previous.insert("627".to_string(), Live::Yes);
        assert!(build_notices(&config, &results, &previous).is_empty());
    }

    /// 真接口冒烟：默认不跑，`cargo test plugins::live_remind::tests::real_douyin_status -- --ignored --nocapture`。
    #[tokio::test]
    #[ignore = "需要访问抖音，手工跑"]
    async fn real_douyin_status() {
        for rid in ["627858614", "39719844720"] {
            let info = probe_room(rid).await.expect("查抖音直播状态失败");
            println!("直播间 {rid}：在播={} 主播={:?} 标题={:?}", info.live, info.nickname, info.title);
        }
    }
}
