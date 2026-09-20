//! 房间与群聊搭话共用的内置 agent 执行层。
//!
//! 这些房间曾经驱动本机安装的一个外部 CLI，模型、系统提示词与
//! 工具全在那边；代价是部署必须多装一套 Node 工具链，且工具集与提示词都不在自己
//! 手里。现在整条链路就在进程内：一轮对话 = 若干个 Chat Completions 请求，
//! 模型要工具就调用本模块里的实现，把结果作为 tool 消息回填，直到它不再要工具。
//!
//! 三块内容分工：
//! - [`tools`]：工具表（名字、说明、JSON Schema）与本地实现（bash / read / write /
//!   edit / glob / grep），以及转发进 [`ChatBridge`] 的 `satori_*`；
//! - [`run`]：消息组装、工具循环、轨迹整理、skill 索引；
//! - [`bash`]：子进程与进程组终止——取消一轮对话必须连带杀掉工具派生出来的进程。
//!
//! 与从前那套外部 CLI 最大的行为差别是**没有会话文件**：房间历史始终由 acumen 侧持有，
//! 每轮按需展开成消息，所以编辑/删除/清空/重新生成的行为与普通房间完全一致。

pub(crate) mod bash;
pub(crate) mod run;
pub(crate) mod tools;

pub(crate) use run::run;

use super::types::{ChatMessage, TraceStep};
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// 聊天界面出口：执行层只负责把 `satori_*` 工具转发出去，具体语义由接入方定义。
///
/// 目前唯一的接入方是群聊搭话（[`crate::plugins::ambient::bridge`]）。抽成 trait 是
/// 为了执行层不反过来依赖调用方：房间那边没有聊天界面时留 `None` 即可。
pub(crate) trait ChatBridge: Send + Sync {
    /// 工具调用：`op` + 参数，`call_id` 兼作回执去重键。
    fn call<'a>(&'a self, call_id: &'a str, op: &'a str, params: Value) -> BoxFuture<'a, Value>;
    /// 这一轮是否真的动用过聊天界面（决定最终回执是否覆盖文本输出）。
    fn used(&self) -> bool {
        false
    }
    /// 界面状态版本号，用于判断群聊是否已经往前走。
    fn revision(&self) -> u64 {
        0
    }
}

/// 解析房间的写法：`agent`、`agent 模型`、`agent/模型`、`agent:模型`（大小写与全角冒号皆可）。
///
/// 返回 `Some(模型)`——空串表示房间没指定模型，用 `[oai] agent_default_model`。
/// 不是这个写法时返回 `None`，调用方按中转站模型处理。
pub(crate) fn parse_agent_spec(spec: &str) -> Option<String> {
    /// 引擎关键字。写模型位时它代替模型名，说「这间房交给内置智能体」。
    const KEYWORD: &str = "agent";
    let spec = spec.trim();
    let tail = spec
        .get(..KEYWORD.len())
        .filter(|head| head.eq_ignore_ascii_case(KEYWORD))
        .map(|_| &spec[KEYWORD.len()..])?;
    let mut chars = tail.chars();
    match chars.next() {
        None => Some(String::new()),
        Some('/' | ':' | '：' | ' ' | '\t') => Some(chars.as_str().trim().to_string()),
        // `agency`、`agent2` 这类名字不是这个写法。
        Some(_) => None,
    }
}

/// 房间模型是否表示「用默认模型」：留空或写 `agent`。
///
/// 留空时由 `[oai] agent_default_model` 接手；写 `agent` 是同一件事的显式说法。
pub(crate) fn uses_default_model(model: &str) -> bool {
    let model = model.trim();
    model.is_empty() || model.eq_ignore_ascii_case("agent")
}

/// 每次调用独占目录，避免中文房间名、私有用户及临时请求之间共享文件。
pub(crate) struct ScratchDir(PathBuf);
impl ScratchDir {
    pub(crate) fn new(base: &Path) -> anyhow::Result<Self> {
        Self::under(base, "runs")
    }

    /// 在 `base/<folder>/` 下开一个只有本进程可读写的临时目录。
    pub(crate) fn under(base: &Path, folder: &str) -> anyhow::Result<Self> {
        let root = base.join(folder);
        std::fs::create_dir_all(&root)?;
        let path = root.join(format!("{:032x}", rand::random::<u128>()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path)?;
        Ok(Self(path))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 一次 agent 对话的全部参数。
///
/// 房间与群聊搭话共用同一个执行层：进程组终止、工具循环、事件整理都只该有一份
/// 实现，两个调用方的差别收敛成这里的字段。
pub(crate) struct AgentRun<'a> {
    /// 接口基址（已按供应商解析好）。
    pub api_base: &'a str,
    /// 该接口的密钥。
    pub api_key: &'a str,
    /// 一次调用独占的临时目录：skill 落在这里，bash 的默认工作目录也可是它。
    pub dir: &'a Path,
    /// bash 与文件工具的工作目录；`None` 表示沿用 bot 自身的工作目录。
    pub cwd: Option<&'a Path>,
    /// 整体替换系统提示词；群聊人格不需要通用助手那一套。
    pub system_prompt: Option<&'a str>,
    /// 追加在系统提示词之后的人设。
    pub append_system_prompt: &'a str,
    /// 要发给接口的模型 id（已剥掉 `供应商/` 前缀）。
    pub model: &'a str,
    /// 思考强度（off/minimal/low/…）。
    pub thinking: Option<&'a str>,
    /// 采样温度；`None` 交给接口默认值。群聊那边调高，判定与普通房间不动。
    pub temperature: Option<f64>,
    /// 额外载入的 skill 文件或目录。
    pub skills: &'a [PathBuf],
    /// 这一轮持有控制通道凭据：系统提示词里会多一句用法说明。
    pub control: bool,
    /// 工具白名单（逗号分隔）；`None` 用全部本地工具。
    pub tools: Option<&'a str>,
    /// 追加给工具子进程的环境变量，例如本轮对话的控制凭据。
    pub env: &'a [(String, String)],
    /// 单次模型请求的静默上限；`None` 表示不看（交给整轮预算）。
    pub stall: Option<std::time::Duration>,
    /// 有外部动作的会话不能在静默后重放整轮。
    pub retry_stalled: bool,
    /// 真实聊天界面的工具出口；`None` 表示这一轮不接聊天界面。
    pub bridge: Option<std::sync::Arc<dyn ChatBridge>>,
    /// 联网搜索出口；`None` 表示这一轮没有 `web_search` / `web_fetch`。
    pub web: Option<&'a super::search::Search>,
    /// 用户正文。
    pub prompt: &'a str,
    /// 随正文送入的图片地址。
    pub images: &'a [String],
    /// 最多几轮工具调用。
    pub max_steps: usize,
}

impl<'a> AgentRun<'a> {
    pub(crate) fn new() -> Self {
        Self {
            api_base: "",
            api_key: "",
            dir: Path::new("."),
            cwd: None,
            system_prompt: None,
            append_system_prompt: "",
            model: "",
            thinking: None,
            temperature: None,
            skills: &[],
            control: false,
            tools: None,
            env: &[],
            stall: None,
            retry_stalled: true,
            bridge: None,
            web: None,
            prompt: "",
            images: &[],
            max_steps: 24,
        }
    }
}

/// 一次 agent 对话的产出。
#[derive(Debug)]
pub(crate) struct AgentReply {
    pub text: String,
    /// 实际应答的模型（`供应商/模型`），用于回复卡片页脚。
    pub model: Option<String>,
    /// 工具调用轨迹。
    pub trace: Vec<TraceStep>,
    /// 超出保留上限、未进入 `trace` 的调用次数。
    pub trace_overflow: usize,
    /// 这一轮联网检索引用过的网页来源，渲染在回复卡片下方。
    pub sources: Vec<super::types::Source>,
}

/// 房间在群里说话时的那一份现场：接进能力层要用它开一轮。
///
/// 私聊里的房间没有这一份（没有群、也就没有群资料与群动作），拿到的仍是本机
/// 工具与联网。
pub(crate) struct ChatContext {
    pub ctx: crate::event::Context,
    pub writer: crate::adapters::satori::LockedWriter,
    pub group: i64,
    /// 这一轮允许做什么：额度、开关、管理群，见 [`crate::plugins::oai::chat::ChatConfig`]。
    pub config: crate::plugins::oai::chat::ChatConfig,
    /// 工具白名单；`None` 表示有什么挂什么。
    pub tools: Option<String>,
}

/// 房间对话：按历史展开消息，驱动一轮 agent。
///
/// 房间历史仍是唯一事实来源；中途的工具调用不写回历史，下一轮按历史重新展开。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn conversation(
    api_base: &str,
    api_key: &str,
    base: &Path,
    persona: &str,
    model: &str,
    thinking: Option<&str>,
    stall: Option<std::time::Duration>,
    hist: &[ChatMessage],
    control: Option<&crate::plugins::ctl::bridge::Lease>,
    search: &super::search::SearchConfig,
    chat: Option<ChatContext>,
) -> anyhow::Result<AgentReply> {
    let (current, previous) = hist
        .split_last()
        .filter(|(message, _)| message.role == "user")
        .ok_or_else(|| anyhow::anyhow!("没有可重新生成的用户消息，请先发送内容"))?;
    let dir = ScratchDir::new(base)?;
    // 房间在群里时把群聊能力层接上：工具表里那十个 `satori_*` 因此才存在。
    let (bridge, tools): (Option<std::sync::Arc<dyn ChatBridge>>, Option<&str>) = match &chat {
        Some(chat) => {
            let opened = crate::plugins::oai::chat::start(crate::plugins::oai::chat::ChatEnv {
                ctx: &chat.ctx,
                writer: &chat.writer,
                group: chat.group,
                config: chat.config.clone(),
                // 房间总是开着：用户问一句就答一句；动手也不必等群聊停下来。
                enabled: true,
                require_fresh: false,
                scratch: dir.path(),
                // 房间里生成的东西放在本轮工作目录里，随后就发出去；一轮结束即清理。
                media: dir.path(),
                persona: None,
                scene: crate::plugins::oai::chat::session::Scene::Channel,
            })
            .await?;
            (
                Some(std::sync::Arc::new(opened)),
                chat.tools.as_deref(),
            )
        }
        None => (None, None),
    };
    let env = control.map(|lease| lease.env()).unwrap_or_default();
    let skills: Vec<PathBuf> = control
        .map(|lease| vec![lease.skill().to_path_buf()])
        .unwrap_or_default();
    // 搜索状态一轮一份：预算、客户端与引用来源都随这一轮生灭。
    let web = search
        .enabled
        .then(|| super::search::Search::new(search.clone()));
    // 房间的工作目录沿用 bot 自身：与从前一致，bash 与文件工具都在这里活动。
    let cwd = std::env::current_dir().ok();
    run::run_with_history(
        AgentRun {
            api_base,
            api_key,
            dir: dir.path(),
            cwd: cwd.as_deref(),
            append_system_prompt: persona,
            model,
            thinking: thinking.filter(|value| !value.trim().is_empty()),
            skills: &skills,
            control: control.is_some(),
            tools,
            bridge,
            env: &env,
            stall,
            web: web.as_ref(),
            prompt: &current.content,
            images: &current.images,
            ..AgentRun::new()
        },
        previous,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_spec_accepts_every_separator_and_rejects_lookalike_names() {
        assert_eq!(parse_agent_spec("agent").as_deref(), Some(""));
        assert_eq!(parse_agent_spec(" AGENT ").as_deref(), Some(""));
        for spec in [
            "agent apilio/claude-opus-5",
            "agent/apilio/claude-opus-5",
            "agent:apilio/claude-opus-5",
            "AGENT：apilio/claude-opus-5",
        ] {
            assert_eq!(
                parse_agent_spec(spec).as_deref(),
                Some("apilio/claude-opus-5"),
                "{spec}"
            );
        }
        // 中转站模型名不能被误当成这种写法。
        for spec in ["", "agents", "agency", "gpt-5.6-luna", "agent-1", "皮"] {
            assert_eq!(parse_agent_spec(spec), None, "{spec}");
        }
        // 空模型等于「用默认模型」。
        assert!(uses_default_model(&parse_agent_spec("agent").unwrap()));
        assert!(!uses_default_model(&parse_agent_spec("agent kimi-k3").unwrap()));
    }

    #[test]
    fn request_directories_are_unique_and_cleaned() {
        let base = std::env::temp_dir();
        let a = ScratchDir::new(&base).unwrap();
        let b = ScratchDir::new(&base).unwrap();
        assert_ne!(a.path(), b.path());
        let path = a.path().to_path_buf();
        drop(a);
        assert!(!path.exists());
    }
}
