//! 规则本子：一条规则 = 几个词（或正则）+ 怎么比 + 用哪种动作 + 在哪些群。
//!
//! 规则是内容、不是配置：群主管理员在群里随手加一条、改一次，写的是
//! `data/keyword_guard/rules.json`（[`BOOK`]）。`config.toml` 里只放口径——默认动作、
//! 默认禁言多久、提示怎么发——不放一条条词。这样群管改词不会重写带注释的配置文件，
//! 也不会碰到别的群的设置。删掉的规则进回收站（[`Book::trash`]），留最近
//! [`TRASH_KEEP`] 条，「违禁词 恢复」找得回来。
//!
//! 比对前两边都先 `fold`：全角转半角、不分大小写、去掉空白标点与零宽字符。于是
//! 「加 微 信」「加，微，信」「Ｖ信」与「加微信」是同一个样子；正则则对 `plain` 过的
//! 原文比（保留空白与标点，只统一宽度与大小写），作者想写 `加\\s*微信` 也照常。

use super::LOG_TARGET;
use crate::storage::JsonState;
use crate::text::{fold, plain};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// 所有群的规则合计上限：再多每条消息都要比一遍。
pub const MAX_RULES: usize = 500;
/// 一条规则最多几个词。
pub const MAX_WORDS: usize = 50;
/// 一个词最长多少字。
pub const MAX_WORD_CHARS: usize = 80;
/// 回收站留几条。
pub const TRASH_KEEP: usize = 20;
/// 禁言时长的范围：平台最多 30 天。
pub const MUTE_MIN: u64 = 60;
pub const MUTE_MAX: u64 = 30 * 86400;
/// 提示最长留多久；0 表示永久保留，不走这个上限。
pub const NOTICE_MAX_SECONDS: u64 = 7 * 86400;

/// 检测到之后做什么。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// 只发提示，不动消息、不禁言。
    #[default]
    Alert,
    /// 撤回消息。
    Recall,
    /// 禁言，消息留着。
    Mute,
    /// 撤回并禁言。
    RecallMute,
}

impl Action {
    /// 从指令里的说法认动作。
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_lowercase().as_str() {
            "提醒" | "仅提醒" | "只提醒" | "alert" => Some(Self::Alert),
            "撤回" | "撤" | "recall" => Some(Self::Recall),
            "禁言" | "mute" => Some(Self::Mute),
            "撤回禁言" | "撤回并禁言" | "撤回+禁言" | "撤禁" | "recall_mute" | "recallmute" => {
                Some(Self::RecallMute)
            }
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Alert => "仅提醒",
            Self::Recall => "撤回",
            Self::Mute => "禁言",
            Self::RecallMute => "撤回并禁言",
        }
    }

    pub fn recalls(self) -> bool {
        matches!(self, Self::Recall | Self::RecallMute)
    }

    pub fn mutes(self) -> bool {
        matches!(self, Self::Mute | Self::RecallMute)
    }
}

/// 怎么比。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// 归一化之后句子里含有这个词。
    #[default]
    Contains,
    /// 正则表达式，对原文比（不分大小写、全角转半角）。
    Regex,
}

impl Mode {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_lowercase().as_str() {
            "包含" | "含" | "contains" => Some(Self::Contains),
            "正则" | "regex" => Some(Self::Regex),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Contains => "包含",
            Self::Regex => "正则",
        }
    }
}

/// 一条规则。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Rule {
    pub id: u32,
    pub enabled: bool,
    /// 词或正则；正则模式下每个词都是一条正则。
    pub words: Vec<String>,
    pub mode: Mode,
    pub action: Action,
    /// 这条规则的禁言时长；`None` 跟随配置里的默认值。
    pub mute_seconds: Option<u64>,
    /// 只在这些群生效；空 = 所有群。
    pub groups: Vec<String>,
    /// 多个词要全部出现才算对上（只对包含模式有意义）。默认任一出现就算。
    pub require_all: bool,
    /// 谁加的（账号；控制台加的是「控制台」）。
    pub author: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// 对上过几次、最近一次是什么时候（Unix 秒）。
    pub hits: u64,
    pub last_hit: i64,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            id: 0,
            enabled: true,
            words: Vec::new(),
            mode: Mode::Contains,
            action: Action::Recall,
            mute_seconds: None,
            groups: Vec::new(),
            require_all: false,
            author: String::new(),
            created_at: 0,
            updated_at: 0,
            hits: 0,
            last_hit: 0,
        }
    }
}

impl Rule {
    /// 这条规则管不管这个群（不看开关）。
    pub fn covers(&self, group: &str) -> bool {
        self.groups.is_empty() || self.groups.iter().any(|g| g == group)
    }

    /// 这条规则是不是「本群自己的」：只写了一个群号，且就是这里。
    /// 多群共用的与全局的由号主维护，群里只读。
    pub fn belongs_to(&self, group: &str) -> bool {
        self.groups.len() == 1 && self.groups[0] == group
    }

    /// 这条规则是不是全局的（所有群）。
    pub fn global(&self) -> bool {
        self.groups.is_empty()
    }

    /// 这句话对上了吗；对上时返回对上的那个词（原样，便于提示与排错）。
    pub fn hit(&self, raw: &str, normalized: &str, folded: &str) -> Option<String> {
        if !self.enabled {
            return None;
        }
        self.hit_unchecked(raw, normalized, folded)
    }

    /// 不看开关，只看词对不对上。用于「试一句」解释「对上了但停用着」。
    pub fn hit_unchecked(&self, raw: &str, normalized: &str, folded: &str) -> Option<String> {
        if self.words.is_empty() {
            return None;
        }
        let mut first = None;
        for word in &self.words {
            let matched = match self.mode {
                Mode::Contains => {
                    let key = fold(word);
                    // 一个字的词也照「一条消息里到处都有」处理，手改 rules.json 也不能绕过。
                    key.chars().count() >= 2 && folded.contains(&key)
                }
                // 连空串都对得上的正则等于见谁都处理；手改出来的也跳过。
                Mode::Regex => regex(word)
                    .filter(|re| !re.is_match(""))
                    .is_some_and(|re| re.is_match(raw) || re.is_match(normalized)),
            };
            if matched {
                if first.is_none() {
                    first = Some(word.clone());
                }
                if !self.require_all {
                    break;
                }
            } else if self.require_all {
                return None;
            }
        }
        first
    }

    /// 存之前查一遍：哪里不行就说哪里。`default_mute` 是配置里的默认禁言时长。
    pub fn problem(&self, default_mute: u64) -> Option<String> {
        let words: Vec<&String> = self.words.iter().filter(|w| !w.trim().is_empty()).collect();
        if words.is_empty() {
            return Some("至少写一个词".into());
        }
        if words.len() > MAX_WORDS {
            return Some(format!("一条规则最多 {MAX_WORDS} 个词"));
        }
        for word in &words {
            if word.chars().count() > MAX_WORD_CHARS {
                return Some(format!(
                    "「{}」太长了，最多 {MAX_WORD_CHARS} 个字",
                    crate::storage::truncate_chars(word, 12)
                ));
            }
            match self.mode {
                Mode::Regex => {
                    if let Err(error) = build_regex(word) {
                        return Some(format!(
                            "正则「{word}」写得不对：{}",
                            first_line(&error.to_string())
                        ));
                    }
                    // 连空串都能对上的正则等于「见什么处理什么」。
                    if regex(word).is_some_and(|re| re.is_match("")) {
                        return Some(format!("正则「{word}」连空消息都对得上，等于见谁都处理"));
                    }
                }
                Mode::Contains => {
                    if fold(word).chars().count() < 2 {
                        return Some(format!(
                            "「{word}」太短了：包含匹配至少两个字，免得一个字就到处触发；要按正则精确比请改成模式「正则」"
                        ));
                    }
                }
            }
        }
        for id in &self.groups {
            if id.trim().is_empty() || id.len() > 128 || id.chars().any(char::is_whitespace) {
                return Some(format!("「{id}」不像一个群号"));
            }
        }
        if let Some(seconds) = self.mute_seconds
            && !(MUTE_MIN..=MUTE_MAX).contains(&seconds)
        {
            return Some(format!(
                "禁言时长要在 {} 到 {} 之间（0 表示跟随默认）",
                crate::clock::duration_text(MUTE_MIN),
                crate::clock::duration_text(MUTE_MAX)
            ));
        }
        if self.action.mutes() && self.mute_seconds.unwrap_or(default_mute) == 0 {
            return Some("选了禁言，却没有可用的禁言时长".into());
        }
        None
    }

    /// 存之前收拾一遍：去掉空词与重复词、规整正则两边的斜杠、群号去重。
    pub fn tidy(&mut self) {
        for word in &mut self.words {
            *word = word.trim().to_string();
            if self.mode == Mode::Regex
                && let Some(inner) = word.strip_prefix('/').and_then(|w| w.strip_suffix('/'))
                && !inner.trim().is_empty()
            {
                *word = inner.to_string();
            }
        }
        let mut seen = Vec::new();
        self.words.retain(|word| {
            if word.is_empty() {
                return false;
            }
            let key = match self.mode {
                Mode::Contains => fold(word),
                Mode::Regex => word.clone(),
            };
            let keep = !key.is_empty() && !seen.contains(&key);
            seen.push(key);
            keep
        });
        let mut groups = Vec::new();
        for id in &self.groups {
            let id = id.trim().to_string();
            if !id.is_empty() && !groups.contains(&id) {
                groups.push(id);
            }
        }
        self.groups = groups;
    }

    /// 列表里的一句话：`加微信、加V → 撤回并禁言 10 分钟（包含）`
    pub fn summary(&self, default_mute: u64) -> String {
        let words = self
            .words
            .iter()
            .map(|word| match self.mode {
                Mode::Regex => format!("/{word}/"),
                Mode::Contains => word.clone(),
            })
            .collect::<Vec<_>>()
            .join("、");
        let mut line = format!("{words} → {}", self.action.label());
        if self.action.mutes() {
            line.push_str(&format!(
                " {}",
                crate::clock::duration_text(self.mute_seconds.unwrap_or(default_mute))
            ));
        }
        line.push_str(&format!("（{}", self.mode.label()));
        if self.require_all && self.words.len() > 1 {
            line.push_str("，全部词都出现");
        }
        line.push('）');
        if self.hits > 0 {
            line.push_str(&format!(" · 命中 {} 次", self.hits));
        }
        if !self.enabled {
            line.push_str("（停用）");
        }
        line
    }
}

/// 一份规则本子。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Book {
    pub next_id: u32,
    pub rules: Vec<Rule>,
    pub trash: Vec<Rule>,
}

impl Book {
    /// 加一条，返回它的编号；太多时返回原因。
    pub fn add(&mut self, mut rule: Rule) -> Result<u32, String> {
        if self.rules.len() >= MAX_RULES {
            return Err(format!("规则太多了，最多 {MAX_RULES} 条；先删掉一些没用的"));
        }
        let highest = self.rules.iter().map(|r| r.id).max().unwrap_or(0);
        let id = self.next_id.max(highest + 1).max(1);
        self.next_id = id + 1;
        rule.id = id;
        self.rules.push(rule);
        Ok(id)
    }

    /// 按编号删一条，进回收站。
    pub fn remove(&mut self, id: u32) -> Option<Rule> {
        let at = self.rules.iter().position(|rule| rule.id == id)?;
        let rule = self.rules.remove(at);
        self.trash.push(rule.clone());
        if self.trash.len() > TRASH_KEEP {
            let excess = self.trash.len() - TRASH_KEEP;
            self.trash.drain(0..excess);
        }
        Some(rule)
    }

    /// 从回收站找最近一条符合范围（全局，或恰好属于这个群）的规则放回来。
    pub fn restore(&mut self, matches: impl Fn(&Rule) -> bool) -> Option<Rule> {
        if self.rules.len() >= MAX_RULES {
            return None;
        }
        let at = self.trash.iter().rposition(matches)?;
        let rule = self.trash.remove(at);
        self.rules.push(rule.clone());
        Some(rule)
    }

    /// 按编号找一条。
    pub fn get(&self, id: u32) -> Option<&Rule> {
        self.rules.iter().find(|rule| rule.id == id)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut Rule> {
        self.rules.iter_mut().find(|rule| rule.id == id)
    }

    /// 这个群里真正生效的规则，顺序即「谁先对上算谁」：
    /// 先本群自己的，再限定几个群的，最后全局的；同类按创建先后。
    pub fn effective(&self, group: &str) -> Vec<&Rule> {
        let mut rules: Vec<&Rule> = self
            .rules
            .iter()
            .filter(|rule| rule.covers(group))
            .collect();
        rules.sort_by_key(|rule| (rule.groups.is_empty(), rule.groups.len(), rule.id));
        rules
    }

    /// 这个群里能改的规则（恰好只属于本群），按创建先后。
    pub fn local(&self, group: &str) -> Vec<&Rule> {
        let mut rules: Vec<&Rule> = self
            .rules
            .iter()
            .filter(|rule| rule.belongs_to(group))
            .collect();
        rules.sort_by_key(|rule| rule.id);
        rules
    }

    /// 全局规则，按创建先后。
    pub fn global(&self) -> Vec<&Rule> {
        let mut rules: Vec<&Rule> = self.rules.iter().filter(|rule| rule.global()).collect();
        rules.sort_by_key(|rule| rule.id);
        rules
    }

    /// 记一次命中：一样的规则被删了就不再记。
    pub fn count_hit(&mut self, id: u32, at: i64) -> bool {
        let Some(rule) = self.get_mut(id) else {
            return false;
        };
        rule.hits = rule.hits.saturating_add(1);
        rule.last_hit = at;
        true
    }
}

/// 全进程共用的一份规则。
pub static BOOK: JsonState<Book> =
    JsonState::new(LOG_TARGET, "keyword_guard", "rules.json", "违禁词规则");

/// 编好的正则记着：每条消息都要比一遍，不能每次现编。写错的记成 `None`，不反复报。
pub fn regex(pattern: &str) -> Option<Regex> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Regex>>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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

fn build_regex(pattern: &str) -> Result<Regex, regex::Error> {
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(1 << 20)
        .dfa_size_limit(1 << 20)
        .build()
}

fn first_line(text: &str) -> String {
    text.lines()
        .rfind(|line| !line.trim().is_empty())
        .unwrap_or(text)
        .trim()
        .to_string()
}

/// 正则与包含匹配都需要的三份文本。
pub struct Text<'a> {
    pub raw: &'a str,
    pub normalized: String,
    pub folded: String,
}

impl<'a> Text<'a> {
    pub fn new(raw: &'a str) -> Self {
        let raw = raw.trim();
        Self {
            raw,
            normalized: plain(raw),
            folded: fold(raw),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.folded.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(mode: Mode, words: &[&str], action: Action) -> Rule {
        Rule {
            mode,
            action,
            words: words.iter().map(|w| (*w).to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn folding_matches_evasive_spacing_and_width() {
        let text = Text::new("请 加，微 信：abc");
        let rule = rule(Mode::Contains, &["加微信"], Action::Recall);
        assert_eq!(
            rule.hit(text.raw, &text.normalized, &text.folded)
                .as_deref(),
            Some("加微信")
        );
        assert!(text.folded.contains("加微信"));
    }

    #[test]
    fn regex_matches_original_and_normalized_text() {
        let pattern = rule(Mode::Regex, &[r"加\s*微\s*信"], Action::RecallMute);
        let text = Text::new("加 微 信 详聊");
        assert!(
            pattern
                .hit(text.raw, &text.normalized, &text.folded)
                .is_some()
        );
        let full = Text::new("ＶＸ：abc");
        let vx = rule(Mode::Regex, &["vx[:：]"], Action::Recall);
        assert!(
            vx.hit(full.raw, &full.normalized, &full.folded).is_some(),
            "全角要转半角"
        );
    }

    #[test]
    fn require_all_needs_every_word() {
        let mut rule = rule(Mode::Contains, &["兼职", "日结"], Action::Mute);
        rule.require_all = true;
        let only = Text::new("招兼职");
        assert!(rule.hit(only.raw, &only.normalized, &only.folded).is_none());
        let both = Text::new("招兼职，日结");
        assert!(rule.hit(both.raw, &both.normalized, &both.folded).is_some());
    }

    #[test]
    fn problems_are_caught_before_saving() {
        assert!(
            rule(Mode::Contains, &["加"], Action::Recall)
                .problem(600)
                .is_some()
        );
        assert!(
            rule(Mode::Contains, &["加微信"], Action::Mute)
                .problem(0)
                .is_some()
        );
        assert!(
            rule(Mode::Contains, &["广告"], Action::Mute)
                .problem(600)
                .is_none()
        );
        assert!(
            rule(Mode::Regex, &["("], Action::Recall)
                .problem(600)
                .is_some()
        );
        assert!(
            rule(Mode::Regex, &["a?"], Action::Recall)
                .problem(600)
                .is_some()
        );
        assert!(
            rule(Mode::Regex, &[r"加\s*微信"], Action::Recall)
                .problem(600)
                .is_none()
        );
    }

    #[test]
    fn tidy_strips_regex_slashes_and_dedupes() {
        let mut rule = rule(Mode::Regex, &["/加\\s*微信/", "加\\s*微信"], Action::Recall);
        rule.tidy();
        assert_eq!(rule.words, ["加\\s*微信"]);
    }

    #[test]
    fn deleted_rules_can_come_back_with_the_same_id() {
        let mut book = Book::default();
        let id = book
            .add(rule(Mode::Contains, &["加微信"], Action::Recall))
            .unwrap();
        assert_eq!(id, 1);
        let removed = book.remove(id).unwrap();
        assert_eq!(removed.id, 1);
        assert!(book.rules.is_empty() && book.trash.len() == 1);
        let restored = book.restore(Rule::global).unwrap();
        assert_eq!(restored.id, 1);
        assert_eq!(book.rules.len(), 1);
        assert!(book.trash.is_empty());
    }

    #[test]
    fn effective_puts_local_first_then_global() {
        let mut book = Book::default();
        let global = book
            .add(Rule {
                groups: Vec::new(),
                ..rule(Mode::Contains, &["全局"], Action::Alert)
            })
            .unwrap();
        let local = book
            .add(Rule {
                groups: vec!["1".into()],
                ..rule(Mode::Contains, &["本群"], Action::Recall)
            })
            .unwrap();
        let order: Vec<&str> = book
            .effective("1")
            .iter()
            .map(|r| r.words[0].as_str())
            .collect();
        assert_eq!(order, ["本群", "全局"]);
        assert!(book.effective("2").iter().all(|r| r.global()));
        assert_eq!((local, global), (2, 1));
    }
}
