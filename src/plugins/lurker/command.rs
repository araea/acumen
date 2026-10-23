//! 群里的「潜水 …」。群主、群管理员（`managers = "staff"` 时）与机器人管理员能用。
//!
//! 移出要两步：「潜水 踢 1-20」先把这些人是谁、要多久说清楚，同一个人两分钟内回「确认」才动手。
//! 「确认」「取消」不必带指令前缀——这时它们一定是在回这件事，不会是闲聊。

use super::purge::{self, Fate, Job, State, Target};
use super::roster::{self, Category, Scan};
use super::sheet;
use super::{Config, GroupRule, LOG_TARGET};
use crate::adapters::satori::{LockedWriter, api, delivery_uncertain, send_msg};
use crate::command::{self, CommandMatch};
use crate::event::Context;
use crate::message::Message;
use crate::plugins::{PluginError, update_config};
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsScalar};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 「确认」的有效期。
const CONFIRM_WITHIN: Duration = Duration::from_mins(2);

fn usage(prefix: &str) -> String {
    [
        "潜水清理的用法（只有群主、管理员能用）：".to_string(),
        format!("· {prefix}潜水：查一遍本群谁在潜水，名单带序号；{prefix}潜水 60：这次按 60 天看"),
        format!("· {prefix}潜水 踢 1-20 25：移出这些序号的人，回「确认」才动手；也可以写「全部」「全部 除 3 7」"),
        format!("· {prefix}潜水 提醒：把名单上的人一次性提醒一遍；{prefix}潜水 提醒 1-20 私聊：只提醒这几个，并且私聊"),
        format!("· {prefix}潜水 进度 / 停 / 继续：看进展、停下、接着中断的那批"),
        format!("· {prefix}潜水 豁免 @某人：这个人永远不进名单；{prefix}潜水 取消豁免 @某人；{prefix}潜水 豁免：看名单"),
        format!("· {prefix}潜水 天数 45：本群改成 45 天没说话算潜水；{prefix}潜水 天数 默认：照全局"),
        format!("· {prefix}潜水 记录：最近几次移出了谁"),
        "群主、管理员、号主与豁免名单里的人不会进名单；进群不满天数的新人也不算。".to_string(),
    ]
    .join("\n")
}

/// 一次待确认的移出。
struct Plan {
    group: String,
    operator: String,
    targets: Vec<Target>,
    reply_to: String,
    at: Instant,
}

static PLANS: Mutex<Vec<Plan>> = Mutex::new(Vec::new());

fn plans() -> std::sync::MutexGuard<'static, Vec<Plan>> {
    let mut plans = PLANS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    plans.retain(|plan| plan.at.elapsed() < CONFIRM_WITHIN);
    plans
}

fn take_plan(group: &str, operator: &str) -> Option<Plan> {
    let mut plans = plans();
    let index = plans.iter().position(|p| p.group == group && p.operator == operator)?;
    Some(plans.remove(index))
}

/// 谁能用：机器人管理员；`staff` 时再加上群主、群管理员。
pub fn allowed(ctx: &Context, config: &Config) -> bool {
    crate::access::is_manager(ctx)
        || (config.managers == "staff" && ctx.as_message().and_then(|m| m.sender_role()).is_some_and(api::is_staff))
}

/// 不带前缀的「确认」「取消」：只在这个人有一件待确认的事时接。返回 true 表示接了。
pub async fn answer(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str) -> Result<bool, PluginError> {
    let Some(message) = ctx.as_message() else {
        return Ok(false);
    };
    let text = message.text().trim().trim_start_matches(|c: char| !c.is_alphanumeric());
    let word = text.trim_end_matches(['。', '！', '!', '.', ' ']);
    let yes = matches!(word, "确认" | "确定" | "确认移出" | "是");
    let no = matches!(word, "取消" | "算了" | "不了" | "否");
    if !yes && !no {
        return Ok(false);
    }
    let user = message.user_id().to_string();
    if !plans().iter().any(|p| p.group == group && p.operator == user) {
        return Ok(false);
    }
    let here = Here::of(ctx, writer, config, group);
    if yes {
        let said = here.confirm().await?;
        here.say(said).await?;
    } else {
        take_plan(group, &user);
        here.say("好，不移出了。").await?;
    }
    Ok(true)
}

/// 「潜水 …」。返回 false 表示这条不归这里管（照常往下传）。
pub async fn run(ctx: &Context, writer: &LockedWriter, config: &Config, group: &str, matched: CommandMatch) -> Result<bool, PluginError> {
    if !allowed(ctx, config) {
        return Ok(false);
    }
    let here = Here::of(ctx, writer, config, group);
    let arg = command::extract_text_arg(&matched.args);
    let arg = arg.trim();
    let (word, rest) = arg.split_once(char::is_whitespace).map_or((arg, ""), |(w, r)| (w, r.trim()));
    let said = match word {
        "" | "名单" | "列表" | "查" => here.list(None).await?,
        days if days.parse::<u32>().is_ok() => match days.parse::<u32>().ok().filter(|d| (1..=3650).contains(d)) {
            Some(days) => here.list(Some(days)).await?,
            None => "天数写 1 到 3650 之间的数".into(),
        },
        "踢" | "移出" | "清理" | "清" => here.plan(rest).await,
        "确认" | "确定" => here.confirm().await?,
        "取消" => {
            if take_plan(group, &here.user).is_some() { "好，不移出了。".into() } else { String::new() }
        }
        "提醒" | "警告" | "通知" => here.warn(rest).await?,
        "进度" | "状态" => here.progress().await,
        "停" | "停下" | "停止" => {
            if purge::stop(group).await { "好，下一个人之前停下。".into() } else { "本群现在没有在移出。".into() }
        }
        "继续" => match purge::resume(ctx, writer, group).await {
            Some(left) => format!("接着移出剩下的 {left} 人。"),
            None => "没有中断的那批要接着做。".into(),
        },
        "豁免" | "白名单" => here.exempt(&matched, rest, true).await?,
        "取消豁免" | "移出豁免" => here.exempt(&matched, rest, false).await?,
        "天数" | "规则" => here.days(rest).await?,
        "记录" | "历史" => here.history().await,
        "帮助" | "-h" | "help" | "用法" => usage(&here.prefix),
        // 没有指令前缀时「潜水 好久了」只是在聊天，不接话。
        _ if command::get_prefixes(ctx).iter().all(String::is_empty) => return Ok(false),
        _ => format!("不认识「{word}」\n{}", usage(&here.prefix)),
    };
    if !said.is_empty() {
        here.say(said).await?;
    }
    Ok(true)
}

struct Here<'a> {
    ctx: &'a Context,
    writer: &'a LockedWriter,
    config: &'a Config,
    group: &'a str,
    user: String,
    user_name: String,
    message_id: String,
    prefix: String,
}

impl<'a> Here<'a> {
    fn of(ctx: &'a Context, writer: &'a LockedWriter, config: &'a Config, group: &'a str) -> Self {
        let message = ctx.as_message();
        Self {
            ctx,
            writer,
            config,
            group,
            user: message.as_ref().map(|m| m.user_id().to_string()).unwrap_or_default(),
            user_name: message.as_ref().map(|m| m.sender_name().to_string()).unwrap_or_default(),
            message_id: message.as_ref().map(|m| m.message_id().to_string()).unwrap_or_default(),
            prefix: command::get_prefixes(ctx).into_iter().next().unwrap_or_default(),
        }
    }

    async fn say(&self, text: impl Into<String>) -> Result<(), PluginError> {
        let message = Message::new().reply(&self.message_id).text(text.into());
        send_msg(self.ctx, self.writer.clone(), Some(self.group), None, message).await?;
        Ok(())
    }

    /// 查一遍并发名单。
    async fn list(&self, days: Option<u32>) -> Result<String, PluginError> {
        let scan = match roster::scan(self.ctx, self.writer, self.config, self.group, days).await {
            Ok(scan) => scan,
            Err(error) => return Ok(format!("没查成：{error}")),
        };
        self.say(headline(&scan, &self.prefix)).await?;
        if !scan.entries.is_empty() || !scan.unsure.is_empty() {
            deliver(self.ctx, self.writer, self.config, &scan, &self.prefix).await?;
        }
        Ok(String::new())
    }

    async fn scan(&self) -> Result<Scan, String> {
        let now = chrono::Utc::now().timestamp();
        match roster::latest(self.group).await {
            Some(scan) if scan.fresh(now) => Ok(scan),
            Some(_) => Err(format!("名单是一天以前查的，先发「{}潜水」重新查一遍", self.prefix)),
            None => Err(format!("先发「{}潜水」查一遍名单，再按序号来", self.prefix)),
        }
    }

    /// 「踢 …」：说清楚要移出谁，等确认。
    async fn plan(&self, rest: &str) -> String {
        let scan = match self.scan().await {
            Ok(scan) => scan,
            Err(text) => return text,
        };
        if !scan.can_kick {
            return "机器人在本群不是管理员，移不出人。".into();
        }
        if let Some(job) = purge::active(self.group).await {
            return match job.state {
                State::Interrupted => format!("上次那批中断了，还剩 {} 人；发「{}潜水 继续」接着做，或「{}潜水 停」作罢", job.queue.len(), self.prefix, self.prefix),
                _ => format!("本群正在移出（{}），做完再来", purge::summary(&job)),
            };
        }
        let picks = match picks(rest, scan.entries.len()) {
            Ok(picks) => picks,
            Err(text) => return text,
        };
        let max = self.config.pace.max_per_run as usize;
        if picks.len() > max {
            return format!("一次最多移出 {max} 人（这次选了 {} 人）；分几次来，或在控制台调高 pace.max_per_run", picks.len());
        }
        let targets: Vec<Target> = picks
            .iter()
            .filter_map(|&no| scan.numbered(no))
            .map(|e| Target { id: e.id.clone(), name: e.name.clone(), no: e.no, since: e.since, kind: e.kind })
            .collect();
        let count = targets.len();
        let names: Vec<&str> = targets.iter().take(8).map(|t| t.name.as_str()).collect();
        let mut text = format!(
            "要把这 {count} 人移出群聊（序号 {}）：{}{}。\n分批慢慢移出，大约 {}；排队时说了话的会留下。",
            ranges(&picks),
            names.join("、"),
            if count > names.len() { format!(" 等 {count} 人") } else { String::new() },
            crate::clock::duration_text(round_minutes(purge::estimate(self.config, count))),
        );
        if self.config.permanent {
            text.push_str("移出后他们不能再申请加群。");
        }
        if count * 2 > scan.total {
            text.push_str(&format!("\n⚠ 这是全群 {} 人里的一半以上，确认前再看一眼名单。", scan.total));
        }
        text.push_str(&format!("\n{} 分钟内回复「确认」开始，回复「取消」作罢。", CONFIRM_WITHIN.as_secs() / 60));
        let mut plans = plans();
        plans.retain(|p| !(p.group == self.group && p.operator == self.user));
        plans.push(Plan {
            group: self.group.to_string(),
            operator: self.user.clone(),
            targets,
            reply_to: self.message_id.clone(),
            at: Instant::now(),
        });
        text
    }

    async fn confirm(&self) -> Result<String, PluginError> {
        let Some(plan) = take_plan(self.group, &self.user) else {
            return Ok("没有等你确认的移出（两分钟没回就作废了），重新发「潜水 踢 …」".into());
        };
        let scan = roster::latest(self.group).await;
        let count = plan.targets.len();
        let job = Job {
            id: chrono::Utc::now().timestamp_millis(),
            group: self.group.to_string(),
            group_name: scan.as_ref().map(|s| s.group_name.clone()).unwrap_or_else(|| self.group.to_string()),
            operator: self.user.clone(),
            operator_name: self.user_name.clone(),
            source: "group".into(),
            reply_to: plan.reply_to,
            permanent: self.config.permanent,
            rule: scan.as_ref().map(|s| s.rule.text.clone()).unwrap_or_default(),
            created: chrono::Utc::now().timestamp(),
            queue: plan.targets,
            done: Vec::new(),
            state: State::Running,
            finished: 0,
            note: String::new(),
        };
        info!(target: LOG_TARGET, "群 {} 的 {}（{}）确认移出 {count} 人", self.group, self.user_name, self.user);
        Ok(match purge::submit(self.ctx, self.writer, job).await {
            Ok(()) => format!(
                "开始移出 {count} 人，大约 {}。发「{p}潜水 进度」看进展，「{p}潜水 停」随时停下。",
                crate::clock::duration_text(round_minutes(purge::estimate(self.config, count))),
                p = self.prefix
            ),
            Err(text) => text,
        })
    }

    async fn warn(&self, rest: &str) -> Result<String, PluginError> {
        if matches!(rest, "状态" | "进度" | "情况") {
            return Ok(super::warn::status(self.group).await);
        }
        let scan = match self.scan().await {
            Ok(scan) => scan,
            Err(text) => return Ok(text),
        };
        // 不写序号就是名单上全部的人；末尾可以跟一句用哪种方式提醒。
        let (mode, rest) = super::warn::parse_mode(rest);
        let picks: Vec<u32> = if rest.trim().is_empty() {
            (1..=u32::try_from(scan.entries.len()).unwrap_or(u32::MAX)).collect()
        } else {
            match picks(&rest, scan.entries.len()) {
                Ok(picks) => picks,
                Err(text) => return Ok(text),
            }
        };
        if scan.entries.is_empty() {
            return Ok(format!("名单上没有要提醒的人。按「{}」查过了，本群没有潜水的。", scan.rule.text));
        }
        let now = chrono::Utc::now().timestamp();
        // 看提醒记录本身，不看名单上的标记：名单可能是提醒之前查的。
        let warned = super::warn::warned(self.group).await;
        let (recent, people): (Vec<_>, Vec<_>) = picks
            .iter()
            .filter_map(|&no| scan.numbered(no))
            .partition(|e| warned.get(&e.id).is_some_and(|at| now - at < 86_400));
        let people: Vec<(String, String)> = people.into_iter().map(|e| (e.id.clone(), e.name.clone())).collect();
        if people.is_empty() {
            return Ok("这些人今天都提醒过了。发「潜水 提醒 状态」看还剩谁。".into());
        }
        let mut config = self.config.clone();
        if let Some(mode) = &mode {
            config.warn.mode.clone_from(mode);
        }
        let report = match super::warn::send(self.ctx, self.writer, &config, self.group, &people).await {
            Ok(report) => report,
            Err(error) => return Ok(error.to_string()),
        };
        let mut text = report.describe(&config);
        if !recent.is_empty() {
            text.push_str(&format!("另有 {} 人今天已经提醒过，跳过了。", recent.len()));
        }
        if !self.prefix.is_empty() {
            text.push_str(&format!("重新查一遍：「{p}潜水」；看提醒进展：「{p}潜水 提醒 状态」。", p = self.prefix));
        }
        Ok(text)
    }

    async fn progress(&self) -> String {
        match purge::active(self.group).await {
            Some(job) if job.state == State::Interrupted => format!(
                "上次那批被打断了：已移出 {} 人，还剩 {} 人。发「{p}潜水 继续」接着做，「{p}潜水 停」作罢。",
                job.count(Fate::Removed),
                job.queue.len(),
                p = self.prefix
            ),
            Some(job) => {
                let left = job.queue.len();
                format!(
                    "{}还剩 {left} 人，大约 {}。",
                    purge::summary(&job).trim_end_matches('。').to_string() + "；",
                    crate::clock::duration_text(round_minutes(purge::estimate(self.config, left)))
                )
            }
            None => match purge::history(Some(self.group), 1).await.first() {
                Some(job) => format!("本群现在没有在移出。上一次（{}）：{}", super::date(job.created), purge::summary(job)),
                None => "本群现在没有在移出。".into(),
            },
        }
    }

    async fn history(&self) -> String {
        let jobs = purge::history(Some(self.group), 5).await;
        if jobs.is_empty() {
            return "本群还没有移出过人。".into();
        }
        let mut lines = vec!["最近几次：".to_string()];
        for job in &jobs {
            let who = if job.operator_name.is_empty() { "控制台".to_string() } else { job.operator_name.clone() };
            let removed: Vec<&str> = job.done.iter().filter(|o| o.result == Fate::Removed).map(|o| o.name.as_str()).collect();
            let mut line = format!("· {} {who}：移出 {} 人", stamp(job.created), removed.len());
            if !removed.is_empty() {
                line.push_str(&format!("（{}{}）", removed.iter().take(6).copied().collect::<Vec<_>>().join("、"), if removed.len() > 6 { "…" } else { "" }));
            }
            lines.push(line);
        }
        lines.join("\n")
    }

    /// 豁免名单：加、删、看。
    async fn exempt(&self, matched: &CommandMatch, rest: &str, add: bool) -> Result<String, PluginError> {
        let mut ids: Vec<String> = matched
            .args
            .iter()
            .filter(|seg| seg.get_str("type") == Some("at"))
            .filter_map(|seg| {
                let data = seg.get("data")?;
                data.get_str("qq").map(str::to_owned).or_else(|| data.get_i64("qq").map(|n| n.to_string()))
            })
            .collect();
        ids.extend(rest.split(|c: char| c.is_whitespace() || c == ',' || c == '，').filter(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_digit())).map(str::to_owned));
        let me = self.ctx.bot.self_id();
        ids.retain(|id| *id != me && id != crate::message::AT_ALL);
        ids.sort();
        ids.dedup();
        let current = roster::exempt_for(self.config, self.group);
        if ids.is_empty() {
            if !add {
                return Ok("要把谁移出豁免名单？发「潜水 取消豁免 @某人」".into());
            }
            return Ok(if current.is_empty() {
                "本群还没有豁免的人。发「潜水 豁免 @某人」加一个：他永远不会进名单。".into()
            } else {
                format!("不会进名单的人（{} 个）：{}", current.len(), current.join("、"))
            });
        }
        let group = self.group.to_string();
        let changed = ids.clone();
        update_config::<Config, _>(self.ctx, move |mut config| {
            let own = config.groups.entry(group).or_default();
            let list = own.exempt.get_or_insert_with(Vec::new);
            if add {
                for id in changed {
                    if !list.contains(&id) {
                        list.push(id);
                    }
                }
            } else {
                list.retain(|id| !changed.contains(id));
            }
            config
        })
        .await?;
        let global: Vec<&String> = ids.iter().filter(|id| self.config.exempt.contains(id)).collect();
        let mut text = if add {
            format!("好，{} 不会再进本群的潜水名单。", ids.join("、"))
        } else {
            format!("好，{} 不再豁免。", ids.join("、"))
        };
        if !add && !global.is_empty() {
            text.push_str(&format!("（{} 在全局豁免名单里，要在控制台删）", global.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("、")));
        }
        Ok(text)
    }

    /// 本群的天数：看、改、照全局。
    async fn days(&self, rest: &str) -> Result<String, PluginError> {
        let rule = roster::rule_for(self.config, self.group);
        let own = self.config.groups.get(self.group).and_then(|g| g.days);
        if rest.is_empty() {
            return Ok(format!(
                "本群的口径：{}{}。改：「{}潜水 天数 45」；照全局：「{}潜水 天数 默认」",
                rule.describe(),
                if own.is_some() { "（本群单独设的）" } else { "（照全局）" },
                self.prefix,
                self.prefix
            ));
        }
        let value = match rest {
            "默认" | "全局" | "重置" => None,
            text => match text.trim_end_matches('天').trim().parse::<u32>() {
                Ok(days) if (1..=3650).contains(&days) => Some(days),
                _ => return Ok("天数写 1 到 3650 之间的数，或「默认」".into()),
            },
        };
        let group = self.group.to_string();
        update_config::<Config, _>(self.ctx, move |mut config| {
            match value {
                Some(days) => config.groups.entry(group).or_default().days = Some(days),
                None => {
                    if let Some(own) = config.groups.get_mut(&group) {
                        own.days = None;
                        if *own == GroupRule::default() {
                            config.groups.remove(&group);
                        }
                    }
                }
            }
            config
        })
        .await?;
        Ok(match value {
            Some(days) => format!("好，本群改成 {days} 天没说话算潜水。"),
            None => format!("好，本群照全局：{} 天。", self.config.days),
        })
    }
}

/// 名单发出去之前的那句话：多少人、怎么办。
fn headline(scan: &Scan, prefix: &str) -> String {
    let mut text = if scan.entries.is_empty() {
        format!("全群 {} 人，按「{}」查了一遍，没有要清理的。", scan.total, scan.rule.text)
    } else {
        let never = scan.entries.iter().filter(|e| e.kind == Category::Never).count();
        format!(
            "全群 {} 人，按「{}」查到 {} 人{}。名单见下面的聊天记录，按序号移出：「{prefix}潜水 踢 1-{}」。",
            scan.total,
            scan.rule.text,
            scan.entries.len(),
            if never > 0 { format!("（其中 {never} 人进群后没说过话）") } else { String::new() },
            scan.entries.len()
        )
    };
    if let Some(note) = roster::source_note(scan) {
        text.push('\n');
        text.push_str(&note);
    }
    if !scan.can_kick {
        text.push_str("\n机器人在本群不是管理员，只能看。");
    }
    text
}

/// 把名单装进合并转发发出去：长图在前，文字名单在后；出图不成就只发文字。
pub async fn deliver(ctx: &Context, writer: &LockedWriter, config: &Config, scan: &Scan, prefix: &str) -> Result<(), PluginError> {
    let me = ctx.bot.self_id();
    let bot_name = ctx.bot.login_user.get().name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| "潜水名单".into());
    let mut images = Vec::new();
    if config.list.image {
        let ids: Vec<String> = scan.entries.iter().chain(scan.unsure.iter()).map(|e| e.id.clone()).collect();
        let qq = matches!(ctx.bot.platform.as_str(), "red" | "qq" | "QQ");
        let avatars = if config.list.avatars { sheet::avatars(&ids, qq).await } else { Default::default() };
        let pages = sheet::pages(scan, config.list.rows_per_image as usize, prefix, &avatars);
        match sheet::render(&pages, ctx.browser_path().as_deref()).await {
            Ok(shots) => images = shots,
            Err(error) => warn!(target: LOG_TARGET, "名单没出成图，改发文字：{error}"),
        }
    }
    let texts = sheet::text(scan, 60);
    let mut forward = Message::new();
    for (index, image) in images.iter().enumerate() {
        let name = if images.len() > 1 { format!("潜水名单 {}/{}", index + 1, images.len()) } else { "潜水名单".into() };
        forward = forward.node_custom(&me, name, Message::new().image(format!("base64://{image}")));
    }
    for text in &texts {
        forward = forward.node_custom(&me, bot_name.clone(), Message::new().text(text.clone()));
    }
    match send_msg(ctx, writer.clone(), Some(&scan.group), None, forward).await {
        Ok(()) => Ok(()),
        Err(error) if delivery_uncertain(&*error) => Err(error),
        Err(error) => {
            warn!(target: LOG_TARGET, "名单的合并转发没发出去，改发文字：{error}");
            let first = texts.first().cloned().unwrap_or_default();
            let more = if texts.len() > 1 { "\n……名单太长，完整的请在控制台看。" } else { "" };
            send_msg(ctx, writer.clone(), Some(&scan.group), None, Message::new().text(format!("{first}{more}"))).await?;
            Ok(())
        }
    }
}

/// 整分钟往上取：「大约 3 分钟」，不说「2 分 41 秒」。
fn round_minutes(seconds: u64) -> u64 {
    seconds.div_ceil(60).max(1) * 60
}

fn stamp(at: i64) -> String {
    use chrono::TimeZone;
    crate::clock::beijing().timestamp_opt(at, 0).single().map_or_else(String::new, |t| t.format("%m-%d %H:%M").to_string())
}

/// 序号区间：「1-20 25」「1~5，8、10到12」「全部」「全部 除 3 7」「1-30 !5」。
pub fn picks(text: &str, max: usize) -> Result<Vec<u32>, String> {
    let max = u32::try_from(max).unwrap_or(u32::MAX);
    if max == 0 {
        return Err("名单上没有人".into());
    }
    let text: String = text
        .chars()
        .map(|c| match c {
            '０'..='９' => char::from_u32(c as u32 - '０' as u32 + '0' as u32).unwrap_or(c),
            '～' | '~' | '—' | '–' | '－' | '到' | '至' => '-',
            '，' | ',' | '、' | '；' | ';' => ' ',
            '！' => '!',
            _ => c,
        })
        .collect();
    let mut chosen: Vec<u32> = Vec::new();
    let mut dropped: Vec<u32> = Vec::new();
    let mut excluding = false;
    let parse = |n: &str| -> Result<u32, String> {
        let n: u32 = n.trim().parse().map_err(|_| format!("「{n}」不是序号；写法如「潜水 踢 1-20 25」「潜水 踢 全部 除 3 7」"))?;
        if n == 0 || n > max {
            return Err(format!("序号 {n} 不在名单上（1-{max}）"));
        }
        Ok(n)
    };
    for token in text.split_whitespace() {
        let (token, negate) = match token.strip_prefix('!') {
            Some(rest) => (rest, true),
            None => (token, false),
        };
        if matches!(token, "除" | "除了" | "不要" | "排除" | "不含") {
            excluding = true;
            continue;
        }
        let range: Vec<u32> = match token {
            "全部" | "所有" | "全" | "all" => (1..=max).collect(),
            "" => continue,
            _ => match token.split_once('-') {
                Some((a, b)) => {
                    let (a, b) = (parse(a)?, parse(b)?);
                    (a.min(b)..=a.max(b)).collect()
                }
                None => vec![parse(token)?],
            },
        };
        if excluding || negate {
            dropped.extend(range);
        } else {
            chosen.extend(range);
        }
    }
    if chosen.is_empty() && !dropped.is_empty() {
        chosen = (1..=max).collect();
    }
    chosen.retain(|n| !dropped.contains(n));
    chosen.sort_unstable();
    chosen.dedup();
    if chosen.is_empty() {
        return Err("要移出哪几个？写序号，比如「潜水 踢 1-20 25」，或「全部」".into());
    }
    Ok(chosen)
}

/// 把序号写回「1-20、25」。
pub fn ranges(picks: &[u32]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut iter = picks.iter().copied().peekable();
    while let Some(start) = iter.next() {
        let mut end = start;
        while iter.peek() == Some(&(end + 1)) {
            end = iter.next().unwrap_or(end);
        }
        out.push(if start == end { start.to_string() } else { format!("{start}-{end}") });
    }
    out.join("、")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_understand_ranges_lists_and_exclusions() {
        assert_eq!(picks("1-3 5", 10).unwrap(), vec![1, 2, 3, 5]);
        assert_eq!(picks("３～１，8、10到10", 10).unwrap(), vec![1, 2, 3, 8, 10]);
        assert_eq!(picks("全部 除 2 4-9", 10).unwrap(), vec![1, 3, 10]);
        assert_eq!(picks("1-5 !3", 10).unwrap(), vec![1, 2, 4, 5]);
        assert_eq!(picks("!1", 3).unwrap(), vec![2, 3]);
        assert!(picks("11", 10).unwrap_err().contains("不在名单上"));
        assert!(picks("", 10).is_err());
        assert!(picks("abc", 10).is_err());
    }

    #[test]
    fn ranges_fold_back() {
        assert_eq!(ranges(&[1, 2, 3, 5, 7, 8]), "1-3、5、7-8");
        assert_eq!(ranges(&[4]), "4");
    }
}
