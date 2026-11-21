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
    endpoint(super::utils::openai_api_base(api_base), api_key, model)
}

/// 原生档走官方根路径（`/chat/completions`），不带 OpenAI 兼容地址里那一截 `/v1`。
pub(crate) fn native_model(api_base: &str, api_key: &str, model: &str) -> ChatModel {
    endpoint(super::native::official_root(api_base), api_key, model)
}

fn endpoint(base: String, api_key: &str, model: &str) -> ChatModel {
    OpenAIConfig::with_key(&COMPATIBLE, api_key)
        .with_base_url(base)
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
///
/// `native` 有值时改走 DeepSeek 官方的 Chatbot 扩展字段：服务端工具、思考、
/// 内容安全与标题都在 `additional_params` 里（其中 `tools` 由 rig 追加在自己
/// 那批函数工具之后）。这类请求里的采样参数由服务端定，发了也被忽略，索性不发。
pub(crate) fn request(
    history: Vec<Message>,
    tools: Vec<ToolDefinition>,
    thinking: Option<&str>,
    temperature: Option<f64>,
    native: Option<&super::native::Native>,
) -> CompletionRequest {
    let mut request = CompletionRequest::from(history).tools(tools);
    request.temperature = if native.is_some() { None } else { temperature };
    // 中转站常回些规范外的结束原因：照常收下，别把整轮当失败。
    request.accept_unknown_finish_reasons = true;
    request.additional_params = match native {
        Some(native) => Some(native.params()),
        None => thinking
            .and_then(reasoning_effort)
            .map(|effort| serde_json::json!({ "reasoning_effort": effort })),
    };
    request
}

/// 把 rig 编不出来的两块内容盖进最终请求：`latest_reminder` 消息与文档 `file` 块。
///
/// rig 的消息类型没有 `latest_reminder` 这个角色，文档块也会编成嵌套的
/// `{"type":"file","file":{"file_id":…}}`（官方要的是扁平的 `file_id`）。做法是：
/// 先用 wire 把请求编码一遍，拿到它生成好的 `messages`，在正确的位置插/拼这两块，
/// 再把整个数组作为 `additional_params.messages` 盖回去——rig 的合并规则是
/// `additional_params` 覆盖同名的顶层字段（数组整体替换），见 `request_params`。
///
/// - `latest_reminder` 要紧贴一条 `user` 消息、且不能夹在 tool_use 与 tool_result 之间，
///   所以只在「最后一条消息就是 user」时插；工具循环里那几步不插，避免整条 400。
/// - 文档块附在最后一条 `user` 消息的 content 末尾，每一步都重盖一次，保证模型在整个
///   工具循环里都读得到文件（官方会按 file_id 把文档拼回上下文）。
///
/// 只在原生档（官方地址 + `deepseek-flash`）调用；编码失败只记一行日志，照原请求发，
/// 不至于为了一段元信息把整轮答复弄没。
#[allow(clippy::too_many_arguments)]
pub(crate) fn inject_extras(
    model: &ChatModel,
    request: &mut CompletionRequest,
    documents: &[String],
    reminder: Option<&str>,
) {
    use rig_core::wire::{Body, Mode, Wire};

    if documents.is_empty() && reminder.is_none() {
        return;
    }
    let Ok(encoded) = model.wire.encode(request.clone(), Mode::Unary) else {
        warn!(target: "Plugin/OAI", "原生请求编码失败，跳过元信息/文档注入");
        return;
    };
    let Body::Bytes(bytes) = encoded.request.body() else {
        return;
    };
    let Ok(mut body) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return;
    };
    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };
    let Some(last_user) = messages
        .iter()
        .rposition(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
    else {
        return;
    };
    // 最后一条消息就是那条 user：此时插 latest_reminder 才不会夹进工具调用中间。
    let trailing_is_user = last_user + 1 == messages.len();
    let inserted = reminder.is_some() && trailing_is_user;
    if let Some(rem) = reminder.filter(|_| trailing_is_user) {
        messages.insert(
            last_user,
            serde_json::json!({"role": "latest_reminder", "content": rem}),
        );
    }
    let target = last_user + usize::from(inserted);
    if !documents.is_empty()
        && let Some(content) = messages.get_mut(target).and_then(|m| m.get_mut("content"))
    {
        // rig 把「只有一段文字」的用户消息收成纯字符串；要挂文档块就先还原成 parts 数组。
        if let Some(text) = content.as_str().map(str::to_string) {
            *content = serde_json::json!([{"type": "text", "text": text}]);
        }
        if let Some(parts) = content.as_array_mut() {
            for id in documents {
                parts.push(super::files::document_part(id));
            }
        }
    }
    let params = request
        .additional_params
        .get_or_insert_with(|| serde_json::json!({}));
    if let Some(map) = params.as_object_mut() {
        map.insert("messages".into(), body["messages"].clone());
    }
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
    // 判定、反思、描述生成这类单轮请求一律不挂原生工具：额度留给真正要答的话。
    let request = request(history, Vec::new(), thinking, None, None);
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

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::message::{Text, UserContent};
    use rig_core::wire::{Body, Mode, Wire};
    use serde_json::{Value, json};

    fn body_of(request: CompletionRequest, model: &ChatModel) -> Value {
        let encoded = model.wire.encode(request, Mode::Unary).unwrap();
        let Body::Bytes(bytes) = encoded.request.body() else {
            panic!("请求体应是 JSON 字节");
        };
        serde_json::from_slice(bytes).unwrap()
    }

    /// 注入后重新编码：`latest_reminder` 紧贴最后一条 user，文档块是该 user 的扁平
    /// `file` 块；声明过的托管工具没被 messages 覆盖顺手带没。
    #[test]
    fn extras_land_in_the_final_body() {
        let cfg = super::super::native::DeepSeekConfig::default();
        let native = super::super::native::Native::resolve(
            &cfg,
            "https://api.deepseek.com/v1",
            "deepseek-flash",
            Some("high"),
            true,
            true,
        )
        .unwrap();
        let history = vec![Message::User {
            content: vec![UserContent::Text(Text::new("这份文档讲了什么"))],
        }];
        let mut request = request(history, Vec::new(), None, None, Some(&native));
        let model = native_model("https://api.deepseek.com/v1", "sk-test", "deepseek-flash");
        inject_extras(
            &model,
            &mut request,
            &["file-api-abc".to_string()],
            Some("2026-11-21,星期六,杭州,Web,中文"),
        );
        let body = body_of(request, &model);
        assert_eq!(
            body["tools"],
            json!([
                {"type": "deepseek_web_search"},
                {"type": "deepseek_read_url"}
            ])
        );
        let messages = body["messages"].as_array().unwrap();
        let last = messages.len() - 1;
        assert_eq!(messages[last]["role"], "user");
        assert_eq!(messages[last - 1]["role"], "latest_reminder");
        assert_eq!(
            messages[last - 1]["content"],
            "2026-11-21,星期六,杭州,Web,中文"
        );
        let parts = messages[last]["content"].as_array().unwrap();
        assert!(parts
            .iter()
            .any(|part| part == &json!({"type": "file", "file_id": "file-api-abc"})));
    }

    /// 末尾已经是 assistant（工具循环里的那几步）：别插 latest_reminder，免得夹进
    /// tool_use 与 tool_result 之间；文档块仍附到最后一条 user 上，模型继续读得到。
    #[test]
    fn reminder_is_skipped_when_a_user_message_is_not_last() {
        let cfg = super::super::native::DeepSeekConfig::default();
        let native = super::super::native::Native::resolve(
            &cfg,
            "https://api.deepseek.com/v1",
            "deepseek-flash",
            None,
            true,
            false,
        )
        .unwrap();
        let history = vec![
            Message::User {
                content: vec![UserContent::Text(Text::new("看这份文件"))],
            },
            Message::assistant("我先看看"),
        ];
        let mut request = request(history, Vec::new(), None, None, Some(&native));
        let model = native_model("https://api.deepseek.com/v1", "sk-test", "deepseek-flash");
        inject_extras(&model, &mut request, &["file-api-xyz".to_string()], Some("2026-11-21,星期六,杭州,Web,中文"));
        let body = body_of(request, &model);
        let messages = body["messages"].as_array().unwrap();
        assert!(messages
            .iter()
            .all(|message| message["role"] != "latest_reminder"));
        assert_eq!(messages[0]["role"], "user");
        let parts = messages[0]["content"].as_array().unwrap();
        assert!(parts
            .iter()
            .any(|part| part == &json!({"type": "file", "file_id": "file-api-xyz"})));
    }
}
