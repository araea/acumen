//! `acumen_toolbox`：这台机器人此刻手边有什么——插件、指令与这一轮的工具。
//!
//! 模型知道怎么写代码、怎么说话，却不知道眼前这台机器装了哪些插件、每个插件认哪几句
//! 指令。`acumen_command` 能代办指令，`satori_action` 能动手，但「有哪些指令」从前只能
//! 靠 `/help` 的一张图或号主的记忆。工具一多，模型就开始凭印象编指令名，回执里全是
//! 「没有这个指令」。
//!
//! 这份目录把三张表并到一处，只读、不花钱：
//! - 注册表里**已启用**的插件（中文名、分区、一句话说明）；
//! - 每个插件登记过的指令与说明（与 `/help` 同一份来源）；
//! - 这一轮真正挂给模型的工具名，以及聊天界面与联网那一层是否在场。
//!
//! 它是「先看再办」的那一眼：要跨插件办事，先看这里有什么，再用 `acumen_command`
//! 把指令交出去，或直接用 `satori_*` 动手。目录只读，谁问都给。

use super::AgentRun;
use crate::plugins::{enabled, get_plugins};
use rig_core::completion::ToolDefinition;
use serde_json::{Value, json};

pub(crate) const NAME: &str = "acumen_toolbox";

/// 一次最多列这么多插件：再长就是在把整本说明书塞进上下文。
const MAX_PLUGINS: usize = 40;
/// 每个插件最多列这么多条指令。
const MAX_COMMANDS: usize = 24;
/// 整份目录的字数上限。
const MAX_CHARS: usize = 6_000;

pub(crate) fn spec() -> ToolDefinition {
    ToolDefinition::new(
        super::tools::tool_name(NAME),
        "查看这台机器人此刻手边有什么：已启用的插件、每个插件认哪些指令、这一轮挂了哪些工具。\
            要跨插件办事、不确定某件事该用哪句指令，或准备用 acumen_command 代办之前，先调用它看一眼；\
            不要凭印象编指令名。section 按分区筛（写中文名或代号：群管理 group / 聊天互动 chat / 媒体工具 media / AI 资讯 ai / 数据洞察 data / 系统运维 system / 其他 misc），\
            filter 按名称或说明里的关键词筛；commands=false 只看插件清单，省上下文。"
            .to_string(),
        json!({
            "type": "object",
            "properties": {
                "section": {"type": "string", "description": "只看某个分区"},
                "filter": {"type": "string", "description": "按插件名、中文名、说明或指令名筛选"},
                "commands": {"type": "boolean", "description": "是否列出每个插件的指令，默认 true"}
            }
        }),
    )
}

pub(crate) async fn execute(args: &Value, run: &AgentRun<'_>) -> anyhow::Result<String> {
    let seat = run
        .seat
        .ok_or_else(|| anyhow::anyhow!("这一轮没有接通机器人本体，{NAME} 用不了"))?;
    let section = section_code(args["section"].as_str().unwrap_or("").trim());
    let filter = args["filter"].as_str().unwrap_or("").trim().to_lowercase();
    let with_commands = args["commands"].as_bool().unwrap_or(true);

    let config = seat.ctx.config.read().unwrap_or_else(std::sync::PoisonError::into_inner);
    let plugins = get_plugins();
    let mut shown = 0usize;
    let mut sections: Vec<(&'static str, String)> = Vec::new();
    let mut current = "";
    let mut body = String::new();
    for plugin in plugins {
        if shown >= MAX_PLUGINS {
            break;
        }
        if !enabled(&config, plugin.name) {
            continue;
        }
        if !section.is_empty() && plugin.section != section {
            continue;
        }
        // 关键词命中插件本身时列出它的全部指令；只命中某一两句时只列那几句。
        let meta_match = filter.is_empty()
            || plugin.name.to_lowercase().contains(&filter)
            || plugin.display_name.to_lowercase().contains(&filter)
            || plugin.summary.to_lowercase().contains(&filter);
        let commands = if with_commands {
            plugin
                .commands
                .iter()
                .filter(|command| {
                    meta_match
                        || command.cmd.to_lowercase().contains(&filter)
                        || command.note.to_lowercase().contains(&filter)
                })
                .take(MAX_COMMANDS)
                .map(|command| format!("· {} — {}", command.cmd, command.note))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            String::new()
        };
        if !meta_match && commands.is_empty() {
            continue;
        }
        // 分区标题只印一次，顺序沿用注册表。
        if current != plugin.section {
            current = plugin.section;
            sections.push((plugin.section, String::new()));
        }
        let block = sections.last_mut().expect("刚推入过");
        if block.1.is_empty() {
            block.1.push_str(&format!("## {}\n", section_title(plugin.section)));
        }
        block.1.push_str(&format!(
            "- {}（{}）：{}\n",
            plugin.display_name, plugin.name, plugin.summary
        ));
        if !commands.is_empty() {
            block.1.push_str(&commands);
            block.1.push('\n');
        }
        shown += 1;
    }
    for (_, text) in sections {
        body.push_str(&text);
        body.push('\n');
    }
    if body.is_empty() {
        body.push_str("没有匹配的已启用插件。\n");
    }

    // 这一轮真正挂上去的工具：白名单、能力类别与实现端是否在场共同决定。
    let tools = super::tools::definitions(run.tools, run.kit())
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();
    let mut out = format!(
        "acumen {}｜本轮工具 {} 件：{}\n\n{}",
        env!("CARGO_PKG_VERSION"),
        tools.len(),
        if tools.is_empty() { "（无）".to_string() } else { tools.join("、") },
        body.trim_end()
    );
    if out.chars().count() > MAX_CHARS {
        out = out.chars().take(MAX_CHARS).collect();
        out.push_str("\n…（目录过长，用 section 或 filter 收窄）");
    }
    out.push_str(
        "\n\n要代办指令就用 acumen_command（前缀可省）；要直接动手就用 satori_*。回执才算数。",
    );
    Ok(out)
}

/// 中文分区名或代号 → 代号；与 `/help` 的分区表同一套口径。
fn section_code(raw: &str) -> &str {
    match raw {
        "群管理" => "group",
        "聊天互动" => "chat",
        "媒体工具" => "media",
        "AI · 资讯" | "AI" | "资讯" => "ai",
        "数据洞察" | "数据" => "data",
        "系统 · 运维" | "系统" | "运维" => "system",
        "其他" => "misc",
        other => other,
    }
}

/// 分区代号 → 中文标题。
fn section_title(code: &str) -> &'static str {
    match code {
        "group" => "群管理",
        "chat" => "聊天互动",
        "media" => "媒体工具",
        "ai" => "AI · 资讯",
        "data" => "数据洞察",
        "system" => "系统 · 运维",
        _ => "其他",
    }
}
