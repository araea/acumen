//! `schedule`：提醒与定时任务的工具面。存储与到点执行在 [`crate::plugins::oai::remind`]。
//!
//! 工具这一层只管把模型说的人话收成一条待办：时间怎么写（`at` 或 `in_minutes`）、
//! 要不要重复、是提醒还是任务。几条边界在这里一并拦下：
//! - **任务只替号主安排**——任务到点是一整轮 agent，客人排一个「每十分钟查一次」就是在花号主的钱；
//!   客人只能排提醒（原样发一句话），而且每人有数量上限；
//! - **下限**：重复间隔至少 10 分钟，任务至少 60 分钟；
//! - **回执里写明确切时刻**：模型常把「明早八点」算错一天，回执里的「2026-10-19 08:00」
//!   让它（和用户）能当场发现。

use super::AgentRun;
use crate::plugins::oai::remind::{self, Kind, NewJob, Repeat};
use rig_core::completion::ToolDefinition;
use serde_json::{Value, json};

pub(crate) const NAME: &str = "schedule";

pub(crate) fn spec() -> ToolDefinition {
    ToolDefinition::new(
        super::tools::tool_name(NAME),
        "替用户安排到点要办的事，重启也不丢。add 新排一条：时间用 at（北京时间，如 2026-10-19 08:30，或只写 08:30 表示今天/明天最近的那个钟点）\
            或 in_minutes（多少分钟后）；repeat 默认 once，也可以是 daily / weekly / monthly（按 at 的钟点与日期重复）或 minutes（再配 every_minutes，至少 10）。\
            内容二选一：text = 到点原样发一句话提醒（群里会 @ 提出的人，不经过模型，准时可靠，「叫我喝水」「十点开会」这类用它）；\
            task = 到点由你把这件事办一遍、结果发回来（「每天早上查天气和新闻简报」这类，要查要算要动手的用它；只替号主排，重复间隔至少 60 分钟）。\
            task 要写成一段自成一体的话（到点的你看不到现在的对话）。list 看这个聊天里排了什么；cancel 按编号取消。\
            用户说「明天」「下周一」「半小时后」时，先对照提示词里的今天日期算准确切时刻再排，回执里会写出实际的触发时刻，核对一遍再告诉用户。"
            .to_string(),
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["add", "list", "cancel"]},
                "text": {"type": "string", "description": "add：到点原样发出的提醒"},
                "task": {"type": "string", "description": "add：到点由你办的任务（与 text 二选一），自成一体"},
                "at": {"type": "string", "description": "add：触发时刻，北京时间，如 2026-10-19 08:30 或 08:30"},
                "in_minutes": {"type": "integer", "minimum": 1, "maximum": 525600, "description": "add：多少分钟后（与 at 二选一）"},
                "repeat": {"type": "string", "enum": ["once", "daily", "weekly", "monthly", "minutes"], "description": "add：重复方式，默认 once"},
                "every_minutes": {"type": "integer", "minimum": 10, "maximum": 43200, "description": "repeat=minutes 时的间隔（分钟）"},
                "id": {"type": "integer", "description": "cancel：待办编号"}
            },
            "required": ["action"]
        }),
    )
}

pub(crate) async fn execute(args: &Value, run: &AgentRun<'_>) -> anyhow::Result<String> {
    let seat = run
        .seat
        .ok_or_else(|| anyhow::anyhow!("这一轮没有接通聊天，{NAME} 用不了"))?;
    let owner = seat.asker.owner;
    match args["action"].as_str().unwrap_or("").trim() {
        "add" => {
            let text = args["text"].as_str().unwrap_or("").trim();
            let task = args["task"].as_str().unwrap_or("").trim();
            let (kind, body) = match (text.is_empty(), task.is_empty()) {
                (false, true) => (Kind::Text, text),
                (true, false) => (Kind::Task, task),
                (true, true) => anyhow::bail!("参数错误：text（提醒）和 task（任务）要写一个"),
                (false, false) => anyhow::bail!("参数错误：text 和 task 只能写一个"),
            };
            if kind == Kind::Task && !owner {
                anyhow::bail!("定时任务只替号主安排；可以帮你排一条提醒（text），到点我把话发给你");
            }
            if body.chars().count() > 1_000 {
                anyhow::bail!("内容太长了（上限 1000 字），精简后再排");
            }
            if seat.ctx.bot.platform.is_empty() {
                anyhow::bail!("找不到当前的机器人账号，排不了");
            }
            let now = crate::clock::beijing_now();
            let first = match (args["at"].as_str().map(str::trim).filter(|at| !at.is_empty()), args["in_minutes"].as_i64()) {
                (Some(at), None) => remind::parse_when(at, now).map_err(anyhow::Error::msg)?,
                (None, Some(minutes)) if minutes >= 1 => now.timestamp() + minutes * 60,
                (None, Some(_)) => anyhow::bail!("参数错误：in_minutes 至少 1"),
                (None, None) => anyhow::bail!("参数错误：at 和 in_minutes 要写一个"),
                (Some(_), Some(_)) => anyhow::bail!("参数错误：at 和 in_minutes 只能写一个"),
            };
            if first <= now.timestamp() + 20 {
                anyhow::bail!("这个时刻已经过去了（现在是 {}），重新算一下", remind::clock_text(now.timestamp()));
            }
            let repeat = repeat_of(args, first, kind)?;
            let job = remind::add(NewJob {
                kind,
                body: body.to_string(),
                next: first,
                repeat,
                room: seat.room.clone(),
                private: seat.private,
                platform: seat.ctx.bot.platform.clone(),
                self_id: seat.ctx.bot.self_id(),
                group: seat.group.clone(),
                user: seat.asker.id.clone(),
                name: seat.asker.name.clone(),
                owner,
            })
            .await
            .map_err(anyhow::Error::msg)?;
            Ok(format!(
                "已排好 [{}]：{}，{}{}，内容：{}",
                job.id,
                match job.kind {
                    Kind::Text => "提醒",
                    Kind::Task => "任务",
                },
                remind::clock_text(job.next),
                if job.repeat == Repeat::Once { String::new() } else { format!("，{}", job.repeat.describe()) },
                body
            ))
        }
        "list" => {
            let jobs = remind::list(seat.group.as_deref(), &seat.asker.id, owner).await;
            if jobs.is_empty() {
                return Ok("现在没有排着的提醒或任务".to_string());
            }
            Ok(jobs
                .iter()
                .map(|job| {
                    format!(
                        "[{}] {} · {}{} · {}{}{}",
                        job.id,
                        match job.kind {
                            Kind::Text => "提醒",
                            Kind::Task => "任务",
                        },
                        remind::clock_text(job.next),
                        if job.repeat == Repeat::Once { String::new() } else { format!(" · {}", job.repeat.describe()) },
                        job.body,
                        // 号主看全部：标出是谁排的、排在哪。
                        if owner { format!(" · {}", job.name) } else { String::new() },
                        match (&job.group, owner) {
                            (Some(group), true) => format!(" @群{group}"),
                            _ => String::new(),
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "cancel" => {
            let id = args["id"].as_u64().unwrap_or(0) as u32;
            let job = remind::cancel(id, &seat.asker.id, owner).await.map_err(anyhow::Error::msg)?;
            Ok(format!("已取消 [{}]：{}", job.id, job.body))
        }
        other => anyhow::bail!("未知操作：{other}"),
    }
}

/// 重复方式：`repeat` + `every_minutes`，并按「任务比提醒更严」检查下限。
fn repeat_of(args: &Value, first: i64, kind: Kind) -> anyhow::Result<Repeat> {
    use chrono::Datelike;
    let repeat = match args["repeat"].as_str().unwrap_or("once").trim() {
        "" | "once" => Repeat::Once,
        "daily" => Repeat::Daily,
        "weekly" => Repeat::Weekly,
        "monthly" => {
            let day = chrono::DateTime::from_timestamp(first, 0)
                .map(|at| at.with_timezone(&crate::clock::beijing()).day())
                .unwrap_or(1);
            Repeat::Monthly(day)
        }
        "minutes" => {
            let every = args["every_minutes"].as_u64().unwrap_or(0) as u32;
            if every < remind::MIN_EVERY {
                anyhow::bail!("每隔多少分钟要用 every_minutes 写明，且至少 {} 分钟", remind::MIN_EVERY);
            }
            Repeat::Every(every)
        }
        other => anyhow::bail!("未知的 repeat：{other}"),
    };
    if kind == Kind::Task
        && let Repeat::Every(every) = repeat
        && every < remind::MIN_EVERY_TASK
    {
        anyhow::bail!("任务每次都是一整轮模型调用，重复间隔至少 {} 分钟", remind::MIN_EVERY_TASK);
    }
    Ok(repeat)
}
