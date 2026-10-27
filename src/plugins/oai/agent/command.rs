//! `acumen_command`：把一条 acumen 插件指令当作群里那个人亲手发的一样执行。
//!
//! acumen 有一整套插件指令（`/help` 里那二十几个插件的几百条用法：点歌、词云、统计、
//! 潜水名单、自动回复、加精、二维码、GIF、群文件……）。这些能力早就写在插件里了，
//! 智能体却够不着——它只会聊天，遇到「帮我把上周发言最多的十个人列出来」只能凭印象
//! 编一段。
//!
//! 这里补上的就是那条缝：把一句话装成一条**真实的群消息事件**（发送者是提要求的那个人，
//! 频道就是眼前这个群），交给插件流水线跑一遍。于是
//!
//! - 权限判断照旧走各插件自己那一套：群管理指令仍然只认群主、管理员与号主，
//!   普通群友让智能体去踢人照样踢不动；
//! - 冷却、限流、额度、交互式等待（「潜水 踢 1-20」之后回「确认」）全都是插件原本的
//!   那一套，不需要为智能体再写一份；
//! - 插件发出去的图、文件、长图、合并转发都是它自己的排版，一次到位。
//!
//! 唯一需要补的是**回执**：插件自己调用发送接口，工具这一侧拿不到返回值。所以适配器
//! 记了一份最近的出站流水（[`crate::adapters::satori::outbound_since`]），这里把这一趟
//! 真正发出去的内容交回给模型——它才知道事情办成了没有、说了什么，不必再重复发一遍。
//!
//! 会自己接话的两个插件（`oai` 与 `ambient`）认 `ctx.agent_command` 这一位，看到代办
//! 指令就只是路过：否则「/搭话 帮我看下」会变成智能体自己叫自己。

use super::AgentRun;
use crate::adapters::satori::{outbound_mark, outbound_since};
use crate::event::{Context, EventType};
use crate::command::get_prefixes;
use rig_core::completion::ToolDefinition;
use serde_json::{Value, json};

/// 工具名。
pub(crate) const NAME: &str = "acumen_command";

/// 一条指令最多这么长：插件指令都是短句，超出的多半是模型把整篇说明抄了进来。
const MAX_LINE: usize = 2_000;

pub(crate) fn spec() -> ToolDefinition {
    ToolDefinition::new(
        super::tools::tool_name(NAME),
         "把一条 acumen 插件指令当成群里这个人亲手发的一样执行，返回插件真正发出去的内容。\
指令就是 /help 里那些（帮助、ctl、点歌、词云、统计、潜水、自动回复、加精、二维码、GIF、群文件、截图……），\
前缀可省。权限照旧：群管理类指令只有群主、管理员与号主按得动，别人叫你办这类事，光调用会得到一句「没有权限」——\
那就照实告诉他，别换个法子硬来。执行完插件自己会把结果发进群、私聊或别处，回执里列着发了什么，\
不要再用 satori_action 把同样的内容发一遍；最终输出 [silent] 即可。"
            .to_string(),
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "要执行的指令，如「潜水 30」「点歌 晴天」「本群本周发言排行榜 10」「ctl show ambient」。前缀可省，会自动补上"
                },
                "reply_to": {
                    "type": "string",
                    "description": "让这条指令引用一条消息（这条消息的 message_id）。收表情、加精、转链接这类指令要的就是被引用的那条"
                },
                "as_peer": {
                    "type": "boolean",
                    "description": "以对面那个人（私聊里的对方）的身份执行，默认以这一轮的提问者执行。只在私聊里用得上"
                }
            },
            "required": ["command"]
        }),
    )
}

/// 跑一条指令。
pub(crate) async fn run(args: &Value, run: &AgentRun<'_>) -> anyhow::Result<String> {
    let Some(seat) = run.seat else {
        anyhow::bail!("这一轮没有办事台，执行不了插件指令");
    };
    let raw = args["command"].as_str().unwrap_or("").trim();
    if raw.is_empty() {
        anyhow::bail!("参数错误：command 不能为空");
    }
    if raw.chars().count() > MAX_LINE {
        anyhow::bail!("指令太长了（超过 {MAX_LINE} 字），插件指令都是一句话");
    }
    let line = with_prefix(&seat.ctx, raw);
    // 会唤起智能体自己的那几条不进流水线：代办一条「/搭话」等于让机器人自问自答。
    let head = strip_prefixes(&seat.ctx, &line);
    let word = head
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_start_matches('/')
        .to_ascii_lowercase();
    if matches!(word.as_str(), "oai" | "搭话" | "ambient") {
        anyhow::bail!(
            "「{line}」是叫醒你自己或另一个智能体的指令，不在这一层代跑；要说话直接说，要办事用别的指令"
        );
    }

    let me = seat.ctx.bot.self_id();
    let self_id = me.clone();
    let asker = if args["as_peer"].as_bool().unwrap_or(false) {
        peer_of(&seat.ctx).unwrap_or_else(|| seat.asker.id.clone())
    } else {
        seat.asker.id.clone()
    };
    let name = if asker == seat.asker.id {
        seat.asker.name.clone()
    } else {
        String::new()
    };
    let group = seat.group.clone();
    let channel = match &group {
        Some(group) => group.clone(),
        None => {
            // 私聊的频道号就是对面那个人；房间那一侧也是这么拼的。
            crate::plugins::oai::chat::direct_channel(&seat.ctx.bot.platform, &asker)
        }
    };
    let role = match &group {
        Some(group) => member_role(seat, group, &asker).await,
        None => "member".to_string(),
    };
    let manual_self = asker == self_id && seat.asker.owner;

    // 用频道里最新那条真消息的号，而不是编一个：不少插件回复时会引用触发消息
    // （帮助卡、媒体转换、加精……），引用一个平台不存在的号会让整条消息发不出去。
    let message_id = latest_message_id(seat, &channel)
        .await
        .unwrap_or_else(|| format!("acumen-command-{}", chrono::Local::now().timestamp_millis()));

    let mut message = vec![json!({"type": "text", "data": {"text": line}})];
    if let Some(id) = args["reply_to"].as_str().filter(|id| !id.trim().is_empty()) {
        message.push(json!({"type": "reply", "data": {"message_id": id.trim()}}));
    }

    let event = json!({
        "id": 1,
        "type": "message-created",
        "platform": seat.ctx.bot.platform,
        "self_id": self_id,
        "timestamp": chrono::Local::now().timestamp_millis(),
        "post_type": "message",
        "message_type": if group.is_some() { "group" } else { "private" },
        "sub_type": "normal",
        "message_id": message_id,
        "channel_id": channel,
        "guild_id": group,
        "user_id": asker,
        "raw_message": line,
        "message": message,
        "sender": {
            "id": asker,
            "name": if name.is_empty() { asker_name(&seat.ctx, &asker) } else { name.clone() },
            "nick": "",
            "card": "",
            "role": role,
        },
        "manual_self": manual_self,
        "login": {"sn": 1, "platform": seat.ctx.bot.platform, "user": {"id": self_id}},
        "_satori": {
            "login": {"sn": 1, "platform": seat.ctx.bot.platform, "user": {"id": self_id}},
            "referrer": null,
        },
    });
    let event = simd_json::serde::to_owned_value(event)
        .map_err(|error| anyhow::anyhow!("装不成一条消息事件：{error}"))?;

    let ctx = Context {
        event: EventType::Satori(event),
        config: seat.ctx.config.clone(),
        config_save_lock: seat.ctx.config_save_lock.clone(),
        db: seat.ctx.db.clone(),
        scheduler: seat.ctx.scheduler.clone(),
        matcher: seat.ctx.matcher.clone(),
        config_path: seat.ctx.config_path.clone(),
        bot: seat.ctx.bot.clone(),
        agent_command: true,
    };
    // 交互式指令（「潜水 踢 1-20」之后回「确认」、欢迎放行之类）靠 matcher 接下一句。
    // 正常事件在适配器那一侧先过 matcher，这里自己派一次，代办的这句才进得了同一个等待。
    let mut ctx = ctx;
    if let EventType::Satori(event) = &ctx.event
        && let Some(next) = ctx.matcher.dispatch(event.clone()) {
            ctx.event = EventType::Satori(next);
        }
    let mark = outbound_mark();
    crate::plugins::run(ctx, seat.writer.clone())
        .await
        .map_err(|error| anyhow::anyhow!("插件流水线报错：{error}"))?;
    let sent = outbound_since(mark);
    if sent.is_empty() {
        return Ok(format!(
            "「{line}」跑完了，但没有任何插件往外发东西：多半是指令名不对（用 /help 看写法），\
或者那个插件没开（用 acumen_plugins list 看一眼）。这条指令没有产生任何可见结果。"
        ));
    }
    let mut out = format!(
        "已在{}以{}的身份执行「{line}」。插件自己发出去了 {} 条（不用你再发一遍）：",
        if group.is_some() { "本群" } else { "这个私聊" },
        if seat.asker.owner { "号主" } else { "提问的人" },
        sent.len()
    );
    for (index, item) in sent.iter().enumerate() {
        let where_to = if group.as_deref() == Some(item.channel.as_str()) {
            String::new()
        } else {
            format!("（发到了 {}）", item.channel)
        };
        let text = item.text.trim();
        let text: String = if text.is_empty() {
            "（只有图或文件，没有文字）".to_string()
        } else {
            text.chars().take(400).collect()
        };
        out.push_str(&format!("\n{}. {text}{where_to}", index + 1));
    }
    out.push_str("\n最终输出 [silent]；插件已经把它摆到群里/私聊里了。");
    Ok(out)
}

/// 频道里最新一条消息的 ID；拿不到返回 `None`（那就用一个合成号，能跑但引用会失败）。
async fn latest_message_id(seat: &super::Seat, channel: &str) -> Option<String> {
    let listed: Value = tokio::time::timeout(
        std::time::Duration::from_secs(6),
        seat.writer
            .call(&seat.ctx, "message.list", json!({"channel_id": channel, "limit": 1})),
    )
    .await
    .ok()?
    .ok()?;
    listed
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("id").or_else(|| item.get("message_id")))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// 补上前缀：没写前缀的指令照默认前缀补，`#` `-` 这类符号指令原样放过。
fn with_prefix(ctx: &Context, raw: &str) -> String {
    let raw = raw.trim();
    let prefixes = get_prefixes(ctx);
    if prefixes.iter().any(|prefix| {
        !prefix.is_empty() && raw.starts_with(prefix.as_str())
    }) || raw.starts_with(['#', '-', '~', '～'])
    {
        return raw.to_string();
    }
    match prefixes.first() {
        Some(prefix) => format!("{prefix}{raw}"),
        None => raw.to_string(),
    }
}

/// 去掉前缀之后的那一截，用来认指令头。
fn strip_prefixes<'a>(ctx: &Context, line: &'a str) -> &'a str {
    let mut text = line.trim();
    for prefix in get_prefixes(ctx) {
        if !prefix.is_empty()
            && let Some(rest) = text.strip_prefix(prefix.as_str())
        {
            text = rest.trim_start();
            break;
        }
    }
    text
}

fn asker_name(ctx: &Context, user_id: &str) -> String {
    ctx.as_message()
        .map(|event| {
            if event.user_id() == user_id {
                event.sender_name().to_string()
            } else {
                String::new()
            }
        })
        .unwrap_or_default()
}

fn peer_of(ctx: &Context) -> Option<String> {
    let event = ctx.as_message()?;
    if event.is_group() {
        return None;
    }
    let channel = event.channel_id();
    let peer = channel.strip_prefix("private:").unwrap_or(channel);
    (!peer.is_empty()).then(|| peer.to_string())
}

/// 提问的人在这个群里是什么身份。拿不到就按普通成员算——多数指令不看它。
async fn member_role(seat: &super::Seat, group: &str, user_id: &str) -> String {
    let member: Value = match seat
        .writer
        .call(
            &seat.ctx,
            "guild.member.get",
            json!({"guild_id": group, "user_id": user_id}),
        )
        .await
    {
        Ok(member) => member,
        Err(_) => return "member".to_string(),
    };
    if let Some(role) = member.get("role").and_then(Value::as_str) {
        return role.to_string();
    }
    if let Some(id) = member
        .get("roles")
        .and_then(Value::as_array)
        .and_then(|roles| roles.first())
        .and_then(|role| role.get("id"))
        .and_then(Value::as_str)
    {
        return id.to_string();
    }
    "member".to_string()
}
