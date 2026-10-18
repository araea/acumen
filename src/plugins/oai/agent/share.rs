//! `send_file`：把本机的一个文件或图片发到当前聊天。
//!
//! 群里的房间有 `satori_action` 的 `send`，能发文件；**私聊里的房间没有群聊那一套工具**，
//! 管家做好了一份报告、导出了一份日志、画好了一张图，却没有办法交到用户手里——卡片里嵌图只管
//! 图，管不了 PDF 或压缩包。这件工具补上这个口子，所以只在没接群聊界面的轮次里挂（见
//! [`super::tools::definitions`]）。
//!
//! 边界：号主可以发本机任何路径的普通文件（≤ 20 MiB）；客人只能发这一轮工作目录里的——
//! 那是管家这一轮刚生成的东西，不是机器上别的什么。发到哪里不由参数决定，永远是当前聊天。

use super::AgentRun;
use crate::message::Message;
use rig_core::completion::ToolDefinition;
use serde_json::{Value, json};

pub(crate) const NAME: &str = "send_file";

/// 单个文件的上限：与群里发文件同一个口径。
const MAX_BYTES: u64 = 20 * 1024 * 1024;

pub(crate) fn spec() -> ToolDefinition {
    ToolDefinition {
        name: NAME.to_string(),
        description: "把本机的一个文件发到当前这个聊天里（图片会直接显示，其他当文件发）。\
            做好了报告、导出了日志、画好了图，用户要拿到手时用它；单个文件不超过 20 MiB，发到哪里不能指定，永远是当前聊天。\
            这是立刻发出的动作，发之前确认路径对、内容是用户该看的（别把密钥、配置文件、别人的私人资料发出去）。\
            回复正文里仍要用一两句话交代发了什么。"
            .to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "文件路径（绝对路径，或相对工作目录）"},
                "name": {"type": "string", "description": "对方看到的文件名，默认取原文件名"}
            },
            "required": ["path"]
        }),
    }
}

pub(crate) async fn execute(args: &Value, run: &AgentRun<'_>) -> anyhow::Result<String> {
    let seat = run
        .seat
        .ok_or_else(|| anyhow::anyhow!("这一轮没有接通聊天，{NAME} 用不了"))?;
    let raw = args["path"].as_str().unwrap_or("").trim();
    anyhow::ensure!(!raw.is_empty(), "参数错误：path 不能为空");
    let input = std::path::Path::new(raw);
    let root = run.cwd.unwrap_or(run.dir);
    let path = tokio::fs::canonicalize(if input.is_absolute() { input.to_path_buf() } else { root.join(input) })
        .await
        .map_err(|error| anyhow::anyhow!("找不到 {raw}：{error}"))?;
    if !seat.asker.owner {
        let scratch = tokio::fs::canonicalize(run.dir).await?;
        anyhow::ensure!(
            path.starts_with(&scratch),
            "客人只能让我发这一轮刚生成在工作目录里的文件；别的文件要号主点头"
        );
    }
    let meta = tokio::fs::metadata(&path).await?;
    anyhow::ensure!(meta.is_file(), "{} 不是普通文件", path.display());
    anyhow::ensure!(
        meta.len() > 0 && meta.len() <= MAX_BYTES,
        "文件须不为空且不超过 {} MiB（这个 {} 字节）",
        MAX_BYTES / 1024 / 1024,
        meta.len()
    );
    let bytes = tokio::fs::read(&path).await?;
    let name = args["name"]
        .as_str()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .or_else(|| path.file_name().and_then(|name| name.to_str()).map(str::to_string))
        .unwrap_or_else(|| "file".to_string());
    let name = crate::plugins::oai::chat::session::safe_file_name(&name);

    let encoded = format!("base64://{}", base64_of(&bytes));
    let image = image::guess_format(&bytes[..bytes.len().min(32)]).is_ok();
    let message = if image {
        Message::new().image(encoded)
    } else {
        Message::new().file(encoded, Some(name.clone()))
    };
    crate::adapters::satori::send_msg(
        &seat.ctx,
        seat.writer.clone(),
        seat.group.as_deref(),
        Some(&seat.asker.id),
        message,
    )
    .await
    .map_err(|error| anyhow::anyhow!("发送失败：{error}"))?;
    info!(
        target: crate::plugins::oai::LOG_TARGET,
        "管家发出文件「{name}」{} 字节（{}）",
        bytes.len(),
        seat.asker.id
    );
    Ok(format!("已发出「{name}」（{} 字节）", bytes.len()))
}

fn base64_of(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
