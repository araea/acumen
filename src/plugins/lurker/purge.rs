//! 移出：确认过的一批人，低速分批、一个个请出去。
//!
//! - **慢**：每两次之间隔 [`super::Pace::interval_seconds`]（上下浮动四分之一，不像机器在点），
//!   每移出 `burst` 个歇 `rest_seconds`；所有群共用一条队列，两个群同时清也不会翻倍提速。
//! - **动手前再看一眼**：排队期间说过话的人留下（[`spoke`] 由事件流水线报来）；
//!   一连三次失败、或失败的样子像是没了权限，就停下来，不硬试。
//! - **看结果不看回执**：全部过完再拉一遍名单，以「还在不在群里」为准——回执丢了、
//!   人自己先退了，都以名单为准说清楚。
//! - **能停、能续**：「潜水 停」在下一个人之前停下；进程中途重启，剩下的人留在
//!   `data/lurker/jobs.json` 里，连上后在群里说一声，「潜水 继续」接着来。
//!
//! 进行中的这批会拦着例行重启（`restart::busy`），最多拦到重启那边的等待上限。

use super::roster::Category;
use super::{Config, LOG_TARGET};
use crate::adapters::satori::{LockedWriter, SatoriApiError, send_msg};
use crate::event::Context;
use crate::message::Message;
use crate::plugins::get_config_or_default;
use crate::storage::JsonState;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// 要移出的一个人。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Target {
    pub id: String,
    pub name: String,
    pub no: u32,
    pub since: i64,
    pub kind: Category,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Fate {
    /// 移出了。
    Removed,
    /// 排队期间说了话，留下。
    Spoke,
    /// 动手前就已经不在群里了。
    Left,
    /// 没移出去。
    Failed,
    /// 停下了，没轮到。
    Skipped,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Outcome {
    pub id: String,
    pub name: String,
    pub result: Fate,
    #[serde(default)]
    pub note: String,
    pub at: i64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Running,
    /// 进程在中途退出了，剩下的还没处理。
    Interrupted,
    Done,
    Stopped,
    /// 停在半路：没有权限、连续失败。
    Halted,
}

/// 一批移出。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Job {
    pub id: i64,
    pub group: String,
    pub group_name: String,
    /// 谁确认的：QQ 号，控制台是空串。
    pub operator: String,
    pub operator_name: String,
    /// 从哪儿发起的：group（群里）、console（控制台）、auto（自动清理）。
    pub source: String,
    /// 回报时引用的那条消息。
    #[serde(default)]
    pub reply_to: String,
    pub permanent: bool,
    /// 口径，回报里说「按 30 天没说话」。
    pub rule: String,
    pub created: i64,
    pub queue: Vec<Target>,
    pub done: Vec<Outcome>,
    pub state: State,
    #[serde(default)]
    pub finished: i64,
    #[serde(default)]
    pub note: String,
}

impl Job {
    pub fn total(&self) -> usize {
        self.queue.len() + self.done.len()
    }

    pub fn count(&self, result: Fate) -> usize {
        self.done.iter().filter(|o| o.result == result).count()
    }
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct Book {
    /// 还没做完的，按提交先后。
    jobs: Vec<Job>,
    /// 做完的，最新在前，留 [`HISTORY`] 条。
    history: Vec<Job>,
}

const HISTORY: usize = 40;

static BOOK: JsonState<Book> = JsonState::new(LOG_TARGET, "lurker", "jobs.json", "移出记录");
static RUNNING: AtomicBool = AtomicBool::new(false);
static STOP: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// 排着队的人（群, 号）；排队期间说了话、自己退了群的记进 [`MARKS`]。
static QUEUED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
static MARKS: Mutex<Vec<(String, String, Fate)>> = Mutex::new(Vec::new());

fn lock<T>(m: &'static Mutex<T>) -> std::sync::MutexGuard<'static, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 事件流水线报来：这个人在这个群说话了（`Fate::Spoke`）、或自己退了群（`Fate::Left`）。
/// 排着队的话，轮到他时就不再动手。
pub fn mark(group: &str, user: &str, fate: Fate) {
    let queued = lock(&QUEUED).iter().any(|(g, u)| g == group && u == user);
    if queued {
        lock(&MARKS).push((group.to_string(), user.to_string(), fate));
    }
}

/// 某个群现在有没有在移出、或排着队。
pub async fn active(group: &str) -> Option<Job> {
    BOOK.read(|book| book.jobs.iter().find(|job| job.group == group).cloned()).await
}

/// 全部还没做完的（控制台用）。
pub async fn pending() -> Vec<Job> {
    BOOK.read(|book| book.jobs.clone()).await
}

/// 同上，不等锁（控制台「现在的情况」用）。
pub fn peek_pending() -> Vec<Job> {
    BOOK.peek().map(|book| book.jobs).unwrap_or_default()
}

/// 做完的，最新在前。
pub async fn history(group: Option<&str>, limit: usize) -> Vec<Job> {
    BOOK.read(|book| book.history.iter().filter(|job| group.is_none_or(|g| job.group == g)).take(limit).cloned().collect())
        .await
}

/// 交一批进队列。已经有一批在这个群里跑的话拒绝。
pub async fn submit(ctx: &Context, writer: &LockedWriter, job: Job) -> Result<(), String> {
    let group = job.group.clone();
    let ids: Vec<String> = job.queue.iter().map(|t| t.id.clone()).collect();
    let accepted = BOOK
        .with(|book| {
            if book.jobs.iter().any(|j| j.group == job.group) {
                return false;
            }
            book.jobs.push(job);
            true
        })
        .await;
    if !accepted {
        return Err("这个群已经有一批在移出了，发「潜水 进度」看看，或「潜水 停」先停下".into());
    }
    lock(&QUEUED).extend(ids.into_iter().map(|id| (group.clone(), id)));
    start(ctx, writer);
    Ok(())
}

/// 请这个群正在跑的那批停下（下一个人之前生效）。
pub async fn stop(group: &str) -> bool {
    let running = BOOK.read(|book| book.jobs.iter().any(|j| j.group == group && j.state == State::Running)).await;
    if running {
        lock(&STOP).push(group.to_string());
        return true;
    }
    // 中断了、等着「继续」的那批：直接收进历史。
    
    BOOK
        .with(|book| {
            let Some(index) = book.jobs.iter().position(|j| j.group == group) else {
                return false;
            };
            let mut job = book.jobs.remove(index);
            finish(&mut job, State::Stopped);
            book.history.insert(0, job);
            book.history.truncate(HISTORY);
            true
        })
        .await
}

/// 中断了的那批接着做。
pub async fn resume(ctx: &Context, writer: &LockedWriter, group: &str) -> Option<usize> {
    let left = BOOK
        .with(|book| {
            let job = book.jobs.iter_mut().find(|j| j.group == group && j.state == State::Interrupted)?;
            job.state = State::Running;
            Some((job.queue.len(), job.queue.iter().map(|t| t.id.clone()).collect::<Vec<_>>()))
        })
        .await?;
    lock(&QUEUED).extend(left.1.into_iter().map(|id| (group.to_string(), id)));
    start(ctx, writer);
    Some(left.0)
}

/// 进程刚起来：上次没做完的标成「中断」，等人说「继续」。返回这些群与剩下的人数。
pub async fn recover() -> Vec<(String, usize)> {
    BOOK.with(|book| {
        book.jobs
            .iter_mut()
            .filter(|job| job.state == State::Running)
            .map(|job| {
                job.state = State::Interrupted;
                (job.group.clone(), job.queue.len())
            })
            .collect()
    })
    .await
}

fn start(ctx: &Context, writer: &LockedWriter) {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let (ctx, writer) = (ctx.clone(), writer.clone());
    tokio::spawn(async move {
        let _busy = crate::plugins::restart::busy();
        loop {
            let next = BOOK.read(|book| book.jobs.iter().find(|j| j.state == State::Running).map(|j| j.id)).await;
            let Some(id) = next else {
                break;
            };
            run(&ctx, &writer, id).await;
        }
        RUNNING.store(false, Ordering::SeqCst);
        // 刚好在收尾时又交进来一批：再起一次。
        if BOOK.read(|book| book.jobs.iter().any(|j| j.state == State::Running)).await {
            start(&ctx, &writer);
        }
    });
}

fn stopping(group: &str) -> bool {
    lock(&STOP).iter().any(|g| g == group)
}

/// 睡 `seconds` 秒，每秒看一眼要不要停。
async fn pause(group: &str, seconds: f64) {
    let mut left = seconds;
    while left > 0.0 && !stopping(group) {
        let step = left.min(1.0);
        tokio::time::sleep(Duration::from_secs_f64(step)).await;
        left -= step;
    }
}

/// 像是没了权限（不再是管理员、对方升了管理员）的失败：再试也一样。
fn denied(error: &(dyn std::error::Error + 'static)) -> bool {
    let text = error.to_string();
    error.downcast_ref::<SatoriApiError>().is_some_and(|e| e.status == 403)
        || ["权限", "permission", "not admin", "no privilege"].iter().any(|word| text.to_lowercase().contains(word))
}

async fn run(ctx: &Context, writer: &LockedWriter, id: i64) {
    let mut kicked = 0usize;
    let mut failures = 0usize;
    loop {
        let config = get_config_or_default::<Config>(ctx);
        let Some((group, target, permanent)) = BOOK
            .read(|book| {
                let job = book.jobs.iter().find(|j| j.id == id)?;
                Some((job.group.clone(), job.queue.first().cloned(), job.permanent))
            })
            .await
        else {
            return;
        };
        if stopping(&group) {
            close(ctx, writer, id, State::Stopped, "按要求停下了").await;
            return;
        }
        let Some(target) = target else {
            verify(ctx, writer, id).await;
            close(ctx, writer, id, State::Done, "").await;
            return;
        };
        let now = chrono::Utc::now().timestamp();
        let marked = {
            let mut marks = lock(&MARKS);
            let hit = marks.iter().find(|(g, u, _)| *g == group && *u == target.id).map(|m| m.2);
            marks.retain(|(g, u, _)| !(*g == group && *u == target.id));
            hit
        };
        let outcome = if let Some(fate) = marked {
            let note = if fate == Fate::Spoke { "排队时说了话" } else { "自己退了群" };
            Outcome { id: target.id.clone(), name: target.name.clone(), result: fate, note: note.into(), at: now }
        } else {
            crate::departures::expect(&group, &target.id);
            let reply: Result<Value, _> = writer
                .call(ctx, "guild.member.kick", json!({"guild_id": group, "user_id": target.id, "permanent": permanent}))
                .await;
            match reply {
                Ok(_) => {
                    kicked += 1;
                    failures = 0;
                    Outcome { id: target.id.clone(), name: target.name.clone(), result: Fate::Removed, note: String::new(), at: now }
                }
                Err(error) => {
                    failures += 1;
                    warn!(target: LOG_TARGET, "群 {group} 移出 {}（{}）没成：{error}", target.name, target.id);
                    let halt = denied(error.as_ref()).then_some("机器人没有移出他的权限（可能不再是管理员）")
                        .or((failures >= 3).then_some("一连三个都没移出去"));
                    let outcome = Outcome {
                        id: target.id.clone(),
                        name: target.name.clone(),
                        result: Fate::Failed,
                        note: short(&error.to_string()),
                        at: now,
                    };
                    if let Some(reason) = halt {
                        record(id, outcome).await;
                        verify(ctx, writer, id).await;
                        close(ctx, writer, id, State::Halted, reason).await;
                        return;
                    }
                    outcome
                }
            }
        };
        let acted = outcome.result == Fate::Removed || outcome.result == Fate::Failed;
        record(id, outcome).await;
        if acted {
            let pace = &config.pace;
            let jitter = 0.75 + rand::random::<f64>() * 0.5;
            let mut wait = f64::from(pace.interval_seconds) * jitter;
            if pace.burst > 0 && kicked > 0 && kicked.is_multiple_of(pace.burst as usize) {
                wait += f64::from(pace.rest_seconds);
            }
            pause(&group, wait).await;
        }
    }
}

fn short(text: &str) -> String {
    crate::storage::truncate_chars(text, 80)
}

async fn record(id: i64, outcome: Outcome) {
    let group = BOOK
        .with(|book| {
            let job = book.jobs.iter_mut().find(|j| j.id == id)?;
            job.queue.retain(|t| t.id != outcome.id);
            let group = job.group.clone();
            job.done.push(outcome.clone());
            Some(group)
        })
        .await;
    if let Some(group) = group {
        lock(&QUEUED).retain(|(g, u)| !(*g == group && *u == outcome.id));
    }
}

/// 做完（或停下）后拉一遍名单，以「还在不在群里」为准改正结果。
async fn verify(ctx: &Context, writer: &LockedWriter, id: i64) {
    let Some(group) = BOOK.read(|book| book.jobs.iter().find(|j| j.id == id).map(|j| j.group.clone())).await else {
        return;
    };
    // 等实现端那边的名单跟上。
    tokio::time::sleep(Duration::from_secs(3)).await;
    let members = match super::roster::members(ctx, writer, &group, true).await {
        Ok(members) => members,
        Err(error) => {
            warn!(target: LOG_TARGET, "群 {group} 移出后核对名单失败：{error}");
            return;
        }
    };
    let present: HashSet<&str> = members.iter().map(|m| m.id.as_str()).collect();
    BOOK.with(|book| {
        let Some(job) = book.jobs.iter_mut().find(|j| j.id == id) else {
            return;
        };
        for outcome in &mut job.done {
            let here = present.contains(outcome.id.as_str());
            match outcome.result {
                Fate::Failed if !here => {
                    // 回执说失败，人却不在了：要么其实移出了，要么先走了一步。
                    outcome.result = Fate::Left;
                    outcome.note = "回执报错，但人已不在群里".into();
                }
                Fate::Removed if here => {
                    outcome.result = Fate::Failed;
                    outcome.note = "回执说成功，人却还在群里".into();
                }
                _ => {}
            }
        }
    })
    .await;
}

fn finish(job: &mut Job, state: State) {
    let now = chrono::Utc::now().timestamp();
    for target in job.queue.drain(..) {
        job.done.push(Outcome { id: target.id, name: target.name, result: Fate::Skipped, note: String::new(), at: now });
    }
    job.state = state;
    job.finished = now;
}

/// 收尾：挪进历史、在群里报一声。
async fn close(ctx: &Context, writer: &LockedWriter, id: i64, state: State, reason: &str) {
    let job = BOOK
        .with(|book| {
            let index = book.jobs.iter().position(|j| j.id == id)?;
            let mut job = book.jobs.remove(index);
            finish(&mut job, state);
            job.note = reason.to_string();
            book.history.insert(0, job.clone());
            book.history.truncate(HISTORY);
            Some(job)
        })
        .await;
    let Some(job) = job else {
        return;
    };
    lock(&STOP).retain(|g| *g != job.group);
    lock(&QUEUED).retain(|(g, _)| *g != job.group);
    lock(&MARKS).retain(|(g, _, _)| *g != job.group);
    let removed: Vec<String> = job.done.iter().filter(|o| o.result == Fate::Removed).map(|o| o.id.clone()).collect();
    super::warn::forget(&job.group, &removed).await;
    info!(target: LOG_TARGET, "群 {}：{}", job.group, summary(&job));
    let config = get_config_or_default::<Config>(ctx);
    let report = match job.source.as_str() {
        "auto" => config.auto.announce.then(|| super::auto::announcement(&config, &job)).flatten(),
        _ => Some(summary(&job)),
    };
    if let Some(text) = report {
        let mut message = Message::new();
        if !job.reply_to.is_empty() {
            message = message.reply(&job.reply_to);
        }
        if let Err(error) = send_msg(ctx, writer.clone(), Some(&job.group), None, message.text(text)).await {
            warn!(target: LOG_TARGET, "群 {} 的移出结果没发出去：{error}", job.group);
        }
    }
}

/// 一句话的结果。
pub fn summary(job: &Job) -> String {
    let removed = job.count(Fate::Removed);
    let mut text = match job.state {
        State::Done => format!("清理完了：移出 {removed} 人"),
        State::Stopped => format!("已停下：移出了 {removed} 人"),
        State::Halted => format!("停在半路（{}）：移出了 {removed} 人", job.note),
        State::Running | State::Interrupted => format!("进行中：已移出 {removed} / {} 人", job.total()),
    };
    let elapsed = job.finished.max(job.created) - job.created;
    if job.finished > 0 && elapsed >= 60 {
        text.push_str(&format!("，用时 {}", crate::clock::duration_text((elapsed as u64 / 60) * 60)));
    }
    let mut bits = Vec::new();
    for (result, label) in [(Fate::Spoke, "刚说过话、留下"), (Fate::Left, "已不在群里"), (Fate::Failed, "没移出去"), (Fate::Skipped, "没轮到")] {
        let n = job.count(result);
        if n > 0 {
            bits.push(format!("{n} 人{label}"));
        }
    }
    if !bits.is_empty() {
        text.push('；');
        text.push_str(&bits.join("，"));
    }
    text.push('。');
    let failed: Vec<&str> = job.done.iter().filter(|o| o.result == Fate::Failed).map(|o| o.name.as_str()).take(5).collect();
    if !failed.is_empty() {
        text.push_str(&format!("\n没移出去的：{}", failed.join("、")));
    }
    if job.state == State::Stopped && job.count(Fate::Skipped) > 0 {
        text.push_str("\n剩下的人还在名单上，重新「潜水」看一眼再决定。");
    }
    text
}

/// 估计要多久：每人一个间隔，每 burst 人一次休息。
pub fn estimate(config: &Config, count: usize) -> u64 {
    let pace = &config.pace;
    let rests = if pace.burst == 0 || count == 0 { 0 } else { (count - 1) / pace.burst as usize };
    count as u64 * u64::from(pace.interval_seconds) + rests as u64 * u64::from(pace.rest_seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_counts_rests_between_bursts() {
        let mut config = Config::default();
        config.pace.interval_seconds = 4;
        config.pace.burst = 15;
        config.pace.rest_seconds = 40;
        assert_eq!(estimate(&config, 15), 60);
        assert_eq!(estimate(&config, 16), 64 + 40);
        assert_eq!(estimate(&config, 100), 400 + 6 * 40);
    }

    #[test]
    fn summary_reads_like_a_sentence() {
        let outcome = |id: &str, result| Outcome { id: id.into(), name: format!("成员{id}"), result, note: String::new(), at: 0 };
        let job = Job {
            id: 1,
            group: "1".into(),
            group_name: "群".into(),
            operator: String::new(),
            operator_name: String::new(),
            source: "group".into(),
            reply_to: String::new(),
            permanent: false,
            rule: "30 天没说话".into(),
            created: 0,
            queue: Vec::new(),
            done: vec![outcome("1", Fate::Removed), outcome("2", Fate::Removed), outcome("3", Fate::Spoke), outcome("4", Fate::Failed)],
            state: State::Done,
            finished: 200,
            note: String::new(),
        };
        assert_eq!(summary(&job), "清理完了：移出 2 人，用时 3 分钟；1 人刚说过话、留下，1 人没移出去。\n没移出去的：成员4");
    }
}
