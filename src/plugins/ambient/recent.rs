//! 号主的语料：他最近亲手打的话，自动从聊天记录里捞，不靠人工挑。
//!
//! `res/ambient/voice.md` 是人工挑过、编译进二进制的样本库，稳，但会旧：人的口头禅在
//! 变。2026-10-01 量出来的一例——号主句尾挂「（」「）」的习惯，8 月底只占他消息的 0.3%，
//! 9 月下半月 3.6%，10 月头一天 8.3%，而 402 条样本里只有 1 条带它；机器人自然学不到
//! 他现在是怎么说话的。样本库补一轮要人来挑、再编译一遍，赶不上他口吻变化的速度。
//!
//! 这里补一层会自己更新的：机器人自己的聊天记录库（`message_records`）里，同一个号里
//! 不是机器人发的（`member_role != 'self'`）就是号主亲手打的。每隔二十分钟捞一遍最近
//! 两个月的（迁移前那份旧库还在就一并读，见 [`legacy_rows`]），按 `scripts/mine-voice.py`
//! 同一套规矩筛一遍（去指令、去词意猜词、去链接与疑似凭据——连着六位以上数字、`sk-`、
//! 密码、验证码字样——与重复），派三样用处，都只存在本机内存里：
//!
//! - **近期原话**：最新的八十条。发言时抽 2 条与样本库的原话混着摆，同样避开眼前的话题
//!   （见 [`super::voice::brief`]）——样本库管「他一贯怎么说」，这一层管「他最近怎么说」。
//! - **话题回忆**：整个两个月的语料按实词建索引，眼前聊到什么就翻他以前就这件事说过的
//!   话（见 [`super::recall`]）——这一层管「他说过什么」。
//! - **近期在线节奏**：他最近两周每个钟点有没有亲手打过字，给精神头的作息曲线校准
//!   （见 [`super::mood`]）——他放假、熬夜、换了作息，曲线跟着变，不必再人工重量。
//!
//! 数据由库派生，丢了下次重捞；不落盘、不进仓库。

use super::habit::Habit;
use super::recall;
use crate::event::Context;
use sea_orm::{ConnectionTrait, Statement};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 近期原话捞多久以内的。口头禅的半衰期以周计，三周够看出趋势又不至于拖着旧习惯。
const DAYS: i64 = 21;
/// 话题回忆的语料捞多久以内的：他对一件事的看法能管上一两个月。
const CORPUS_DAYS: i64 = 60;
/// 近期原话最多留多少条（最新的）。
const KEEP: usize = 80;
/// 另外每个群各留他在那个群里说的最新几条：放假一天他在二十几个群手打一千多句，
/// 全局最新八十条只够盖住最近一两个钟头，轮不到安静的群。
const GROUP_KEEP: usize = 40;
/// 打字习惯看最近多少天：句尾挂「（」这种习惯几周就能变（8 月底 0.3%，10 月初 8%）。
const HABIT_DAYS: i64 = 30;
/// 隔多久重捞一次。他刚说过的话二十分钟内就进得了样本，口头禅换得再快也跟得上。
const REFRESH: Duration = Duration::from_secs(20 * 60);
/// 失败后隔多久再试。
const RETRY: Duration = Duration::from_secs(10 * 60);
/// 一条最多这么多字：再长的多半是转述或贴的文字，不是说话。
const MAX_CHARS: usize = 36;
/// 话题回忆里一句话至少这么多字：「？」「草」这种反应没有话题可言。
const MIN_RECALL_CHARS: usize = 4;
/// 在线节奏看最近多少天，以及每过多少天权重减半：放假头两三天就该看出来，
/// 又不至于被某一天的熬夜带偏（2026-10-02 一天里他在 26 个群手打了 1412 句，
/// 平时一天二三百句；那一天的在线率几乎顶满，与内置的表对半混之后才不至于失控）。
const PRESENCE_DAYS: i64 = 14;
const PRESENCE_HALF_LIFE_DAYS: f32 = 5.0;
/// 至少有几个完整的日子才信这张表。
const PRESENCE_MIN_DAYS: usize = 4;
/// 把「这个钟点里有几个十分钟格子他在线」换算成 0.30–0.92 的精神头基线用的两端：
/// 与 `scripts/owner-rhythm.py` 量出内置那两张表时用的是同一把尺（2026-10-01，
/// 5699 条手打消息平滑后的最低与最高在线率），两边才对得上。
const PRESENCE_LOW: f32 = 0.087;
const PRESENCE_HIGH: f32 = 0.272;

/// 一条原始记录：群号、时刻、正文。
pub(super) type Row = (String, i64, String);

/// 近期原话里的一句：说在哪个群，以及原话。
#[derive(Debug, Clone)]
pub(super) struct Line {
    pub group: String,
    pub text: String,
}

/// 一次捞回来的全部。
#[derive(Default)]
pub(super) struct Loaded {
    lines: Vec<Line>,
    /// 每个群里他最近说的话（最新的在前）。
    by_group: HashMap<String, Vec<Line>>,
    /// 每个群里的打字习惯（量得出的群才有）。
    habits: HashMap<String, Habit>,
    corpus: recall::Corpus,
    /// 每个钟点的精神头基线（0.30–0.92）；日子不够时没有。
    energy: Option<[f32; 24]>,
}

#[derive(Default)]
struct State {
    loaded_data: Loaded,
    loaded: Option<Instant>,
    loading: bool,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default)
}

fn lock() -> std::sync::MutexGuard<'static, State> {
    state().lock().unwrap_or_else(|error| error.into_inner())
}

/// 当前这批近期原话（最新的在前）。
pub(super) fn lines() -> Vec<Line> {
    lock().loaded_data.lines.clone()
}

/// 他在这个群里最近说的话（最新的在前）。
pub(super) fn lines_in(group: &str) -> Vec<Line> {
    lock()
        .loaded_data
        .by_group
        .get(group)
        .cloned()
        .unwrap_or_default()
}

/// 他在这个群里的打字习惯，已经排好版的一句；这个群里他说得太少、量不出来就是空串。
pub(super) fn habit_brief(group: &str) -> String {
    lock()
        .loaded_data
        .habits
        .get(group)
        .map(Habit::brief)
        .unwrap_or_default()
}

/// 眼前这段聊天沾边的、号主以前说过的话，已经排好版摆进提示词的一段；没有就是空串。
pub(super) fn recalled(group: &str, turns: &[super::window::Turn]) -> String {
    let guard = lock();
    let found = guard
        .loaded_data
        .corpus
        .related(group, chrono::Local::now().timestamp(), turns);
    if !found.is_empty() {
        debug!(target: super::LOG_TARGET, "群 {group} 翻出他以前说过的 {} 句相关的话", found.len());
    }
    recall::brief(&found)
}

/// 这个钟点他最近的在线情况换算成的精神头基线；日子不够、没捞到时是 `None`。
pub(super) fn energy_at(hour: u32) -> Option<f32> {
    lock().loaded_data.energy.map(|table| table[(hour % 24) as usize])
}

/// 该重捞就在后台捞一次，不阻塞调用方。
pub(super) fn ensure_fresh(ctx: &Context) {
    let me = ctx.bot.self_id();
    if me.is_empty() {
        return;
    }
    {
        let mut guard = lock();
        if guard.loading || guard.loaded.is_some_and(|at| at.elapsed() < REFRESH) {
            return;
        }
        guard.loading = true;
    }
    let ctx = ctx.clone();
    tokio::spawn(async move {
        let result = load(&ctx, &me).await;
        let mut guard = lock();
        guard.loading = false;
        match result {
            Ok(loaded) => {
                info!(
                    target: super::LOG_TARGET,
                    "号主语料：近期原话 {} 条，{} 个群有打字习惯，话题回忆 {} 句，在线节奏{}",
                    loaded.lines.len(),
                    loaded.habits.len(),
                    loaded.corpus.len(),
                    if loaded.energy.is_some() { "已校准" } else { "日子不够" }
                );
                guard.loaded_data = loaded;
                guard.loaded = Some(Instant::now());
            }
            Err(error) => {
                debug!(target: super::LOG_TARGET, "号主语料没捞成：{error}");
                guard.loaded = Instant::now().checked_sub(REFRESH - RETRY);
            }
        }
    });
}

pub(super) async fn load(ctx: &Context, me: &str) -> Result<Loaded, sea_orm::DbErr> {
    let now = chrono::Local::now().timestamp();
    let since = now - CORPUS_DAYS * 86_400;
    let rows = ctx
        .db
        .query_all_raw(Statement::from_sql_and_values(
            ctx.db.get_database_backend(),
            "select guild_id, time, content_rich from message_records \
             where user_id = ? and member_role != 'self' and guild_id != '' and time >= ? \
             order by time",
            [me.into(), since.into()],
        ))
        .await?;
    let mut parsed: Vec<Row> = Vec::with_capacity(rows.len());
    for row in rows {
        parsed.push((
            row.try_get::<String>("", "guild_id")?,
            row.try_get::<i64>("", "time")?,
            row.try_get::<String>("", "content_rich")?,
        ));
    }
    // 迁移前的旧库只在这台机器上有，且只在迁移后两个月内还有用；读不到就算了。
    let mut older = legacy_rows(me, since).await;
    if !older.is_empty() {
        older.extend(parsed);
        parsed = older;
        parsed.sort_by_key(|row| row.1);
    }
    Ok(assemble(&parsed, now, &known_bank()))
}

/// 2026-09-27 记录库迁成字符串 ID 的新结构时，旧库留作备份（`group_id` / `role` 两列、
/// 整数的 `user_id`）。号主在那里面手打了一个多月的话，不读等于白白丢掉。
///
/// **旧库把私聊也记在里面（`group_id = 0`）**，新库不记（`guild_id` 为空）。私聊是说给一个人
/// 听的，这里的语料会摆进群里的提示词，所以一条都不能读：必须滤掉 `group_id = 0`。
async fn legacy_rows(me: &str, since: i64) -> Vec<Row> {
    const PATH: &str = "data/bot.db.pre-string-ids";
    let Ok(me) = me.parse::<i64>() else {
        return Vec::new();
    };
    if !std::path::Path::new(PATH).exists() {
        return Vec::new();
    }
    let mut options = sea_orm::ConnectOptions::new(format!("sqlite:{PATH}?mode=ro"));
    options
        .max_connections(1)
        .connect_timeout(Duration::from_secs(5))
        .sqlx_logging(false);
    let db = match sea_orm::Database::connect(options).await {
        Ok(db) => db,
        Err(error) => {
            debug!(target: super::LOG_TARGET, "迁移前的旧记录库打不开，跳过：{error}");
            return Vec::new();
        }
    };
    let result = db
        .query_all_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "select cast(group_id as text) as guild_id, time, content_rich from message_records \
             where user_id = ? and role != 'self' and group_id != 0 and time >= ? order by time",
            [me.into(), since.into()],
        ))
        .await;
    let rows = match result {
        Ok(rows) => rows
            .iter()
            .filter_map(|row| {
                Some((
                    row.try_get::<String>("", "guild_id").ok()?,
                    row.try_get::<i64>("", "time").ok()?,
                    row.try_get::<String>("", "content_rich").ok()?,
                ))
            })
            .collect(),
        Err(error) => {
            debug!(target: super::LOG_TARGET, "迁移前的旧记录库没读成，跳过：{error}");
            Vec::new()
        }
    };
    let _ = db.close().await;
    rows
}

/// 记录（按时间正序）→ 三样用处。单拎出来，不碰数据库也能核对。
pub(super) fn assemble(rows: &[Row], now: i64, known: &HashSet<String>) -> Loaded {
    let recent_from = now - DAYS * 86_400;
    let start = rows.partition_point(|row| row.1 < recent_from);
    let skip = guesses(rows);
    let corpus = recall::Corpus::build(rows.iter().enumerate().filter_map(
        |(index, (group, at, raw))| {
            if skip.contains(&index) {
                return None;
            }
            let text = usable(raw)?;
            (text.chars().count() >= MIN_RECALL_CHARS).then(|| (group.clone(), *at, text))
        },
    ));
    let (lines, by_group) = pick(&rows[start..], known);
    Loaded {
        lines,
        by_group,
        habits: habits(rows, &skip, now),
        corpus,
        energy: energy_table(rows.iter().map(|row| row.1), now),
    }
}

/// 每个群里他最近亲手打字的习惯。
///
/// 词意猜词（九十秒内连着三个以上光秃秃的两三字词）不算：玩那个游戏的群里，它能把
/// 中位字数从 7 拉到 2。带 @ 的消息只算引用与回合、不算字数——`@昵称` 那几个字不是他打的。
fn habits(rows: &[Row], skip: &HashSet<usize>, now: i64) -> HashMap<String, Habit> {
    let from = now - HABIT_DAYS * 86_400;
    let mut per_group: HashMap<&str, Vec<(i64, String, bool)>> = HashMap::new();
    for (index, (group, at, raw)) in rows.iter().enumerate() {
        if *at < from || skip.contains(&index) || is_command(raw) {
            continue;
        }
        let text = if raw.contains("[@") {
            String::new()
        } else {
            strip_placeholders(raw)
        };
        if is_command(&text) {
            continue;
        }
        per_group
            .entry(group)
            .or_default()
            .push((*at, text, raw.contains("[回复]")));
    }
    per_group
        .into_iter()
        .filter_map(|(group, rows)| Habit::measure(&rows).map(|habit| (group.to_string(), habit)))
        .collect()
}

/// 样本库里已有的原话：近期这一层不再重复它们。
fn known_bank() -> HashSet<String> {
    super::voice::bank_lines().into_iter().map(str::to_string).collect()
}

/// 这些开头的是发给机器人的指令或贴进来的长提示词，不是闲聊。与 mine-voice.py 同一份。
const COMMAND_HEADS: [&str; 12] = [
    "/", "#", "-", ".", "～", "~", "mj", "ciyi", "dsr", "dsapp", "ds", "oai",
];

/// 占位符：留下来会把样本污染成「[图片]」。
fn strip_placeholders(raw: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"\[(图片|视频|语音|表情|动画表情|转发|文件|音乐|合并转发|回复|@\d+)\]").unwrap()
    })
    .replace_all(raw, "")
    .trim()
    .to_string()
}

fn is_command(text: &str) -> bool {
    let head = text.trim_start();
    COMMAND_HEADS.iter().any(|prefix| head.starts_with(prefix))
        || head
            .strip_prefix('c')
            .is_some_and(|rest| rest.starts_with(char::is_whitespace))
}

fn is_han(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

/// 光秃秃的两三个汉字：词意猜词的形状。
fn is_bare_word(text: &str) -> bool {
    let count = text.chars().count();
    (2..=3).contains(&count) && text.chars().all(is_han)
}

/// 显示宽度：全角字符算两格，中英混排时不把长句误判成短句。
fn width(text: &str) -> usize {
    text.chars()
        .map(|c| {
            let wide = matches!(c as u32,
                0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF
                | 0xFE30..=0xFE6F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6);
            if wide { 2 } else { 1 }
        })
        .sum()
}

/// 像不像一句人在群里说的话：有汉字，或者至少两个英文词；不含 @、链接。
fn speech_like(text: &str) -> bool {
    if text.contains(['@', '\n']) || text.contains("http") || text.contains(".com") {
        return false;
    }
    if text.chars().all(|c| !c.is_alphanumeric()) {
        return false;
    }
    if text.chars().all(|c| c.is_ascii_digit() || c.is_whitespace() || c == '.') {
        return false;
    }
    if text.chars().any(is_han) {
        return true;
    }
    text.split(|c: char| !c.is_ascii_alphabetic())
        .filter(|word| word.len() >= 2)
        .count()
        >= 2
}

/// 看着像凭据或私人信息的：连着的长数字、密钥前缀、验证码与密码字样。
/// 这些话会原样进模型提示词，宁可错杀。
fn sensitive(text: &str) -> bool {
    let lowered = text.to_lowercase();
    if ["sk-", "token", "密码", "验证码", "cookie", "bearer"].iter().any(|needle| lowered.contains(needle)) {
        return true;
    }
    let mut run = 0;
    for c in text.chars() {
        run = if c.is_ascii_digit() { run + 1 } else { 0 };
        if run >= 6 {
            return true;
        }
    }
    false
}

/// 记录里属于词意猜词的那些下标：同一个群 90 秒内有三条以上光秃秃的两三字词。
fn guesses(rows: &[(String, i64, String)]) -> HashSet<usize> {
    let bare: Vec<(usize, &str, i64)> = rows
        .iter()
        .enumerate()
        .filter(|(_, (_, _, raw))| is_bare_word(&strip_placeholders(raw)))
        .map(|(index, (group, at, _))| (index, group.as_str(), *at))
        .collect();
    let mut out = HashSet::new();
    for (spot, (_, group, at)) in bare.iter().enumerate() {
        let run: Vec<usize> = bare[spot.saturating_sub(3)..(spot + 4).min(bare.len())]
            .iter()
            .filter(|(_, other_group, other_at)| other_group == group && (other_at - at).abs() <= 90)
            .map(|(index, _, _)| *index)
            .collect();
        if run.len() >= 3 {
            out.extend(run);
        }
    }
    out
}

/// 一条原始记录能不能当他的原话：去指令、占位符，太长、不像人话、疑似凭据的都不要。
fn usable(raw: &str) -> Option<String> {
    if is_command(raw) {
        return None;
    }
    let text = strip_placeholders(raw);
    (!text.is_empty()
        && !is_command(&text)
        && text.chars().count() <= MAX_CHARS
        && width(&text) <= MAX_CHARS * 2
        && speech_like(&text)
        && !sensitive(&text))
    .then_some(text)
}

/// 记录（按时间正序）→ 近期原话（全局最新的八十条）与每个群各自最新的几条，都是最新的在前。
pub(super) fn pick(
    rows: &[Row],
    known: &HashSet<String>,
) -> (Vec<Line>, HashMap<String, Vec<Line>>) {
    let skip = guesses(rows);
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    let mut by_group: HashMap<String, Vec<Line>> = HashMap::new();
    for (index, (group, _, raw)) in rows.iter().enumerate().rev() {
        if skip.contains(&index) {
            continue;
        }
        let Some(text) = usable(raw) else {
            continue;
        };
        if known.contains(&text) || !seen.insert(text.clone()) {
            continue;
        }
        let line = Line {
            group: group.clone(),
            text,
        };
        let mine = by_group.entry(group.clone()).or_default();
        if mine.len() < GROUP_KEEP {
            mine.push(line.clone());
        }
        if out.len() < KEEP {
            out.push(line);
        }
    }
    (out, by_group)
}

/// 他最近每个钟点有没有在线 → 精神头基线。
///
/// 量法与 `scripts/owner-rhythm.py` 一致：数他在每个钟点的十分钟格子里有没有亲手发过
/// 消息（量在线，不量话量），三点环形平滑，再按同一把尺拉到 0.30–0.92。不同的是只看
/// 最近两周、越近权重越大（半衰期五天），并且**不分工作日周末**——放假、请假、赶工熬夜
/// 这种整段的变化，内置那两张按星期分的表是看不见的。今天还没过完，不算进去，否则
/// 傍晚一看全天后半截都是空的。完整的日子不到四天就不给表。
fn energy_table(times: impl Iterator<Item = i64>, now: i64) -> Option<[f32; 24]> {
    use chrono::{Datelike as _, TimeZone as _, Timelike as _};
    let local = |at: i64| chrono::Local.timestamp_opt(at, 0).single();
    let today = local(now)?.date_naive();
    let first = today - chrono::Duration::days(PRESENCE_DAYS);
    let mut cells: HashSet<(chrono::NaiveDate, u32, u32)> = HashSet::new();
    let mut seen_days: HashSet<chrono::NaiveDate> = HashSet::new();
    for at in times {
        let Some(time) = local(at) else { continue };
        let day = time.date_naive();
        if day < first || day >= today {
            continue;
        }
        seen_days.insert(day);
        cells.insert((day, time.hour(), time.minute() / 10));
    }
    let start = seen_days.iter().min().copied()?;
    let days: Vec<chrono::NaiveDate> = start
        .iter_days()
        .take_while(|day| *day < today)
        .collect();
    if days.len() < PRESENCE_MIN_DAYS {
        return None;
    }
    let mut table = [0.0_f32; 24];
    let mut total = 0.0_f32;
    for day in &days {
        let age = (today.num_days_from_ce() - day.num_days_from_ce()) as f32;
        let weight = 0.5_f32.powf(age / PRESENCE_HALF_LIFE_DAYS);
        total += weight;
        for (hour, slot) in table.iter_mut().enumerate() {
            let online = (0..6)
                .filter(|cell| cells.contains(&(*day, hour as u32, *cell)))
                .count();
            *slot += weight * online as f32 / 6.0;
        }
    }
    let raw: Vec<f32> = table.iter().map(|value| value / total).collect();
    let mut out = [0.0_f32; 24];
    for hour in 0..24 {
        let smooth = (raw[(hour + 23) % 24] + raw[hour] + raw[(hour + 1) % 24]) / 3.0;
        let scaled = ((smooth - PRESENCE_LOW) / (PRESENCE_HIGH - PRESENCE_LOW)).clamp(0.0, 1.0);
        out[hour] = 0.30 + 0.62 * scaled;
    }
    Some(out)
}
