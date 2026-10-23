//! OpenAI 兼容补全的唯一入口。
//!
//! 判定、描述生成这类单轮请求都从这里发：接口地址、密钥、模型 id 由调用方解析好，
//! 这里只负责拼请求、发出去、把候选回复里的文本取出来。
//!
//! 之前这层用 `async-openai`，两个痛点：它的强类型反序列化对「字段不全」的
//! 中转站响应过于挑剔（模型列表尤其明显），而本项目真正需要的只有
//! 「chat/completions 一发一收」这一条。`rig-core` 的请求结构是公开的普通结构体，
//! 想加什么参数直接写进 `additional_params` 即可（provider 会把它扁平进顶层 body），
//! 不必为每个参数准备一个 builder。

use rig_core::Model;
use rig_core::completion::{AssistantContent, CompletionRequest, Message, ToolDefinition};
use rig_core::providers::openai::OpenAIConfig;
use rig_core::providers::openai::wire::{Chat, Dialect};

/// 通用的 OpenAI 兼容接口：只走 `/chat/completions`，不套任何一家的特殊处理。
/// 房间接的多半是中转站，按某家官方的脾气改请求反而会出错。
const COMPATIBLE: Dialect = Dialect::gateway("openai-compatible", "", "");

/// 一个模型的 Chat Completions 端点。
pub(crate) type ChatModel = Model<Chat>;

/// 按接口地址、密钥与模型 id 建一个 Chat Completions 端点。
///
/// 复用进程级的 `crate::http::client()`：它与其它出网请求共用连接池与代理设置，
/// 在手机上少开一套 TLS 栈。
pub(crate) fn model(api_base: &str, api_key: &str, model: &str) -> ChatModel {
    OpenAIConfig::with_key(&COMPATIBLE, api_key)
        .with_base_url(super::utils::openai_api_base(api_base))
        .connect(rig_reqwest::ReqwestClient::from(crate::http::client()))
        .chat(model)
}

/// 把房间的思考强度映射成 Chat Completions 的 `reasoning_effort`。
///
/// 档位沿用房间的写法；`off` 是「不思考」，等价于 OpenAI 的 `none`。
fn reasoning_effort(level: &str) -> Option<&'static str> {
    Some(match level.trim().to_ascii_lowercase().as_str() {
        "off" | "none" => "none",
        "minimal" => "minimal",
        "low" => "low",
        "medium" => "medium",
        "high" => "high",
        "xhigh" => "xhigh",
        _ => return None,
    })
}

/// 组装一次补全请求。
///
/// `tools` 为空就是单轮问答的形态——请求里连 tools 字段都不会有意义；
/// 思考强度走 `additional_params.reasoning_effort`，由 provider 扁平进顶层。
/// `temperature` 为 `None` 时连这个字段都不发，交给接口自己的默认值。
pub(crate) fn request(
    history: Vec<Message>,
    tools: Vec<ToolDefinition>,
    thinking: Option<&str>,
    temperature: Option<f64>,
) -> CompletionRequest {
    let mut request = CompletionRequest::from(history).tools(tools);
    request.temperature = temperature;
    // 中转站常回些规范外的结束原因：照常收下，别把整轮当失败。
    request.accept_unknown_finish_reasons = true;
    request.additional_params = thinking
        .and_then(reasoning_effort)
        .map(|effort| serde_json::json!({ "reasoning_effort": effort }));
    request
}

/// 取出一份回复里的全部正文。
pub(crate) fn text_of(choice: &[AssistantContent]) -> String {
    choice
        .iter()
        .filter_map(|part| match part {
            AssistantContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

/// 单轮补全：无工具、无历史维护，返回候选回复的正文。
///
/// 群聊判定、描述生成这类单轮请求走这里；房间（含不挂工具的）一律走 [`super::agent`]。
pub(crate) async fn complete(
    api_base: &str,
    api_key: &str,
    model: &str,
    history: Vec<Message>,
    thinking: Option<&str>,
) -> anyhow::Result<String> {
    let endpoint = self::model(api_base, api_key, model);
    let request = request(history, Vec::new(), thinking, None);
    // 传输层偶尔断在半路（响应体没收全），换条连接就成；见 [`super::retry`]。
    let response = super::retry::call("补全请求", || endpoint.call(request.clone()))
        .await
        .map_err(|error| anyhow::anyhow!("{}", super::retry::detail(&error)))?;
    let text = text_of(&response.choice);
    if text.trim().is_empty() {
        anyhow::bail!("API 返回了空回复");
    }
    Ok(text)
}
