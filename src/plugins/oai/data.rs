use super::types::{Config, GeneratingState, MjCache};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use tokio::sync::RwLock;

const DEFAULT_MODEL: &str = "gpt-5.6-luna";
const LEGACY_DEFAULT_MODEL: &str = "gpt-4o";
const CURRENT_DEFAULTS_VERSION: u32 = 4;

/// 旧版建房间时自动填充的默认系统提示词，现已改为留空；迁移时按原样匹配后清掉。
const LEGACY_DEFAULT_PROMPT: &str = "You are a helpful assistant.";

/// 内置 agent 房间的默认名字。
///
/// 取四个字是刻意的：房间指令是前缀匹配的（`parse_agent_cmd`），名字越短越容易在
/// 日常聊天里撞上；「管家大人」既不会被顺口带出，也叫得明白。
const BUILTIN_ROOM: &str = "管家大人";

/// 内置 agent 房间的人设。
///
/// 只写「是谁、什么风格」；运行环境、工具策略与排版要求由内置 agent 自己生成——
/// 那些内容写死在人设里会随时间过期，也没法随工具集变化。
const BUILTIN_PERSONA: &str = "你是管家大人，一个务实、直接的通用助手，回答简洁但不省略关键依据。";

// 全局单例管理器
pub static MANAGER: OnceLock<Arc<Manager>> = OnceLock::new();

pub struct Manager {
    pub config: RwLock<Config>,
    persisted: std::sync::Mutex<Config>,
    pub generating: RwLock<GeneratingState>,
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
            for agent in config.agents.iter_mut() {
                if agent.system_prompt == LEGACY_DEFAULT_PROMPT {
                    agent.system_prompt = String::new();
                }
            }
            config.defaults_version = CURRENT_DEFAULTS_VERSION;
            config_dirty = true;
        }

        // 引擎关键字从 `pi` 改成 `agent`：老配置里写着旧值，改一次就好。
        for agent in config.agents.iter_mut() {
            if agent.engine.trim().eq_ignore_ascii_case("pi") {
                agent.engine = super::types::ENGINE_AGENT.to_string();
                config_dirty = true;
            }
        }

        // `模型:强度` 的旧写法收进独立的 `thinking` 字段：行为等价，但让强度可单独
        // 修改而不必重写模型名。幂等——已经收好的房间再跑一次什么都不做。
        for agent in config.agents.iter_mut() {
            let (model, thinking) = super::utils::split_thinking(&agent.model);
            if let Some(level) = thinking {
                agent.model = model;
                agent.thinking = level;
                config_dirty = true;
            }
        }

        // 内置智能体房间和其他内置房间一样，建过一次就记名：删掉的不复活，
        // 而新加的房间（新预设）仍会补建，因为它的名字还不在表里。
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
                let model = if config.default_model.trim().is_empty() {
                    DEFAULT_MODEL
                } else {
                    &config.default_model
                };
                let mut room = super::types::Agent::new(
                    BUILTIN_ROOM,
                    model,
                    BUILTIN_PERSONA,
                    "终端与联网工具助手",
                );
                room.set_engine(super::types::ENGINE_AGENT, model);
                config.agents.push(room);
            }
            config.seeded_presets.push(BUILTIN_ROOM.to_string());
            config_dirty = true;
        }
        // 内置画图 / 音乐 / 视频预设：建过一次就记下名字，删掉的不复活，新加的才补建。
        let created = super::presets::seed(&mut config);
        if created > 0 {
            config_dirty = true;
            info!(
                target: "Plugin/OAI",
                "已建好 {created} 间预设房间（/# 里的「{}」「{}」「{}」分区）",
                super::presets::SECTION,
                super::presets::MUSIC_SECTION,
                super::presets::VIDEO_SECTION
            );
        }

        if config_dirty && let Err(error) = write_config(&path, &config) {
            error!(target: "Plugin/OAI", "初始化配置写入失败：{error:#}");
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
            mj_cache: RwLock::new(mj_cache),
            mj_inflight: RwLock::new(HashSet::new()),
            path,
            mj_cache_path,
            mj_images_dir,
        }
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

    pub fn save_mj_cache(&self, cache: &MjCache) {
        if let Ok(s) = serde_json::to_string_pretty(cache) {
            let _ = std::fs::write(&self.mj_cache_path, s);
        }
    }

    /// 拉取并按 `filter` 收敛模型列表。过滤规则来自 `[oai] model_filter`，
    /// 由调用方读取后传入——Manager 是全局单例，够不到 `Context` 里的配置。
    pub async fn fetch_models(
        &self,
        filter: &super::utils::ModelFilterConfig,
    ) -> anyhow::Result<Vec<String>> {
        let (base, key) = {
            let c = self.config.read().await;
            (c.api_base.clone(), c.api_key.clone())
        };
        if base.is_empty() {
            return Err(anyhow::anyhow!("API未配置"));
        }

        // 自实现 GET {base}/models：DeepSeek 官方（及多数中转）返回的 model 对象
        // 字段不全（缺 created），async-openai 的强类型反序列化会失败，这里宽松解析只取 id。
        let base = base.trim_end_matches('/');
        let mut urls = vec![format!("{base}/models")];
        if !base.ends_with("/v1") {
            urls.push(format!("{base}/v1/models"));
        }
        let mut body = None;
        let mut last_error = String::new();
        for url in urls {
            match crate::http::client()
                .get(&url)
                .bearer_auth(&key)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => match resp.json().await {
                    Ok(value) => {
                        body = Some(value);
                        break;
                    }
                    Err(e) => last_error = format!("{url}: {e}"),
                },
                Ok(resp) => last_error = format!("{url}: HTTP {}", resp.status().as_u16()),
                Err(e) => last_error = format!("{url}: {e}"),
            }
        }
        let body: serde_json::Value =
            body.ok_or_else(|| anyhow::anyhow!("模型列表请求失败: {last_error}"))?;
        let mut models: Vec<String> = body
            .get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("id").and_then(|id| id.as_str()))
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        models.sort();
        models.dedup();

        // 过滤结果为空说明关键字写错了，此时不再回落到完整列表——
        // 悄悄塞回上千个模型比空列表更难排查。空的 `keep` 本身已经表示"全部接受"。
        let mut final_models = super::utils::filter_models(&models, filter);
        if final_models.is_empty() && !models.is_empty() {
            warn!(
                target: "Plugin/OAI",
                "站点返回 {} 个模型，但 [oai] model_filter 过滤后为空，请检查 keep/drop 关键字",
                models.len()
            );
        }
        for model in super::mj::MJ_MODELS {
            if !final_models.iter().any(|m| m == model) {
                final_models.push((*model).to_string());
            }
        }

        {
            let mut c = self.config.write().await;
            c.models = final_models.clone();
            self.save(&mut c)?;
        }
        Ok(final_models)
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

fn write_config(path: &std::path::Path, cfg: &Config) -> anyhow::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("json.tmp");
    let result = (|| -> anyhow::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(serde_json::to_string_pretty(cfg)?.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn failed_save_restores_memory_and_preserves_disk() {
        let dir = std::env::temp_dir().join(format!("acumen-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mgr = super::Manager::new(dir.clone());
        let mut config = mgr.config.write().await;
        mgr.save(&mut config).unwrap();
        let previous = std::fs::read(&mgr.path).unwrap();
        let model = config.default_model.clone();
        std::fs::create_dir(mgr.path.with_extension("json.tmp")).unwrap();
        config.default_model = "unsaved".into();
        assert!(mgr.save(&mut config).is_err());
        assert_eq!(config.default_model, model);
        assert_eq!(std::fs::read(&mgr.path).unwrap(), previous);
        std::fs::remove_dir_all(dir).unwrap();
    }

    use super::*;

    #[test]
    fn initializes_the_builtin_room_once() {
        let unique = format!(
            "acumen-oai-room-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&dir).unwrap();

        let manager = Manager::new(dir.clone());
        let serialized = std::fs::read_to_string(&manager.path).unwrap();
        let config: Config = serde_json::from_str(&serialized).unwrap();
        let room = config
            .agents
            .iter()
            .find(|agent| agent.name == BUILTIN_ROOM)
            .unwrap();
        assert_eq!(room.description, "终端与联网工具助手");
        assert_eq!(room.model, DEFAULT_MODEL);
        assert!(room.public_history.is_empty());
        assert!(room.uses_agent());
        assert_eq!(config.default_model, DEFAULT_MODEL);
        assert_eq!(config.defaults_version, CURRENT_DEFAULTS_VERSION);
        assert!(
            config
                .seeded_presets
                .iter()
                .any(|name| name == BUILTIN_ROOM),
            "内置房间与其他内置房间共用「建过就记名」的规矩"
        );

        // 删掉之后不再复活：第二次启动只认记下的名字。
        let mut pruned: Config =
            serde_json::from_str(&std::fs::read_to_string(&manager.path).unwrap()).unwrap();
        pruned.agents.retain(|agent| agent.name != BUILTIN_ROOM);
        std::fs::write(
            &manager.path,
            serde_json::to_string_pretty(&pruned).unwrap(),
        )
        .unwrap();
        let _ = Manager::new(dir.clone());
        let again: Config =
            serde_json::from_str(&std::fs::read_to_string(&manager.path).unwrap()).unwrap();
        assert!(
            !again.agents.iter().any(|agent| agent.name == BUILTIN_ROOM),
            "管理员删过的内置房间不该被补建回来"
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    /// 引擎关键字从 `pi` 改成 `agent` 之后，老配置里的写值要跟着换过来，
    /// 否则那间房会静默掉回中转站房间（引擎认不出，功能退化且没有报错）。
    #[test]
    fn the_old_engine_keyword_is_rewritten_to_the_new_one() {
        let unique = format!(
            "acumen-oai-engine-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let mut legacy = Config {
            api_base: "https://api.deepseek.com/v1".into(),
            api_key: "sk-test".into(),
            defaults_version: CURRENT_DEFAULTS_VERSION,
            seeded_presets: vec![BUILTIN_ROOM.to_string()],
            ..Default::default()
        };
        let mut room =
            super::super::types::Agent::new("管家大人", "deepseek/deepseek-flash", "", "");
        room.engine = "pi".into();
        legacy.agents.push(room);
        std::fs::write(&path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();

        let _ = Manager::new(dir.clone());
        let config: Config =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let room = config.agents.iter().find(|a| a.name == "管家大人").unwrap();
        assert_eq!(room.engine, super::super::types::ENGINE_AGENT);
        assert!(room.uses_agent());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_model_suffix_is_folded_into_the_thinking_field_once() {
        let unique = format!(
            "acumen-oai-thinking-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        // 旧配置里思考强度写在模型串尾部。Config 字段没有 serde 默认值，
        // 直接写半截 JSON 会被判为损坏并回落到空配置，所以整份序列化出来。
        let mut legacy = Config {
            api_base: "https://api.deepseek.com/v1".into(),
            api_key: "sk-test".into(),
            defaults_version: CURRENT_DEFAULTS_VERSION,
            ..Default::default()
        };
        let mut room =
            super::super::types::Agent::new("助手", "deepseek/deepseek-flash:high", "", "");
        room.set_engine(
            super::super::types::ENGINE_CHAT,
            "deepseek/deepseek-flash:high",
        );
        legacy.agents.push(room);
        std::fs::write(&path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();

        let manager = Manager::new(dir.clone());
        let config: Config =
            serde_json::from_str(&std::fs::read_to_string(&manager.path).unwrap()).unwrap();
        let room = config.agents.iter().find(|a| a.name == "助手").unwrap();
        assert_eq!(room.model, "deepseek/deepseek-flash");
        assert_eq!(room.thinking, "high");

        // 再启动一次不会二次改动（幂等）。
        let _ = Manager::new(dir.clone());
        let again: Config = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let room = again.agents.iter().find(|a| a.name == "助手").unwrap();
        assert_eq!(room.model, "deepseek/deepseek-flash");
        assert_eq!(room.thinking, "high");

        std::fs::remove_dir_all(dir).unwrap();
    }
}
