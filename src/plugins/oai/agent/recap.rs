//! 房间历史的交接摘要：窗口装不下的那部分，用一次模型调用写成摘要，之后每轮复用。
//!
//! 每轮只展开最近的一截历史（[`super::run::HISTORY_BUDGET`]），更早的话从前是直接不带——
//! 聊了一下午的事，到第二天一句「接着弄昨天那个」就接不上了。这里照 pi 的压缩做：把落出
//! 窗口的那段写成一份结构化的交接摘要（在办什么、定下了什么、关键的名字数字路径、还差什么），
//! 放在展开的历史前面。
//!
//! **省额度的关键是摘要算一次、反复用**：摘要连同它覆盖到第几条、那几条的指纹一起落盘
//! （`data/oai/recaps.json`），之后每一轮直接拿来用，不再调用；窗口外又攒够
//! [`REFRESH_CHARS`] 字新内容，才拿「旧摘要 + 新落出窗口的那段」增量更新一次。比起每轮
//! 重发几十万字的全量历史，这一次调用的花费很快就回本了。
//!
//! 历史被编辑、删除、清空后指纹对不上，摘要作废、按需重算；摘要失败就退回只带最近一截。

use super::super::types::ChatMessage;
use rig_core::completion::Message;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::plugins::oai::LOG_TARGET;

/// 窗口外没被摘要覆盖的部分攒到这么多字，才更新一次摘要；不到这个量就连同窗口一起原样带上。
const REFRESH_CHARS: usize = 24_000;

/// 一次摘要最多读这么多字的原文：第一次给一间聊了几个月的房间写摘要，不必把几十万字全读一遍。
const SOURCE_CHARS: usize = 80_000;

/// 单条消息进摘要原文时最多这么多字：贴进来的长文、长回复只留头尾。
const MESSAGE_CHARS: usize = 3_000;

/// 落盘最多留几份（按更新时间淘汰）。
const MAX_RECAPS: usize = 200;

const SYSTEM: &str = "\
你在给一段很长的对话写交接摘要。下一轮接手的助手只看得到这份摘要和最近的几轮原文，摘要之外的早先内容它一概看不到。
写清楚接着办事要知道的东西，按下面几节写（某节没有内容就省掉那一节）：
## 在办的事
用户想要什么、做到了哪一步。
## 已经定下的
做过的决定、用户提过的要求与偏好、约好的规矩。
## 关键信息
名字、账号、数字、文件路径、命令、链接、版本号这类原样记下，别概括掉。
## 还没完成
待办、悬而未决的问题、说好下次再弄的事。
## 聊过的其他话题
一两句带过即可。
规则：
- 原文里以〔…〕开头的用户消息标着说话人（号主或某位客人），摘要里也要写明是谁说的、谁要求的，别混在一起。
- 只写原文里有的，不推测、不补充、不评价；时间先后要对。
- 已经有旧摘要时，把新内容并进去：过时的改掉，办完的挪出「在办的事」，别把旧摘要整段照抄一遍再在后面续写。
- 密钥、口令、令牌这类凭据不写进摘要，写「（已提供，未记录）」。
- 用中文，总长不超过 1500 字。直接输出摘要正文，不要开场白。";

/// 落盘的一份摘要。
#[derive(Clone, Serialize, Deserialize)]
struct Recap {
    /// 覆盖了历史的前多少条。
    covers: usize,
    /// 这些条的指纹：历史被改过就对不上。
    fingerprint: String,
    text: String,
    updated: i64,
}

/// 这一轮能用的摘要：历史从 `start` 起原样展开，之前的由 `text` 代替。
pub(crate) struct Window {
    pub start: usize,
    pub text: String,
}

/// 前 `count` 条历史的指纹。
fn fingerprint(history: &[ChatMessage]) -> String {
    let mut context = md5::Context::new();
    for message in history {
        context.consume(message.role.as_bytes());
        context.consume([0]);
        context.consume(message.from.as_bytes());
        context.consume([0]);
        context.consume(message.content.as_bytes());
        context.consume([1]);
    }
    format!("{:x}", context.finalize())
}

fn chars(history: &[ChatMessage]) -> usize {
    history.iter().map(|message| message.content.chars().count()).sum()
}

fn store_path() -> Option<std::path::PathBuf> {
    crate::storage::data_path("oai").ok().map(|dir| dir.join("recaps.json"))
}

/// 读写摘要文件用的锁：几间房同时收尾时别互相覆盖。
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn load() -> HashMap<String, Recap> {
    let Some(path) = store_path() else { return HashMap::new() };
    tokio::fs::read_to_string(path)
        .await
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

async fn save(key: &str, recap: Recap) {
    let _guard = LOCK.lock().await;
    let Some(path) = store_path() else { return };
    let mut all = load().await;
    all.insert(key.to_string(), recap);
    if all.len() > MAX_RECAPS {
        let mut keys: Vec<(i64, String)> = all.iter().map(|(key, recap)| (recap.updated, key.clone())).collect();
        keys.sort();
        for (_, key) in keys.into_iter().take(all.len() - MAX_RECAPS) {
            all.remove(&key);
        }
    }
    let Ok(text) = serde_json::to_string(&all) else { return };
    let temporary = path.with_extension("json.tmp");
    if tokio::fs::write(&temporary, text).await.is_ok() {
        let _ = tokio::fs::rename(&temporary, &path).await;
    }
}

/// 一间房一份历史一个键：公共历史按房间，私有历史按房间 + 人。
pub(crate) fn key(room: &str, private_for: Option<&str>) -> String {
    match private_for {
        Some(uid) => format!("{room}\u{0}{uid}"),
        None => room.to_string(),
    }
}

/// 摘要的原文：一条一行，标出说话人，过长的留头尾。
fn transcript(history: &[ChatMessage]) -> String {
    let mut lines: Vec<String> = history
        .iter()
        .filter(|message| !message.content.trim().is_empty())
        .map(|message| {
            let who = match message.role.as_str() {
                "assistant" => "助手".to_string(),
                _ if !message.from.is_empty() => format!("〔{}〕", message.from),
                _ => "用户".to_string(),
            };
            let body = super::super::utils::truncate_middle(message.content.trim(), MESSAGE_CHARS);
            format!("{who}：{body}")
        })
        .collect();
    // 太长时从最老的一头丢：最近的事才是接着办要知道的。
    let mut total: usize = lines.iter().map(|line| line.chars().count()).sum();
    let mut dropped = 0;
    while total > SOURCE_CHARS && lines.len() > 1 {
        total -= lines.remove(0).chars().count();
        dropped += 1;
    }
    let mut out = String::new();
    if dropped > 0 {
        out.push_str(&format!("（更早的 {dropped} 条太久远，没有放进来）\n"));
    }
    out.push_str(&lines.join("\n\n"));
    out
}

/// 算出这一轮的历史窗口：要不要摘要、从哪条起原样展开。
///
/// `skip` 是不带摘要时本该丢掉的条数（[`super::run::history_start`]）。返回 `None` 表示
/// 没有可用的摘要（不需要，或者写摘要失败了），照旧只带最近一截。
pub(crate) async fn window(
    key: &str,
    history: &[ChatMessage],
    skip: usize,
    api_base: &str,
    api_key: &str,
    model: &str,
) -> Option<Window> {
    if skip == 0 {
        return None;
    }
    let stored = load()
        .await
        .remove(key)
        .filter(|recap| recap.covers <= history.len() && fingerprint(&history[..recap.covers]) == recap.fingerprint);
    // 现成的摘要还够用：窗口外没被覆盖的那段不多，连同窗口一起原样带上，不调用。
    if let Some(recap) = &stored
        && (recap.covers >= skip || chars(&history[recap.covers..skip]) <= REFRESH_CHARS)
    {
        return Some(Window { start: recap.covers, text: recap.text.clone() });
    }
    let (previous, from) = match &stored {
        Some(recap) => (Some(recap.text.as_str()), recap.covers),
        None => (None, 0),
    };
    let mut prompt = String::new();
    if let Some(previous) = previous {
        prompt.push_str("旧摘要（覆盖更早的对话）：\n");
        prompt.push_str(previous);
        prompt.push_str("\n\n---\n\n接在旧摘要之后的对话：\n");
    } else {
        prompt.push_str("要写摘要的对话：\n");
    }
    prompt.push_str(&transcript(&history[from..skip]));
    let messages = vec![Message::System { content: SYSTEM.to_string() }, Message::user(prompt)];
    let started = std::time::Instant::now();
    match super::super::llm::complete(api_base, api_key, model, messages, None).await {
        Ok(text) => {
            let text = text.trim().to_string();
            info!(
                target: LOG_TARGET,
                "房间历史写了交接摘要：覆盖前 {skip} 条（这次读了第 {} 条起），{} 字，{:.1} 秒",
                from + 1,
                text.chars().count(),
                started.elapsed().as_secs_f32()
            );
            save(
                key,
                Recap {
                    covers: skip,
                    fingerprint: fingerprint(&history[..skip]),
                    text: text.clone(),
                    updated: chrono::Local::now().timestamp(),
                },
            )
            .await;
            Some(Window { start: skip, text })
        }
        Err(error) => {
            warn!(target: LOG_TARGET, "房间历史的交接摘要没写成，这一轮只带最近的一截：{error:#}");
            // 旧摘要还对得上就先用着，总比什么都没有强。
            stored.map(|recap| Window { start: recap.covers, text: recap.text })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_changes_when_history_is_edited() {
        let mut history = vec![ChatMessage::new("user", "a", Vec::new()), ChatMessage::new("assistant", "b", Vec::new())];
        let before = fingerprint(&history);
        history[1].content = "c".into();
        assert_ne!(before, fingerprint(&history));
    }

    #[test]
    fn transcript_drops_the_oldest_first() {
        let long = "字".repeat(MESSAGE_CHARS);
        let history: Vec<ChatMessage> = (0..60).map(|_| ChatMessage::new("user", &long, Vec::new())).collect();
        let text = transcript(&history);
        assert!(text.starts_with("（更早的 "));
        assert!(text.chars().count() <= SOURCE_CHARS + 200);
    }
}
