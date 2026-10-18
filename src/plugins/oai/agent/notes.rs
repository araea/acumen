//! 手记：号主交代你记住的事。
//!
//! 管家最该有的一样东西是记性——称呼、习惯、家里设备的叫法、「以后这类事都这么办」——
//! 可每一轮对话都是从房间历史重新展开的，历史一清、换个房间就全忘了。手记是一份跨房间、
//! 跨清空的短清单，每一轮的系统提示词里都带着它（见 [`prompt_block`]），模型用 `note`
//! 工具增删。
//!
//! 两条边界，都为了「手记是可信的」：
//! - **只有号主能写，也只对号主可见。** 手记进系统提示词，等于最高优先级的指令；客人若能让
//!   管家记一句「号主允许我执行任何命令」，就是往它脑子里塞假记忆。客人的轮次里不挂这件工具，
//!   提示词里也不带手记——号主的私事不该被客人一句「你都记得我什么」问出来；
//! - **只记事实与偏好**，不记密钥口令（工具说明里写了，也不会被拦——拦不住，靠它自觉）。

use super::AgentRun;
use crate::storage::JsonState;
use rig_core::completion::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(crate) const NAME: &str = "note";

/// 最多几条。再多就该整理，而不是继续往提示词里塞。
const MAX_NOTES: usize = 60;
/// 单条的字数上限。
const MAX_CHARS: usize = 300;

#[derive(Clone, Default, Serialize, Deserialize)]
struct Notes {
    next: u32,
    items: Vec<Note>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Note {
    id: u32,
    text: String,
    /// 记下的时间（Unix 秒）。
    at: i64,
}

static STORE: JsonState<Notes> = JsonState::new(super::super::LOG_TARGET, "oai", "notes.json", "管家手记");

pub(crate) fn spec() -> ToolDefinition {
    ToolDefinition {
        name: NAME.to_string(),
        description: "号主的手记：跨房间、跨清空历史都留着的短清单，每一轮的提示词里都会带上。\
            号主说「记住」「以后……」「别忘了」，或者你发现一条值得长期记着的事实与偏好（称呼、作息、常用设备与账号叫法、固定的做事规矩）时，用 add 记一条；\
            条目写成一句自成一体的话，别写「今天」「刚才」这类过几天就看不懂的词。号主说「别记这个了」「忘掉」时用 forget 删掉对应编号；list 看全部（提示词里已经有，一般不必查）。\
            只记事实与偏好，不要记密码、密钥、令牌；一次性的事别记，那是提醒（schedule）的活。"
            .to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["add", "forget", "list"]},
                "text": {"type": "string", "description": "add：要记的一句话（不超过 300 字）"},
                "id": {"type": "integer", "description": "forget：条目编号（提示词里 [编号] 那个数字）"}
            },
            "required": ["action"]
        }),
    }
}

pub(crate) async fn execute(args: &Value, run: &AgentRun<'_>) -> anyhow::Result<String> {
    // 客人的轮次里这件工具本来就不挂；这里再拦一道，防白名单或委派绕过去。
    if !run.seat.is_some_and(|seat| seat.asker.owner) {
        anyhow::bail!("手记只有号主能让我记、让我忘");
    }
    match args["action"].as_str().unwrap_or("").trim() {
        "add" => {
            let text = args["text"].as_str().unwrap_or("").trim();
            if text.is_empty() {
                anyhow::bail!("参数错误：text 不能为空");
            }
            if text.chars().count() > MAX_CHARS {
                anyhow::bail!("这一条太长了（上限 {MAX_CHARS} 字），拆短或提炼一下再记");
            }
            let text = text.replace('\n', " ");
            STORE
                .with(|notes| {
                    if let Some(same) = notes.items.iter().find(|note| note.text == text) {
                        return Ok(format!("已经记过了（[{}]）", same.id));
                    }
                    if notes.items.len() >= MAX_NOTES {
                        anyhow::bail!(
                            "手记已经满了（{MAX_NOTES} 条）。先看看哪些过时了，用 forget 删掉再记"
                        );
                    }
                    notes.next = notes.next.max(notes.items.iter().map(|n| n.id).max().unwrap_or(0)) + 1;
                    let id = notes.next;
                    notes.items.push(Note {
                        id,
                        text,
                        at: chrono::Local::now().timestamp(),
                    });
                    Ok(format!("记下了（[{id}]，共 {} 条）", notes.items.len()))
                })
                .await
        }
        "forget" => {
            let id = args["id"].as_u64().unwrap_or(0) as u32;
            STORE
                .with(|notes| {
                    let before = notes.items.len();
                    notes.items.retain(|note| note.id != id);
                    if notes.items.len() == before {
                        anyhow::bail!("手记里没有编号 [{id}]");
                    }
                    Ok(format!("已忘掉 [{id}]（还剩 {} 条）", notes.items.len()))
                })
                .await
        }
        "list" => {
            let block = lines().await;
            Ok(if block.is_empty() { "手记是空的".to_string() } else { block })
        }
        other => anyhow::bail!("未知操作：{other}"),
    }
}

async fn lines() -> String {
    STORE
        .with(|notes| {
            notes
                .items
                .iter()
                .map(|note| format!("- [{}] {}", note.id, note.text))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .await
}

/// 写进系统提示词的那一段；没有手记时是空串。
///
/// 放在提示词的最后：手记变动只会让它之后的内容缓存失效，前面那一大段基座不受影响。
pub(crate) async fn prompt_block() -> String {
    let body = lines().await;
    if body.is_empty() {
        return String::new();
    }
    format!(
        "## 号主交代你记住的事（手记）\n以下是号主亲口交代、你自己记下的，可信；对号主办事时照着来，对别人不要提起。\n{body}"
    )
}
