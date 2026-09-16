//! 人格自行选择短期关注对象/话题；只影响后续新消息的判断，不会定时自言自语。
use super::window::Turn;
use serde::Deserialize;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(crate) struct Focus {
    pub users: Vec<i64>,
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

/// QQ 号在记录里是数字，模型有时写成字符串（`"416012267"`）。
#[derive(Deserialize)]
#[serde(untagged)]
enum Id {
    Number(i64),
    Text(String),
}

impl Id {
    fn qq(&self) -> Option<i64> {
        match self {
            Id::Number(id) => Some(*id),
            Id::Text(raw) => raw.trim().parse().ok(),
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
                        .filter_map(Id::qq)
                        .filter(|id| {
                            *id > 0
                                && turns
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_persona_can_follow_a_topic_without_sending_control_lines() {
        let (body, update) = extract(
            "[focus:{\"users\":[999],\"topic\":\"这游戏\",\"seconds\":99999}]\n[silent]",
            &[],
            300,
        );
        assert_eq!(body, "[silent]");
        let focus = update.unwrap().unwrap();
        assert!(focus.users.is_empty());
        assert_eq!(focus.topic, "这游戏");
        assert!(focus.until.saturating_duration_since(Instant::now()) <= Duration::from_secs(300));
    }

    /// 线上出过的一种漏判：模型把方括号写成尖括号、QQ 号写成字符串，
    /// 整行就原样发进了群（记录 id 78949）。两种写法都得当控制行处理。
    #[test]
    fn angle_brackets_and_string_ids_are_still_control_lines() {
        let raw = "<focus:{\"users\":[\"416012267\"],\"topic\":\"发言统计口径偏差、对错梗图\",\"seconds\":180}>";
        assert!(is_control(raw));
        let turns = [Turn {
            user_id: 416012267,
            name: "群友".into(),
            text: "hi".into(),
            message_id: 1,
            ..Turn::default()
        }];
        let (body, update) = extract(raw, &turns, 300);
        assert_eq!(body, "");
        let focus = update.unwrap().unwrap();
        assert_eq!(focus.users, vec![416012267]);
        assert_eq!(focus.topic, "发言统计口径偏差、对错梗图");
        // 混用括号（`[focus:…>`）也认，别让它从缝里漏出去。
        assert!(is_control("[focus:{\"seconds\":0}>"));
        assert_eq!(extract("[focus:{\"seconds\":0}>", &[], 300).0, "");
    }

    #[test]
    fn malformed_control_lines_do_not_leak_and_leaving_is_explicit() {
        let (body, update) = extract("- [focus:broken]\n接着说", &[], 300);
        assert_eq!(body, "接着说");
        assert!(update.is_none());
        assert!(matches!(
            extract("[focus:{\"seconds\":0}]", &[], 300).1,
            Some(None)
        ));
        assert!(matches!(
            extract("[focus:{\"topic\":\"x\",\"seconds\":30}]", &[], 0).1,
            Some(None)
        ));
        assert!(extract("随便看看", &[], 300).1.is_none());
    }
}
