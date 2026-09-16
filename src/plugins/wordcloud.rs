use crate::adapters::satori::{LockedWriter, send_msg};
use crate::command::strip_prefix;
use crate::db::queries::get_text_corpus;
use crate::db::utils::get_time_range;
use crate::event::Context;
use crate::message::Message;
use crate::plugins::{PluginError, get_config_or_default};
use futures_util::future::BoxFuture;
use regex::Regex;
use std::sync::OnceLock;

const LOG_TARGET: &str = "Plugin/WordCloud";

pub mod config;
pub mod image;
pub mod stopwords;

use config::WordCloudConfig;
pub use config::default_config;

static COMMAND_REGEX: OnceLock<Regex> = OnceLock::new();

fn get_regex() -> &'static Regex {
    COMMAND_REGEX.get_or_init(|| {
        Regex::new(
            r"^(本群|跨群|我的)(今日|昨日|本周|上周|近7天|近30天|本月|上月|今年|去年|总)词云$",
        )
        .unwrap()
    })
}

pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let msg = match ctx.as_message() {
            Some(m) => m,
            None => return Ok(Some(ctx)),
        };
        let text = msg.text();

        let content_to_match = match strip_prefix(&ctx, text) {
            Some(c) => c,
            None => return Ok(Some(ctx)),
        };

        let regex = get_regex();
        if let Some(caps) = regex.captures(content_to_match) {
            let scope_str = caps.get(1).map_or("", |m| m.as_str());
            let time_str = caps.get(2).map_or("", |m| m.as_str());

            info!(target: LOG_TARGET, "收到词云请求: Scope={}, Time={}", scope_str, time_str);

            let (start_time, end_time) = get_time_range(time_str);

            let (query_group_id, query_user_id) = match scope_str {
                "本群" => {
                    if let Some(gid) = msg.group_id() {
                        (Some(gid), None)
                    } else {
                        (None, Some(msg.user_id()))
                    }
                }
                "跨群" => (None, None),
                "我的" => (None, Some(msg.user_id())),
                _ => (None, None),
            };

            if scope_str == "本群" && query_group_id.is_none() && msg.group_id().is_none() {
                let reply =
                    Message::new().text("❌ 这个范围只在群里有效\n用「本群」查群里的词云，或用「我的」查个人的");
                send_msg(&ctx, writer, msg.group_id(), Some(msg.user_id()), reply).await?;
                return Ok(None);
            }
            let title = format!("{} 的 {} 词云", scope_str, time_str);
            let reply_id = msg.message_id();
            let target_group = msg.group_id();
            let target_user = Some(msg.user_id());

            // 发送提示
            send_msg(
                &ctx,
                writer.clone(),
                target_group,
                target_user,
                Message::new()
                    .reply(reply_id)
                    .text(format!("⏳ 正在生成 {}…", title)),
            )
            .await?;

            // 生成并发送
            match generate_image(&ctx, query_group_id, query_user_id, start_time, end_time).await {
                Ok(b64) => {
                    let img_msg = Message::new().image(b64);
                    send_msg(&ctx, writer, target_group, target_user, img_msg).await?;
                }
                Err(GenError::Empty) => {
                    // 空态不是错误：说清为什么空，再给一条能立刻做的事。
                    let empty = Message::new()
                        .reply(reply_id)
                        .text("📭 这段时间没有聊天记录\n换一个时间范围，或先让群里聊几句");
                    send_msg(&ctx, writer, target_group, target_user, empty).await?;
                }
                Err(GenError::Failed(e)) => {
                    let err_msg = Message::new().text(format!("❌ 生成失败：{}", e));
                    send_msg(&ctx, writer, target_group, target_user, err_msg).await?;
                    error!(target: LOG_TARGET, "Handler error: {}", e);
                }
            }

            return Ok(None);
        }

        Ok(Some(ctx))
    })
}

/// 词云生不成的原因。分成两类是因为对用户来说这是两件事：
/// 「这段时间没聊」是空态（📭，不是谁的错），「读取失败」才是故障（❌）。
#[derive(Debug)]
pub enum GenError {
    /// 区间里没有可用的聊天记录。
    Empty,
    /// 真的失败了，附带原因。
    Failed(String),
}

impl std::fmt::Display for GenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "区间内没有可用的聊天记录"),
            Self::Failed(reason) => write!(f, "{reason}"),
        }
    }
}

/// 核心生成逻辑供外部调用 (例如综合日报插件)
pub async fn generate_image(
    ctx: &Context,
    query_group_id: Option<i64>,
    query_user_id: Option<i64>,
    start_time: i64,
    end_time: i64,
) -> Result<String, GenError> {
    let config: WordCloudConfig = get_config_or_default(ctx, "wordcloud");

    if !config.enabled {
        return Err(GenError::Failed("词云插件已停用".to_string()));
    }

    let db = &ctx.db;
    let mut corpus = get_text_corpus(db, query_group_id, query_user_id, start_time, end_time)
        .await
        .map_err(|e| GenError::Failed(format!("读取聊天记录失败：{}", e)))?;

    if corpus.is_empty() {
        return Err(GenError::Empty);
    }

    // 截断过多消息
    if config.max_msg > 0 && corpus.len() > config.max_msg {
        let start = corpus.len().saturating_sub(config.max_msg);
        corpus = corpus.split_off(start);
    }

    let font_path = config.font_path.clone();
    let font_family = config.font_family.clone();
    let limit = config.limit;
    let width = config.width;
    let height = config.height;

    // 在阻塞线程中生成图片
    let task_result = crate::render::worker::run(move || {
        image::generate_word_cloud(corpus, font_path, font_family, limit, width, height)
    })
    .await;

    match task_result {
        Ok(res) => res.map_err(GenError::Failed),
        Err(e) => Err(GenError::Failed(format!("任务中断：{}", e))),
    }
}

pub fn validate_config(value: &toml::Value) -> Result<(), String> {
    <WordCloudConfig as serde::Deserialize>::deserialize(value.clone())
        .map(|_| ())
        .map_err(|_| "词云配置类型错误".to_string())
}
