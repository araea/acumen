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
use super::{AgentRun, compact, figures};
use rig_core::client::CompletionClient;
use rig_core::completion::message::{
    DocumentSourceKind, EMPTY_RESPONSE_ERROR, Image, Text, ToolCall, ToolChoice, ToolResult, ToolResultContent,
    UserContent,
};
use rig_core::completion::{AssistantContent, CompletionModel, Message};
use std::collections::HashMap;

use crate::plugins::oai::LOG_TARGET;

/// 一轮里 `view_image` 最多看几张：每张图都是下一次请求里实打实的输入。
const MAX_VIEWS: usize = 6;

/// 页脚保留的工具调用条数上限；再多只记次数。
const TRACE_LIMIT: usize = 12;

/// 同一个工具、同样的参数最多执行几次；再来就直接驳回。
///
/// 模型卡进「同一条命令一遍遍重试」是最常见的空转：结果不会因为再跑一次而变，
/// 步数与时间却实打实地烧掉。驳回时把话说明白，逼它换个做法或直接作答。
const MAX_SAME_CALL: usize = 4;

/// 房间系统提示词里与运行环境有关的那一段。
///
/// 只写「这是哪儿、手边有什么」——人格与风格由房间提示词负责，工具细节由每个工具的
/// description 负责，这里再抄一遍就会开始过期。
const ROOM_BASE: &str = "\
你是运行在本机的中文助手，可以用工具读写文件、执行命令。
工作目录：{cwd}
今天是 {today}；要知道此刻几点，用 bash 跑 date。
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

/// 回复排成卡片图时的写法说明；只有这一轮的回复真的会渲染成卡片（[`AgentRun::figures`]）才写。
///
/// 模型默认不知道自己的话会被排成图，更不知道能往里嵌图——不说它就只会贴一个链接。
/// 路径里不许有空格是 Markdown 的限制：`![x](a b.png)` 根本不会被解析成图片。
const CARD_HINT: &str = "\
你的回复会排成一张图片卡发出：标题、列表、表格、代码块都排得好看，不必写成纯文本的样子。
要让用户看见一张图，在回复里单独占一行写 `![一句说明](地址)`，它会嵌在这个位置，说明文字作图注。
地址可以是公网图片链接，也可以是你刚用工具生成、保存在本机的图片文件路径（绝对路径，
或相对工作目录；支持 PNG / JPEG / GIF / WebP / SVG，路径里不要有空格）。
一条回复最多嵌 6 张；写之前先确认文件真的存在。图只放用户要看的东西，别拿来装饰。
生成图表、截图这类中间产物写进临时目录 {scratch}，本轮结束会自动清理，别弄脏工作目录。";

/// 手边有 `view_image` 时再补的一句：画完先自己看一眼。
const VIEW_HINT: &str = "画好图之后可以先用 view_image 自己看一眼，确认没画坏（字没挤在一起、数据没错）再写进回复。";

/// 「今天」的写法：模型不知道日期，问「最近」「今天」的事时只能按训练时的记忆猜。
///
/// 只给到日、不给钟点：系统提示词是请求的开头，开头一变整段前缀缓存就作废，
/// 按分钟变等于每轮都白付一遍；要几点让它自己跑 `date`。
pub(crate) fn today() -> String {
    use chrono::Datelike;
    let now = crate::clock::beijing_now();
    let weekday = ["一", "二", "三", "四", "五", "六", "日"]
        [now.weekday().num_days_from_monday() as usize];
    format!("{}年{}月{}日 星期{weekday}（北京时间）", now.year(), now.month(), now.day())
}

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
            warn!(target: LOG_TARGET, "{}，重试一次", error.message);
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
    // 图没取到时只提醒模型改一次：再不行就让占位说话，别在这上面来回磨。
    let mut corrected = false;
    // 空回复也只催一次。
    let mut nudged = false;
    let steps = run.max_steps.max(1);
    // 本轮已经看过几张图（`view_image`），以及这一批工具回执之后要附上的图。
    let mut viewed = 0;
    let mut seen: Vec<String> = Vec::new();
    let mut repeats = Repeats::default();

    for step in 0..steps {
        // 倒数第二次请求前打个招呼，最后一次请求不再给工具：把「用完步数就整轮报错」
        // 变成「用手上的东西收尾」，已经查到的、做完的不会一起作废。
        let last = run.rescue && steps >= 3 && step + 1 == steps;
        if run.rescue && steps >= 3 && step + 2 == steps {
            messages.push(user_note(RUNNING_OUT));
        } else if last {
            messages.push(user_note(LAST_REPLY));
        }
        let saved = compact::compact(&mut messages, compact::BUDGET);
        if saved > 0 {
            debug!(target: LOG_TARGET, "上下文偏长，压缩了较早的回执：省下 {saved} 字符");
        }
        let mut request = llm::request(
            messages.clone(),
            definitions.clone(),
            run.thinking,
            run.temperature,
        );
        if last && !definitions.is_empty() {
            request.tool_choice = Some(ToolChoice::None);
        }
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
        });
        let response = match response {
            Ok(response) => response,
            // 接口回了一条什么都没有的消息（连思考块都没有）：和「只想不说」同一种待遇，催一次。
            Err(failure)
                if run.rescue
                    && !nudged
                    && step + 1 < steps
                    && failure.message.to_string().contains(EMPTY_RESPONSE_ERROR) =>
            {
                nudged = true;
                messages.push(user_note(EMPTY_REPLY));
                continue;
            }
            Err(failure) => return Err(failure),
        };

        let calls: Vec<_> = response
            .choice
            .iter()
            .filter_map(|part| match part {
                AssistantContent::ToolCall(call) => Some(call.clone()),
                _ => None,
            })
            .collect();
        let text = llm::text_of(&response.choice);

        // 最后一次请求里模型仍坚持调工具：只要它同时写了话，就当这是最终回复。
        if calls.is_empty() || (last && !text.trim().is_empty()) {
            if text.trim().is_empty() {
                // 思考完了却什么都没说：催一次，而不是整轮报错。
                if run.rescue && !nudged && step + 1 < steps {
                    nudged = true;
                    messages.push(user_note(EMPTY_REPLY));
                    continue;
                }
                return Err(Failure {
                    message: anyhow::anyhow!("模型未返回最终回复"),
                    stalled: false,
                    used_tools,
                });
            }
            // 回复里的图在这里就取好：本轮的临时目录一会儿就被清掉，图多半放在那里。
            let figures = if run.figures {
                let roots = [run.cwd.unwrap_or(run.dir), run.dir];
                figures::gather(&text, &roots).await
            } else {
                figures::Figures::default()
            };
            if !figures.failures().is_empty() && !corrected && step + 1 < steps {
                corrected = true;
                messages.push(Message::Assistant {
                    id: response.message_id.clone(),
                    content: response.choice.clone(),
                });
                messages.push(user_note(&complaint(&figures)));
                continue;
            }
            return Ok(super::AgentReply {
                text,
                // 实际应答的模型名由调用方补全（它才知道 `供应商/` 前缀）。
                trace: trace.steps,
                trace_overflow: trace.overflow,
                sources: run.web.map(super::super::search::Search::sources).unwrap_or_default(),
                figures,
            });
        }

        // 原样回填模型的这一份回复：工具调用、思考块与签名都要跟着走。
        messages.push(Message::Assistant {
            id: response.message_id.clone(),
            content: response.choice.clone(),
        });

        for call in &calls {
            trace.push(&call.function.name, super::tools::label(&call.function.arguments));
            if super::tools::is_side_effecting(&call.function.name) {
                used_tools = true;
            }
        }
        let outputs = execute_batch(run, &calls, &mut repeats).await;
        let mut results = Vec::with_capacity(calls.len());
        for (call, mut output) in calls.iter().zip(outputs) {
            if !output.images.is_empty() {
                if viewed + output.images.len() > MAX_VIEWS {
                    output.images.clear();
                    output.text = format!("这一轮看的图已经够多了（最多 {MAX_VIEWS} 张），不再附图。");
                } else {
                    viewed += output.images.len();
                    seen.extend(std::mem::take(&mut output.images));
                }
            }
            results.push(UserContent::ToolResult(ToolResult {
                call: call.id.clone(),
                provider: call.provider.clone(),
                name: call.function.name.clone(),
                content: vec![ToolResultContent::text(output.text)],
            }));
        }
        messages.push(Message::User { content: results });
        // 工具读来的图附在这一批回执之后：tool 消息里放不了图，放在用户消息里模型照样看得见。
        if !seen.is_empty() {
            let mut content = vec![UserContent::Text(Text::new(compact::VIEW_MARK))];
            content.extend(seen.drain(..).map(|data_url| {
                UserContent::Image(Image {
                    data: DocumentSourceKind::Url(data_url),
                    media_type: None,
                    detail: None,
                    additional_params: None,
                })
            }));
            messages.push(Message::User { content });
        }
    }

    Err(Failure {
        message: anyhow::anyhow!("工具调用超过 {steps} 步仍未收尾"),
        stalled: false,
        used_tools,
    })
}

/// 步数将尽时的提醒（倒数第二次请求前）。只对模型可见。
const RUNNING_OUT: &str = "（系统提醒，用户看不到：工具调用的次数快用完了。接下来请把已经拿到的结果整理成最终回复；\
还没做完的部分如实说明做到了哪一步。）";

/// 最后一次请求前的说明：这一次不能再调工具。
const LAST_REPLY: &str = "（系统提醒，用户看不到：这是最后一次回复，不能再调用工具。直接用已有的结果给出最终回复，\
没做完的部分如实说明；回复里不要提这条提醒。）";

/// 模型什么都没说时催它。
const EMPTY_REPLY: &str = "（系统提醒，用户看不到：你刚才没有给出任何回复。请直接给出最终回复。）";

fn user_note(text: &str) -> Message {
    Message::User {
        content: vec![UserContent::Text(Text::new(text))],
    }
}

/// 同一个调用（工具名 + 参数）已经执行过几次。
#[derive(Default)]
struct Repeats(HashMap<String, usize>);

impl Repeats {
    /// 登记一次调用；超过 [`MAX_SAME_CALL`] 次时返回驳回的说明，调用不再执行。
    ///
    /// `satori_*` 不计：同样的参数（比如 `satori_context` 的空参数）在群聊往前走之后
    /// 结果本来就不同。
    fn check(&mut self, call: &ToolCall) -> Option<String> {
        let name = &call.function.name;
        if name.starts_with("satori_") {
            return None;
        }
        let seen = self
            .0
            .entry(format!("{name}\u{0}{}", call.function.arguments))
            .or_default();
        *seen += 1;
        (*seen > MAX_SAME_CALL).then(|| {
            format!(
                "你已经用完全相同的参数调用过 {name} {MAX_SAME_CALL} 次了，再调结果也不会变，这次没有执行。\
                 换个做法（换参数、换工具），或者就用手上已有的信息作答。"
            )
        })
    }
}

/// 执行一批工具调用，结果按调用顺序排列。
///
/// 连续的只读调用（见 [`super::tools::parallel_safe`]）一起并行：同一次回复里查三个网页、
/// 读四个文件，没有理由排队。其余按顺序一件件来——`bash` 前后常相依。
/// 并行靠 `join_all` 挂在这个 future 上，不 `spawn`：整轮被取消时一起干净地停掉。
async fn execute_batch(
    run: &AgentRun<'_>,
    calls: &[ToolCall],
    repeats: &mut Repeats,
) -> Vec<super::tools::ToolOutput> {
    let refused: Vec<Option<String>> = calls.iter().map(|call| repeats.check(call)).collect();
    let mut outputs: Vec<super::tools::ToolOutput> =
        (0..calls.len()).map(|_| Default::default()).collect();
    let mut index = 0;
    while index < calls.len() {
        let mut end = index;
        while end < calls.len()
            && refused[end].is_none()
            && super::tools::parallel_safe(&calls[end].function.name)
        {
            end += 1;
        }
        if end - index >= 2 {
            let group = (index..end).map(|k| {
                super::tools::execute(
                    &calls[k].function.name,
                    &calls[k].function.arguments,
                    run,
                    calls[k].id.as_str(),
                )
            });
            for (k, output) in (index..end).zip(futures_util::future::join_all(group).await) {
                outputs[k] = output;
            }
            index = end;
        } else {
            outputs[index] = if let Some(reason) = &refused[index] {
                reason.clone().into()
            } else {
                let call = &calls[index];
                super::tools::execute(
                    &call.function.name,
                    &call.function.arguments,
                    run,
                    call.id.as_str(),
                )
                .await
            };
            index += 1;
        }
    }
    outputs
}

/// 回复里有图没取到：把原因告诉模型，让它改好再给最终回复。
///
/// 这条消息只对模型可见；它不会进房间历史，用户也看不到。
fn complaint(figures: &figures::Figures) -> String {
    let mut out = format!(
        "你刚才的回复里有 {} 张图没能嵌进卡片：",
        figures.failures().len()
    );
    for (source, reason) in figures.failures() {
        out.push_str(&format!(
            "\n- {}：{reason}",
            super::super::utils::truncate_middle(source, 120)
        ));
    }
    out.push_str(
        "\n请改成真实存在、能打开的路径或链接（需要的话先用工具生成或确认），\
         或者把这些图去掉，然后重新给出完整的最终回复。这条提醒用户看不到，回复里不要提它。",
    );
    out
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
        let mut system = if let Some(explicit) = run.system_prompt { explicit.trim().to_string() } else {
            let mut base = ROOM_BASE
                .replace("{cwd}", &cwd.display().to_string())
                .replace("{today}", &today());
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
            let tool_names: Vec<String> =
                super::tools::definitions(run.tools, run.bridge.is_some(), run.web.is_some())
                    .into_iter()
                    .map(|tool| tool.name)
                    .collect();
            if !run.nested && tool_names.iter().any(|name| name == "delegate") {
                base.push_str("\n\n");
                base.push_str(super::delegate::HINT);
            }
            if run.figures {
                base.push_str("\n\n");
                base.push_str(&CARD_HINT.replace("{scratch}", &run.dir.display().to_string()));
                if tool_names.iter().any(|name| name == "view_image") {
                    base.push('\n');
                    base.push_str(VIEW_HINT);
                }
            }
            match run.append_system_prompt.trim() {
                "" => base,
                persona => format!("{base}\n\n---\n\n{persona}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::message::{ToolCallId, ToolFunction};

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall::new(ToolCallId::mint(), ToolFunction::new(name.into(), args))
    }

    #[test]
    fn identical_calls_are_refused_after_the_limit() {
        let mut repeats = Repeats::default();
        let same = call("bash", serde_json::json!({"command": "ls"}));
        for _ in 0..MAX_SAME_CALL {
            assert!(repeats.check(&same).is_none());
        }
        let refused = repeats.check(&same).expect("第 5 次应当被驳回");
        assert!(refused.contains("完全相同的参数") && refused.contains("bash"));
        // 参数一变就是另一个调用。
        assert!(repeats.check(&call("bash", serde_json::json!({"command": "ls -a"}))).is_none());
        // 工具不同、参数相同也各算各的。
        assert!(repeats.check(&call("read", serde_json::json!({"command": "ls"}))).is_none());
    }

    #[test]
    fn chat_tools_are_never_refused_as_repeats() {
        let mut repeats = Repeats::default();
        let context = call("satori_context", serde_json::json!({}));
        for _ in 0..MAX_SAME_CALL * 3 {
            assert!(repeats.check(&context).is_none());
        }
    }

    #[test]
    fn today_reads_like_a_date_with_weekday() {
        let today = today();
        assert!(today.contains('年') && today.contains('月') && today.contains('日'));
        assert!(today.contains("星期") && today.ends_with("（北京时间）"));
    }
}
