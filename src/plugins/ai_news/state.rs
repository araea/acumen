//! 推送状态：分别记录每个群在实时线与定时线已推送的条目，并维护实时待发队列。
//!
//! 状态落盘到 `data/ai_news/state.json`，进程重启后仍然生效；
//! 超过保留期的记录会在每次写入时清理，文件不会无限增长。
//!
//! 去重有两层：**条目**按 id（同一篇不会推两遍），**事件**按标题与摘要的相似度
//! （同一件事的另一家报道不会再推，见 [`super::cluster`]）。后一层要拿新条目跟
//! 已经推过的比，所以已推条目会连标题带摘要留 36 小时，过了窗口只剩 id。

use super::api::Item;
use super::cluster::{self, Cluster, Fingerprint};
use super::render::Rendered;
use crate::plugins::get_data_dir;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;
use tokio::sync::Mutex as AsyncMutex;

const LOG_TARGET: &str = "Plugin/AiNews";
const STATE_FILE: &str = "state.json";
const EXTRACTION_RETAIN_DAYS: i64 = 30;
// 引用驱动里由插件自己存对应关系的只剩这一处，30 天是它的口径。

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeenEntry {
    /// 条目去重键（见 `api::Item::dedupe_key`）
    pub key: String,
    /// 推送时的 Unix 时间戳（秒）
    pub ts: i64,
    /// 推送时的标题与摘要，用来认出同一事件的后续报道。只留 [`cluster::WINDOW_SECONDS`]
    /// 之内的，更早的清成 `None`，状态文件不随天数膨胀。旧版落盘的记录没有这两项，
    /// 只能按 id 去重，等它们滑出窗口也就无所谓了。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl SeenEntry {
    fn new(key: String, item: Option<&Item>, ts: i64) -> Self {
        Self {
            key,
            ts,
            title: item.and_then(|i| i.title.clone()),
            summary: item.and_then(|i| i.summary.clone()),
        }
    }

    /// 还在事件窗口内、且留着文字的记录才有指纹
    fn fingerprint(&self, now: i64) -> Option<Fingerprint> {
        if now - self.ts > cluster::WINDOW_SECONDS {
            return None;
        }
        let title = self.title.as_deref()?;
        Some(Fingerprint::new(title, self.summary.as_deref().unwrap_or_default()))
    }
}

/// 一个待发事件：代表报道 + 已并进来的同事件报道
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingEntry {
    /// 代表报道的去重键
    pub key: String,
    /// 代表报道
    pub item: Item,
    /// 事件在官网时间轴上的最新时刻，用于保鲜淘汰和按时间顺序出队
    pub discovered_ts: i64,
    /// 并进来的其它报道；旧版落盘的队列没有这一项
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<Item>,
}

impl PendingEntry {
    fn cluster(&self) -> Cluster {
        Cluster {
            lead: self.item.clone(),
            also: self.also.clone(),
        }
    }

    fn holds(&self, key: &str) -> bool {
        self.key == key || self.also.iter().any(|i| i.dedupe_key().as_deref() == Some(key))
    }

    /// 并入一条同事件的报道。比代表更早报出的会顶替它，队列里的身份跟着换
    fn absorb(&mut self, item: Item, ts: i64) {
        let mut cluster = self.cluster();
        cluster.absorb(item);
        self.key = cluster.lead.dedupe_key().unwrap_or_else(|| self.key.clone());
        self.item = cluster.lead;
        self.also = cluster.also;
        self.discovered_ts = self.discovered_ts.max(ts);
    }

    /// 把另一个待发事件整个并进来（新到的一条同时像两个事件，说明它们是一件事）
    fn merge(&mut self, other: PendingEntry) {
        let ts = other.discovered_ts;
        for member in std::iter::once(other.item).chain(other.also) {
            self.absorb(member, ts);
        }
    }

    /// 与这个事件里任何一条像、且时间相近
    fn similarity_to(&self, print: &Fingerprint, ts: Option<i64>) -> Option<f64> {
        std::iter::once(&self.item)
            .chain(self.also.iter())
            .filter(|member| cluster::within_window(ts, member.timeline_ts()))
            .map(|member| print.similarity(&Fingerprint::of(member)))
            .filter(|s| *s >= cluster::SAME_EVENT)
            .max_by(f64::total_cmp)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GroupState {
    /// 实时快报去重记录。`seen` 是升级前实时/定时共用字段的兼容别名。
    #[serde(default, alias = "seen")]
    pub realtime_seen: Vec<SeenEntry>,
    /// 定时精选去重记录；与实时线分离，确保精选内容仍能按时形成回顾。
    #[serde(default)]
    pub brief_seen: Vec<SeenEntry>,
    /// 已推送过的最新日报日期，防止同一期日报重复推送
    #[serde(default)]
    pub last_daily_date: Option<String>,
    /// 实时推送的基线时间（Unix 秒）：只有收录时间晚于它的资讯才会被实时推送。
    ///
    /// 第一次轮询到本群时建立，之后不再变动——没有它，新装机器或刚开启推送的群
    /// 会把时间窗内的存量资讯当成「新消息」一次性倒出来。
    #[serde(default)]
    pub realtime_since: Option<i64>,
    /// 最近若干次实时推送的时间戳（Unix 秒），用于每小时频次上限；只保留最近一小时
    #[serde(default)]
    pub realtime_pushes: Vec<i64>,
    /// 已抓到但因单次条数、频次上限或发送失败尚未送达的实时资讯
    #[serde(default)]
    pub realtime_pending: Vec<PendingEntry>,
}

/// 一张已发送资讯卡片对应的可提取文本。消息 ID 与会话 ID 共同定位，避免
/// 不同实现端的局部消息 ID 碰撞。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionRecord {
    #[serde(deserialize_with = "string_or_number")]
    pub target_id: String,
    #[serde(deserialize_with = "string_or_number")]
    pub message_id: String,
    pub created_ts: i64,
    pub rendered: Rendered,
    /// 已经按卡片序号提取过的下标（0-based），避免同一内容被反复提取
    #[serde(default)]
    pub extracted: Vec<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub groups: HashMap<String, GroupState>,
    #[serde(default)]
    pub extractions: Vec<ExtractionRecord>,
}

/// 标识符在旧版本里是整数，现在是字符串；两种都认，免得一次协议升级把整份状态判成损坏
fn string_or_number<'de, D>(de: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match serde_json::Value::deserialize(de)? {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        other => Err(serde::de::Error::custom(format!("期望字符串或整数，得到 {other}"))),
    }
}

static STORE: OnceLock<AsyncMutex<Option<State>>> = OnceLock::new();

fn store() -> &'static AsyncMutex<Option<State>> {
    STORE.get_or_init(|| AsyncMutex::new(None))
}

async fn load_from_disk() -> State {
    let Ok(dir) = get_data_dir("ai_news").await else {
        warn!(target: LOG_TARGET, "无法创建数据目录，去重状态本次仅驻留内存。");
        return State::default();
    };
    let path = dir.join(STATE_FILE);
    let Ok(content) = tokio::fs::read_to_string(&path).await else {
        return State::default();
    };
    match parse_state(&content) {
        Ok(state) => state,
        Err(e) => {
            // 连 JSON 都不是。别直接覆盖：挪到一旁留作证据，也方便手工抢救。
            let aside = dir.join(format!("{}.broken-{}", STATE_FILE, Utc::now().timestamp()));
            let moved = tokio::fs::rename(&path, &aside).await.is_ok();
            warn!(
                target: LOG_TARGET,
                "去重状态文件无法解析（{}），{}，将重新开始记录。",
                e,
                if moved { format!("原文件已移到 {}", aside.display()) } else { "原文件保持原样".to_string() }
            );
            State::default()
        }
    }
}

/// 宽容地读状态：坏的只丢坏的那一条。
///
/// 2026-09-27 一次协议升级把标识符从整数改成字符串，一条旧的提取记录读不进来，
/// 整份状态被判成损坏、连 7 天的去重记录一起清空，第二天的精选速递把前一天
/// 实时推过的内容又推了一遍。现在按群、按提取记录逐条读，读不进的记一行日志、
/// 跳过，其余照常。
fn parse_state(content: &str) -> Result<State, serde_json::Error> {
    let raw: serde_json::Value = serde_json::from_str(content)?;
    let mut state = State::default();
    let (mut skipped_groups, mut skipped_records) = (0usize, 0usize);

    if let Some(groups) = raw.get("groups").and_then(|g| g.as_object()) {
        for (group_id, value) in groups {
            let mut group: GroupState = match serde_json::from_value(value.clone()) {
                Ok(group) => group,
                Err(e) => {
                    skipped_groups += 1;
                    warn!(target: LOG_TARGET, "去重状态里 {} 这一项读不进（{}），已跳过。", group_id, e);
                    continue;
                }
            };
            // 旧版只有一份 `seen`。首次升级时把它同时作为两条线的历史基线，既完成
            // 去重域拆分，也不会让进程重启后立刻重发最近几天的内容。
            if value.get("brief_seen").is_none() {
                group.brief_seen = group.realtime_seen.clone();
            }
            state.groups.insert(group_id.clone(), group);
        }
    }
    if let Some(records) = raw.get("extractions").and_then(|e| e.as_array()) {
        for value in records {
            match serde_json::from_value::<ExtractionRecord>(value.clone()) {
                Ok(record) => state.extractions.push(record),
                Err(e) => {
                    skipped_records += 1;
                    debug!(target: LOG_TARGET, "一条提取记录读不进（{}），已跳过。", e);
                }
            }
        }
    }
    if skipped_groups + skipped_records > 0 {
        warn!(
            target: LOG_TARGET,
            "去重状态部分读入：跳过 {} 个群、{} 条提取记录。", skipped_groups, skipped_records
        );
    }
    Ok(state)
}

async fn save_to_disk(state: &State) {
    let Ok(dir) = get_data_dir("ai_news").await else {
        return;
    };
    let path = dir.join(STATE_FILE);
    let temp_path = dir.join(format!("{}.tmp", STATE_FILE));
    match serde_json::to_string(state) {
        Ok(json) => {
            if let Err(e) = tokio::fs::write(&temp_path, json).await {
                warn!(target: LOG_TARGET, "去重状态写入失败: {}", e);
            } else if let Err(e) = tokio::fs::rename(&temp_path, &path).await {
                warn!(target: LOG_TARGET, "去重状态原子替换失败: {}", e);
            }
        }
        Err(e) => warn!(target: LOG_TARGET, "去重状态序列化失败: {}", e),
    }
}

/// 在全局锁内读改写状态，并把结果落盘
async fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut guard = store().lock().await;
    if guard.is_none() {
        *guard = Some(load_from_disk().await);
    }
    let state = guard.as_mut().expect("状态已在上一步初始化");
    let result = f(state);
    let snapshot = state.clone();
    save_to_disk(&snapshot).await;
    result
}

/// 预加载状态文件（插件初始化时调用，避免首次推送时才读盘）
pub async fn preload() {
    let mut guard = store().lock().await;
    if guard.is_none() {
        *guard = Some(load_from_disk().await);
    }
}

/// 挑出该群尚未在定时精选中推送过的条目（顺带清理过期记录）。
///
/// 「没推过」有两层：id 没记过；`fold` 打开时，这件事的别家报道也没推过。
/// 标记已推送是 [`mark_brief_seen`] 的职责——只有真正发出去了才记，
/// 否则条目数未达阈值或发送失败时就再也不会补推了。
pub async fn unseen_brief(
    group_id: String,
    items: Vec<(String, Item)>,
    retain_days: i64,
    fold: bool,
) -> Vec<(String, Item)> {
    let now = Utc::now().timestamp();
    let cutoff = now - retain_days.max(1) * 86_400;

    with_state(move |state| {
        let entry = state.groups.entry(group_id.clone()).or_default();
        entry.brief_seen.retain(|s| s.ts >= cutoff);
        forget_old_text(&mut entry.brief_seen, now);
        let known = recent_prints(&entry.brief_seen, now);

        items
            .into_iter()
            .filter(|(key, _)| !entry.brief_seen.iter().any(|s| &s.key == key))
            .filter(|(_, item)| !fold || !covered(&known, item))
            .collect()
    })
    .await
}

/// 记录这些条目已经通过定时精选推送给该群（折进事件里的报道也算送到了）。
pub async fn mark_brief_seen(group_id: String, sent: Vec<(String, Item)>) {
    let now = Utc::now().timestamp();
    with_state(move |state| {
        let entry = state.groups.entry(group_id.clone()).or_default();
        remember(&mut entry.brief_seen, sent, now);
    })
    .await
}

/// 把条目计入去重表（已在表内的不重复记），连标题摘要一起留着供事件比对
fn remember(history: &mut Vec<SeenEntry>, sent: Vec<(String, Item)>, now: i64) {
    for (key, item) in sent {
        if history.iter().any(|s| s.key == key) {
            continue;
        }
        history.push(SeenEntry::new(key, Some(&item), now));
    }
}

/// 给升级前落盘、没带文字的已推记录补上标题与摘要。
///
/// 事件比对靠已推条目的文字；升级前推过的那些只有 id，若不补，刚推完的一件事的
/// 后续报道在头 36 小时里认不出来。接口的 24 小时窗口里多半还留着它们，
/// 按 id 对上就能补，补一次以后不再有这件事。
pub async fn backfill_text(group_id: String, items: Vec<Item>) {
    let now = Utc::now().timestamp();
    with_state(move |state| {
        let Some(entry) = state.groups.get_mut(&group_id) else {
            return;
        };
        fill_missing_text(&mut entry.realtime_seen, &items, now);
        fill_missing_text(&mut entry.brief_seen, &items, now);
    })
    .await
}

fn fill_missing_text(history: &mut [SeenEntry], items: &[Item], now: i64) -> usize {
    let mut filled = 0;
    for seen in history
        .iter_mut()
        .filter(|s| s.title.is_none() && now - s.ts <= cluster::WINDOW_SECONDS)
    {
        if let Some(item) = items
            .iter()
            .find(|item| item.dedupe_key().as_deref() == Some(seen.key.as_str()))
        {
            seen.title = item.title.clone();
            seen.summary = item.summary.clone();
            filled += 1;
        }
    }
    filled
}

/// 事件窗口之外的记录只留 id，把标题与摘要丢掉
fn forget_old_text(history: &mut [SeenEntry], now: i64) {
    for entry in history.iter_mut() {
        if now - entry.ts > cluster::WINDOW_SECONDS {
            entry.title = None;
            entry.summary = None;
        }
    }
}

fn recent_prints(history: &[SeenEntry], now: i64) -> Vec<Fingerprint> {
    history.iter().filter_map(|s| s.fingerprint(now)).collect()
}

/// 这件事的别家报道是不是已经推过了
fn covered(known: &[Fingerprint], item: &Item) -> bool {
    let print = Fingerprint::of(item);
    known.iter().any(|seen| print.same_event(seen))
}

/// 某个群的实时推送状态（一次加锁取齐，避免逐项读写反复落盘）
#[derive(Debug, Clone, Copy)]
pub struct RealtimeStatus {
    /// 实时推送基线：只推收录时间晚于它的资讯
    pub since: i64,
    /// 本次调用刚刚建立基线——说明这是该群的第一轮，只对齐时间线，不推送
    pub just_primed: bool,
    /// 最近一小时内已经实时推送过几次
    pub pushes_last_hour: u32,
}

/// 读取该群的实时推送状态；首次调用会以当前时刻建立基线
pub async fn realtime_status(group_id: String) -> RealtimeStatus {
    let now = Utc::now().timestamp();
    with_state(move |state| {
        let entry = state.groups.entry(group_id.clone()).or_default();
        entry.realtime_pushes.retain(|ts| *ts > now - 3_600);

        let just_primed = entry.realtime_since.is_none();
        let since = *entry.realtime_since.get_or_insert(now);

        RealtimeStatus {
            since,
            just_primed,
            pushes_last_hour: entry.realtime_pushes.len() as u32,
        }
    })
    .await
}

/// 记录一次实时推送：条目计入去重，同时留下一个时间戳供频次上限统计
pub async fn mark_realtime_sent(group_id: String, sent: Vec<Cluster>) {
    let now = Utc::now().timestamp();
    with_state(move |state| {
        let entry = state.groups.entry(group_id.clone()).or_default();
        let members: Vec<(String, Item)> = sent
            .into_iter()
            .flat_map(|c| std::iter::once(c.lead).chain(c.also))
            .filter_map(|item| Some((item.dedupe_key()?, item)))
            .collect();
        let keys: HashSet<&str> = members.iter().map(|(key, _)| key.as_str()).collect();
        entry
            .realtime_pending
            .retain(|pending| !keys.contains(pending.key.as_str()));
        remember(&mut entry.realtime_seen, members, now);
        entry.realtime_pushes.push(now);
    })
    .await
}

/// 入队的结果：新排进去几个事件，又有几条报道并进了已有事件或已推事件
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Enqueued {
    pub added: usize,
    pub folded: usize,
}

/// 把新抓到的实时资讯并入持久队列；已发送或已在队列中的条目不会重复加入。
///
/// `fold` 打开时，还会认出「同一件事的另一家报道」：并进还没发的那个事件里
/// （发出去时卡片写「另有 N 家信源报道」），或者——这件事已经推过了——直接记为
/// 已送达，不再打扰。
pub async fn enqueue_realtime(
    group_id: String,
    items: Vec<(String, Item, i64)>,
    retain_days: i64,
    fold: bool,
) -> Enqueued {
    let now = Utc::now().timestamp();
    let cutoff = now - retain_days.max(1) * 86_400;
    with_state(move |state| {
        let entry = state.groups.entry(group_id.clone()).or_default();
        enqueue_pending(entry, items, cutoff, now, fold)
    })
    .await
}

fn enqueue_pending(
    entry: &mut GroupState,
    mut items: Vec<(String, Item, i64)>,
    seen_cutoff: i64,
    now: i64,
    fold: bool,
) -> Enqueued {
    entry
        .realtime_seen
        .retain(|seen| seen.ts >= seen_cutoff);
    forget_old_text(&mut entry.realtime_seen, now);
    let delivered = recent_prints(&entry.realtime_seen, now);

    // 先报出的先入队：同一批里的几家报道，最早的那条自然成为事件的代表
    items.sort_by_key(|(_, item, ts)| (item.first_reported_ts().unwrap_or(*ts), *ts));

    let mut out = Enqueued::default();
    for (key, item, ts) in items {
        if entry.realtime_seen.iter().any(|seen| seen.key == key)
            || entry.realtime_pending.iter().any(|pending| pending.holds(&key))
        {
            continue;
        }

        if fold {
            let print = Fingerprint::of(&item);
            let item_ts = item.timeline_ts();

            let hits: Vec<(usize, f64)> = entry
                .realtime_pending
                .iter()
                .enumerate()
                .filter_map(|(at, pending)| Some((at, pending.similarity_to(&print, item_ts)?)))
                .collect();
            if let Some(&(best, _)) = hits.iter().max_by(|a, b| a.1.total_cmp(&b.1)) {
                // 同时像好几个事件：它把它们接成了一件事（同一发布会各家写法差得远时常见），
                // 全部并进最像的那个
                let main = entry.realtime_pending[best].key.clone();
                let mut bridged = Vec::new();
                let mut others: Vec<usize> =
                    hits.iter().map(|&(at, _)| at).filter(|&at| at != best).collect();
                others.sort_unstable_by(|a, b| b.cmp(a));
                for at in others {
                    bridged.push(entry.realtime_pending.remove(at));
                }
                if let Some(target) = entry.realtime_pending.iter_mut().find(|p| p.key == main) {
                    target.absorb(item, ts);
                    for other in bridged {
                        target.merge(other);
                    }
                }
                out.folded += 1;
                continue;
            }
            if delivered.iter().any(|seen| print.same_event(seen)) {
                remember(&mut entry.realtime_seen, vec![(key, item)], now);
                out.folded += 1;
                continue;
            }
        }

        entry.realtime_pending.push(PendingEntry {
            key,
            item,
            discovered_ts: ts,
            also: Vec::new(),
        });
        out.added += 1;
    }
    entry.realtime_pending.sort_by_key(|pending| pending.discovered_ts);
    out
}

/// 查看下一批待发事件。过期条目以及已被定时档送达的条目会在这里淘汰。
pub async fn realtime_pending(
    group_id: String,
    max_items: usize,
    max_age_minutes: i64,
) -> Vec<Cluster> {
    let cutoff = Utc::now().timestamp() - max_age_minutes.max(1) * 60;
    with_state(move |state| {
        let entry = state.groups.entry(group_id.clone()).or_default();
        next_pending(entry, max_items, cutoff)
    })
    .await
}

pub async fn realtime_pending_count(group_id: String, max_age_minutes: i64) -> usize {
    let cutoff = Utc::now().timestamp() - max_age_minutes.max(1) * 60;
    with_state(move |state| {
        state
            .groups
            .get_mut(&group_id)
            .map_or(0, |entry| {
                prune_pending(entry, cutoff);
                entry.realtime_pending.len()
            })
    })
    .await
}

fn next_pending(entry: &mut GroupState, max_items: usize, freshness_cutoff: i64) -> Vec<Cluster> {
    prune_pending(entry, freshness_cutoff);
    entry
        .realtime_pending
        .iter()
        .take(max_items.max(1))
        .map(PendingEntry::cluster)
        .collect()
}

fn prune_pending(entry: &mut GroupState, freshness_cutoff: i64) {
    let seen: HashSet<&str> = entry
        .realtime_seen
        .iter()
        .map(|seen| seen.key.as_str())
        .collect();
    entry.realtime_pending.retain(|pending| {
        pending.discovered_ts >= freshness_cutoff && !seen.contains(pending.key.as_str())
    });
}

/// 把实时基线对齐到当前时刻，并清空旧频次窗口。
///
/// 群暂停实时快报后重新开启时调用，确保暂停期间积压的条目不会突然集中补发。
pub async fn align_realtime_baseline(group_id: String) {
    let now = Utc::now().timestamp();
    with_state(move |state| {
        let entry = state.groups.entry(group_id.clone()).or_default();
        entry.realtime_since = Some(now);
        entry.realtime_pushes.clear();
        entry.realtime_pending.clear();
    })
    .await
}

/// 该群是否已经推送过这一期日报
pub async fn has_pushed_daily(group_id: String, date: &str) -> bool {
    let date = date.to_string();
    with_state(move |state| {
        state
            .groups
            .get(&group_id)
            .and_then(|g| g.last_daily_date.as_deref())
            == Some(date.as_str())
    })
    .await
}

/// 记录该群已推送的日报期号
pub async fn mark_daily(group_id: String, date: &str) {
    let date = date.to_string();
    with_state(move |state| {
        let entry = state.groups.entry(group_id.clone()).or_default();
        entry.last_daily_date = Some(date);
    })
    .await
}

/// 清空某个群的去重记录（用于 `/ai推送重置`，便于重新推送一遍）
pub async fn reset_group(group_id: String) {
    with_state(move |state| {
        state.groups.remove(&group_id);
    })
    .await
}

/// 保存图片消息与其文本/链接的映射，供用户稍后引用图片提取。
pub async fn remember_extraction(target_id: String, message_id: String, rendered: Rendered) {
    let now = Utc::now().timestamp();
    let cutoff = now - EXTRACTION_RETAIN_DAYS * 86_400;
    with_state(move |state| {
        state.extractions.retain(|record| {
            record.created_ts >= cutoff
                && !(record.target_id == target_id && record.message_id == message_id)
        });
        state.extractions.push(ExtractionRecord {
            target_id,
            message_id,
            created_ts: now,
            rendered,
            extracted: Vec::new(),
        });
    })
    .await
}

/// 读取被引用卡片的可提取内容。超过 30 天的映射会顺手清理。
pub async fn extraction(target_id: String, message_id: &str) -> Option<Rendered> {
    let message_id = message_id.to_string();
    let cutoff = Utc::now().timestamp() - EXTRACTION_RETAIN_DAYS * 86_400;
    with_state(move |state| {
        state
            .extractions
            .retain(|record| record.created_ts >= cutoff);
        state
            .extractions
            .iter()
            .find(|record| record.target_id == target_id && record.message_id == message_id)
            .map(|record| record.rendered.clone())
    })
    .await
}

/// 一次「引用卡片 + 直接回复序号」的原子提取结果。
#[derive(Debug)]
pub enum ExtractionOutcome {
    /// 卡片存在，返回 (完整渲染, 本次新提取的 0-based 下标)
    Ready(Rendered, Vec<usize>),
    /// 未找到这张卡片的提取记录（可能已过期）
    Missing,
    /// 请求的条目此前均已提取过
    AlreadyExtracted,
}

/// 原子地挑选并标记一批条目为已提取，避免同一内容被反复提取。
///
/// `wanted` 为 0-based 下标。已在 `extracted` 中的条目会被跳过，
/// 只返回仍可提取的下标，并把它们并入已提取集合；全部被跳过时返回
/// [`ExtractionOutcome::AlreadyExtracted`]。
pub async fn extract_entries(
    target_id: String,
    message_id: &str,
    wanted: &[usize],
) -> ExtractionOutcome {
    let message_id = message_id.to_string();
    let wanted: BTreeSet<usize> = wanted.iter().copied().collect();
    let cutoff = Utc::now().timestamp() - EXTRACTION_RETAIN_DAYS * 86_400;
    with_state(move |state| apply_extraction(state, target_id, &message_id, &wanted, cutoff))
        .await
}

/// `extract_entries` 的纯逻辑部分，便于测试；不触碰全局状态与磁盘。
fn apply_extraction(
    state: &mut State,
    target_id: String,
    message_id: &str,
    wanted: &BTreeSet<usize>,
    cutoff: i64,
) -> ExtractionOutcome {
    state.extractions.retain(|record| record.created_ts >= cutoff);
    let Some(record) = state
        .extractions
        .iter_mut()
        .find(|record| record.target_id == target_id && record.message_id == message_id)
    else {
        return ExtractionOutcome::Missing;
    };

    let already: HashSet<usize> = record.extracted.iter().copied().collect();
    let fresh: Vec<usize> = wanted
        .iter()
        .copied()
        .filter(|index| !already.contains(index))
        .collect();
    if fresh.is_empty() {
        return ExtractionOutcome::AlreadyExtracted;
    }

    record.extracted.extend(wanted.iter().copied());
    record.extracted.sort_unstable();
    record.extracted.dedup();

    ExtractionOutcome::Ready(record.rendered.clone(), fresh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> Item {
        Item {
            id: Some(id.to_string()),
            title: Some(format!("资讯 {}", id)),
            ..Default::default()
        }
    }

    fn seen(key: &str, ts: i64) -> SeenEntry {
        SeenEntry::new(key.to_string(), None, ts)
    }

    fn keys(clusters: &[Cluster]) -> Vec<String> {
        clusters.iter().map(|c| c.lead.dedupe_key().unwrap()).collect()
    }

    #[test]
    fn pending_queue_is_deduplicated_ordered_and_fresh() {
        let mut group = GroupState::default();
        group.realtime_seen.push(seen("id:seen", 100));

        let added = enqueue_pending(
            &mut group,
            vec![
                ("id:newer".into(), item("newer"), 300),
                ("id:older".into(), item("older"), 200),
                ("id:seen".into(), item("seen"), 250),
                ("id:newer".into(), item("newer"), 300),
            ],
            0,
            1_000,
            true,
        );
        assert_eq!(added, Enqueued { added: 2, folded: 0 });
        assert_eq!(keys(&next_pending(&mut group, 1, 0)), ["id:older"]);

        group.realtime_seen.push(seen("id:older", 400));
        let remaining = next_pending(&mut group, 5, 250);
        assert_eq!(keys(&remaining), ["id:newer"]);
    }

    #[test]
    fn legacy_state_without_pending_queue_still_parses() {
        let state = parse_state(
            r#"{"groups":{"42":{"seen":[{"key":"id:old","ts":100}],"realtime_since":123,"realtime_pushes":[]}}}"#,
        )
        .unwrap();
        assert!(state.groups["42"].realtime_pending.is_empty());
        assert_eq!(state.groups["42"].realtime_seen.len(), 1);
        assert_eq!(state.groups["42"].brief_seen.len(), 1);
        assert!(state.extractions.is_empty());
    }

    #[test]
    fn extraction_records_roundtrip_and_default_for_legacy_state() {
        let state = State {
            extractions: vec![ExtractionRecord {
                target_id: "42".into(),
                message_id: "9001".into(),
                created_ts: 123,
                rendered: Rendered {
                    header: "AI 资讯".into(),
                    entries: vec!["1. 测试\n   🔗 https://example.com".into()],
                    footer: "AIHOT".into(),
                    links: Vec::new(),
                },
                extracted: Vec::new(),
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&state).unwrap();
        let parsed = parse_state(&json).unwrap();
        assert_eq!(parsed.extractions[0].message_id, "9001");
        assert_eq!(parsed.extractions[0].rendered.entries.len(), 1);

        let legacy = parse_state(r#"{"groups":{}}"#).unwrap();
        assert!(legacy.extractions.is_empty());
    }

    #[test]
    fn extraction_tracks_already_used_indices_and_skips_them() {
        let mut state = State {
            extractions: vec![ExtractionRecord {
                target_id: "42".into(),
                message_id: "m1".into(),
                created_ts: 100,
                rendered: Rendered {
                    header: "AI 资讯".into(),
                    entries: vec!["1. a".into(), "2. b".into(), "3. c".into()],
                    footer: "AIHOT".into(),
                    links: Vec::new(),
                },
                // 第 2 条（1-based）之前已提取
                extracted: vec![1],
            }],
            ..Default::default()
        };

        // 请求 1,2：其中 2 已提取，只返回 1-based 第 1 条
        let wanted: BTreeSet<usize> = [0usize, 1].into_iter().collect();
        match apply_extraction(&mut state, "42".into(), "m1", &wanted, 0) {
            ExtractionOutcome::Ready(_, fresh) => assert_eq!(fresh, vec![0]),
            _ => panic!("应返回 Ready"),
        }

        // 已提取下标并入记录，且顺序稳定
        assert_eq!(state.extractions[0].extracted, vec![0, 1]);

        // 再次请求同样的下标：全部已提取
        let wanted2: BTreeSet<usize> = [0usize, 1].into_iter().collect();
        assert!(matches!(
            apply_extraction(&mut state, "42".into(), "m1", &wanted2, 0),
            ExtractionOutcome::AlreadyExtracted
        ));

        // 请求未知下标的新条目仍可提取
        let wanted3: BTreeSet<usize> = [2usize].into_iter().collect();
        assert!(matches!(
            apply_extraction(&mut state, "42".into(), "m1", &wanted3, 0),
            ExtractionOutcome::Ready(_, _)
        ));

        // 未找到卡片
        let missing: BTreeSet<usize> = [0usize].into_iter().collect();
        assert!(matches!(
            apply_extraction(&mut state, "42".into(), "nope", &missing, 0),
            ExtractionOutcome::Missing
        ));
    }

    /// 同一份内容推到多个群，各群的卡片是独立的提取记录：
    /// 某群提取后被标记的序号不会殃及另群的卡片，各群仍可各自提取。
    #[test]
    fn extraction_records_are_isolated_per_target() {
        let mut state = State {
            extractions: vec![
                ExtractionRecord {
                    target_id: "111".into(),
                    message_id: "m_a".into(),
                    created_ts: 100,
                    rendered: Rendered {
                        header: "群A".into(),
                        entries: vec!["1. a".into(), "2. b".into()],
                        footer: "AIHOT".into(),
                        links: Vec::new(),
                    },
                    extracted: Vec::new(),
                },
                ExtractionRecord {
                    target_id: "222".into(),
                    message_id: "m_b".into(),
                    created_ts: 100,
                    rendered: Rendered {
                        header: "群B".into(),
                        entries: vec!["1. x".into(), "2. y".into()],
                        footer: "AIHOT".into(),
                        links: Vec::new(),
                    },
                    extracted: Vec::new(),
                },
            ],
            ..Default::default()
        };

        // 群 A 提取第 2 条
        let wanted_a: BTreeSet<usize> = [1usize].into_iter().collect();
        assert!(matches!(
            apply_extraction(&mut state, "111".into(), "m_a", &wanted_a, 0),
            ExtractionOutcome::Ready(_, _)
        ));
        // A 已标记，B 不受影响
        assert_eq!(state.extractions[0].extracted, vec![1]);
        assert!(state.extractions[1].extracted.is_empty());

        // 群 B 仍可提取自己的第 2 条（序号互不干扰）
        let wanted_b: BTreeSet<usize> = [1usize].into_iter().collect();
        assert!(matches!(
            apply_extraction(&mut state, "222".into(), "m_b", &wanted_b, 0),
            ExtractionOutcome::Ready(_, _)
        ));
        assert_eq!(state.extractions[1].extracted, vec![1]);
    }

    #[test]
    fn realtime_and_brief_histories_are_independent() {
        let mut group = GroupState::default();
        remember(&mut group.realtime_seen, vec![("id:same".into(), item("same"))], 100);
        assert!(group.brief_seen.is_empty());

        remember(&mut group.brief_seen, vec![("id:same".into(), item("same"))], 200);
        assert_eq!(group.realtime_seen[0].ts, 100);
        assert_eq!(group.brief_seen[0].ts, 200);
    }

    // ---- 事件折叠 ----

    use crate::plugins::ai_news::cluster::fixture;

    /// Sonnet 5.5 发布那晚的五家报道，按各自被 AIHOT 收录的先后排好
    fn sonnet() -> Vec<(String, Item, i64)> {
        let (items, groups) = fixture::load();
        let mut batch: Vec<(String, Item, i64)> = fixture::pick(&items, &groups["sonnet"])
            .into_iter()
            .map(|item| (item.dedupe_key().unwrap(), item.clone(), item.timeline_ts().unwrap()))
            .collect();
        batch.sort_by_key(|(_, _, ts)| *ts);
        batch
    }

    fn clock(items: &[(String, Item, i64)]) -> i64 {
        items.iter().map(|(_, _, ts)| *ts).max().unwrap() + 60
    }

    #[test]
    fn five_reports_of_one_launch_queue_as_one_event() {
        let batch = sonnet();
        let now = clock(&batch);
        let mut group = GroupState::default();

        let out = enqueue_pending(&mut group, batch, 0, now, true);
        assert_eq!(out, Enqueued { added: 1, folded: 4 });

        let due = next_pending(&mut group, 30, 0);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].also.len(), 4);
        assert_eq!(due[0].keys().len(), 5);
    }

    #[test]
    fn reports_trickling_in_over_several_polls_still_fold() {
        // 真实那晚的节奏：第一轮两条（YouTube 一式两份），过几分钟 HN，再过一会儿别家。
        // 逐轮入队，队列里始终只有一个事件。
        let batch = sonnet();
        let now = clock(&batch);
        let mut group = GroupState::default();
        for one in batch {
            enqueue_pending(&mut group, vec![one], 0, now, true);
        }
        // Hacker News 那条与 YouTube 两条只有三成像，先各自成事件；后到的官方号与转载
        // 同时像两边，把它们接成了一件事
        assert_eq!(group.realtime_pending.len(), 1);
        assert_eq!(group.realtime_pending[0].also.len(), 4);
    }

    #[test]
    fn a_report_of_an_event_already_delivered_is_recorded_not_queued() {
        // Agent Arena 的同一则战报，隔半小时来一遍
        let (items, groups) = fixture::load();
        let mut batch: Vec<(String, Item, i64)> = fixture::pick(&items, &groups["arena"])
            .into_iter()
            .map(|item| (item.dedupe_key().unwrap(), item.clone(), item.timeline_ts().unwrap()))
            .collect();
        batch.sort_by_key(|(_, _, ts)| *ts);
        let now = clock(&batch);
        let mut group = GroupState::default();

        // 第一条已经推出去了
        let first = batch[0].clone();
        remember(&mut group.realtime_seen, vec![(first.0.clone(), first.1.clone())], now - 300);

        let out = enqueue_pending(&mut group, batch[1..].to_vec(), 0, now, true);
        assert_eq!(out, Enqueued { added: 0, folded: 2 });
        assert!(group.realtime_pending.is_empty());
        // 记成已送达，下一轮它们还在接口里，也不会再被当成新条目
        assert_eq!(group.realtime_seen.len(), 3);
        let again = enqueue_pending(&mut group, batch[1..].to_vec(), 0, now + 60, true);
        assert_eq!(again, Enqueued::default());
    }

    #[test]
    fn turning_folding_off_queues_every_report() {
        let batch = sonnet();
        let now = clock(&batch);
        let mut group = GroupState::default();
        let out = enqueue_pending(&mut group, batch, 0, now, false);
        assert_eq!(out, Enqueued { added: 5, folded: 0 });
    }

    #[test]
    fn the_earliest_reporter_takes_over_as_lead_when_it_arrives_late() {
        // 转载先到、官方号后到：卡片上应当是官方号
        let mut batch = sonnet();
        let official = batch
            .iter()
            .min_by_key(|(_, item, _)| item.first_reported_ts())
            .cloned()
            .unwrap();
        batch.retain(|(key, _, _)| *key != official.0);
        let now = clock(&batch) + 600;

        let mut group = GroupState::default();
        enqueue_pending(&mut group, batch, 0, now, true);
        assert_ne!(group.realtime_pending[0].key, official.0);

        enqueue_pending(&mut group, vec![official.clone()], 0, now, true);
        assert_eq!(group.realtime_pending.len(), 1);
        assert_eq!(group.realtime_pending[0].key, official.0);
        assert_eq!(group.realtime_pending[0].also.len(), 4);
    }

    #[test]
    fn sending_an_event_marks_every_member_delivered() {
        let batch = sonnet();
        let now = clock(&batch);
        let mut group = GroupState::default();
        enqueue_pending(&mut group, batch, 0, now, true);
        let due = next_pending(&mut group, 30, 0);

        // 与 mark_realtime_sent 同样的动作
        let members: Vec<(String, Item)> = due
            .into_iter()
            .flat_map(|c| std::iter::once(c.lead).chain(c.also))
            .map(|item| (item.dedupe_key().unwrap(), item))
            .collect();
        group.realtime_pending.clear();
        remember(&mut group.realtime_seen, members, now);
        assert_eq!(group.realtime_seen.len(), 5);
        assert!(group.realtime_seen.iter().all(|s| s.title.is_some()));
    }

    #[test]
    fn delivered_text_is_forgotten_after_the_event_window() {
        let mut history = vec![
            SeenEntry::new("id:old".into(), Some(&item("old")), 1_000),
            SeenEntry::new("id:new".into(), Some(&item("new")), 1_000 + cluster::WINDOW_SECONDS),
        ];
        forget_old_text(&mut history, 1_000 + cluster::WINDOW_SECONDS + 1);
        assert!(history[0].title.is_none() && history[0].summary.is_none());
        assert!(history[1].title.is_some());
        assert_eq!(history[0].key, "id:old", "id 还在，条目级去重不受影响");
    }

    #[test]
    fn an_old_delivery_no_longer_shadows_a_fresh_report_of_the_same_words() {
        // 三天前推过同样措辞的一条：那是另一回事，这条要推
        let batch = sonnet();
        let now = clock(&batch);
        let mut group = GroupState::default();
        let old = batch[0].clone();
        remember(&mut group.realtime_seen, vec![(old.0.clone(), old.1.clone())], now - 3 * 86_400);

        let mut fresh = batch[1].clone();
        fresh.0 = "id:fresh".into();
        let out = enqueue_pending(&mut group, vec![fresh], 0, now, true);
        assert_eq!(out, Enqueued { added: 1, folded: 0 });
    }

    #[test]
    fn records_from_before_this_release_get_their_text_back_from_the_feed() {
        let batch = sonnet();
        let now = clock(&batch);
        let mut history = vec![
            seen(&batch[0].0, now - 600),
            seen("id:not-in-feed", now - 600),
            seen("id:days-ago", now - 2 * cluster::WINDOW_SECONDS),
        ];
        let mut days_ago = batch[1].1.clone();
        days_ago.id = Some("days-ago".into());
        let mut feed: Vec<Item> = batch.iter().map(|(_, item, _)| item.clone()).collect();
        feed.push(days_ago);

        assert_eq!(fill_missing_text(&mut history, &feed, now), 1);
        assert!(history[0].title.is_some());
        assert!(history[1].title.is_none(), "接口里没有的补不了");
        assert!(history[2].title.is_none(), "窗口之外的不补");

        // 补完之后，同一件事的后续报道就认得出来了
        let mut group = GroupState::default();
        group.realtime_seen = history;
        let out = enqueue_pending(&mut group, vec![batch[1].clone()], 0, now, true);
        assert_eq!(out, Enqueued { added: 0, folded: 1 });
        assert_eq!(fill_missing_text(&mut group.realtime_seen, &feed, now), 0, "补过不再补");
    }

    #[test]
    fn brief_history_also_recognises_a_delivered_event() {
        let batch = sonnet();
        let now = clock(&batch);
        let mut group = GroupState::default();
        remember(
            &mut group.brief_seen,
            vec![(batch[0].0.clone(), batch[0].1.clone())],
            now - 3_600,
        );
        let known = recent_prints(&group.brief_seen, now);
        assert!(covered(&known, &batch[3].1));
        assert!(!covered(&[], &batch[3].1));
    }

    // ---- 读盘 ----

    #[test]
    fn numeric_ids_from_an_older_release_are_still_read() {
        let state = parse_state(
            r#"{"groups":{},"extractions":[{"target_id":175131947,"message_id":9001,"created_ts":5,
                "rendered":{"header":"h","entries":[],"footer":"f"}}]}"#,
        )
        .unwrap();
        assert_eq!(state.extractions[0].target_id, "175131947");
        assert_eq!(state.extractions[0].message_id, "9001");
    }

    #[test]
    fn one_unreadable_record_does_not_wipe_the_rest() {
        let state = parse_state(
            r#"{"groups":{
                "g:1":{"realtime_seen":[{"key":"id:a","ts":1}],"realtime_since":9},
                "g:2":{"realtime_seen":"这不是数组"}
              },
              "extractions":[
                {"target_id":{"坏":1},"message_id":"m","created_ts":5,"rendered":{"header":"","entries":[],"footer":""}},
                {"target_id":"g:1","message_id":"m2","created_ts":6,"rendered":{"header":"","entries":[],"footer":""}}
              ]}"#,
        )
        .unwrap();
        assert_eq!(state.groups.len(), 1);
        assert_eq!(state.groups["g:1"].realtime_seen[0].key, "id:a");
        assert_eq!(state.extractions.len(), 1);
        assert_eq!(state.extractions[0].message_id, "m2");
    }

    #[test]
    fn a_file_that_is_not_json_is_reported_not_silently_accepted() {
        assert!(parse_state("{\"groups\":").is_err());
    }

    #[test]
    fn seen_entries_written_before_this_release_still_load_without_text() {
        let state = parse_state(r#"{"groups":{"g:1":{"realtime_seen":[{"key":"id:x","ts":7}]}}}"#).unwrap();
        let entry = &state.groups["g:1"].realtime_seen[0];
        assert!(entry.title.is_none());
        assert!(entry.fingerprint(8).is_none(), "没有文字就不参与事件比对");
        // 新格式落盘时没有文字的字段不写出
        let json = serde_json::to_string(entry).unwrap();
        assert!(!json.contains("title"), "{json}");
    }
}
