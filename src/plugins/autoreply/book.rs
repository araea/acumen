//! 规则本子：一条规则 = 几个触发词 + 怎么比 + 几种回复 + 在哪些群回。
//!
//! 规则是内容、不是配置：群里管理员随手加一条、改一句，控制台里也能改，两边写的是同一份
//! `data/autoreply/rules.json`（[`BOOK`]）。`config.toml` 里只放「怎么回」的口径（冷却、
//! 限流、包含匹配的字数上限），不放一条条问答。删掉的规则进回收站（[`Book::trash`]），
//! 留最近 [`TRASH_KEEP`] 条，群里「自动回复 恢复」或控制台的「撤销」都能找回来。

use super::LOG_TARGET;
use crate::storage::JsonState;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// 规则总数上限：再多就不是「常见问题」了，比对也慢。
pub const MAX_RULES: usize = 500;
/// 回收站留几条。
pub const TRASH_KEEP: usize = 20;
/// 一条规则最多几个触发词、几种回复。
pub const MAX_KEYWORDS: usize = 20;
pub const MAX_REPLIES: usize = 10;
/// 触发词、回复的长度上限（字）。
pub const MAX_KEYWORD_CHARS: usize = 60;
pub const MAX_REPLY_CHARS: usize = 4000;
/// 一条合并转发最多几条。
pub const MAX_NODES: usize = 100;

/// 怎么比：消息与触发词都先 [`fold`]（去空白与标点、全角转半角、不分大小写）再比，正则除外。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// 整句一样才回：最不会误触。
    Exact,
    /// 句子里含有触发词就回；消息太长不算（见 `contains_max_chars`）。
    #[default]
    Contains,
    /// 以触发词开头就回。
    Prefix,
    /// 正则表达式，对原文比（不分大小写）。
    Regex,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Exact, Mode::Contains, Mode::Prefix, Mode::Regex];

    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_lowercase().as_str() {
            "精确" | "完全" | "全句" | "整句" | "exact" => Some(Self::Exact),
            "包含" | "含" | "模糊" | "contains" => Some(Self::Contains),
            "开头" | "前缀" | "prefix" => Some(Self::Prefix),
            "正则" | "regex" => Some(Self::Regex),
            _ => None,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Contains => "contains",
            Self::Prefix => "prefix",
            Self::Regex => "regex",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Exact => "精确",
            Self::Contains => "包含",
            Self::Prefix => "开头",
            Self::Regex => "正则",
        }
    }

    /// 一句话说清楚什么样的消息会触发。
    pub fn explain(self, max_chars: u32) -> String {
        match self {
            Self::Exact => "消息去掉空格和标点后与触发词一模一样才回".into(),
            Self::Contains if max_chars > 0 => format!("{max_chars} 字以内的消息里含有触发词就回"),
            Self::Contains => "消息里含有触发词就回".into(),
            Self::Prefix => "消息以触发词开头就回".into(),
            Self::Regex => "消息原文匹配这个正则表达式就回（不分大小写）".into(),
        }
    }

    /// 几条规则同时对上时谁先：越精确越先。
    fn rank(self) -> u8 {
        match self {
            Self::Exact => 0,
            Self::Prefix => 1,
            Self::Contains => 2,
            Self::Regex => 3,
        }
    }
}

/// 合并转发里的一条。`text` 与普通回复同一套写法（`{image:…}`、`{face:…}`）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Node {
    pub name: String,
    pub user_id: String,
    pub text: String,
}

/// 一种回复：一段文字（可以夹图与表情），或一整条合并转发。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum Reply {
    Forward { forward: Vec<Node> },
    Text { text: String },
}

impl Reply {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    fn blank(&self) -> bool {
        match self {
            Self::Text { text } => text.trim().is_empty(),
            Self::Forward { forward } => forward.iter().all(|node| node.text.trim().is_empty()),
        }
    }

    /// 列表与提示里的一句话说法。
    pub fn brief(&self, max: usize) -> String {
        match self {
            Self::Forward { forward } => format!("〔合并转发 · {} 条〕", forward.len()),
            Self::Text { text } => {
                let mut plain = String::new();
                let mut rest = text.as_str();
                while let Some(open) = rest.find("{image:")
                    && let Some(close) = rest[open..].find('}')
                {
                    plain.push_str(&rest[..open]);
                    plain.push_str("〔图〕");
                    rest = &rest[open + close + 1..];
                }
                plain.push_str(rest);
                let line = plain.split_whitespace().collect::<Vec<_>>().join(" ");
                crate::storage::truncate_chars(&line, max)
            }
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Rule {
    pub id: u32,
    pub enabled: bool,
    /// 触发词，任一对上就算。
    pub keywords: Vec<String>,
    pub mode: Mode,
    /// 几种回复，每次随机一种。
    pub replies: Vec<Reply>,
    /// 只在这些群回；空 = 所有群。
    pub groups: Vec<String>,
    /// 这些群不回：所有群共用的规则在某个群停用时记在这里。
    pub exclude: Vec<String>,
    /// 私聊里也回（只看所有群共用的规则）。
    pub private: bool,
    /// 只有 @ 机器人时才回。
    pub at_only: bool,
    /// 折成一条合并转发发出去，长内容不刷屏；文字里单独一行的 `---` 分成几条。
    pub forward: bool,
    /// 这条规则自己的冷却（秒）；没写就照全局。
    pub cooldown_seconds: Option<u64>,
    /// 谁加的（账号；控制台加的是「控制台」）。
    pub author: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// 回过几次、最近一次是什么时候（Unix 秒）。
    pub hits: u64,
    pub last_hit: i64,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            id: 0,
            enabled: true,
            keywords: Vec::new(),
            mode: Mode::default(),
            replies: Vec::new(),
            groups: Vec::new(),
            exclude: Vec::new(),
            private: false,
            at_only: false,
            forward: false,
            cooldown_seconds: None,
            author: String::new(),
            created_at: 0,
            updated_at: 0,
            hits: 0,
            last_hit: 0,
        }
    }
}

/// 一条消息，比对之前先整理好的样子。
pub struct Asked<'a> {
    /// 原文（去了首尾空白）：正则对它比。
    pub raw: &'a str,
    /// [`fold`] 过的：其余方式对它比。
    pub folded: String,
    pub at_bot: bool,
    /// 群号；私聊为 `None`。
    pub group: Option<&'a str>,
}

impl<'a> Asked<'a> {
    pub fn new(raw: &'a str, at_bot: bool, group: Option<&'a str>) -> Self {
        let raw = raw.trim();
        Self { raw, folded: fold(raw), at_bot, group }
    }
}

impl Rule {
    /// 这条规则管不管这个地方（不看开关）。
    pub fn covers(&self, group: Option<&str>) -> bool {
        match group {
            Some(group) => (self.groups.is_empty() || self.groups.iter().any(|g| g == group)) && !self.exclude.iter().any(|g| g == group),
            None => self.private && self.groups.is_empty(),
        }
    }

    /// 只在某几个群用的规则（区别于所有群共用的）。
    pub fn scoped(&self) -> bool {
        !self.groups.is_empty()
    }

    /// 对上了哪个触发词；对不上是 `None`。`max_chars` 是包含匹配的消息字数上限（0 = 不限）。
    pub fn hit(&self, asked: &Asked, max_chars: u32) -> Option<&str> {
        if self.at_only && !asked.at_bot {
            return None;
        }
        self.keywords.iter().map(String::as_str).find(|keyword| {
            let key = fold(keyword);
            match self.mode {
                Mode::Regex => regex(keyword).is_some_and(|re| re.is_match(asked.raw)),
                // 全是标点的触发词（「？」「。。。」）折完是空的：那就比原文。
                _ if key.is_empty() => {
                    let key = keyword.trim();
                    !key.is_empty() && asked.raw == key
                }
                Mode::Exact => asked.folded == key,
                Mode::Prefix => asked.folded.starts_with(&key),
                Mode::Contains => {
                    (max_chars == 0 || asked.folded.chars().count() <= max_chars as usize) && asked.folded.contains(&key)
                }
            }
        })
    }

    /// 存之前查一遍：哪里不行就说哪里。
    pub fn problem(&self) -> Option<String> {
        let keywords: Vec<&String> = self.keywords.iter().filter(|k| !k.trim().is_empty()).collect();
        if keywords.is_empty() {
            return Some("至少写一个触发词".into());
        }
        if keywords.len() > MAX_KEYWORDS {
            return Some(format!("触发词最多 {MAX_KEYWORDS} 个"));
        }
        for keyword in &keywords {
            if keyword.chars().count() > MAX_KEYWORD_CHARS {
                return Some(format!("触发词「{}」太长了，最多 {MAX_KEYWORD_CHARS} 个字", crate::storage::truncate_chars(keyword, 12)));
            }
            match self.mode {
                Mode::Regex => {
                    if let Err(error) = build_regex(keyword) {
                        return Some(format!("正则「{keyword}」写得不对：{}", first_line(&error.to_string())));
                    }
                    // 能匹配空串的正则见什么都回，等于没有触发词。
                    if regex(keyword).is_some_and(|re| re.is_match("")) {
                        return Some(format!("正则「{keyword}」连空消息都能对上，会见什么回什么"));
                    }
                }
                Mode::Contains if fold(keyword).chars().count() < 2 => {
                    return Some(format!("「{keyword}」太短：包含匹配的触发词至少两个字，免得一个字就到处触发；要一个字的就改成「精确」"));
                }
                _ => {}
            }
        }
        let replies: Vec<&Reply> = self.replies.iter().filter(|r| !r.blank()).collect();
        if replies.is_empty() {
            return Some("至少写一种回复".into());
        }
        if replies.len() > MAX_REPLIES {
            return Some(format!("回复最多 {MAX_REPLIES} 种"));
        }
        for reply in &replies {
            match reply {
                Reply::Text { text } if text.chars().count() > MAX_REPLY_CHARS => {
                    return Some(format!("有一种回复太长了，最多 {MAX_REPLY_CHARS} 字"));
                }
                Reply::Forward { forward } if forward.len() > MAX_NODES => {
                    return Some(format!("合并转发最多 {MAX_NODES} 条"));
                }
                _ => {}
            }
        }
        for id in self.groups.iter().chain(&self.exclude) {
            if id.trim().is_empty() || id.len() > 128 || id.chars().any(char::is_whitespace) {
                return Some(format!("「{id}」不像一个群号"));
            }
        }
        if self.cooldown_seconds.is_some_and(|s| s > 86400) {
            return Some("冷却最多 86400 秒（一天）".into());
        }
        None
    }

    /// 存之前收拾一遍：去掉空的触发词与回复、重复的群号。
    pub fn tidy(&mut self) {
        let mut seen = Vec::new();
        self.keywords.retain(|k| {
            let key = (fold(k), k.trim().to_string());
            let keep = !k.trim().is_empty() && !seen.contains(&key);
            seen.push(key);
            keep
        });
        for keyword in &mut self.keywords {
            *keyword = keyword.trim().to_string();
        }
        self.replies.retain(|r| !r.blank());
        for list in [&mut self.groups, &mut self.exclude] {
            let mut seen = Vec::new();
            list.retain(|id| {
                let id = id.trim().to_string();
                let keep = !id.is_empty() && !seen.contains(&id);
                seen.push(id);
                keep
            });
        }
        if !self.groups.is_empty() {
            // 只在几个群用的规则不需要「这些群不回」。
            self.exclude.clear();
            self.private = false;
        }
    }

    /// 触发词的一句话说法：「下载」「安装包」。
    pub fn title(&self) -> String {
        let shown: Vec<String> = self.keywords.iter().take(3).map(|k| format!("「{k}」")).collect();
        let more = self.keywords.len().saturating_sub(3);
        if more > 0 { format!("{} 等 {} 个", shown.join(""), self.keywords.len()) } else { shown.join("") }
    }
}

/// 几条规则都对上时挑哪一条：越精确越先，只在本群用的先于所有群共用的，触发词长的先于短的，
/// 再按编号。返回规则下标与对上的触发词。
pub fn best<'r>(rules: &'r [Rule], asked: &Asked, max_chars: u32) -> Option<(&'r Rule, String)> {
    rules
        .iter()
        .filter(|rule| rule.enabled && rule.covers(asked.group))
        .filter_map(|rule| rule.hit(asked, max_chars).map(|keyword| (rule, keyword.to_string())))
        .min_by_key(|(rule, keyword)| (rule.mode.rank(), !rule.scoped(), std::cmp::Reverse(keyword.chars().count()), rule.id))
}

/// 比对用的写法：全角转半角、不分大小写，去掉空白与标点（「怎么下载？」与「怎么 下载」是一句话）。
pub fn fold(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            _ => c,
        })
        .filter(|c| !c.is_whitespace() && !punctuation(*c))
        .flat_map(char::to_lowercase)
        .collect()
}

fn punctuation(c: char) -> bool {
    c.is_ascii_punctuation()
        || matches!(
            c,
            '，' | '。' | '、' | '；' | '：' | '？' | '！' | '“' | '”' | '‘' | '’' | '（' | '）' | '【' | '】' | '《' | '》'
                | '〈' | '〉' | '「' | '」' | '『' | '』' | '…' | '—' | '～' | '·' | '〜' | '・' | '｡' | '､'
        )
}

fn first_line(text: &str) -> String {
    text.lines().rfind(|line| !line.trim().is_empty()).unwrap_or(text).trim().to_string()
}

fn build_regex(pattern: &str) -> Result<Regex, regex::Error> {
    RegexBuilder::new(pattern).case_insensitive(true).size_limit(1 << 20).dfa_size_limit(1 << 20).build()
}

/// 编好的正则记着：每条消息都要比一遍，不能每次现编。写错的记成 `None`，不反复报。
fn regex(pattern: &str) -> Option<Regex> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Regex>>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache.len() > 512 {
        cache.clear();
    }
    cache
        .entry(pattern.to_string())
        .or_insert_with(|| {
            build_regex(pattern)
                .inspect_err(|error| debug!(target: LOG_TARGET, "正则「{pattern}」编不了：{error}"))
                .ok()
        })
        .clone()
}

// ==================== 本子 ====================

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Book {
    pub next_id: u32,
    pub rules: Vec<Rule>,
    /// 删掉的，最近的在后面。
    pub trash: Vec<Trashed>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Trashed {
    pub rule: Rule,
    pub at: i64,
    /// 在哪个群删的（控制台删的为空）：群里「恢复」只找本群删掉的。
    pub group: String,
}

pub static BOOK: JsonState<Book> = JsonState::new(LOG_TARGET, "autoreply", "rules.json", "自动回复规则");

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl Book {
    pub fn get(&self, id: u32) -> Option<&Rule> {
        self.rules.iter().find(|rule| rule.id == id)
    }

    fn allot(&mut self) -> u32 {
        let top = self.rules.iter().chain(self.trash.iter().map(|t| &t.rule)).map(|r| r.id).max().unwrap_or(0);
        self.next_id = self.next_id.max(top) + 1;
        self.next_id
    }

    /// 加一条新的，给它编号。
    pub fn insert(&mut self, mut rule: Rule) -> Result<Rule, String> {
        rule.tidy();
        if let Some(problem) = rule.problem() {
            return Err(problem);
        }
        if self.rules.len() >= MAX_RULES {
            return Err(format!("规则已经有 {MAX_RULES} 条了，删掉几条不用的再加"));
        }
        rule.id = self.allot();
        let stamp = now();
        rule.created_at = stamp;
        rule.updated_at = stamp;
        self.rules.push(rule.clone());
        Ok(rule)
    }

    /// 用改好的一份替换原来那条；命中次数与作者不归调用方管，照旧留着。
    pub fn replace(&mut self, mut rule: Rule) -> Result<Rule, String> {
        rule.tidy();
        if let Some(problem) = rule.problem() {
            return Err(problem);
        }
        let Some(slot) = self.rules.iter_mut().find(|r| r.id == rule.id) else {
            return Err(format!("没有 #{} 这条规则（可能刚被删了）", rule.id));
        };
        rule.author = slot.author.clone();
        rule.created_at = slot.created_at;
        rule.hits = slot.hits;
        rule.last_hit = slot.last_hit;
        rule.updated_at = now();
        *slot = rule.clone();
        Ok(rule)
    }

    /// 删掉一条，挪进回收站。
    pub fn remove(&mut self, id: u32, group: &str) -> Option<Rule> {
        let index = self.rules.iter().position(|r| r.id == id)?;
        let rule = self.rules.remove(index);
        self.trash.push(Trashed { rule: rule.clone(), at: now(), group: group.to_string() });
        let overflow = self.trash.len().saturating_sub(TRASH_KEEP);
        self.trash.drain(..overflow);
        Some(rule)
    }

    /// 从回收站拿回来。`id` 为空时拿最近删的那条（`group` 不空时只找在这个群删的）。
    pub fn restore(&mut self, id: Option<u32>, group: Option<&str>) -> Option<Rule> {
        let index = self.trash.iter().rposition(|t| id.is_none_or(|id| t.rule.id == id) && group.is_none_or(|g| t.group == g))?;
        let mut rule = self.trash.remove(index).rule;
        if self.rules.iter().any(|r| r.id == rule.id) {
            rule.id = self.allot();
        }
        self.rules.push(rule.clone());
        self.rules.sort_by_key(|r| r.id);
        Some(rule)
    }

    /// 这个群里（或私聊里）看得到的规则：本群专用的在前，再是共用的，各按编号。
    pub fn visible(&self, group: Option<&str>) -> Vec<&Rule> {
        let mut list: Vec<&Rule> = self
            .rules
            .iter()
            .filter(|rule| match group {
                Some(g) => rule.groups.is_empty() || rule.groups.iter().any(|x| x == g),
                None => rule.groups.is_empty(),
            })
            .collect();
        list.sort_by_key(|rule| (!rule.scoped(), rule.id));
        list
    }

    /// 同一处、同一种比法、触发词一样的规则：新加的回复并进去当另一种说法，不另起一条。
    pub fn twin(&self, keywords: &[String], mode: Mode, groups: &[String]) -> Option<u32> {
        let keys: Vec<String> = keywords.iter().map(|k| fold(k)).collect();
        self.rules
            .iter()
            .find(|rule| {
                rule.mode == mode
                    && rule.groups == groups
                    && rule.keywords.len() == keys.len()
                    && rule.keywords.iter().all(|k| keys.contains(&fold(k)))
            })
            .map(|rule| rule.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(mode: Mode, keywords: &[&str]) -> Rule {
        Rule {
            id: 1,
            mode,
            keywords: keywords.iter().map(ToString::to_string).collect(),
            replies: vec![Reply::text("好")],
            ..Default::default()
        }
    }

    fn ask(text: &str) -> Asked<'_> {
        Asked::new(text, false, Some("100"))
    }

    #[test]
    fn folding_ignores_spaces_punctuation_and_width() {
        assert_eq!(fold("怎么 下载？？"), "怎么下载");
        assert_eq!(fold("ＱＱ群！"), "qq群");
        assert_eq!(fold("  Hello, World. "), "helloworld");
    }

    #[test]
    fn each_mode_matches_what_it_says() {
        let exact = rule(Mode::Exact, &["怎么下载"]);
        assert!(exact.hit(&ask("怎么下载？"), 40).is_some());
        assert!(exact.hit(&ask("请问怎么下载"), 40).is_none());

        let contains = rule(Mode::Contains, &["下载"]);
        assert!(contains.hit(&ask("请问在哪下载啊"), 40).is_some());
        let long = "我昨天在家里下载了一个很大的游戏然后玩了一整晚第二天上班迟到了被老板骂了一顿真是太倒霉了";
        assert!(contains.hit(&ask(long), 40).is_none(), "长消息顺口提到不算");
        assert!(contains.hit(&ask(long), 0).is_some(), "0 = 不限字数");

        let prefix = rule(Mode::Prefix, &["教程"]);
        assert!(prefix.hit(&ask("教程在哪"), 40).is_some());
        assert!(prefix.hit(&ask("有没有教程"), 40).is_none());

        let re = rule(Mode::Regex, &[r"^(在吗|有人吗)[?？]*$"]);
        assert!(re.hit(&ask("在吗？"), 40).is_some());
        assert!(re.hit(&ask("你在吗"), 40).is_none());
    }

    #[test]
    fn punctuation_only_keyword_compares_raw_text() {
        let q = rule(Mode::Exact, &["？"]);
        assert!(q.hit(&ask("？"), 40).is_some());
        assert!(q.hit(&ask("？？"), 40).is_none());
    }

    #[test]
    fn at_only_rules_wait_for_a_mention() {
        let mut r = rule(Mode::Exact, &["菜单"]);
        r.at_only = true;
        assert!(r.hit(&ask("菜单"), 40).is_none());
        assert!(r.hit(&Asked::new("菜单", true, Some("1")), 40).is_some());
    }

    #[test]
    fn scope_and_exclusions() {
        let mut r = rule(Mode::Exact, &["x"]);
        assert!(r.covers(Some("1")) && !r.covers(None));
        r.exclude = vec!["1".into()];
        assert!(!r.covers(Some("1")) && r.covers(Some("2")));
        r.groups = vec!["2".into()];
        r.tidy();
        assert!(r.exclude.is_empty() && !r.covers(Some("3")));
        r.groups.clear();
        r.private = true;
        assert!(r.covers(None));
    }

    #[test]
    fn best_prefers_exact_then_local_then_longer() {
        let mut a = rule(Mode::Contains, &["下载"]);
        a.id = 1;
        let mut b = rule(Mode::Contains, &["下载地址"]);
        b.id = 2;
        let mut c = rule(Mode::Exact, &["下载地址"]);
        c.id = 3;
        c.groups = vec!["999".into()];
        let rules = vec![a.clone(), b.clone(), c];
        assert_eq!(best(&rules, &ask("下载地址"), 40).unwrap().0.id, 2, "别的群专用的不算；长的先");
        let mut d = rule(Mode::Contains, &["下载"]);
        d.id = 4;
        d.groups = vec!["100".into()];
        assert_eq!(best(&[a, d], &ask("哪里下载"), 40).unwrap().0.id, 4, "本群专用的先于共用的");
    }

    #[test]
    fn problems_are_reported_in_plain_words() {
        assert!(rule(Mode::Contains, &["下"]).problem().unwrap().contains("太短"));
        assert!(rule(Mode::Exact, &["下"]).problem().is_none());
        assert!(rule(Mode::Regex, &["("]).problem().unwrap().contains("写得不对"));
        assert!(rule(Mode::Regex, &[".*"]).problem().unwrap().contains("空消息"));
        let mut empty = rule(Mode::Exact, &["x"]);
        empty.replies = vec![Reply::text("  ")];
        assert!(empty.problem().unwrap().contains("回复"));
    }

    #[test]
    fn trash_and_restore_keep_ids() {
        let mut book = Book::default();
        let first = book.insert(rule(Mode::Exact, &["a"])).unwrap();
        let second = book.insert(rule(Mode::Exact, &["b"])).unwrap();
        assert_eq!((first.id, second.id), (1, 2));
        book.remove(1, "100");
        assert!(book.restore(None, Some("200")).is_none(), "别的群删的不归这个群恢复");
        assert_eq!(book.restore(None, Some("100")).unwrap().id, 1);
        assert_eq!(book.insert(rule(Mode::Exact, &["c"])).unwrap().id, 3);
    }

    #[test]
    fn reply_shapes_round_trip() {
        let text: Reply = serde_json::from_str(r#"{"text":"你好{image:a.png}"}"#).unwrap();
        assert_eq!(text.brief(20), "你好〔图〕");
        let forward: Reply = serde_json::from_str(r#"{"forward":[{"name":"甲","user_id":"1","text":"一"}]}"#).unwrap();
        assert!(matches!(forward, Reply::Forward { .. }));
        assert_eq!(serde_json::to_string(&forward).unwrap(), r#"{"forward":[{"name":"甲","user_id":"1","text":"一"}]}"#);
    }
}
