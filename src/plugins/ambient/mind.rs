//! 心思：每一轮开口之前，人格先给自己写一笔。
//!
//! [`super::mood`] 那一条是算出来的——钟点、刚才有没有人接话，最后落在两个数上；
//! 这一份是它自己写下来的：眼前是什么场面、自己在干什么、心情如何、心里最实的那句
//! 想法。写在一行控制记号里，跟正文一起回来，发送前被剥掉，只落进 `mind.json`；
//! 下一轮摆在记录旁边，于是它记得「我刚才对这件事是这么想的」，前后接得上，
//! 而不是每轮把同一个群重新读一遍、重新做一次人。
//!
//! 六格：场景、对话对象、动作、状态、心情、想法。不要求严格按这六个词分——
//! 模型想怎么写就怎么写，只要每轮都写。
//!
//! 这一行同时也是给下一轮留下的把手：它比 [`super::mood`] 细，又比记忆短命，
//! 说的都是「此时此刻」——半天前的念头比没有还误导，所以过了 [`KEEP`] 就不再摆出来。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use super::LOG_TARGET;

/// 超过这么久的心思不再摆出来（秒）：半天前的「我正在想」比没有还误导。
const KEEP: i64 = 12 * 3_600;
/// 括号迟迟不闭合时最多再往下读几行：一行写坏的记号不该把后面的消息一起吞掉。
const MAX_WRAP: usize = 8;

/// 发言那一轮追加的一段说明：先写心思，再回话。
///
/// 长度直接进每轮的账单，所以写到「六格各写什么、这行不会发出去」为止；
/// 六个格子的名字与下面的解析一一对应，改一处就要改另一处。
pub(crate) const RULES: &str = "\n每一轮回话之前，先给自己写一行心思。它独占一行，写在消息前面（用了工具也一样，写在动手之前或最终正文里）：\n[心]{\"scene\":\"…\",\"to\":\"…\",\"act\":\"…\",\"state\":\"…\",\"mood\":\"…\",\"thought\":\"…\"}\n这一行只有你自己看得见，不会发进群里，也不占条数；别在正文里复述它，别把它念出来。六个格每次都写全，各一句短话：\n- scene：眼前是什么场面——谁和谁在聊什么、气氛怎么样；\n- to：你在对着谁说，写名字；没有特定对象就写「群里」；\n- act：你这会儿在干什么——只是看着、刚说完一句在等回应、边忙别的边瞄一眼、在翻记录；\n- state：精神状态，跟现在几点、腾不腾得出手合得上；\n- mood：心情和情绪，写感受不是写评价；\n- thought：心里最实的那句——你怎么看这件事、想不想接、为什么。写你真正觉得的，别写成答题提纲。\n这一行每轮都是新的，可以跟上一轮不一样：人本来就会走着走着换个心情。上一轮留下的那几行叫「你上一轮给自己留的心思」，接着写或变了都行。写不出想接的话，就写完这行再 [silent]。";

/// 一格心思。六个字段都是短句，空了就不摆出来。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Entry {
    /// 眼前的场面。
    pub scene: String,
    /// 对着谁说话。
    pub to: String,
    /// 这会儿在干什么。
    pub act: String,
    /// 精神状态。
    pub state: String,
    /// 心情与情绪。
    pub mood: String,
    /// 心里那句想法。
    pub thought: String,
    /// 写下的时刻（秒）。
    pub at: i64,
}

impl Entry {
    fn blank(&self) -> bool {
        self.scene.is_empty()
            && self.to.is_empty()
            && self.act.is_empty()
            && self.state.is_empty()
            && self.mood.is_empty()
            && self.thought.is_empty()
    }
}

/// 模型写回来的那一行。字段全给默认值，缺一格不算错。
///
/// 别名是为小模型留的：它们常把键写成中文，或者换个近义的说法。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Raw {
    #[serde(alias = "场景", alias = "where")]
    scene: String,
    #[serde(alias = "对话对象", alias = "target", alias = "with")]
    to: String,
    #[serde(alias = "动作", alias = "action", alias = "doing")]
    act: String,
    #[serde(alias = "状态", alias = "status", alias = "body")]
    state: String,
    #[serde(alias = "心情", alias = "情绪", alias = "feeling")]
    mood: String,
    #[serde(alias = "想法", alias = "心理活动", alias = "thinking", alias = "idea")]
    thought: String,
}

/// 一行是不是心思记号；是的话给出行内那截 JSON。
///
/// 提示词写的是方括号，模型偶尔写成尖括号（仿 Satori 的元素），两种都认；
/// 漏判的代价是这行原样发进群里，所以宁可宽一点。
fn marker(line: &str) -> Option<&str> {
    let clean = line.trim().trim_start_matches(['-', '*', ' ']).trim();
    for prefix in ["[心]", "[心:", "[心：", "<心>", "<心:", "<心："] {
        if let Some(rest) = clean.strip_prefix(prefix) {
            let rest = rest.trim();
            let rest = rest.strip_suffix(']').or_else(|| rest.strip_suffix('>')).unwrap_or(rest);
            return Some(rest.trim());
        }
    }
    None
}

/// 这一行是不是心思记号。给发送前的兜底用（见 [`super::Ambient::vet_text`]）。
pub(crate) fn is_control(line: &str) -> bool {
    marker(line).is_some()
}

/// 一段正文里有没有心思记号——模型偶尔会把这行抄进要发的那条消息里。
pub(crate) fn contains_control(text: &str) -> bool {
    text.lines().any(is_control)
}

/// 括号是否已经闭合，且至少有一个对象。逐字符走，字符串里的括号不算数。
fn closed(body: &str) -> bool {
    let (mut depth, mut braces) = (0i32, 0usize);
    let (mut quoted, mut escaped) = (false, false);
    for ch in body.chars() {
        if quoted {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
            continue;
        }
        match ch {
            '"' => quoted = true,
            '{' => {
                depth += 1;
                braces += 1;
            }
            '}' => depth -= 1,
            _ => {}
        }
    }
    braces > 0 && depth <= 0 && !quoted
}

/// 一格短句：压平空白、掐掉方括号与像在给人下命令的句子、限长。
fn clean(raw: &str, max: usize) -> String {
    let flat = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = crate::plugins::oai::chat::memory::sanitize(&flat).unwrap_or_default();
    flat.chars().take(max).collect()
}

fn parse(body: &str) -> Option<Entry> {
    let body = body.trim();
    let body = body
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim();
    let body = body.trim_end_matches("```").trim();
    let start = body.find('{')?;
    let end = body.rfind('}')?;
    let raw: Raw = serde_json::from_str(&body[start..=end]).ok()?;
    let entry = Entry {
        scene: clean(&raw.scene, 48),
        to: clean(&raw.to, 16),
        act: clean(&raw.act, 28),
        state: clean(&raw.state, 28),
        mood: clean(&raw.mood, 24),
        thought: clean(&raw.thought, 64),
        at: 0,
    };
    (!entry.blank()).then_some(entry)
}

/// 从一整段回复里剥掉心思那几行，留下一份可发的正文和一个心思。
///
/// 记号行无论解析得成解析不成都不留下——宁可这一轮没记上，也不能让它进群。
/// 一行放不下（写成缩进过的 JSON）时一直读到括号闭合，最多 [`MAX_WRAP`] 行。
pub(crate) fn extract(raw: &str) -> (String, Option<Entry>) {
    let lines: Vec<&str> = raw.lines().collect();
    let mut kept: Vec<&str> = Vec::with_capacity(lines.len());
    let mut found = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        let Some(first) = marker(line) else {
            kept.push(line);
            continue;
        };
        let mut body = first.to_string();
        // 只有「这一行是个开头的花括号 / 干脆空的、JSON 在下一行」才往下接；
        // 一眼就不像 JSON 的（模型写了句白话），赶紧放手，别把后面的消息吞掉。
        let mut extra = 0;
        let starts_json = body.trim().is_empty() || body.contains('{');
        while starts_json && !closed(&body) && i < lines.len() && extra < MAX_WRAP && !marker(lines[i]).is_some() {
            body.push_str(lines[i].trim());
            i += 1;
            extra += 1;
        }
        if let Some(entry) = parse(&body) {
            found = Some(entry);
        }
    }
    (kept.join("\n"), found)
}

#[derive(Default)]
struct Store {
    path: Option<PathBuf>,
    minds: HashMap<String, Entry>,
    loaded: bool,
    dirty: bool,
}

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Store::default()))
}

fn lock() -> MutexGuard<'static, Store> {
    store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 指定心思的落盘位置；启动时调用一次。
pub(crate) fn attach(base: &Path) {
    let mut store = lock();
    store.path = Some(base.join("mind.json"));
    store.minds.clear();
    store.loaded = false;
    store.dirty = false;
}

fn ensure_loaded(store: &mut Store) {
    if store.loaded {
        return;
    }
    store.loaded = true;
    if let Some(minds) = store
        .path
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str::<HashMap<String, Entry>>(&raw).ok())
    {
        store.minds = minds;
    }
}

/// 记下这一轮的心思。
pub(crate) fn record(scope: &str, mut entry: Entry) {
    if scope.is_empty() || entry.blank() {
        return;
    }
    entry.at = chrono::Local::now().timestamp();
    let mut store = lock();
    ensure_loaded(&mut store);
    store.minds.insert(scope.to_string(), entry);
    store.dirty = true;
}

/// 上一轮留下的心思，摆到这一轮的记录旁边；没有或太旧就是空串。
pub(crate) fn brief(scope: &str) -> String {
    let mut store = lock();
    ensure_loaded(&mut store);
    let now = chrono::Local::now().timestamp();
    let Some(entry) = store.minds.get(scope) else {
        return String::new();
    };
    if now - entry.at > KEEP || entry.blank() {
        return String::new();
    }
    let age = crate::plugins::oai::chat::memory::ago((now - entry.at).max(0));
    let mut body = String::new();
    for (label, value) in [
        ("场景", &entry.scene),
        ("对话对象", &entry.to),
        ("动作", &entry.act),
        ("状态", &entry.state),
        ("心情", &entry.mood),
        ("想法", &entry.thought),
    ] {
        if !value.is_empty() {
            body.push_str(&format!("- {label}：{value}\n"));
        }
    }
    format!(
        "你上一轮给自己留的心思（{age}，只有你自己看得见）：\n{body}眼前跟这几行对不上，就按眼前来——这一轮照旧另写一行。\n"
    )
}

/// 把心思写出去；没有改动时不碰磁盘。
pub(crate) async fn flush() {
    let payload = {
        let mut store = lock();
        if !store.dirty {
            return;
        }
        store.dirty = false;
        let now = chrono::Local::now().timestamp();
        store.minds.retain(|_, entry| now - entry.at <= KEEP);
        store
            .path
            .clone()
            .map(|path| (path, serde_json::to_string(&store.minds).unwrap_or_default()))
    };
    let Some((path, json)) = payload else {
        return;
    };
    if json.is_empty() {
        return;
    }
    if let Err(error) = crate::storage::write_atomic_async(path, json.into_bytes()).await {
        warn!(target: LOG_TARGET, "写入心思失败：{error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_line_and_keeps_the_message() {
        let raw = "[心]{\"scene\":\"三个人在聊手机\",\"to\":\"老白\",\"act\":\"刚插了一句\",\"state\":\"还行\",\"mood\":\"有点懒\",\"thought\":\"两边差不多\"}\n确实";
        let (text, entry) = extract(raw);
        assert_eq!(text, "确实");
        let entry = entry.expect("应当解析出来");
        assert_eq!(entry.scene, "三个人在聊手机");
        assert_eq!(entry.to, "老白");
        assert_eq!(entry.thought, "两边差不多");
    }

    #[test]
    fn wraps_pretty_printed_json() {
        let raw = "[心]{\n  \"scene\": \"群里在斗图\",\n  \"thought\": \"接不上\"\n}\n[表情包]";
        let (text, entry) = extract(raw);
        assert_eq!(text, "[表情包]");
        assert_eq!(entry.expect("应当解析出来").thought, "接不上");
    }

    #[test]
    fn drops_a_broken_line_instead_of_leaking_it() {
        let raw = "[心]不是 JSON\n这句话该发出去";
        let (text, entry) = extract(raw);
        assert_eq!(text, "这句话该发出去");
        assert!(entry.is_none());
    }

    #[test]
    fn reads_chinese_keys() {
        let raw = "[心]{\"场景\":\"半夜没人\",\"想法\":\"看看就好\"}";
        let entry = extract(raw).1.expect("应当解析出来");
        assert_eq!(entry.scene, "半夜没人");
        assert_eq!(entry.thought, "看看就好");
    }

    #[test]
    fn recogises_angle_brackets() {
        assert!(is_control("<心:{\"scene\":\"x\"}>"));
        assert!(is_control("  [心] {}"));
        assert!(!is_control("[心情不错]"));
    }
}
