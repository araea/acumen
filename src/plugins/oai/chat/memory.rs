//! 熟人记忆：跨重启记住群里的人和这个群里发生过的事。
//!
//! 滚动窗口（[`super::window`]）只有最近几十条消息，重启就没了——那是「刚才」，
//! 不是「认识」。真正让一个群友显得像常驻的人，是他记得你上次修的那台破电脑、
//! 记得这个群三天前开始玩的梗。这里就存这一点点东西：每个人一句印象、每个群
//! 几条旧事，落在磁盘上，随时间自然淡忘。
//!
//! 记忆只在两处产生：见到消息时自动更新的露面统计（不花钱），以及人格自己
//! 通过 `satori_memo` 写下的一句话（它自己决定什么值得记）。两者都不调用模型。

use super::window::Turn;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

/// 每个群最多记住多少人；超出时先忘掉没有印象、最久没露面的那些。
const MAX_PEOPLE: usize = 120;
/// 每个群最多记住多少条旧事。
const MAX_NOTES: usize = 24;
/// 旧事的保鲜期；过了就自然淡忘，免得半年前的梗还挂在嘴边。
const NOTE_TTL_DAYS: i64 = 45;
/// 自己说过的关于自己的话，最多留几条、留多久：说过一阵的「明天开始休」不该一直当真。
const MAX_CLAIMS: usize = 12;
const CLAIM_TTL_DAYS: i64 = 10;
/// 复盘的「几摊事」多久之内还拿来用；再久就是上一阵的事了。
const THREADS_FRESH_SECONDS: i64 = 8 * 3_600;
/// 复盘最多留几摊事。
pub(crate) const MAX_THREADS: usize = 5;
/// 一条印象或旧事的字数上限——记忆是提示，不是日记。
pub(crate) const MAX_NOTE_CHARS: usize = 60;
/// 一个人最多保留几条暂定观察；低证据、最久没印证的先淡忘。
const MAX_OBSERVATIONS: usize = 3;
/// 一次注入提示词的熟人卡片上限。
const BRIEF_PEOPLE: usize = 6;
/// 熟人卡片只给最近这些条消息里开过口的人：两小时前说过一句的人，今天这一茬早就不是他了，
/// 把他的印象摆在眼前，人格就会没头没脑地提起他。
const PEOPLE_TURNS: usize = 12;
/// 一次注入提示词的自述条数上限。
const BRIEF_CLAIMS: usize = 4;
/// 一次注入提示词的旧事条数上限。
const BRIEF_NOTES: usize = 3;
/// 这么新的旧事不必贴题也带上：刚起的梗、刚答过的事，眼前多半还用得着。
const FRESH_NOTE_SECONDS: i64 = 30 * 60;
/// 猜「眼前在聊什么」只看最近这些条。
const TOPIC_TURNS: usize = 12;
/// 两次落盘之间至少隔多久。露面统计每条消息都在变，值不上一次写盘；
/// 人格自己写下的印象走 [`flush_now`]，不受这个节流影响。
const WRITE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// 一条根据有限聊天样本形成的暂定观察。
/// 它与人格主动写下的长期印象分开保存：新证据可以强化它，也可能与它矛盾；
/// 不会因为一次复盘就把旧印象整体替换掉。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Observation {
    pub text: String,
    pub at: i64,
    /// 0–1，只表示证据强弱，不表示对这个人的好感或评价高低。
    pub confidence: f32,
    /// 形成这条观察时看到的消息样本数；多次复盘可以累积。
    pub samples: u32,
}

impl Observation {
    fn evidence(&self) -> String {
        let level = if self.confidence < 0.45 {
            "初步猜测"
        } else if self.confidence < 0.75 {
            "有些依据"
        } else {
            "多次印证"
        };
        format!("{level}，{} 条样本", self.samples)
    }
}

/// 对一个人的记忆。
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Person {
    /// 最近一次见到的群名片。
    #[serde(default)]
    pub name: String,
    /// 人格自己写的一句印象；空表示见过但没什么印象。
    #[serde(default)]
    pub note: String,
    /// 平时怎么称呼他；空表示跟着群名片叫。
    ///
    /// 群里一个人的名片、昵称和「你俩之间怎么叫」是三回事：号主会管某个群友叫
    /// 「大人」「兄弟」「坏猫」。记住这个称呼，开口时才是同一个人。
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub first_seen: i64,
    #[serde(default)]
    pub last_seen: i64,
    /// 见过他发的消息条数。
    #[serde(default)]
    pub messages: u32,
    /// 自己回应过他多少次。
    #[serde(default)]
    pub exchanges: u32,
    /// 复盘形成的暂定观察，最多保留几条；与上面的长期印象并列而非互相覆盖。
    #[serde(default)]
    pub observations: Vec<Observation>,
    /// 最近真实互动积累的热度。只由点名、回应等互动增加，不从一句发言推断。
    #[serde(default)]
    warmth: f32,
    #[serde(default)]
    warmth_at: i64,
}

impl Person {
    /// 刚冒头的新面孔：见过的话不多，第一次露面也没多久。
    fn stranger(&self, now: i64) -> bool {
        self.note.is_empty()
            && self.address.is_empty()
            && self.observations.is_empty()
            && self.messages < 6
            && now - self.first_seen < 3 * 86_400
    }

    /// 互动热度的半衰期为三天。发言本身不加热度，避免把「话多」误读成「亲近」。
    fn warmth(&self, now: i64) -> f32 {
        if self.warmth_at == 0 {
            return 0.0;
        }
        let days = (now - self.warmth_at).max(0) as f32 / 86_400.0;
        self.warmth * 0.5_f32.powf(days / 3.0)
    }

    fn engage(&mut self, delta: f32, at: i64) {
        self.warmth = (self.warmth(at) + delta).clamp(0.0, 1.0);
        self.warmth_at = at;
    }
}

/// 群里的一件旧事：梗、共同话题、约定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Note {
    pub text: String,
    pub at: i64,
}

/// 复盘出来的「这阵子群里在聊什么」：几摊事，整体替换，不累积。
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Threads {
    #[serde(default)]
    pub lines: Vec<String>,
    /// 复盘的时刻（Unix 秒）；0 表示还没复盘过。
    #[serde(default)]
    pub at: i64,
}

/// 一个群的全部长期记忆。
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(crate) struct GroupMemory {
    #[serde(default)]
    pub people: HashMap<String, Person>,
    #[serde(default)]
    pub notes: Vec<Note>,
    /// 最近一次复盘整理出的几摊事（见搭话那边的复盘）。
    #[serde(default)]
    pub threads: Threads,
    /// 自己在群里说过的、关于自己的事实与态度：前后要对得上。
    #[serde(default)]
    pub claims: Vec<Note>,
}

/// 把一句印象裁到能进提示词的长度，并压平换行。
fn tidy(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_NOTE_CHARS)
        .collect()
}

/// 复盘写进记忆的文字：压平、裁短，再把方括号换成全角——记忆会原样回到提示词里，
/// 群友在聊天里写的东西经过复盘落进来时，不能带着能冒充控制行的 `[silent]` 之类的标记，
/// 也不收看着像「给模型下令」的句子。
pub(crate) fn sanitize(text: &str) -> Option<String> {
    let flat = tidy(&text.replace('[', "（").replace(']', "）"));
    let lowered = flat.to_lowercase();
    let command = ["忽略以上", "忽略之前", "忽略上面", "系统提示", "ignore previous", "ignore all", "system prompt", "你必须", "你现在是"]
        .iter()
        .any(|needle| lowered.contains(needle));
    (!flat.is_empty() && !command).then_some(flat)
}

/// 「多久以前」的口语说法；记忆里的时间感比精确时刻有用。
pub(crate) fn ago(seconds: i64) -> String {
    match seconds {
        ..=59 => "刚刚".to_string(),
        60..=3_599 => format!("{} 分钟前", seconds / 60),
        3_600..=86_399 => format!("{} 小时前", seconds / 3_600),
        86_400..=2_591_999 => format!("{} 天前", seconds / 86_400),
        _ => "很久以前".to_string(),
    }
}

impl GroupMemory {
    /// 见到某人发了一条消息。只更新统计，不花任何模型开销。
    pub(crate) fn see(&mut self, user_id: &str, name: &str, at: i64) {
        if user_id.is_empty() {
            return;
        }
        let person = self
            .people
            .entry(user_id.to_string())
            .or_insert_with(|| Person {
            first_seen: at,
            ..Person::default()
        });
        if !name.trim().is_empty() {
            person.name = name.trim().chars().take(32).collect();
        }
        if person.first_seen == 0 {
            person.first_seen = at;
        }
        person.last_seen = person.last_seen.max(at);
        person.messages = person.messages.saturating_add(1);
    }

    /// 自己回应了某人一次。
    pub(crate) fn exchange(&mut self, user_id: &str, at: i64) {
        if let Some(person) = self.people.get_mut(user_id) {
            person.exchanges = person.exchanges.saturating_add(1);
            person.last_seen = person.last_seen.max(at);
            person.engage(0.06, at);
        }
    }

    /// 对方直接 @、引用或戳了自己。互动热度只来自这种真实往来，不从聊天内容猜。
    pub(crate) fn engage(&mut self, user_id: &str, at: i64) {
        if let Some(person) = self.people.get_mut(user_id) {
            person.engage(0.14, at);
        }
    }

    /// 写下（或改写）对一个人的印象；空字符串等于把印象抹掉但仍认得这个人。
    pub(crate) fn remember(&mut self, user_id: &str, note: &str) -> anyhow::Result<()> {
        anyhow::ensure!(!user_id.is_empty(), "用户 ID 是空的");
        let person = self.people.entry(user_id.to_string()).or_default();
        person.note = tidy(note);
        Ok(())
    }

    /// 增加一条有证据边界的观察。单次发言即使模型给了很高置信度，也只能
    /// 形成「初步猜测」；同一观察在后续复盘中反复出现才逐渐增强。
    pub(crate) fn observe(
        &mut self,
        user_id: &str,
        text: &str,
        confidence: f32,
        samples: usize,
        at: i64,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(!user_id.is_empty(), "用户 ID 是空的");
        // 观察会原样回到提示词里，所以和旧事一样过一遍注入清洗。
        let text = sanitize(text).ok_or_else(|| anyhow::anyhow!("观察是空的或不可用"))?;
        let samples = samples.clamp(1, 99) as u32;
        let sample_cap = (0.20 + 0.15 * samples as f32).min(0.85);
        let confidence = if confidence.is_finite() {
            confidence
        } else {
            0.2
        }
        .clamp(0.05, sample_cap);
        let person = self.people.entry(user_id.to_string()).or_default();
        if let Some(existing) = person
            .observations
            .iter_mut()
            .find(|item| item.text == text)
        {
            let total = existing.samples.saturating_add(samples).max(1);
            let old_weight = existing.samples as f32;
            let new_weight = samples as f32;
            existing.confidence = ((existing.confidence * old_weight + confidence * new_weight)
                / total as f32)
                .clamp(0.05, (0.20 + 0.15 * total as f32).min(0.95));
            existing.samples = total;
            existing.at = at;
            return Ok(true);
        }
        let candidate = Observation {
            text,
            at,
            confidence,
            samples,
        };
        if person.observations.len() >= MAX_OBSERVATIONS {
            let Some((index, weakest)) = person
                .observations
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    let score = |item: &Observation| {
                        item.confidence * (1.0 + (item.samples as f32).ln_1p())
                    };
                    score(a).total_cmp(&score(b)).then(a.at.cmp(&b.at))
                })
                .map(|(index, item)| (index, item.clone()))
            else {
                return Ok(false);
            };
            let new_score = candidate.confidence * (1.0 + (candidate.samples as f32).ln_1p());
            let old_score = weakest.confidence * (1.0 + (weakest.samples as f32).ln_1p());
            if new_score <= old_score {
                return Ok(false);
            }
            person.observations.remove(index);
        }
        person.observations.push(candidate);
        Ok(true)
    }

    /// 写下（或改写）平时怎么称呼他；空字符串等于回到跟着群名片叫。
    pub(crate) fn address(&mut self, user_id: &str, address: &str) -> anyhow::Result<()> {
        anyhow::ensure!(!user_id.is_empty(), "用户 ID 是空的");
        let person = self.people.entry(user_id.to_string()).or_default();
        person.address = tidy(address);
        Ok(())
    }

    /// 彻底忘掉一个人。
    pub(crate) fn forget(&mut self, user_id: &str) -> bool {
        self.people.remove(user_id).is_some()
    }

    /// 记下群里的一件事。重复的旧事只刷新时间，不堆成一摞。
    pub(crate) fn jot(&mut self, text: &str, at: i64) -> anyhow::Result<()> {
        let text = tidy(text);
        anyhow::ensure!(!text.is_empty(), "要记的内容是空的");
        if let Some(existing) = self.notes.iter_mut().find(|note| note.text == text) {
            existing.at = at;
            return Ok(());
        }
        self.notes.push(Note { text, at });
        Ok(())
    }

    /// 忘掉一件旧事；按内容前缀匹配，人格记不住下标。
    pub(crate) fn drop_note(&mut self, needle: &str) -> bool {
        let needle = tidy(needle);
        if needle.is_empty() {
            return false;
        }
        let before = self.notes.len();
        self.notes
            .retain(|note| !note.text.contains(needle.as_str()));
        self.notes.len() != before
    }

    /// 换上这一阵复盘出来的几摊事（整体替换）。写进去的都过了 [`sanitize`]；一条都没有时不动旧的。
    pub(crate) fn set_threads(&mut self, lines: &[String], at: i64) {
        let lines: Vec<String> = lines
            .iter()
            .filter_map(|line| sanitize(line))
            .take(MAX_THREADS)
            .collect();
        if !lines.is_empty() {
            self.threads = Threads { lines, at };
        }
    }

    /// 记下一条自己说过的、关于自己的话。重复的只刷新时间。
    pub(crate) fn claim(&mut self, text: &str, at: i64) -> bool {
        let Some(text) = sanitize(text) else {
            return false;
        };
        if let Some(existing) = self.claims.iter_mut().find(|claim| claim.text == text) {
            existing.at = at;
            return true;
        }
        self.claims.push(Note { text, at });
        true
    }

    /// 忘掉一条自己说过的话（说错了、不想再让它被当真）。
    pub(crate) fn drop_claim(&mut self, needle: &str) -> bool {
        let needle = tidy(needle);
        if needle.is_empty() {
            return false;
        }
        let before = self.claims.len();
        self.claims.retain(|claim| !claim.text.contains(needle.as_str()));
        self.claims.len() != before
    }

    /// 自己说过的、跟眼前话题沾边的话 → 发言提示词里的一段；没有就是空串。
    pub(crate) fn claims_brief(&self, turns: &[Turn], now: i64) -> String {
        // 只带跟眼前话题沾边的：从前不管聊什么都摆最近六条，「认为那款众筹游戏不值一千万」
        // 就在一句没人问起的话里被翻了出来（线上 2026-10-03 09:24「上次说不值一千万是我嘴硬了」），
        // 群友回了一句「笨笨的」。说过的话是为了在被问起、被提起时前后对得上，不是用来起话头的。
        let topic = super::tone::content_words(&super::tone::spoken_by_others(turns, TOPIC_TURNS));
        let mut live: Vec<&Note> = self
            .claims
            .iter()
            .filter(|claim| now - claim.at < CLAIM_TTL_DAYS * 86_400)
            .filter(|claim| super::tone::touches(&claim.text, &topic))
            .collect();
        live.sort_by_key(|claim| std::cmp::Reverse(claim.at));
        if live.is_empty() {
            return String::new();
        }
        let lines: Vec<String> = live
            .into_iter()
            .take(BRIEF_CLAIMS)
            .map(|claim| format!("- {}（{}）", claim.text, ago((now - claim.at).max(0))))
            .collect();
        format!(
            "眼前聊到的事，你以前在群里自己说过（前后要对得上，别说反；是记录，没人问起不必提，更别说「上次说过」）：\n{}\n",
            lines.join("\n")
        )
    }

    /// 淡忘：过期的旧事、太多的人。有印象的人比路人先留下。
    pub(crate) fn prune(&mut self, now: i64) {
        let ttl = NOTE_TTL_DAYS * 86_400;
        self.notes.retain(|note| now - note.at < ttl);
        for person in self.people.values_mut() {
            person.observations.retain(|item| now - item.at < ttl);
        }
        self.claims.retain(|claim| now - claim.at < CLAIM_TTL_DAYS * 86_400);
        if self.claims.len() > MAX_CLAIMS {
            self.claims.sort_by_key(|claim| claim.at);
            let excess = self.claims.len() - MAX_CLAIMS;
            self.claims.drain(..excess);
        }
        if self.notes.len() > MAX_NOTES {
            self.notes.sort_by_key(|note| note.at);
            let excess = self.notes.len() - MAX_NOTES;
            self.notes.drain(..excess);
        }
        if self.people.len() > MAX_PEOPLE {
            let mut ranked: Vec<(String, i64, bool)> = self
                .people
                .iter()
                .map(|(id, person)| {
                    (
                        id.clone(),
                        person.last_seen,
                        person.note.is_empty() && person.observations.is_empty(),
                    )
                })
                .collect();
            // 先淘汰没有印象的，再按最久没露面淘汰。
            ranked.sort_by(|a, b| b.2.cmp(&a.2).then(a.1.cmp(&b.1)));
            for (id, _, _) in ranked.into_iter().take(self.people.len() - MAX_PEOPLE) {
                self.people.remove(&id);
            }
        }
    }

    /// 当前这段聊天里出现的人 + 跟眼前沾边的几摊事与旧事 → 注入提示词的一段话。
    ///
    /// 只列眼前这些人：把整本通讯录倒进上下文既贵又没用，人也不是那样想事情的。
    /// 记忆是背景，不是话题——凡是不沾眼前这几句的，宁可不给：线上人格几次把没人提起的
    /// 旧事、一个钟头前的话头、两小时前才露过面的人，没头没脑地搬到眼前的话里，群友一
    /// 看就是「自说自话」。
    pub(crate) fn brief(&self, turns: &[Turn], now: i64) -> String {
        let mut seen: Vec<&str> = Vec::new();
        for turn in turns.iter().rev().take(PEOPLE_TURNS) {
            if turn.from_me || turn.user_id.is_empty() || seen.contains(&turn.user_id.as_str()) {
                continue;
            }
            seen.push(&turn.user_id);
            if seen.len() >= BRIEF_PEOPLE {
                break;
            }
        }
        let topic_text = super::tone::spoken_by_others(turns, TOPIC_TURNS);
        let topic_words = super::tone::content_words(&topic_text);
        let mut out = String::new();
        // 几摊事是复盘时对整段群聊的概括；只留跟眼前几句沾边的，其余是上一阵的事。
        if now - self.threads.at < THREADS_FRESH_SECONDS {
            let lines: Vec<&String> = self
                .threads
                .lines
                .iter()
                .filter(|line| super::tone::touches(line, &topic_words))
                .collect();
            if !lines.is_empty() {
                out.push_str(&format!(
                    "跟眼前沾边的几摊事（{}复盘的，现场已经聊到别处就别接）：\n",
                    ago((now - self.threads.at).max(0))
                ));
                for line in lines {
                    out.push_str(&format!("- {line}\n"));
                }
            }
        }
        let mut cards = Vec::new();
        for id in seen {
            let Some(person) = self.people.get(id) else {
                continue;
            };
            let name = if person.name.is_empty() {
                id.to_string()
            } else {
                format!("{}({id})", person.name)
            };
            if person.stranger(now) {
                cards.push(format!("- {name}：新面孔，之前没打过交道"));
                continue;
            }
            let mut facts = Vec::new();
            if person.exchanges > 0 {
                facts.push(format!("聊过 {} 次", person.exchanges));
            }
            if person.last_seen > 0 && now > person.last_seen {
                facts.push(format!("上次露面 {}", ago(now - person.last_seen)));
            }
            let tail = if facts.is_empty() {
                String::new()
            } else {
                format!("（{}）", facts.join("，"))
            };
            let mut parts = Vec::new();
            if !person.note.is_empty() {
                parts.push(person.note.clone());
            }
            // 称呼是「你俩之间怎么叫」，跟名片上的名字不一样，得单独说清楚。
            if !person.address.is_empty() {
                parts.push(format!("你平时叫他「{}」", person.address));
            }
            let warmth = person.warmth(now);
            if warmth >= 0.25 {
                parts.push(if warmth >= 0.55 {
                    "最近互动挺热络".to_string()
                } else {
                    "最近有过几回直接互动".to_string()
                });
            }
            let mut observations: Vec<&Observation> = person.observations.iter().collect();
            observations
                .sort_by(|a, b| b.confidence.total_cmp(&a.confidence).then(b.at.cmp(&a.at)));
            for observation in observations.into_iter().take(2) {
                parts.push(format!(
                    "暂定观察：{}（{}）",
                    observation.text,
                    observation.evidence()
                ));
            }
            // 没印象的熟脸不列：一串「眼熟，没什么具体印象」只会让人格觉得
            // 该跟每个人都搭一句，而人对说不出什么的人本来就想不起什么。
            if parts.is_empty() {
                continue;
            }
            cards.push(format!("- {name}：{}{tail}", parts.join("；")));
        }
        if !cards.is_empty() {
            out.push_str(
                "你记得的人（全是会过时的参考，不是事实或定论；以当前言行为准，人会变化，\
                 玩笑、反串和一时情绪不代表稳定性格，别拿旧印象套人，更别主动提起他们的事）：\n",
            );
            out.push_str(&cards.join("\n"));
            out.push('\n');
        }
        // 旧事只带「眼前用得上」的：跟最近几条在聊的沾边，或者就是这半小时的事。
        // 从前按时间倒序全摆出来，人格会把三天前的梗硬塞进不相干的话里；后来留了六小时
        // 的「刚起的梗」例外，同样会把一个钟头前的话头拎回来，所以缩到半小时。
        let topic = super::tone::grams(&topic_text);
        let mut notes: Vec<(f32, &Note)> = self
            .notes
            .iter()
            .map(|note| (super::tone::affinity(&note.text, &topic), note))
            .filter(|(score, note)| *score >= 0.6 || now - note.at < FRESH_NOTE_SECONDS)
            .collect();
        notes.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.at.cmp(&a.1.at)));
        let notes: Vec<String> = notes
            .into_iter()
            .take(BRIEF_NOTES)
            .map(|(_, note)| format!("- {}（{}）", note.text, ago((now - note.at).max(0))))
            .collect();
        if !notes.is_empty() {
            out.push_str("这场聊天的旧事（背景；眼前的话没碰到它就别提）：\n");
            out.push_str(&notes.join("\n"));
            out.push('\n');
        }
        out
    }

    /// 给工具回执用的一句话概况。
    pub(crate) fn summary(&self) -> String {
        let with_note = self
            .people
            .values()
            .filter(|p| !p.note.is_empty() || !p.observations.is_empty())
            .count();
        format!(
            "记得 {} 个人（{} 个有印象），{} 条旧事",
            self.people.len(),
            with_note,
            self.notes.len()
        )
    }
}

/// 进程内的记忆总账。`dir` 为空时只在内存里活着（测试与未初始化阶段）。
#[derive(Default)]
struct Store {
    dir: Option<PathBuf>,
    groups: HashMap<String, GroupMemory>,
    dirty: HashSet<String>,
    written: HashMap<String, Instant>,
}

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Store::default()))
}

fn lock() -> MutexGuard<'static, Store> {
    store().lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 一个群一份文件，文件名就是群 ID。ID 由实现端给出，路径分隔符换掉，免得跑出目录。
fn path_of(dir: &Path, group: &str) -> PathBuf {
    // 私聊的窗口键是 `private:<QQ号>`：冒号在 Windows 上是非法文件名，一并换掉。
    dir.join(format!("{}.json", group.replace(['/', '\\', ':'], "_")))
}

/// 指定记忆的落盘位置；启动时调用一次。
pub(crate) fn attach(base: &Path) {
    let mut store = lock();
    store.dir = Some(base.join("memory"));
    store.groups.clear();
    store.dirty.clear();
    store.written.clear();
}

/// 每个群的记忆快照，按群号排序，供控制台展示。
///
/// 以内存那份为准：落盘是节流的（见 [`WRITE_INTERVAL`]），磁盘上那份随时可能落后
/// 几十秒。内存里还没有的群从磁盘补上——进程刚起来、那个群还没说过话时就是这种。
pub(crate) fn snapshot() -> Vec<(String, GroupMemory)> {
    let store = lock();
    let mut out: Vec<(String, GroupMemory)> = store
        .groups
        .iter()
        .map(|(group, memory)| (group.clone(), memory.clone()))
        .collect();
    if let Some(dir) = store.dir.as_ref()
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(group) = name.to_string_lossy().strip_suffix(".json").map(str::to_string) else {
                continue;
            };
            if store.groups.contains_key(&group) {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(entry.path())
                && let Ok(memory) = serde_json::from_str::<GroupMemory>(&text)
            {
                out.push((group, memory));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// 取出某个群的记忆做一次修改。闭包里不要 await——锁是同步的。
pub(crate) fn with_group<T>(group: &str, action: impl FnOnce(&mut GroupMemory) -> T) -> T {
    let mut store = lock();
    if !store.groups.contains_key(group) {
        let loaded = store
            .dir
            .as_ref()
            .and_then(|dir| std::fs::read_to_string(path_of(dir, group)).ok())
            .and_then(|raw| serde_json::from_str::<GroupMemory>(&raw).ok())
            .unwrap_or_default();
        store.groups.insert(group.to_string(), loaded);
    }
    action(store.groups.get_mut(group).expect("just inserted"))
}

/// 同上，但顺带标记「有改动，待落盘」。
pub(crate) fn edit<T>(group: &str, action: impl FnOnce(&mut GroupMemory) -> T) -> T {
    let result = with_group(group, action);
    lock().dirty.insert(group.to_string());
    result
}

/// 把待落盘的改动写出去，最多每 [`WRITE_INTERVAL`] 一次。没有改动时不碰磁盘。
pub(crate) async fn flush(group: &str) {
    write(group, false).await;
}

/// 立刻落盘，不受节流限制：人格刚写下的印象值得马上留住。
pub(crate) async fn flush_now(group: &str) {
    write(group, true).await;
}

async fn write(group: &str, force: bool) {
    let payload = {
        let mut store = lock();
        if !store.dirty.contains(group) {
            return;
        }
        if !force
            && store
                .written
                .get(group)
                .is_some_and(|at| at.elapsed() < WRITE_INTERVAL)
        {
            return;
        }
        store.dirty.remove(group);
        store.written.insert(group.to_string(), Instant::now());
        let Some(dir) = store.dir.clone() else {
            return;
        };
        store.groups.get_mut(group).map(|memory| {
            memory.prune(chrono::Local::now().timestamp());
            (
                path_of(&dir, group),
                serde_json::to_string(memory).unwrap_or_default(),
            )
        })
    };
    let Some((path, json)) = payload else {
        return;
    };
    if json.is_empty() {
        return;
    }
    if let Err(error) = crate::storage::write_atomic_async(path, json.into_bytes()).await {
        warn!(target: super::LOG_TARGET, "写入群 {group} 的记忆失败：{error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_message_cannot_become_a_firm_judgement() {
        let mut memory = GroupMemory::default();
        assert!(memory.observe("42", "爱开玩笑", 0.98, 1, 1_000).unwrap());
        let person = memory.people.get("42").unwrap();
        // 模型给得多高都要按样本数压下来；一句玩梗不该变成定性。
        assert_eq!(person.observations[0].samples, 1);
        assert!(person.observations[0].confidence <= 0.36);
    }

    #[test]
    fn repeated_evidence_strengthens_and_caps() {
        let mut memory = GroupMemory::default();
        for round in 0..6 {
            memory
                .observe("42", "常在深夜出现", 0.9, 8, 1_000 + round * 100)
                .unwrap();
        }
        let person = memory.people.get("42").unwrap();
        assert_eq!(person.observations.len(), 1);
        assert_eq!(person.observations[0].samples, 48);
        assert!(person.observations[0].confidence > 0.6);
        assert!(person.observations[0].confidence <= 0.95);
    }

    #[test]
    fn observations_are_bounded_and_forget_old_ones() {
        let mut memory = GroupMemory::default();
        for (index, text) in ["喜欢折腾手机", "常发老图", "爱用长句", "在准备考试"]
            .iter()
            .enumerate()
        {
            let _ = memory.observe("42", text, 0.5, 2 + index, 1_000);
        }
        assert!(memory.people["42"].observations.len() <= 3);
        // 过期观察随 prune 淡忘，但人还在（长期印象不被观察挤掉）。
        memory.remember("42", "聊得来").unwrap();
        memory.prune(1_000 + NOTE_TTL_DAYS * 86_400 + 1);
        let person = memory.people.get("42").unwrap();
        assert!(person.observations.is_empty());
        assert_eq!(person.note, "聊得来");
    }

    #[test]
    fn warmth_comes_from_interaction_not_from_talking() {
        let mut memory = GroupMemory::default();
        let now = 10 * 86_400;
        memory.see("42", "老张", now);
        // 只是说话多：不加热度。
        for step in 0..30 {
            memory.see("42", "老张", now + step);
        }
        assert_eq!(memory.people["42"].warmth(now + 30), 0.0);
        // 真实往来才加，而且会随时间衰减。
        memory.engage("42", now + 30);
        let warm = memory.people["42"].warmth(now + 30);
        assert!(warm > 0.0);
        assert!(memory.people["42"].warmth(now + 30 + 6 * 86_400) < warm / 3.0);
    }
}
