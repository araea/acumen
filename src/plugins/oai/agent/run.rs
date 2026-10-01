//! 一轮 agent 对话：把历史展开成消息，循环请求，直到模型不再要工具。
//!
//! 循环的形状很朴素——请求、执行工具、把结果回填、再请求——值得写下来的只有
//! 三件事：
//!
//! 1. **模型的每一份回复都原样回填**。工具调用、思考块、签名都留在消息里，
//!    重放时上游才认得出自己上一轮说了什么；只留正文会让多轮工具调用散架。
//! 2. **轨迹只服务于页脚**。`tool_execution_start` 那套事件流没有了，改由这里
//!    在调用工具时记一笔，同名同参的连续调用合并成一条带次数的记录。
//! 3. **取消必须是干净的**。整轮可能被外层超时或「停止」指令 drop，所以工具执行
//!    直接挂在这个 future 上（见 [`super::bash`]），绝不 `tokio::spawn`。

use super::super::types::TraceStep;
use super::super::llm;
use super::AgentRun;
use rig_core::client::CompletionClient;
use rig_core::completion::message::{
    DocumentSourceKind, Image, Text, ToolResult, ToolResultContent, UserContent,
};
use rig_core::completion::{AssistantContent, CompletionModel, Message};

/// 页脚保留的工具调用条数上限；再多只记次数。
const TRACE_LIMIT: usize = 12;

/// 房间系统提示词里与运行环境有关的那一段。
///
/// 只写「这是哪儿、手边有什么」——人格与风格由房间提示词负责，工具细节由每个工具的
/// description 负责，这里再抄一遍就会开始过期。
const ROOM_BASE: &str = "\
你是运行在本机的中文助手，可以用工具读写文件、执行命令。
工作目录：{cwd}
回复要简洁，但关键依据不能省：引用了哪个文件、跑了哪条命令、拿到什么结果，都要说清楚。
工具报告的错误就是结果的一部分，照着它换个做法，别把它当成需要解释的现象。
工作目录里的路径直接写相对路径即可。";

/// 控制通道的用法说明；只有这一轮真的持有时才写进提示词。
const CONTROL_HINT: &str = "\
这一轮你还能直接操作机器人自己：用 bash 执行 `\"$ACUMEN_CTL_BIN\" --ctl \"<命令>\"`，
可以查看和修改插件开关与配置。具体用法见下面列的 skill，命令的回执会原样打回来。";

/// 群聊那一套工具的用法说明；只有这一轮真的接通了群聊界面（房间正开在群里）才写。
///
/// 每个工具都有自己的 description，这里只交代它们描述不了的三件事：动手之前先看现场、
/// 回执才算数、以及**回复本身就是答案**——它会发进这个群，别用 `satori_action` 再发一遍。
const CHAT_HINT: &str = "\
这一轮你在一个 QQ 群里，手边有一整套群聊工具（各自能做什么见每件工具的说明）。
动手之前先看一眼 satori_context：它给出现在可用的动作、精确消息 ID 与各项剩余额度。
回执才算数，失败就按错误换个做法。
你的回复本身会发进这个群，它就是你对用户那句话的回答；只有确实要单独发一条
（发媒体、引用某条消息、@某人）时才用 satori_action 的 send，别把同一句话发两遍。";

/// 联网搜索的用法说明；只有这一轮真挂了出网工具时才写进提示词。
///
/// 工具本身各有 description，这里只交代那件工具描述不了的事：什么时候该伸手去搜
/// （模型常常凭记忆直接答，答的还是过期信息），以及**不必每轮都搜**——它是按需
/// 触发的旁路，不是每轮的前置步骤，日常闲聊与已经确定的事照旧直接答。
const SEARCH_HINT: &str = "\
这一轮你能联网，但不必每轮都搜：只在回答依赖你不知道、或可能已经变了的事实时才用。
web_search 查训练知识之外的最新信息（赛程战况、版本、新闻、近况），
web_fetch 读某个网址的正文。日常闲聊、能算能推的、你本来就知道的，直接答；
真去查了就把来源链接带上，查不到就说查不到。";

/// 跑一轮对话。
pub(crate) async fn run(run: AgentRun<'_>) -> anyhow::Result<super::AgentReply> {
    run_with_history(run, &[]).await
}

/// 跑一轮对话，`history` 是这轮之前已经发生过的往来。
///
/// 卡死且一个工具都还没动过时自动重来一次：上游抽风是常态，静静等满整轮预算太亏。
/// 动过工具就不能重放——同一份副作用做两遍比慢一点糟糕得多。
pub(crate) async fn run_with_history(
    run: AgentRun<'_>,
    history: &[super::super::types::ChatMessage],
) -> anyhow::Result<super::AgentReply> {
    match attempt(&run, history).await {
        Err(error) if error.stalled && run.retry_stalled && !error.used_tools => {
            warn!(target: "Plugin/OAI", "{}，重试一次", error.message);
            attempt(&run, history).await.map_err(|error| error.message)
        }
        Err(error) => Err(error.message),
        Ok(reply) => Ok(reply),
    }
}

/// 一次失败：区分「卡死」与「真的错了」，前者才值得重试。
struct Failure {
    message: anyhow::Error,
    stalled: bool,
    used_tools: bool,
}

async fn attempt(
    run: &AgentRun<'_>,
    history: &[super::super::types::ChatMessage],
) -> Result<super::AgentReply, Failure> {
    let context = Context::prepare(run, history).await?;
    let definitions = super::tools::definitions(run.tools, run.bridge.is_some(), run.web.is_some());
    let client = llm::client(run.api_base, run.api_key).map_err(|message| Failure {
        message,
        stalled: false,
        used_tools: false,
    })?;
    let model = client.completion_model(run.model);

    let mut messages = context.messages;
    let mut trace = Trace::default();
    let mut used_tools = false;

    for _ in 0..run.max_steps.max(1) {
        let request = llm::request(
            messages.clone(),
            definitions.clone(),
            run.thinking,
            run.temperature,
        );
        let response = match run.stall {
            Some(limit) => match tokio::time::timeout(limit, model.completion(request)).await {
                Ok(response) => response,
                Err(_) => {
                    return Err(Failure {
                        message: anyhow::anyhow!(
                            "模型请求静默超过 {} 秒无响应",
                            limit.as_secs()
                        ),
                        stalled: true,
                        used_tools,
                    });
                }
            },
            None => model.completion(request).await,
        }
        .map_err(|error| Failure {
            message: anyhow::anyhow!("{error}"),
            stalled: false,
            used_tools,
        })?;

        let calls: Vec<_> = response
            .choice
            .iter()
            .filter_map(|part| match part {
                AssistantContent::ToolCall(call) => Some(call.clone()),
                _ => None,
            })
            .collect();
        let text = llm::text_of(&response.choice);

        if calls.is_empty() {
            if text.trim().is_empty() {
                return Err(Failure {
                    message: anyhow::anyhow!("模型未返回最终回复"),
                    stalled: false,
                    used_tools,
                });
            }
            return Ok(super::AgentReply {
                text,
                // 实际应答的模型名由调用方补全（它才知道 `供应商/` 前缀）。
                model: Some(run.model.to_string()),
                trace: trace.steps,
                trace_overflow: trace.overflow,
                sources: run.web.map(|web| web.sources()).unwrap_or_default(),
            });
        }

        // 原样回填模型的这一份回复：工具调用、思考块与签名都要跟着走。
        messages.push(Message::Assistant {
            id: response.message_id.clone(),
            content: response.choice.clone(),
        });

        let mut results = Vec::with_capacity(calls.len());
        for call in &calls {
            let name = call.function.name.clone();
            trace.push(&name, super::tools::label(&call.function.arguments));
            if super::tools::is_side_effecting(&name) {
                used_tools = true;
            }
            let output = super::tools::execute(&name, &call.function.arguments, run, call.id.as_str())
                .await;
            results.push(UserContent::ToolResult(ToolResult {
                call: call.id.clone(),
                provider: call.provider.clone(),
                name,
                content: vec![ToolResultContent::text(output)],
            }));
        }
        messages.push(Message::User { content: results });
    }

    Err(Failure {
        message: anyhow::anyhow!("工具调用超过 {} 步仍未收尾", run.max_steps.max(1)),
        stalled: false,
        used_tools,
    })
}

/// 这一轮要发的开头消息。
struct Context {
    messages: Vec<Message>,
}

impl Context {
    async fn prepare(
        run: &AgentRun<'_>,
        history: &[super::super::types::ChatMessage],
    ) -> Result<Self, Failure> {
        let bad = |message: anyhow::Error| Failure {
            message,
            stalled: false,
            used_tools: false,
        };

        let index = skills(run).map_err(bad)?;
        let cwd = run.cwd.unwrap_or(run.dir);
        let mut system = match run.system_prompt {
            Some(explicit) => explicit.trim().to_string(),
            None => {
                let mut base = ROOM_BASE.replace("{cwd}", &cwd.display().to_string());
                if run.control {
                    base.push_str("\n\n");
                    base.push_str(CONTROL_HINT);
                }
                if run.bridge.is_some() {
                    base.push_str("\n\n");
                    base.push_str(CHAT_HINT);
                }
                if run.web.is_some() {
                    base.push_str("\n\n");
                    base.push_str(SEARCH_HINT);
                }
                match run.append_system_prompt.trim() {
                    "" => base,
                    persona => format!("{base}\n\n---\n\n{persona}"),
                }
            }
        };
        if !index.is_empty() {
            system.push_str("\n\n---\n\n");
            system.push_str(&index);
        }

        let mut messages = Vec::new();
        if !system.trim().is_empty() {
            messages.push(Message::System { content: system });
        }
        for message in history {
            match message.role.as_str() {
                "user" => {
                    // 历史里的图片与这一条一样处理：先转成模型收得下的 data URL。
                    let content = user_content(&message.content, &message.images).await;
                    if !content.is_empty() {
                        messages.push(Message::User { content });
                    }
                }
                "assistant" => {
                    let clean = clean_history(&message.content);
                    if !clean.trim().is_empty() {
                        messages.push(Message::Assistant {
                            id: None,
                            content: vec![AssistantContent::Text(Text::new(clean))],
                        });
                    }
                }
                _ => {}
            }
        }

        let content = user_content(run.prompt, run.images).await;
        let content = if content.is_empty() {
            // 只发了图片、或者引用加空正文：给模型一句能落地的话，别让请求空着。
            vec![UserContent::Text(Text::new("请看图片。"))]
        } else {
            content
        };
        messages.push(Message::User { content });

        Ok(Self { messages })
    }
}

/// 正文 + 图片 → 一条用户消息的内容块。
async fn user_content(text: &str, images: &[String]) -> Vec<UserContent> {
    let mut content = Vec::new();
    if !text.trim().is_empty() {
        content.push(UserContent::Text(Text::new(text)));
    }
    for url in images {
        let data_url = super::super::logic::to_data_url(url).await;
        content.push(UserContent::Image(Image {
            data: DocumentSourceKind::Url(data_url),
            media_type: None,
            detail: None,
            additional_params: None,
        }));
    }
    content
}

/// 历史里内嵌的 base64 图片重放只会撑爆上下文，留个占位即可。
fn clean_history(content: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"!\[.*?\]\((data:image/[^\s\)]+)\)").unwrap());
    re.replace_all(content, "[Image Created]").to_string()
}

/// 把 skill 铺进这一轮的目录，并返回给模型看的索引。
///
/// 外部 CLI 时代的 `--skill` 是渐进披露：提示词里只有一行描述，正文要模型自己去读。
/// 这里照搬同一套：目录复制进 run dir，索引写清路径，模型用 `read` 打开。
fn skills(run: &AgentRun<'_>) -> anyhow::Result<String> {
    let mut lines = Vec::new();
    for source in run.skills {
        let (Some(name), Some(body)) = (
            source.file_name().and_then(|name| name.to_str()),
            read_skill(source),
        ) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let dir = run.dir.join("skills").join(name);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("SKILL.md"), &body)?;
        let description = frontmatter(&body, "description").unwrap_or_else(|| name.to_string());
        lines.push(format!(
            "- {name}：{description}\n  正文：skills/{name}/SKILL.md（需要时用 read 读一遍）"
        ));
    }
    if lines.is_empty() {
        return Ok(String::new());
    }
    Ok(format!(
        "以下是这一轮随身的说明文档，用到哪份读哪份：\n{}",
        lines.join("\n")
    ))
}

/// skill 可以给目录（读其中的 SKILL.md），也可以直接给文件。
fn read_skill(source: &std::path::Path) -> Option<String> {
    let file = if source.is_dir() {
        source.join("SKILL.md")
    } else {
        source.to_path_buf()
    };
    std::fs::read_to_string(file).ok()
}

/// 取 SKILL.md 头部 frontmatter 里的一个字段。
fn frontmatter(body: &str, key: &str) -> Option<String> {
    let rest = body.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    for line in rest[..end].lines() {
        if let Some(value) = line.trim().strip_prefix(&format!("{key}:")) {
            let value = value.trim().trim_matches(['"', '\'']).trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// 页脚的工具轨迹：同名同参的连续调用合并，超出上限的只计数。
#[derive(Default)]
struct Trace {
    steps: Vec<TraceStep>,
    overflow: usize,
}
impl Trace {
    fn push(&mut self, name: &str, detail: String) {
        // 同一个工具连着用同样的参数（重试、分页）在页脚里排成一列毫无信息量，
        // 合并成一条带次数的记录。
        if let Some(last) = self.steps.last_mut()
            && last.name == name
            && last.detail == detail
        {
            last.repeats += 1;
        } else if self.steps.len() < TRACE_LIMIT {
            self.steps.push(TraceStep::new(name, detail));
        } else {
            self.overflow += 1;
        }
    }
}
