//! 控制台的规则面板：读一份、存一条、删一条、找回来、试一句。
//!
//! 规则不在 `config.toml` 里，所以不走 `ctl::change`；写入照样先校验（[`Rule::problem`]）、
//! 再原子落盘（[`BOOK`]），失败不改内存。群里的指令写的是同一份，两边随改随见。

use super::book::{self, BOOK, Mode, Rule};
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
    let limits = &config.limits;
    let max = limits.contains_max_chars;
    let modes: Vec<Value> =
        Mode::ALL.iter().map(|m| json!({ "code": m.code(), "label": m.label(), "explain": m.explain(max) })).collect();
    let moment = crate::clock::moment();
    json!({
        "rules": rules,
        "trash": trash,
        "modes": modes,
        "enabled": crate::plugins::enabled(&ctx.config.read().unwrap(), "autoreply"),
        "defaults": {
            "mode": Mode::default().code(),
            "cooldown_seconds": limits.cooldown_seconds,
            "user_cooldown_seconds": limits.user_cooldown_seconds,
            "per_minute": limits.per_minute,
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

/// 「试一句」：这句话在这个群（`None` = 私聊，`*` = 没有单独规则的群）里会不会回、由哪条回、为什么。
pub(crate) async fn test(ctx: &Context, text: &str, group: Option<&str>, at_bot: bool) -> Value {
    let config = get_config_or_default::<Config>(ctx);
    let trial = super::trial(ctx, &config, text, group, at_bot).await;
    let tone = match (&trial.hit, trial.blocks.is_empty()) {
        (Some(_), true) => "ok",
        (Some(_), false) => "warn",
        (None, _) => "info",
    };
    json!({
        "hit": trial.hit.map(|(id, keyword)| json!({ "id": id, "keyword": keyword })),
        "message": trial.message,
        "blocks": trial.blocks,
        "notes": trial.notes,
        "tone": tone,
    })
}

/// 插件页上方「现在的情况」：规则总数与回过几次。防刷屏的口径就在下方配置里，不再抄一遍。
pub(crate) fn live(ctx: &Context) -> Value {
    let config = get_config_or_default::<Config>(ctx);
    // 插件关着这件事由规则面板顶上的横幅来说（带「打开」），这里不再重复。
    let mut lines = Vec::new();
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
        Some(_) => lines.push("还没有规则。点下面的「新建规则」，或者群管理员在群里发「自动回复 添加 下载 安装包在群文件里～」。".to_string()),
        None => {}
    }
    if !config.owner_triggers {
        lines.push("号主（机器人登录的这个号）自己发的话不会触发；想自己在群里试，打开配置里的「号主自己说也回」。".to_string());
    }
    json!({ "lines": lines })
}
