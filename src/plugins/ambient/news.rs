//! 见闻：这两天「刷到过」的新闻，眼前聊到了才想起来。
//!
//! 发言模型的常识停在训练那会儿。群里聊「Anthropic 上市」「奥特曼说 AI 是泡沫」这种
//! 这几天的新事，它要么接不上，要么拿旧印象硬接；每次都去联网搜又太贵、太慢，而且
//! 随口接一句的事本来就不值得查。
//!
//! 可这台机器人自己就在往群里推 AI 资讯（[`crate::plugins::ai_news`]）：推过的每一条
//! 标题和摘要都记在 `data/ai_news/state.json` 里。一个真人群友也是这样知道新闻的——
//! 刷手机时扫到过。所以这里把最近三天推过的条目当作「刷到过的」，按和眼前话题沾不沾边
//! 挑最多三条放进提示词。只读本地文件，不调模型、不联网；文件没变就不重读。
//!
//! 沾边的判定照 [`super::recall`] 的思路从简：眼前几句的实词与条目的实词重合，多条
//! 条目里都有的泛词（「AI」「模型」「发布」）不算数，英文专名（`Anthropic`、`GPT`）记双份。

use crate::plugins::oai::chat::tone;
use crate::plugins::oai::chat::window::Turn;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// 多久以内推过的算「这两天刷到过」。
const FRESH_SECONDS: i64 = 72 * 3600;
/// 一轮最多想起几条。
const MAX_ITEMS: usize = 3;
/// 看最近几句在聊什么。
const QUERY_TURNS: usize = 10;
/// 至少重合这么多分才算沾边（英文专名一个顶两分）。
const MIN_SCORE: usize = 3;
/// 在这么大比例的条目里都出现的词是泛词，不算。
const COMMON_SHARE: f32 = 0.2;

/// 一条见闻。
#[derive(Clone)]
struct Item {
    at: i64,
    title: String,
    summary: String,
    words: HashSet<String>,
}

#[derive(Default)]
struct Cache {
    modified: Option<std::time::SystemTime>,
    items: Vec<Item>,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache { modified: None, items: Vec::new() });

/// 推送卡片里的一条 → 标题与摘要。
///
/// 两种版式：实时推送是「1. 标题」，日榜是「第 1 名 标题」；第二行是来源与时间，第三行是摘要。
fn parse_entry(entry: &str) -> Option<(String, String)> {
    let mut lines = entry.lines().map(str::trim);
    let head = lines.next()?;
    let title = if let Some(rest) = head.strip_prefix('第') {
        rest.split_once('名').map_or(head, |(_, title)| title).trim()
    } else {
        head.split_once(". ").filter(|(index, _)| index.chars().all(|c| c.is_ascii_digit())).map_or(head, |(_, title)| title).trim()
    };
    let summary = lines
        .skip(1)
        .find(|line| !line.is_empty() && !line.starts_with(['💡', '🔗', '📄']) && !line.starts_with("最新进展"))
        .unwrap_or("");
    (!title.is_empty()).then(|| (title.to_string(), summary.to_string()))
}

fn words_of(text: &str) -> HashSet<String> {
    tone::content_words(text)
}

/// 读资讯插件的状态文件（变了才重读），返回最近三天的条目。
fn load(now: i64) -> Vec<Item> {
    let Some(path) = crate::storage::data_path("ai_news").ok().map(|dir| dir.join("state.json")) else {
        return Vec::new();
    };
    let modified = std::fs::metadata(&path).and_then(|meta| meta.modified()).ok();
    let mut cache = CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if modified.is_some() && cache.modified == modified {
        return cache.items.iter().filter(|item| now - item.at < FRESH_SECONDS).cloned().collect();
    }
    let mut items: Vec<Item> = Vec::new();
    if let Some(records) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| value.get("extractions").and_then(|records| records.as_array()).cloned())
    {
        for record in records {
            let at = record["created_ts"].as_i64().unwrap_or(0);
            if now - at >= FRESH_SECONDS {
                continue;
            }
            for entry in record["rendered"]["entries"].as_array().into_iter().flatten().filter_map(|entry| entry.as_str()) {
                let Some((title, summary)) = parse_entry(entry) else { continue };
                let words = words_of(&format!("{title}\n{summary}"));
                // 同一件事推过好几回（实时一条、日榜又一条）：只留最新的那份。
                let title_grams = tone::grams(&title);
                if let Some(same) = items.iter_mut().find(|item| overlap(&tone::grams(&item.title), &title_grams) >= 0.5) {
                    if at > same.at {
                        *same = Item { at, title, summary, words };
                    }
                    continue;
                }
                items.push(Item { at, title, summary, words });
            }
        }
    }
    cache.modified = modified;
    cache.items = items.clone();
    items
}

fn overlap(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    a.intersection(b).count() as f32 / a.len().min(b.len()) as f32
}

/// 挑出和眼前几句沾边的条目。
fn related(items: &[Item], turns: &[Turn]) -> Vec<Item> {
    if items.is_empty() {
        return Vec::new();
    }
    // 只看别人说的：自己刚说过的话天然沾边，拿它当话题就会越说越远（见 `tone::spoken_by_others`）。
    let topic = words_of(&tone::spoken_by_others(turns, QUERY_TURNS));
    if topic.is_empty() {
        return Vec::new();
    }
    let mut frequency: HashMap<&str, usize> = HashMap::new();
    for item in items {
        for word in &item.words {
            *frequency.entry(word.as_str()).or_default() += 1;
        }
    }
    let common = ((items.len() as f32 * COMMON_SHARE).ceil() as usize).max(3);
    let mut scored: Vec<(usize, &Item)> = items
        .iter()
        .map(|item| {
            let score = item
                .words
                .iter()
                .filter(|word| topic.contains(*word) && frequency.get(word.as_str()).copied().unwrap_or(0) < common)
                .map(|word| if word.is_ascii() && word.len() >= 3 { 2 } else { 1 })
                .sum::<usize>();
            (score, item)
        })
        .filter(|(score, _)| *score >= MIN_SCORE)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.at.cmp(&a.1.at)));
    scored.into_iter().take(MAX_ITEMS).map(|(_, item)| item.clone()).collect()
}

/// 摆进发言提示词的一段；没有沾边的就是空串。
pub(super) fn brief(turns: &[Turn]) -> String {
    let now = chrono::Local::now().timestamp();
    let found = related(&load(now), turns);
    if found.is_empty() {
        return String::new();
    }
    let lines: String = found
        .iter()
        .map(|item| {
            let when = chrono::DateTime::from_timestamp(item.at, 0)
                .map(|at| at.with_timezone(&chrono::Local).format("%m-%d").to_string())
                .unwrap_or_default();
            let summary: String = item.summary.chars().take(90).collect();
            if summary.is_empty() {
                format!("- {when} {}\n", item.title)
            } else {
                format!("- {when} {}：{summary}\n", item.title)
            }
        })
        .collect();
    format!(
        "这两天你刷手机时扫到过这几条，跟眼前聊的沾边（当成自己知道的事自然用上就行，别像念新闻，\
         也别说「我刷到」「据报道」；细节记不清就别编，要较真再查）：\n{lines}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(text: &str) -> Turn {
        Turn { text: text.to_string(), ..Default::default() }
    }

    fn item(title: &str, summary: &str) -> Item {
        Item { at: 0, title: title.into(), summary: summary.into(), words: words_of(&format!("{title}\n{summary}")) }
    }

    #[test]
    fn both_card_layouts_parse() {
        let (title, summary) = parse_entry("1. Anthropic 提交 S-1\n   X：Anthropic · 行业 · 10-26 发布\n   公司推进 IPO。\n   🔗 https://x").unwrap();
        assert_eq!((title.as_str(), summary.as_str()), ("Anthropic 提交 S-1", "公司推进 IPO。"));
        let (title, _) = parse_entry("第 2 名 债务市场收紧\n   1 个报道来源\n   摘要").unwrap();
        assert_eq!(title, "债务市场收紧");
    }

    #[test]
    fn only_items_touching_the_topic_come_back() {
        let items = vec![
            item("Anthropic 向 SEC 提交 S-1，拟以 ANT 登陆纳斯达克", "Anthropic 推进 IPO 上市"),
            item("谷歌发布新款手机", "像素系列新机上市"),
            item("某模型刷新榜单", "模型能力提升"),
        ];
        let found = related(&items, &[turn("听说 Anthropic 要上市了？"), turn("纳斯达克那个")]);
        assert_eq!(found.len(), 1);
        assert!(found[0].title.starts_with("Anthropic"));
        assert!(related(&items, &[turn("今天好冷啊")]).is_empty());
    }
}
