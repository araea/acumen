//! 控制台的「潜水名单」面板：选群、查一遍、勾人、移出、看进度。
//!
//! 写操作都对着「最近一次查的那份名单」：页面提交时带上那份名单的时刻（`at`），对不上就请
//! 重新查——页面上看到的是哪些人，移出的就只能是这些人。移出走与群里同一条队列（同样慢、
//! 同样会在冒泡时留人），只是确认在页面的对话框里。

use super::purge::{self, Fate, Job, State, Target};
use super::roster::{self, Scan};
use super::{Config, LOG_TARGET};
use crate::adapters::satori::LockedWriter;
use crate::event::Context;
use crate::plugins::{get_config_or_default, update_config};
use serde_json::{Value, json};

fn link(ctx: &Context) -> Result<(Context, LockedWriter), String> {
    super::link(ctx).ok_or_else(|| "还没连上账号，等机器人连上再来".to_string())
}

fn job_json(job: &Job, config: &Config) -> Value {
    let count = |fate| job.count(fate);
    json!({
        "id": job.id,
        "group": job.group,
        "group_name": job.group_name,
        "operator": job.operator,
        "operator_name": job.operator_name,
        "source": job.source,
        "state": job.state,
        "rule": job.rule,
        "created": job.created,
        "finished": job.finished,
        "note": job.note,
        "total": job.total(),
        "left": job.queue.len(),
        "removed": count(Fate::Removed),
        "spoke": count(Fate::Spoke),
        "gone": count(Fate::Left),
        "failed": count(Fate::Failed),
        "skipped": count(Fate::Skipped),
        "eta": purge::estimate(config, job.queue.len()),
        "summary": purge::summary(job),
        "done": job.done,
        "queue": job.queue,
    })
}

fn scan_json(scan: &Scan) -> Value {
    let mut value = serde_json::to_value(scan).unwrap_or(Value::Null);
    if let Some(object) = value.as_object_mut() {
        object.insert("note".into(), roster::source_note(scan).map_or(Value::Null, Value::from));
    }
    value
}

/// 面板打开时的一份：各群最近查过的名单（只给个数）、进行中的、最近做完的、自动清理的结果。
pub async fn overview(ctx: &Context) -> Value {
    let config = get_config_or_default::<Config>(ctx);
    let now = chrono::Utc::now().timestamp();
    let lists: Vec<Value> = roster::recent()
        .await
        .iter()
        .map(|scan| json!({ "group": scan.group, "group_name": scan.group_name, "at": scan.at, "count": scan.entries.len(), "fresh": scan.fresh(now) }))
        .collect();
    let pending: Vec<Value> = purge::pending().await.iter().map(|job| job_json(job, &config)).collect();
    let history: Vec<Value> = purge::history(None, 20).await.iter().map(|job| job_json(job, &config)).collect();
    json!({
        "enabled": config.enabled,
        "connected": super::link(ctx).is_some(),
        "rule": { "days": config.days, "min_messages": config.min_messages, "newcomer_days": config.newcomer_days },
        "pace": { "interval_seconds": config.pace.interval_seconds, "burst": config.pace.burst, "rest_seconds": config.pace.rest_seconds, "max_per_run": config.pace.max_per_run },
        "permanent": config.permanent,
        "grace_days": config.warn.grace_days,
        "lists": lists,
        "pending": pending,
        "history": history,
    })
}

/// 查一遍这个群；`days` 只这一次换个天数看。没改天数时先给最近一份（还新鲜就不必重查）。
pub async fn scan(ctx: &Context, group: &str, days: Option<u32>, again: bool) -> Result<Value, String> {
    let now = chrono::Utc::now().timestamp();
    if !again
        && days.is_none()
        && let Some(scan) = roster::latest(group).await
        && now - scan.at < 600
    {
        return Ok(scan_json(&scan));
    }
    let (ctx, writer) = link(ctx)?;
    let config = get_config_or_default::<Config>(&ctx);
    let scan = roster::scan(&ctx, &writer, &config, group, days).await?;
    Ok(scan_json(&scan))
}

/// 页面上勾好的人，对着那一份名单核过再交进队列。
fn chosen(scan: &Scan, ids: &[String]) -> Result<Vec<Target>, String> {
    let targets: Vec<Target> = scan
        .entries
        .iter()
        .filter(|e| ids.contains(&e.id))
        .map(|e| Target { id: e.id.clone(), name: e.name.clone(), no: e.no, since: e.since, kind: e.kind })
        .collect();
    if targets.is_empty() {
        return Err("一个都没选（或选的人已经不在这份名单上）".into());
    }
    if targets.len() < ids.len() {
        return Err(format!("有 {} 人不在最新的名单上，重新查一遍再选", ids.len() - targets.len()));
    }
    Ok(targets)
}

async fn fresh_scan(group: &str, at: i64) -> Result<Scan, String> {
    let now = chrono::Utc::now().timestamp();
    let scan = roster::latest(group).await.ok_or("还没查过这个群，先查一遍")?;
    if scan.at != at {
        return Err("名单在你看的时候刷新过了，重新看一眼再选".into());
    }
    if !scan.fresh(now) {
        return Err("这份名单是一天以前的，重新查一遍".into());
    }
    Ok(scan)
}

/// 移出页面上勾的人。
pub async fn kick(ctx: &Context, group: &str, at: i64, ids: &[String]) -> Result<Value, String> {
    let (ctx, writer) = link(ctx)?;
    let config = get_config_or_default::<Config>(&ctx);
    if !config.enabled {
        return Err("潜水清理关着，先打开".into());
    }
    let scan = fresh_scan(group, at).await?;
    if !scan.can_kick {
        return Err("机器人在这个群不是管理员，移不出人".into());
    }
    let targets = chosen(&scan, ids)?;
    if targets.len() > config.pace.max_per_run as usize {
        return Err(format!("一次最多移出 {} 人（选了 {} 人）；分几次来，或调高「一次最多移出」", config.pace.max_per_run, targets.len()));
    }
    let count = targets.len();
    let job = Job {
        id: chrono::Utc::now().timestamp_millis(),
        group: group.to_string(),
        group_name: scan.group_name.clone(),
        operator: String::new(),
        operator_name: "控制台".into(),
        source: "console".into(),
        reply_to: String::new(),
        permanent: config.permanent,
        rule: scan.rule.text.clone(),
        created: chrono::Utc::now().timestamp(),
        queue: targets,
        done: Vec::new(),
        state: State::Running,
        finished: 0,
        note: String::new(),
    };
    purge::submit(&ctx, &writer, job).await?;
    info!(target: LOG_TARGET, "控制台确认移出群 {group} 的 {count} 人");
    Ok(job_status(&ctx, group).await)
}

/// @ 页面上勾的人提醒一声。
pub async fn warn(ctx: &Context, group: &str, at: i64, ids: &[String]) -> Result<String, String> {
    let (ctx, writer) = link(ctx)?;
    let config = get_config_or_default::<Config>(&ctx);
    let scan = fresh_scan(group, at).await?;
    let people: Vec<(String, String)> = chosen(&scan, ids)?.into_iter().map(|t| (t.id, t.name)).collect();
    if super::warn::pick(&config.warn.text).is_none() {
        return Err("「提醒的话」是空的，先写一句".into());
    }
    let n = super::warn::send(&ctx, &writer, &config, group, &people).await.map_err(|e| format!("提醒没发出去：{e}"))?;
    Ok(format!("提醒了 {n} 人；{} 天后还没冒泡的再移出", config.warn.grace_days))
}

/// 这个群进行中的那批，与最近做完的一批。
pub async fn job_status(ctx: &Context, group: &str) -> Value {
    let config = get_config_or_default::<Config>(ctx);
    let active = purge::active(group).await;
    let last = purge::history(Some(group), 1).await.into_iter().next();
    json!({
        "active": active.map(|job| job_json(&job, &config)),
        "last": last.map(|job| job_json(&job, &config)),
    })
}

pub async fn job(ctx: &Context, group: &str) -> Value {
    job_status(ctx, group).await
}

pub async fn stop(group: &str) -> Result<String, String> {
    if purge::stop(group).await { Ok("下一个人之前停下".into()) } else { Err("这个群现在没有在移出".into()) }
}

/// 把一个人加进（或移出）本群的豁免名单。
pub async fn exempt(ctx: &Context, group: &str, id: &str, on: bool) -> Result<String, String> {
    let (group, id) = (group.to_string(), id.to_string());
    let (g, i) = (group.clone(), id.clone());
    update_config::<Config, _>(ctx, move |mut config| {
        let list = config.groups.entry(g).or_default().exempt.get_or_insert_with(Vec::new);
        list.retain(|x| *x != i);
        if on {
            list.push(i);
        }
        config
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(if on { format!("{id} 不会再进这个群的名单") } else { format!("{id} 不再豁免") })
}

/// 「现在的情况」那张卡。控制台同步取，读的是各份状态此刻的快照（正被改着就少一行）。
pub fn live(ctx: &Context) -> Value {
    let config = get_config_or_default::<Config>(ctx);
    let mut lines = vec![format!(
        "口径：{}{}。每人间隔约 {} 秒，每 {} 人歇 {} 秒，一次最多 {} 人。",
        super::judge::Rule {
            days: config.days,
            min_messages: config.min_messages,
            newcomer_days: config.newcomer_days,
            protect_titled: config.protect_titled,
            protect_level: config.protect_level,
        }
        .describe(),
        if config.groups.is_empty() { String::new() } else { format!("（{} 个群单独设了）", config.groups.len()) },
        config.pace.interval_seconds,
        config.pace.burst,
        config.pace.rest_seconds,
        config.pace.max_per_run
    )];
    for job in purge::peek_pending() {
        lines.push(match job.state {
            State::Interrupted => format!("「{}」上次那批被打断了，还剩 {} 人", job.group_name, job.queue.len()),
            _ => format!("「{}」正在移出：{}", job.group_name, purge::summary(&job)),
        });
    }
    if config.auto.enabled {
        let results = super::auto::peek_results();
        if config.auto.groups.is_empty() {
            lines.push("自动清理开着，但还没选群，所以什么都不做。".into());
        }
        for group in &config.auto.groups {
            let name = roster::peek_latest(group).map_or_else(|| group.clone(), |s| s.group_name);
            match results.get(group) {
                Some(result) => lines.push(format!("自动清理「{name}」：{result}")),
                None => lines.push(format!("自动清理「{name}」：还没到时候（每天 {}）", config.auto.time)),
            }
        }
    } else {
        lines.push("自动清理关着：只在群里或这里手动移出。".into());
    }
    json!({ "lines": lines })
}
