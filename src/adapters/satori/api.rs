#![allow(dead_code)]

use super::{LockedWriter, message};
use crate::event::{Context, EventType};
use crate::message::Message;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use simd_json::derived::ValueObjectAccessAsScalar;
use std::time::Duration;

pub type ApiError = Box<dyn std::error::Error + Send + Sync>;

pub async fn call_action<P, R>(
    ctx: &Context,
    writer: LockedWriter,
    action: &str,
    params: P,
) -> Result<R, ApiError>
where
    P: Serialize,
    R: serde::de::DeserializeOwned,
{
    writer.call(ctx, action, params).await
}

pub async fn call_action_no_wait<P>(
    ctx: &Context,
    writer: LockedWriter,
    action: &str,
    params: P,
) -> Result<(), ApiError>
where
    P: Serialize,
{
    let _: Value = writer.call(ctx, action, params).await?;
    Ok(())
}

/// 撤回失败后隔多久再试：消息刚发出去时内核还没把它收进本地库，第一次常被回绝。
pub(crate) const RECALL_BACKOFF: [Duration; 2] =
    [Duration::from_millis(700), Duration::from_millis(1600)];

/// 这次撤回失败是不是「过一会儿再发就好」。
///
/// QQ 的撤回有两个已知的短空窗：消息刚发出、内核还没落库时回 code=8；
/// 会话里刚有动静时回 c2c custom recall beat。两者都不是权限问题，
/// 隔一秒再发一次就过了；真正的拒绝（消息不存在、超过时限、不是管事的）没有这两个记号。
pub(crate) fn recall_retryable(error: &str) -> bool {
    if error.contains("recall beat") || error.contains("timeout") || error.contains("Timeout") {
        return true;
    }
    error
        .split("code=")
        .skip(1)
        .filter_map(|tail| {
            let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<u32>().ok()
        })
        .any(|code| code == 8 || code == 100_000_041)
}

/// 撤回当前会话里的一条消息。
///
/// 撞上上一段说到的空窗时退避重试两次（共约 2.3 秒，远在平台的两分钟时限内）——
/// 配置供应商后要撤的那条「带密钥的消息」几乎必然撞上第一次。
pub async fn delete_msg(
    ctx: &Context,
    writer: LockedWriter,
    message_id: &str,
) -> Result<(), ApiError> {
    let channel = channel_id(ctx)?;
    let mut attempt = 0;
    loop {
        match delete_msg_in(ctx, writer.clone(), &channel, message_id).await {
            Ok(()) => return Ok(()),
            Err(error)
                if attempt < RECALL_BACKOFF.len() && recall_retryable(&error.to_string()) =>
            {
                tokio::time::sleep(RECALL_BACKOFF[attempt]).await;
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// 撤回指定频道里的一条消息；频道与当前上下文无关时（跟随撤回回复）用它。
pub async fn delete_msg_in(
    ctx: &Context,
    writer: LockedWriter,
    channel_id: &str,
    message_id: &str,
) -> Result<(), ApiError> {
    let _: Value = writer
        .call(
            ctx,
            "message.delete",
            json!({"channel_id": channel_id, "message_id": message_id}),
        )
        .await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct SenderInfo {
    pub nickname: Option<String>,
    pub card: Option<String>,
    #[serde(flatten)]
    pub other: std::collections::HashMap<String, simd_json::OwnedValue>,
}

#[derive(Debug, Deserialize)]
pub struct MsgData {
    pub time: i64,
    pub message_type: String,
    pub message_id: String,
    pub sender: SenderInfo,
    pub message: Message,
}

pub async fn get_msg(
    ctx: &Context,
    writer: LockedWriter,
    message_id: &str,
) -> Result<MsgData, ApiError> {
    let value: Value = writer
        .call(
            ctx,
            "message.get",
            json!({"channel_id": channel_id(ctx)?, "message_id": message_id}),
        )
        .await?;
    let resources = writer.resources();
    let channel = value.get("channel").unwrap_or(&Value::Null);
    let user = value.get("user").unwrap_or(&Value::Null);
    let member = value.get("member").unwrap_or(&Value::Null);
    Ok(MsgData {
        time: value
            .get("created_at")
            .and_then(Value::as_i64)
            .unwrap_or_default()
            / 1000,
        // Satori Channel.Type：1 是私聊（DIRECT）。
        message_type: if channel.get("type").and_then(Value::as_i64) == Some(1) {
            "private".to_string()
        } else {
            "group".to_string()
        },
        message_id: value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(message_id)
            .to_string(),
        sender: SenderInfo {
            nickname: super::account_name(user),
            card: optional_string(member.get("nick")).or_else(|| optional_string(user.get("nick"))),
            other: Default::default(),
        },
        message: message::from_content_with(
            value.get("content").and_then(Value::as_str).unwrap_or(""),
            &resources,
        ),
    })
}

#[derive(Debug, Deserialize)]
pub struct ForwardMsgData {
    pub message: Message,
}

pub async fn get_forward_msg(
    ctx: &Context,
    writer: LockedWriter,
    id: String,
) -> Result<ForwardMsgData, ApiError> {
    let value: Value = writer
        .call(
            ctx,
            "internal/get_forward",
            json!({"id": id, "channel_id": channel_id(ctx)?}),
        )
        .await?;
    let resources = writer.resources();
    let mut chain = Message::new();
    if let Some(items) = value.get("data").and_then(Value::as_array) {
        for item in items {
            let user = item.get("user").unwrap_or(&Value::Null);
            let content = message::from_content_with(
                item.get("content").and_then(Value::as_str).unwrap_or(""),
                &resources,
            );
            chain = chain.node_custom(
                user.get("id").and_then(Value::as_str).unwrap_or_default(),
                user.get("name").and_then(Value::as_str).unwrap_or(""),
                content,
            );
        }
    }
    Ok(ForwardMsgData { message: chain })
}

pub async fn set_group_special_title(
    ctx: &Context,
    writer: LockedWriter,
    group_id: &str,
    user_id: &str,
    special_title: String,
    _duration: i64,
) -> Result<(), ApiError> {
    let _: Value = writer
        .call(
            ctx,
            "internal/special_title",
            json!({
                "guild_id": group_id,
                "user_id": user_id,
                "title": special_title,
            }),
        )
        .await?;
    Ok(())
}

/// 把一条群消息设为精华，或取消它的精华。
///
/// 账号需要是群主或管理员。`message_id` 必须是本模块见过、还留在缓存里的消息
/// （刚收到的、或者从记录里取到的）；QQ 只认本地记录里的 `msgSeq` / `msgRandom`。
pub async fn set_group_essence(
    ctx: &Context,
    writer: LockedWriter,
    group_id: &str,
    message_id: &str,
    add: bool,
) -> Result<(), ApiError> {
    let _: Value = writer
        .call(
            ctx,
            "internal/essence",
            json!({
                "guild_id": group_id,
                "message_id": message_id,
                "remove": !add,
            }),
        )
        .await?;
    Ok(())
}

/// 读群身份目录：群头衔（群主、管理员、群等级）与当前戴着哪一枚。
pub async fn title_list(
    ctx: &Context,
    writer: LockedWriter,
    group_id: &str,
) -> Result<Value, ApiError> {
    writer
        .call(ctx, "internal/title_list", json!({"guild_id": group_id}))
        .await
}

/// 佩戴 / 更换 / 摘下自己头上的群身份头衔。
///
/// `params` 直接交给实现端的 `title_wear`：`role`（群主、管理员、等级、off）、
/// `name`（按目录里的名字挑）、`title`（群主专属的六字以内文字）、`title_id`
/// （指定编号）、`strategy`（`auto` / `identity` / `special`）与 `verify`。
/// 回包里 `accepted` 是 QQ 收下了，`worn` 是回读确认真的戴着；`reason` / `hint`
/// 说明没戴上的原因（普通成员戴不上群主 / 管理员不是接口错误）。
pub async fn title_wear(
    ctx: &Context,
    writer: LockedWriter,
    _group_id: &str,
    params: Value,
) -> Result<Value, ApiError> {
    writer.call(ctx, "internal/title_wear", params).await
}

/// 只读群管理里「展示成员群头衔」开关的当前状态，不写。
pub async fn get_group_title_display(
    ctx: &Context,
    writer: LockedWriter,
    group_id: &str,
) -> Result<bool, ApiError> {
    let value: Value = writer
        .call(ctx, "internal/title_display", json!({"guild_id": group_id}))
        .await?;
    Ok(value
        .get("title_open")
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

/// 打开或关闭群管理里「展示成员群头衔」的开关。
pub async fn set_group_title_display(
    ctx: &Context,
    writer: LockedWriter,
    group_id: &str,
    show: bool,
) -> Result<(), ApiError> {
    let _: Value = writer
        .call(
            ctx,
            "internal/title_display",
            json!({"guild_id": group_id, "show": show}),
        )
        .await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct GroupMemberInfo {
    pub group_id: String,
    pub user_id: String,
    pub nickname: String,
    pub card: String,
    pub sex: String,
    pub age: i32,
    pub area: String,
    pub join_time: i64,
    pub last_sent_time: i64,
    pub level: String,
    pub role: String,
    pub unfriendly: bool,
    pub title: String,
    pub title_expire_time: i64,
    pub card_changeable: bool,
}

/// 群里的管事身份：群主或管理员。
pub fn is_staff(role: &str) -> bool {
    matches!(role, "owner" | "admin" | "administrator")
}

/// 账号自己在这个群里的身份，查一次记十分钟：要罚人、撤别人的消息，先得罚得动。
/// 查不到时给空串（按普通成员算）。
pub async fn own_role(ctx: &Context, writer: LockedWriter, group_id: &str) -> String {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    static ROLES: OnceLock<Mutex<HashMap<String, (String, Instant)>>> = OnceLock::new();
    let roles = || {
        ROLES
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    let key = format!("{}/{group_id}", ctx.bot.self_id());
    if let Some((role, at)) = roles().get(&key)
        && at.elapsed() < Duration::from_mins(10)
    {
        return role.clone();
    }
    let role = get_group_member_info(ctx, writer, group_id, &ctx.bot.self_id(), false)
        .await
        .map(|info| info.role)
        .unwrap_or_default();
    roles().insert(key, (role.clone(), Instant::now()));
    role
}

pub async fn get_group_member_info(
    ctx: &Context,
    writer: LockedWriter,
    group_id: &str,
    user_id: &str,
    _no_cache: bool,
) -> Result<GroupMemberInfo, ApiError> {
    let value: Value = writer
        .call(
            ctx,
            "guild.member.get",
            json!({"guild_id": group_id, "user_id": user_id}),
        )
        .await?;
    let user = value.get("user").unwrap_or(&Value::Null);
    let role = value
        .get("roles")
        .and_then(Value::as_array)
        .and_then(|roles| roles.first())
        .and_then(|role| role.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("member")
        .to_string();
    Ok(GroupMemberInfo {
        group_id: group_id.to_string(),
        user_id: user_id.to_string(),
        nickname: super::account_name(user).unwrap_or_default(),
        card: value
            .get("nick")
            .or_else(|| value.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        sex: "unknown".to_string(),
        age: 0,
        area: String::new(),
        join_time: value
            .get("joined_at")
            .and_then(Value::as_i64)
            .unwrap_or_default()
            / 1000,
        last_sent_time: value
            .get("last_sent_at")
            .and_then(Value::as_i64)
            .unwrap_or_default()
            / 1000,
        level: value
            .get("level")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        role,
        unfriendly: false,
        title: value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        title_expire_time: value
            .get("title_expire_time")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        card_changeable: true,
    })
}

#[derive(Debug, Deserialize)]
pub struct LoginInfo {
    pub user_id: String,
    pub nickname: String,
}

pub async fn get_login_info(ctx: &Context, writer: LockedWriter) -> Result<LoginInfo, ApiError> {
    let value: Value = writer.call(ctx, "login.get", json!({})).await?;
    let user = value.get("user").unwrap_or(&Value::Null);
    Ok(LoginInfo {
        user_id: user
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        nickname: user
            .get("name")
            .or_else(|| user.get("nick"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

#[derive(Debug, Deserialize)]
pub struct GroupInfo {
    pub group_id: String,
    pub group_name: String,
    pub member_count: Option<i32>,
    pub max_member_count: Option<i32>,
}

/// 单个群的资料（`guild.get`）。
///
/// 只为一个群名去翻 `guild.list` 那张分页整表不划算，而群名恰恰是每条群消息的
/// 上下文里都用得上的一样东西。
pub async fn get_guild_info(
    ctx: &Context,
    writer: LockedWriter,
    group_id: &str,
) -> Result<GroupInfo, ApiError> {
    let value: Value = writer
        .call(ctx, "guild.get", json!({"guild_id": group_id}))
        .await?;
    Ok(GroupInfo {
        group_id: group_id.to_string(),
        group_name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        member_count: value
            .get("member_count")
            .and_then(Value::as_i64)
            .map(|n| n as i32),
        max_member_count: value
            .get("max_member_count")
            .and_then(Value::as_i64)
            .map(|n| n as i32),
    })
}

/// `guild.list` 是标准分页列表：跟着 `next` 令牌翻到底，否则群多时会漏群。
pub async fn get_group_list(
    ctx: &Context,
    writer: LockedWriter,
    _no_cache: bool,
) -> Result<Vec<GroupInfo>, ApiError> {
    const MAX_PAGES: usize = 64;
    let mut out = Vec::new();
    let mut next: Option<String> = None;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..MAX_PAGES {
        let params = match &next {
            Some(cursor) => json!({"next": cursor}),
            None => json!({}),
        };
        let value: Value = writer.call(ctx, "guild.list", params).await?;
        out.extend(
            value
                .get("data")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|guild| {
                    Some(GroupInfo {
                        group_id: guild
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|id| !id.is_empty())?
                            .to_string(),
                        group_name: guild
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        member_count: None,
                        max_member_count: None,
                    })
                }),
        );
        next = value
            .get("next")
            .and_then(Value::as_str)
            .filter(|cursor| !cursor.is_empty())
            .map(str::to_owned);
        match next.as_ref() {
            None => return Ok(out),
            Some(cursor) if !seen.insert(cursor.clone()) => {
                return Err(format!("Satori guild.list 重复分页令牌: {cursor}").into());
            }
            _ => {}
        }
    }
    Err(format!("Satori guild.list 超过 {MAX_PAGES} 页，拒绝返回不完整群列表").into())
}

pub async fn upload_file(
    ctx: &Context,
    writer: LockedWriter,
    group_id: Option<&str>,
    user_id: Option<&str>,
    file: &str,
    name: &str,
) -> Result<(), ApiError> {
    let data = tokio::fs::read(file).await?;
    let mime = if name.ends_with(".json") {
        "application/json"
    } else if name.ends_with(".txt") || name.ends_with(".md") {
        "text/plain; charset=utf-8"
    } else {
        "application/octet-stream"
    };
    let uploaded = writer.upload(ctx, data, name, mime).await?;
    let resource = uploaded
        .get("file")
        .and_then(Value::as_str)
        .ok_or("Satori upload.create 未返回 file 资源")?;
    super::send_msg(
        ctx,
        writer,
        group_id,
        user_id,
        Message::new().file(resource, Some(name)),
    )
    .await
}

pub async fn set_msg_emoji_like(
    ctx: &Context,
    writer: LockedWriter,
    message_id: &str,
    emoji_id: &str,
    set: bool,
) -> Result<(), ApiError> {
    let method = if set {
        "reaction.create"
    } else {
        "reaction.delete"
    };
    let _: Value = writer
        .call(
            ctx,
            method,
            json!({
                "channel_id": channel_id(ctx)?,
                "message_id": message_id,
                "emoji_id": emoji_id,
            }),
        )
        .await?;
    Ok(())
}

/// 本号此刻能不能在这个群 @全体成员。satori-qq 与 satori-wx 的 `internal/at_all_quota` 同名同形：
/// QQ 只让群主、管理员发并按天限次；微信只看身份、不限次（两个次数是 `None`）。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AtAllQuota {
    pub allowed: bool,
    /// 本号今天还剩几次；`None` 是平台不限次或没说。
    #[serde(default)]
    pub remaining: Option<u32>,
    /// 全群今天还剩几次。
    #[serde(default)]
    pub group_remaining: Option<u32>,
    /// 不能发时平台给的说法。
    #[serde(default)]
    pub reason: String,
}

impl AtAllQuota {
    /// 现在发得出去：有权限，次数（如果有）也没用完。
    pub fn usable(&self) -> bool {
        self.allowed && self.remaining != Some(0) && self.group_remaining != Some(0)
    }

    /// 发不出去时给人看的一句话。
    pub fn why_not(&self) -> String {
        if !self.allowed {
            return if self.reason.is_empty() {
                "没有 @全体成员 的权限".into()
            } else {
                self.reason.clone()
            };
        }
        "今天的 @全体成员 次数用完了".into()
    }
}

/// 问实现端这个群里能不能 @全体成员。只在真要发之前问：QQ 那边每问一次都是一次真请求。
/// 实现端没有这个方法（旧版本）时报 `unsupported_method`。
pub async fn at_all_quota(
    ctx: &Context,
    writer: LockedWriter,
    group_id: &str,
) -> Result<AtAllQuota, ApiError> {
    writer
        .call(
            ctx,
            "internal/at_all_quota",
            json!({"channel_id": group_id}),
        )
        .await
}

/// [`send_at_all`] 的结果。
#[derive(Debug, Clone)]
pub struct AtAllSent {
    /// 实现端分配的第一条消息 ID。
    pub message_id: Option<String>,
    /// 为什么没带上 @全体成员（`None` = 带上了）。
    pub skipped: Option<String>,
}

/// 发一条想 @全体成员 的群消息：先问额度，能用就在开头加上 @全体成员 再发；不能用（没权限、
/// 次数用完、实现端问不出来）就照常发、不带它，并在 `skipped` 里说明原因。
///
/// 问不出来时宁可不带：@全体成员 会打扰整个群，没有确认过能用就不冒这个险。
pub async fn send_at_all(
    ctx: &Context,
    writer: LockedWriter,
    group_id: &str,
    message: Message,
) -> Result<AtAllSent, ApiError> {
    let skipped = match at_all_quota(ctx, writer.clone(), group_id).await {
        Ok(quota) if quota.usable() => None,
        Ok(quota) => Some(quota.why_not()),
        Err(error) => Some(format!("问不到 @全体成员 的额度：{error}")),
    };
    let message = if skipped.is_none() {
        let mut head = Message::new().at_all().text("\n");
        head.0.extend(message.0);
        head
    } else {
        message
    };
    let message_id = super::send_msg_id(ctx, writer, Some(group_id), None, message).await?;
    Ok(AtAllSent {
        message_id,
        skipped,
    })
}

pub(crate) fn channel_id(ctx: &Context) -> Result<String, ApiError> {
    let event = match &ctx.event {
        EventType::Satori(event) => Some(event),
        EventType::BeforeSend(packet) => packet.original_event.as_ref(),
        EventType::Init => None,
    }
    .ok_or("当前上下文没有 Satori 频道")?;
    event
        .get_str("channel_id")
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "当前上下文缺少 Satori channel_id".into())
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}
