//! `acumen_plugins`：管家管这台机器人自己——插件开关、配置、状态、重启。
//!
//! 从前这件事要绕一圈：给每轮房间签发一枚一次性凭据、塞进环境变量，让模型用 bash 去调
//! `acumen --ctl "<命令>"`，再回头读 skill 才知道命令怎么写。现在它就是一件工具：参数是
//! 结构化的，回执是 `/ctl` 同一份文本，不绕 shell、不带凭据，权限也看得见——谁在问、
//! 能不能动手，都在这里一处判断。
//!
//! **谁能动手**
//! - 看（`list` / `show` / `defaults` / `diff` / `status`）任何人都行，配置里的密钥由 ctl 自己遮掉；
//! - 改（`enable` / `disable` / `set` / `reset` / `restart`）要 `[ctl].agent_control` 开着，且提问的人是号主，
//!   或者 `[ctl].guest_control` 开着（默认开，沿用部署者「我相信大家」的选择）；
//! - 重启只给号主：它会让这一轮之后的所有会话断一下，不是客人该按的按钮。
//!
//! **谁问都不开放的入口**：凭据（含 `token` / `api_key` 这类键）、`ctl` 自己（管理员名单与开关）、
//! 控制台、`oai` 里的供应商与搜索后端（改一个 `api_base` 就能把密钥寄给别人），以及会让这条
//! 通道自己失联的操作（停掉 `oai`）。这些是「谋害号主」最短的几条路，封死不碍任何日常用途，
//! 要动请去本机控制台或用 `/ctl`。
//!
//! 实际改动一律走 [`crate::plugins::ctl`]：校验、先落盘再生效、`ctl` 保命规则都是它的，这里不另写一套。

use super::AgentRun;
use crate::plugins::ctl;
use crate::plugins::{get_plugins, pending_startup};
use rig_core::completion::ToolDefinition;
use serde_json::{Value, json};

use crate::plugins::oai::LOG_TARGET;

pub(crate) const NAME: &str = "acumen_plugins";

/// 只读的动作：任何人都可以。
const READ: [&str; 5] = ["list", "show", "defaults", "diff", "status"];

/// 无论谁问都不能经这件工具停用的插件：停掉它们等于让管家或管理入口自己失联。
const KEEP_ON: [&str; 3] = ["ctl", "oai", "console"];

/// 无论谁问都不能经这件工具写的位置（插件名 + 路径前缀；空前缀表示整个插件）。
const SEALED: [(&str, &str); 5] = [
    ("ctl", ""),
    ("console", ""),
    ("oai", "providers"),
    ("oai", "search.backends"),
    ("oai", "api_key"),
];

pub(crate) fn spec() -> ToolDefinition {
    ToolDefinition::new(
        super::tools::tool_name(NAME),
        "查看和管理这台机器人（acumen）自己：有哪些插件、各自开没开、配置是什么，开关插件、改配置、看运行状态、重启。\
            等价于号主在聊天里手打 /ctl。动手之前先 list 或 show 看一眼现状，改完再 show 复核，用一句话告诉对方改了什么、什么时候生效。\
            list 列出全部插件（带一句话说明与开关状态，filter 可写 on / off / 关键词）；show 看某个插件的配置（密钥自动隐藏），path 可钻进嵌套字段；\
            defaults 看默认值，diff 看与默认值的差异；status 看版本、内存与待重启的插件；\
            enable / disable 批量开关插件（plugins 数组，英文名或中文显示名都行），多数立即生效；\
            set 改一个配置项（plugin + path + value，一次只改一项；value 写成文本：true、120、[123, 456]、\"文字\"，数组是整体替换）；\
            reset 把某项（或整个插件）恢复默认；restart 过几秒重启机器人（仅号主），让需要重启才生效的改动落地——调用后立刻写最终回复，重启会打断这一轮之后的收尾。\
            凭据、管理员名单、控制台、模型供应商与搜索后端的配置不开放，也不能停用 oai / ctl / console；要动这些请号主去本机控制台。\
            这台机器人同时服务很多群和人，改动会影响所有人，涉及推送目标、白名单这类会波及他人的项，先说清要改什么、等对方确认。"
            .to_string(),
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["list", "show", "enable", "disable", "set", "reset", "diff", "defaults", "status", "restart"]},
                "plugins": {"type": "array", "items": {"type": "string"}, "description": "enable / disable：插件名，可一次多个"},
                "plugin": {"type": "string", "description": "show / set / reset / diff / defaults：插件名（英文名或中文显示名）"},
                "path": {"type": "string", "description": "配置路径，点分，数组元素用数字下标，如 channel.white、ambient.groups、score_threshold；show / defaults 省略则看整份"},
                "value": {"type": "string", "description": "set：新值，写成 TOML 字面量文本，如 true、120、0.5、[123456, 789012]、\"文字\"；字符串字段直接写文字也行"},
                "filter": {"type": "string", "description": "list：on（只看开着的）/ off（只看关着的）/ 名称关键词"},
                "delay_seconds": {"type": "integer", "minimum": 8, "maximum": 120, "description": "restart：多少秒后重启，默认 15"},
                "reason": {"type": "string", "description": "restart：为什么重启，会写进日志"}
            },
            "required": ["action"]
        }),
    )
}

pub(crate) async fn execute(args: &Value, run: &AgentRun<'_>) -> anyhow::Result<String> {
    let seat = run
        .seat
        .ok_or_else(|| anyhow::anyhow!("这一轮没有接通机器人本体，{NAME} 用不了"))?;
    let action = args["action"].as_str().unwrap_or("").trim();
    if action.is_empty() {
        anyhow::bail!("参数错误：action 不能为空");
    }
    let ctx = ctl::maintainer(&seat.ctx);
    if READ.contains(&action) {
        return read(action, args, &ctx).await;
    }

    // —— 以下都是改动 ——
    // 公开场合（群聊搭话）里客人一律不行，不看 guest_control：叫得到它的人太多了。
    let allowed = ctl::agent_may_change(&seat.ctx, seat.asker.owner) && (seat.asker.owner || !seat.public);
    if !allowed {
        anyhow::bail!(
            "这里不让我改机器人的设置（{}）。可以看，要改请号主本人来说，或在本机控制台操作。",
            if seat.asker.owner { "号主关了智能体改设置的开关" } else { "只有号主能让我改" }
        );
    }
    info!(
        target: LOG_TARGET,
        "管家改机器人设置（{}{}）：{}",
        seat.asker.id,
        if seat.asker.owner { "，号主" } else { "" },
        // 只记动作与对象，不记 value：被拒的那些尝试里，值可能正是人家想写进去的密钥。
        super::tools::label(args)
    );
    match action {
        "enable" | "disable" => {
            let on = action == "enable";
            let names = names(args);
            if names.is_empty() {
                anyhow::bail!("参数错误：plugins 里至少写一个插件名");
            }
            if !on {
                for name in &names {
                    let plugin = ctl::resolve(name).map_err(anyhow::Error::msg)?;
                    if KEEP_ON.contains(&plugin.name) {
                        anyhow::bail!(
                            "{} 不能由我停用：停掉它我自己或管理入口就失联了。要停请号主在本机控制台操作。",
                            plugin.name
                        );
                    }
                }
            }
            say(&ctx, &format!("{} {}", if on { "on" } else { "off" }, names.join(" "))).await
        }
        "set" => {
            let plugin = plugin_arg(args)?;
            let path = args["path"].as_str().unwrap_or("").trim();
            if path.is_empty() {
                anyhow::bail!("参数错误：set 需要 path");
            }
            let value = value_text(&args["value"])?;
            sealed(plugin, path)?;
            say(&ctx, &format!("set {plugin} {path} {value}")).await
        }
        "reset" => {
            let plugin = plugin_arg(args)?;
            let path = args["path"].as_str().unwrap_or("").trim();
            sealed(plugin, path)?;
            // 整个插件一并复位会连同凭据类的项一起清掉：这几位只能逐项复位。
            if path.is_empty() && KEEP_ON.contains(&ctl::resolve(plugin).map_err(anyhow::Error::msg)?.name) {
                anyhow::bail!("{plugin} 不能整份复位，请指明要复位哪一项（path）");
            }
            say(&ctx, &format!("reset {plugin} {path} --confirm")).await
        }
        "restart" => {
            if !seat.asker.owner {
                anyhow::bail!("重启只有号主能让我做");
            }
            let delay = args["delay_seconds"].as_u64().unwrap_or(15).clamp(8, 120);
            let reason = args["reason"].as_str().unwrap_or("").trim();
            let reason = if reason.is_empty() { "管家按号主的要求重启" } else { reason };
            let delay = crate::plugins::restart::schedule(
                &seat.ctx,
                reason,
                std::time::Duration::from_secs(delay),
            )
            .map_err(anyhow::Error::msg)?;
            Ok(format!(
                "已安排 {} 秒后重启（{reason}）。现在就写最终回复；重启后会话从房间历史接着聊，这一轮不会再有回执。",
                delay.as_secs()
            ))
        }
        other => anyhow::bail!("未知操作：{other}"),
    }
}

async fn read(action: &str, args: &Value, ctx: &crate::event::Context) -> anyhow::Result<String> {
    match action {
        "list" => Ok(listing(ctx, args["filter"].as_str().unwrap_or("").trim())),
        "status" => Ok(status(ctx)),
        _ => {
            let plugin = plugin_arg(args)?;
            let path = args["path"].as_str().unwrap_or("").trim();
            let command = match action {
                "diff" => format!("diff {plugin}"),
                _ => format!("{action} {plugin} {path}"),
            };
            say(ctx, &command).await
        }
    }
}

/// 把一条 `/ctl` 命令交给 ctl，原样取回执文本。
async fn say(ctx: &crate::event::Context, command: &str) -> anyhow::Result<String> {
    ctl::execute(ctx, command.trim())
        .await
        .map(|output| output.text)
        .map_err(anyhow::Error::msg)
}

fn plugin_arg(args: &Value) -> anyhow::Result<&str> {
    let plugin = args["plugin"].as_str().unwrap_or("").trim();
    if plugin.is_empty() {
        anyhow::bail!("参数错误：需要 plugin（插件名）");
    }
    Ok(plugin)
}

/// `plugins` 数组，也容忍模型直接给一个字符串（空白或逗号分隔）。
fn names(args: &Value) -> Vec<String> {
    match &args["plugins"] {
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect(),
        Value::String(text) => text
            .split(|c: char| c.is_whitespace() || c == ',' || c == '，')
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect(),
        _ => args["plugin"]
            .as_str()
            .map(|name| vec![name.trim().to_string()])
            .unwrap_or_default(),
    }
}

/// `set` 的值交给 ctl 解析：字符串原样给（ctl 按落点类型读），别的 JSON 值转成 TOML 字面量。
fn value_text(value: &Value) -> anyhow::Result<String> {
    match value {
        Value::Null => anyhow::bail!("参数错误：set 需要 value"),
        Value::String(text) if !text.trim().is_empty() => Ok(text.trim().to_string()),
        Value::String(_) => Ok("\"\"".to_string()),
        other => {
            let converted = toml::Value::try_from(other.clone())
                .map_err(|error| anyhow::anyhow!("value 转不成配置值：{error}"))?;
            Ok(converted.to_string())
        }
    }
}

/// 封死的位置：不管谁问、也不管字面怎么写，都不经这件工具改。
fn sealed(plugin: &str, path: &str) -> anyhow::Result<()> {
    let name = ctl::resolve(plugin).map_err(anyhow::Error::msg)?.name;
    let reason = "这一项关系到凭据或管理入口，不开放给我改；请号主在本机控制台操作。";
    if path.split('.').any(ctl::sensitive) {
        anyhow::bail!("{name}.{path}：{reason}");
    }
    for (sealed_plugin, prefix) in SEALED {
        if name == sealed_plugin
            && (prefix.is_empty() || path == prefix || path.starts_with(&format!("{prefix}.")))
        {
            anyhow::bail!("{name}.{path}：{reason}");
        }
    }
    Ok(())
}

/// 插件清单：开关、名字、一句话说明，比 `/ctl list` 多一句说明——选插件靠它。
fn listing(ctx: &crate::event::Context, filter: &str) -> String {
    let cfg = ctx.config.read().unwrap();
    let key = filter.to_lowercase();
    let rows: Vec<String> = get_plugins()
        .iter()
        .filter(|plugin| {
            let on = ctl::enabled(&cfg, plugin.name);
            match key.as_str() {
                "" => true,
                "on" | "开启" => on,
                "off" | "关闭" => !on,
                _ => {
                    plugin.name.contains(&key)
                        || plugin.display_name.contains(filter)
                        || plugin.summary.contains(filter)
                }
            }
        })
        .map(|plugin| {
            let on = ctl::enabled(&cfg, plugin.name);
            format!(
                "{} {}（{}）{}：{}",
                if on { "开" } else { "关" },
                plugin.name,
                plugin.display_name,
                if on && pending_startup(plugin.name) { " · 待重启" } else { "" },
                plugin.summary
            )
        })
        .collect();
    if rows.is_empty() {
        return "没有匹配的插件".to_string();
    }
    rows.join("\n")
}

fn status(ctx: &crate::event::Context) -> String {
    let cfg = ctx.config.read().unwrap();
    let all = get_plugins();
    let on = all.iter().filter(|p| ctl::enabled(&cfg, p.name)).count();
    let pending: Vec<&str> = all
        .iter()
        .filter(|p| ctl::enabled(&cfg, p.name) && pending_startup(p.name))
        .map(|p| p.name)
        .collect();
    let mut out = format!(
        "acumen {} · 插件 {on}/{} 开启",
        env!("CARGO_PKG_VERSION"),
        all.len()
    );
    if let Some(mb) = crate::plugins::restart::current_rss_mb() {
        out.push_str(&format!(" · 内存 {mb} MB"));
    }
    if !pending.is_empty() {
        out.push_str(&format!("\n待重启生效：{}", pending.join("、")));
    }
    out
}
