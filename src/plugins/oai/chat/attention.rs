//! 人格自行选择短期关注对象/话题；只影响后续新消息的判断，不会定时自言自语。
use super::window::Turn;
use serde::Deserialize;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(crate) struct Focus {
    pub users: Vec<String>,
    pub topic: String,
    pub until: Instant,
}

#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    users: Vec<Id>,
    #[serde(default)]
    topic: String,
    seconds: u64,
}

/// 用户 ID 是字符串；模型照着记录抄数字号码时常写成 JSON 数字（`416012267`）。
#[derive(Deserialize)]
#[serde(untagged)]
enum Id {
    Number(i64),
    Text(String),
}

impl Id {
    fn get(&self) -> Option<String> {
        match self {
            Id::Number(id) => Some(id.to_string()),
            Id::Text(raw) => Some(raw.trim().to_string()).filter(|id| !id.is_empty()),
        }
    }
}

/// 去掉列表符号与首尾空白，供控制行判定使用。
fn decorated(line: &str) -> &str {
    line.trim()
        .trim_start_matches("- ")
        .trim_start_matches("* ")
        .trim()
}

/// 这一行是不是「关注」控制行。
///
/// 文档与提示词都写方括号，但模型偶尔会仿着 Satori 的 XML 写成尖括号
/// （`<focus:{…}>`）。两种都要认：控制行一旦漏判，就会原样发进群里。
pub(crate) fn is_control(line: &str) -> bool {
    let clean = decorated(line);
    clean.starts_with("[focus:") || clean.starts_with("<focus:")
}

/// 取出控制行里的 JSON 正文；括号不闭合（或不是控制行）时给 None。
fn body_of(line: &str) -> Option<&str> {
    let clean = decorated(line);
    let rest = clean
        .strip_prefix("[focus:")
        .or_else(|| clean.strip_prefix("<focus:"))?;
    let rest = rest.trim_end();
    rest.strip_suffix(']').or_else(|| rest.strip_suffix('>'))
}

/// None 保持现状，Some(None) 主动离场，Some(Some(..)) 更新关注。
/// 内部控制行无论格式是否正确都不发到群里。
pub(crate) fn extract(
    raw: &str,
    turns: &[Turn],
    max_seconds: u64,
) -> (String, Option<Option<Focus>>) {
    let mut lines = Vec::new();
    let mut update = None;
    for line in raw.lines() {
        if is_control(line) {
            if let Some(body) = body_of(line)
                && let Ok(request) = serde_json::from_str::<Request>(body)
            {
                if request.seconds == 0 || max_seconds == 0 {
                    update = Some(None);
                } else {
                    let users = request
                        .users
                        .iter()
                        .filter_map(Id::get)
                        .filter(|id| {
                            turns
                                .iter()
                                .any(|turn| !turn.from_me && turn.user_id == *id)
                        })
                        .take(3)
                        .collect::<Vec<_>>();
                    let topic = request
                        .topic
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .chars()
                        .take(80)
                        .collect::<String>();
                    if !users.is_empty() || !topic.is_empty() {
                        update = Some(Some(Focus {
                            users,
                            topic,
                            until: Instant::now()
                                + Duration::from_secs(request.seconds.min(max_seconds).min(600)),
                        }));
                    }
                }
            }
            continue;
        }
        lines.push(line);
    }
    (lines.join("\n"), update)
}
