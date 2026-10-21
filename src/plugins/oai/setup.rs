//! `oai` 这条指令：看接入状态，一句话接上一家模型供应商。
//!
//! 新装好的机器人，号主手里通常只有一把 DeepSeek 密钥。从前要先在控制台的映射表里
//! 加一项、抄官方地址、再填密钥，管家大人那边还得另外换模型；现在发一句
//! `oai deepseek sk-…` 就完事：地址从 [`super::catalog`] 补，密钥先拿去请求一次
//! 模型列表验过再存（存进 `config.toml` 的 `[oai.providers.<名字>]`，走 ctl 同一条事务），
//! 存好就把这家的模型列表拉回来，并回一句「管家大人现在用哪个模型、联网走哪儿」。
//!
//! 写法（`oai` 前面带不带指令前缀都认）：
//! - `oai`：接入状态 + 指令帮助；
//! - `oai <供应商> <密钥>`：认得的供应商（[`super::catalog::KNOWN`]，名字或别名）或已经配过的名字；
//! - `oai <名字> <地址> <密钥>`：自己的中转站，地址与密钥顺序随意；
//! - `oai <地址> <密钥>`：默认接口（不带供应商前缀的模型走它，从前就是这么配的）；
//! - `oai <供应商> 清除`：清掉这一家的密钥。
//!
//! 改接口只有号主与 `ctl.admins` 能做：换一个 `api_base` 就能把所有对话寄到别人的服务器上。
//! 存好之后试着撤回带密钥的那条消息（号主就在自己的账号上打字，打在哪个对话框里别人都看得见），
//! 撤不回就明说该去重置。

use super::catalog::{self, KeyFrom};
use super::data::Manager;
use super::logic::reply_text;
use crate::adapters::satori::{LockedWriter, api};
use crate::event::Context;
use std::sync::Arc;
use toml::Value;

use super::LOG_TARGET;

/// 解析出来的一次接入。
#[derive(Debug, Clone, PartialEq)]
pub enum Connect {
    /// 默认接口（不带供应商前缀的模型走它）。
    Default { base: String, key: String },
    /// `[oai.providers.<name>]`；`base` 为空表示沿用已写的或官方地址。
    Provider { name: String, base: String, key: String },
    /// 清掉一家的密钥。
    Clear { name: String },
}

fn is_url(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// 像不像一把密钥：ASCII、不带空白、够长。各家格式不一（`sk-…`、`AIza…`、`xxx.yyy`），
/// 只能这样粗判——要紧的是别把「oai deepseek 好用吗」这种闲聊当成指令吃掉。
fn is_key(token: &str) -> bool {
    token.len() >= 8 && token.is_ascii() && !token.chars().any(char::is_whitespace) && !is_url(token)
}

/// 供应商名字：字母数字、下划线、连字符，与控制台起名的规矩一致。
fn is_name(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 32
        && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && !is_url(token)
}

/// `oai` 后面那一截 → 一次接入；不像接入就返回 `None`，消息照常往下传。
pub fn parse(rest: &str) -> Option<Connect> {
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    match tokens.as_slice() {
        [a, b] if is_url(a) && is_key(b) => Some(Connect::Default { base: (*a).into(), key: (*b).into() }),
        [a, b] if is_key(a) && is_url(b) => Some(Connect::Default { base: (*b).into(), key: (*a).into() }),
        [name, word] if is_name(name) && ["清除", "删除", "clear", "-"].contains(word) => {
            Some(Connect::Clear { name: name.to_ascii_lowercase() })
        }
        [name, key] if is_name(name) && is_key(key) => {
            Some(Connect::Provider { name: name.to_ascii_lowercase(), base: String::new(), key: (*key).into() })
        }
        [name, a, b] if is_name(name) && is_url(a) && is_key(b) => {
            Some(Connect::Provider { name: name.to_ascii_lowercase(), base: (*a).into(), key: (*b).into() })
        }
        [name, a, b] if is_name(name) && is_key(a) && is_url(b) => {
            Some(Connect::Provider { name: name.to_ascii_lowercase(), base: (*b).into(), key: (*a).into() })
        }
        _ => None,
    }
}

/// 执行一次接入并回话。
pub async fn connect(request: Connect, ctx: &Context, writer: &LockedWriter, mgr: &Arc<Manager>) {
    let Some(event) = ctx.as_message() else { return };
    if !crate::plugins::ctl::is_manager(ctx) {
        reply_text(ctx, writer, &event, "❌ 只有号主和 ctl.admins 里的管理员能改模型接口").await;
        return;
    }
    let oai = crate::plugins::get_config_or_default::<super::OaiConfig>(ctx);
    let outcome = match &request {
        Connect::Default { base, key } => connect_default(base, key, mgr).await,
        Connect::Provider { name, base, key } => connect_provider(ctx, &oai, mgr, name, base, key).await,
        Connect::Clear { name } => clear_provider(ctx, &oai, name).await,
    };
    let mut text = match outcome {
        Ok(text) => text,
        Err(text) => format!("❌ {text}"),
    };

    // 带密钥的那条消息留在会话里不妥（号主多半是在某个群或某个好友的对话框里打的）：
    // 能撤就撤，撤不回就让号主去重置。本机控制台没有「别人看得见」的问题；清除不带密钥。
    let console = ctx.bot.adapter == "console" && ctx.bot.platform == "console";
    if !console && !matches!(request, Connect::Clear { .. }) {
        match api::delete_msg(ctx, writer.clone(), event.message_id()).await {
            Ok(()) => text.push_str("\n\n带密钥的那条消息已撤回。"),
            Err(error) => {
                debug!(target: LOG_TARGET, "撤回带密钥的消息失败：{error}");
                text.push_str("\n\n⚠️ 带密钥的那条消息没能撤回：别人可能看得到，建议到供应商后台重置这把密钥再发一次。");
            }
        }
    }
    reply_text(ctx, writer, &event, text).await;

    // 列表刷新放在回话之后：多家供应商一起拉要一两秒，不让号主干等。
    let oai = crate::plugins::get_config_or_default::<super::OaiConfig>(ctx);
    if let Err(error) = mgr.fetch_models(&oai.model_filter, &oai.providers).await {
        debug!(target: LOG_TARGET, "接入后刷新模型列表失败：{error}");
    }
}

async fn connect_default(base: &str, key: &str, mgr: &Arc<Manager>) -> Result<String, String> {
    let base = super::utils::openai_api_base(base);
    let note = match super::data::list_models(&base, key).await {
        Ok(models) => format!("{} 个模型", models.len()),
        Err(error) if error.to_string().starts_with("密钥无效") => {
            return Err(format!("默认接口拒绝了这把密钥（{error}），没有保存"));
        }
        Err(error) => format!("暂时没连上（{error}），先存下了"),
    };
    let mut c = mgr.config.write().await;
    c.api_base = base.clone();
    c.api_key = key.to_string();
    mgr.save(&mut c).map_err(|error| format!("保存失败，已恢复原配置：{error}"))?;
    Ok(format!("✅ 默认接口已接上：{base}（{note}）\n不带供应商前缀的模型走这里，`/%` 查看。"))
}

async fn connect_provider(
    ctx: &Context,
    oai: &super::OaiConfig,
    mgr: &Manager,
    name: &str,
    base: &str,
    key: &str,
) -> Result<String, String> {
    let known = catalog::find(name);
    let configured = catalog::configured(&oai.providers, name);
    // 写进配置用的名字：已经配过就沿用那一项的写法，认得的用正名（`kimi` → `moonshot`）。
    let entry_name = configured
        .map(|(name, _)| name.to_string())
        .or_else(|| known.map(|known| known.id.to_string()))
        .unwrap_or_else(|| name.to_string());
    let base = if !base.trim().is_empty() {
        super::utils::openai_api_base(base)
    } else if let Some((_, entry)) = configured.filter(|(_, entry)| !entry.api_base.trim().is_empty()) {
        entry.api_base.trim().to_string()
    } else if let Some(known) = known {
        known.base.to_string()
    } else {
        return Err(format!(
            "不认得供应商「{name}」。自己的中转站写成：oai {name} <地址> <密钥>\n认得的有：{}",
            known_names()
        ));
    };
    let label = known.map_or(entry_name.as_str(), |known| known.name);

    // 先验再存：一把复制漏了半截的密钥存进去，只会在管家大人那边变成一句看不懂的报错。
    let models = super::data::list_models(&base, key).await;
    let note = match &models {
        Ok(models) => format!("{} 个模型", models.len()),
        Err(error) if error.to_string().starts_with("密钥无效") => {
            return Err(format!("{label} 拒绝了这把密钥（{error}），没有保存。检查是否复制完整。"));
        }
        // 网络抽风不代表密钥不对：照存，回话里说清楚。
        Err(error) => format!("暂时没连上（{error}），密钥先存下了"),
    };

    let table = {
        let mut table = toml::map::Map::new();
        table.insert("api_base".into(), Value::String(base.clone()));
        table.insert("api_key".into(), Value::String(key.to_string()));
        Value::Table(table)
    };
    let entry = entry_name.clone();
    crate::plugins::ctl::change(ctx, move |cfg| {
        let oai = cfg.plugins.get_mut("oai").and_then(Value::as_table_mut).ok_or("oai 配置不存在")?;
        let providers = oai
            .entry("providers")
            .or_insert_with(|| Value::Table(Default::default()))
            .as_table_mut()
            .ok_or("oai.providers 不是表")?;
        providers.insert(entry, table);
        Ok(String::new())
    })
    .await?;
    info!(target: LOG_TARGET, "号主接入了供应商 {entry_name}（{base}）");

    let oai = crate::plugins::get_config_or_default::<super::OaiConfig>(ctx);
    Ok(format!("✅ {label} 已接上（{note}）。\n{}", butler_line(&oai, mgr).await))
}

async fn clear_provider(ctx: &Context, oai: &super::OaiConfig, name: &str) -> Result<String, String> {
    let Some((entry, _)) = catalog::configured(&oai.providers, name) else {
        return Err(format!("没配过「{name}」，不用清"));
    };
    let entry = entry.to_string();
    let path = entry.clone();
    crate::plugins::ctl::change(ctx, move |cfg| {
        let slot = cfg
            .plugins
            .get_mut("oai")
            .and_then(|oai| oai.get_mut("providers"))
            .and_then(|providers| providers.get_mut(&path))
            .and_then(Value::as_table_mut)
            .ok_or("这一项已经不在了")?;
        slot.insert("api_key".into(), Value::String(String::new()));
        Ok(String::new())
    })
    .await?;
    Ok(format!("✅ 已清掉 {entry} 的密钥（地址留着，再发 `oai {entry} <密钥>` 就能接回）。"))
}

fn known_names() -> String {
    catalog::KNOWN.iter().map(|known| known.id).collect::<Vec<_>>().join("、")
}

/// 内置智能体房间（优先管家大人）的名字与它实际用的模型写法。
pub(crate) async fn butler_spec(oai: &super::OaiConfig, mgr: &Manager) -> (String, String) {
    let c = mgr.config.read().await;
    let room = c
        .agents
        .iter()
        .find(|agent| agent.uses_agent() && agent.name == super::data::BUILTIN_ROOM)
        .or_else(|| c.agents.iter().find(|agent| agent.uses_agent()));
    let name = room.map_or(super::data::BUILTIN_ROOM, |room| room.name.as_str()).to_string();
    let spec = match room {
        Some(room) if !super::agent::uses_default_model(&room.model) => room.model.clone(),
        _ => oai.agent_default_model().unwrap_or(c.default_model.as_str()).to_string(),
    };
    (name, spec)
}

/// 「管家大人现在用 X，联网走 Y」——接入之后、状态卡上都说这一句。
async fn butler_line(oai: &super::OaiConfig, mgr: &Manager) -> String {
    let (room, spec) = butler_spec(oai, mgr).await;
    let (provider, _) = super::utils::split_provider(&spec);
    let ready = match provider.as_deref() {
        Some(name) => {
            let (base, key) = {
                let c = mgr.config.read().await;
                (c.api_base.clone(), c.api_key.clone())
            };
            super::resolve_endpoint(&oai.providers, &base, &key, Some(name)).is_some_and(|(_, key)| !key.is_empty())
        }
        None => !mgr.config.read().await.api_key.is_empty(),
    };
    let mut line = if ready {
        format!("{room}用 {spec}，可以直接用了")
    } else {
        let mut line = format!("{room}用 {spec}，但这家还没配好");
        if let Some((name, Some(known))) = catalog::ready(&oai.providers)
            .into_iter()
            .find(|(_, known)| known.is_some_and(|known| !known.model.is_empty()))
        {
            line.push_str(&format!("；可换成已接上的：{room}%{name}/{}", known.model));
        }
        line
    };
    let search = oai.search_config();
    if search.enabled {
        line.push_str(&format!("\n联网：{}", search.chain().join(" → ")));
    }
    line
}

/// `oai` 卡片开头那段：接入状态。
pub async fn status(oai: &super::OaiConfig, mgr: &Manager) -> String {
    let (base, has_key) = {
        let c = mgr.config.read().await;
        (c.api_base.clone(), !c.api_key.is_empty())
    };
    let mut out = String::from("## 接入状态\n");
    out.push_str(&format!("- {}\n", butler_line(oai, mgr).await.replace('\n', "\n- ")));
    let default = if base.is_empty() {
        "未配置（不带供应商前缀的模型走它）".to_string()
    } else if has_key {
        format!("{base}（已接上）")
    } else {
        format!("{base}（缺密钥）")
    };
    out.push_str(&format!("- 默认接口：{default}\n"));

    let mut rows = Vec::new();
    for known in catalog::KNOWN {
        let configured = catalog::configured(&oai.providers, known.id);
        let Some(found) = catalog::resolve(&oai.providers, configured.map_or(known.id, |(name, _)| name)) else {
            continue;
        };
        let state = match found.key_from {
            KeyFrom::Config => "已接上".to_string(),
            KeyFrom::Env(var) => format!("已接上，密钥取自 {var}"),
            KeyFrom::Keyless if configured.is_some() => "本机".to_string(),
            _ if configured.is_some() => "缺密钥".to_string(),
            _ => continue,
        };
        rows.push(format!("{}（{state}）", known.name));
    }
    for (name, entry) in &oai.providers {
        if catalog::find(name).is_some() || name.eq_ignore_ascii_case("apilio") {
            continue;
        }
        let state = if entry.api_key.trim().is_empty() { "缺密钥" } else { "已接上" };
        rows.push(format!("{name}（{state}）"));
    }
    out.push_str(&format!(
        "- 供应商：{}\n",
        if rows.is_empty() { "一家都没配".to_string() } else { rows.join(" · ") }
    ));
    out.push_str(&format!(
        "\n接入一家：`oai deepseek 密钥`（存好后自动撤回带密钥的那条消息）\n自己的中转站：`oai 名字 地址 密钥`；默认接口：`oai 地址 密钥`；清掉：`oai 名字 清除`\n认得的供应商：{}\n",
        known_names()
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_shapes_and_leaves_chatter_alone() {
        assert_eq!(
            parse("deepseek sk-abcdefgh1234"),
            Some(Connect::Provider { name: "deepseek".into(), base: String::new(), key: "sk-abcdefgh1234".into() })
        );
        assert_eq!(
            parse("Relay sk-abcdefgh1234 https://r.example/v1"),
            Some(Connect::Provider {
                name: "relay".into(),
                base: "https://r.example/v1".into(),
                key: "sk-abcdefgh1234".into()
            })
        );
        assert_eq!(
            parse("https://api.example.com sk-abcdefgh1234"),
            Some(Connect::Default { base: "https://api.example.com".into(), key: "sk-abcdefgh1234".into() })
        );
        assert_eq!(parse("kimi 清除"), Some(Connect::Clear { name: "kimi".into() }));
        assert_eq!(parse("deepseek 好用吗"), None);
        assert_eq!(parse("是什么"), None);
        assert_eq!(parse("deepseek short"), None);
    }
}
