use super::types::{Config, GeneratingState, MjCache, Tools};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use tokio::sync::RwLock;

use super::LOG_TARGET;

const DEFAULT_MODEL: &str = "deepseek/deepseek-flash";
const LEGACY_DEFAULT_MODEL: &str = "gpt-4o";
const CURRENT_DEFAULTS_VERSION: u32 = 6;

/// 旧版建房间时自动填充的默认系统提示词，现已改为留空；迁移时按原样匹配后清掉。
const LEGACY_DEFAULT_PROMPT: &str = "You are a helpful assistant.";

/// 内置 agent 房间的默认名字。
///
/// 取四个字是刻意的：房间指令是前缀匹配的（`parse_agent_cmd`），名字越短越容易在
/// 日常聊天里撞上；「管家大人」既不会被顺口带出，也叫得明白。
pub(crate) const BUILTIN_ROOM: &str = "管家大人";

/// 内置 agent 房间的人设。
///
/// 只写「是谁、什么风格」；运行环境、工具策略与排版要求由内置 agent 自己生成——
/// 那些内容写死在人设里会随时间过期，也没法随工具集变化。
const BUILTIN_PERSONA: &str = "你是管家大人：一位干练的老管家，务实、可靠、有分寸，不卖萌也不谄媚。\
办正事时先给结论再给依据，条理清楚、不说废话；该提醒的风险直说，不绕弯。\
闲聊时随和自然，一两句就好，偶尔带点不动声色的幽默。对号主忠心但不盲从，对客人客气但守得住分寸。";

/// 从前几版的内置人设：房间里还是原样的就换成新的（改过的不碰）。
const RETIRED_PERSONAS: [&str; 2] = [
    "你是管家大人：务实、可靠、有分寸，不卖萌也不谄媚。谁问都先给结论，再给依据；该提醒的风险直说，不绕弯。",
    "你是管家大人：务实、可靠、有分寸，不卖萌也不谄媚。号主问什么先给结论，再给依据；该提醒他注意的风险直说，不绕弯。",
];

/// 从前随启动补建的画图 / 音乐 / 视频预设房间都挂在这几个分区下（1.11.0 起不再内置）。
const RETIRED_PRESET_SECTIONS: [&str; 3] = ["画图预设", "音乐房间", "视频房间"];

// 全局单例管理器
pub static MANAGER: OnceLock<Arc<Manager>> = OnceLock::new();

pub struct Manager {
    pub config: RwLock<Config>,
    persisted: std::sync::Mutex<Config>,
    pub generating: RwLock<GeneratingState>,
    /// 正在进行的智能体轮次接收「追加话」的信箱，键同 `GeneratingState` 的会话（见 [`Manager::steer_key`]）。
    steering: std::sync::Mutex<HashMap<String, Arc<super::agent::Steering>>>,
    pub mj_cache: RwLock<MjCache>,
    pub mj_inflight: RwLock<HashSet<String>>,
    pub path: PathBuf,
    pub mj_cache_path: PathBuf,
    pub mj_images_dir: PathBuf,
}

impl Manager {
    pub fn new(dir: PathBuf) -> Self {
        let path = dir.join("config.json");
        let mj_cache_path = dir.join("mj-cache.json");
        let mj_images_dir = dir.join("mj-images");
        // 同步加载一次配置 (初始化时使用)
        let default = Config {
            default_model: DEFAULT_MODEL.to_string(),
            ..Default::default()
        };

        let mut config = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(s) => serde_json::from_str(&s).unwrap_or(default),
                Err(_) => default,
            }
        } else {
            default
        };

        let mut config_dirty = false;
        // 将旧版默认值迁移到工具调用能力更完整的模型；只执行一次，不干预后续手动设置。
        if config.defaults_version < CURRENT_DEFAULTS_VERSION {
            if config.default_model.trim().is_empty()
                || config
                    .default_model
                    .eq_ignore_ascii_case(LEGACY_DEFAULT_MODEL)
            {
                config.default_model = DEFAULT_MODEL.to_string();
            }
            // 不再默认填充「你是一个有帮助的助手」：清掉历史建房时被写入该默认值的房间。
            // 只匹配完全相同的那句，避免误伤管理员自定义的提示词。
            for agent in &mut config.agents {
                if agent.system_prompt == LEGACY_DEFAULT_PROMPT {
                    agent.system_prompt = String::new();
                }
            }
            // 内置的画图 / 音乐 / 视频预设房间不再提供：一并收走，`/#` 里只剩自己建的与管家大人。
            let before = config.agents.len();
            config.agents.retain(|agent| !RETIRED_PRESET_SECTIONS.contains(&agent.section.trim()));
            if config.agents.len() < before {
                info!(target: LOG_TARGET, "已移除 {} 间内置预设房间", before - config.agents.len());
            }
            config.seeded_presets.retain(|name| name.eq_ignore_ascii_case(BUILTIN_ROOM));
            // 早先补建的管家大人把中转站默认模型抄成了自己的模型：那个模型要中转站密钥，
            // 只填了 DeepSeek 的新用户一问就是「API 未配置」。改回「跟随默认模型」。
            for agent in &mut config.agents {
                if ["agent", "pi"].contains(&agent.engine.trim().to_ascii_lowercase().as_str())
                    && agent.name.eq_ignore_ascii_case(BUILTIN_ROOM)
                    && [DEFAULT_MODEL, "deepseek/deepseek-flash"]
                        .iter()
                        .any(|model| agent.model.trim().eq_ignore_ascii_case(model))
                {
                    agent.model = String::new();
                }
            }
            // 1.22.0 起不再分「普通房间 / 智能体房间」，所有房间都是智能体，差别只在工具档位：
            // 从前的智能体房间开全量，普通房间不挂工具——升级前后每间房能做的事不变。
            for agent in &mut config.agents {
                agent.tools = if ["agent", "pi"].contains(&agent.engine.trim().to_ascii_lowercase().as_str()) {
                    Tools::Full
                } else {
                    Tools::Off
                };
            }
            config.defaults_version = CURRENT_DEFAULTS_VERSION;
            config_dirty = true;
        }

        // `模型:强度` 的旧写法收进独立的 `thinking` 字段：行为等价，但让强度可单独
        // 修改而不必重写模型名。幂等——已经收好的房间再跑一次什么都不做。
        for agent in &mut config.agents {
            let (model, thinking) = super::utils::split_thinking(&agent.model);
            if let Some(level) = thinking {
                agent.model = model;
                agent.thinking = level;
                config_dirty = true;
            }
        }

        // 人设还是从前内置的原文（房间改了名也算）：换成这一版。
        for agent in &mut config.agents {
            if RETIRED_PERSONAS.contains(&agent.system_prompt.trim()) {
                agent.system_prompt = BUILTIN_PERSONA.to_string();
                config_dirty = true;
            }
        }

        // 内置智能体房间建过一次就记名：删掉的不复活。
        if !config
            .seeded_presets
            .iter()
            .any(|name| name.eq_ignore_ascii_case(BUILTIN_ROOM))
        {
            if !config
                .agents
                .iter()
                .any(|agent| agent.name.eq_ignore_ascii_case(BUILTIN_ROOM))
            {
                // 模型留空 = 跟随 `[oai] agent_default_model`（默认 deepseek/deepseek-flash）：
                // 填了 DeepSeek 密钥就能用，换默认模型时也不必逐间房去改。
                // 管家大人是来办事的：别的房间建出来不挂工具，它一建好就是全量。
                let mut room = super::types::Agent::new(
                    BUILTIN_ROOM,
                    "",
                    BUILTIN_PERSONA,
                    "终端与联网工具助手",
                );
                room.tools = Tools::Full;
                config.agents.push(room);
            }
            config.seeded_presets.push(BUILTIN_ROOM.to_string());
            config_dirty = true;
        }

        if config_dirty && let Err(error) = write_config(&path, &config) {
            error!(target: LOG_TARGET, "初始化配置写入失败：{error:#}");
        }

        let mj_cache = std::fs::read_to_string(&mj_cache_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let _ = std::fs::create_dir_all(&mj_images_dir);

        Self {
            persisted: std::sync::Mutex::new(config.clone()),
            config: RwLock::new(config),
            generating: RwLock::new(GeneratingState::default()),
            steering: std::sync::Mutex::new(HashMap::new()),
            mj_cache: RwLock::new(mj_cache),
            mj_inflight: RwLock::new(HashSet::new()),
            path,
            mj_cache_path,
            mj_images_dir,
        }
    }

    /// 同一个会话的键：公共房间一间一个，私有历史每人一个。
    fn steer_key(agent: &str, private: bool, uid: &str) -> String {
        if private {
            format!("{agent}\u{0}{uid}")
        } else {
            agent.to_string()
        }
    }

    fn steering(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<super::agent::Steering>>> {
        self.steering.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 开一个信箱：这一轮进行当中，发起人的话可以排进来。
    pub(crate) fn open_steering(&self, agent: &str, private: bool, uid: &str) -> Arc<super::agent::Steering> {
        let steering = super::agent::Steering::new(uid);
        self.steering().insert(Self::steer_key(agent, private, uid), steering.clone());
        steering
    }

    /// 这一轮结束：撤掉信箱，交出没来得及处理的话。
    pub(crate) fn close_steering(&self, agent: &str, private: bool, uid: &str) -> Vec<String> {
        match self.steering().remove(&Self::steer_key(agent, private, uid)) {
            Some(steering) => steering.close(),
            None => Vec::new(),
        }
    }

    /// 往进行中的轮次追加一句；这间房此刻没有开着的信箱、或话不该排进去时返回 `false`。
    pub(crate) fn steer(&self, agent: &str, private: bool, uid: &str, text: String) -> bool {
        self.steering()
            .get(&Self::steer_key(agent, private, uid))
            .is_some_and(|steering| steering.push(uid, text))
    }

    /// 原子保存；失败恢复最后一次持久化状态，调用者必须报告失败。
    pub fn save(&self, cfg: &mut Config) -> anyhow::Result<()> {
        let mut persisted = self.persisted.lock().unwrap();
        match write_config(&self.path, cfg) {
            Ok(()) => {
                *persisted = cfg.clone();
                Ok(())
            }
            Err(error) => {
                *cfg = persisted.clone();
                Err(error)
            }
        }
    }

    /// 把一家供应商的模型列表写成「手选」的那份缓存（控制台改完模型后调用）。
    /// 传空表表示不再手选，缓存也一并清掉，等下一次刷新重新拉。
    pub async fn set_provider_models(&self, provider: &str, models: Vec<String>) -> anyhow::Result<()> {
        let mut c = self.config.write().await;
        if models.is_empty() {
            c.provider_models.remove(provider);
        } else {
            c.provider_models.insert(provider.to_string(), models);
        }
        self.save(&mut c)
    }

    pub fn save_mj_cache(&self, cache: &MjCache) {
        if let Ok(s) = serde_json::to_string_pretty(cache)
            && let Err(error) = crate::storage::write_atomic(&self.mj_cache_path, s.as_bytes())
        {
            warn!(target: LOG_TARGET, "写入 MJ 缓存失败：{error}");
        }
    }

    /// 拉取模型列表：默认接口一份（按 `filter` 收敛），每家配好的供应商各一份。
    ///
    /// 过滤规则来自 `[oai] model_filter`，供应商表来自 `[oai.providers]`，都由调用方读好传入——
    /// Manager 是全局单例，够不到 `Context` 里的配置。哪一家拉不到就保留它上次的那份，
    /// 全都没配或全都失败才报错。返回合并后的列表（与 `/%` 的序号一致）。
    pub async fn fetch_models(
        &self,
        filter: &super::utils::ModelFilterConfig,
        providers: &HashMap<String, super::ProviderConfig>,
    ) -> anyhow::Result<Vec<String>> {
        let (base, key) = {
            let c = self.config.read().await;
            (c.api_base.clone(), c.api_key.clone())
        };
        let ready = super::catalog::ready(providers);
        if base.is_empty() && ready.is_empty() {
            return Err(anyhow::anyhow!("还没配模型接口：发 `oai deepseek <密钥>`，或 `oai <地址> <密钥>` 接中转站"));
        }

        let relay = async {
            if base.is_empty() {
                return None;
            }
            Some(list_models(&base, &key).await.map(|models| {
                // 过滤结果为空说明关键字写错了，此时不再回落到完整列表——
                // 悄悄塞回上千个模型比空列表更难排查。空的 `keep` 本身已经表示"全部接受"。
                let mut kept = super::utils::filter_models(&models, filter);
                if kept.is_empty() && !models.is_empty() {
                    warn!(
                        target: LOG_TARGET,
                        "站点返回 {} 个模型，但 [oai] model_filter 过滤后为空，请检查 keep/drop 关键字",
                        models.len()
                    );
                }
                for model in super::mj::MJ_MODELS {
                    if !kept.iter().any(|m| m == model) {
                        kept.push((*model).to_string());
                    }
                }
                kept
            }))
        };
        let official = futures_util::future::join_all(ready.iter().map(|(name, _)| async move {
            let found = super::catalog::resolve(providers, name)?;
            let listed = list_models(&found.base, &found.key)
                .await
                .map(|models| chat_models_only(models, filter));
            Some((name.clone(), listed))
        }));
        let (relay, official) = tokio::join!(relay, official);

        let mut errors = Vec::new();
        let mut c = self.config.write().await;
        match relay {
            Some(Ok(models)) => c.models = models,
            Some(Err(error)) => errors.push(format!("默认接口：{error}")),
            None => c.models.clear(),
        }
        // 撤掉了的供应商连同它的列表一起收走，免得 `/%` 里挂着用不了的模型。
        c.provider_models.retain(|name, _| ready.iter().any(|(ready, _)| ready == name));
        for (name, listed) in official.into_iter().flatten() {
            // 手选过模型的供应商以手选为准：刷新是去发现新模型的，不该把它挑剩的那几枚冲掉。
            let curated = providers.get(&name).map(|entry| entry.models.clone()).filter(|list| !list.is_empty());
            match listed {
                Ok(models) => {
                    c.provider_models.insert(name, curated.unwrap_or(models));
                }
                Err(error) => {
                    if let Some(models) = curated {
                        c.provider_models.insert(name.clone(), models);
                    }
                    errors.push(format!("{name}：{error}"));
                }
            }
        }
        let all = Self::all_models(&c);
        self.save(&mut c)?;
        drop(c);
        if !errors.is_empty() {
            warn!(target: LOG_TARGET, "模型列表有几家没拉到：{}", errors.join("；"));
            if all.is_empty() {
                return Err(anyhow::anyhow!("模型列表请求失败：{}", errors.join("；")));
            }
        }
        Ok(all)
    }

    /// `/%` 的那份完整列表：默认接口的裸模型名在前，各家供应商的 `供应商/模型` 接在后面。
    /// 指令里写序号就是写这份列表的第几个。
    pub fn all_models(config: &Config) -> Vec<String> {
        let mut all = config.models.clone();
        for (name, models) in &config.provider_models {
            all.extend(models.iter().map(|model| format!("{name}/{model}")));
        }
        all
    }

    pub fn resolve_model(&self, input: &str, models: &[String]) -> Option<String> {
        if input.is_empty() {
            return None;
        }
        if let Ok(i) = input.parse::<usize>()
            && i > 0
            && i <= models.len()
        {
            return Some(models[i - 1].clone());
        }
        let lower = input.to_lowercase();
        if let Some(exact) = models.iter().find(|model| model.to_lowercase() == lower) {
            return Some(exact.clone());
        }
        // 只写了模型名、没写供应商：哪家有这个名字就用哪家（`deepseek-flash` → `deepseek/deepseek-flash`）。
        if let Some(exact) = models
            .iter()
            .find(|model| model.rsplit_once('/').is_some_and(|(_, id)| id.to_lowercase() == lower))
        {
            return Some(exact.clone());
        }
        for m in models {
            if m.to_lowercase().contains(&lower) {
                return Some(m.clone());
            }
        }
        Some(input.to_string())
    }

    pub async fn agent_names(&self) -> Vec<String> {
        self.config
            .read()
            .await
            .agents
            .iter()
            .map(|a| a.name.clone())
            .collect()
    }
}

/// `GET {base}/models`，只取 id。
///
/// 自己发请求而不用强类型客户端：DeepSeek 官方（及多数中转）返回的 model 对象
/// 字段不全（缺 created），强类型反序列化会失败，这里宽松解析只取 id。
pub(crate) async fn list_models(base: &str, key: &str) -> anyhow::Result<Vec<String>> {
    let base = base.trim_end_matches('/');
    let mut urls = vec![format!("{base}/models")];
    if !base.ends_with("/v1") && url::Url::parse(base).is_ok_and(|url| matches!(url.path(), "" | "/")) {
        urls.push(format!("{base}/v1/models"));
    }
    let mut last_error = String::new();
    for url in urls {
        match crate::http::client()
            .get(&url)
            .bearer_auth(key)
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
                Ok(body) => {
                    let mut models: Vec<String> = body
                        .get("data")
                        .and_then(|d| d.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|m| m.get("id").and_then(|id| id.as_str()))
                                .map(ToString::to_string)
                                .collect()
                        })
                        .unwrap_or_default();
                    models.sort();
                    models.dedup();
                    return Ok(models);
                }
                Err(e) => last_error = format!("{url}: {e}"),
            },
            Ok(resp) if matches!(resp.status().as_u16(), 401 | 403) => {
                return Err(anyhow::anyhow!("密钥无效（HTTP {}）", resp.status().as_u16()));
            }
            Ok(resp) => last_error = format!("{url}: HTTP {}", resp.status().as_u16()),
            Err(e) => last_error = format!("{url}: {e}"),
        }
    }
    Err(anyhow::anyhow!("{last_error}"))
}

/// 供应商官方列表里只留聊天模型：语音、向量、重排这些挂进房间也说不了话。
/// 列表太长（OpenRouter 有几百个）时再套一遍 `[oai] model_filter`。
pub(crate) fn chat_models_only(models: Vec<String>, filter: &super::utils::ModelFilterConfig) -> Vec<String> {
    const NOT_CHAT: &[&str] = &[
        "tts", "asr", "embed", "whisper", "rerank", "moderation", "transcribe", "speech", "audio", "voice", "dall-e",
    ];
    let chat: Vec<String> = models
        .into_iter()
        .filter(|model| {
            let lower = model.to_lowercase();
            !NOT_CHAT.iter().any(|word| lower.contains(word))
        })
        .collect();
    if chat.len() > 40 {
        let kept = super::utils::filter_models(&chat, filter);
        if !kept.is_empty() {
            return kept;
        }
    }
    chat
}

fn write_config(path: &std::path::Path, cfg: &Config) -> anyhow::Result<()> {
    crate::storage::write_atomic_private(path, serde_json::to_string_pretty(cfg)?.as_bytes())?;
    Ok(())
}
