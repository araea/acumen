//! 控制台的接口面：读的是进程里现成的东西，写的一律走 `ctl` 那条路。
//!
//! 每个写操作都落到 `ctl::change` / `ctl::set_value` / `ctl::execute` 上——校验、
//! 串行化、失败不改内存、原子替换这一套都在那里，控制台只是换了个触发器。
//! 于是「网页上改了配置」与「群里敲 /ctl」是同一件事的两种按键。

use super::state::Console;

use crate::adapters::satori::Roster;
use crate::plugins::{Plugin, get_plugins, pending_startup};
use crate::storage::write_atomic_async;
use axum::Json;
use axum::Router;
use axum::extract::{DefaultBodyLimit, Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use toml::Value as Toml;

pub(crate) fn routes(console: Arc<Console>) -> Router<Arc<Console>> {
    let router: Router<Arc<Console>> = Router::new()
        .route("/overview", get(overview))
        .route("/plugins", get(plugins))
        .route("/plugins/{name}", get(plugin_detail))
        .route("/plugins/{name}/enabled", post(set_enabled))
        .route("/plugins/{name}/config", post(set_config))
        .route("/plugins/{name}/config/insert", post(insert_config))
        .route("/plugins/{name}/config/remove", post(remove_config))
        .route("/plugins/{name}/reset", post(reset_config))
        .route("/plugins/{name}/test", post(test_plugin))
        // 本机图库：相册里选的配图从这里进来，页面再按缩略图取回。
        // 只有上传这一条放开 16 MB 的请求体上限，其余接口仍是默认的 2 MB。
        .route(
            "/plugins/{name}/asset",
            post(super::gallery::upload).layer(DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
        .route("/plugins/{name}/asset/{file}", get(super::gallery::file))
        .route("/autoreply", get(autoreply))
        .route("/autoreply/rules", post(autoreply_save))
        .route("/autoreply/rules/{id}/delete", post(autoreply_delete))
        .route("/autoreply/restore", post(autoreply_restore))
        .route("/autoreply/test", post(autoreply_test))
        .route("/lurker", get(lurker))
        .route("/lurker/scan", post(lurker_scan))
        .route("/lurker/kick", post(lurker_kick))
        .route("/lurker/warn", post(lurker_warn))
        .route("/lurker/job", get(lurker_job))
        .route("/lurker/stop", post(lurker_stop))
        .route("/lurker/exempt", post(lurker_exempt))
        // 智能对话的供应商与模型：弹窗里一次改完，写入仍走 ctl。
        .route("/oai", get(oai_panel))
        .route("/oai/provider", post(oai_save_provider))
        .route("/oai/provider/remove", post(oai_remove_provider))
        .route("/oai/models", post(oai_fetch_models))
        .route("/oai/ping", post(oai_ping))
        .route("/oai/default", post(oai_set_default))
        .route("/oai/endpoint", post(oai_save_endpoint))
        .route("/ambient", get(ambient))
        .route("/ambient/sticker/{id}", get(sticker_image))
        .route("/ambient/source", post(write_ambient_source))
        .route("/settings", get(settings))
        .route("/settings/bot", post(save_bot))
        .route("/settings/global", post(save_global))
        .route("/groups", get(groups))
        .route("/friends", get(friends))
        .route("/theme", get(theme).post(save_theme))
        .route("/logs", get(log_history))
        .route("/logs/stream", get(log_stream))
        .route("/command", post(command))
        .route("/restart", post(restart))
        .route("/power", post(power))
        .route("/update", get(update_info).post(update_apply));
    router.layer(axum::middleware::from_fn_with_state(
        console,
        super::server::guard,
    ))
}

/// 一次不成功的调用：说清发生了什么与下一步。
fn bad(message: impl Into<String>) -> Response {
    super::server::fail(StatusCode::BAD_REQUEST, &message.into())
}

fn missing(what: &str) -> Response {
    super::server::fail(
        StatusCode::NOT_FOUND,
        &format!("这里没有「{what}」，请回上一页重新选一个"),
    )
}

// ==================== 总览 ====================

async fn overview(State(console): State<Arc<Console>>) -> Response {
    let ctx = console.ctx();
    let (today_start, now) = crate::db::utils::get_time_range("今日");
    let (week_start, _) = crate::db::utils::get_time_range("近7天");

    let today = crate::db::queries::get_message_count(&ctx.db, None, None, today_start, now)
        .await
        .unwrap_or(0);
    let week = crate::db::queries::get_message_count(&ctx.db, None, None, week_start, now)
        .await
        .unwrap_or(0);
    let today_people = crate::db::queries::get_active_user_count(&ctx.db, None, today_start, now)
        .await
        .unwrap_or(0);

    let (total, on, pending) = {
        let config = ctx.config.read().unwrap();
        let mut on = 0;
        let mut pending = 0;
        for plugin in get_plugins() {
            let running = crate::plugins::enabled(&config, plugin.name);
            if running {
                on += 1;
                if pending_startup(plugin.name) {
                    pending += 1;
                }
            }
        }
        (get_plugins().len(), on, pending)
    };

    let bots: Vec<Value> = console
        .bots()
        .iter()
        .map(|bot| {
            let login = bot.login_user.get();
            json!({
                "adapter": bot.adapter,
                "platform": bot.platform,
                "id": login.id,
                "name": login.name,
                "nick": login.nick,
                "avatar": avatar_for(&bot.platform, &login.id, login.avatar.as_deref()),
            })
        })
        .collect();

    Json(json!({
        "app": {
            "name": super::assets::APP_NAME,
            "version": env!("CARGO_PKG_VERSION"),
            "started": console.started_at(),
            "uptime": console.uptime_seconds(),
        },
        "bots": bots,
        "plugins": { "total": total, "on": on, "pending": pending },
        "messages": { "today": today, "week": week, "people": today_people },
        "console": { "address": console.url.split("?t=").next().unwrap_or_default() },
        "power": power_state(&console),
    }))
    .into_response()
}

/// 总览里「这一页能不能把知微停下来」那一格。
///
/// 只认装好的那一份：`cargo run` 出来的进程不归管理命令管，给按钮只会停错东西。
/// 面板那一格带上完整地址（含口令）与「此刻答不答得出来」，页面据此决定停下来之后
/// 还能不能从这里再启动。
fn power_state(console: &Console) -> Value {
    let available = super::power::available();
    let panel = if available {
        let config: super::Config = crate::plugins::get_config_or_default(console.ctx());
        (config.panel_port > 0).then(|| {
            json!({
                "url": super::power::panel_url(&config.bind, config.panel_port, console.token()),
                "ready": super::power::panel_ready(),
            })
        })
    } else {
        None
    };
    json!({ "available": available, "panel": panel })
}

/// 连接列表里的头像：实现端给了就用它，没给时 QQ 还能用号码拼出公开头像。
///
/// 微信没有可推导的公开地址（实现端的登录快照里本来就不带头像），界面会退成首字。
/// 只收 http(s)：头像是一条外部内容，不接受其它协议。
fn avatar_for(platform: &str, id: &str, avatar: Option<&str>) -> Option<String> {
    if let Some(url) = avatar.map(str::trim).filter(|url| !url.is_empty())
        && (url.starts_with("http://") || url.starts_with("https://"))
    {
        return Some(url.to_string());
    }
    let qq = matches!(platform.to_ascii_lowercase().as_str(), "red" | "qq");
    let numeric = !id.is_empty() && id.chars().all(|c| c.is_ascii_digit());
    (qq && numeric).then(|| format!("https://q.qlogo.cn/headimg_dl?dst_uin={id}&spec=640"))
}

// ==================== 插件 ====================

/// 列表与详情共用同一份指令文本：列表靠它支持按指令搜索，详情靠它显示完整清单。
/// 前缀取配置里的第一条，与机器人实际认的写法一致。
fn command_entries(plugin: &Plugin, prefix: &str) -> Vec<Value> {
    plugin
        .commands
        .iter()
        .map(|cmd| {
            let first = cmd.cmd.split(" / ").next().unwrap_or(cmd.cmd);
            let text = if crate::plugins::help::needs_prefix(first) {
                format!("{prefix}{first}")
            } else {
                first.to_string()
            };
            json!({ "cmd": text, "note": cmd.note })
        })
        .collect()
}

async fn plugins(State(console): State<Arc<Console>>) -> Response {
    let config = console.ctx().config.read().unwrap();
    let sections: Vec<Value> = crate::plugins::help::sections()
        .iter()
        .map(|(code, cn, en)| json!({ "code": code, "name": cn, "en": en }))
        .collect();
    let prefix = config.command_prefix.first().map_or("", String::as_str);
    let list: Vec<Value> = get_plugins()
        .iter()
        .map(|plugin| {
            let on = crate::plugins::enabled(&config, plugin.name);
            json!({
                "name": plugin.name,
                "display": plugin.display_name,
                "section": plugin.section,
                "summary": plugin.summary,
                "on": on,
                "pending": on && pending_startup(plugin.name),
                "commands": command_entries(plugin, prefix),
                "configurable": plugin.name != "meta_filter",
            })
        })
        .collect();
    Json(json!({ "plugins": list, "sections": sections })).into_response()
}

async fn plugin_detail(
    State(console): State<Arc<Console>>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    let Ok(plugin) = crate::plugins::ctl::resolve(&name) else {
        return missing(&name);
    };
    let config = console.ctx().config.read().unwrap();
    let Some(current) = config.plugins.get(plugin.name) else {
        return missing(&name);
    };
    let defaults = (plugin.default_config)();
    let mut diff = Vec::new();
    crate::plugins::ctl::differences(&defaults, current, "", &mut diff);
    let on = crate::plugins::enabled(&config, plugin.name);
    let prefix = config.command_prefix.first().map_or("", String::as_str);
    let commands = command_entries(plugin, prefix);

    Json(json!({
        "name": plugin.name,
        "display": plugin.display_name,
        "section": plugin.section,
        "summary": plugin.summary,
        "on": on,
        "pending": on && pending_startup(plugin.name),
        "effect": crate::plugins::ctl::effect(plugin.name),
        "commands": commands,
        "config": crate::plugins::ctl::redacted(current),
        "field_help": field_help(plugin.name),
        "field_order": field_order(plugin.name),
        "labels": labels(plugin.name),
        "depends": depends(plugin.name),
        "option_labels": option_labels(plugin.name),
        "templates": templates(plugin.name, current),
        "field_options": field_options(plugin.name, current),
        "maps": maps(plugin.name, current),
        "defaults": crate::plugins::ctl::redacted(&defaults),
        "diff": diff,
        "live": live(plugin.name, console.ctx()),
        "panel": panel(plugin.name),
    }))
    .into_response()
}

/// 配置表单之外、插件自己的一块面板（自动回复的规则）。界面按名字画，接口在下面各自的路由。
fn panel(name: &str) -> Value {
    match name {
        "autoreply" => json!("autoreply"),
        "lurker" => json!("lurker"),
        "oai" => json!("oai"),
        _ => Value::Null,
    }
}

// ---------- 智能对话的供应商与模型：一个弹窗里改完接入 ----------
//
// 读的是 `[oai.providers]` 与 `data/oai/config.json`，写的一律走 ctl；
// 唯一会出网的是「拉取模型列表」（只读 `/models`，不计费、不消耗对话额度）。

async fn oai_panel(State(console): State<Arc<Console>>) -> Response {
    Json(crate::plugins::oai::panel::overview(console.ctx()).await).into_response()
}

async fn oai_save_provider(
    State(console): State<Arc<Console>>,
    Json(body): Json<crate::plugins::oai::panel::SaveProvider>,
) -> Response {
    match crate::plugins::oai::panel::save(&console.local(), body).await {
        Ok(payload) => Json(payload).into_response(),
        Err(message) => bad(message),
    }
}

#[derive(Deserialize)]
struct OaiName {
    name: String,
}

async fn oai_remove_provider(
    State(console): State<Arc<Console>>,
    Json(body): Json<OaiName>,
) -> Response {
    match crate::plugins::oai::panel::remove(&console.local(), &body.name).await {
        Ok(payload) => Json(payload).into_response(),
        Err(message) => bad(message),
    }
}

async fn oai_fetch_models(
    State(console): State<Arc<Console>>,
    Json(body): Json<crate::plugins::oai::panel::FetchModels>,
) -> Response {
    match crate::plugins::oai::panel::fetch(console.ctx(), body).await {
        Ok(payload) => Json(payload).into_response(),
        Err(message) => bad(message),
    }
}

async fn oai_ping(
    State(console): State<Arc<Console>>,
    Json(body): Json<crate::plugins::oai::panel::Ping>,
) -> Response {
    match crate::plugins::oai::panel::ping(console.ctx(), body).await {
        Ok(payload) => Json(payload).into_response(),
        Err(message) => bad(message),
    }
}

async fn oai_set_default(
    State(console): State<Arc<Console>>,
    Json(body): Json<crate::plugins::oai::panel::DefaultModel>,
) -> Response {
    match crate::plugins::oai::panel::set_default(&console.local(), &body.model).await {
        Ok(payload) => Json(payload).into_response(),
        Err(message) => bad(message),
    }
}

async fn oai_save_endpoint(
    State(console): State<Arc<Console>>,
    Json(body): Json<crate::plugins::oai::panel::Endpoint>,
) -> Response {
    match crate::plugins::oai::panel::save_endpoint(&console.local(), body).await {
        Ok(payload) => Json(payload).into_response(),
        Err(message) => bad(message),
    }
}

// ---------- 潜水清理的名单面板：查、勾、移出、看进度 ----------

async fn lurker(State(console): State<Arc<Console>>) -> Response {
    Json(crate::plugins::lurker::panel_overview(console.ctx()).await).into_response()
}

#[derive(Deserialize)]
struct LurkerScan {
    group: String,
    #[serde(default)]
    days: Option<u32>,
    #[serde(default)]
    again: bool,
}

async fn lurker_scan(State(console): State<Arc<Console>>, Json(body): Json<LurkerScan>) -> Response {
    if body.days.is_some_and(|days| !(1..=3650).contains(&days)) {
        return bad("天数要在 1 到 3650 之间");
    }
    match crate::plugins::lurker::panel_scan(console.ctx(), &body.group, body.days, body.again).await {
        Ok(reply) => Json(reply).into_response(),
        Err(message) => bad(message),
    }
}

#[derive(Deserialize)]
struct LurkerPick {
    group: String,
    at: i64,
    ids: Vec<String>,
}

async fn lurker_kick(State(console): State<Arc<Console>>, Json(body): Json<LurkerPick>) -> Response {
    match crate::plugins::lurker::panel_kick(console.ctx(), &body.group, body.at, &body.ids).await {
        Ok(reply) => Json(reply).into_response(),
        Err(message) => bad(message),
    }
}

async fn lurker_warn(State(console): State<Arc<Console>>, Json(body): Json<LurkerPick>) -> Response {
    match crate::plugins::lurker::panel_warn(console.ctx(), &body.group, body.at, &body.ids).await {
        Ok(message) => Json(json!({ "message": message })).into_response(),
        Err(message) => bad(message),
    }
}

#[derive(Deserialize)]
struct LurkerGroup {
    group: String,
}

async fn lurker_job(State(console): State<Arc<Console>>, Query(query): Query<LurkerGroup>) -> Response {
    Json(crate::plugins::lurker::panel_job(console.ctx(), &query.group).await).into_response()
}

async fn lurker_stop(Json(body): Json<LurkerGroup>) -> Response {
    match crate::plugins::lurker::panel_stop(&body.group).await {
        Ok(message) => Json(json!({ "message": message })).into_response(),
        Err(message) => bad(message),
    }
}

#[derive(Deserialize)]
struct LurkerExempt {
    group: String,
    id: String,
    on: bool,
}

async fn lurker_exempt(State(console): State<Arc<Console>>, Json(body): Json<LurkerExempt>) -> Response {
    match crate::plugins::lurker::panel_exempt(console.ctx(), &body.group, &body.id, body.on).await {
        Ok(message) => Json(json!({ "message": message })).into_response(),
        Err(message) => bad(message),
    }
}

// ---------- 自动回复的规则面板：规则不在配置里，单独几条路由 ----------

async fn autoreply(State(console): State<Arc<Console>>) -> Response {
    Json(crate::plugins::autoreply::panel_overview(console.ctx()).await).into_response()
}

async fn autoreply_save(Json(body): Json<Value>) -> Response {
    match crate::plugins::autoreply::panel_save(body).await {
        Ok(reply) => Json(reply).into_response(),
        Err(message) => bad(message),
    }
}

async fn autoreply_delete(AxumPath(id): AxumPath<u32>) -> Response {
    match crate::plugins::autoreply::panel_delete(id).await {
        Ok(reply) => Json(reply).into_response(),
        Err(message) => bad(message),
    }
}

#[derive(Deserialize)]
struct Restore {
    id: Option<u32>,
}

async fn autoreply_restore(Json(body): Json<Restore>) -> Response {
    match crate::plugins::autoreply::panel_restore(body.id).await {
        Ok(reply) => Json(reply).into_response(),
        Err(message) => bad(message),
    }
}

#[derive(Deserialize)]
struct Trial {
    text: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    at: bool,
}

async fn autoreply_test(State(console): State<Arc<Console>>, Json(body): Json<Trial>) -> Response {
    let group = Some(body.group.as_str()).filter(|g| !g.is_empty());
    Json(crate::plugins::autoreply::panel_test(console.ctx(), &body.text, group, body.at).await).into_response()
}

/// 插件现在的情况（去处、计数之类），控制台插件页上方那张卡。只有少数插件有。
fn live(name: &str, ctx: &crate::event::Context) -> Value {
    match name {
        "anti_recall" => crate::plugins::anti_recall::live(ctx),
        "welcome" => crate::plugins::welcome::live(ctx),
        "autoreply" => crate::plugins::autoreply::live(ctx),
        "lurker" => crate::plugins::lurker::live(ctx),
        "keyword_guard" => crate::plugins::keyword_guard::live(ctx),
        _ => Value::Null,
    }
}

/// 字段的中文名（键名照旧显示在旁边）。映射表里一项的字段写成 `表.*.字段`，按想要的先后排。
fn label_list(name: &str) -> &'static [(&'static str, &'static str)] {
    match name {
        "welcome" => crate::plugins::welcome::LABELS,
        "anti_recall" => crate::plugins::anti_recall::LABELS,
        "autoreply" => crate::plugins::autoreply::LABELS,
        "lurker" => crate::plugins::lurker::LABELS,
        "repeater" => crate::plugins::repeater::LABELS,
        "ambient" => crate::plugins::ambient::LABELS,
        "keyword_guard" => crate::plugins::keyword_guard::LABELS,
        "markdown" => &[("theme", "配色主题")],
        "nailong" => &[
            ("targets", "识别类型"),
            ("threshold", "判定阈值"),
            ("judge", "认图方式"),
            ("sure", "直接提醒的分数"),
            ("ask_floor", "请求复核的分数"),
            ("manage.min_score", "自动管理的最低分"),
        ],
        "video_parse" => &[
            ("prefer_quality", "最高画质"),
            ("send", "发送方式"),
            ("platforms", "支持平台"),
        ],
        "oai" => &[
            ("music_send", "音乐发送方式"),
            ("video_send", "视频发送方式"),
            ("providers.*.api_base", "接口地址"),
            ("providers.*.api_key", "密钥"),
            ("providers.*.models", "模型"),
        ],
        "ai_news" => &[
            ("mode", "资讯范围"),
            ("window", "时间窗"),
            ("category", "资讯分类"),
            ("card_theme", "卡片主题"),
            ("realtime_mode", "实时快报来源"),
        ],
        _ => &[],
    }
}

fn labels(name: &str) -> Value {
    label_list(name)
        .iter()
        .map(|(path, label)| ((*path).to_string(), Value::from(*label)))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// 只在某个取值下才用得到的项：`{ 路径: { on: 看哪一项, in: [那一项取这些值时才用得到] } }`。
fn depends(name: &str) -> Value {
    match name {
        "welcome" => crate::plugins::welcome::depends(),
        "lurker" => crate::plugins::lurker::depends(),
        "repeater" => crate::plugins::repeater::depends(),
        _ => json!({}),
    }
}

/// 选项里每个取值的说法：`{ 路径: { 取值: 说法 } }`。
fn option_labels(name: &str) -> Value {
    let mut base = match name {
        "welcome" => crate::plugins::welcome::option_labels(),
        "autoreply" => crate::plugins::autoreply::option_labels(),
        "lurker" => crate::plugins::lurker::option_labels(),
        "ambient" => crate::plugins::ambient::option_labels(),
        "repeater" => crate::plugins::repeater::option_labels(),
        "keyword_guard" => crate::plugins::keyword_guard::option_labels(),
        _ => json!({}),
    };
    let weekdays = json!({ "1": "周一", "2": "周二", "3": "周三", "4": "周四", "5": "周五", "6": "周六", "7": "周日" });
    let extra = match name {
        "markdown" => json!({ "theme": { "light": "浅色", "dark": "深色" } }),
        "nailong" => json!({
            "judge": { "auto": "自动", "local": "本地", "hybrid": "混合", "ai": "AI" },
            "targets": { "nailong": "奶龙", "naiwa": "奶蛙" },
        }),
        "video_parse" => json!({
            "prefer_quality": { "80": "1080P", "64": "720P", "32": "480P", "16": "360P" },
            "send": { "bubble": "视频气泡", "file": "群文件", "both": "气泡 + 文件" },
            "platforms": { "bilibili": "哔哩哔哩", "douyin": "抖音" },
        }),
        "oai" => json!({
            "music_send": { "voice": "语音气泡", "file": "群文件", "both": "语音 + 文件" },
            "video_send": { "bubble": "视频气泡", "file": "群文件", "both": "气泡 + 文件" },
        }),
        "ai_news" => json!({
            "mode": { "selected": "精选", "all": "全部" },
            "realtime_mode": { "selected": "精选", "all": "全部" },
            "window": { "24h": "近 24 小时", "7d": "近 7 天" },
            "category": { "": "全部类别", "ai-models": "模型", "ai-products": "产品", "industry": "行业", "paper": "论文", "tip": "教程", "opinion": "观点" },
            "card_theme": { "auto": "自动", "light": "浅色", "dark": "深色" },
        }),
        "lurker" => json!({ "auto.weekdays": weekdays }),
        "ambient" => json!({ "peak.weekdays": weekdays }),
        _ => json!({}),
    };
    if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
        for (path, choices) in extra {
            base.insert(path.clone(), choices.clone());
        }
    }
    base
}

/// 哪些字段是带占位符的提示模板：界面换成文案编辑器（几种说法、插入占位符、预览）。
fn templates(name: &str, current: &Toml) -> Value {
    match name {
        "welcome" => crate::plugins::welcome::templates(current),
        "lurker" => crate::plugins::lurker::templates(),
        "keyword_guard" => crate::plugins::keyword_guard::templates(),
        _ => Value::Null,
    }
}

/// 「发一条试试」：让插件往它的去处真发一条，看那边收不收得到。
async fn test_plugin(State(console): State<Arc<Console>>, AxumPath(name): AxumPath<String>) -> Response {
    let result = match name.as_str() {
        "anti_recall" => crate::plugins::anti_recall::test(console.ctx()).await,
        _ => return missing(&name),
    };
    match result {
        Ok(message) => Json(json!({ "message": message, "live": live(&name, console.ctx()) })).into_response(),
        Err(message) => super::server::fail(StatusCode::BAD_GATEWAY, &message),
    }
}

#[derive(Deserialize)]
struct Toggle {
    on: bool,
}

async fn set_enabled(
    State(console): State<Arc<Console>>,
    AxumPath(name): AxumPath<String>,
    Json(body): Json<Toggle>,
) -> Response {
    let Ok(plugin) = crate::plugins::ctl::resolve(&name) else {
        return missing(&name);
    };
    let verb = if body.on { "on" } else { "off" };
    match crate::plugins::ctl::execute(&console.local(), &format!("{verb} {}", plugin.name)).await {
        Ok(output) => Json(json!({ "message": output.text })).into_response(),
        Err(message) => bad(message),
    }
}

#[derive(Deserialize)]
struct Edit {
    path: String,
    value: Value,
}

async fn set_config(
    State(console): State<Arc<Console>>,
    AxumPath(name): AxumPath<String>,
    Json(body): Json<Edit>,
) -> Response {
    let Ok(plugin) = crate::plugins::ctl::resolve(&name) else {
        return missing(&name);
    };
    let value = match to_toml(&body.value) {
        Ok(value) => value,
        Err(message) => return bad(message),
    };
    match crate::plugins::ctl::set_value(&console.local(), plugin.name, &body.path, value).await {
        Ok(message) => Json(json!({ "message": message })).into_response(),
        Err(message) => bad(message),
    }
}

/// 往映射表里加一项（每群覆盖、供应商……），或给已有的一项补回一个字段。
async fn insert_config(
    State(console): State<Arc<Console>>,
    AxumPath(name): AxumPath<String>,
    Json(body): Json<Edit>,
) -> Response {
    let Ok(plugin) = crate::plugins::ctl::resolve(&name) else {
        return missing(&name);
    };
    if let Err(message) = inside_map(plugin.name, &console, &body.path) {
        return bad(message);
    }
    let value = match to_toml(&body.value) {
        Ok(value) => value,
        Err(message) => return bad(message),
    };
    match crate::plugins::ctl::insert_value(&console.local(), plugin.name, &body.path, value).await {
        Ok(message) => Json(json!({ "message": message })).into_response(),
        Err(message) => bad(message),
    }
}

#[derive(Deserialize)]
struct Remove {
    path: String,
}

/// 删掉映射表里的一项，或一项里的某个字段（删了就沿用上层的设置）。
async fn remove_config(
    State(console): State<Arc<Console>>,
    AxumPath(name): AxumPath<String>,
    Json(body): Json<Remove>,
) -> Response {
    let Ok(plugin) = crate::plugins::ctl::resolve(&name) else {
        return missing(&name);
    };
    if let Err(message) = inside_map(plugin.name, &console, &body.path) {
        return bad(message);
    }
    match crate::plugins::ctl::remove_value(&console.local(), plugin.name, &body.path).await {
        Ok(message) => Json(json!({ "message": message })).into_response(),
        Err(message) => bad(message),
    }
}

/// 增删只开放给映射表：`表.键` 是一整项，`表.键.字段` 是项里的一个字段。
/// 其余的配置键是插件定死的，删了只会让校验报「缺少配置键」，干脆在门口拦下。
fn inside_map(plugin: &str, console: &Console, path: &str) -> Result<(), &'static str> {
    let config = console.ctx().config.read().unwrap();
    let current = config.plugins.get(plugin).ok_or("插件配置不存在")?;
    let maps = maps(plugin, current);
    let parts: Vec<&str> = path.split('.').collect();
    let ok = maps.iter().any(|(map, spec)| {
        // 表的路径里可以有 `*`（映射表里一项的某个字段本身又是一张映射表，如每群的自定义占位符）。
        let head: Vec<&str> = map.split('.').collect();
        if parts.len() <= head.len()
            || !head.iter().zip(&parts).all(|(want, got)| *want == "*" || want == got)
        {
            return false;
        }
        let rest = &parts[head.len()..];
        // 项里的字段只认模板里有的：插件的反序列化不拒绝陌生键，写进去也只是一条没人读的垃圾。
        let field_ok = |field: &str| spec["optional"] == true && spec["template"].get(field).is_some();
        !parts.iter().any(|part| part.is_empty())
            && (rest.len() == 1 || (rest.len() == 2 && field_ok(rest[1])))
    });
    if ok { Ok(()) } else { Err("这一项不能增删，只能修改") }
}

/// 「键由人定」的表：每群覆盖、供应商、搜索后端、推送目标偏好。
///
/// 默认配置里它们是空表，表单画不出任何东西、也加不进新项；这里告诉界面每张表的键是什么
/// （群号、推送目标还是自取的名字）、新的一项长什么样、各字段是什么意思。
/// `optional`：项里的字段可以单独删掉，删了就沿用上层的设置。模板取自当前的上层设置，
/// 所以新加一项时行为不变，改哪项才是给它单独定。`value = "template"` 的表每一项本身就是一条
/// 提示模板（自定义占位符），界面直接给文案编辑器；表的路径里的 `*` 匹配任意一个键。
fn maps(plugin: &str, current: &Toml) -> serde_json::Map<String, Value> {
    let at = |path: &str| -> Value {
        path.split('.')
            .try_fold(current, |value, key| value.get(key))
            .and_then(|value| serde_json::to_value(value).ok())
            .unwrap_or(Value::Null)
    };
    let mut out = serde_json::Map::new();
    match plugin {
        "welcome" => {
            out.insert("groups".into(), json!({
                "key": "group",
                "noun": "群",
                "add": "为一个群单独设置",
                "empty": "所有群都用上面的设置。要让某个群说不一样的欢迎词、单独开关验证，就在这里加上它。",
                "optional": true,
                "template": {
                    "join_enabled": at("join.enabled"),
                    "join_text": at("join.text"),
                    "images": at("join.images"),
                    "leave_enabled": at("leave.enabled"),
                    "leave_text": at("leave.text"),
                    "kicked_text": at("leave.kicked_text"),
                    "verify_enabled": at("verify.enabled"),
                    "verify_mode": at("verify.mode"),
                    "answers": at("verify.answers"),
                    "approve_enabled": at("approve.enabled"),
                    "keywords": at("approve.keywords"),
                    "custom": {},
                },
                "help": {
                    "join_enabled": "这个群发不发欢迎。",
                    "join_text": "这个群单独的欢迎词。",
                    "images": "这个群的欢迎配图：图片直链或 data/welcome/ 下的文件名。",
                    "leave_enabled": "这个群有人退群时说不说一声。",
                    "leave_text": "这个群有人自己退群时说的话。",
                    "kicked_text": "这个群有人被移出时说的话。",
                    "verify_enabled": "这个群的新人要不要先验证。",
                    "verify_mode": "这个群的验证方式。",
                    "answers": "这个群的口令（验证方式是「发出口令」时用）。",
                    "approve_enabled": "这个群的加群申请要不要自动审批。",
                    "keywords": "这个群审批看的附言关键词。",
                    "custom": "这个群自己的自定义占位符：同名的顶掉上面那份，没写的照用上面的。",
                },
            }));
            let custom = |empty: &str| {
                json!({
                    "key": "word",
                    "noun": "占位符",
                    "add": "新建占位符",
                    "empty": empty,
                    "value": "template",
                    "optional": false,
                    "template": [""],
                    "reserved": crate::plugins::welcome::reserved().collect::<Vec<_>>(),
                })
            };
            out.insert("custom".into(), custom("还没有自定义占位符。把群规、下载地址这类常用的话存成一个，各条提示里点一下就能插进去，改这里处处跟着变。"));
            out.insert("groups.*.custom".into(), custom("这个群没有自己的占位符，全部照用上面的。要让这个群的 {群规} 说别的，就在这里加一个同名的。"));
        }
        "lurker" => {
            out.insert("groups".into(), json!({
                "key": "group",
                "noun": "群",
                "add": "为一个群单独设置",
                "empty": "所有群都用上面的口径。群规不一样的群（比如要求两周说一次话）在这里单独设；群管理员在群里发「潜水 天数 14」「潜水 豁免 @某人」也会写到这里。",
                "optional": true,
                "template": {
                    "days": at("days"),
                    "min_messages": at("min_messages"),
                    "newcomer_days": at("newcomer_days"),
                    "exempt": [],
                },
                "help": {
                    "days": "这个群多少天没说话算潜水。",
                    "min_messages": "这个群在这段时间里至少说几句才算活跃（1 = 说过话就行）。",
                    "newcomer_days": "这个群的新人进群满几天还一句没说也算（0 = 不单独看新人）。",
                    "exempt": "这个群里永远不进名单的人（叠在全局豁免名单之上）。",
                },
            }));
        }
        "ai_news" => {
            out.insert("group_preferences".into(), json!({
                "key": "target",
                "noun": "推送目标",
                "add": "为一个推送目标单独设置",
                "empty": "所有推送目标都用全局的分类与静默时段。群里用 /ai分类、/ai静默 也会写到这里。",
                "optional": true,
                "template": {
                    "category": at("category"),
                    "quiet_start": at("realtime_quiet_start"),
                    "quiet_end": at("realtime_quiet_end"),
                },
                "help": {
                    "category": "只推这个分类：ai-models / ai-products / industry / paper / tip / opinion；留空 = 不限。",
                    "quiet_start": "静默时段起点（HH:MM）；起止都留空 = 这个目标不静默。",
                    "quiet_end": "静默时段终点（HH:MM），跨午夜按跨天算。",
                },
            }));
        }
        "oai" => {
            out.insert("providers".into(), json!({
                "key": "name",
                "noun": "供应商",
                "add": "添加供应商",
                "empty": "还没有供应商。填一把 DeepSeek 密钥管家大人就能用：点下面的 deepseek 加上，只填 api_key（地址留空用官方）；也可以由号主发「oai deepseek 密钥」。房间模型写成「供应商/模型」时按这里的名字找接口与密钥。",
                "suggest": crate::plugins::oai::catalog::KNOWN.iter().map(|known| known.id).collect::<Vec<_>>(),
                "optional": false,
                "template": { "api_base": "", "api_key": "", "models": [] },
                "help": {
                    "api_base": "OpenAI 兼容接口的基址。认得的供应商（建议名里那些）留空就用官方地址；中转站必填。",
                    "api_key": "这个供应商的密钥；认得的供应商留空时读环境变量（如 DEEPSEEK_API_KEY）。也可以私聊机器人发「oai 名字 密钥」，验过再存。",
                    "models": "这家要用的模型（裸 id）。留空跟随接口拉回的那份；写了就只认这几个。平时用上面的「供应商与模型」弹窗改。",
                },
            }));
            out.insert("search.backends".into(), json!({
                "key": "name",
                "noun": "搜索后端",
                "add": "添加搜索后端",
                "empty": "没有配密钥的后端。auto 会只用免密钥的抓取（以及借 deepseek 供应商的密钥）。",
                "suggest": ["tavily", "brave", "serper", "exa", "bocha", "searxng", "deepseek"],
                "optional": false,
                "template": { "api_key": "", "base_url": "" },
                "help": {
                    "api_key": "密钥；可以写多份（空白或逗号分隔），前一份额度用完就换下一份。",
                    "base_url": "searxng 必填（自建实例地址）；deepseek 可改写接口根；其余留空。",
                },
            }));
        }
        _ => {}
    }
    out
}

#[derive(Deserialize)]
struct Reset {
    path: Option<String>,
}

async fn reset_config(
    State(console): State<Arc<Console>>,
    AxumPath(name): AxumPath<String>,
    Json(body): Json<Reset>,
) -> Response {
    let Ok(plugin) = crate::plugins::ctl::resolve(&name) else {
        return missing(&name);
    };
    let command = match body.path.as_deref().filter(|path| !path.is_empty()) {
        Some(path) => format!("reset {} {path} --confirm", plugin.name),
        None => format!("reset {} --confirm", plugin.name),
    };
    match crate::plugins::ctl::execute(&console.local(), &command).await {
        Ok(output) => Json(json!({ "message": output.text })).into_response(),
        Err(message) => bad(message),
    }
}

/// 页面传上来的 JSON 转成 TOML 值。
///
/// 数字只按整数或小数两分：配置里的数字不是 `u32` 就是 `f64`，页面上的输入框
/// 本来就分不出更细的类型，`set_value` 会按真实字段类型再校验一次。
fn to_toml(value: &Value) -> Result<Toml, String> {
    Ok(match value {
        Value::Bool(flag) => Toml::Boolean(*flag),
        // TOML 把整数与小数分成两种类型，JSON 只有一种数字：按能不能整除往下分，
        // 分不出来的（超出 i64）退回小数，`set_value` 那边还会按真实字段类型再判一次。
        Value::Number(number) => match number.as_i64() {
            Some(integer) => Toml::Integer(integer),
            None => Toml::Float(number.as_f64().ok_or("这个数字读不出来")?),
        },
        Value::String(text) => Toml::String(text.clone()),
        Value::Array(items) => {
            Toml::Array(items.iter().map(to_toml).collect::<Result<Vec<_>, _>>()?)
        }
        Value::Object(map) => {
            let mut table = toml::Table::new();
            for (key, child) in map {
                table.insert(key.clone(), to_toml(child)?);
            }
            Toml::Table(table)
        }
        Value::Null => return Err("这一项不能是空值；字符串写 \"\"，清空数组写 []".into()),
    })
}

// ==================== 搭话 ====================

async fn ambient(State(console): State<Arc<Console>>) -> Response {
    let Ok(dir) = crate::plugins::get_data_dir("ambient").await else {
        return Json(json!({ "ready": false })).into_response();
    };
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    let persona = read("persona.md");
    let self_portrait = read("self.md");
    // 自动学习只落归纳后的短画像，没有原始聊天；这里原样交给控制台看一眼。
    let learning: Value = serde_json::from_str(&read("learning.json")).unwrap_or(Value::Null);

    let memory: Vec<Value> = crate::plugins::oai::chat::memory::snapshot()
        .into_iter()
        .map(|(group, memory)| {
            let mut people: Vec<Value> = memory
                .people
                .iter()
                .map(|(id, person)| {
                    json!({
                        "id": id.clone(),
                        "name": person.name,
                        "note": person.note,
                        "address": person.address,
                        "messages": person.messages,
                        "exchanges": person.exchanges,
                        "last_seen": person.last_seen,
                    })
                })
                .collect();
            // 有印象的排前面：这一页是拿来回顾「它记住了什么」，不是点名册。
            people.sort_by(|a, b| {
                let noted = |item: &Value| item["note"].as_str().is_some_and(|n| !n.is_empty());
                noted(b)
                    .cmp(&noted(a))
                    .then(b["messages"].as_u64().cmp(&a["messages"].as_u64()))
            });
            let notes: Vec<Value> = memory
                .notes
                .iter()
                .rev()
                .map(|note| json!({ "text": note.text, "at": note.at }))
                .collect();
            json!({ "group": group.clone(), "people": people, "notes": notes })
        })
        .collect();

    let stickers: Vec<Value> = crate::plugins::oai::chat::stickers::gallery()
        .into_iter()
        .map(|entry| {
            json!({
                "id": entry.id,
                "label": entry.label,
                "from": entry.from,
                "group": entry.group.clone(),
                "uses": entry.uses,
                "added_at": entry.added_at,
                "image": crate::plugins::oai::chat::stickers::file_of(&entry).is_some(),
            })
        })
        .collect();

    Json(json!({
        "ready": true,
        "persona": persona,
        "self": self_portrait,
        "learning": learning,
        "memory": memory,
        "stickers": stickers,
        "config": console
            .ctx()
            .config
            .read()
            .unwrap()
            .plugins
            .get("ambient")
            .map(crate::plugins::ctl::redacted)
            .and_then(|value| serde_json::to_value(value).ok())
            .unwrap_or(Value::Null),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct StickerPreview {
    #[serde(default)]
    play: bool,
}

async fn sticker_image(
    State(_console): State<Arc<Console>>,
    AxumPath(id): AxumPath<u32>,
    Query(preview): Query<StickerPreview>,
) -> Response {
    let entry = crate::plugins::oai::chat::stickers::by_id(id);
    let Some(entry) = entry else {
        return missing(&format!("#{id}"));
    };
    let Some(path) = crate::plugins::oai::chat::stickers::file_of(&entry) else {
        // 商城表情只存参数不存字节，本来就没有图可看。
        return super::server::fail(StatusCode::NOT_FOUND, "这张是商城表情，库里只存了参数");
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            if !preview.play {
                let decoded = crate::render::worker::run(move || -> Result<Vec<u8>, String> {
                    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
                        .with_guessed_format()
                        .map_err(|e| e.to_string())?;
                    let mut limits = image::Limits::default();
                    limits.max_image_width = Some(8192);
                    limits.max_image_height = Some(8192);
                    limits.max_alloc = Some(64 * 1024 * 1024);
                    reader.limits(limits);
                    let image = reader
                        .decode()
                        .map_err(|e| e.to_string())?
                        .thumbnail(396, 396);
                    let mut out = std::io::Cursor::new(Vec::new());
                    image
                        .write_to(&mut out, image::ImageFormat::Png)
                        .map_err(|e| e.to_string())?;
                    Ok(out.into_inner())
                })
                .await;
                return match decoded {
                    Ok(Ok(bytes)) => (
                        [
                            (http::header::CONTENT_TYPE, "image/png"),
                            (http::header::CACHE_CONTROL, "private, max-age=86400"),
                        ],
                        bytes,
                    )
                        .into_response(),
                    _ => super::server::fail(StatusCode::UNPROCESSABLE_ENTITY, "无法生成静态预览"),
                };
            }
            let mime = match path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str()
            {
                "png" => "image/png",
                "gif" => "image/gif",
                "webp" => "image/webp",
                _ => "image/jpeg",
            };
            // 文件名就是内容的 md5，同一张图永远是同一个地址，可以放心长缓存：
            // 画廊一屏几十张，每次回访重下一遍纯属浪费。
            (
                [
                    (http::header::CONTENT_TYPE, mime),
                    (
                        http::header::CACHE_CONTROL,
                        "public, max-age=31536000, immutable",
                    ),
                ],
                bytes,
            )
                .into_response()
        }
        Err(error) => super::server::fail(
            StatusCode::NOT_FOUND,
            &format!("这张图已经不在库目录里了（{error}）"),
        ),
    }
}

#[derive(Deserialize)]
struct Source {
    name: String,
    text: String,
}

/// 改写搭话的运行时文本：人格（`persona.md`）与本体档案（`self.md`）。
///
/// 这两个文件本来就有「手动覆盖运行时人设」这条维护路径，控制台把它做成一步：
/// 落盘前先按 `<名字>.md.backup-<日期>-<时分>` 存一份旧的，与手工做法一字不差。
/// 上限 64 KB——它们是提示词，不是日记，超过这个体积模型也读不完。
async fn write_ambient_source(
    State(_console): State<Arc<Console>>,
    Json(body): Json<Source>,
) -> Response {
    let file = match body.name.as_str() {
        "persona" => "persona.md",
        "self" => "self.md",
        other => {
            return bad(format!(
                "不认识的文本「{other}」；只有 persona 与 self 两份"
            ));
        }
    };
    if body.text.len() > 64 * 1024 {
        return bad("太长了；这两份是提示词，上限 64 KB");
    }
    let Ok(dir) = crate::plugins::get_data_dir("ambient").await else {
        return bad("读不到 data/ambient，搭话插件还没有初始化过");
    };
    let path = dir.join(file);
    if let Ok(previous) = tokio::fs::read_to_string(&path).await
        && !previous.is_empty()
    {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M");
        let backup = dir.join(format!("{file}.backup-{stamp}"));
        if let Err(error) = write_atomic_async(backup, previous.into_bytes()).await {
            return bad(format!("备份失败，没有覆盖原文件：{error}"));
        }
    }
    match write_atomic_async(path, body.text.into_bytes()).await {
        Ok(()) => Json(json!({
            "message": format!("已保存 {file}；下一轮对话生效，旧的那份留在同目录的 backup 里。")
        }))
        .into_response(),
        Err(error) => bad(format!("写入失败：{error}")),
    }
}

// ==================== 接入与全局 ====================

/// 框架级的几项：连哪几个实现端、指令前缀、全局群名单、浏览器路径。
///
/// 它们不在任何一个插件的配置里（`AppConfig` 的具名字段，不是 `plugins` 那棵
/// 展开的表），所以 `/ctl` 够不着——一个只带图形界面的设备上，够不着就等于
/// 装不起来。这一组接口补的就是这个缺口，写入照旧走 `ctl::change`。
async fn settings(State(console): State<Arc<Console>>) -> Response {
    let config = console.ctx().config.read().unwrap();
    let bots: Vec<Value> = config
        .bots
        .iter()
        .map(|bot| {
            json!({
                "enabled": bot.enabled,
                "protocol": bot.protocol,
                "url": bot.url.clone().unwrap_or_default(),
                "module": bot.module,
                "has_token": bot
                    .access_token
                    .as_deref()
                    .is_some_and(|token| !token.is_empty()),
            })
        })
        .collect();
    Json(json!({
        "command_prefix": config.command_prefix,
        "browser_path": config.browser_path.clone().unwrap_or_default(),
        "global_filter": config.global_filter,
        "bots": bots,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct BotEdit {
    /// 要改的是第几条；不给就是新增。
    index: Option<usize>,
    /// 删掉这一条。
    #[serde(default)]
    remove: bool,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    protocol: String,
    #[serde(default)]
    url: String,
    /// 不给（或只给空白）表示「不动原来那个」：页面上读到的永远是隐去后的样子，
    /// 「没写字」只能当「别动它」。
    #[serde(default)]
    access_token: Option<String>,
    /// 把已设的令牌清掉（改成不鉴权）。要明说，不能靠留空——留空是「不动」。
    #[serde(default)]
    clear_token: bool,
}

async fn save_bot(State(console): State<Arc<Console>>, Json(body): Json<BotEdit>) -> Response {
    let result = crate::plugins::ctl::change(&console.local(), |config| {
        if body.remove {
            let index = body.index.ok_or("要删哪一条？")?;
            if index >= config.bots.len() {
                return Err("这一条已经不在列表里了，刷新一次看看".into());
            }
            config.bots.remove(index);
            return Ok("已删掉这条连接；改动在下次启动后生效。".to_string());
        }

        let protocol = body.protocol.trim();
        if !["satori", "console"].contains(&protocol) {
            return Err(format!(
                "不认识的协议「{protocol}」；现在只有 satori 与 console"
            ));
        }
        let url = body.url.trim();
        // 跟随模块的那几条可以不写地址（端口与令牌从模块设置里现读）。
        let module = body
            .index
            .and_then(|index| config.bots.get(index))
            .and_then(|bot| bot.module.clone());
        if protocol == "satori" && url.is_empty() && module.is_none() {
            return Err("Satori 要填实现端地址，例如 http://127.0.0.1:3001".into());
        }

        let token = body
            .access_token
            .as_ref()
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty());
        let entry = |previous: Option<&crate::config::BotConfig>| crate::config::BotConfig {
            enabled: body.enabled,
            protocol: protocol.to_string(),
            module: module.clone(),
            url: (!url.is_empty()).then(|| url.to_string()),
            // 页面读不到原来的令牌，所以「没写字」当「别动它」处理；清掉要明说。
            access_token: if body.clear_token {
                None
            } else {
                token
                    .clone()
                    .or_else(|| previous.and_then(|bot| bot.access_token.clone()))
            },
        };

        if let Some(index) = body.index {
            let previous = config.bots.get(index);
            if previous.is_none() {
                return Err("这一条已经不在列表里了，刷新一次看看".into());
            }
            let entry = entry(previous);
            config.bots[index] = entry;
            Ok("已保存这条连接；改动在下次启动后生效。".to_string())
        } else {
            config.bots.push(entry(None));
            Ok("已加一条连接；改动在下次启动后生效。".to_string())
        }
    })
    .await;
    match result {
        Ok(message) => Json(json!({ "message": message })).into_response(),
        Err(message) => bad(message),
    }
}

#[derive(Deserialize)]
struct GlobalEdit {
    command_prefix: Vec<String>,
    #[serde(default)]
    browser_path: String,
    global_filter: crate::config::GlobalFilterConfig,
}

async fn save_global(
    State(console): State<Arc<Console>>,
    Json(body): Json<GlobalEdit>,
) -> Response {
    if !body.command_prefix.is_empty()
        && body
            .command_prefix
            .iter()
            .all(|prefix| prefix.trim().is_empty())
    {
        // 空数组是合法的（无前缀），但一条空串只会在匹配时添乱。
        return bad("前缀写空串没有意义；不带前缀请用空数组 []");
    }
    let result = crate::plugins::ctl::change(&console.local(), |config| {
        config.command_prefix = body
            .command_prefix
            .iter()
            .map(|prefix| prefix.trim().to_string())
            .filter(|prefix| !prefix.is_empty())
            .collect();
        let path = body.browser_path.trim();
        config.browser_path = (!path.is_empty()).then(|| path.to_string());
        config.global_filter = crate::config::GlobalFilterConfig {
            blacklist: tidy_ids(&body.global_filter.blacklist),
            whitelist: tidy_ids(&body.global_filter.whitelist),
            ..body.global_filter.clone()
        };
        Ok("已保存全局设置；前缀与名单下一条消息生效，浏览器路径下次启动生效。".to_string())
    })
    .await;
    match result {
        Ok(message) => Json(json!({ "message": message })).into_response(),
        Err(message) => bad(message),
    }
}

/// 名单去掉首尾空白、空串与重复项，顺序照旧——页面之外（`curl`、脚本）也能写这个接口。
fn tidy_ids(ids: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in ids.iter().map(|id| id.trim()).filter(|id| !id.is_empty()) {
        if !out.iter().any(|known| known == id) {
            out.push(id.to_string());
        }
    }
    out
}

#[derive(Deserialize, Default)]
struct RosterQuery {
    /// 带上它（值不拘）就绕过缓存，重新问一遍实现端。
    refresh: Option<String>,
}

// ==================== 外观 ====================

/// 控制台的主题：种子色、配色方案、明暗与对比度。存在 `data/console/theme.json`，
/// 换一台设备打开也是同一个样子；字号是各设备自己的，只在浏览器里记。
/// 配色由页面自己算（`res/console/theme.js`），这里只存四个选择、只认白名单里的值。
#[derive(Deserialize, serde::Serialize)]
struct Theme {
    seed: String,
    variant: String,
    mode: String,
    contrast: String,
}

const THEME_VARIANTS: [&str; 7] = [
    "fidelity",
    "tonal",
    "vibrant",
    "expressive",
    "content",
    "neutral",
    "monochrome",
];

impl Theme {
    fn check(&self) -> Result<(), &'static str> {
        let seed = self.seed.strip_prefix('#').unwrap_or("");
        if seed.len() != 6 || !seed.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("主题色要写成 #RRGGBB 这样的六位十六进制");
        }
        if !THEME_VARIANTS.contains(&self.variant.as_str()) {
            return Err("没有这种配色方案");
        }
        if !["system", "light", "dark"].contains(&self.mode.as_str()) {
            return Err("明暗只能是跟随系统、浅色或深色");
        }
        if !["system", "standard", "medium", "high"].contains(&self.contrast.as_str()) {
            return Err("对比度只能是跟随系统、标准、中或高");
        }
        Ok(())
    }
}

async fn theme_path() -> Option<std::path::PathBuf> {
    crate::plugins::get_data_dir("console")
        .await
        .ok()
        .map(|dir| dir.join("theme.json"))
}

/// 没存过就是 `null`：页面按内置的默认主题画，不必知道默认值是什么。
async fn theme() -> Response {
    let Some(path) = theme_path().await else {
        return Json(Value::Null).into_response();
    };
    let saved = tokio::fs::read(&path)
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Theme>(&bytes).ok())
        .filter(|theme| theme.check().is_ok());
    Json(json!(saved)).into_response()
}

async fn save_theme(Json(body): Json<Theme>) -> Response {
    if let Err(message) = body.check() {
        return bad(message);
    }
    let Some(path) = theme_path().await else {
        return bad("数据目录不可写，主题没能保存");
    };
    let mut theme = body;
    theme.seed = theme.seed.to_ascii_lowercase();
    match write_atomic_async(path, serde_json::to_vec_pretty(&theme).unwrap_or_default()).await {
        Ok(()) => Json(json!(theme)).into_response(),
        Err(error) => bad(format!("主题没能保存：{error}")),
    }
}

/// 已加入的群：给名单编辑器「从已加入的群里选」用，也用来把名单里的群号翻成群名。
async fn groups(Query(query): Query<RosterQuery>) -> Response {
    roster(Roster::Groups, query).await
}

/// 好友：给私聊对象的名单（资讯推送的私聊目标、管理员……）挑人、把账号翻成昵称用。
async fn friends(Query(query): Query<RosterQuery>) -> Response {
    roster(Roster::Friends, query).await
}

/// 名册现问实现端，所以只有连着的账号才看得到；30 秒内重复打开页面不再重问。
/// `items` 是 `{id, name, alias?, platform}`；`failed` 是没问成的连接数，大于 0 时页面要说明这张表可能不全。
async fn roster(kind: Roster, query: RosterQuery) -> Response {
    const FRESH: std::time::Duration = std::time::Duration::from_secs(30);
    type Cache = std::sync::Mutex<[Option<(std::time::Instant, Value)>; 2]>;
    static CACHE: Cache = std::sync::Mutex::new([None, None]);
    let slot = kind as usize;

    if query.refresh.is_none()
        && let Some((at, cached)) = &CACHE.lock().unwrap()[slot]
        && at.elapsed() < FRESH
    {
        return Json(cached.clone()).into_response();
    }
    let (items, failed) = crate::adapters::satori::list_roster(kind).await;
    let value = json!({ "items": items, "failed": failed });
    // 一条都没问到就不留：连接刚恢复时不该还顶着一张空表。
    if !items.is_empty() {
        CACHE.lock().unwrap()[slot] = Some((std::time::Instant::now(), value.clone()));
    }
    Json(value).into_response()
}

// ==================== 日志 ====================

#[derive(Deserialize, Default)]
struct LogQuery {
    limit: Option<usize>,
}

async fn log_history(
    State(console): State<Arc<Console>>,
    Query(query): Query<LogQuery>,
) -> Response {
    let limit = query
        .limit
        .unwrap_or(super::state::HISTORY_LIMIT)
        .min(super::state::HISTORY_LIMIT);
    Json(json!({ "lines": console.recent(limit) })).into_response()
}

async fn log_stream(State(console): State<Arc<Console>>) -> Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use tokio::sync::broadcast::error::{RecvError, TryRecvError};

    // 持同一把日志锁订阅并取快照，避免历史请求与订阅之间漏行或重复。
    let (history, receiver) = console.snapshot();
    let first = futures_util::stream::once(async move {
        Ok::<_, std::convert::Infallible>(
            Event::default()
                .event("snapshot")
                .json_data(json!({ "lines": history }))
                .unwrap_or_default(),
        )
    });
    let stream =
        futures_util::stream::unfold((console, receiver), |(console, mut receiver)| async move {
            let event = match receiver.recv().await {
                Ok(entry) => {
                    // 手机上把高密度日志合成一批，减少网络唤醒与 JS message 事件。
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    let mut entries = vec![entry];
                    let mut snapshot = false;
                    let mut skipped = 0u64;
                    while entries.len() < 128 {
                        match receiver.try_recv() {
                            Ok(entry) => entries.push(entry),
                            Err(TryRecvError::Lagged(dropped)) => {
                                let (history, fresh) = console.snapshot();
                                receiver = fresh;
                                entries = history;
                                snapshot = true;
                                skipped = dropped;
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                    Event::default()
                        .event(if snapshot { "snapshot" } else { "batch" })
                        // 落后时把落下的行数一并报上去，页面在状态行上写明，
                        // 「不静默跳行」才是真的（缓冲本身只有 HISTORY_LIMIT 行）。
                        .json_data(json!({ "lines": entries, "dropped": skipped }))
                        .unwrap_or_default()
                }
                // 消费者跟不上时补一份有界快照，不能静默漏掉错误日志。
                Err(RecvError::Lagged(dropped)) => {
                    let (history, fresh) = console.snapshot();
                    receiver = fresh;
                    Event::default()
                        .event("snapshot")
                        .json_data(json!({ "lines": history, "dropped": dropped }))
                        .unwrap_or_default()
                }
                Err(RecvError::Closed) => return None,
            };
            Some((
                Ok::<_, std::convert::Infallible>(event),
                (console, receiver),
            ))
        });
    Sse::new(futures_util::StreamExt::chain(first, stream))
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(20)))
        .into_response()
}

// ==================== 命令 ====================

#[derive(Deserialize)]
struct Command {
    input: String,
}

/// 把一行 `/ctl` 命令喂给同一套执行器，回执原样带回来。
///
/// 只走 `ctl`：它管的是本机配置，不往群里发消息。要触发别的东西请回群里或者用
/// agent 房间——这个面板不该长成第二个消息入口。
async fn command(State(console): State<Arc<Console>>, Json(body): Json<Command>) -> Response {
    let input = body.input.trim().trim_start_matches('/').to_string();
    if input.is_empty() {
        return bad("写点什么再回车；例如 list、show ambient、diff oai");
    }
    match crate::plugins::ctl::execute(&console.local(), &input).await {
        Ok(output) => Json(json!({ "message": output.text })).into_response(),
        Err(message) => bad(message),
    }
}

// ==================== 重启 ====================

/// 总览页那颗「重启」按钮。
///
/// 与 `/restart` 指令、每日定时重启共用同一条请求：防抖标记在这里只读，
/// 真正的置位与清理仍由 `restart::do_restart` 与主循环负责。延一小会儿
/// 再喊，好让这条回执先写回浏览器；页面随后会自己轮询到新进程。
async fn restart(State(console): State<Arc<Console>>) -> Response {
    let delay = std::time::Duration::from_millis(1200);
    match crate::plugins::restart::request_now(&console.local(), "控制台", delay) {
        Ok(()) => Json(json!({
            "message": "正在重启，本页会在就绪后自动恢复",
            "delay": delay.as_secs_f64(),
        }))
        .into_response(),
        Err(message) => bad(message),
    }
}

// ==================== 停止 ====================

#[derive(Deserialize)]
struct PowerRequest {
    /// `bot` = 只停机器人；`all` = 连管理面板与开机守护一起停。
    scope: String,
}

/// 总览页那颗「停止」。回执照写，动作交给管理命令（见 [`super::power`]）——它比回执
/// 晚一步动手，停的就是机器人自己也不会把这句话截断。
async fn power(Json(body): Json<PowerRequest>) -> Response {
    let action = match body.scope.as_str() {
        "bot" => super::power::Action::Stop,
        "all" => super::power::Action::Halt,
        _ => return bad("停法只有 bot（只停机器人）与 all（全面停止）两种"),
    };
    if !super::power::available() {
        return super::server::fail(
            StatusCode::CONFLICT,
            "这个进程不归管理命令管（cargo run 起来的），停它请回终端按 Ctrl+C",
        );
    }
    match super::power::spawn(action) {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(json!({
                "message": action.message(),
                "scope": body.scope,
                "delay": super::power::GRACE_SECONDS,
            })),
        )
            .into_response(),
        Err(super::power::SpawnError::Busy) => {
            super::server::fail(StatusCode::CONFLICT, "上一个操作还没结束，稍等一下")
        }
        Err(super::power::SpawnError::Unavailable(why)) => {
            super::server::fail(StatusCode::INTERNAL_SERVER_ERROR, &why)
        }
    }
}

#[derive(Deserialize)]
struct UpdateQuery {
    #[serde(default)]
    force: bool,
}

/// 有没有新版本。查询结果缓存半小时（`?force=true` 立即再查），页面打开时问一下就够。
async fn update_info(Query(query): Query<UpdateQuery>) -> Response {
    let current = crate::release::VERSION;
    if !crate::release::managed() {
        return Json(json!({ "current": current, "managed": false, "latest": null })).into_response();
    }
    let age = if query.force {
        std::time::Duration::ZERO
    } else {
        std::time::Duration::from_mins(30)
    };
    match crate::release::check_cached(age).await {
        Ok(latest) => Json(json!({
            "current": current,
            "managed": true,
            "latest": latest.map(|release| json!({ "version": release.version, "notes": release.notes })),
        }))
        .into_response(),
        Err(error) => Json(json!({
            "current": current,
            "managed": true,
            "latest": null,
            "error": format!("{error:#}"),
        }))
        .into_response(),
    }
}

/// 立即更新：下载、校验、换上，随后重启（页面按启动时间变了来判断回来了没有）。
async fn update_apply(State(console): State<Arc<Console>>) -> Response {
    match crate::plugins::restart::update_now(&console.local()).await {
        Ok(message) => Json(json!({ "message": message, "delay": 1.5 })).into_response(),
        Err(message) => bad(message),
    }
}

/// 控制台自己的配置从 `[console]` 读，改它走 `/api/plugins/console/config`。
fn field_help(plugin: &str) -> std::collections::BTreeMap<String, String> {
    let mut result = std::collections::BTreeMap::new();
    let mut section = String::new();
    let mut comments = Vec::new();
    for line in include_str!("../../../config.example.toml").lines() {
        let line = line.trim();
        if let Some(comment) = line.strip_prefix("# ") {
            if !comment.starts_with('=') {
                comments.push(comment.to_string());
            }
        } else if line.starts_with('[') {
            section = line.trim_matches(['[', ']']).to_string();
            // 表头前的那段话就是这一节的说明（界面画在小标题下）；夹在里面的示例配置不算。
            if let Some(path) = section.strip_prefix(&format!("{plugin}."))
                && let Some(text) = tidy_help(&comments)
            {
                result.insert(path.to_string(), text);
            }
            comments.clear();
        } else if let Some((key, _)) = line.split_once('=') {
            if section == plugin || section.starts_with(&format!("{plugin}.")) {
                let prefix = section
                    .strip_prefix(plugin)
                    .unwrap()
                    .trim_start_matches('.');
                let path = if prefix.is_empty() {
                    key.trim().to_string()
                } else {
                    format!("{prefix}.{}", key.trim())
                };
                if let Some(text) = tidy_help(&comments) {
                    result.insert(path, text);
                }
            }
            comments.clear();
        } else {
            comments.clear();
        }
    }
    result
}

/// 配置项在配置参考里出现的先后：表单照这个顺序排，而不是按键名的字母序——
/// 配置参考是按「先要紧的、相关的挨着」写的，字母序会把「欢迎」排到「审批」后面。
fn field_order(plugin: &str) -> Vec<String> {
    let mut order = Vec::new();
    let mut section = String::new();
    let mut push = |path: String| {
        if !order.contains(&path) {
            order.push(path);
        }
    };
    for line in include_str!("../../../config.example.toml").lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']']).to_string();
            if let Some(path) = section.strip_prefix(&format!("{plugin}.")) {
                push(path.to_string());
            }
        } else if !line.starts_with('#')
            && let Some((key, _)) = line.split_once('=')
            && let Some(rest) = section.strip_prefix(plugin)
            && (rest.is_empty() || rest.starts_with('.'))
        {
            let prefix = rest.trim_start_matches('.');
            let key = key.trim();
            push(if prefix.is_empty() { key.to_string() } else { format!("{prefix}.{key}") });
        }
    }
    // 映射表里一项的字段（`表.*.字段`）配置参考里没有，按中文名表的先后排。
    for (path, _) in label_list(plugin) {
        if path.contains(".*.") {
            push((*path).to_string());
        }
    }
    order
}

/// 说明拼好以后的收尾：示例配置抽走了，留下的「例如……：」就没了下文，一并剪掉。
fn tidy_help(lines: &[String]) -> Option<String> {
    let text = lines
        .iter()
        .filter(|line| !example_line(line))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let text = match text.strip_suffix('：').or_else(|| text.strip_suffix(':')) {
        Some(head) => match head.rfind("例如") {
            Some(at) => format!("{}。", head[..at].trim_end_matches([' ', '，', ',', '。'])),
            None => text,
        },
        None => text,
    };
    let text = text.trim().to_string();
    (!text.is_empty() && text != "。").then_some(text)
}

/// 注释里的示例配置（`# [x.y]`、`# key = 值`），不是给人读的说明。
fn example_line(line: &str) -> bool {
    let line = line.trim_start_matches(['#', ' ']);
    line.starts_with('[')
        || line
            .split_once(" = ")
            .is_some_and(|(key, _)| {
                // 带引号的键（`"群规" = …`）也是示例；说明文字里的「留空 = 不发」不是。
                let quoted = key.len() > 2 && key.starts_with('"') && key.ends_with('"') && !key[1..key.len() - 1].contains('"');
                quoted || (!key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.'))
            })
}

/// 单选与多选共用的选项元数据。数字选项也走这里，前端不必按字段名猜画质或星期。
fn field_options(plugin: &str, value: &Toml) -> std::collections::BTreeMap<String, Vec<Value>> {
    fn visit(
        plugin: &str,
        value: &Toml,
        prefix: &str,
        out: &mut std::collections::BTreeMap<String, Vec<Value>>,
    ) {
        if let Some(table) = value.as_table() {
            for (key, value) in table {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                let mut choices: Vec<Value> = match (plugin, path.as_str()) {
                    ("video_parse", "prefer_quality") => [80, 64, 32, 16].into_iter().map(Value::from).collect(),
                    ("lurker", "auto.weekdays") | ("ambient", "peak.weekdays") => {
                        (1..=7).map(Value::from).collect()
                    }
                    _ => crate::plugins::ctl::options(plugin, &path)
                        .iter()
                        .map(|option| Value::from(*option))
                        .collect(),
                };
                // 卡片主题仍认得旧别名，但选择器只摆自动 / 浅色 / 深色三种含义不同的取值。
                if (plugin, path.as_str()) == ("ai_news", "card_theme") {
                    choices.retain(|choice| matches!(choice.as_str(), Some("auto" | "light" | "dark")));
                }
                if !choices.is_empty() {
                    out.insert(path.clone(), choices);
                }
                visit(plugin, value, &path, out);
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    visit(plugin, value, "", &mut out);
    out
}
