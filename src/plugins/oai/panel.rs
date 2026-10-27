//! 控制台的「供应商与模型」面板：一屏看全接入状态，一个弹窗改完。
//!
//! 群里那条 `oai <供应商> <密钥>` 负责「一句话接上第一家」；这个面板补它办不到的那几件事：
//! 同时摆出全部供应商、看明白每一家缺的是什么、亲手挑这家要用哪几个模型、换默认模型。
//! 参考了 Magpie 的做法——供应商是一张表，密钥、地址、模型各自一格，改了立刻生效；
//! 添加时按厂商 / 聚合平台 / 本机分组，挑一家只要贴一把密钥；每家看得到「谁在用它」，
//! 「测试连接」报延迟与模型数；模型是一张勾选清单，默认全选，勾掉哪个就不再给房间用。
//!
//! 三条边界：
//!
//! 1. **密钥不回传。** 页面只拿到「设没设」「来自配置还是环境变量」，永远拿不到值本身；
//!    改密钥是单向写入，输入框里留空表示不动它。群里 `oai` 指令要把带密钥的消息撤回，
//!    网页没有这个问题——它本来就只在本机回环地址上。
//! 2. **不拿对话试连通。** 唯一会出网的动作是 `GET /models`（测试连接、刷新列表、
//!    保存新密钥时顺手验一次）：只读、不计费、不消耗对话额度。Magpie 的测试会真发一句话，
//!    这里刻意不学——额度是用来回群友的。保存、删除、换默认模型都是本机改配置，立即生效。
//! 3. **写配置只走 [`crate::plugins::ctl::change`]。** 与 `/ctl`、群里的 `oai` 是同一条事务：
//!    校验、串行化、失败不改内存、原子替换都在那里，面板不另开一条路。

use super::catalog::{self, KeyFrom};
use super::{ProviderConfig, data, ensure_manager, utils, OaiConfig};
use crate::event::Context;
use crate::plugins::get_config_or_default;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;
use toml::Value as Toml;

/// 每家最近一次从接口拉到的完整列表（键是小写的供应商名，默认接口是空串）。
///
/// 手选过模型的那一家，配置与缓存里都只剩手选的那几个；勾选清单要摆出「还能勾哪些」，
/// 得另记一份接口原样给的。只放内存：重启后第一次打开那一家时页面会自己再拉一次。
static AVAILABLE: LazyLock<Mutex<HashMap<String, Vec<String>>>> = LazyLock::new(Default::default);

fn remember(name: &str, models: &[String]) {
    if let Ok(mut map) = AVAILABLE.lock() {
        map.insert(name.to_ascii_lowercase(), models.to_vec());
    }
}

fn recalled(name: &str) -> Option<Vec<String>> {
    AVAILABLE.lock().ok()?.get(&name.to_ascii_lowercase()).cloned()
}

fn forget(name: &str) {
    if let Ok(mut map) = AVAILABLE.lock() {
        map.remove(&name.to_ascii_lowercase());
    }
}

/// 一条模型写法落在哪一家：`供应商/模型` 按配置里的写法认（别名、大小写都算同一家），
/// 裸模型 id 落在默认接口（空串）。认不出来的供应商原样返回，页面上就对不上任何一家。
fn owner(providers: &HashMap<String, ProviderConfig>, spec: &str) -> String {
    match utils::split_provider(spec) {
        (Some(prefix), _) => catalog::configured(providers, &prefix)
            .map_or(prefix.to_ascii_lowercase(), |(name, _)| name.to_ascii_lowercase()),
        (None, _) => String::new(),
    }
}

/// 谁在用这一家：默认模型、写死了它的房间、跟随默认的房间、画图 / 写歌 / 拍片。
#[derive(Default)]
struct Usage {
    default: bool,
    rooms: Vec<String>,
    follow: usize,
    media: Vec<&'static str>,
}

impl Usage {
    fn json(&self) -> Value {
        json!({ "default": self.default, "rooms": self.rooms, "follow": self.follow, "media": self.media })
    }
}

fn usage(oai: &OaiConfig, default_model: &str, agents: &[(String, String)]) -> HashMap<String, Usage> {
    let mut map: HashMap<String, Usage> = HashMap::new();
    let default_owner = owner(&oai.providers, default_model);
    map.entry(default_owner.clone()).or_default().default = true;
    for (room, model) in agents {
        if model.trim().is_empty() {
            map.entry(default_owner.clone()).or_default().follow += 1;
        } else {
            map.entry(owner(&oai.providers, model)).or_default().rooms.push(room.clone());
        }
    }
    for (label, spec) in [("画图", &oai.media.image), ("写歌", &oai.media.music), ("拍片", &oai.media.video)] {
        let spec = spec.trim();
        if spec.is_empty() {
            continue;
        }
        // 媒体那几项可以只写供应商名，不带模型。
        let who = if spec.contains('/') { owner(&oai.providers, spec) } else { owner(&oai.providers, &format!("{spec}/_")) };
        map.entry(who).or_default().media.push(label);
    }
    map
}

/// 一家最多挂多少个模型。几百个的站点本来就该先在 `[oai] model_filter` 里收一遍，
/// 手选一份超过这个数的多半是误操作。
const MAX_MODELS: usize = 300;

/// 名字的规矩与聊天里的 `oai <名字>` 一致：字母、数字、下划线、连字符，32 字以内。
fn provider_name(providers: &HashMap<String, ProviderConfig>, raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() || name.len() > 32 || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err("名字只能用字母、数字、下划线和连字符，1–32 个字符。".into());
    }
    // 已经配过就沿用配置里的写法（大小写不同也算同一家）。
    if let Some((existing, _)) = providers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)) {
        return Ok(existing.clone());
    }
    // 认得的那几家一律写正名：`kimi` 与 `moonshot` 是同一家，别让配置里长出两份。
    Ok(catalog::find(name).map_or_else(|| name.to_ascii_lowercase(), |known| known.id.to_string()))
}

/// 手选的模型列表：去空白、去重、保序；空 id 丢掉。
fn tidy_models(list: Vec<String>) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for model in list {
        let model = model.trim();
        if model.is_empty() {
            continue;
        }
        if model.len() > 200 || model.chars().any(char::is_whitespace) {
            return Err(format!("模型 id「{model}」里有空白或太长了，检查一下。"));
        }
        if !out.iter().any(|kept| kept == model) {
            out.push(model.to_string());
        }
    }
    if out.len() > MAX_MODELS {
        return Err(format!("一家最多挂 {MAX_MODELS} 个模型，现在写进来 {} 个。", out.len()));
    }
    Ok(out)
}

/// 一家现在实际可用的模型：手选的那份非空就只用它，否则用接口拉回的缓存。
/// 返回 `(列表, 来源)`，来源是页面上一句话要写的那个词。
fn effective<'a>(entry: Option<&'a ProviderConfig>, cached: Option<&'a Vec<String>>) -> (&'a [String], &'static str) {
    match entry {
        Some(entry) if !entry.models.is_empty() => (&entry.models, "curated"),
        _ => match cached {
            Some(list) if !list.is_empty() => (list.as_slice(), "cache"),
            _ => (&[], "none"),
        },
    }
}

fn key_state(resolved: Option<&catalog::Resolved>) -> (&'static str, &'static str) {
    match resolved.map(|found| found.key_from) {
        Some(KeyFrom::Config) => ("config", ""),
        Some(KeyFrom::Env(var)) => ("env", var),
        Some(KeyFrom::Keyless) => ("keyless", ""),
        _ => ("missing", ""),
    }
}

/// 面板的整份数据：默认模型、默认接口、全部供应商、认得的供应商表。
pub(crate) async fn overview(ctx: &Context) -> Value {
    let oai: OaiConfig = get_config_or_default(ctx);
    let (relay_base, relay_key, relay_models, cached, fallback, agents) = match ensure_manager().await {
        Ok(mgr) => {
            let c = mgr.config.read().await;
            (
                c.api_base.clone(),
                !c.api_key.trim().is_empty(),
                c.models.clone(),
                c.provider_models.clone(),
                c.default_model.clone(),
                c.agents.iter().map(|agent| (agent.name.clone(), agent.model.clone())).collect::<Vec<_>>(),
            )
        }
        Err(_) => (String::new(), false, Vec::new(), BTreeMap::new(), String::new(), Vec::new()),
    };
    let current = oai.agent_default_model().unwrap_or(fallback.as_str()).to_string();
    let uses = usage(&oai, &current, &agents);
    let used = |name: &str| uses.get(&name.to_ascii_lowercase()).map_or_else(|| Usage::default().json(), Usage::json);

    let mut names: Vec<&String> = oai.providers.keys().collect();
    names.sort_by_key(|name| {
        let rank = catalog::find(name)
            .and_then(|known| catalog::KNOWN.iter().position(|item| item.id == known.id))
            .unwrap_or(usize::MAX);
        (rank, name.to_ascii_lowercase())
    });

    let providers: Vec<Value> = names
        .iter()
        .map(|name| {
            let entry = &oai.providers[*name];
            let known = catalog::find(name);
            let resolved = catalog::resolve(&oai.providers, name);
            let (models, source) = effective(Some(entry), cached.get(*name));
            let (from, env) = key_state(resolved.as_ref());
            let base = resolved.as_ref().map(|found| found.base.clone()).unwrap_or_default();
            let ready = !base.is_empty() && from != "missing";
            // 勾选清单的全集：接口最近一次给的那份；没拉过就拿现在能用的凑合（手选时只有手选的）。
            let available = recalled(name).unwrap_or_else(|| models.to_vec());
            json!({
                "name": name,
                "label": known.map_or(name.as_str(), |known| known.name),
                "known": known.is_some(),
                "base": base,
                "base_own": !entry.api_base.trim().is_empty(),
                "key_from": from,
                "key_env": env,
                "ready": ready,
                "keys_url": known.map_or("", |known| known.keys_url),
                "env": known.map_or("", |known| known.env),
                "recommended": known.map_or("", |known| known.model),
                "models": models,
                "models_source": source,
                "curated": entry.models,
                "available": available,
                "fetched": recalled(name).is_some(),
                "uses": used(name),
            })
        })
        .collect();

    let catalog: Vec<Value> = catalog::KNOWN
        .iter()
        .map(|known| {
            json!({
                "id": known.id,
                "name": known.name,
                "base": known.base,
                "env": known.env,
                "keys_url": known.keys_url,
                "model": known.model,
                "keyless": known.keyless,
                "kind": known.kind.id(),
                // 配过的这家在配置里的写法（可能是别名、也可能大小写不同），页面按它找得到那一项。
                "entry": catalog::configured(&oai.providers, known.id).map(|(name, _)| name),
                "configured": catalog::configured(&oai.providers, known.id).is_some(),
            })
        })
        .collect();

    // 换默认模型时能选的全部模型：默认接口那份写裸 id，各家写 `供应商/模型`。
    let mut groups = Vec::new();
    if !relay_models.is_empty() {
        groups.push(json!({ "name": "", "label": "默认接口", "models": relay_models }));
    }
    for provider in &providers {
        if provider["ready"] != Value::Bool(true) {
            continue;
        }
        let name = provider["name"].as_str().unwrap_or_default();
        let models: Vec<String> = provider["models"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .map(|model| format!("{name}/{model}"))
                    .collect()
            })
            .unwrap_or_default();
        if !models.is_empty() {
            groups.push(json!({ "name": name, "label": provider["label"], "models": models }));
        }
    }

    json!({
        "default_model": current,
        "agents_follow": !oai.agent_default_model.is_empty(),
        "endpoint": {
            "base": relay_base,
            "key_set": relay_key,
            "models": relay_models,
            "uses": used(""),
        },
        "providers": providers,
        "catalog": catalog,
        "groups": groups,
    })
}

/// `POST /oai/provider`：新建或更新一家。密钥留空表示不动；模型留空表示跟随接口。
#[derive(Deserialize)]
pub(crate) struct SaveProvider {
    pub name: String,
    /// 不传 = 不动这一格（只改模型时走这条路）；传空串 = 跟随官方地址（认得的供应商才行）。
    #[serde(default)]
    pub api_base: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub models: Option<Vec<String>>,
}

pub(crate) async fn save(ctx: &Context, body: SaveProvider) -> Result<Value, String> {
    let oai: OaiConfig = get_config_or_default(ctx);
    let name = provider_name(&oai.providers, &body.name)?;
    let known = catalog::find(&name);
    let existing = oai.providers.get(&name).cloned();
    let created = existing.is_none();

    let existing_base = existing
        .as_ref()
        .map(|entry| entry.api_base.trim().to_string())
        .unwrap_or_default();
    let typed = body.api_base.as_deref().map(str::trim).unwrap_or("");
    let base = if typed.is_empty() { String::new() } else { utils::openai_api_base(typed) };
    if !base.is_empty() && !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err("接口地址要以 http:// 或 https:// 开头。".into());
    }
    // 没带这一格就沿用原来的；带了空串只有认得的供应商收得下（官方地址）。
    let stored_base = if body.api_base.is_none() { existing_base.clone() } else { base.clone() };
    let has_base = !stored_base.is_empty() || known.is_some();
    if !has_base {
        return Err("自己的中转站要写接口地址（OpenAI 兼容，通常以 /v1 结尾）。".into());
    }

    let typed_key = body.api_key.as_deref().map(str::trim).filter(|key| !key.is_empty());
    let key = typed_key
        .map(str::to_string)
        .or_else(|| existing.as_ref().map(|entry| entry.api_key.clone()))
        .unwrap_or_default();
    // 密钥允许稍后再补：页面会把它标成「缺密钥」；认得的供应商还可能已经在本机环境变量里。

    // 换了密钥或地址就顺手验一次：只读 /models，不计费。被拒（401/403）不存，
    // 网络抽风照存——本机连不上不代表这把钥匙不对。拉到的列表顺便给勾选清单用。
    let probe_base = if stored_base.is_empty() { known.map(|known| known.base.to_string()).unwrap_or_default() } else { stored_base.clone() };
    let base_changed = body.api_base.is_some() && stored_base != existing_base;
    let mut probed: Option<Vec<String>> = None;
    let mut note = String::new();
    if !key.trim().is_empty() && (typed_key.is_some() || base_changed || created) {
        let started = Instant::now();
        match data::list_models(&probe_base, &key).await {
            Ok(list) => {
                let kept = data::chat_models_only(list, &oai.model_filter);
                note = format!("连接正常，{} ms，{} 个可聊的模型。", started.elapsed().as_millis(), kept.len());
                probed = Some(kept);
            }
            Err(error) => {
                let text = error.to_string();
                if text.starts_with("密钥无效") {
                    return Err(format!("这家拒绝了这把密钥（{text}），没有保存。核对一下有没有多复制空格或少了几位。"));
                }
                note = format!("暂时没连上（{text}），先存下了。");
            }
        }
    }

    // 页面带没带模型：带了才是「以我挑的为准」，没带就沿用原来的手选清单。
    let wants_models = body.models.is_some();
    let models = match body.models {
        Some(list) => tidy_models(list)?,
        None => existing.as_ref().map(|entry| entry.models.clone()).unwrap_or_default(),
    };

    let mut table = toml::map::Map::new();
    table.insert("api_base".into(), Toml::String(stored_base));
    table.insert("api_key".into(), Toml::String(key));
    if !models.is_empty() {
        table.insert("models".into(), Toml::Array(models.iter().map(|model| Toml::String(model.clone())).collect()));
    }

    let entry_name = name.clone();
    let entry = Toml::Table(table);
    crate::plugins::ctl::change(ctx, move |cfg| {
        let oai = cfg.plugins.get_mut("oai").and_then(Toml::as_table_mut).ok_or("oai 配置不存在")?;
        let providers = oai
            .entry("providers")
            .or_insert_with(|| Toml::Table(Default::default()))
            .as_table_mut()
            .ok_or("oai.providers 不是表")?;
        providers.insert(entry_name, entry);
        Ok(String::new())
    })
    .await?;

    // 保存后把「手选」这份同步进 `/%` 用的缓存：换了模型不必等下一次刷新。
    let oai: OaiConfig = get_config_or_default(ctx);
    if let Some(list) = &probed {
        remember(&name, list);
    }
    if let Ok(mgr) = ensure_manager().await
        && !wants_models
        && models.is_empty()
        && let Some(list) = probed.clone()
    {
        // 跟随接口的这一家：验密钥时拉到的就是它的列表，不必再等一次刷新。
        let _ = mgr.set_provider_models(&name, list).await;
    }
    if let Ok(mgr) = ensure_manager().await
        && wants_models
    {
        if models.is_empty() {
            // 回到跟随接口：立刻拉一次，拉不到就把旧的手选缓存清掉，别让两处对不上。
            let fresh = match catalog::resolve(&oai.providers, &name) {
                Some(found) => match recalled(&name) {
                    Some(list) => list,
                    None => data::list_models(&found.base, &found.key)
                        .await
                        .map(|list| data::chat_models_only(list, &oai.model_filter))
                        .unwrap_or_default(),
                },
                None => Vec::new(),
            };
            let _ = mgr.set_provider_models(&name, fresh).await;
        } else {
            let _ = mgr.set_provider_models(&name, models.clone()).await;
        }
    }

    let label = known.map_or(name.as_str(), |known| known.name);
    let verb = if created { "已添加" } else { "已保存" };
    let message = if note.is_empty() { format!("{verb} {label}。") } else { format!("{verb} {label}：{note}") };
    Ok(json!({
        "name": name,
        "message": message,
        "overview": overview(ctx).await,
    }))
}

pub(crate) async fn remove(ctx: &Context, raw: &str) -> Result<Value, String> {
    let oai: OaiConfig = get_config_or_default(ctx);
    let Some((name, _)) = catalog::configured(&oai.providers, raw) else {
        return Err(format!("没有供应商「{}」。", raw.trim()));
    };
    let name = name.to_string();
    let mut note = String::new();
    if let (Some(provider), _) = utils::split_provider(&oai.agent_default_model)
        && provider.eq_ignore_ascii_case(&name)
    {
        note = String::from("\n默认模型还指着它，到上面换一个能用的。");
    }
    let entry = name.clone();
    crate::plugins::ctl::change(ctx, move |cfg| {
        let oai = cfg.plugins.get_mut("oai").and_then(Toml::as_table_mut).ok_or("oai 配置不存在")?;
        let providers = oai
            .get_mut("providers")
            .and_then(Toml::as_table_mut)
            .ok_or("oai.providers 不是表")?;
        providers.remove(&entry).ok_or("这家供应商已经删掉了")?;
        Ok(format!("已删除供应商 {entry}。"))
    })
    .await?;
    if let Ok(mgr) = ensure_manager().await {
        let _ = mgr.set_provider_models(&name, Vec::new()).await;
    }
    forget(&name);
    Ok(json!({
        "message": format!("已删除供应商 {name}。{note}"),
        "overview": overview(ctx).await,
    }))
}

/// `POST /oai/models`：测试连接并拉一次这家的模型列表。只读 `GET /models`，不计费。
///
/// 带了 `api_base` / `api_key` 就拿页面上还没保存的那一份试（添加之前先试一试），
/// 这时什么都不存；不带就用配置里的，拉到的顺便写进缓存。
#[derive(Deserialize)]
pub(crate) struct FetchModels {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub api_base: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
}

pub(crate) async fn fetch(ctx: &Context, body: FetchModels) -> Result<Value, String> {
    let oai: OaiConfig = get_config_or_default(ctx);
    let mgr = ensure_manager().await.map_err(|error| error.to_string())?;
    let name = body.provider.as_deref().map(str::trim).filter(|name| !name.is_empty());
    let typed_base = body.api_base.as_deref().map(str::trim).filter(|base| !base.is_empty()).map(utils::openai_api_base);
    let typed_key = body.api_key.as_deref().map(str::trim).filter(|key| !key.is_empty()).map(str::to_string);
    let draft = typed_base.is_some() || typed_key.is_some();

    // 先按配置解出这一家现在的地址与密钥，再拿页面上敲的覆盖。还没加进配置的一家
    // （添加页上先试一试）：认得的补官方地址、读环境变量里的密钥，不认得的只能靠页面上的。
    let (base, key, curated, label, display, configured) = if let Some(name) = name {
        let entry = catalog::configured(&oai.providers, name);
        let known = catalog::find(name);
        if entry.is_none() && known.is_none() && !draft {
            return Err(format!("没有供应商「{name}」。"));
        }
        let label = entry.map_or(name, |(configured, _)| configured).to_string();
        let found = catalog::resolve(&oai.providers, &label);
        (
            found.as_ref().map(|found| found.base.clone()).unwrap_or_default(),
            found.map(|found| found.key).unwrap_or_default(),
            entry.map(|(_, entry)| entry.models.clone()).unwrap_or_default(),
            label.clone(),
            known.map_or(label.as_str(), |known| known.name).to_string(),
            entry.is_some(),
        )
    } else {
        let c = mgr.config.read().await;
        (c.api_base.clone(), c.api_key.clone(), Vec::new(), String::new(), "默认接口".to_string(), true)
    };
    let base = typed_base.unwrap_or(base);
    let key = typed_key.unwrap_or(key);
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err(format!("{display} 还没有能用的接口地址（要以 http:// 或 https:// 开头）。"));
    }
    if key.trim().is_empty() {
        return Err(format!("{display} 还没有密钥，先填上再试。"));
    }

    let started = Instant::now();
    let raw = data::list_models(&base, &key)
        .await
        .map_err(|error| format!("{display} 连不上：{error}"))?;
    let ms = started.elapsed().as_millis() as u64;
    let total = raw.len();
    let kept = data::chat_models_only(raw, &oai.model_filter);

    // 页面上的草稿只试不存；手选过模型的这一家也不动缓存（那份就是它的列表）。
    let stored = if draft || !configured {
        false
    } else {
        remember(&label, &kept);
        if !curated.is_empty() {
            false
        } else if name.is_some() {
            mgr.set_provider_models(&label, kept.clone()).await.is_ok()
        } else {
            let mut c = mgr.config.write().await;
            c.models = kept.clone();
            mgr.save(&mut c).is_ok()
        }
    };
    let filtered = if total > kept.len() { format!("，按模型过滤留下 {} 个可聊的", kept.len()) } else { String::new() };
    Ok(json!({
        "message": format!("{display} 连接正常：{ms} ms，接口给了 {total} 个模型{filtered}。"),
        "ms": ms,
        "models": kept,
        "total": total,
        "stored": stored,
        "draft": draft || !configured,
    }))
}

/// `POST /oai/default`：换默认模型。空串表示跟随内置默认。
#[derive(Deserialize)]
pub(crate) struct DefaultModel {
    #[serde(default)]
    pub model: String,
}

pub(crate) async fn set_default(ctx: &Context, model: &str) -> Result<Value, String> {
    let model = model.trim();
    if !model.is_empty() {
        let oai: OaiConfig = get_config_or_default(ctx);
        let mgr = ensure_manager().await.map_err(|error| error.to_string())?;
        let (base, key) = {
            let c = mgr.config.read().await;
            (c.api_base.clone(), c.api_key.clone())
        };
        let (provider, _) = utils::split_provider(model);
        let ready = super::resolve_endpoint(&oai.providers, &base, &key, provider.as_deref())
            .is_some_and(|(base, key)| !base.is_empty() && !key.trim().is_empty());
        if !ready {
            return Err(match provider {
                Some(name) => format!("{name} 还没接上（缺地址或密钥），先在上面补好再设默认。"),
                None => "默认接口还没配好：先在上面填中转站，或换一个带供应商前缀的模型。".into(),
            });
        }
    }
    let spec = model.to_string();
    let said = if spec.is_empty() {
        "默认模型已改回跟随内置默认。没单独写模型的房间下一句就跟着用。".to_string()
    } else {
        format!("默认模型已换成 {spec}。没单独写模型的房间下一句就跟着用。")
    };
    crate::plugins::ctl::change(ctx, move |cfg| {
        let oai = cfg.plugins.get_mut("oai").and_then(Toml::as_table_mut).ok_or("oai 配置不存在")?;
        oai.insert("agent_default_model".into(), Toml::String(spec));
        Ok(String::new())
    })
    .await?;
    Ok(json!({
        "message": said,
        "overview": overview(ctx).await,
    }))
}

/// `POST /oai/endpoint`：默认接口（中转站）。地址与密钥存在 `data/oai/config.json`，
/// 与 `oai <地址> <密钥>` 同一份；密钥留空表示不动。
#[derive(Deserialize)]
pub(crate) struct Endpoint {
    #[serde(default)]
    pub api_base: String,
    #[serde(default)]
    pub api_key: Option<String>,
}

pub(crate) async fn save_endpoint(ctx: &Context, body: Endpoint) -> Result<Value, String> {
    let mgr = ensure_manager().await.map_err(|error| error.to_string())?;
    let typed = body.api_base.trim();
    if typed.is_empty() {
        return Err("填中转站的接口地址（OpenAI 兼容，通常以 /v1 结尾）。".into());
    }
    let base = utils::openai_api_base(typed);
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err("接口地址要以 http:// 或 https:// 开头。".into());
    }
    let typed_key = body.api_key.as_deref().map(str::trim).filter(|key| !key.is_empty());
    let key = match typed_key {
        Some(key) => key.to_string(),
        None => mgr.config.read().await.api_key.clone(),
    };
    if key.trim().is_empty() {
        return Err("还缺一把密钥；密钥只存在本机，不会回显。".into());
    }

    // 先验一次：只读模型列表，不消耗对话额度；401 就不存，网络抽风照存。
    let oai: OaiConfig = get_config_or_default(ctx);
    let (note, models) = match data::list_models(&base, &key).await {
        Ok(list) => {
            let kept = data::chat_models_only(list, &oai.model_filter);
            (format!("{} 个模型", kept.len()), Some(kept))
        }
        Err(error) => {
            let text = error.to_string();
            if text.starts_with("密钥无效") {
                return Err(format!("这个接口拒绝了这把密钥（{text}），没有保存。"));
            }
            (format!("暂时没连上（{text}），先存下了"), None)
        }
    };
    let mut c = mgr.config.write().await;
    c.api_base = base.clone();
    c.api_key = key;
    if let Some(models) = models {
        c.models = models;
    }
    mgr.save(&mut c).map_err(|error| format!("保存失败，已恢复原配置：{error}"))?;
    drop(c);
    Ok(json!({
        "message": format!("默认接口已存：{base}（{note}）。不带供应商前缀的模型走这里。"),
        "overview": overview(ctx).await,
    }))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_chat_rules() {
        let providers = HashMap::new();
        assert_eq!(provider_name(&providers, " deepseek ").unwrap(), "deepseek");
        assert_eq!(provider_name(&providers, "Kimi").unwrap(), "moonshot");
        assert!(provider_name(&providers, "有中文").is_err());
        assert!(provider_name(&providers, "").is_err());
        assert!(provider_name(&providers, "a b").is_err());
    }

    #[test]
    fn models_are_trimmed_and_deduped() {
        let list = tidy_models(vec![" a ".into(), "a".into(), "".into(), "b".into()]).unwrap();
        assert_eq!(list, ["a", "b"]);
        assert!(tidy_models(vec!["a b".into()]).is_err());
    }

    #[test]
    fn usage_finds_who_relies_on_each_provider() {
        let mut oai = OaiConfig::default();
        oai.providers.insert("deepseek".into(), ProviderConfig::default());
        oai.providers.insert("moonshot".into(), ProviderConfig::default());
        oai.media.image = "kimi".into();
        let agents = vec![
            ("管家大人".to_string(), String::new()),
            ("翻译".to_string(), "Kimi/kimi-k3".to_string()),
            ("老房间".to_string(), "gpt-4o".to_string()),
        ];
        let map = usage(&oai, "deepseek/deepseek-flash", &agents);
        let deepseek = &map["deepseek"];
        assert!(deepseek.default);
        assert_eq!(deepseek.follow, 1);
        let moonshot = &map["moonshot"];
        assert_eq!(moonshot.rooms, ["翻译"]);
        assert_eq!(moonshot.media, ["画图"]);
        assert_eq!(map[""].rooms, ["老房间"]);
    }

    #[test]
    fn curated_models_win_over_the_cache() {
        let entry = ProviderConfig { models: vec!["picked".into()], ..Default::default() };
        let cached = vec!["cached".into()];
        assert_eq!(effective(Some(&entry), Some(&cached)), (&["picked".to_string()][..], "curated"));
        let empty = ProviderConfig::default();
        assert_eq!(effective(Some(&empty), Some(&cached)).1, "cache");
        assert_eq!(effective(None, None).1, "none");
    }
}
