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
    pub group: i64,
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
    group: i64,
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
        group,
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
    turns
        .iter()
        .rev()
        .take(TOPIC_TURNS)
        .filter(|turn| !turn.from_me && turn.message_id != 0)
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::plugins::oai::chat::memory;

    /// 库是进程级的一份，几个测试都要动它；借用记忆那把锁把它们串起来——两者都会被
    /// 场景构建读到（发言轮同时贴口吻样本与表情包），串行才不会有互相误伤。
    pub(crate) fn exclusive() -> MutexGuard<'static, ()> {
        memory::exclusive()
    }

    /// 一个只属于这次测试的库目录；用完删掉。
    pub(crate) fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "acumen-stickers-{name}-{:032x}",
            rand::random::<u128>()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        attach(&dir);
        dir
    }

    fn turn(name: &str, text: &str) -> Turn {
        Turn {
            user_id: 7,
            name: name.into(),
            text: text.into(),
            ..Turn::default()
        }
    }

    fn shop(emoji_id: &str, package_id: &str) -> Segment {
        let mut data = Object::new();
        data.insert("emoji_id".into(), emoji_id.into());
        data.insert("emoji_package_id".into(), package_id.into());
        data.insert("key".into(), "k1".into());
        data.insert("summary".into(), "[开心]".into());
        Segment::new("mface", data)
    }

    fn picture() -> Segment {
        let mut data = Object::new();
        data.insert("url".into(), "http://127.0.0.1:3001/v1/assets/abc.image".into());
        Segment::new("image", data)
    }

    /// 一张最小的 PNG，够用来说明「字节被抄下来了」。
    const PNG: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D',
        b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89,
    ];

    #[test]
    fn a_stolen_shop_sticker_is_kept_and_the_same_one_is_not_stored_twice() {
        let _guard = exclusive();
        let dir = scratch("shop");
        let source = turn("老张", "笑死");
        assert_eq!(
            keep(&shop("296f", "241904"), &source, 1, "猫捂着嘴笑", None, 20),
            Some(1)
        );
        // 同一张再偷一次认原来那条。
        assert_eq!(
            keep(&shop("296f", "241904"), &source, 1, "", None, 20),
            Some(1)
        );
        assert_eq!(count(), 1);
        // 参数原样留着，重发时就是当初收到的那一段。
        let entry = take(1, "").expect("库里有");
        assert_eq!(entry.label, "猫捂着嘴笑");
        assert_eq!(entry.uses, 1);
        match entry.kind {
            Kind::Shop { data } => assert_eq!(data.get("key").unwrap(), "k1"),
            other => panic!("{other:?}"),
        }
        assert!(dir.join("stickers/index.json").is_file());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 名字缺了就退回商城表情自带的摘要，不能留一条读了也不知道是什么的记录。
    #[test]
    fn a_sticker_without_a_name_falls_back_to_its_own_summary() {
        let _guard = exclusive();
        let dir = scratch("fallback");
        let source = turn("老张", "笑死");
        keep(&shop("1", "2"), &source, 1, "", None, 20);
        assert_eq!(take(1, "").unwrap().label, "[开心]");
        // 图没有摘要，退回偷它时群里那句话。
        keep(&picture(), &turn("阿云", "今天又要加班"), 1, "", Some(PNG), 20);
        assert_eq!(take(2, "").unwrap().label, "今天又要加班");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_stolen_image_is_copied_to_disk_and_survives_a_restart() {
        let _guard = exclusive();
        let dir = scratch("image");
        let source = turn("老张", "笑死");
        assert_eq!(
            keep(&picture(), &source, 1, "一只在笑的猫", Some(PNG), 20),
            Some(1)
        );
        let entry = take(1, "").unwrap();
        let path = file_of(&entry).expect("图那条有自己的文件");
        assert_eq!(std::fs::read(&path).unwrap(), PNG);
        assert!(path.to_string_lossy().ends_with(".png"), "{path:?}");

        // 重启：换一次 attach，库还在，编号还是原来那个。
        attach(&dir);
        assert_eq!(count(), 1);
        assert_eq!(take(1, "").unwrap().label, "一只在笑的猫");
        // 同一张图从别的群偷来也还是这一条。
        assert_eq!(
            keep(&picture(), &turn("别人", "哈哈"), 2, "", Some(PNG), 20),
            Some(1)
        );
        assert_eq!(count(), 1);

        // 文件被人清掉之后，这条自己收走，不再贴一个发不出去的编号。
        std::fs::remove_file(&path).unwrap();
        assert!(take(1, "").is_none());
        assert_eq!(count(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_big_image_is_left_in_the_window_instead_of_being_copied() {
        let _guard = exclusive();
        let dir = scratch("big");
        let source = turn("老张", "笑死");
        let bytes = vec![0u8; MAX_BYTES + 1];
        assert_eq!(keep(&picture(), &source, 1, "大图", Some(&bytes), 20), None);
        // 没带字节（下载没成）也一样：不存，窗口里照样偷。
        assert_eq!(keep(&picture(), &source, 1, "没下下来", None, 20), None);
        assert_eq!(count(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 库满了丢最没人用的：用得少的先走，同样少用的先走老的。
    #[test]
    fn the_library_drops_the_least_used_first() {
        let _guard = exclusive();
        let dir = scratch("trim");
        let source = turn("老张", "笑死");
        for index in 0..3 {
            keep(
                &shop(&format!("{index}"), "9"),
                &source,
                1,
                &format!("第 {index} 张"),
                None,
                3,
            );
        }
        take(1, "");
        take(1, "");
        take(2, "");
        keep(&shop("new", "9"), &source, 1, "新偷的", None, 3);
        assert_eq!(count(), 3);
        let ids: Vec<u32> = lock().library.entries.iter().map(|e| e.id).collect();
        assert_eq!(ids, [1, 2, 4], "该丢的是没人用的第 3 张");
        // 关掉库就不再攒新的，也不影响已经存下的。
        assert_eq!(
            keep(&shop("off", "9"), &source, 1, "关掉时偷的", None, 0),
            None
        );
        assert_eq!(count(), 3);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 手边这几张要挑贴题的：聊到加班的时候，「加班」那张排前面。
    #[test]
    fn the_brief_puts_the_stickers_that_fit_the_topic_first() {
        let _guard = exclusive();
        let dir = scratch("brief");
        let source = turn("老张", "笑死");
        keep(&shop("1", "9"), &source, 1, "猫捂着嘴笑", None, 20);
        keep(&shop("2", "9"), &source, 1, "加班到天亮", None, 20);
        keep(&shop("3", "9"), &source, 1, "裂开", None, 20);
        let text = brief(&[turn("群友", "今天又要加班到几点啊")], 20);
        assert!(text.contains("一共 3 张"), "{text}");
        let first = text.lines().nth(1).unwrap();
        assert!(first.contains("加班到天亮"), "{text}");
        // 编号与「怎么发」都在，模型照着就能取。
        assert!(text.contains("#2 加班到天亮（老张发的）"), "{text}");
        assert!(text.contains("sticker"), "{text}");
        // 一句话也不该像禁令清单。
        for word in ["禁止", "不得", "必须", "不要", "不能"] {
            assert!(!text.contains(word), "{text}");
        }
        // 关掉库时这一段整个不出现。
        assert!(brief(&[turn("群友", "加班")], 0).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 冷门话题也得摆几张：挑出来的是还没怎么用过的，新偷的先露一面。
    #[test]
    fn an_unrelated_topic_still_shows_the_freshest_stickers() {
        let _guard = exclusive();
        let dir = scratch("fresh");
        let source = turn("老张", "笑死");
        for index in 0..6 {
            keep(
                &shop(&format!("{index}"), "9"),
                &source,
                1,
                &format!("第 {index} 张"),
                None,
                20,
            );
        }
        let text = brief(&[turn("群友", "zzz qqq")], 20);
        let lines: Vec<&str> = text
            .lines()
            .filter(|line| line.starts_with("- #"))
            .collect();
        assert_eq!(lines.len(), BRIEF_LIMIT, "{text}");
        assert!(lines[0].contains("#6"), "{text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 库空着的时候也得告诉它能偷：记录里谁刚发了图、消息 ID 是几，一条一行点出来。
    /// 从前这一段在库空时整个不出现，偷这件事就一直想不起来，库也就一直是空的。
    #[test]
    fn an_empty_library_still_points_at_what_can_be_stolen() {
        let _guard = exclusive();
        let dir = scratch("loot");
        // 眼前没图、库也空着：什么都不贴。
        assert!(brief(&[turn("群友", "今天又要加班")], 20).is_empty());

        let mut meme = turn("老张", "[表情包:捂脸笑]");
        meme.message_id = 321;
        meme.elements = crate::message::Message::new().mface("296f", "241904", "k1");
        let mut pics = turn("阿云", "看这个[图片]");
        pics.message_id = 322;
        pics.elements = crate::message::Message::new()
            .image("https://example.com/a.png")
            .image("https://example.com/b.png");
        let mut mine = turn("我", "[图片]");
        mine.message_id = 323;
        mine.from_me = true;
        mine.elements = crate::message::Message::new().image("https://example.com/c.png");
        let text = brief(&[meme, pics, turn("群友", "笑死"), mine], 20);
        assert!(text.contains("库还空着"), "{text}");
        assert!(text.contains("- id=321 老张：[表情包:捂脸笑]"), "{text}");
        assert!(text.contains("- id=322 阿云：看这个[图片]（2 张）"), "{text}");
        // 自己发的不点名；最近的排前面。
        assert!(!text.contains("id=323"), "{text}");
        assert!(text.find("id=322") < text.find("id=321"), "{text}");
        assert!(text.contains("message_id") && text.contains("note"), "{text}");
        for word in ["禁止", "不得", "必须", "不要", "不能"] {
            assert!(!text.contains(word), "{text}");
        }

        // 库里有货之后「库还空着」那句就不说了，货架和能偷的两段都在。
        keep(&shop("1", "9"), &turn("老张", "笑死"), 1, "猫捂着嘴笑", None, 20);
        let mut again = turn("老张", "[图片]");
        again.message_id = 400;
        again.elements = crate::message::Message::new().image("https://example.com/d.png");
        let text = brief(&[again], 20);
        assert!(!text.contains("库还空着"), "{text}");
        assert!(text.contains("#1 猫捂着嘴笑"), "{text}");
        assert!(text.contains("- id=400 老张：[图片]"), "{text}");
        // 关掉库时哪一段都不出现。
        assert!(brief(&[turn("群友", "加班")], 0).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 偷来的图按表情的样子发；商城表情本来就是表情，不动它。
    #[test]
    fn stolen_pictures_go_out_as_stickers() {
        let sent = sticker_style(picture());
        assert_eq!(sent.data.get("sub_type").and_then(|v| v.as_i64()), Some(1));
        assert!(sent.data.get("url").is_some(), "原来的参数都留着");
        let shop = sticker_style(shop("1", "9"));
        assert!(shop.data.get("sub_type").is_none());
    }

    #[test]
    fn labels_are_flattened_and_cut_to_one_line() {
        assert_eq!(clean_label("  猫\n捂着  嘴笑 "), "猫 捂着 嘴笑");
        assert_eq!(clean_label(&"字".repeat(60)).chars().count(), LABEL_CHARS + 1);
    }

    /// 名字可以后补：起名的时候没想好，或者当初没写。取用的时候顺手改名。
    #[test]
    fn a_name_can_be_fixed_when_the_sticker_is_used() {
        let _guard = exclusive();
        let dir = scratch("rename");
        let source = turn("老张", "笑死");
        keep(&shop("1", "9"), &source, 1, "", None, 20);
        assert_eq!(take(1, "  这才是  它真正的样子 ").unwrap().label, "这才是 它真正的样子");
        // 空名字不改动，也不影响取用。
        assert_eq!(take(1, "  ").unwrap().label, "这才是 它真正的样子");
        assert!(take(99, "").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
