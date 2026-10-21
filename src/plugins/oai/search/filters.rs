//! 一次搜索的请求与过滤条件：站点、日期、语种、要不要正文。
//!
//! 模型在工具参数里用结构化字段表达这些意图（`site`、`after`、`lang`……），
//! 而不是自己拼搜索语法：字段有 schema 可校验，写错了当场给出可读的错误，
//! 并且换到别的后端时还能各按各的办法落实。DeepSeek 官方接口的搜索语法
//! （`site:` / `-site:` / `after:` / `before:` / `lang:`）由 [`Filters::deepseek_query`] 编出来。

use super::Recency;
use chrono::{Duration, NaiveDate};
use serde_json::Value;

/// DeepSeek 搜索词的字符上限；操作符也算在里面，超了接口直接 400。
const QUERY_CHARS: usize = 128;

/// 触发搜索的原始问题的字符上限（接口要求 1～128）。
const QUESTION_CHARS: usize = 128;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Filters {
    pub recency: Option<Recency>,
    /// 只看这些站点（小写、不带协议与路径）。
    pub sites: Vec<String>,
    pub exclude_sites: Vec<String>,
    pub after: Option<NaiveDate>,
    pub before: Option<NaiveDate>,
    /// 模型明确要的语种代码（`zh`、`en`、`ja`……）。
    pub lang: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SearchRequest {
    pub query: String,
    pub limit: Option<usize>,
    pub filters: Filters,
    /// 要不要连网页正文一起带回来（省一次 `web_fetch`）。
    pub content: bool,
    /// 触发这次搜索的原始问题，帮后端弄清搜索意图。
    pub question: Option<String>,
}

impl SearchRequest {
    /// 从工具参数读出请求；写错的参数在这里就报错，不扣联网额度。
    pub(crate) fn from_args(args: &Value) -> anyhow::Result<Self> {
        let query = args["query"].as_str().unwrap_or("").trim().to_string();
        if query.is_empty() {
            anyhow::bail!("参数错误：query 不能为空");
        }
        let after = date_arg(args, "after")?;
        let before = date_arg(args, "before")?;
        if let (Some(after), Some(before)) = (after, before)
            && after >= before
        {
            anyhow::bail!("参数错误：after（{after}）要早于 before（{before}）");
        }
        Ok(Self {
            query,
            limit: args["limit"].as_u64().map(|value| value as usize),
            filters: Filters {
                recency: args["recency"].as_str().and_then(Recency::parse),
                sites: words(&args["site"]).iter().filter_map(|s| domain(s)).collect(),
                exclude_sites: words(&args["exclude_site"])
                    .iter()
                    .filter_map(|s| domain(s))
                    .collect(),
                after,
                before,
                lang: words(&args["lang"])
                    .into_iter()
                    .map(|code| code.to_ascii_lowercase())
                    .filter(|code| {
                        (2..=8).contains(&code.len())
                            && code.chars().all(|c| c.is_ascii_alphabetic() || c == '-')
                    })
                    .collect(),
            },
            content: args["content"].as_bool().unwrap_or(false),
            question: args["question"]
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(|text| text.chars().take(QUESTION_CHARS).collect()),
        })
    }
}

/// 搜索词里有汉字或假名：中文语境的查询。
pub(crate) fn is_cjk_query(query: &str) -> bool {
    query.chars().any(super::is_cjk)
}

impl Recency {
    pub(crate) fn days(self) -> i64 {
        match self {
            Self::Day => 1,
            Self::Week => 7,
            Self::Month => 30,
            Self::Year => 365,
        }
    }
}

impl Filters {
    /// 有没有任何一项显式的限定（不含中文查询的默认语种）。
    pub(crate) fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// 起始日期：显式的 `after` 优先，其次由 `recency` 折算。
    pub(crate) fn effective_after(&self, today: NaiveDate) -> Option<NaiveDate> {
        self.after
            .or_else(|| self.recency.map(|window| today - Duration::days(window.days())))
    }

    /// 只会按「最近一段」限定时间的后端用：把起始日期折回最接近的窗口。
    pub(crate) fn approx_recency(&self, today: NaiveDate) -> Option<Recency> {
        if self.recency.is_some() {
            return self.recency;
        }
        let days = (today - self.after?).num_days();
        Some(match days {
            ..=1 => Recency::Day,
            2..=7 => Recency::Week,
            8..=31 => Recency::Month,
            _ => Recency::Year,
        })
    }

    /// 编成 DeepSeek 的搜索语法，连同被截短与否一起返回。
    ///
    /// 总长度（含操作符）不超过 128 个字符：先留足操作符的位置，搜索词本体超出的
    /// 部分从词边界处截掉。`default_lang` 是「没指定语种的中文查询」默认带的 `zh,en`，
    /// 它只是软偏好，零结果时调用方会去掉它再搜一次。
    pub(crate) fn deepseek_query(
        &self,
        query: &str,
        today: NaiveDate,
        default_lang: bool,
    ) -> (String, bool) {
        let mut ops: Vec<String> = Vec::new();
        if !self.sites.is_empty() {
            ops.push(
                self.sites
                    .iter()
                    .map(|site| format!("site:{site}"))
                    .collect::<Vec<_>>()
                    .join(" OR "),
            );
        }
        ops.extend(self.exclude_sites.iter().map(|site| format!("-site:{site}")));
        if let Some(after) = self.effective_after(today) {
            ops.push(format!("after:{after}"));
        }
        if let Some(before) = self.before {
            ops.push(format!("before:{before}"));
        }
        if !self.lang.is_empty() {
            ops.push(format!("lang:{}", self.lang.join(",")));
        } else if default_lang && is_cjk_query(query) {
            ops.push("lang:zh,en".to_string());
        }
        let suffix = ops.join(" ");
        let room = QUERY_CHARS.saturating_sub(suffix.chars().count() + 1).max(8);
        let (text, cut) = clip_at_word(query, room);
        let joined = if suffix.is_empty() {
            text
        } else {
            format!("{text} {suffix}")
        };
        (joined, cut)
    }

    /// 没有 `site:` 语法的后端用：站点限定直接拼进搜索词。
    pub(crate) fn with_site_operators(&self, query: &str) -> String {
        let mut out = query.to_string();
        if !self.sites.is_empty() {
            let sites: Vec<String> = self.sites.iter().map(|site| format!("site:{site}")).collect();
            out.push_str(&format!(" {}", sites.join(" OR ")));
        }
        for site in &self.exclude_sites {
            out.push_str(&format!(" -site:{site}"));
        }
        out
    }

    /// 这个后端落实不了的显式限定，给模型一句交代（它得知道结果没被这样过滤）。
    pub(crate) fn unsupported_by(&self, provider: &str) -> Option<String> {
        let site_ok = matches!(provider, "bing" | "duckduckgo" | "brave" | "serper" | "searxng");
        let mut missing = Vec::new();
        if !site_ok && (!self.sites.is_empty() || !self.exclude_sites.is_empty()) {
            missing.push("站点限定");
        }
        if self.after.is_some() || self.before.is_some() {
            missing.push("精确日期");
        }
        if !self.lang.is_empty() {
            missing.push("语种");
        }
        (!missing.is_empty()).then(|| {
            format!(
                "（{provider} 不支持{}，这次的结果没按它过滤，请自行看日期与来源）",
                missing.join("、")
            )
        })
    }
}

/// 截到 `limit` 个字符内；超出时尽量停在最后一个空白上（至少保留一半）。
fn clip_at_word(text: &str, limit: usize) -> (String, bool) {
    let text = text.trim();
    if text.chars().count() <= limit {
        return (text.to_string(), false);
    }
    let cut: String = text.chars().take(limit).collect();
    let keep = cut
        .rfind(char::is_whitespace)
        .filter(|&position| cut[..position].chars().count() >= limit / 2)
        .map_or(cut.as_str(), |position| &cut[..position]);
    (keep.trim_end().to_string(), true)
}

/// 工具参数里的「一串词」：字符串（空白、逗号、顿号分隔）或字符串数组。
fn words(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |text: &str| {
        out.extend(
            text.split(|c: char| c.is_whitespace() || matches!(c, ',' | '，' | '、' | ';' | '；'))
                .map(str::trim)
                .filter(|word| !word.is_empty())
                .map(str::to_string),
        );
    };
    match value {
        Value::String(text) => push(text),
        Value::Array(items) => items.iter().filter_map(Value::as_str).for_each(&mut push),
        _ => {}
    }
    out
}

/// 站点写法归一：去掉协议、路径、端口与 `www.`，只认像域名的东西。
fn domain(raw: &str) -> Option<String> {
    let lower = raw.trim().to_lowercase();
    let host = lower
        .split_once("://")
        .map_or(lower.as_str(), |(_, rest)| rest)
        .split(['/', '?', '#'])
        .next()?
        .rsplit('@')
        .next()?
        .split(':')
        .next()?
        .trim_start_matches("www.")
        .trim_matches('.');
    let valid = host.contains('.')
        && host
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '.'))
        && !host.chars().all(|c| c.is_ascii_digit() || c == '.');
    valid.then(|| host.to_string())
}

fn date_arg(args: &Value, key: &str) -> anyhow::Result<Option<NaiveDate>> {
    let Some(raw) = args[key].as_str().map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(None);
    };
    parse_date(raw).map(Some).ok_or_else(|| {
        anyhow::anyhow!("参数错误：{key} 写成 2026、2026-10 或 2026-10-01 这样，收到「{raw}」")
    })
}

/// `2026`、`2026-10`、`2026-10-01`（`/`、`.`、年月日 也认）；缺的月、日按 1 算。
pub(crate) fn parse_date(raw: &str) -> Option<NaiveDate> {
    let normalized: String = raw
        .chars()
        .map(|c| match c {
            '年' | '月' | '/' | '.' => '-',
            '日' | '号' => ' ',
            other => other,
        })
        .collect();
    let mut parts = normalized.trim().split('-').map(str::trim);
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next().map_or(Some(1), |part| part.parse().ok())?;
    let day: u32 = parts.next().map_or(Some(1), |part| part.parse().ok())?;
    if parts.next().is_some() || !(1990..=2100).contains(&year) {
        return None;
    }
    NaiveDate::from_ymd_opt(year, month, day)
}
