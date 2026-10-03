//! 偷来的表情包：群友发的图与商城表情攒成一个自己的小库，往后随手取用。
//!
//! 「偷」就是把群里那张表情包再发一遍。从前只偷得动眼前这段记录里的那张：窗口滑过去、
//! 进程一重启，它就找不回来了——偷一次就没了。所以这里在偷的那一刻把东西抄成自己的一份：
//! 商城表情留下收到时那几个参数，图把字节存进 `data/ambient/stickers/`，库里的东西就此
//! 跟当下的窗口脱钩，往后哪一轮都能取出来。
//!
//! 标签是给「什么时候发它」用的，不是给「像不像」用的。人格在偷的那一刻正看着这张图
//! （图片本来就随上下文进模型），顺手写一句它长什么样就够；没写的时候退回商城表情自带的
//! 摘要，再不成才退回发它时那句话。发言轮按当下话题挑几张贴进提示词，用的是与口吻样本
//! 同一套字组重合度：贴题的排前面，同分先给还没怎么用过的，于是新偷进来的自然浮上来。

use super::tone;
use super::window::Turn;
use crate::message::Segment;
use serde::{Deserialize, Serialize};
use simd_json::base::ValueAsScalar;
use simd_json::owned::Object;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// 一张图最多留多大。表情包通常几十到几百 KB；上了这个量级的多半是照片，
/// 抄一份既占地方又不常用——这种留在窗口里照样偷得动。
const MAX_BYTES: usize = 4 * 1024 * 1024;
/// 标签最多几个字。它跟着每轮的账单走，长了就是花钱买废话。
const LABEL_CHARS: usize = 24;
/// 发言轮最多贴几张给模型挑。库是货架不是清单，看得见手边这几张就够。
const BRIEF_LIMIT: usize = 4;
/// 只看最近的这些条消息来猜话题，与口吻样本同一条口径。
const TOPIC_TURNS: usize = 12;
/// 记录里能偷的最多点名几条。刷图的时候一屏全是图，点名最近这几条就够。
const LOOT_LIMIT: usize = 3;

/// 收进来的是哪一种。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Kind {
    /// 商城表情：重发要的就是收到时那几个参数，原样留着。
    ///
    /// 这份参数只对这张表情成立、与哪条消息无关，所以 `key` 也照收着——重新发出去
    /// 与当场偷那张走的是同一条路。
    Shop { data: Object },
    /// 群友发的图：字节抄一份在自己这儿，重发时走上传。`file` 是库目录里的文件名。
    Image { file: String },
}

/// 库里的一个条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Entry {
    /// 编号。只增不复用：提示词里过了期的编号要干脆落空，不能指到另一张图上。
    pub id: u32,
    /// 一句话说明它长什么样、什么场合发。
    pub label: String,
    pub kind: Kind,
    /// 谁发的，从哪个群偷的。
    pub from: String,
    pub group: String,
    pub added_at: i64,
    /// 用过几次、最后一次什么时候用的：库满了先丢最没人用的。
    pub uses: u32,
    pub last_used: i64,
}

impl Entry {
    /// 提示词里那一行：`#12 猫捂着嘴笑（老张发的）`。
    fn line(&self) -> String {
        let label = if self.label.is_empty() {
            "没起名的表情包"
        } else {
            self.label.as_str()
        };
        let from = self.from.trim();
        if from.is_empty() {
            format!("#{} {}", self.id, label)
        } else {
            format!("#{} {}（{}发的）", self.id, label, from)
        }
    }
}

/// 落盘的那一份。
#[derive(Debug, Default, Serialize, Deserialize)]
struct Library {
    next_id: u32,
    entries: Vec<Entry>,
}

/// 进程内的状态。`root` 为空时只在内存里活着（测试与未初始化阶段）。
#[derive(Default)]
struct Store {
    root: Option<PathBuf>,
    library: Library,
}

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Store::default()))
}

fn lock() -> MutexGuard<'static, Store> {
    store().lock().unwrap_or_else(|error| error.into_inner())
}

/// 指定库的落盘位置；启动时调用一次。
///
/// 库是全局一份、不分群：同一个 QQ 号的表情包在哪个群都发得出去，人格也只有一个。
pub(crate) fn attach(base: &Path) {
    let root = base.join("stickers");
    let library = std::fs::read_to_string(root.join("index.json"))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let mut store = lock();
    store.root = Some(root);
    store.library = library;
}

/// 库目录。上传那份文件时要拿它当「这份文件是自己人」的凭据。
pub(crate) fn root() -> Option<PathBuf> {
    lock().root.clone()
}

/// 库里现有多少张。
pub(crate) fn count() -> usize {
    lock().library.entries.len()
}

/// 库里的全部条目，供控制台画廊展示：用过的排前面，同次数按收藏时间倒序。
///
/// 与 [`brief`] 挑给提示词的那四张不同——那是「此刻这张图该不该用」，这是清点。
pub(crate) fn gallery() -> Vec<Entry> {
    let mut entries = lock().library.entries.clone();
    entries.sort_by(|a, b| b.uses.cmp(&a.uses).then(b.added_at.cmp(&a.added_at)));
    entries
}

/// 按编号取一条。控制台按图取用走这里：不需要顺序，也就不必整库克隆再排序。
pub(crate) fn by_id(id: u32) -> Option<Entry> {
    lock().library.entries.iter().find(|entry| entry.id == id).cloned()
}

/// 收下一张偷来的表情包，返回它的编号；`max` 为 0 表示不攒。
///
/// `bytes` 只有图那一路要：商城表情重发靠参数，字节没用。`label` 是人格在偷的那一刻
/// 顺手写的一句说明，落库前先收拾成一行（见 [`clean_label`]）。
///
/// 已经在库里的那张认原来那条（同一张图不该占两个编号），顺手把缺的名字补上。
pub(crate) fn keep(
    segment: &Segment,
    source: &Turn,
    group: &str,
    label: &str,
    bytes: Option<&[u8]>,
    max: usize,
) -> Option<u32> {
    if max == 0 {
        return None;
    }
    // 图那一路先把文件名定下来：内容指纹当名字，同一张图偷两次落的是同一个文件。
    let (kind, write) = match segment.type_.as_str() {
        "mface" => (Kind::Shop { data: segment.data.clone() }, None),
        "image" => {
            let bytes = bytes.filter(|bytes| !bytes.is_empty() && bytes.len() <= MAX_BYTES)?;
            let file = format!("{:x}.{}", md5::compute(bytes), super::image_extension(bytes));
            (Kind::Image { file: file.clone() }, Some((file, bytes)))
        }
        _ => return None,
    };
    let label = match clean_label(label) {
        empty if empty.is_empty() => fallback_label(segment, source),
        named => named,
    };
    let mut store = lock();
    let root = store.root.clone()?;
    let known = store
        .library
        .entries
        .iter()
        .position(|entry| same_kind(&entry.kind, &kind));
    if let Some(index) = known {
        let id = store.library.entries[index].id;
        let unnamed = store.library.entries[index].label.is_empty();
        if unnamed && !label.is_empty() {
            store.library.entries[index].label = label;
            persist(&store);
        }
        return Some(id);
    }
    if let Some((file, bytes)) = write
        && (std::fs::create_dir_all(&root).is_err()
            || std::fs::write(root.join(&file), bytes).is_err())
    {
        return None;
    }
    let id = store.library.next_id + 1;
    store.library.next_id = id;
    let now = chrono::Local::now().timestamp();
    store.library.entries.push(Entry {
        id,
        label,
        kind,
        from: source.name.trim().to_string(),
        group: group.to_string(),
        added_at: now,
        uses: 0,
        last_used: 0,
    });
    trim(&mut store.library, max, &root);
    persist(&store);
    Some(id)
}

/// 取用一张：把条目交给发送侧，顺手记一次使用。
///
/// `note` 非空时顺手给这张改个名——起名的时候没想好，或者当初没写，事后补一句。
/// 图那一路顺手看一眼文件还在不在：库目录是活的，被人清掉之后留下的空条目自己收走，
/// 比每轮贴一个发不出去的编号好。
pub(crate) fn take(id: u32, note: &str) -> Option<Entry> {
    let note = clean_label(note);
    let mut store = lock();
    let root = store.root.clone()?;
    let index = store.library.entries.iter().position(|entry| entry.id == id)?;
    if let Kind::Image { file } = &store.library.entries[index].kind
        && !root.join(file).is_file()
    {
        store.library.entries.remove(index);
        persist(&store);
        return None;
    }
    let now = chrono::Local::now().timestamp();
    let entry = &mut store.library.entries[index];
    entry.uses = entry.uses.saturating_add(1);
    entry.last_used = now;
    if !note.is_empty() {
        entry.label = note;
    }
    let taken = entry.clone();
    persist(&store);
    Some(taken)
}

/// 图那种条目落在磁盘上的绝对路径；商城表情没有文件，返回 None。
pub(crate) fn file_of(entry: &Entry) -> Option<PathBuf> {
    match &entry.kind {
        Kind::Image { file } => Some(root()?.join(file)),
        Kind::Shop { .. } => None,
    }
}

/// 图按 QQ 表情的样子发（图片子类型 1）：聊天里是一张小表情，会话列表里是「[动画表情]」，
/// 而不是一张带相框的大图。偷来的就是拿来当表情包用的，群友当初是当照片发的也一样。
/// 商城表情与别的段落原样返回。
pub(crate) fn sticker_style(mut segment: Segment) -> Segment {
    if segment.type_ == "image" {
        segment
            .data
            .insert("sub_type".into(), simd_json::OwnedValue::from(1_i64));
    }
    segment
}

/// 发言轮贴进提示词的那一段；库是关的、或者库空着而眼前也没什么可偷时返回空串。
///
/// 两样东西摆在手边。一是库里的几张，按「离眼前的话有多近」挑：接梗、吐槽、被逗乐这些
/// 场合用不用得上，看的就是它自己写的那个标签与此刻话题的重合度；一条都贴不上时也给
/// 几张——那时挑出来的是还没怎么用过的，新偷进来的正好露一面。
///
/// 二是记录里刚有人发过的图与表情包，连着消息 ID 一起点出来。从前库空着时这一段整个
/// 不出现，偷这件事只剩工具说明里的一句话，于是一次都没偷过，库也就一直空着——
/// 先有货架才会去偷，先偷了才会有货架。点名眼前能偷的那几条，这个圈就解开了。
pub(crate) fn brief(turns: &[Turn], max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let loot = loot(turns);
    let store = lock();
    let total = store.library.entries.len();
    let mut out = String::new();
    if total > 0 {
        let topic = tone::grams(&recent(turns));
        let mut ranked: Vec<(&Entry, f32)> = store
            .library
            .entries
            .iter()
            .map(|entry| (entry, tone::affinity(&entry.label, &topic)))
            .collect();
        ranked.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then(a.0.uses.cmp(&b.0.uses))
                .then(b.0.added_at.cmp(&a.0.added_at))
                // 同一秒里偷进来的几张按编号倒着排：刚偷的先露一面。
                .then(b.0.id.cmp(&a.0.id))
        });
        let lines: Vec<String> = ranked
            .iter()
            .take(BRIEF_LIMIT)
            .map(|(entry, _)| format!("- {}", entry.line()))
            .collect();
        out.push_str(&format!(
            "偷来的表情包（一共 {total} 张，这几张离眼前的话最近）：\n{}\n\
             要发就用 send 里的 sticker 配 id。\n",
            lines.join("\n")
        ));
    } else if !loot.is_empty() {
        out.push_str("你的表情包库还空着，库里没有编号可取。\n");
    }
    if !loot.is_empty() {
        out.push_str(&format!(
            "记录里刚有人发了图或表情包，好玩的可以偷来回一张（sticker 配 message_id，\
             一条里有好几张就用 index 从 0 数），顺手写一句 note 说清它是什么、什么场合发，\
             往后才挑得出来：\n{}\n",
            loot.join("\n")
        ));
    }
    out
}

/// 记录里最近那几条带图或商城表情的消息，一条一行：`- id=123 老张：[表情包:开心]`。
///
/// 自己发的不算——那张要么本来就在库里，要么是自己画的。
fn loot(turns: &[Turn]) -> Vec<String> {
    // 「刚有人发了」得是真的刚：隔了半小时的图还摆在眼前，模型会去接一张早就聊过的图。
    let now = chrono::Local::now().timestamp();
    turns
        .iter()
        .rev()
        .take(TOPIC_TURNS)
        .filter(|turn| !turn.from_me && !turn.message_id.is_empty() && now - turn.at <= 15 * 60)
        .filter_map(|turn| {
            let count = turn
                .elements
                .0
                .iter()
                .filter(|segment| matches!(segment.type_.as_str(), "image" | "mface"))
                .count();
            (count > 0).then(|| {
                let what = match clean_label(&turn.text) {
                    empty if empty.is_empty() => "[图片]".to_string(),
                    text => text,
                };
                let many = if count > 1 {
                    format!("（{count} 张）")
                } else {
                    String::new()
                };
                format!("- id={} {}：{what}{many}", turn.message_id, turn.name.trim())
            })
        })
        .take(LOOT_LIMIT)
        .collect()
}

/// 最近这些条消息拼成的一段话，用来猜此刻在聊什么。
fn recent(turns: &[Turn]) -> String {
    turns
        .iter()
        .rev()
        .take(TOPIC_TURNS)
        .map(|turn| turn.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// 两个条目是不是同一张表情包。
///
/// 商城表情认「表情 ID + 表情包 ID」这一对——同一个表情在群里发两次，收到的就是同一个
/// 表情；图认内容指纹，同一张图从两个群偷来的也只留一份。
fn same_kind(left: &Kind, right: &Kind) -> bool {
    match (left, right) {
        (Kind::Shop { data: left }, Kind::Shop { data: right }) => {
            let key = |data: &Object| {
                (
                    data.get("emoji_id").map(|value| value.to_string()),
                    data.get("emoji_package_id").map(|value| value.to_string()),
                )
            };
            key(left) == key(right)
        }
        (Kind::Image { file: left }, Kind::Image { file: right }) => left == right,
        _ => false,
    }
}

/// 人格没写名字时的退路：商城表情自带那句摘要（`[开心]`），图就退回偷它时群里那句话——
/// 标签是给「什么时候发它」用的，含糊一点也比没有强。
fn fallback_label(segment: &Segment, source: &Turn) -> String {
    let summary = segment
        .data
        .get("summary")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let summary = clean_label(summary);
    if !summary.is_empty() {
        return summary;
    }
    clean_label(&source.text)
}

/// 把标签收拾成能进提示词的一行：压掉换行与多余空白，超长的截断。
pub(crate) fn clean_label(raw: &str) -> String {
    let flat = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= LABEL_CHARS {
        return flat;
    }
    let head: String = flat.chars().take(LABEL_CHARS).collect();
    format!("{head}…")
}

/// 库满了先丢最没人用的那几张：比使用次数，再比最后一次用的时候，最后比谁老。
fn trim(library: &mut Library, max: usize, root: &Path) {
    while library.entries.len() > max {
        let Some(victim) = library
            .entries
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                a.uses
                    .cmp(&b.uses)
                    .then(a.last_used.cmp(&b.last_used))
                    .then(a.added_at.cmp(&b.added_at))
            })
            .map(|(index, _)| index)
        else {
            return;
        };
        let removed = library.entries.remove(victim);
        // 同一个文件只该有一个条目，但库是手工也能改的：真有两个时留着文件，
        // 别把还能用的那张连坐掉。
        if let Kind::Image { file } = &removed.kind
            && !library
                .entries
                .iter()
                .any(|entry| matches!(&entry.kind, Kind::Image { file: other } if other == file))
        {
            let _ = std::fs::remove_file(root.join(file));
        }
    }
}

/// 把库写回磁盘。条目是小文件，真写不进去的代价只是下次启动少几张，不必打断这一轮发言。
fn persist(store: &Store) {
    let Some(root) = &store.root else {
        return;
    };
    let Ok(json) = serde_json::to_string(&store.library) else {
        return;
    };
    if std::fs::create_dir_all(root).is_err() {
        return;
    }
    let _ = std::fs::write(root.join("index.json"), json);
}
