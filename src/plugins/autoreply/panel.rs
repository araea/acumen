//! 控制台的规则面板：读一份、存一条、删一条、找回来、试一句。
//!
//! 规则不在 `config.toml` 里，所以不走 `ctl::change`；写入照样先校验（[`Rule::problem`]）、
//! 再原子落盘（[`BOOK`]），失败不改内存。群里的指令写的是同一份，两边随改随见。

use super::book::{self, Asked, BOOK, Mode, Rule};
use super::{Config, reply};
use crate::event::Context;
use crate::plugins::get_config_or_default;
use serde_json::{Value, json};

/// 一条规则给页面的样子：原样的字段，外加几句现成的说法。
fn shape(rule: &Rule) -> Value {
    let mut value = serde_json::to_value(rule).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert("title".into(), rule.title().into());
        map.insert("briefs".into(), rule.replies.iter().map(|r| r.brief(60)).collect::<Vec<_>>().into());
    }
    value
}

/// 整个面板要的东西。
pub(crate) async fn overview(ctx: &Context) -> Value {
    let config = get_config_or_default::<Config>(ctx);
    let (rules, trash) = BOOK
        .read(|b| {
            let rules: Vec<Value> = b.rules.iter().map(shape).collect();
            let trash: Vec<Value> = b
                .trash
                .iter()
                .rev()
                .map(|t| json!({ "id": t.rule.id, "title": t.rule.title(), "at": t.at, "group": t.group }))
                .collect();
            (rules, trash)
        })
        .await;
    let max = config.contains_max_chars;
    let modes: Vec<Value> =
        Mode::ALL.iter().map(|m| json!({ "code": m.code(), "label": m.label(), "explain": m.explain(max) })).collect();
    let moment = crate::clock::moment();
    json!({
        "rules": rules,
        "trash": trash,
        "modes": modes,
        "defaults": {
            "mode": config.default_mode().code(),
            "cooldown_seconds": config.cooldown_seconds,
            "user_cooldown_seconds": config.user_cooldown_seconds,
            "per_minute": config.per_minute,
            "contains_max_chars": max,
        },
        "limits": {
            "keywords": book::MAX_KEYWORDS,
            "replies": book::MAX_REPLIES,
            "keyword_chars": book::MAX_KEYWORD_CHARS,
            "reply_chars": book::MAX_REPLY_CHARS,
            "rules": book::MAX_RULES,
        },
        "placeholders": reply::PLACEHOLDERS.iter().map(|(key, label, hint)| json!({ "key": key, "label": label, "hint": hint })).collect::<Vec<_>>(),
        "samples": {
            "name": "小林",
            "user_id": "10001",
            "keyword": "下载",
            "group": "示例群",
            "group_id": "123456",
            "greeting": moment.greeting,
            "time": moment.time,
            "date": moment.date,
            "weekday": moment.weekday,
        },
    })
}

/// 存一条：`id` 是 0 就是新加的。
pub(crate) async fn save(body: Value) -> Result<Value, String> {
    let mut rule: Rule = serde_json::from_value(body).map_err(|e| format!("规则的写法不对：{e}"))?;
    let fresh = rule.id == 0;
    if fresh {
        rule.author = "控制台".into();
    }
    let saved = BOOK.with(|b| if fresh { b.insert(rule) } else { b.replace(rule) }).await?;
    let message = if fresh { format!("已添加 #{}{}", saved.id, saved.title()) } else { format!("已保存 #{}", saved.id) };
    Ok(json!({ "message": message, "rule": shape(&saved) }))
}

pub(crate) async fn delete(id: u32) -> Result<Value, String> {
    let rule = BOOK.with(|b| b.remove(id, "")).await.ok_or_else(|| format!("没有 #{id} 这条规则（可能已经删了）"))?;
    Ok(json!({ "message": format!("已删除 #{}{}", rule.id, rule.title()), "id": rule.id }))
}

pub(crate) async fn restore(id: Option<u32>) -> Result<Value, String> {
    let rule = BOOK.with(|b| b.restore(id, None)).await.ok_or("回收站里没有这一条")?;
    Ok(json!({ "message": format!("#{}{} 回来了", rule.id, rule.title()), "rule": shape(&rule) }))
}

/// 「试一句」：这句话在这个群（`None` = 私聊）里会不会触发、由哪条回、为什么。
pub(crate) async fn test(ctx: &Context, text: &str, group: Option<&str>, at_bot: bool) -> Value {
    let config = get_config_or_default::<Config>(ctx);
    let max = config.contains_max_chars;
    let text = text.trim();
    if text.is_empty() {
        return json!({ "hit": Value::Null, "message": "" });
    }
    let hit = pick(text, group, at_bot, max).await;
    let (hit, message) = match hit {
        Some((id, title, mode, keyword)) => {
            (json!({ "id": id, "keyword": keyword }), format!("会由 #{id}{title} 回复：{}匹配，对上了「{keyword}」。", mode.label()))
        }
        None => {
            if let Some((id, title, ..)) = pick(text, group, true, max).await.filter(|_| !at_bot) {
                (Value::Null, format!("直接说不会触发；@ 机器人再说，会由 #{id}{title} 回复。"))
            } else {
                let folded = book::fold(text).chars().count();
                let long = max > 0 && folded > max as usize;
                let note = if long { format!("这句去掉空格标点有 {folded} 字，超过包含匹配的 {max} 字上限。") } else { String::new() };
                (Value::Null, format!("没有规则会回这句。{note}"))
            }
        }
    };
    let skipped = config.skip_commands
        && crate::command::get_prefixes(ctx).iter().any(|p| !p.is_empty() && text.starts_with(p.as_str()));
    let message = if skipped { "这句以指令前缀开头，按设置不自动回复（交给指令）。".to_string() } else { message };
    json!({ "hit": if skipped { Value::Null } else { hit }, "message": message })
}

async fn pick(text: &str, group: Option<&str>, at_bot: bool, max: u32) -> Option<(u32, String, Mode, String)> {
    let asked = Asked::new(text, at_bot, group);
    BOOK.read(|b| book::best(&b.rules, &asked, max).map(|(rule, keyword)| (rule.id, rule.title(), rule.mode, keyword))).await
}

/// 插件页上方「现在的情况」。
pub(crate) fn live(ctx: &Context) -> Value {
    let config = get_config_or_default::<Config>(ctx);
    let mut lines = Vec::new();
    if !config.enabled {
        lines.push("插件关着。打开后群里有人问到下面的触发词就自动回；规则可以先加好。".to_string());
    }
    match BOOK.peek() {
        Some(book) if !book.rules.is_empty() => {
            let shared = book.rules.iter().filter(|r| r.groups.is_empty()).count();
            let off = book.rules.iter().filter(|r| !r.enabled).count();
            let mut line = format!("共 {} 条规则：{} 条所有群共用、{} 条只在某几个群", book.rules.len(), shared, book.rules.len() - shared);
            if off > 0 {
                line.push_str(&format!("，{off} 条停用着"));
            }
            lines.push(format!("{line}。"));
            let day = book::now() - 86400;
            let recent = book.rules.iter().filter(|r| r.last_hit > day).count();
            let total: u64 = book.rules.iter().map(|r| r.hits).sum();
            lines.push(format!("一共回过 {total} 次；最近一天里有 {recent} 条规则被问到。"));
        }
        Some(_) => lines.push("还没有规则。点「新建规则」，或者群管理员在群里发「自动回复 添加 下载 安装包在群文件里～」。".to_string()),
        None => {}
    }
    lines.push(format!(
        "防误触：同一条规则每群 {} 秒内只回一次，同一个人 {} 秒内只回一次，每群每分钟最多 {} 次{}。",
        config.cooldown_seconds,
        config.user_cooldown_seconds,
        config.per_minute,
        if config.contains_max_chars > 0 { format!("；包含匹配只看 {} 字以内的消息", config.contains_max_chars) } else { String::new() }
    ));
    json!({ "lines": lines })
}
