use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub images: Vec<String>,
    #[serde(default)]
    pub timestamp: i64,
    /// 这条回复之前用过哪些工具（给下一轮当线索，用户看不到）。没用工具的回复没有。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub work: String,
    /// 这句话是谁说的（`号主` 或 `名字（QQ 号）·客人`）。只有**公共历史**里的用户消息才有：
    /// 一间房的公共历史是大家共用的，下一轮展开时不标出说话人，模型就会把别人说过的话算到眼前这个人头上。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from: String,
}

impl ChatMessage {
    pub fn new(role: &str, content: &str, images: Vec<String>) -> Self {
        Self {
            role: role.to_string(),
            content: content.to_string(),
            images,
            timestamp: chrono::Local::now().timestamp(),
            work: String::new(),
            from: String::new(),
        }
    }

    pub(crate) fn with_from(mut self, from: &str) -> Self {
        self.from = from.to_string();
        self
    }

    pub(crate) fn with_work(mut self, work: String) -> Self {
        self.work = work;
        self
    }
}

/// 房间手边有哪些工具。
///
/// 所有房间都是同一个智能体，差别只在这里：能力按需打开，而不是建房时选一种「引擎」。
/// 新建的房间是 [`Tools::Off`]——一间只聊天的房间不该先背上一整套终端和群聊工具的提示词，
/// 也不该让客人顺手就能叫它跑命令；要查资料开 [`Tools::Web`]，要办事（终端、文件、群聊动作、
/// 提醒、插件管控）开 [`Tools::Full`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tools {
    /// 不挂任何工具：一问一答。
    #[default]
    Off,
    /// 只挂联网搜索与网页抓取。
    Web,
    /// 全部工具：终端与文件、联网、群聊工具、办事台。
    Full,
}

impl Tools {
    /// 三档按从轻到重排好，状态卡与帮助里按这个顺序列。
    pub const ALL: [Tools; 3] = [Tools::Off, Tools::Web, Tools::Full];

    /// 给人看的名字；也是切换指令里写的那个词（`房间?联网`）。
    pub fn label(self) -> &'static str {
        match self {
            Tools::Off => "无工具",
            Tools::Web => "联网",
            Tools::Full => "全量",
        }
    }

    /// 一句话说明这一档能做什么。
    pub fn summary(self) -> &'static str {
        match self {
            Tools::Off => "只聊天，不调任何工具",
            Tools::Web => "按需联网搜索、读网页",
            Tools::Full => "终端与文件、联网、群聊动作、提醒与插件管控",
        }
    }

    /// 这一档有没有联网工具。
    pub fn web(self) -> bool {
        matches!(self, Tools::Web | Tools::Full)
    }

    /// 切换指令里的那个词 → 档位；认不出来返回 `None`。
    pub fn parse(word: &str) -> Option<Tools> {
        let word = super::utils::normalize(word.trim()).trim().to_ascii_lowercase();
        Some(match word.as_str() {
            "无工具" | "无" | "关" | "关闭" | "不联网" | "聊天" | "纯聊天" | "off" | "none" | "chat" | "0" => Tools::Off,
            "联网" | "搜索" | "开" | "打开" | "开联网" | "web" | "search" | "on" | "1" => Tools::Web,
            "全量" | "全部" | "全能" | "完整" | "满血" | "full" | "all" | "agent" | "2" => Tools::Full,
            _ => return None,
        })
    }
}

/// 合法的思考强度档位，与 Pi 的 `--thinking` 取值一致（请求里转成 `reasoning_effort`）。
pub const THINKING_LEVELS: [&str; 5] = ["off", "minimal", "low", "medium", "high"];

/// 归一化思考强度：只认内置档位，大小写与首尾空白不敏感；其余返回 `None`。
pub fn normalize_thinking(value: &str) -> Option<String> {
    let level = value.trim().to_ascii_lowercase();
    THINKING_LEVELS.contains(&level.as_str()).then_some(level)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// 房间在 `/#` 列表里归到哪个分区；留空则按模型分组（一直以来的样子）。
    #[serde(default)]
    pub section: String,
    /// 这间房手边有哪些工具，用 `房间?` 系指令切换。
    #[serde(default)]
    pub tools: Tools,
    /// 房间的模型：`供应商/模型` 或裸模型 id（`供应商/` 决定打到哪个接口，见 `[oai.providers]`），
    /// 留空表示跟随 `[oai] agent_default_model`。画图、写歌、拍片这类模型走各自的专用接口，
    /// 工具档位对它们不起作用。
    pub model: String,
    /// 房间默认思考强度（`off` / `minimal` / `low` / `medium` / `high`）。
    /// 留空表示交给引擎默认；模型写法里的 `:强度` 后缀优先于这里。
    #[serde(default)]
    pub thinking: String,
    /// 1.22.0 之前的房间类型（`agent` / `chat`），只在升级时读一次，换算成 [`Self::tools`]。
    #[serde(default, skip_serializing)]
    pub engine: String,
    pub system_prompt: String,
    #[serde(default)]
    pub public_history: Vec<ChatMessage>,
    #[serde(default)]
    pub private_histories: HashMap<String, Vec<ChatMessage>>,
    #[serde(default)]
    pub generation_id: u64,
    #[serde(default)]
    pub created_at: i64,
}

impl Agent {
    pub fn new(name: &str, model: &str, prompt: &str, desc: &str) -> Self {
        Self {
            name: name.to_string(),
            description: desc.to_string(),
            section: String::new(),
            tools: Tools::Off,
            engine: String::new(),
            model: model.to_string(),
            thinking: String::new(),
            system_prompt: prompt.to_string(),
            public_history: Vec::new(),
            private_histories: HashMap::new(),
            generation_id: 0,
            created_at: chrono::Local::now().timestamp(),
        }
    }

    /// 请求时真正生效的思考强度：模型写法里的 `:强度` 后缀优先，其次房间字段。
    ///
    /// 换模型时会把后缀收进 `thinking`，所以两者通常一致；保留后缀优先是因为
    /// 手改过的模型串可能还带着后缀，不该被静默忽略。
    pub fn effective_thinking(&self) -> Option<String> {
        super::utils::split_thinking(&self.model)
            .1
            .or_else(|| normalize_thinking(&self.thinking))
    }

    pub fn history_mut(&mut self, private: bool, uid: &str) -> &mut Vec<ChatMessage> {
        if private {
            self.private_histories.entry(uid.to_string()).or_default()
        } else {
            &mut self.public_history
        }
    }

    pub fn history(&self, private: bool, uid: &str) -> &[ChatMessage] {
        if private {
            self.private_histories
                .get(uid)
                .map(Vec::as_slice)
                .unwrap_or(&[])
        } else {
            &self.public_history
        }
    }

    pub fn clear_history(&mut self, private: bool, uid: &str) {
        if private {
            if let Some(h) = self.private_histories.get_mut(uid) {
                h.clear();
            }
        } else {
            self.public_history.clear();
        }
    }

    pub fn delete_at(&mut self, private: bool, uid: &str, indices: &[usize]) -> Vec<usize> {
        let h = self.history_mut(private, uid);
        let mut deleted = Vec::new();
        let mut sorted: Vec<usize> = indices.to_vec();
        sorted.sort_by(|a, b| b.cmp(a));
        sorted.dedup();
        for i in sorted {
            if i > 0 && i <= h.len() {
                h.remove(i - 1);
                deleted.push(i);
            }
        }
        deleted.reverse();
        deleted
    }

    pub fn edit_at(&mut self, private: bool, uid: &str, idx: usize, content: &str) -> bool {
        let h = self.history_mut(private, uid);
        if idx > 0 && idx <= h.len() {
            h[idx - 1].content = content.to_string();
            true
        } else {
            false
        }
    }
}

/// `data/oai/config.json` 的形状：房间、模型列表与默认接口。
///
/// 这一份不是 `config.toml`——它由 `/ctl`、`##` 系符号指令与启动时的内置房间补建共同维护，
/// 用户不直接手改。接口与密钥的权威来源仍是 `[oai]` 那一段，这里只存运行时选定的值。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// 默认接口的 OpenAI 兼容基址。
    pub api_base: String,
    /// 默认接口的密钥。
    pub api_key: String,
    /// 默认接口（中转站）拉回的模型列表，供 `/%` 展示。
    #[serde(default)]
    pub models: Vec<String>,
    /// 各家供应商自己的模型列表（键是 `[oai.providers]` 里的名字，值是裸模型 id）。
    /// `/%` 接在默认接口那份后面列出、序号接着编；`房间%模型` 写裸名字时也在这里找。
    #[serde(default)]
    pub provider_models: std::collections::BTreeMap<String, Vec<String>>,
    /// 全部房间（智能体）。
    #[serde(default)]
    pub agents: Vec<Agent>,
    /// 没写供应商前缀时用的模型名。
    #[serde(default)]
    pub default_model: String,
    /// 内置默认值迁移版本；避免每次启动覆盖管理员后续的模型选择。
    #[serde(default)]
    pub defaults_version: u32,
    /// 已经建过的内置房间名（目前只有管家大人）。
    ///
    /// 记的是「建过」而不是「存在」：管理员删掉哪间就是不要哪间，下次启动不复活。
    #[serde(default)]
    pub seeded_presets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MjMessageTask {
    pub task_id: String,
    /// 任务实际使用的接入点（`mj-fast` / `mj-relax`），旧缓存缺省时走当前默认值。
    #[serde(default)]
    pub api_base: String,
    #[serde(default)]
    pub upscale_buttons: HashMap<u8, String>,
    #[serde(default)]
    pub created_at: i64,
}

/// MJ 的引用关系和放大结果独立于聊天记录持久化。
/// `upscales` 的值优先是本地缓存文件，下载失败时回退为远程图片 URL。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MjCache {
    #[serde(default)]
    pub messages: HashMap<String, MjMessageTask>,
    #[serde(default)]
    pub upscales: HashMap<String, String>,
}

#[derive(Debug, Default)]
pub struct GeneratingState {
    pub public: HashMap<String, u64>,
    pub private: HashMap<String, HashMap<String, u64>>,
    /// 这一轮里已经被告知过「还在生成」的人（轮次编号 + 账号）。同一个人连着催，
    /// 只回第一次：后面几句再回一遍同样的话，就是在群里刷屏。
    told: std::collections::HashSet<(u64, String)>,
}

impl GeneratingState {
    pub fn is_generating(&self, agent: &str, private: bool, uid: &str) -> bool {
        self.current(agent, private, uid).is_some()
    }
    fn current(&self, agent: &str, private: bool, uid: &str) -> Option<u64> {
        if private {
            self.private.get(agent)?.get(uid).copied()
        } else {
            self.public.get(agent).copied()
        }
    }
    pub fn is_current(&self, agent: &str, private: bool, uid: &str, id: u64) -> bool {
        self.current(agent, private, uid) == Some(id)
    }
    /// 这一轮里第一次告诉 `who`「还在生成」时为真；之后同一轮再问都是假。
    pub fn first_notice(&mut self, agent: &str, private: bool, uid: &str, who: &str) -> bool {
        match self.current(agent, private, uid) {
            Some(id) => self.told.insert((id, who.to_string())),
            None => true,
        }
    }
    /// 在同一个写锁内占用会话，避免两个请求同时通过空闲检查。
    pub fn begin(&mut self, agent: &str, private: bool, uid: &str) -> Option<u64> {
        if self.is_generating(agent, private, uid) {
            return None;
        }
        // 顺手把已经结束的轮次的告知记录清掉。
        let live: std::collections::HashSet<u64> = self
            .public
            .values()
            .chain(self.private.values().flat_map(HashMap::values))
            .copied()
            .collect();
        self.told.retain(|(id, _)| live.contains(id));
        let id = rand::random();
        if private {
            self.private
                .entry(agent.to_string())
                .or_default()
                .insert(uid.to_string(), id);
        } else {
            self.public.insert(agent.to_string(), id);
        }
        Some(id)
    }
    pub fn cancel_room(&mut self, agent: &str) {
        self.public.remove(agent);
        self.private.remove(agent);
    }
    pub fn set_generating(&mut self, agent: &str, private: bool, uid: &str, generating: bool) {
        if generating {
            self.begin(agent, private, uid);
        } else if private {
            if let Some(users) = self.private.get_mut(agent) {
                users.remove(uid);
            }
        } else {
            self.public.remove(agent);
        }
    }
}

/// 正文引用到的网页来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Source {
    pub title: String,
    pub url: String,
}

/// 工具调用轨迹里的一步。
///
/// 页脚按结构渲染而不是拼成一行长文本：一行文本迟早要被字数截断，
/// 而工具参数（命令、搜索词、URL）恰恰是尾巴最有信息量。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TraceStep {
    /// 工具名，例如 `bash`。
    pub name: String,
    /// 参数摘要；可能为空。
    pub detail: String,
    /// 连续同名同参调用的合并次数，至少为 1。
    pub repeats: u32,
}

impl TraceStep {
    pub(crate) fn new(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            detail: detail.into(),
            repeats: 1,
        }
    }
}
