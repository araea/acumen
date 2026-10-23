//! 自动清理（默认关，只管 `auto.groups` 里点了名的群）。
//!
//! 每天到 `auto.time`（之后两小时内，错过就等明天）看一遍：
//!
//! 1. 名单上提醒满宽限期（`warn.grace_days`）还没冒泡的，移出——一次最多 `auto.max_kicks` 人；
//! 2. 到了 `auto.weekdays` 里的日子，给还没提醒过的人发提醒（`auto.warn = false` 时不提醒，直接移出）。
//!
//! 几道闸：只认确定潜水的人（拿不准的从不进名单）；一个群查出来的人超过 `auto.max_ratio`%
//! 就不动手，私聊告诉号主去看一眼——多半是哪里出了岔子，而不是真有那么多人潜水；
//! 机器人不是管理员的群跳过并说一声。

use super::purge::{self, Job, State, Target};
use super::roster::{self, Scan};
use super::{Config, LOG_TARGET};
use crate::adapters::satori::LockedWriter;
use crate::event::Context;
use crate::plugins::get_config_or_default;
use crate::storage::JsonState;
use chrono::{Datelike, Timelike};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Default, Clone)]
struct Log {
    /// 最近一次跑完的日子（北京时间，`YYYY-MM-DD`）。
    last_day: String,
    /// 各群最近一次的结果（控制台「现在的情况」用）。
    #[serde(default)]
    results: HashMap<String, String>,
}

static LOG: JsonState<Log> = JsonState::new(LOG_TARGET, "lurker", "auto.json", "自动清理记录");

/// 每分钟看一眼：到点了、今天还没跑过，就跑一遍。
pub async fn tick(ctx: &Context, writer: &LockedWriter) {
    let config = get_config_or_default::<Config>(ctx);
    if !config.enabled || !config.auto.enabled || config.auto.groups.is_empty() {
        return;
    }
    let now = crate::clock::beijing_now();
    let Some(at) = crate::clock::parse_clock(&config.auto.time) else {
        return;
    };
    // 只在约定时刻起的两小时内动手：错过了（机器人那会儿不在线）就等明天，
    // 免得半夜重启、或刚打开开关的那一刻突然清起人来。
    let minutes = (now.hour() * 60 + now.minute()) as i64 - (at.hour() * 60 + at.minute()) as i64;
    if !(0..120).contains(&minutes) {
        return;
    }
    let today = now.format("%Y-%m-%d").to_string();
    let fresh = LOG
        .with(|log| {
            if log.last_day == today {
                return false;
            }
            log.last_day = today.clone();
            true
        })
        .await;
    if !fresh {
        return;
    }
    let weekday = now.weekday().number_from_monday();
    let warn_day = config.auto.weekdays.contains(&weekday);
    for group in &config.auto.groups {
        if !config.channel.allows_group(group) || !ctx.group_allowed(group) {
            continue;
        }
        let result = run(ctx, writer, &config, group, warn_day).await;
        info!(target: LOG_TARGET, "自动清理 群 {group}：{result}");
        LOG.with(|log| {
            log.results.insert(group.clone(), format!("{today} {result}"));
        })
        .await;
    }
}

/// 各群最近一次自动清理的结果（不等锁）。
pub fn peek_results() -> HashMap<String, String> {
    LOG.peek().map(|log| log.results).unwrap_or_default()
}

async fn run(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str, warn_day: bool) -> String {
    if purge::active(group).await.is_some() {
        return "上一批还没做完，这次跳过".into();
    }
    let scan = match roster::scan(ctx, writer, config, group, None).await {
        Ok(scan) => scan,
        Err(error) => return format!("没查成：{error}"),
    };
    if !scan.can_kick {
        super::tell_keepers(ctx, writer, &format!("自动清理潜水：我在「{}」（{group}）不是管理员，这次跳过了。", scan.group_name)).await;
        return "机器人不是管理员，跳过".into();
    }
    if scan.entries.is_empty() {
        return "没有潜水的人".into();
    }
    let ratio = scan.entries.len() * 100 / scan.total.max(1);
    if ratio > config.auto.max_ratio as usize {
        super::tell_keepers(
            ctx,
            writer,
            &format!(
                "自动清理潜水：「{}」（{group}）查出 {} 人潜水，占全群 {ratio}%，超过自动清理的上限 {}%，这次没动手。\n请在群里发「潜水」看一眼名单，确认无误再手动移出。",
                scan.group_name,
                scan.entries.len(),
                config.auto.max_ratio
            ),
        )
        .await;
        return format!("{} 人（{ratio}%）超过上限，没动手", scan.entries.len());
    }
    let now = scan.at;
    let grace = i64::from(config.warn.grace_days) * 86_400;
    let cap = config.auto.max_kicks as usize;
    let (due, fresh): (Vec<_>, Vec<_>) = if config.auto.warn {
        let due = scan.entries.iter().filter(|e| e.warned > 0 && now - e.warned >= grace).take(cap).cloned().collect();
        let fresh = if warn_day { scan.entries.iter().filter(|e| e.warned == 0).take(cap).cloned().collect() } else { Vec::new() };
        (due, fresh)
    } else if warn_day {
        (scan.entries.iter().take(cap).cloned().collect(), Vec::new())
    } else {
        (Vec::new(), Vec::new())
    };
    let mut said = Vec::new();
    if !fresh.is_empty() {
        let people: Vec<(String, String)> = fresh.iter().map(|e| (e.id.clone(), e.name.clone())).collect();
        match super::warn::send(ctx, writer, config, group, &people).await {
            Ok(report) => said.push(report.brief()),
            Err(error) => said.push(format!("提醒没发出去：{error}")),
        }
    }
    if !due.is_empty() {
        let job = job_of(&scan, config, due.iter().map(target).collect());
        let n = job.queue.len();
        match purge::submit(ctx, writer, job).await {
            Ok(()) => said.push(format!("开始移出 {n} 人")),
            Err(error) => said.push(error),
        }
    }
    if said.is_empty() {
        format!("{} 人潜水，等宽限期满", scan.entries.len())
    } else {
        said.join("，")
    }
}

fn target(entry: &roster::Entry) -> Target {
    Target { id: entry.id.clone(), name: entry.name.clone(), no: entry.no, since: entry.since, kind: entry.kind }
}

fn job_of(scan: &Scan, config: &Config, queue: Vec<Target>) -> Job {
    let now = chrono::Utc::now().timestamp();
    Job {
        id: chrono::Utc::now().timestamp_millis(),
        group: scan.group.clone(),
        group_name: scan.group_name.clone(),
        operator: String::new(),
        operator_name: "自动清理".into(),
        source: "auto".into(),
        reply_to: String::new(),
        permanent: config.permanent,
        rule: scan.rule.text.clone(),
        created: now,
        queue,
        done: Vec::new(),
        state: State::Running,
        finished: 0,
        note: String::new(),
    }
}

/// 自动清理做完在群里说的那句。
pub fn announcement(config: &Config, job: &Job) -> Option<String> {
    let removed: Vec<&str> = job.done.iter().filter(|o| o.result == purge::Fate::Removed).map(|o| o.name.as_str()).collect();
    if removed.is_empty() {
        return None;
    }
    let text = super::warn::pick(&config.auto.announce_text)?;
    let mut names = removed.iter().take(10).copied().collect::<Vec<_>>().join("、");
    if removed.len() > 10 {
        names.push_str(&format!(" 等 {} 人", removed.len()));
    }
    let values = HashMap::from([
        ("count", removed.len().to_string()),
        ("days", roster::rule_for(config, &job.group).days.to_string()),
        ("names", names),
    ]);
    let pieces = crate::template::pieces(&text, &values);
    Some(
        pieces
            .into_iter()
            .filter_map(|piece| match piece {
                crate::template::Piece::Text(text) => Some(text),
                _ => None,
            })
            .collect(),
    )
}

