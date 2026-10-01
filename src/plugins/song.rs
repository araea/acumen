//! 点歌：一句话在 B 站找到一首歌，把能听的那个版本发进群。
//!
//! `点歌 <歌名>` 之后的一切都在幕后完成：B 站搜一遍、便宜模型从结果里挑一条
//! 最贴近的（没点名歌手时优先原唱与官方 MV），再交由视频解析的同一条链路取片
//! 进群（[`crate::plugins::video_parse::take_by_bvid`]，画质、体积上限与发法
//! 都读 `[video_parse]` 的配置）。成品时长有上限——群里要的是一首歌，不是一部
//! 几个小时的合集。
//!
//! 群里只有成品那一条消息：不回「正在找」，搜不到、挑不出、取不到也都不吭声，
//! 原因写进日志。模型失手时不至于没结果——解析不出编号就退回相关度最高的那条。
//!
//! 另有一条手动指令「导出音频」（[`export`]）：引用一条视频消息发过去，画面去掉、
//! 声音作为一个音频文件发回群里。这条是用户点名的活，成不了会在群里回一句原因。

mod export;
mod search;

use crate::adapters::satori::{LockedWriter, send_msg};
use crate::command::{extract_text_arg, first_command_match, match_command};
use crate::config::build_config;
use crate::event::Context;
use crate::message::Message;
use crate::plugins::oai::llm;
use crate::plugins::oai::{self};
use crate::plugins::video_parse;
use crate::plugins::{ChannelConfig, PluginError, get_config_or_default};
use anyhow::Result;
use futures_util::future::BoxFuture;
use rig_core::completion::Message as LlmMessage;
use rig_core::completion::message::{Text, UserContent};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time;
use toml::Value;

const LOG_TARGET: &str = "Plugin/Song";

/// 触发词。不带参数的「点歌」只在日志里记一笔，不在群里教用法。
const COMMANDS: &[&str] = &["点歌"];

// ================= Config =================

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    /// 挑结果用的模型，写 `供应商/模型`（接口取自 `[oai.providers]`）。
    /// 判断只看标题、UP 主与时长，便宜档足够。
    pub model: String,
    /// 交给模型的候选上限，取相关度靠前的几条。
    pub candidates: u32,
    /// 成品时长上限（秒）：超长的直接从候选里去掉，不让几个小时的视频进群。
    pub max_seconds: u64,
    /// 等模型挑结果的时间上限（秒）；等不到就退回相关度最高的候选。
    pub timeout_seconds: u64,
    /// 群名单：配了黑名单就对名单外的所有群生效，配了白名单则只对名单内的群生效。
    pub channel: ChannelConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            model: "mimo/mimo-v2.6-flash".to_string(),
            candidates: 10,
            max_seconds: 600,
            timeout_seconds: 30,
            channel: ChannelConfig::default(),
        }
    }
}

pub fn default_config() -> Value {
    build_config(Config::default())
}

// ================= Main Handler =================

pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let Some(msg) = ctx.as_message() else {
            return Ok(Some(ctx));
        };
        let config: Config = get_config_or_default(&ctx, "song");
        if !config.enabled {
            return Ok(Some(ctx));
        }
        if !config.channel.allows(msg.group_id()) {
            return Ok(Some(ctx));
        }
        // 号主与机器人共用同一个 QQ 号：只跳过机器人自己的回声。
        let self_id = ctx.bot.self_id();
        if msg.user_id() == self_id && !msg.is_manual_self() {
            return Ok(Some(ctx));
        }

        // 手动指令「导出音频」：引用一条视频消息，画面去掉、声音作为文件进群。
        // 与点歌不同，这是用户点名的活，成不了要在群里给个说法。
        if let Some(matched) = export::COMMANDS.iter().find_map(|cmd| match_command(&ctx, cmd)) {
            if let Err(error) = export::run(&ctx, &writer, &matched).await {
                warn!(target: LOG_TARGET, "导出音频失败：{error}");
                let _ = send_msg(
                    &ctx,
                    writer.clone(),
                    msg.group_id(),
                    Some(msg.user_id()),
                    Message::new()
                        .reply(msg.message_id())
                        .text(format!("音频没导出来：{error}")),
                )
                .await;
            }
            return Ok(None);
        }

        let Some(matched) = first_command_match(&ctx, COMMANDS) else {
            return Ok(Some(ctx));
        };
        let keyword = extract_text_arg(&matched.args);

        // 从这里起事件归本插件：成败都不再交给后面的插件，群里也不多说一个字。
        if keyword.is_empty() {
            info!(target: LOG_TARGET, "点歌没带关键词，不作声");
            return Ok(None);
        }
        if let Err(error) = request(&ctx, &writer, &config, &keyword, msg.group_id(), msg.user_id())
            .await
        {
            warn!(target: LOG_TARGET, "点歌失败（{keyword}）：{error}");
        }
        Ok(None)
    })
}

// ================= 点歌 =================

/// 一次点歌的完整流程：搜索 → 挑选 → 取片进群。
async fn request(
    ctx: &Context,
    writer: &LockedWriter,
    config: &Config,
    keyword: &str,
    group_id: Option<&str>,
    user_id: &str,
) -> Result<()> {
    // 拉一整页再筛：时长超限的去掉，剩下的按相关度取前几条交给模型。
    let found = search::videos(keyword, 20).await?;
    let max = config.max_seconds.max(1);
    let pool: Vec<_> = found
        .into_iter()
        .filter(|item| item.duration <= max)
        .take(config.candidates.max(1) as usize)
        .collect();
    let Some(fallback) = pool.first() else {
        anyhow::bail!("B 站没有搜到时长合格的结果");
    };

    // 模型挑编号；挑不出就用相关度最高的那条，点歌不能没结果。
    let picked = match pick(ctx, config, keyword, &pool).await {
        Some((index, candidate)) => {
            info!(target: LOG_TARGET, "模型选中第 {} 条", index + 1);
            candidate
        }
        None => {
            warn!(target: LOG_TARGET, "模型没挑出来，退回相关度最高的一条");
            fallback
        }
    };
    info!(
        target: LOG_TARGET,
        "点歌：{keyword} → {}（{} · {} · 播放 {}）",
        picked.title,
        picked.bvid,
        picked.duration_label(),
        picked.play,
    );

    let take = get_config_or_default::<video_parse::Config>(ctx, "video_parse");
    video_parse::take_by_bvid(ctx, writer, &take, &picked.bvid, group_id, user_id).await
}

/// 让便宜模型从候选里挑一条，返回（下标，候选）。
///
/// 失败的原因不需要往外说：模型超时、接口没配、回话不像编号，统统交给
/// 调用方退回第一条。
async fn pick<'a>(
    ctx: &Context,
    config: &Config,
    keyword: &str,
    pool: &'a [search::Candidate],
) -> Option<(usize, &'a search::Candidate)> {
    let (base, key, model) = model_endpoint(ctx, &config.model).await?;
    let history = vec![LlmMessage::User {
        content: vec![UserContent::Text(Text::new(prompt_for(keyword, pool)))],
    }];
    let budget = Duration::from_secs(config.timeout_seconds.clamp(5, 120));
    let reply = time::timeout(budget, llm::complete(&base, &key, &model, history, None))
        .await
        .ok()?
        .ok()?;
    let index = parse_choice(&reply, pool.len())?;
    Some((index, &pool[index]))
}

/// 模型写法 `供应商/模型` → 接口地址与密钥，口径与搭话那边一致。
async fn model_endpoint(ctx: &Context, model: &str) -> Option<(String, String, String)> {
    let (provider, model_id) = oai::utils::split_provider(model);
    let oai_config = get_config_or_default::<oai::OaiConfig>(ctx, "oai");
    let manager = oai::data::MANAGER.get()?;
    let (default_base, default_key) = {
        let stored = manager.config.read().await;
        (stored.api_base.clone(), stored.api_key.clone())
    };
    let (base, key) = oai::resolve_endpoint(
        &oai_config.providers,
        &default_base,
        &default_key,
        provider.as_deref(),
    )?;
    Some((base, key, model_id))
}

/// 给模型的判断题：候选编号、标题、UP 主、时长、播放量，一段话说完。
fn prompt_for(keyword: &str, pool: &[search::Candidate]) -> String {
    let mut prompt = format!(
        "用户想听：{keyword}\n\
         从下面的搜索结果里挑一条最合适的，只回复编号（1-{}），不要解释。\n\n",
        pool.len()
    );
    for (index, candidate) in pool.iter().enumerate() {
        prompt.push_str(&format!(
            "{}. {} | UP主：{} | 时长：{} | 播放 {}\n",
            index + 1,
            candidate.title,
            candidate.author,
            candidate.duration_label(),
            candidate.play,
        ));
    }
    prompt.push_str(
        "\n要求：请求里写了歌手名就选那位歌手的版本；没写就优先原唱或官方MV，\
         翻唱、翻跳、纯伴奏、串烧、剪辑合集、教学都不要；优先选时长接近一首完整歌曲的。\n",
    );
    prompt
}

/// 从模型的回话里认出编号：取第一个落在范围内的数字，返回它的下标。
fn parse_choice(reply: &str, len: usize) -> Option<usize> {
    let mut digits: Option<String> = None;
    for character in reply.chars() {
        if character.is_ascii_digit() {
            digits.get_or_insert_with(String::new).push(character);
        } else if let Some(number) = digits.take()
            && let Ok(chosen) = number.parse::<usize>()
            && (1..=len).contains(&chosen)
        {
            return Some(chosen - 1);
        }
    }
    digits?
        .parse::<usize>()
        .ok()
        .filter(|chosen| (1..=len).contains(chosen))
        .map(|chosen| chosen - 1)
}

/// Validate control edits against the plugin's actual configuration type.
pub fn validate_config(value: &toml::Value) -> Result<(), String> {
    <Config as serde::Deserialize>::deserialize(value.clone())
        .map(|_| ())
        .map_err(|_| "配置类型不匹配（请检查数组元素、字段类型及整数范围）".to_string())
}
