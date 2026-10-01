//! An opt-in, low-frequency visual joke for a *direct* "are you a bot?" question.
//! QQ renders its own same-group history; this image is not proof of human identity.
use super::*;
use crate::adapters::satori::qq;
use serde::Deserialize;

const CAPTION: &str = "真人认证（？）先把刚才的聊天记录截给你看，虽然这图也证明不了啥";

#[derive(Debug, Deserialize)]
struct Screenshot {
    file: String,
    mime: String,
    count: usize,
}

/// Use only fresh, directed questions. Never use an AI model's untrusted guessed IDs.
fn candidate(turns: &[Turn], now: i64) -> Option<(String, String)> {
    let last = turns.last()?;
    if last.from_me
        // satori-qq 给 QQ 客户端手发消息挂的虚拟作者不是群友，谈不上在质问我们。
        || last.user_id.is_empty()
        || last.user_id.starts_with("qq-client:")
        || last.message_id.is_empty()
        || now.saturating_sub(last.at) > 90
        || last.at > now + 10
        || !(last.mentions_me || last.call.named_me)
    {
        return None;
    }
    let text: String = last
        .text
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let suspected = ["机器人", "人机", "bot", "ai"].iter().any(|word| {
        ["是不是", "你是", "像", "怀疑你是", "你就是"]
            .iter()
            .any(|prefix| text.contains(&format!("{prefix}{word}")))
    });
    if !suspected {
        return None;
    }
    // 只给本群眼前的 3 条，防止一次玩笑顺带曝光半小时前的长段记录。
    // 按窗口里的位置取：窗口顺序就是到达顺序，消息号不必可比大小。
    let first = turns
        .iter()
        .rev()
        .take(3)
        .filter(|turn| !turn.message_id.is_empty() && now.saturating_sub(turn.at) <= 90)
        .last()
        .map_or_else(|| last.message_id.clone(), |turn| turn.message_id.clone());
    Some((first, last.message_id.clone()))
}

/// Returns true only after a confirmed send. Failures fall through to ordinary ambient speech.
pub(super) async fn try_reply(
    ctx: &Context,
    writer: &LockedWriter,
    group: &str,
    config: &AmbientConfig,
    seq: u64,
    turns: &[Turn],
) -> bool {
    if !config.screenshot_on_suspicion || ctx.bot.adapter != "satori-qq" {
        return false;
    }
    let Some((start, end)) = candidate(turns, chrono::Local::now().timestamp()) else {
        return false;
    };
    if !window::with_group(group, |state| {
        state.allow_screenshot(config.screenshot_cooldown_seconds) && !state.screenshot_seen(&end)
    }) || !super::current(ctx, group, seq)
    {
        return false;
    }
    let channel = group.to_string();
    let mut image: Result<Screenshot, _> = qq::call(ctx, writer, "chat_screenshot", serde_json::json!({
        "channel_id": channel, "start_message_id": start, "end_message_id": end
    })).await;
    // On a cold QQ history cache the earliest local message can be absent. Retry once with
    // just the provoking message; never return a partial or unrelated group image.
    if image.is_err() && start != end {
        image = qq::call(ctx, writer, "chat_screenshot", serde_json::json!({
            "channel_id": channel, "start_message_id": end, "end_message_id": end
        })).await;
    }
    let image = match image {
        Ok(image) => image,
        Err(error) => {
            debug!(target: LOG_TARGET, "群 {group} 聊天记录截图不可用：{error}");
            return false;
        }
    };
    let prefix = format!("internal:red/{}/_tmp/", ctx.bot.login_user.get().id);
    if image.mime != "image/png"
        || image.count == 0
        || image.count > 40
        || !image.file.starts_with(&prefix)
        || image.file[prefix.len()..].is_empty()
        || image.file[prefix.len()..].contains('/')
    {
        warn!(target: LOG_TARGET, "群 {group} 聊天记录截图返回无效资源");
        return false;
    }
    // While the kernel was rendering, a new chat message may have arrived. Anchor specifically
    // to the provoking ID, not simply the current latest inbound message.
    if !super::current(ctx, group, seq) {
        return false;
    }
    let Some(freshness) = freshness_for(
        group,
        Duration::from_secs(config.send_freshness_seconds.clamp(5, 60)),
    )
    .filter(|f| f.message_id == end) else {
        return false;
    };
    // Send the image on its own. QQ may split a mixed image/text into two messages; if the
    // second fails the first can already be visible despite the RPC reporting an error.
    let picture = Message::new().image(image.file);
    let sent = match send_fresh_msg_id(
        ctx,
        writer.clone(),
        Some(group),
        None,
        &picture,
        Some(freshness.clone()),
    )
    .await
    {
        Ok(Some(id)) => id,
        Ok(None) => return false,
        Err(error) => {
            // Timeout has an unknown outcome. Do not answer a second time and risk a duplicate.
            window::with_group(group, |state| state.mark_screenshot(&end));
            warn!(target: LOG_TARGET, "群 {group} 聊天记录截图发送结果未知：{error}");
            return true;
        }
    };
    window::with_group(group, |state| {
        state.mark_screenshot(&end);
        state.mark_spoke();
        state.receive(Turn {
            user_id: ctx.bot.self_id(),
            name: "我".into(),
            text: "[聊天记录图]".into(),
            elements: picture,
            message_id: sent,
            from_me: true,
            at: chrono::Local::now().timestamp(),
            ..Turn::default()
        });
    });
    // The caption is best effort. Never send it if somebody has already moved the conversation on.
    if super::current(ctx, group, seq)
        && freshness_for(group, Duration::from_secs(25))
            .is_some_and(|f| f.message_id == end)
    {
        let caption = Message::new().text(CAPTION);
        match send_fresh_msg_id(
            ctx,
            writer.clone(),
            Some(group),
            None,
            &caption,
            Some(freshness),
        )
        .await
        {
            Ok(Some(id)) => window::with_group(group, |state| {
                state.receive(Turn {
                    user_id: ctx.bot.self_id(),
                    name: "我".into(),
                    text: CAPTION.into(),
                    elements: caption,
                    message_id: id,
                    from_me: true,
                    at: chrono::Local::now().timestamp(),
                    ..Turn::default()
                });
            }),
            Ok(None) => {}
            Err(error) => warn!(target: LOG_TARGET, "群 {group} 截图附言结果未知：{error}"),
        }
    }
    info!(target: LOG_TARGET, "群 {group} 发送聊天记录截图（{start}..{end}，{} 条）", image.count);
    true
}
