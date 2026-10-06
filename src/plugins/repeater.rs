//! 复读机：同一句话在频道里被接力说到阈值时，机器人跟读一次。
//!
//! 判定不看原始消息 JSON，而是先抽一条可比较的**指纹**：文本折叠空白，
//! 图片/表情取资源 ID。同一张图换个带 token 的链接再发，仍算同一句话。
//!
//! 有些消息不该跟读，指纹阶段直接判定为不可复读并**打断当前接力**：
//!
//!   - 带 `@`、引用、转发、文件、卡片的消息——跟读等于机器人替人 at，
//!     或者刷出一张没有上下文的卡片；
//!   - 指令消息（`ignore_commands`）——两个人发 `/help`，机器人不必跟着刷；
//!   - 超过 `max_chars` 的长文——复读长文是纯刷屏。
//!
//! Bot 自己说的话（`BeforeSend` 拦截到的发送包，以及实现端回显的自身消息）
//! 只更新状态、不参与计数，避免自己接自己的话形成连锁。
//! 每个频道另存最近 128 条已跟读（含打断）的内容指纹，跨接力去重：
//! 同一句话跟读过一次，`remember_hours` 小时内再怎么接力也不再跟读。
//! 记录以发送回执为准，回执丢了（报错但其实发出去了）也能由自己的回显补记；
//! 未实际发送的候选不入记录。记录落盘在 `data/repeater/recent.json`，
//! 重启、重连都不会忘。
//!
//! 触发点上依次过三道闸：冷却 → 概率 → 打断。命中打断则改发一句打断语，
//! 概率未命中不置位 `repeated`，同一句话的下一条仍有机会触发。

use crate::adapters::satori::{Freshness, LockedWriter, send_guarded_msg};
use crate::command::get_prefixes;
use crate::event::{Context, EventType, SendGuard, SendPacket};
use crate::message::Message;
use crate::plugins::{
    ChannelConfig, Receipt, PluginConfig, PluginError, get_config_or_default,
};
use futures_util::future::BoxFuture;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use simd_json::OwnedValue;
use simd_json::base::{ValueAsArray, ValueAsMutObject};
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsArray, ValueObjectAccessAsScalar};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

const LOG_TARGET: &str = "Plugin/Repeater";

// ================= 配置定义 =================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RepeaterConfig {
    pub enabled: bool,
    /// 同一句话累计到几条开始跟读（小于 2 按 2 处理，否则等于逢消息必复读）
    pub min_times: usize,
    /// 触发概率，取值 0~1
    pub probability: f64,
    /// 同一频道两次复读之间的冷却秒数，0 为不限制
    pub cooldown_seconds: u64,
    /// 触发消息超过此毫秒数就放弃跟读（至少 1ms），避免补发旧话题
    pub max_delay_ms: u64,
    /// 参与判定的文本长度上限，超长不复读；0 为不限制
    pub max_chars: usize,
    /// 是否允许同一个人自己刷屏凑够阈值
    pub allow_same_user: bool,
    /// 是否跳过指令消息
    pub ignore_commands: bool,
    /// 是否允许复读图片与表情
    pub allow_media: bool,
    /// 打断复读的概率：在本该跟读时改为发送一句打断语
    pub interrupt_probability: f64,
    /// 打断语文案池，随机取一条
    pub interrupt_texts: Vec<String>,
    /// 跟读过的内容多少小时内不再跟读（跨重启保留）；0 为只按条数上限淘汰
    pub remember_hours: u64,
    /// 群黑白名单
    pub channel: ChannelConfig,
}

impl Default for RepeaterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_times: 2,
            probability: 1.0,
            cooldown_seconds: 15,
            max_delay_ms: 3000,
            max_chars: 200,
            allow_same_user: false,
            ignore_commands: true,
            allow_media: true,
            interrupt_probability: 0.0,
            interrupt_texts: vec!["打断复读".to_string(), "打断施法".to_string()],
            remember_hours: 24,
            channel: ChannelConfig::default(),
        }
    }
}

impl PluginConfig for RepeaterConfig {
    const NAME: &'static str = "repeater";
}


impl RepeaterConfig {
    /// min_times = 0/1 等价于逢消息必复读，收敛到 2
    fn threshold(&self) -> usize {
        self.min_times.max(2)
    }

    fn remember_seconds(&self) -> Option<u64> {
        (self.remember_hours > 0).then(|| self.remember_hours.saturating_mul(3600))
    }
}

// ================= 状态定义 =================

/// 一条消息的来源：参与计数的人，或不参与计数的 Bot 自身
#[derive(Debug, Clone, PartialEq, Eq)]
enum Sender {
    User(String),
    Bot,
}

/// 一条跟读记录：内容指纹与跟读时刻（秒）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Remembered {
    sig: String,
    at: u64,
}

#[derive(Debug, Default, Clone)]
struct ChannelState {
    /// 当前接力中的消息指纹，空串表示接力已被打断
    sig: String,
    /// 复读时原样发出的消息内容
    content: OwnedValue,
    /// 当前指纹已累计的条数
    times: usize,
    /// 本轮是否已经跟读过
    repeated: bool,
    /// 已确认发送的原接力指纹，独立于当前接力，按发送顺序淘汰
    recent_repeats: VecDeque<Remembered>,
    /// 上一条消息的来源
    last_sender: Option<Sender>,
    /// 上次实际复读的时间戳（跨轮保留，用于冷却）
    last_repeat_at: u64,
    last_active: u64,
    generation: u64,
}

impl ChannelState {
    /// 换了一句话：重置接力，但保留已复读记录、冷却与活跃时间
    fn restart(&mut self, sig: String, content: OwnedValue, sender: Sender) {
        self.generation = next_generation();
        // Bot 自己刚说的话和近期已跟读的内容都不再跟读。
        self.repeated = sender == Sender::Bot || self.has_repeated(&sig);
        self.sig = sig;
        self.content = content;
        self.times = if sender == Sender::Bot { 0 } else { 1 };
        self.last_sender = Some(sender);
    }

    fn has_repeated(&self, sig: &str) -> bool {
        self.recent_repeats.iter().any(|known| known.sig == sig)
    }

    fn remember_repeat(&mut self, sig: &str, now: u64) {
        if self.sig == sig {
            self.repeated = true;
        }
        if self.has_repeated(sig) {
            return;
        }
        if self.recent_repeats.len() >= MAX_RECENT_REPEATS {
            self.recent_repeats.pop_front();
        }
        self.recent_repeats.push_back(Remembered {
            sig: sig.to_owned(),
            at: now,
        });
        DIRTY.store(true, Ordering::Relaxed);
    }

    /// 超过记忆时长的跟读记录作废，那句话可以重新接力
    fn forget_expired(&mut self, config: &RepeaterConfig, now: u64) {
        if let Some(window) = config.remember_seconds() {
            let before = self.recent_repeats.len();
            self.recent_repeats
                .retain(|known| now.saturating_sub(known.at) < window);
            if self.recent_repeats.len() != before {
                DIRTY.store(true, Ordering::Relaxed);
            }
        }
    }

    /// 打断接力：下一条消息一律从头开始数
    fn interrupt_chain(&mut self) {
        self.generation = next_generation();
        self.sig.clear();
        self.times = 0;
        self.repeated = false;
        self.last_sender = None;
    }
}

static STATES: OnceLock<Mutex<HashMap<String, ChannelState>>> = OnceLock::new();

/// 取状态表。锁中毒说明此前某次持锁 panic 过，状态本身仍可用，不再连坐 panic。
/// 首次取用时从磁盘恢复跟读记录。
fn states() -> MutexGuard<'static, HashMap<String, ChannelState>> {
    STATES
        .get_or_init(|| Mutex::new(load_memory()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

// ================= 跟读记录落盘 =================

/// 跟读记录有变动、尚未落盘
static DIRTY: AtomicBool = AtomicBool::new(false);
const MEMORY_FILE: &str = "recent.json";

/// 状态表是同步锁，这里只能同步读写，所以不走异步的 `get_data_dir`。
fn memory_path() -> Option<PathBuf> {
    Some(crate::storage::data_path("repeater").ok()?.join(MEMORY_FILE))
}

fn encode_memory(map: &HashMap<String, ChannelState>) -> serde_json::Result<String> {
    let saved: HashMap<&str, &VecDeque<Remembered>> = map
        .iter()
        .filter(|(_, state)| !state.recent_repeats.is_empty())
        .map(|(key, state)| (key.as_str(), &state.recent_repeats))
        .collect();
    serde_json::to_string(&saved)
}

fn decode_memory(text: &str, now: u64) -> serde_json::Result<HashMap<String, ChannelState>> {
    let saved: HashMap<String, VecDeque<Remembered>> = serde_json::from_str(text)?;
    Ok(saved
        .into_iter()
        .map(|(key, recent_repeats)| {
            let state = ChannelState {
                recent_repeats,
                last_active: now,
                ..Default::default()
            };
            (key, state)
        })
        .collect())
}

fn load_memory() -> HashMap<String, ChannelState> {
    let Some(text) = memory_path().and_then(|path| std::fs::read_to_string(path).ok()) else {
        return HashMap::new();
    };
    decode_memory(&text, now_secs()).unwrap_or_else(|e| {
        warn!(target: LOG_TARGET, "跟读记录解析失败({e})，将重新开始记录。");
        HashMap::new()
    })
}

/// 跟读记录有变动就整份写回。调用方持有状态锁，写入顺序即变动顺序；
/// 一频道十几秒最多跟读一次，文件也就几 KB，同步写无妨。
fn persist(map: &HashMap<String, ChannelState>) {
    if !DIRTY.swap(false, Ordering::Relaxed) {
        return;
    }
    let Some(path) = memory_path() else {
        return;
    };
    let result = encode_memory(map)
        .map_err(std::io::Error::other)
        .and_then(|json| crate::storage::write_atomic(&path, json.as_bytes()));
    if let Err(e) = result {
        warn!(target: LOG_TARGET, "跟读记录写入失败: {e}");
    }
}

// 长期运行的 Bot 中频道数量可能膨胀。超过上限时先清理空闲频道，
// 仍超限则按最后活跃时间淘汰最旧的，保证表长有界。
const MAX_CHANNELS: usize = 4096;
const MAX_RECENT_REPEATS: usize = 128;
const IDLE_TIMEOUT_SECS: u64 = 60 * 60; // 1 小时

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn maybe_evict(map: &mut HashMap<String, ChannelState>, now: u64) {
    if map.len() <= MAX_CHANNELS {
        return;
    }
    map.retain(|_, st| now.saturating_sub(st.last_active) < IDLE_TIMEOUT_SECS);
    if map.len() <= MAX_CHANNELS {
        return;
    }
    let mut by_age: Vec<(u64, String)> = map
        .iter()
        .map(|(key, st)| (st.last_active, key.clone()))
        .collect();
    by_age.sort_unstable_by_key(|(active, _)| *active);
    for (_, key) in by_age.into_iter().take(map.len() - MAX_CHANNELS) {
        map.remove(&key);
    }
}

// ================= 指纹 =================

/// 折叠空白：换行、空格数量不同的同一句话仍算同一句
fn normalize_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_query(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

/// 取媒体段的稳定标识：优先资源 ID，退而求其次用去掉查询串的链接
fn media_key(type_: &str, data: &OwnedValue) -> Option<String> {
    let pick = |keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|key| {
                data.get_str(*key)
                    .map(str::to_string)
                    .or_else(|| data.get_i64(*key).map(|v| v.to_string()))
                    .or_else(|| data.get_u64(*key).map(|v| v.to_string()))
            })
            .filter(|value| !value.is_empty())
    };
    match type_ {
        "image" => pick(&["file", "md5", "file_unique", "file_id"])
            .or_else(|| pick(&["url"]).map(|url| strip_query(&url).to_string()))
            .map(|key| format!("i:{key}")),
        "face" => pick(&["id"]).map(|key| format!("f:{key}")),
        "mface" => pick(&["emoji_id", "summary", "key", "id"]).map(|key| format!("m:{key}")),
        _ => None,
    }
}

/// 消息指纹。返回 None 表示这条消息不可复读，当前接力就此打断。
fn signature(message: &[OwnedValue], config: &RepeaterConfig) -> Option<String> {
    let mut parts: Vec<String> = Vec::with_capacity(message.len());
    let mut chars = 0usize;

    for segment in message {
        let type_ = segment.get_str("type")?;
        let data = segment.get("data");
        match type_ {
            "text" => {
                let text = normalize_text(data.and_then(|d| d.get_str("text")).unwrap_or(""));
                if text.is_empty() {
                    continue;
                }
                chars += text.chars().count();
                parts.push(format!("t:{text}"));
            }
            "image" | "face" | "mface" if config.allow_media => {
                parts.push(media_key(type_, data?)?);
            }
            // at / reply / forward / file / json / poke ……跟读它们只会误伤
            _ => return None,
        }
    }

    if parts.is_empty() {
        return None;
    }
    if config.max_chars > 0 && chars > config.max_chars {
        return None;
    }
    Some(parts.join("|"))
}

fn is_command(prefixes: &[String], text: &str) -> bool {
    let text = text.trim_start();
    prefixes
        .iter()
        .any(|prefix| !prefix.is_empty() && text.starts_with(prefix.as_str()))
}

fn channel_key(bot_id: &str, group_id: Option<&str>, user_id: &str) -> Option<String> {
    match group_id {
        Some(gid) => Some(format!("{bot_id}#g{gid}")),
        None if !user_id.is_empty() => Some(format!("{bot_id}#p{user_id}")),
        _ => None,
    }
}

// ================= 触发判定 =================

#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    /// 只更新状态
    Silent,
    /// 原样跟读
    Repeat,
    /// 打断复读
    Interrupt(String),
}

fn roll(probability: f64) -> bool {
    if probability >= 1.0 {
        return true;
    }
    if probability <= 0.0 {
        return false;
    }
    rand::rng().random_bool(probability)
}

fn pick_interrupt(texts: &[String]) -> Option<String> {
    let candidates: Vec<&String> = texts.iter().filter(|t| !t.trim().is_empty()).collect();
    match candidates.len() {
        0 => None,
        1 => Some(candidates[0].clone()),
        n => Some(candidates[rand::rng().random_range(0..n)].clone()),
    }
}

/// 把一条消息喂给频道状态，返回该做什么。纯逻辑，便于单测。
fn feed(
    state: &mut ChannelState,
    sig: String,
    content: OwnedValue,
    sender: Sender,
    config: &RepeaterConfig,
    now: u64,
) -> Action {
    state.last_active = now;
    state.forget_expired(config, now);

    if state.sig != sig {
        state.restart(sig, content, sender);
        return Action::Silent;
    }

    // Bot 自己重复了这句话：只压住后续跟读，不计数
    if sender == Sender::Bot {
        // 有人在接力的话从 Bot 嘴里出来了，就算跟读过。复读的回执可能因报错丢失，
        // 自己的回显是它确实发出去的第二个凭据；别的插件恰好说了同一句也一样。
        if state.times > 0 {
            state.remember_repeat(&sig, now);
        }
        state.generation = next_generation();
        state.repeated = true;
        state.last_sender = Some(Sender::Bot);
        return Action::Silent;
    }

    // 同一个人连着刷，默认不算接力
    if !config.allow_same_user && state.last_sender.as_ref() == Some(&sender) {
        return Action::Silent;
    }

    state.times += 1;
    state.last_sender = Some(sender);

    if state.repeated || state.times < config.threshold() {
        return Action::Silent;
    }
    if config.cooldown_seconds > 0
        && now.saturating_sub(state.last_repeat_at) < config.cooldown_seconds
    {
        return Action::Silent;
    }
    // 概率没命中不置位，同一句话的下一条还能再摇一次
    if !roll(config.probability) {
        return Action::Silent;
    }

    state.repeated = true;
    state.last_repeat_at = now;

    if roll(config.interrupt_probability)
        && let Some(text) = pick_interrupt(&config.interrupt_texts)
    {
        return Action::Interrupt(text);
    }
    Action::Repeat
}

// ================= 逻辑实现 =================

/// 不可复读的消息：打断接力后返回
fn break_chain(key: String, now: u64) {
    let mut map = states();
    maybe_evict(&mut map, now);
    let state = map.entry(key).or_default();
    state.last_active = now;
    state.interrupt_chain();
}

fn observe(
    key: String,
    sig: String,
    content: OwnedValue,
    sender: Sender,
    config: &RepeaterConfig,
    now: u64,
) -> Action {
    let mut map = states();
    maybe_evict(&mut map, now);
    let state = map.entry(key).or_default();
    let action = feed(state, sig, content, sender, config, now);
    persist(&map);
    action
}

// Updated synchronously at ingress, before event tasks can be reordered.
const OBSERVED: &str = "_acumen_repeater_observed";

fn next_generation() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Debug, Clone)]
pub struct RepeatGuard {
    key: String,
    /// 触发接力的指纹；发送打断语时也应记住原内容
    sig: String,
    generation: u64,
    expires_at: u64,
    message_id: String,
}

impl SendGuard for RepeatGuard {
    fn is_current(&self) -> bool {
        now_ms() < self.expires_at
            && states().get(&self.key).is_some_and(|state| {
                state.generation == self.generation && !state.has_repeated(&self.sig)
            })
    }

    /// 接力条件本身就是一份时效条件：锚点是触发这次复读的那条消息。
    fn freshness(&self) -> Option<Freshness> {
        Some(Freshness {
            message_id: self.message_id.clone(),
            expires_at: self.expires_at,
        })
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct PreparedRepeat {
    guard: RepeatGuard,
    group_id: Option<String>,
    user_id: String,
    content: OwnedValue,
}

fn scoped_key(
    ctx: &Context,
    writer: &LockedWriter,
    group: Option<&str>,
    user: &str,
) -> Option<String> {
    channel_key(
        &format!(
            "{}|{}|{}",
            writer.connection_key(),
            ctx.bot.platform,
            ctx.bot.login_user.get().id
        ),
        group,
        user,
    )
}

/// Consumed interactive input still breaks a conversation's pending repeat.
pub fn interrupt(ctx: &Context, writer: &LockedWriter) {
    if let Some(msg) = ctx.as_message()
        && let Some(key) = scoped_key(
            ctx,
            writer,
            msg.group_id(),
            msg.user_id(),
        )
    {
        break_chain(key, now_secs());
    }
}

/// 发出之后的钩子：确认一次条件发送，把它记成已跟读。
pub fn on_sent<'a>(
    ctx: &'a Context,
    _writer: &'a LockedWriter,
    sent: &'a Receipt<'a>,
) -> BoxFuture<'a, ()> {
    Box::pin(async move {
        if !sent.message_ids.is_empty() {
            confirm_send(ctx, sent.packet);
        }
    })
}

/// Record a confirmed conditional send without overwriting a newer conversation.
fn confirm_send(ctx: &Context, packet: &SendPacket) {
    let Some(guard) = packet
        .guard
        .as_ref()
        .and_then(|guard| guard.as_any().downcast_ref::<RepeatGuard>())
    else {
        return;
    };
    let config: RepeaterConfig = get_config_or_default(ctx);
    let content = packet.message().cloned().unwrap_or_default();
    let now = now_secs();
    let mut map = states();
    let Some(state) = map.get_mut(&guard.key) else {
        return;
    };
    // 回执可能晚于下一条入站消息；仍记录已发送的内容，但不覆盖新接力。
    state.remember_repeat(&guard.sig, now);
    if state.generation == guard.generation {
        match content.as_array().and_then(|arr| signature(arr, &config)) {
            Some(sig) => {
                feed(state, sig, content, Sender::Bot, &config, now);
            }
            None => state.interrupt_chain(),
        }
    }
    persist(&map);
}

pub fn prepare(ctx: &mut Context, writer: &LockedWriter) -> Option<PreparedRepeat> {
    let EventType::Satori(event) = &mut ctx.event else {
        return None;
    };
    if event.get_str("post_type") != Some("message") || event.get_bool(OBSERVED) == Some(true) {
        return None;
    }
    event.as_object_mut()?.insert(OBSERVED.into(), true.into());
    let config: RepeaterConfig = get_config_or_default(ctx);
    let msg = ctx.as_message()?;
    let group_id = msg.group_id();
    if !config.channel.allows(group_id) {
        return None;
    }
    let user_id = msg.user_id();
    let key = scoped_key(ctx, writer, group_id, user_id)?;
    let EventType::Satori(event) = &ctx.event else {
        return None;
    };
    let now = now_ms();
    let source = event
        .get("_satori")
        .and_then(|body| {
            body.get("message")
                .and_then(|m| m.get_u64("created_at"))
                .filter(|t| *t > 0)
                .or_else(|| body.get_u64("timestamp").filter(|t| *t > 0))
        })
        .unwrap_or(now);
    let expires_at = source.min(now).saturating_add(config.max_delay_ms.max(1));
    let sig = event
        .get_array("message")
        .and_then(|segments| signature(segments, &config));
    if now >= expires_at
        || (config.ignore_commands && is_command(&get_prefixes(ctx), msg.text()))
        || sig.is_none()
    {
        break_chain(key, now / 1000);
        return None;
    }
    let self_id = ctx.bot.self_id();
    let sender = if !user_id.is_empty() && user_id == self_id {
        Sender::Bot
    } else {
        Sender::User(user_id.to_string())
    };
    let content = event.get("message")?.clone();
    let mut map = states();
    maybe_evict(&mut map, now / 1000);
    let state = map.entry(key.clone()).or_default();
    let action = feed(
        state,
        sig.unwrap(),
        content.clone(),
        sender,
        &config,
        now / 1000,
    );
    let (sig, generation) = (state.sig.clone(), state.generation);
    persist(&map);
    drop(map);
    let content = match action {
        Action::Silent => return None,
        Action::Repeat => content,
        Action::Interrupt(text) => {
            simd_json::serde::to_owned_value(Message::new().text(text)).ok()?
        }
    };
    Some(PreparedRepeat {
        guard: RepeatGuard {
            key,
            sig,
            generation,
            expires_at,
            message_id: msg.message_id().to_string(),
        },
        group_id: group_id.map(str::to_owned),
        user_id: user_id.to_string(),
        content,
    })
}

/// 收到事件时的钩子：在事件进入流水线之前、按收到的顺序同步更新接力状态；
/// 该跟读（或打断）时，返回流水线之前要先发出去的那一条。
pub fn on_receive(
    ctx: &mut Context,
    writer: &LockedWriter,
) -> Option<BoxFuture<'static, Result<(), PluginError>>> {
    let pending = prepare(ctx, writer)?;
    let (ctx, writer) = (ctx.clone(), writer.clone());
    Some(Box::pin(async move {
        send_prepared(&ctx, writer, pending).await
    }))
}

async fn send_prepared(
    ctx: &Context,
    writer: LockedWriter,
    pending: PreparedRepeat,
) -> Result<(), PluginError> {
    if pending.guard.is_current() {
        send_guarded_msg(
            ctx,
            writer,
            pending.group_id.as_deref(),
            Some(&pending.user_id),
            pending.content,
            Arc::new(pending.guard),
        )
        .await?;
    } else {
        debug!(target: LOG_TARGET, "丢弃过时复读");
    }
    Ok(())
}

pub fn handle(
    mut ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        if ctx.as_message().is_some() {
            if let Some(pending) = prepare(&mut ctx, &writer) {
                send_prepared(&ctx, writer, pending).await?;
            }
        } else if let EventType::BeforeSend(packet) = &ctx.event {
            // An interrupt phrase must not invalidate its own pending send.
            if packet.guard.is_some() {
                return Ok(Some(ctx));
            }
            let config: RepeaterConfig = get_config_or_default(&ctx);
            let now = now_secs();
            let group_id = packet.group_id();
            let user_id = packet.user_id().unwrap_or("");
            let Some(key) = scoped_key(&ctx, &writer, group_id, user_id) else {
                return Ok(Some(ctx));
            };
            let content = packet.message().cloned().unwrap_or_default();
            match content.as_array().and_then(|arr| signature(arr, &config)) {
                Some(sig) => {
                    observe(key, sig, content, Sender::Bot, &config, now);
                }
                None => break_chain(key, now),
            }
        }
        Ok(Some(ctx))
    })
}

