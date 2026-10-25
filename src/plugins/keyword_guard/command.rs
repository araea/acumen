//! 「违禁词 …」指令：看规则、加词、改动作与时长、停用、删除、恢复、试一句。
//!
//! 全都只给管得着的人用：本群规则要群主、管理员（managers = admin 时只有机器人管理员），
//! 全局规则只有号主与 ctl.admins。普通群成员连词表都看不到——否则等于把「怎么绕过去」
//! 的教学材料发在群里。

use super::book::{Action, BOOK, Book, Mode, Rule, regex};
use super::{Config, done_preview, notice_lifetime, notice_preview};
use crate::adapters::satori::{LockedWriter, api, send_msg};
use crate::command::{CommandMatch, extract_text_arg};
use crate::event::Context;
use crate::plugins::PluginError;
use simd_json::derived::ValueObjectAccessAsScalar;

const MAX_SHOW: usize = 40;
const MAX_TEXT: usize = 3500;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scope {
    Local,
    Global,
}

/// 处理一条「违禁词 …」。调用方已经确认它是一条指令。
pub(super) async fn run(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    group: Option<&str>,
    matched: CommandMatch,
) -> Result<(), PluginError> {
    let message = ctx.as_message();
    let user = message
        .as_ref()
        .map(crate::event::MessageEvent::user_id)
        .unwrap_or("");
    let name = message
        .as_ref()
        .map(crate::event::MessageEvent::sender_name)
        .unwrap_or("");
    let raw = extract_text_arg(&matched.args);
    let (scope, rest) = scope_of(&raw, group);
    let (verb, rest) = split_verb(rest);

    // 权限：先挡人，再谈内容。
    let allowed = match scope {
        Scope::Global => crate::access::is_manager(ctx),
        Scope::Local => group.is_some() && may_manage(ctx, config),
    };
    if !allowed {
        let why = match scope {
            Scope::Global => "全局规则只有号主（或 ctl.admins 里的管理员）能看、能改。",
            Scope::Local if group.is_none() => {
                "本群规则要到群里改。想看全局规则，发「违禁词 全局」。"
            }
            Scope::Local => "违禁词只有本群群主、管理员能看、能改。",
        };
        return say(ctx, writer, group, user, why).await;
    }

    match verb {
        "" | "列表" | "查看" | "状态" => list(ctx, writer, config, scope, group, user).await,
        "添加" | "加" | "新增" => {
            add(ctx, writer, config, scope, group, user, rest, false).await
        }
        "正则添加" | "添加正则" | "正则" => {
            add(ctx, writer, config, scope, group, user, rest, true).await
        }
        "删除" | "删" | "移除" => delete(ctx, writer, config, scope, group, user, rest).await,
        "改" | "设置" | "动作" => change(ctx, writer, config, scope, group, user, rest).await,
        "模式" => mode(ctx, writer, config, scope, group, user, rest).await,
        "全含" | "都要" | "全部" => {
            require_all(ctx, writer, config, scope, group, user, rest).await
        }
        "停用" | "关闭" | "关" => {
            toggle(ctx, writer, config, scope, group, user, rest, false).await
        }
        "启用" | "打开" | "开" => {
            toggle(ctx, writer, config, scope, group, user, rest, true).await
        }
        "试" | "测试" | "检查" => {
            trial(ctx, writer, config, scope, group, user, name, rest).await
        }
        "恢复" | "撤销" | "找回" => restore(ctx, writer, config, scope, group, user).await,
        "帮助" | "用法" | "?" => say(ctx, writer, group, user, help()).await,
        other => {
            say(
                ctx,
                writer,
                group,
                user,
                &format!("不认识的用法「{other}」。发「违禁词 帮助」看全部。"),
            )
            .await
        }
    }
}

fn may_manage(ctx: &Context, config: &Config) -> bool {
    if crate::access::is_manager(ctx) {
        return true;
    }
    config.managers == "staff"
        && ctx
            .as_message()
            .is_some_and(|message| message.sender_role().is_some_and(api::is_staff))
}

/// 第一个词是「全局」或「本群」时就切范围；「全局添加」这种连写也认。
fn scope_of<'a>(raw: &'a str, group: Option<&str>) -> (Scope, &'a str) {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix("全局") {
        return (Scope::Global, rest.trim_start());
    }
    if let Some(rest) = raw.strip_prefix("本群") {
        return (Scope::Local, rest.trim_start());
    }
    (
        if group.is_some() {
            Scope::Local
        } else {
            Scope::Global
        },
        raw,
    )
}

fn split_verb(rest: &str) -> (&str, &str) {
    let rest = rest.trim();
    match rest.split_once(char::is_whitespace) {
        Some((verb, tail)) => (verb, tail.trim()),
        None => (rest, ""),
    }
}

async fn say(
    ctx: &Context,
    writer: &LockedWriter,
    group: Option<&str>,
    user: &str,
    text: &str,
) -> Result<(), PluginError> {
    let text = crate::storage::truncate_chars(text.trim(), MAX_TEXT);
    if text.is_empty() {
        return Ok(());
    }
    send_msg(ctx, writer.clone(), group, Some(user), text).await?;
    Ok(())
}

// ==================== 列表 ====================

async fn list(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    scope: Scope,
    group: Option<&str>,
    user: &str,
) -> Result<(), PluginError> {
    let (local, global, shared) = BOOK
        .read(|book| {
            (
                group
                    .map(|group| book.local(group).into_iter().cloned().collect::<Vec<_>>())
                    .unwrap_or_default(),
                book.global().into_iter().cloned().collect::<Vec<_>>(),
                group
                    .map(|group| {
                        book.rules
                            .iter()
                            .filter(|rule| {
                                !rule.global() && !rule.belongs_to(group) && rule.covers(group)
                            })
                            .cloned()
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
            )
        })
        .await;

    let mut lines = Vec::new();
    match scope {
        Scope::Local => {
            lines.push(format!(
                "违禁词 · 本群 {} 条{}",
                local.len(),
                if local.is_empty() {
                    ""
                } else {
                    "（群主、管理员可改）"
                }
            ));
            if local.is_empty() {
                lines
                    .push("还没有本群的规则。发「违禁词 添加 加微信 撤回禁言」就能加一条。".into());
            }
            for (index, rule) in local.iter().take(MAX_SHOW).enumerate() {
                lines.push(format!(
                    "{}. {}",
                    index + 1,
                    rule.summary(config.mute_seconds)
                ));
            }
            if local.len() > MAX_SHOW {
                lines.push(format!(
                    "……还有 {} 条，先删掉一些再看。",
                    local.len() - MAX_SHOW
                ));
            }
            if !global.is_empty() {
                let brief = global
                    .iter()
                    .take(6)
                    .map(|rule| rule.summary(config.mute_seconds))
                    .collect::<Vec<_>>()
                    .join("；");
                lines.push(format!(
                    "全局 {} 条（号主维护，本群只读）：{}{}",
                    global.len(),
                    brief,
                    if global.len() > 6 { "；……" } else { "" }
                ));
            }
            if !shared.is_empty() {
                let brief = shared
                    .iter()
                    .take(4)
                    .map(|rule| rule.summary(config.mute_seconds))
                    .collect::<Vec<_>>()
                    .join("；");
                lines.push(format!(
                    "多群共用 {} 条（本群只读）：{}{}",
                    shared.len(),
                    brief,
                    if shared.len() > 4 { "；……" } else { "" }
                ));
            }
        }
        Scope::Global => {
            lines.push(format!("违禁词 · 全局 {} 条（所有群生效）", global.len()));
            if global.is_empty() {
                lines.push(
                    "还没有全局规则。发「违禁词 全局添加 加微信 撤回禁言」就能加一条。".into(),
                );
            }
            for (index, rule) in global.iter().take(MAX_SHOW).enumerate() {
                lines.push(format!(
                    "{}. {}",
                    index + 1,
                    rule.summary(config.mute_seconds)
                ));
            }
            if global.len() > MAX_SHOW {
                lines.push(format!("……还有 {} 条。", global.len() - MAX_SHOW));
            }
            if let Some(group) = group {
                let local = BOOK.read(|book| book.local(group).len()).await;
                if local > 0 {
                    lines.push(format!("本群另有 {local} 条自己的规则，发「违禁词」看。"));
                }
            }
        }
    }
    lines.push(settings_line(config));
    if let Some(group) = group
        && scope == Scope::Local
    {
        lines.push(
            if api::is_staff(&api::own_role(ctx, writer.clone(), group).await) {
                "机器人：本群管理员，撤得动、禁得动。".to_string()
            } else {
                "机器人：不是本群管理员，撤回与禁言会被平台拒绝；请给它管理员。".to_string()
            },
        );
    }
    say(ctx, writer, group, user, &lines.join("\n")).await
}

fn settings_line(config: &Config) -> String {
    let mut bits = vec![format!("提示：{}", notice_lifetime(config))];
    if config.exempt_staff {
        bits.push("群主管理员不动手".into());
    }
    if config.repeat_seconds > 0 {
        bits.push(format!(
            "同一个人 {}内只提示一次",
            crate::clock::duration_text(config.repeat_seconds)
        ));
    }
    bits.push(format!(
        "谁能改：{}",
        if config.managers == "admin" {
            "只有机器人管理员"
        } else {
            "群主、管理员与机器人管理员"
        }
    ));
    bits.join("；")
}

// ==================== 加、改、删 ====================

#[allow(clippy::too_many_arguments)]
async fn add(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    scope: Scope,
    group: Option<&str>,
    user: &str,
    rest: &str,
    force_regex: bool,
) -> Result<(), PluginError> {
    if scope == Scope::Local && group.is_none() {
        return say(ctx, writer, group, user, "本群规则要到群里加。").await;
    }
    let spec = match parse_add(rest, config, force_regex) {
        Ok(spec) => spec,
        Err(why) => return say(ctx, writer, group, user, &why).await,
    };
    let now = chrono::Utc::now().timestamp();
    let mut rule = Rule {
        words: spec.words,
        mode: spec.mode,
        action: spec.action,
        mute_seconds: spec.mute,
        groups: match scope {
            Scope::Local => vec![group.unwrap_or_default().to_string()],
            Scope::Global => Vec::new(),
        },
        author: user.to_string(),
        created_at: now,
        updated_at: now,
        ..Default::default()
    };
    rule.tidy();
    if let Some(why) = rule.problem(config.mute_seconds) {
        return say(ctx, writer, group, user, &why).await;
    }
    let words = rule.words.clone();
    let action = rule.action;
    let mute = rule.mute_seconds.unwrap_or(config.mute_seconds);
    let added = BOOK
        .update(|book| match book.add(rule) {
            Ok(id) => (Ok(id), true),
            Err(why) => (Err(why), false),
        })
        .await;
    let id = match added {
        Ok(id) => id,
        Err(why) => return say(ctx, writer, group, user, &why).await,
    };
    let number = BOOK
        .read(|book| number_of(book, scope, group.unwrap_or(""), id))
        .await
        .unwrap_or(0);
    let mut line = format!(
        "记下了：{}「{}」→ {}",
        scope_name(scope, number),
        words.join("、"),
        describe(action, mute)
    );
    if config.notice.is_silent() {
        line.push_str("。提示是静默的，处理了也不说话。");
    } else {
        line.push_str(&format!(
            "。发「违禁词 试 一句话」可以先看效果（提示{}）。",
            notice_lifetime(config)
        ));
    }
    say(ctx, writer, group, user, &line).await
}

struct Spec {
    words: Vec<String>,
    mode: Mode,
    action: Action,
    mute: Option<u64>,
}

fn parse_add(rest: &str, config: &Config, force_regex: bool) -> Result<Spec, String> {
    let mut tokens: Vec<&str> = rest.split_whitespace().collect();
    if tokens.is_empty() {
        return Err(
            "要加什么词？例：「违禁词 添加 加微信 撤回禁言 10分钟」；多个词用「、」隔开。".into(),
        );
    }
    // 从右往左摘掉时长、动作：这样「加微信 撤回禁言 2小时」与「加微信 2小时」都认。
    let mut mute = None;
    if tokens.len() > 1
        && let Some(seconds) = parse_duration(tokens[tokens.len() - 1])
    {
        mute = (seconds > 0).then_some(seconds);
        tokens.pop();
    }
    let mut action = config.default_action;
    if tokens.len() > 1
        && let Some(parsed) = Action::parse(tokens[tokens.len() - 1])
    {
        action = parsed;
        tokens.pop();
    }
    if !action.mutes() && mute.is_some() {
        return Err(format!(
            "「{}」不用禁言时长，去掉最后的那个时间。",
            action.label()
        ));
    }
    if tokens.is_empty() {
        return Err("只写了动作，没有词。".into());
    }
    let raw = tokens.join(if force_regex { " " } else { "" });
    let (words, mode) = split_words(&raw, force_regex)?;
    Ok(Spec {
        words,
        mode,
        action,
        mute,
    })
}

fn split_words(raw: &str, force_regex: bool) -> Result<(Vec<String>, Mode), String> {
    let trimmed = raw.trim();
    if force_regex || (trimmed.starts_with('/') && trimmed.ends_with('/') && trimmed.len() > 2) {
        let pattern = trimmed
            .strip_prefix('/')
            .and_then(|text| text.strip_suffix('/'))
            .unwrap_or(trimmed)
            .trim();
        if pattern.is_empty() {
            return Err("正则不能是空的。".into());
        }
        return Ok((vec![pattern.to_string()], Mode::Regex));
    }
    let mut words = Vec::new();
    for word in trimmed.split(['、', '，', ',']) {
        let word = word.trim();
        if word.is_empty() {
            continue;
        }
        if word.starts_with('/') && word.ends_with('/') {
            return Err(
                "要按正则比，请把整条用 /…/ 包住，或发「违禁词 正则添加 <正则> [动作] [时长]」。"
                    .into(),
            );
        }
        words.push(word.to_string());
    }
    if words.is_empty() {
        return Err("至少要有一个词。".into());
    }
    Ok((words, Mode::Contains))
}

#[allow(clippy::too_many_arguments)]
async fn change(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    scope: Scope,
    group: Option<&str>,
    user: &str,
    rest: &str,
) -> Result<(), PluginError> {
    if scope == Scope::Local && group.is_none() {
        return say(ctx, writer, group, user, "本群规则要到群里改。").await;
    }
    let place = group.unwrap_or("");
    let (number, tail) = match take_number(rest) {
        Ok(found) => found,
        Err(why) => return say(ctx, writer, group, user, &why).await,
    };
    let mut tokens = tail.split_whitespace();
    let Some(action_text) = tokens.next() else {
        return say(
            ctx,
            writer,
            group,
            user,
            "要改成什么动作？仅提醒 / 撤回 / 禁言 / 撤回禁言。例：「违禁词 改 1 禁言 2小时」",
        )
        .await;
    };
    let Some(action) = Action::parse(action_text) else {
        return say(
            ctx,
            writer,
            group,
            user,
            &format!("不认识动作「{action_text}」：只有仅提醒、撤回、禁言、撤回禁言。"),
        )
        .await;
    };
    let duration = match tokens.next() {
        Some(text) => match parse_duration(text) {
            Some(seconds) => Some(seconds),
            None => {
                return say(
                    ctx,
                    writer,
                    group,
                    user,
                    &format!("看不懂时长「{text}」：写 10分钟 / 2小时 / 1天。"),
                )
                .await;
            }
        },
        None => None,
    };
    if tokens.next().is_some() {
        return say(
            ctx,
            writer,
            group,
            user,
            "时长只能写一个，如 10分钟 / 2小时 / 1天。",
        )
        .await;
    }
    if !action.mutes() && duration.is_some() {
        return say(
            ctx,
            writer,
            group,
            user,
            &format!("「{}」不用禁言时长。", action.label()),
        )
        .await;
    }
    let result = BOOK
        .update(|book| {
            let Some(id) = id_of(book, scope, place, number) else {
                return (
                    Err(format!("没有第 {number} 条。发「违禁词」看看编号。")),
                    false,
                );
            };
            let Some(current) = book.get(id) else {
                return (Err("这条规则刚被删了。".into()), false);
            };
            let mut next = current.clone();
            next.action = action;
            if action.mutes() {
                if let Some(seconds) = duration {
                    next.mute_seconds = (seconds > 0).then_some(seconds);
                }
            } else {
                next.mute_seconds = None;
            }
            next.updated_at = chrono::Utc::now().timestamp();
            if let Some(why) = next.problem(config.mute_seconds) {
                return (Err(why), false);
            }
            let summary = next.summary(config.mute_seconds);
            if let Some(rule) = book.get_mut(id) {
                *rule = next;
            }
            (Ok(summary), true)
        })
        .await;
    match result {
        Ok(summary) => {
            say(
                ctx,
                writer,
                group,
                user,
                &format!("{number}. 改成：{summary}"),
            )
            .await
        }
        Err(why) => say(ctx, writer, group, user, &why).await,
    }
}

async fn mode(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    scope: Scope,
    group: Option<&str>,
    user: &str,
    rest: &str,
) -> Result<(), PluginError> {
    if scope == Scope::Local && group.is_none() {
        return say(ctx, writer, group, user, "本群规则要到群里改。").await;
    }
    let place = group.unwrap_or("");
    let (number, tail) = match take_number(rest) {
        Ok(found) => found,
        Err(why) => return say(ctx, writer, group, user, &why).await,
    };
    let Some(mode) = Mode::parse(tail.split_whitespace().next().unwrap_or("")) else {
        return say(ctx, writer, group, user, "模式只有「包含」与「正则」。").await;
    };
    let result = BOOK
        .update(|book| {
            let Some(id) = id_of(book, scope, place, number) else {
                return (Err(format!("没有第 {number} 条。")), false);
            };
            let Some(current) = book.get(id) else {
                return (Err("这条规则刚被删了。".into()), false);
            };
            let mut next = current.clone();
            next.mode = mode;
            next.updated_at = chrono::Utc::now().timestamp();
            if let Some(why) = next.problem(config.mute_seconds) {
                return (Err(format!("不能改成{}：{why}", mode.label())), false);
            }
            let summary = next.summary(config.mute_seconds);
            if let Some(rule) = book.get_mut(id) {
                *rule = next;
            }
            (Ok(summary), true)
        })
        .await;
    match result {
        Ok(summary) => {
            say(
                ctx,
                writer,
                group,
                user,
                &format!("{number}. 改成：{summary}"),
            )
            .await
        }
        Err(why) => say(ctx, writer, group, user, &why).await,
    }
}

#[allow(clippy::too_many_arguments)]
async fn require_all(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    scope: Scope,
    group: Option<&str>,
    user: &str,
    rest: &str,
) -> Result<(), PluginError> {
    if scope == Scope::Local && group.is_none() {
        return say(ctx, writer, group, user, "本群规则要到群里改。").await;
    }
    let place = group.unwrap_or("");
    let (number, tail) = match take_number(rest) {
        Ok(found) => found,
        Err(why) => return say(ctx, writer, group, user, &why).await,
    };
    let on = match tail.trim() {
        "" => None,
        "开" | "开启" | "是" | "on" => Some(true),
        "关" | "关闭" | "不" | "off" => Some(false),
        _ => {
            return say(
                ctx,
                writer,
                group,
                user,
                "写「开」或「关」；不带就切换。例：「违禁词 全含 1 开」",
            )
            .await;
        }
    };
    let result = BOOK
        .update(|book| {
            let Some(id) = id_of(book, scope, place, number) else {
                return (Err(format!("没有第 {number} 条。")), false);
            };
            let Some(current) = book.get(id) else {
                return (Err("这条规则刚被删了。".into()), false);
            };
            let real = current
                .words
                .iter()
                .filter(|word| !word.trim().is_empty())
                .count();
            if real < 2 {
                return (
                    Err("这条只有一个词，没有「全部出现」这回事；用「、」再多加一个词。".into()),
                    false,
                );
            }
            let mut next = current.clone();
            next.require_all = on.unwrap_or(!next.require_all);
            next.updated_at = chrono::Utc::now().timestamp();
            if let Some(why) = next.problem(config.mute_seconds) {
                return (Err(why), false);
            }
            let summary = next.summary(config.mute_seconds);
            if let Some(rule) = book.get_mut(id) {
                *rule = next;
            }
            (Ok(summary), true)
        })
        .await;
    match result {
        Ok(summary) => {
            say(
                ctx,
                writer,
                group,
                user,
                &format!("{number}. 改成：{summary}"),
            )
            .await
        }
        Err(why) => say(ctx, writer, group, user, &why).await,
    }
}

#[allow(clippy::too_many_arguments)]
async fn toggle(
    ctx: &Context,
    writer: &LockedWriter,
    _config: &Config,
    scope: Scope,
    group: Option<&str>,
    user: &str,
    rest: &str,
    enabled: bool,
) -> Result<(), PluginError> {
    if scope == Scope::Local && group.is_none() {
        return say(ctx, writer, group, user, "本群规则要到群里改。").await;
    }
    let place = group.unwrap_or("");
    let (number, _) = match take_number(rest) {
        Ok(found) => found,
        Err(why) => return say(ctx, writer, group, user, &why).await,
    };
    let result = BOOK
        .update(|book| {
            let Some(id) = id_of(book, scope, place, number) else {
                return (Err(format!("没有第 {number} 条。")), false);
            };
            let Some(rule) = book.get_mut(id) else {
                return (Err("这条规则刚被删了。".into()), false);
            };
            if rule.enabled == enabled {
                return (
                    Ok(format!(
                        "第 {number} 条本来就{}着。",
                        if enabled { "开" } else { "停用" }
                    )),
                    false,
                );
            }
            rule.enabled = enabled;
            rule.updated_at = chrono::Utc::now().timestamp();
            (
                Ok(format!(
                    "第 {number} 条已{}。",
                    if enabled { "启用" } else { "停用" }
                )),
                true,
            )
        })
        .await;
    match result {
        Ok(message) => say(ctx, writer, group, user, &message).await,
        Err(why) => say(ctx, writer, group, user, &why).await,
    }
}

#[allow(clippy::too_many_arguments)]
async fn delete(
    ctx: &Context,
    writer: &LockedWriter,
    _config: &Config,
    scope: Scope,
    group: Option<&str>,
    user: &str,
    rest: &str,
) -> Result<(), PluginError> {
    if scope == Scope::Local && group.is_none() {
        return say(ctx, writer, group, user, "本群规则要到群里删。").await;
    }
    let place = group.unwrap_or("");
    let numbers = match parse_numbers(rest) {
        Ok(numbers) => numbers,
        Err(why) => return say(ctx, writer, group, user, &why).await,
    };
    let result = BOOK
        .update(|book| {
            let ids: Vec<u32> = numbers
                .iter()
                .filter_map(|number| id_of(book, scope, place, *number))
                .collect();
            if ids.is_empty() {
                return (
                    Err("这些编号都不存在。发「违禁词」看看编号。".to_string()),
                    false,
                );
            }
            let mut removed = Vec::new();
            for id in ids {
                if let Some(rule) = book.remove(id) {
                    removed.push(format!("#{} {}", rule.id, rule.words.join("、")));
                }
            }
            (Ok(removed), true)
        })
        .await;
    match result {
        Ok(removed) if !removed.is_empty() => {
            let recover = match scope {
                Scope::Local => "「违禁词 恢复」",
                Scope::Global => "「违禁词 全局恢复」",
            };
            let line = format!(
                "删了 {} 条：{}。删错了发{recover}找回来。",
                removed.len(),
                removed.join("；")
            );
            say(ctx, writer, group, user, &line).await
        }
        Ok(_) => say(ctx, writer, group, user, "没有删掉任何一条。").await,
        Err(why) => say(ctx, writer, group, user, &why).await,
    }
}

async fn restore(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    scope: Scope,
    group: Option<&str>,
    user: &str,
) -> Result<(), PluginError> {
    if scope == Scope::Local && group.is_none() {
        return say(ctx, writer, group, user, "本群规则要到群里恢复。").await;
    }
    let place = group.unwrap_or("");
    let restored = BOOK
        .update(|book| {
            let found = book.restore(|rule| match scope {
                Scope::Local => rule.belongs_to(place),
                Scope::Global => rule.global(),
            });
            let changed = found.is_some();
            (found, changed)
        })
        .await;
    let Some(rule) = restored else {
        return say(
            ctx,
            writer,
            group,
            user,
            "回收站里没有可恢复的规则（只留最近 20 条）。",
        )
        .await;
    };
    let number = BOOK
        .read(|book| number_of(book, scope, place, rule.id))
        .await
        .unwrap_or(0);
    let line = format!(
        "恢复了{}「{}」。",
        scope_name(scope, number),
        rule.summary(config.mute_seconds)
    );
    say(ctx, writer, group, user, &line).await
}

// ==================== 试一句 ====================

#[allow(clippy::too_many_arguments)]
async fn trial(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    scope: Scope,
    group: Option<&str>,
    user: &str,
    name: &str,
    text: &str,
) -> Result<(), PluginError> {
    let text = text.trim();
    if text.is_empty() {
        return say(
            ctx,
            writer,
            group,
            user,
            "要试哪一句？例：「违禁词 试 加微信领红包」",
        )
        .await;
    }
    let body = super::book::Text::new(text);
    let all: Vec<Rule> = BOOK.read(|book| book.rules.clone()).await;
    let mut candidates: Vec<&Rule> = match (scope, group) {
        (Scope::Local, Some(group)) => all.iter().filter(|rule| rule.covers(group)).collect(),
        _ => all.iter().filter(|rule| rule.global()).collect(),
    };
    candidates.sort_by_key(|rule| (rule.groups.len(), rule.id));

    let hit = candidates.iter().find_map(|rule| {
        rule.hit(body.raw, &body.normalized, &body.folded)
            .map(|word| (*rule, word))
    });
    let Some((rule, keyword)) = hit else {
        // 没对上时尽量说清是不是「对上了但被停用 / 只在别的群 / 正则写坏了」。
        let reason = all.iter().find_map(|rule| {
            let matched = rule.hit_unchecked(body.raw, &body.normalized, &body.folded)?;
            let why = if !rule.enabled {
                "停用着"
            } else if !group.is_some_and(|group| rule.covers(group)) {
                "只在别的群生效"
            } else if scope == Scope::Global && !rule.global() {
                "是本群自己的规则，不在全局范围里"
            } else {
                "对上了，但眼下不处理"
            };
            Some(format!("「{matched}」对上了，但那条规则{why}。"))
        });
        let broken = all.iter().find_map(|rule| {
            if rule.mode != Mode::Regex {
                return None;
            }
            rule.words.iter().find_map(|word| {
                regex(word)
                    .is_none()
                    .then(|| format!("「{word}」这条正则写得不对，永远不会触发。"))
            })
        });
        let line = reason.or(broken).unwrap_or_else(|| {
            format!(
                "不会触发：{} 条规则都看过了。",
                if candidates.is_empty() {
                    0
                } else {
                    candidates.len()
                }
            )
        });
        return say(ctx, writer, group, user, &line).await;
    };

    let label = rule_label(&all, rule, group);
    let mute = rule.mute_seconds.unwrap_or(config.mute_seconds);
    let mut lines = vec![format!(
        "会触发 {label}「{keyword}」：{}。",
        rule.summary(config.mute_seconds)
    )];
    if config.notice.is_silent() {
        lines.push("提示：静默，群里不说话。".into());
    } else {
        let done = done_preview(rule.action, mute);
        let group_name = ctx
            .as_message()
            .and_then(|message| message.0.get_str("group_name"))
            .unwrap_or("");
        let preview = notice_preview(config, name, &keyword, &done, mute, group_name);
        let preview = if preview.is_empty() {
            "（空）".to_string()
        } else {
            preview
        };
        lines.push(format!("提示：{preview}（{}）", notice_lifetime(config)));
    }
    if config.exempt_staff && may_manage(ctx, config) && group.is_some() {
        lines.push("你是本群的管理，这句话真发出来不会被处理。".into());
    }
    if (rule.action.recalls() || rule.action.mutes())
        && let Some(group) = group
        && !api::is_staff(&api::own_role(ctx, writer.clone(), group).await)
    {
        lines.push("但机器人不是本群管理员，真的触发时撤不了、禁不了。".into());
    }
    say(ctx, writer, group, user, &lines.join("\n")).await
}

fn rule_label(all: &[Rule], rule: &Rule, group: Option<&str>) -> String {
    if let Some(group) = group
        && rule.belongs_to(group)
        && let Some(index) = sorted_index(all, |item| item.belongs_to(group), rule.id)
    {
        return format!("本群 #{}", index + 1);
    }
    if rule.global()
        && let Some(index) = sorted_index(all, Rule::global, rule.id)
    {
        return format!("全局 #{}", index + 1);
    }
    if rule.global() {
        return "全局规则".into();
    }
    format!("#{}（只在 {} 个群）", rule.id, rule.groups.len())
}

fn sorted_index(all: &[Rule], keep: impl Fn(&Rule) -> bool, id: u32) -> Option<usize> {
    let mut rules: Vec<&Rule> = all.iter().filter(|rule| keep(rule)).collect();
    rules.sort_by_key(|rule| rule.id);
    rules.iter().position(|rule| rule.id == id)
}

// ==================== 小工具 ====================

fn scope_name(scope: Scope, number: usize) -> String {
    match scope {
        Scope::Local => format!("本群 #{number}"),
        Scope::Global => format!("全局 #{number}"),
    }
}

fn describe(action: Action, mute_seconds: u64) -> String {
    if action.mutes() {
        format!(
            "{} {}",
            action.label(),
            crate::clock::duration_text(mute_seconds)
        )
    } else {
        action.label().to_string()
    }
}

fn scoped<'a>(book: &'a Book, scope: Scope, group: &str) -> Vec<&'a Rule> {
    match scope {
        Scope::Local => book.local(group),
        Scope::Global => book.global(),
    }
}

fn id_of(book: &Book, scope: Scope, group: &str, number: usize) -> Option<u32> {
    scoped(book, scope, group)
        .get(number.checked_sub(1)?)
        .map(|rule| rule.id)
}

fn number_of(book: &Book, scope: Scope, group: &str, id: u32) -> Option<usize> {
    scoped(book, scope, group)
        .iter()
        .position(|rule| rule.id == id)
        .map(|index| index + 1)
}

fn take_number(rest: &str) -> Result<(usize, &str), String> {
    let rest = rest.trim();
    let (first, tail) = match rest.split_once(char::is_whitespace) {
        Some((first, tail)) => (first, tail.trim()),
        None => (rest, ""),
    };
    let number = first
        .parse::<usize>()
        .map_err(|_| format!("第 {first} 条？编号要是数字。发「违禁词」看看编号。"))?;
    if number == 0 {
        return Err("编号从 1 开始。".into());
    }
    Ok((number, tail))
}

/// 1 2 5-8 这样的编号，最多一次 50 条。
fn parse_numbers(rest: &str) -> Result<Vec<usize>, String> {
    let mut numbers = Vec::new();
    for token in rest
        .split([',', '，', '、'])
        .flat_map(str::split_whitespace)
    {
        if let Some((start, end)) = token.split_once(['-', '~', '—']) {
            let start: usize = start
                .parse()
                .map_err(|_| format!("「{token}」不像一个编号范围"))?;
            let end: usize = end
                .parse()
                .map_err(|_| format!("「{token}」不像一个编号范围"))?;
            if start == 0 || end < start {
                return Err(format!("「{token}」这个范围不对"));
            }
            if end - start > 100 {
                return Err("一次最多删 100 条。".into());
            }
            numbers.extend(start..=end);
        } else {
            let number: usize = token
                .parse()
                .map_err(|_| format!("「{token}」不像一个编号"))?;
            if number == 0 {
                return Err("编号从 1 开始。".into());
            }
            numbers.push(number);
        }
    }
    numbers.sort_unstable();
    numbers.dedup();
    if numbers.is_empty() {
        return Err("要删哪一条？例：「违禁词 删除 1 3-5」".into());
    }
    if numbers.len() > 50 {
        return Err("一次最多删 50 条。".into());
    }
    Ok(numbers)
}

/// 时长：30秒 10分钟 2小时 1天，也可以写 10m 2h 1d；只写数字按分钟算。
/// 「默认」给 0，由调用方当成「跟随配置」。
fn parse_duration(text: &str) -> Option<u64> {
    let text = text.trim().to_lowercase();
    if text == "默认" {
        return Some(0);
    }
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let number: u64 = text[..split].parse().ok()?;
    let unit = text[split..].trim();
    let seconds = match unit {
        "" | "分" | "分钟" | "m" | "min" | "mins" | "minute" | "minutes" => {
            number.checked_mul(60)?
        }
        "秒" | "s" | "sec" | "secs" | "second" | "seconds" => number,
        "小时" | "时" | "h" | "hr" | "hour" | "hours" => number.checked_mul(3600)?,
        "天" | "d" | "day" | "days" => number.checked_mul(86400)?,
        _ => return None,
    };
    (seconds <= super::book::MUTE_MAX).then_some(seconds)
}

fn help() -> &'static str {
    "违禁词 · 群管理
违禁词                              看本群规则与当前设置
违禁词 添加 <词> [动作] [时长]      例：违禁词 添加 加微信、加V 撤回禁言 2小时
违禁词 改 <编号> <动作> [时长]      改动作；禁言可以顺手改时长，如「违禁词 改 1 禁言 10分钟」
违禁词 模式 <编号> <包含|正则>      切换怎么比
违禁词 全含 <编号> [开|关]          多个词要全部出现才算对上（默认任一出现就算）
违禁词 停用 / 启用 <编号>            暂时不生效 / 恢复
违禁词 删除 <编号…>                 删 1 条或多条，如「违禁词 删除 1 3-5」；删错可恢复
违禁词 恢复                         找回最近删掉的一条
违禁词 试 <一句话>                  不发出去，看会不会触发、由哪条处理
违禁词 全局 …                       号主维护所有群共用的规则，用法同上

动作：仅提醒 / 撤回 / 禁言 / 撤回禁言；时长如 10分钟、2小时、1天，只有禁言用得到
词：多个词用「、」隔开；整条写成 /正则/ 就按正则比（也可发「违禁词 正则添加 …」）
例：违禁词 添加 /加\\s*微\\s*信/ 撤回禁言 1小时
检测前会忽略空格、标点、全角半角与零宽字符，「加 微 信」也算「加微信」。
提示语、留存多久、默认动作与禁言时长、豁免名单在控制台的插件页或 config.toml 里改。"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_and_one_word_forms() {
        let (scope, rest) = scope_of("全局添加 加微信", Some("1"));
        assert_eq!(scope, Scope::Global);
        assert_eq!(split_verb(rest), ("添加", "加微信"));
        assert_eq!(
            scope_of("添加 加微信", Some("1")),
            (Scope::Local, "添加 加微信")
        );
        assert_eq!(
            scope_of("添加 加微信", None),
            (Scope::Global, "添加 加微信")
        );
    }

    #[test]
    fn add_parses_action_duration_and_words() {
        let config = Config::default();
        let spec = parse_add("加微信、加V 撤回禁言 2小时", &config, false).unwrap();
        assert_eq!(spec.words, ["加微信", "加V"]);
        assert_eq!(spec.action, Action::RecallMute);
        assert_eq!(spec.mute, Some(7200));
        let spec = parse_add("加微信", &config, false).unwrap();
        assert_eq!(spec.action, Action::Recall);
        assert_eq!(spec.mute, None);
        let spec = parse_add("加 微 信", &config, false).unwrap();
        assert_eq!(spec.words, ["加微信"]);
    }

    #[test]
    fn regex_wrapper_sets_regex_mode() {
        let config = Config::default();
        let spec = parse_add(r"/加\s*微\s*信/ 撤回", &config, false).unwrap();
        assert_eq!(spec.mode, Mode::Regex);
        assert_eq!(spec.words, [r"加\s*微\s*信"]);
        assert_eq!(spec.action, Action::Recall);
        assert_eq!(spec.mute, None, "撤回不用时长");
        assert!(parse_add(r"/加\s*微\s*信/ 撤回 1小时", &config, false).is_err());
    }

    #[test]
    fn numbers_and_durations() {
        assert_eq!(parse_numbers("1 3-5 5").unwrap(), [1, 3, 4, 5]);
        assert!(parse_numbers("0").is_err());
        assert_eq!(parse_duration("10分钟"), Some(600));
        assert_eq!(parse_duration("2h"), Some(7200));
        assert_eq!(parse_duration("30秒"), Some(30));
        assert_eq!(parse_duration("10"), Some(600));
        assert_eq!(parse_duration("默认"), Some(0));
        assert_eq!(parse_duration("很久"), None);
    }
}
