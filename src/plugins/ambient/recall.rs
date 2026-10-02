//! 话题回忆：眼前聊到什么，就把号主以前就这件事亲手说过的话翻出来。
//!
//! 样本库（[`super::voice`]）教的是「怎么说」，所以刻意避开眼前的话题——贴题的样本会被
//! 模型当成此刻要表的态（2026-09-26 的「什么时候华为上双击熄屏我就回归」）。这一层反
//! 过来：它教的是「说过什么」。同一个人前天刚在另一个群说「豆包不如从前」，今天有人
//! 聊起豆包，说出口的话就不该是另一个立场；群里的老梗他怎么叫、怎么接，也只有翻他自己
//! 的话才知道。
//!
//! 语料是号主最近两个月在所有群里亲手打的话（见 [`super::recent`]），按「眼前这几条
//! 消息里的实词」找：只认他自己话里不常出现的词——共有的是「游戏」「意思」这类满语料
//! 都有的字组不算，得是「大肥鱼」「反重力」「gpt6」这样的专名或梗才算话题沾边。找得到就
//! 摆最多三句，找不到什么都不摆：宁缺毋滥，一句不相干的旧话比没有更糟。
//!
//! 2026-10-02 拿真实群聊回放量过：不分场合地「找类似的话他怎么回」（按整句相似度）命中
//! 的几乎全是无关的话——群聊的回话靠的是前后几条的来龙去脉，不是上一句的字面；
//! 按实词找「他说过什么」才有用，所以这里只做这一种。

use super::voice::content_words;
use super::window::Turn;
use std::collections::HashMap;

/// 拿最近几条消息当话题。再往前的多半已经聊到别处去了。
const QUERY_TURNS: usize = 8;
/// 一个词在号主的话里出现超过这么多句就不当话题词：满语料都有的词说明不了什么。
const MAX_DF: usize = 40;
/// 稀有度是 `ln(句数 / 出现句数)`，满分（只出现一句）是 `ln(句数)`；下面几条线都按满分的
/// 比例给，语料从几千句长到几万句也不用重调。语料约一万句时分别是 6.4 / 8.0 / 5.0 / 11。
///
/// 单个词稀有到这个比例才算专名；英文数字词只要到这条线。
const RARE: f32 = 0.70;
/// 稀有到这个比例，一个词就够。
const VERY_RARE: f32 = 0.87;
/// 相邻两个字组拼成三个字（「大肥」「肥鱼」→ 大肥鱼）时，每个字组至少要有的比例。
const PAIR: f32 = 0.54;
/// 所有共有词的稀有度加起来要到满分的这么多倍。
const MIN_SUM: f32 = 1.2;
/// 同一个群里说的更贴近这次的听众：分数乘这个数。
const SAME_GROUP: f32 = 1.25;
/// 旧话的新旧：一个月前的算六成多一点，不至于被新的压得翻不出来。
const HALF_LIFE_DAYS: f32 = 30.0;
/// 本群二十分钟内说的话窗口里本来就有，不翻。
const FRESH_SECONDS: i64 = 20 * 60;
/// 一次最多摆几句、一共多少字。
const SLOTS: usize = 3;
const BUDGET_CHARS: usize = 100;

/// 号主说过的一句话。
#[derive(Debug, Clone)]
pub(super) struct Entry {
    pub group: String,
    pub at: i64,
    pub text: String,
}

/// 他最近说过的话，按实词建好的倒排索引。
#[derive(Default)]
pub(super) struct Corpus {
    entries: Vec<Entry>,
    index: HashMap<String, Vec<u32>>,
}

impl Corpus {
    /// 一条一条收进来：`(群号, 时刻, 正文)`，相同的话只留最新的一条。
    pub(super) fn build(lines: impl IntoIterator<Item = (String, i64, String)>) -> Self {
        let mut latest: HashMap<String, (String, i64)> = HashMap::new();
        for (group, at, text) in lines {
            match latest.get(&text) {
                Some((_, seen)) if *seen >= at => {}
                _ => {
                    latest.insert(text, (group, at));
                }
            }
        }
        let mut entries: Vec<Entry> = latest
            .into_iter()
            .map(|(text, (group, at))| Entry { group, at, text })
            .collect();
        entries.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.text.cmp(&b.text)));
        let mut index: HashMap<String, Vec<u32>> = HashMap::new();
        for (position, entry) in entries.iter().enumerate() {
            for word in content_words(&entry.text) {
                index.entry(word).or_default().push(position as u32);
            }
        }
        Self { entries, index }
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    /// 眼前这段聊天沾边的、他自己说过的话（最多三句，分数高的在前）。
    pub(super) fn related(&self, group: &str, now: i64, turns: &[Turn]) -> Vec<&Entry> {
        if self.entries.is_empty() {
            return Vec::new();
        }
        let visible = turns
            .iter()
            .map(|turn| turn.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let topic: String = turns
            .iter()
            .rev()
            .take(QUERY_TURNS)
            .map(|turn| strip_markers(&turn.text))
            .collect::<Vec<_>>()
            .join("\n");
        let total = self.entries.len() as f32;
        let ceiling = (total + 1.0).ln();
        let mut shared: HashMap<u32, Vec<(String, f32)>> = HashMap::new();
        for word in content_words(&topic) {
            let Some(posting) = self.index.get(&word) else {
                continue;
            };
            if posting.len() > MAX_DF {
                continue;
            }
            let rarity = ((total + 1.0) / (posting.len() as f32 + 1.0)).ln();
            for &position in posting {
                shared
                    .entry(position)
                    .or_default()
                    .push((word.clone(), rarity));
            }
        }
        let mut scored: Vec<(f32, &Entry, String)> = Vec::new();
        for (position, words) in shared {
            let entry = &self.entries[position as usize];
            // 窗口里本来就看得见的，不当「旧话」再翻一遍。
            if visible.contains(entry.text.as_str())
                || (entry.group == group && now - entry.at < FRESH_SECONDS)
            {
                continue;
            }
            let sum: f32 = words.iter().map(|(_, rarity)| rarity).sum();
            if sum < MIN_SUM * ceiling || !is_topic(&words, ceiling) {
                continue;
            }
            let age = (now - entry.at).max(0) as f32 / 86_400.0;
            let freshness = 0.6 + 0.4 * 0.5_f32.powf(age / HALF_LIFE_DAYS);
            let bonus = if entry.group == group { SAME_GROUP } else { 1.0 };
            let key = words
                .iter()
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(word, _)| word.clone())
                .unwrap_or_default();
            scored.push((sum * bonus * freshness, entry, key));
        }
        scored.sort_by(|a, b| {
            b.0.total_cmp(&a.0)
                .then_with(|| b.1.at.cmp(&a.1.at))
                .then_with(|| a.1.text.cmp(&b.1.text))
        });
        // 同一个词只摆一句：三句都在说「大肥鱼」，等于只给了一条信息。
        let mut keys: Vec<String> = Vec::new();
        let mut used = 0;
        let mut chosen = Vec::new();
        for (_, entry, key) in scored {
            if chosen.len() >= SLOTS {
                break;
            }
            let chars = entry.text.chars().count();
            if keys.contains(&key) || used + chars > BUDGET_CHARS {
                continue;
            }
            used += chars;
            keys.push(key);
            chosen.push(entry);
        }
        chosen
    }
}

/// 共有的这些词够不够说「聊的是同一件事」。
///
/// 一个英文数字词（`gpt6`、`isTrusted`）稀有就够；一个极稀有的词也够；中文常常只有
/// 「相邻两个字组拼起来」才站得住——「大肥」「肥鱼」同时出现，说明两边都有「大肥鱼」
/// 三个字，比单独一个二字词可靠得多。
fn is_topic(words: &[(String, f32)], ceiling: f32) -> bool {
    let ascii = |word: &str| word.is_ascii();
    if words.iter().any(|(word, rarity)| {
        (ascii(word) && *rarity >= RARE * ceiling) || *rarity >= VERY_RARE * ceiling
    }) {
        return true;
    }
    let pairs: Vec<&(String, f32)> = words
        .iter()
        .filter(|(word, rarity)| !ascii(word) && *rarity >= PAIR * ceiling)
        .collect();
    pairs.iter().any(|(a, _)| {
        pairs.iter().any(|(b, _)| {
            let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
            a.len() == 2 && b.len() == 2 && a[1] == b[0]
        })
    }) && words.iter().any(|(_, rarity)| *rarity >= RARE * ceiling)
}

/// 去掉记录里的占位符与标记（`[图片]`、`[@123]`、`〔@了你〕`）：它们不是在聊的东西。
fn strip_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '[' | '〔' => depth += 1,
            ']' | '〕' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// 摆进发言提示词的一段；没有翻到就是空串。
///
/// 口气是「参考」而不是「台词」：模型把样本当台词是这个功能最大的风险——
/// 把一句旧话原样再说一遍，比没有这一层更像机器。
pub(super) fn brief(found: &[&Entry]) -> String {
    if found.is_empty() {
        return String::new();
    }
    let lines: String = found
        .iter()
        .map(|entry| format!("- {}\n", entry.text))
        .collect();
    format!(
        "眼前聊到的这些，你以前自己说过类似的话（看法、叫法、梗保持前后一致就行，别原样搬\
         回来；没人提到就别主动翻旧账，也别说「我以前说过」）：\n{lines}"
    )
}
