//! 近期原话：号主最近亲手打的话，自动从聊天记录里捞，不靠人工挑。
//!
//! `res/ambient/voice.md` 是人工挑过、编译进二进制的样本库，稳，但会旧：人的口头禅在
//! 变。2026-10-01 量出来的一例——号主句尾挂「（」「）」的习惯，8 月底只占他消息的 0.3%，
//! 9 月下半月 3.6%，10 月头一天 8.3%，而 402 条样本里只有 1 条带它；机器人自然学不到
//! 他现在是怎么说话的。样本库补一轮要人来挑、再编译一遍，赶不上他口吻变化的速度。
//!
//! 这里补一层会自己更新的：机器人自己的聊天记录库（`message_records`）里，同一个号里
//! 不是机器人发的（`member_role != 'self'`）就是号主亲手打的。每隔几个钟头捞最近三周的，
//! 按 `scripts/mine-voice.py` 同一套规矩筛一遍（去指令、去词意猜词、去链接与疑似凭据、
//! 去重），留最新的八十条。发言时从里面抽两条，与样本库的原话混着摆进提示词（见
//! [`super::voice::brief`]）——样本库管「他一贯怎么说」，这一层管「他最近怎么说」。
//!
//! 数据只在本机内存里，由库派生，丢了下次重捞；不落盘、不进仓库。

use crate::event::Context;
use sea_orm::{ConnectionTrait, Statement};
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 捞多久以内的。口头禅的半衰期以周计，三周够看出趋势又不至于拖着旧习惯。
const DAYS: i64 = 21;
/// 最多留多少条（最新的）。
const KEEP: usize = 80;
/// 隔多久重捞一次。
const REFRESH: Duration = Duration::from_secs(6 * 3_600);
/// 失败后隔多久再试。
const RETRY: Duration = Duration::from_secs(10 * 60);
/// 一条最多这么多字：再长的多半是转述或贴的文字，不是说话。
const MAX_CHARS: usize = 36;

/// 一条原始记录：群号、时刻、正文。
pub(super) type Row = (String, i64, String);

#[derive(Default)]
struct State {
    lines: Vec<String>,
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
pub(super) fn lines() -> Vec<String> {
    lock().lines.clone()
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
            Ok(lines) => {
                info!(target: super::LOG_TARGET, "近期原话：捞到 {} 条号主最近亲手打的话", lines.len());
                guard.lines = lines;
                guard.loaded = Some(Instant::now());
            }
            Err(error) => {
                debug!(target: super::LOG_TARGET, "近期原话没捞成：{error}");
                guard.loaded = Instant::now().checked_sub(REFRESH - RETRY);
            }
        }
    });
}

pub(super) async fn load(ctx: &Context, me: &str) -> Result<Vec<String>, sea_orm::DbErr> {
    let since = chrono::Local::now().timestamp() - DAYS * 86_400;
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
    Ok(pick(&parsed, &known_bank()))
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

/// 记录（按时间正序）→ 近期原话，最新的在前。
pub(super) fn pick(rows: &[Row], known: &HashSet<String>) -> Vec<String> {
    let skip = guesses(rows);
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for (index, (_, _, raw)) in rows.iter().enumerate().rev() {
        if skip.contains(&index) || is_command(raw) {
            continue;
        }
        let text = strip_placeholders(raw);
        if text.is_empty()
            || is_command(&text)
            || text.chars().count() > MAX_CHARS
            || width(&text) > MAX_CHARS * 2
            || !speech_like(&text)
            || sensitive(&text)
            || known.contains(&text)
            || !seen.insert(text.clone())
        {
            continue;
        }
        out.push(text);
        if out.len() >= KEEP {
            break;
        }
    }
    out
}
