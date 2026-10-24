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
//! - `oai <供应商> 清除`：清掉这一家的密钥；
//! - `oai 用 <模型>`：换默认模型（`[oai] agent_default_model`），没单独写模型的房间都跟着换。
//!   可以只写供应商名（取它的推荐模型）、`供应商/模型`、只写模型名，或 `/%` 里的序号。
//!
//! 接上第一家能用的供应商时，默认模型若还指着一家没接的，就顺手换成这一家的推荐模型：
//! 只填了小米密钥的新用户，不该还要再学一条指令才能让管家大人开口。
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
    /// 换默认模型；空串表示只看现在用的是哪个、还能换成什么。
    Use { spec: String },
}

/// `oai 用 …` 的那个词。
const USE_WORDS: [&str; 4] = ["用", "默认", "换", "use"];

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
        [word, spec @ ..] if USE_WORDS.contains(&word.to_ascii_lowercase().as_str()) && spec.len() <= 1 => {
            Some(Connect::Use { spec: spec.first().map_or_else(String::new, |spec| (*spec).to_string()) })
        }
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
    if !crate::access::is_manager(ctx) {
        reply_text(ctx, writer, &event, "❌ 只有号主和 ctl.admins 里的管理员能改模型接口").await;
        return;
    }
    let oai = crate::plugins::get_config_or_default::<super::OaiConfig>(ctx);
    let outcome = match &request {
        Connect::Default { base, key } => connect_default(base, key, mgr).await,
        Connect::Provider { name, base, key } => connect_provider(ctx, &oai, mgr, name, base, key).await,
        Connect::Clear { name } => clear_provider(ctx, &oai, name).await,
        Connect::Use { spec } => use_model(ctx, &oai, mgr, spec).await,
    };
    let mut text = match outcome {
        Ok(text) => text,
        Err(text) => format!("❌ {text}"),
    };

    // 带密钥的那条消息留在会话里不妥（号主多半是在某个群或某个好友的对话框里打的）：
    // 能撤就撤，撤不回就让号主去重置。本机控制台没有「别人看得见」的问题；清除不带密钥。
    let console = ctx.bot.adapter == "console" && ctx.bot.platform == "console";
    if !console && matches!(request, Connect::Default { .. } | Connect::Provider { .. }) {
        match api::delete_msg(ctx, writer.clone(), event.message_id()).await {
            Ok(()) => text.push_str("\n\n带密钥的那条消息已撤回。"),
            Err(error) => {
                debug!(target: LOG_TARGET, "撤回带密钥的消息失败：{error}");
                text.push_str("\n\n⚠️ 带密钥的那条消息没能撤回：别人可能看得到。到供应商后台重置这把密钥，再手动删掉那条消息。");
            }
        }
    }
    reply_text(ctx, writer, &event, text).await;

    // 列表刷新放在回话之后：多家供应商一起拉要一两秒，不让号主干等。换默认模型不动接口，不必拉。
    if matches!(request, Connect::Use { .. }) {
        return;
    }
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

    // 默认模型指着一家没接的（新装好的机器人都指着 DeepSeek）：换成刚接上的这家，
    // 省得号主再学一条 `oai 用`。默认模型本来就能用时不动——接第二家不该改掉号主的选择。
    let oai = crate::plugins::get_config_or_default::<super::OaiConfig>(ctx);
    let mut switched = String::new();
    let (_, current) = butler_spec(&oai, mgr).await;
    if !spec_ready(&oai, mgr, &current).await
        && let Some(model) = known.map(|known| known.model).filter(|model| !model.is_empty())
    {
        let spec = format!("{entry_name}/{model}");
        match set_default_model(ctx, &spec).await {
            Ok(()) => switched = format!("默认模型 {current} 这家还没接，已换成 {spec}。\n"),
            Err(error) => debug!(target: LOG_TARGET, "接入后换默认模型失败：{error}"),
        }
    }
    let oai = crate::plugins::get_config_or_default::<super::OaiConfig>(ctx);
    Ok(format!(
        "✅ {label} 已接上（{note}）。\n{switched}{}\n它的模型写成 {entry_name}/…：`oai 用 {entry_name}/模型` 换默认，`/%` 看它有哪些。",
        butler_line(&oai, mgr).await
    ))
}

/// 把 `[oai] agent_default_model` 写成 `spec`（经 ctl，与控制台同一条事务）。
async fn set_default_model(ctx: &Context, spec: &str) -> Result<(), String> {
    let spec = spec.to_string();
    crate::plugins::ctl::change(ctx, move |cfg| {
        let oai = cfg.plugins.get_mut("oai").and_then(Value::as_table_mut).ok_or("oai 配置不存在")?;
        oai.insert("agent_default_model".into(), Value::String(spec));
        Ok(String::new())
    })
    .await
    .map(|_| ())
}

/// 这个模型写法现在打得通吗：它的供应商（或默认接口）有地址也有密钥。
async fn spec_ready(oai: &super::OaiConfig, mgr: &Manager, spec: &str) -> bool {
    let (provider, _) = super::utils::split_provider(spec);
    let (base, key) = {
        let c = mgr.config.read().await;
        (c.api_base.clone(), c.api_key.clone())
    };
    super::resolve_endpoint(&oai.providers, &base, &key, provider.as_deref())
        .is_some_and(|(base, key)| !base.is_empty() && !key.is_empty())
}

/// `oai 用 <模型>`：换默认模型。
///
/// 写法从宽：供应商名（取推荐模型；没有推荐的列出它的模型让号主挑）、`供应商/模型`、
/// 只写模型名或 `/%` 序号（在完整列表里找）。换之前确认这家接上了——换成一个打不通的模型，
/// 下一句话就是满屏报错。
async fn use_model(ctx: &Context, oai: &super::OaiConfig, mgr: &Manager, spec: &str) -> Result<String, String> {
    let (_, current) = butler_spec(oai, mgr).await;
    let spec = spec.trim();
    if spec.is_empty() {
        let mut lines = vec![format!("默认模型：{current}{}", if spec_ready(oai, mgr, &current).await { "" } else { "（这家还没接上）" })];
        let ready: Vec<String> = catalog::ready(&oai.providers)
            .into_iter()
            .map(|(name, known)| match known.map(|known| known.model).filter(|model| !model.is_empty()) {
                Some(model) => format!("`oai 用 {name}`（{name}/{model}）"),
                None => format!("`oai 用 {name}/模型`"),
            })
            .collect();
        if ready.is_empty() {
            lines.push("还没接任何供应商：先发 `oai deepseek 密钥`".to_string());
        } else {
            lines.push(format!("能换成：{}", ready.join("、")));
        }
        lines.push("也可以写 `/%` 里的序号或模型名；只想换一间房就用 `房间%模型`。".to_string());
        return Ok(lines.join("\n"));
    }

    let (models, provider_models) = {
        let c = mgr.config.read().await;
        (Manager::all_models(&c), c.provider_models.clone())
    };
    // 只写了供应商名：取它的推荐模型；没有推荐就把它的模型列出来。
    let as_provider = (!spec.contains('/'))
        .then(|| catalog::configured(&oai.providers, spec).map(|(name, _)| name.to_string()).or_else(|| catalog::find(spec).map(|known| known.id.to_string())))
        .flatten();
    let target = if let Some(name) = as_provider {
        if let Some(model) = catalog::find(&name).map(|known| known.model).filter(|model| !model.is_empty()) { format!("{name}/{model}") } else {
            let listed = provider_models.get(&name).map(Vec::as_slice).unwrap_or_default();
            if listed.is_empty() {
                return Err(format!("{name} 没有推荐模型，写全：`oai 用 {name}/模型名`（先 `/%` 看它有哪些）"));
            }
            let shown: Vec<&str> = listed.iter().take(12).map(String::as_str).collect();
            return Err(format!(
                "{name} 没有推荐模型，挑一个写全：`oai 用 {name}/模型名`\n它有：{}{}",
                shown.join("、"),
                if listed.len() > shown.len() { "……（`/%` 看全部）" } else { "" }
            ));
        }
    } else {
        match super::utils::split_provider(spec) {
            (Some(_), _) => spec.to_string(),
            (None, bare) => mgr.resolve_model(&bare, &models).unwrap_or(bare),
        }
    };
    if !spec_ready(oai, mgr, &target).await {
        let (provider, _) = super::utils::split_provider(&target);
        return Err(match provider {
            Some(name) if super::resolve_endpoint(&oai.providers, "", "", Some(&name)).is_some() => {
                format!("{target} 打不通：{}", catalog::missing_key_hint(&name))
            }
            Some(name) => format!("不认得供应商「{name}」。认得的有：{}；自己的中转站先 `oai {name} 地址 密钥`", known_names()),
            None => format!("{target} 走默认接口，而默认接口还没配：先 `oai 地址 密钥`，或写成 `供应商/模型`"),
        });
    }
    if target == current {
        return Ok(format!("默认模型本来就是 {target}"));
    }
    set_default_model(ctx, &target).await?;
    info!(target: LOG_TARGET, "默认模型：{current} → {target}");

    // 说清楚谁跟着换了、谁没换：单独写了模型的房间不受影响。
    let (following, pinned): (Vec<String>, Vec<String>) = {
        let c = mgr.config.read().await;
        let following = c
            .agents
            .iter()
            .filter(|agent| super::agent::uses_default_model(&agent.model))
            .map(|agent| agent.name.clone())
            .collect();
        let pinned = c
            .agents
            .iter()
            .filter(|agent| !super::agent::uses_default_model(&agent.model))
            .map(|agent| format!("{}（{}）", agent.name, agent.model))
            .collect();
        (following, pinned)
    };
    let mut text = format!("✅ 默认模型：{current} → {target}");
    if !following.is_empty() {
        text.push_str(&format!("\n跟着换的房间：{}", following.join("、")));
    }
    if !pinned.is_empty() {
        text.push_str(&format!("\n单独写了模型、没换的：{}（`房间%默认` 改成跟随）", pinned.join("、")));
    }
    Ok(text)
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

/// 管家大人（不在了就找一间全量档的房间）的名字与它实际用的模型写法。
pub(crate) async fn butler_spec(oai: &super::OaiConfig, mgr: &Manager) -> (String, String) {
    let c = mgr.config.read().await;
    let room = c
        .agents
        .iter()
        .find(|agent| agent.name == super::data::BUILTIN_ROOM)
        .or_else(|| c.agents.iter().find(|agent| agent.tools == super::types::Tools::Full));
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
        format!("默认模型 {spec}，{room}可以直接用了")
    } else {
        let mut line = format!("默认模型 {spec}，但这家还没配好");
        if let Some((name, Some(known))) = catalog::ready(&oai.providers)
            .into_iter()
            .find(|(_, known)| known.is_some_and(|known| !known.model.is_empty()))
        {
            line.push_str(&format!("；可换成已接上的：`oai 用 {name}`（{name}/{}）", known.model));
        }
        line
    };
    let search = oai.search_config();
    if search.enabled {
        line.push_str(&format!("\n联网：{}", search.chain().join(" → ")));
    }
    line
}

/// `oai` 帮助卡开头那段的数据：接入状态。卡片与纯文本两种输出都从这里取（见 [`super::manual`]）。
pub(crate) struct Overview {
    /// 默认模型（管家大人与没写模型的房间用的那个）。
    pub default_model: String,
    /// 默认模型现在打不打得通。
    pub ready: bool,
    /// 打不通时，已经接上的哪一家可以换：`oai 用 <这个>`。
    pub switch_to: Option<String>,
    /// 默认接口（中转站）的地址；没配是空串。
    pub relay: String,
    pub relay_key: bool,
    /// 各家供应商：（显示名，状态）。
    pub providers: Vec<(String, String)>,
    /// 其中已经接上（有密钥或本机免密钥）的几家。
    pub connected: usize,
    /// 联网后端链；`[oai.search] enabled` 关着时为 `None`。
    pub search: Option<Vec<String>>,
}

pub(crate) async fn overview(oai: &super::OaiConfig, mgr: &Manager) -> Overview {
    let (_, default_model) = butler_spec(oai, mgr).await;
    let ready = spec_ready(oai, mgr, &default_model).await;
    let switch_to = (!ready)
        .then(|| {
            catalog::ready(&oai.providers)
                .into_iter()
                .find(|(_, known)| known.is_some_and(|known| !known.model.is_empty()))
                .map(|(name, _)| name)
        })
        .flatten();
    let (relay, relay_key) = {
        let c = mgr.config.read().await;
        (c.api_base.clone(), !c.api_key.is_empty())
    };

    let mut providers = Vec::new();
    let mut connected = 0;
    for known in catalog::KNOWN {
        let configured = catalog::configured(&oai.providers, known.id);
        let Some(found) = catalog::resolve(&oai.providers, configured.map_or(known.id, |(name, _)| name)) else {
            continue;
        };
        let state = match found.key_from {
            KeyFrom::Config => "已接上".to_string(),
            KeyFrom::Env(var) => format!("已接上 · 密钥取自 {var}"),
            KeyFrom::Keyless if configured.is_some() => "本机".to_string(),
            _ if configured.is_some() => "缺密钥".to_string(),
            _ => continue,
        };
        if state != "缺密钥" {
            connected += 1;
        }
        providers.push((known.name.to_string(), state));
    }
    for (name, entry) in &oai.providers {
        if catalog::find(name).is_some() || name.eq_ignore_ascii_case("apilio") {
            continue;
        }
        let key = !entry.api_key.trim().is_empty();
        connected += usize::from(key);
        providers.push((name.clone(), if key { "已接上" } else { "缺密钥" }.to_string()));
    }
    let search = oai.search_config();
    Overview {
        default_model,
        ready,
        switch_to,
        relay,
        relay_key,
        providers,
        connected,
        search: search.enabled.then(|| search.chain()),
    }
}

/// 认得的供应商名字，帮助卡末尾列出。
pub(crate) fn known_list() -> String {
    known_names()
}
