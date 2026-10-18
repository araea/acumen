//! 提醒与定时任务：管家替号主「到点办事」。
//!
//! 管家最典型的缺口曾经是这个：模型只在有人说话的那一轮活着，说一句「明早八点叫我」
//! 它只能口头答应。现在 `schedule` 工具把约定记进磁盘，这里的后台循环到点执行：
//!
//! - **提醒**（`text`）：到点原样发一句话，不经过模型——准时、便宜、不会被改写；群里会 @ 提出的人；
//! - **任务**（`task`）：到点让这间房的 agent 把任务办一遍，结果按房间回复的样子发回来
//!   （长了排成卡片）。「每天早上查一下天气和我的日程」这类事就靠它。任务只替号主安排——
//!   客人不能借它让管家定期去做事。
//!
//! 重复的有每 N 分钟、每天、每周、每月。重启不丢：任务记在 `data/oai/reminders.json`，
//! 启动后连上机器人再接着排；停机期间错过的，一次性的补发（带「迟到」说明），重复的只当没这回事、
//! 直接排下一次，免得重启后一口气补发一串。
//!
//! 发出去需要一个活着的连接：循环里按「平台 + 账号」找到创建时的那个机器人，找不到（还没连上）
//! 就留着，下一拍再看。

use super::LOG_TARGET;
use crate::adapters::satori::{LockedWriter, send_msg, send_text_chunks};
use crate::event::Context;
use crate::message::Message;
use crate::storage::JsonState;
use chrono::{DateTime, Datelike, Months, TimeZone};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// 循环多久看一次到点的。提醒的精度就是它——十几秒的误差对「几点叫我」无所谓。
const TICK: Duration = Duration::from_secs(15);
/// 同时在办的任务数。任务是一整轮 agent，别让几个同时到点的把手机拖住。
const MAX_RUNNING: usize = 2;
/// 一次性的提醒错过多久还补发；再久就没意义了，丢弃并记日志。
const MAX_LATE: i64 = 48 * 3600;
/// 重复的错过多久还照常发；超过就当这一次没发生，排下一次。
const RECURRING_GRACE: i64 = 30 * 60;
/// 到点晚于这个才在消息里说「迟到」。
const LATE_NOTICE: i64 = 5 * 60;

/// 全部待办上限；每人（含客人）名下提醒上限。
pub(crate) const MAX_JOBS: usize = 60;
pub(crate) const MAX_PER_GUEST: usize = 5;
/// 重复间隔的下限（分钟）：任务更严——每次都是一整轮模型调用。
pub(crate) const MIN_EVERY: u32 = 10;
pub(crate) const MIN_EVERY_TASK: u32 = 60;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    /// 到点原样发一句话。
    Text,
    /// 到点让 agent 办一遍。
    Task,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Repeat {
    Once,
    /// 每 N 分钟。
    Every(u32),
    Daily,
    Weekly,
    /// 每月几号。记着号数是因为 31 号的任务落到 2 月会退到 28 号，到 3 月要回到 31。
    Monthly(u32),
}

impl Repeat {
    pub(crate) fn describe(self) -> String {
        match self {
            Repeat::Once => "一次".to_string(),
            Repeat::Every(minutes) if minutes % 60 == 0 => format!("每 {} 小时", minutes / 60),
            Repeat::Every(minutes) => format!("每 {minutes} 分钟"),
            Repeat::Daily => "每天".to_string(),
            Repeat::Weekly => "每周".to_string(),
            Repeat::Monthly(day) => format!("每月 {day} 号"),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub(crate) struct Job {
    pub id: u32,
    pub kind: Kind,
    pub body: String,
    /// 下一次到点的时刻（Unix 秒）。
    pub next: i64,
    pub repeat: Repeat,
    /// 创建它的房间；任务到点用这间房的模型与人设办事。
    pub room: String,
    /// 房间历史走私有那一份。
    pub private: bool,
    /// 发到哪个机器人：平台 + 账号。
    pub platform: String,
    pub self_id: String,
    /// 发到哪里：群号；私聊为 `None`。
    pub group: Option<String>,
    /// 创建的人（群里提醒会 @ 他；私聊就发给他）。
    pub user: String,
    pub name: String,
    /// 创建时他是不是号主；任务到点时以这个身份办事。
    pub owner: bool,
    pub created: i64,
    pub fired: u32,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Book {
    next: u32,
    jobs: Vec<Job>,
}

static STORE: JsonState<Book> = JsonState::new(LOG_TARGET, "oai", "reminders.json", "提醒与定时任务");

// ================= 对工具的接口 =================

/// 新建一条的参数（id 与创建时间由这里补）。
pub(crate) struct NewJob {
    pub kind: Kind,
    pub body: String,
    pub next: i64,
    pub repeat: Repeat,
    pub room: String,
    pub private: bool,
    pub platform: String,
    pub self_id: String,
    pub group: Option<String>,
    pub user: String,
    pub name: String,
    pub owner: bool,
}

pub(crate) async fn add(new: NewJob) -> Result<Job, String> {
    STORE
        .with(|book| {
            if book.jobs.len() >= MAX_JOBS {
                return Err(format!("待办已经有 {MAX_JOBS} 条，先取消几条再排"));
            }
            if !new.owner && book.jobs.iter().filter(|job| job.user == new.user).count() >= MAX_PER_GUEST {
                return Err(format!("每人最多同时排 {MAX_PER_GUEST} 条提醒"));
            }
            book.next = book.next.max(book.jobs.iter().map(|job| job.id).max().unwrap_or(0)) + 1;
            let job = Job {
                id: book.next,
                kind: new.kind,
                body: new.body,
                next: new.next,
                repeat: new.repeat,
                room: new.room,
                private: new.private,
                platform: new.platform,
                self_id: new.self_id,
                group: new.group,
                user: new.user,
                name: new.name,
                owner: new.owner,
                created: chrono::Local::now().timestamp(),
                fired: 0,
            };
            book.jobs.push(job.clone());
            Ok(job)
        })
        .await
}

/// 某个聊天里的待办（号主在任何聊天里都看全部）。
pub(crate) async fn list(group: Option<&str>, user: &str, owner: bool) -> Vec<Job> {
    STORE
        .with(|book| {
            let mut jobs: Vec<Job> = book
                .jobs
                .iter()
                .filter(|job| owner || (job.user == user && job.group.as_deref() == group))
                .cloned()
                .collect();
            jobs.sort_by_key(|job| job.next);
            jobs
        })
        .await
}

/// 取消一条：号主可以取消任何一条，其他人只能取消自己的。
pub(crate) async fn cancel(id: u32, user: &str, owner: bool) -> Result<Job, String> {
    STORE
        .with(|book| {
            let Some(position) = book.jobs.iter().position(|job| job.id == id) else {
                return Err(format!("没有编号 {id} 的待办"));
            };
            if !owner && book.jobs[position].user != user {
                return Err("这一条不是你排的，只有排它的人或号主能取消".to_string());
            }
            Ok(book.jobs.remove(position))
        })
        .await
}

/// 北京时间的钟点描述，写进回执与列表里。
pub(crate) fn clock_text(unix: i64) -> String {
    let at = crate::clock::beijing().timestamp_opt(unix, 0).single();
    at.map_or_else(|| "?".to_string(), |at| at.format("%Y-%m-%d %H:%M").to_string())
}

/// 这次之后的下一次到点；一次性的没有下一次。`now` 之前错过的几次直接跳过。
pub(crate) fn following(job_next: i64, repeat: Repeat, now: i64) -> Option<i64> {
    let beijing = crate::clock::beijing();
    let mut at: DateTime<chrono::FixedOffset> = beijing.timestamp_opt(job_next, 0).single()?;
    let step = |at: DateTime<chrono::FixedOffset>| -> Option<DateTime<chrono::FixedOffset>> {
        match repeat {
            Repeat::Once => None,
            Repeat::Every(minutes) => Some(at + chrono::Duration::minutes(i64::from(minutes.max(1)))),
            Repeat::Daily => Some(at + chrono::Duration::days(1)),
            Repeat::Weekly => Some(at + chrono::Duration::days(7)),
            Repeat::Monthly(_) => at.checked_add_months(Months::new(1)),
        }
    };
    loop {
        at = step(at)?;
        if let Repeat::Monthly(day) = repeat {
            at = at.with_day(day.min(days_in_month(at.year(), at.month()))).unwrap_or(at);
        }
        if at.timestamp() > now {
            return Some(at.timestamp());
        }
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    chrono::NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .and_then(|first| first.pred_opt())
        .map_or(28, |last| last.day())
}

/// 解析「几点」：`2026-10-19 08:30`、`10-19 08:30`、`08:30`（今天，已过则明天），北京时间。
pub(crate) fn parse_when(raw: &str, now: DateTime<chrono::FixedOffset>) -> Result<i64, String> {
    let raw = raw.trim().replace('T', " ");
    let raw = raw.trim();
    let beijing = crate::clock::beijing();
    let localize = |date: chrono::NaiveDate, time: chrono::NaiveTime| {
        beijing
            .from_local_datetime(&date.and_time(time))
            .single()
            .map(|at| at.timestamp())
            .ok_or_else(|| format!("时间「{raw}」不存在"))
    };
    let bad = || format!("看不懂时间「{raw}」，写成 2026-10-19 08:30 或 08:30（北京时间）");
    let (date_part, time_part) = match raw.rsplit_once(' ') {
        Some((date, time)) => (Some(date.trim()), time.trim()),
        None => (None, raw),
    };
    let time = crate::clock::parse_clock(time_part).ok_or_else(bad)?;
    if let Some(date) = date_part {
        let parts: Vec<u32> = date
            .split(['-', '/', '.'])
            .map(|part| part.trim().parse::<u32>().map_err(|_| bad()))
            .collect::<Result<_, _>>()?;
        let (year, month, day) = match parts[..] {
            [year, month, day] => (year as i32, month, day),
            [month, day] => (now.year(), month, day),
            _ => return Err(bad()),
        };
        let date = chrono::NaiveDate::from_ymd_opt(year, month, day).ok_or_else(bad)?;
        return localize(date, time);
    }
    let today = localize(now.date_naive(), time)?;
    Ok(if today > now.timestamp() { today } else { today + 86_400 })
}

// ================= 后台循环 =================

/// 一个连着的机器人：到点发消息要用它的上下文与写端。
#[derive(Clone)]
pub(crate) struct Bot {
    pub platform: String,
    pub self_id: String,
    pub ctx: Context,
    pub writer: LockedWriter,
}

static BOTS: Mutex<Vec<Bot>> = Mutex::new(Vec::new());
static STARTED: AtomicBool = AtomicBool::new(false);

pub(crate) fn bot_for(platform: &str, self_id: &str) -> Option<Bot> {
    let bots = BOTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bots.iter()
        .find(|bot| bot.platform == platform && bot.self_id == self_id)
        .cloned()
}

/// 机器人连上时登记（同一个机器人重连会换掉旧的），并确保循环只起一个。
pub(crate) fn connected(ctx: &Context, writer: &LockedWriter) {
    let bot = Bot {
        platform: ctx.bot.platform.clone(),
        self_id: ctx.bot.self_id(),
        ctx: ctx.clone(),
        writer: writer.clone(),
    };
    {
        let mut bots = BOTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        bots.retain(|old| !(old.platform == bot.platform && old.self_id == bot.self_id));
        bots.push(bot);
    }
    if !STARTED.swap(true, Ordering::SeqCst) {
        tokio::spawn(async {
            let running = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_RUNNING));
            loop {
                tokio::time::sleep(TICK).await;
                tick(&running).await;
            }
        });
        info!(target: LOG_TARGET, "提醒与定时任务的循环已启动");
    }
}

async fn tick(running: &std::sync::Arc<tokio::sync::Semaphore>) {
    let now = chrono::Local::now().timestamp();
    let due: Vec<(Job, i64, bool)> = STORE
        .with(|book| {
            let mut due = Vec::new();
            book.jobs.retain_mut(|job| {
                if job.next > now {
                    return true;
                }
                // 机器人还没连上：留着，下一拍再看。
                if bot_for(&job.platform, &job.self_id).is_none() {
                    return true;
                }
                let late = now - job.next;
                let snapshot = job.clone();
                job.fired += 1;
                let skip = match job.repeat {
                    Repeat::Once => late > MAX_LATE,
                    _ => late > RECURRING_GRACE,
                };
                due.push((snapshot, late, skip));
                match following(job.next, job.repeat, now) {
                    Some(next) => {
                        job.next = next;
                        true
                    }
                    None => false,
                }
            });
            due
        })
        .await;
    for (job, late, skip) in due {
        if skip {
            info!(target: LOG_TARGET, "待办 {} 错过了 {} 分钟，这一次不补发", job.id, late / 60);
            continue;
        }
        let running = running.clone();
        tokio::spawn(async move {
            let Ok(_permit) = running.acquire_owned().await else { return };
            if let Err(error) = fire(&job, late).await {
                warn!(target: LOG_TARGET, "待办 {}（{}）没能送达：{error:#}", job.id, job.room);
            }
        });
    }
}

async fn fire(job: &Job, late: i64) -> anyhow::Result<()> {
    let bot = bot_for(&job.platform, &job.self_id)
        .ok_or_else(|| anyhow::anyhow!("机器人 {}/{} 不在线", job.platform, job.self_id))?;
    let tardy = if late > LATE_NOTICE {
        format!("（迟到了 {} 分钟，机器人刚恢复）", late / 60)
    } else {
        String::new()
    };
    match job.kind {
        Kind::Text => {
            let text = format!("⏰ {}{tardy}", job.body);
            let message = match &job.group {
                Some(_) => Message::new().at(&job.user).text(format!(" {text}")),
                None => Message::new().text(text),
            };
            send_msg(&bot.ctx, bot.writer.clone(), job.group.as_deref(), Some(&job.user), message)
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            info!(target: LOG_TARGET, "提醒已发出（{}，{}）：{}", job.id, job.name, job.body);
            Ok(())
        }
        Kind::Task => run_task(job, &bot, &tardy).await,
    }
}

/// 到点办一次任务：让房间的 agent 办完，把结果发回创建它的那个聊天。
async fn run_task(job: &Job, bot: &Bot, tardy: &str) -> anyhow::Result<()> {
    let mgr = super::ensure_manager().await.map_err(|error| anyhow::anyhow!("{error}"))?;
    let (agent, api) = {
        let config = mgr.config.read().await;
        (
            config.agents.iter().find(|agent| agent.name == job.room).cloned(),
            (config.api_base.clone(), config.api_key.clone()),
        )
    };
    let agent = agent
        .filter(super::types::Agent::uses_agent)
        .ok_or_else(|| anyhow::anyhow!("房间「{}」已经不在或不是智能体房间", job.room))?;
    let oai = crate::plugins::get_config_or_default::<super::OaiConfig>(&bot.ctx);
    let (api_base, api_key, model) = super::logic::room_endpoint(&agent, &oai, &mgr, &api)
        .await
        .map_err(anyhow::Error::msg)?;
    let base = super::utils::openai_api_base(&api_base);

    let prompt = format!(
        "〔定时任务〕{}\n（这是你之前替{}安排的任务，现在到点了{tardy}。直接办，办完把结果当作给他的回复来写；\
         不要反问，也不要提「定时」这个词。）",
        job.body,
        if job.owner { "号主" } else { &job.name }
    );
    let hist = vec![super::types::ChatMessage::new("user", &prompt, vec![])];
    let seat = super::agent::Seat {
        room: job.room.clone(),
        asker: super::agent::Asker {
            id: job.user.clone(),
            name: job.name.clone(),
            owner: job.owner,
        },
        ctx: bot.ctx.clone(),
        writer: bot.writer.clone(),
        group: job.group.clone(),
        private: job.private,
        origin: super::agent::Origin::Scheduled,
        public: false,
    };
    let chat = job.group.as_ref().map(|group| super::agent::ChatContext {
        ctx: bot.ctx.clone(),
        writer: bot.writer.clone(),
        group: group.clone(),
        config: oai.chat.clone(),
        tools: Some(super::logic::room_tools(&oai.chat)),
        trigger: None,
    });
    let started = std::time::Instant::now();
    let reply = tokio::time::timeout(
        oai.request_timeout(),
        super::logic::respond(
            &base,
            &api_key,
            &agent,
            &model,
            &hist,
            &oai,
            mgr.path.parent().unwrap_or(&mgr.path),
            Some(seat),
            None,
            chat,
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("任务超过 {} 秒还没办完", oai.request_timeout().as_secs()))??;
    info!(target: LOG_TARGET, "定时任务办完（{}，{} 秒）：{}", job.id, started.elapsed().as_secs(), job.body);

    deliver(job, bot, &oai, &reply, tardy).await;

    // 记进房间历史：之后接着聊「刚才那份报告里……」时，管家知道说的是什么。
    let mut config = mgr.config.write().await;
    if let Some(room) = config.agents.iter_mut().find(|agent| agent.name == job.room) {
        let history = room.history_mut(job.private, &job.user);
        let from = if job.private { String::new() } else { seat_tag(job) };
        history.push(super::types::ChatMessage::new("user", &format!("〔定时任务〕{}", job.body), vec![]).with_from(&from));
        history.push(super::types::ChatMessage::new("assistant", &reply.text, vec![]));
        if let Err(error) = mgr.save(&mut config) {
            warn!(target: LOG_TARGET, "定时任务的结果没能写进房间历史：{error}");
        }
    }
    Ok(())
}

/// 创建这条待办的人在公共历史里的说话人标签。
fn seat_tag(job: &Job) -> String {
    super::agent::Asker { id: job.user.clone(), name: job.name.clone(), owner: job.owner }.tag()
}

/// 把任务的结果发出去：短的发文本，长的排成卡片，卡片发不出就退回文本。
async fn deliver(job: &Job, bot: &Bot, oai: &super::OaiConfig, reply: &super::logic::Reply, tardy: &str) {
    let header = format!("{} · 定时任务{tardy}", job.room);
    let group = job.group.as_deref();
    let text = reply.text.trim();
    let plain = reply.sources.is_empty()
        && super::agent::figures::sources(text).is_empty()
        && super::logic::is_plain_enough(text, oai.plain_text_max_chars());
    if !plain && oai.image_enabled() {
        let card = super::render::Card {
            title: &header,
            markdown: text,
            sources: &reply.sources,
            figures: &reply.figures,
            footer: None,
        };
        match super::render::render_card(card, oai.image_scale()).await {
            Ok(b64) => {
                let message = Message::new().image_described(format!("base64://{b64}"), &header);
                match send_msg(&bot.ctx, bot.writer.clone(), group, Some(&job.user), message).await {
                    Ok(()) => return,
                    Err(error) => warn!(target: LOG_TARGET, "定时任务的卡片没发出去，改发文本：{error}"),
                }
            }
            Err(error) => warn!(target: LOG_TARGET, "定时任务的卡片渲染失败，改发文本：{error:#}"),
        }
    }
    let spoken = super::agent::figures::without_images(text);
    let body = format!("{header}\n{spoken}");
    if let Err(error) = send_text_chunks(&bot.ctx, bot.writer.clone(), group, Some(&job.user), &body).await {
        warn!(target: LOG_TARGET, "定时任务的结果没发出去：{error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(date: &str) -> i64 {
        parse_when(date, crate::clock::beijing().with_ymd_and_hms(2026, 10, 18, 12, 0, 0).unwrap()).unwrap()
    }

    #[test]
    fn when_accepts_date_time_or_bare_clock() {
        assert_eq!(clock_text(at("2026-10-19 08:30")), "2026-10-19 08:30");
        assert_eq!(clock_text(at("10-19 08:30")), "2026-10-19 08:30");
        assert_eq!(clock_text(at("2026/10/19T08:30")), "2026-10-19 08:30");
        // 只写钟点：没过就是今天，过了就是明天。
        assert_eq!(clock_text(at("18:00")), "2026-10-18 18:00");
        assert_eq!(clock_text(at("08:00")), "2026-10-19 08:00");
        assert!(parse_when("明天早上", crate::clock::beijing_now()).is_err());
        assert!(parse_when("2026-02-30 08:00", crate::clock::beijing_now()).is_err());
    }

    #[test]
    fn repeats_advance_and_skip_what_was_missed() {
        let start = at("2026-10-18 20:00");
        let now = start + 10;
        assert_eq!(following(start, Repeat::Once, now), None);
        assert_eq!(following(start, Repeat::Daily, now), Some(start + 86_400));
        assert_eq!(following(start, Repeat::Weekly, now), Some(start + 7 * 86_400));
        assert_eq!(following(start, Repeat::Every(30), now), Some(start + 1_800));
        // 停机三天：不补发，直接排到「现在之后」最近的一次，并且保持原来的钟点。
        let later = start + 3 * 86_400 + 600;
        assert_eq!(following(start, Repeat::Daily, later), Some(start + 4 * 86_400));
        assert_eq!(following(start, Repeat::Every(60), later), Some(start + 3 * 86_400 + 3_600));
    }

    #[test]
    fn monthly_keeps_its_day_across_short_months() {
        let start = at("2026-01-31 09:00");
        let feb = following(start, Repeat::Monthly(31), start).unwrap();
        assert_eq!(clock_text(feb), "2026-02-28 09:00");
        let mar = following(feb, Repeat::Monthly(31), feb).unwrap();
        assert_eq!(clock_text(mar), "2026-03-31 09:00");
    }

    #[test]
    fn repeat_reads_naturally() {
        assert_eq!(Repeat::Every(90).describe(), "每 90 分钟");
        assert_eq!(Repeat::Every(120).describe(), "每 2 小时");
        assert_eq!(Repeat::Once.describe(), "一次");
    }
}
