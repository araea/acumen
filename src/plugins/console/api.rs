//! 控制台的接口面：读的是进程里现成的东西，写的一律走 `ctl` 那条路。
//!
//! 每个写操作都落到 `ctl::change` / `ctl::set_value` / `ctl::execute` 上——校验、
//! 串行化、失败不改内存、原子替换这一套都在那里，控制台只是换了个触发器。
//! 于是「网页上改了配置」与「群里敲 /ctl」是同一件事的两种按键。

use super::state::Console;

use crate::plugins::{get_plugins, pending_startup};
use axum::Json;
use axum::Router;
use axum::extract::{Path as AxumPath, State};
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
        .route("/plugins/{name}/reset", post(reset_config))
        .route("/ambient", get(ambient))
        .route("/ambient/sticker/{id}", get(sticker_image))
        .route("/ambient/source", post(write_ambient_source))
        .route("/settings", get(settings))
        .route("/settings/bot", post(save_bot))
        .route("/settings/global", post(save_global))
        .route("/logs", get(log_history))
        .route("/logs/stream", get(log_stream))
        .route("/command", post(command));
    router.layer(axum::middleware::from_fn_with_state(
        console,
        super::server::guard,
    ))
}

/// 一次不成功的调用。文案照 `CONTENT.md` 的口径：说清发生了什么与下一步。
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
    let today_people =
        crate::db::queries::get_active_user_count(&ctx.db, None, today_start, now)
            .await
            .unwrap_or(0);

    let (total, on, pending) = {
        let config = ctx.config.read().unwrap();
        let mut on = 0;
        let mut pending = 0;
        for plugin in get_plugins() {
            let running = crate::plugins::ctl::enabled(&config, plugin.name);
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
                "avatar": login.avatar,
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
    }))
    .into_response()
}

// ==================== 插件 ====================

async fn plugins(State(console): State<Arc<Console>>) -> Response {
    let config = console.ctx().config.read().unwrap();
    let sections: Vec<Value> = crate::plugins::help::sections()
        .iter()
        .map(|(code, cn, en)| json!({ "code": code, "name": cn, "en": en }))
        .collect();
    let list: Vec<Value> = get_plugins()
        .iter()
        .map(|plugin| {
            let on = crate::plugins::ctl::enabled(&config, plugin.name);
            json!({
                "name": plugin.name,
                "display": plugin.display_name,
                "section": plugin.section,
                "summary": plugin.summary,
                "on": on,
                "pending": on && pending_startup(plugin.name),
                "commands": plugin.commands.len(),
                "configurable": plugin.name != "meta_filter",
            })
        })
        .collect();
    Json(json!({ "plugins": list, "sections": sections })).into_response()
}

async fn plugin_detail(State(console): State<Arc<Console>>, AxumPath(name): AxumPath<String>) -> Response {
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
    let on = crate::plugins::ctl::enabled(&config, plugin.name);
    let commands: Vec<Value> = plugin
        .commands
        .iter()
        .map(|cmd| json!({ "cmd": cmd.cmd, "note": cmd.note }))
        .collect();

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
        "defaults": crate::plugins::ctl::redacted(&defaults),
        "diff": diff,
    }))
    .into_response()
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
        Value::Array(items) => Toml::Array(items.iter().map(to_toml).collect::<Result<Vec<_>, _>>()?),
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

    let memory: Vec<Value> = crate::plugins::ambient::memory::snapshot()
        .into_iter()
        .map(|(group, memory)| {
            let mut people: Vec<Value> = memory
                .people
                .iter()
                .map(|(id, person)| {
                    json!({
                        "id": id.to_string(),
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
            json!({ "group": group.to_string(), "people": people, "notes": notes })
        })
        .collect();

    let stickers: Vec<Value> = crate::plugins::ambient::stickers::gallery()
        .into_iter()
        .map(|entry| {
            json!({
                "id": entry.id,
                "label": entry.label,
                "from": entry.from,
                "group": entry.group.to_string(),
                "uses": entry.uses,
                "added_at": entry.added_at,
                "image": crate::plugins::ambient::stickers::file_of(&entry).is_some(),
            })
        })
        .collect();

    Json(json!({
        "ready": true,
        "persona": persona,
        "self": self_portrait,
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

async fn sticker_image(
    State(_console): State<Arc<Console>>,
    AxumPath(id): AxumPath<u32>,
) -> Response {
    let entry = crate::plugins::ambient::stickers::gallery()
        .into_iter()
        .find(|entry| entry.id == id);
    let Some(entry) = entry else {
        return missing(&format!("#{id}"));
    };
    let Some(path) = crate::plugins::ambient::stickers::file_of(&entry) else {
        // 商城表情只存参数不存字节，本来就没有图可看。
        return super::server::fail(StatusCode::NOT_FOUND, "这张是商城表情，库里只存了参数");
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
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
            ([(axum::http::header::CONTENT_TYPE, mime)], bytes).into_response()
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
        other => return bad(format!("不认识的文本「{other}」；只有 persona 与 self 两份")),
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
        if let Err(error) = tokio::fs::write(&backup, previous).await {
            return bad(format!("备份失败，没有覆盖原文件：{error}"));
        }
    }
    match tokio::fs::write(&path, body.text.as_bytes()).await {
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
    /// 不给表示「不动原来那个」（页面上读到的永远是隐去后的样子）；
    /// 给空串表示清掉。
    #[serde(default)]
    access_token: Option<String>,
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
            return Err(format!("不认识的协议「{protocol}」；现在只有 satori 与 console"));
        }
        let url = body.url.trim();
        if protocol == "satori" && url.is_empty() {
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
            url: (!url.is_empty()).then(|| url.to_string()),
            // 页面读不到原来的令牌，所以「没写字」当「别动它」处理。
            access_token: token
                .clone()
                .or_else(|| previous.and_then(|bot| bot.access_token.clone())),
        };

        match body.index {
            Some(index) => {
                let previous = config.bots.get(index);
                if previous.is_none() {
                    return Err("这一条已经不在列表里了，刷新一次看看".into());
                }
                let entry = entry(previous);
                config.bots[index] = entry;
                Ok("已保存这条连接；改动在下次启动后生效。".to_string())
            }
            None => {
                config.bots.push(entry(None));
                Ok("已加一条连接；改动在下次启动后生效。".to_string())
            }
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

async fn save_global(State(console): State<Arc<Console>>, Json(body): Json<GlobalEdit>) -> Response {
    if body.command_prefix.iter().all(|prefix| prefix.trim().is_empty()) {
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
        config.global_filter = body.global_filter.clone();
        Ok("已保存全局设置；前缀与名单下一条消息生效，浏览器路径下次启动生效。".to_string())
    })
    .await;
    match result {
        Ok(message) => Json(json!({ "message": message })).into_response(),
        Err(message) => bad(message),
    }
}

// ==================== 日志 ====================

async fn log_history(State(console): State<Arc<Console>>) -> Response {
    Json(json!({ "lines": console.recent() })).into_response()
}

async fn log_stream(State(console): State<Arc<Console>>) -> Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use tokio::sync::broadcast::error::RecvError;

    // 订阅跟不上时跳过那一段而不是断开：面板上少几行，比整页失去连接好。
    let stream = futures_util::stream::unfold(console.subscribe(), |mut receiver| async move {
        loop {
            match receiver.recv().await {
                Ok(entry) => {
                    let event = Event::default().json_data(&entry).unwrap_or_default();
                    return Some((Ok::<_, std::convert::Infallible>(event), receiver));
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream)
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
async fn command(
    State(console): State<Arc<Console>>,
    Json(body): Json<Command>,
) -> Response {
    let input = body.input.trim().trim_start_matches('/').to_string();
    if input.is_empty() {
        return bad("写点什么再回车；例如 list、show ambient、diff oai");
    }
    match crate::plugins::ctl::execute(&console.local(), &input).await {
        Ok(output) => Json(json!({ "message": output.text })).into_response(),
        Err(message) => bad(message),
    }
}

/// 控制台自己的配置从 `[console]` 读，改它走 `/api/plugins/console/config`。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_becomes_toml_for_every_shape_a_config_holds() {
        assert_eq!(to_toml(&json!(true)).unwrap(), Toml::Boolean(true));
        assert_eq!(to_toml(&json!(7)).unwrap(), Toml::Integer(7));
        assert_eq!(to_toml(&json!(0.5)).unwrap(), Toml::Float(0.5));
        assert_eq!(
            to_toml(&json!("中")).unwrap(),
            Toml::String("中".to_string())
        );
        assert_eq!(
            to_toml(&json!([123456, 789012])).unwrap(),
            Toml::Array(vec![Toml::Integer(123456), Toml::Integer(789012)])
        );
        let table = to_toml(&json!({ "white": [1], "black": [] })).unwrap();
        assert_eq!(table["white"], Toml::Array(vec![Toml::Integer(1)]));
        assert_eq!(table["black"], Toml::Array(vec![]));
    }

    #[test]
    fn an_empty_value_is_refused_with_the_alternative() {
        let message = to_toml(&json!(null)).unwrap_err();
        assert!(
            message.contains('['),
            "要给出一条此刻能照做的写法：{message}"
        );
    }
}
