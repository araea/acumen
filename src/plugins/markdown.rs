//! Markdown 转图：指令后直接写 Markdown，或引用一条消息再发指令，回一张（长文分几页）
//! 排版好的图。
//!
//! 排版与分页都在 [`crate::render::markdown`]，这里只管三件事：从消息里取出文本、
//! 交给渲染、把图发回去。`oai` 的回复卡走同一套渲染，不另写一份。
//!
//! 不发「正在渲染」：结果本身就是回应（发出去时引用请求那条）；
//! 只有显式指令用错了才说话，并给出下一步。

use crate::adapters::satori::{LockedWriter, api, send_msg};
use crate::command::{extract_text_arg, match_word_command};
use crate::event::Context;
use crate::message::Message;
use crate::plugins::{PluginConfig, PluginError, get_config_or_default};
use crate::render::markdown::{self, Settings};
use crate::render::web::{self as web, Shot};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use simd_json::base::ValueAsScalar;
use std::time::Duration;

const LOG_TARGET: &str = "Plugin/Markdown";

/// 指令词。`md` 最短，`渲染` 给不想切英文输入法的人。
const COMMANDS: [&str; 3] = ["md", "markdown", "渲染"];

/// 一次请求的总预算：分页渲染是逐页排队进浏览器的，页多时比单张慢。
const TOTAL_BUDGET: Duration = Duration::from_secs(150);

// ================= 配置定义 =================

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    enabled: bool,
    /// 配色：light 或 dark。
    theme: String,
    /// 卡面宽度（CSS 像素）。宽一点每行才放得下更多字；图要在手机上全屏看，
    /// 字与卡宽的比例才是决定「一屏能读多少」的那一个。
    width: u32,
    /// 正文字号（CSS 像素）。整套字阶跟着它缩放，间距不缩：字号相对卡宽越小，
    /// 每行塞得下的字越多、扫读越快。
    font_size: u32,
    /// 出图倍率（1—4）。
    image_scale: f64,
    /// 每页的目标高度（CSS 像素）；块不拆开时可以略超。
    page_height: u32,
    /// 最多发几页，超出的不发并说明。
    max_pages: usize,
    /// 单次最多渲染多少字，防止一条消息刷出一屏图。
    max_chars: usize,
    /// 单个换行按换行显示（聊天里的写法）。关掉则按 CommonMark 把相邻行并成一段。
    keep_line_breaks: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            theme: "light".into(),
            width: 560,
            font_size: 18,
            image_scale: 2.0,
            page_height: 2000,
            max_pages: 6,
            max_chars: 20_000,
            keep_line_breaks: true,
        }
    }
}

impl PluginConfig for Config {
    const NAME: &'static str = "markdown";

    fn check(&self) -> Result<(), String> {
        if !matches!(self.theme.as_str(), "light" | "dark") {
            return Err("theme 只能是 light 或 dark".into());
        }
        if !(320..=1200).contains(&self.width) {
            return Err("width 需在 320—1200 之间".into());
        }
        if !(14..=26).contains(&self.font_size) {
            return Err("font_size 需在 14—26 之间".into());
        }
        if !(1.0..=4.0).contains(&self.image_scale) {
            return Err("image_scale 需在 1—4 之间".into());
        }
        if !(600..=8000).contains(&self.page_height) {
            return Err("page_height 需在 600—8000 之间".into());
        }
        if !(1..=10).contains(&self.max_pages) {
            return Err("max_pages 需在 1—10 之间".into());
        }
        if !(100..=100_000).contains(&self.max_chars) {
            return Err("max_chars 需在 100—100000 之间".into());
        }
        Ok(())
    }
}


// ================= 取文本 =================

/// 把消息段里的文字按顺序拼起来；`at` 段的名字平台已写进相邻文本，这里不再补。
fn text_of_segments<'a>(segments: impl Iterator<Item = (&'a str, Option<&'a str>)>) -> String {
    let mut text = String::new();
    for (kind, value) in segments {
        if kind == "text"
            && let Some(value) = value
        {
            text.push_str(value);
        }
    }
    text
}

const USAGE: &str =
    "把 Markdown 渲染成图：\n· /md 后面直接写内容，可以多行\n· 或者引用一条消息，只发 /md";

// ================= 插件入口 =================

pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let Some(msg) = ctx.as_message() else {
            return Ok(Some(ctx));
        };
        let Some(matched) = COMMANDS
            .iter()
            .find_map(|cmd| match_word_command(&ctx, cmd))
        else {
            return Ok(Some(ctx));
        };
        let config: Config = get_config_or_default(&ctx);

        let base = || Message::new().reply(msg.message_id());

        // 1. 取文本：指令后面写的优先，没有再看引用的那条。
        let mut source = extract_text_arg(&matched.args);
        if source.trim().is_empty()
            && let Some(reply_id) = &matched.reply_id
        {
            match api::get_msg(&ctx, writer.clone(), reply_id).await {
                Ok(quoted) => {
                    source = text_of_segments(quoted.message.0.iter().map(|seg| {
                        (
                            seg.type_.as_str(),
                            seg.data.get("text").and_then(|v| v.as_str()),
                        )
                    }));
                }
                Err(e) => {
                    warn!(target: LOG_TARGET, "读取被引用的消息失败: {}", e);
                    send_msg(
                        &ctx,
                        writer,
                        msg.group_id(),
                        Some(msg.user_id()),
                        base().text("❌ 没能读到被引用的消息，请直接把内容写在指令后面"),
                    )
                    .await?;
                    return Ok(None);
                }
            }
        }
        if source.trim().is_empty() {
            send_msg(
                &ctx,
                writer,
                msg.group_id(),
                Some(msg.user_id()),
                base().text(if matched.reply_id.is_some() {
                    "❌ 被引用的消息里没有文字可渲染（图片、表情不算）".to_string()
                } else {
                    USAGE.to_string()
                }),
            )
            .await?;
            return Ok(None);
        }
        let chars = source.chars().count();
        if chars > config.max_chars {
            send_msg(
                &ctx,
                writer,
                msg.group_id(),
                Some(msg.user_id()),
                base().text(format!(
                    "❌ 内容有 {chars} 字，超过单次上限 {}，请分几次发送",
                    config.max_chars
                )),
            )
            .await?;
            return Ok(None);
        }

        // 2. 渲染。
        let settings = Settings {
            width: config.width,
            font_size: config.font_size as f64,
            dark: config.theme == "dark",
            keep_breaks: config.keep_line_breaks,
            page_height: config.page_height as f64,
            max_pages: config.max_pages,
        };
        let browser_path = ctx.config.read().unwrap().browser_path.clone();
        let outcome = tokio::time::timeout(
            TOTAL_BUDGET,
            render_images(
                &source,
                &settings,
                config.image_scale,
                browser_path.as_deref(),
            ),
        )
        .await;
        let (images, total_pages) = match outcome {
            Ok(Ok(done)) => done,
            Ok(Err(e)) => {
                error!(target: LOG_TARGET, "渲染失败: {}", e);
                send_msg(
                    &ctx,
                    writer,
                    msg.group_id(),
                    Some(msg.user_id()),
                    base().text(format!("❌ 渲染失败：{e}")),
                )
                .await?;
                return Ok(None);
            }
            Err(_) => {
                error!(target: LOG_TARGET, "渲染超时（{} 秒）", TOTAL_BUDGET.as_secs());
                send_msg(
                    &ctx,
                    writer,
                    msg.group_id(),
                    Some(msg.user_id()),
                    base().text("❌ 渲染超时，内容太长时请分几次发送"),
                )
                .await?;
                return Ok(None);
            }
        };
        if images.is_empty() {
            send_msg(
                &ctx,
                writer,
                msg.group_id(),
                Some(msg.user_id()),
                base().text("❌ 没有可渲染的内容"),
            )
            .await?;
            return Ok(None);
        }

        // 3. 发送：所有页在同一条消息里，顺序不会被别的消息插进来。
        let mut body = base();
        for image in &images {
            body = body.image(format!("base64://{image}"));
        }
        if total_pages > images.len() {
            body = body.text(format!(
                "内容较长，共 {total_pages} 页，只渲染了前 {} 页",
                images.len()
            ));
        }
        send_msg(&ctx, writer, msg.group_id(), Some(msg.user_id()), body).await?;
        Ok(None)
    })
}

/// 渲染全部页，返回各页的 base64 与总页数。
async fn render_images(
    source: &str,
    settings: &Settings,
    scale: f64,
    browser_path: Option<&str>,
) -> anyhow::Result<(Vec<String>, usize)> {
    let rendered = markdown::render(source, settings);
    let mut images = Vec::with_capacity(rendered.pages.len());
    for html in &rendered.pages {
        let image = web::shoot(
            Shot::new(html, settings.width + 40)
                .scale(scale)
                .max_height(settings.page_height * 8.0)
                .browser(browser_path),
        )
        .await?;
        images.push(image);
    }
    Ok((images, rendered.total_pages))
}
