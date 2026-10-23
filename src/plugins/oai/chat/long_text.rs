//! 长文转图：一条要说出去的长文字，先排成图，再作为一条消息发出去。
//!
//! 目的只有一个：**不刷屏**。几百字贴进群会把别人的话顶走，也会让人一眼看到就划过去；
//! 同样一段话排成一页纸，一条消息就说完了，还能慢慢读。
//!
//! 判断发生在**发出去的那一刻**，不在提示词里靠模型配合：模型照常把话写完，能力层
//! 看到整条消息除了引用之外全是文字、又长过阈值（`ChatConfig::long_text_chars`），
//! 就换成图。于是房间、搭话、兼容文字路径共用同一条规则，谁也不会漏，模型也不必
//! 先学会一件新工具。
//!
//! 版式直接复用 `/md` 指令那一套（[`crate::render::markdown`]）：群友在两种场合看到的
//! 文档长得一样，省掉第二套卡片样式。渲染失败就退回原文——宁可多发一段字，也不能
//! 把话丢掉。

use crate::message::Message;
use crate::render::markdown::{self, Settings};
use crate::render::math;
use crate::render::web::{self as web, Shot};
use simd_json::base::ValueAsScalar;
use std::time::Duration;

const LOG_TARGET: &str = "Plugin/OAI/Chat/LongText";

/// 一次渲染的总预算。长文分几页时要逐页过浏览器；到点还没画完就退回原文，
/// 不能让一句话把这一轮拖住。
const BUDGET: Duration = Duration::from_secs(60);

/// 出图倍率：与 `/md` 的默认值一致。
const SCALE: f64 = 2.0;

/// 排版成图之后，等多久才发。
///
/// 正文是逐字敲出来的，但一张图不是——它是「按住语音键说完再转写」那一类长句，
/// 按正文全长的打字时间等（上限四十秒）只会让这句话错过眼前的话题。按一句长话的
/// 语音时长给几秒，剩下的是渲染本身在花时间。
const CARD_TYPING_CHARS: usize = 48;

/// 这条消息里能排成图的正文：除引用之外全是文字。
///
/// 带图片、文件、@ 的段落各有各的落点，转成图就丢了——那些消息照原样发，由
/// 别的路径管它们的体量。
pub(crate) fn body(message: &Message) -> Option<String> {
    let mut text = String::new();
    for segment in &message.0 {
        match segment.type_.as_str() {
            "reply" => {}
            "text" => text.push_str(
                segment
                    .data
                    .get("text")
                    .and_then(|value| value.as_str())
                    .unwrap_or(""),
            ),
            _ => return None,
        }
    }
    (!text.trim().is_empty()).then_some(text)
}

/// 长到该排成图的正文；`limit` 为 0 表示关掉。
///
/// 一行字说不清的公式（矩阵、多行推导、独立成行的式子，见
/// [`math::is_heavy`]）不论长短都排成图：换成文字只剩一串斜杠或丢了结构。
pub(crate) fn source(message: &Message, limit: usize) -> Option<String> {
    if limit == 0 {
        return None;
    }
    let text = body(message)?;
    (text.chars().count() >= limit || math::is_heavy(&text)).then_some(text)
}

/// 照文字发出去之前，把里面的公式换成能直接读的写法（`$x^2$` → `x²`）。
pub(crate) fn readable(mut message: Message) -> Message {
    for segment in &mut message.0 {
        if segment.type_ != "text" {
            continue;
        }
        let Some(text) = segment.data.get("text").and_then(|value| value.as_str()) else {
            continue;
        };
        if math::has_math(text) {
            let plain = math::plain(text);
            segment.data.insert("text".into(), plain.into());
        }
    }
    message
}

/// 排版一张长文卡要等多久。调用方据此估算「打字」的时长。
pub(crate) fn typing_chars() -> usize {
    CARD_TYPING_CHARS
}

/// 把这条消息排成图：引用照留，文字换成整页图片。不适合、或渲染失败时返回 `None`，
/// 调用方按原来的文字发。
pub(crate) async fn as_card(
    message: &Message,
    text: &str,
    browser: Option<&str>,
) -> Option<Message> {
    let mut card = Message::new();
    for segment in &message.0 {
        if segment.type_ == "reply" {
            card.0.push(segment.clone());
        }
    }
    match render(text, browser).await {
        Ok((images, total)) => {
            if images.is_empty() {
                return None;
            }
            for image in &images {
                card = card.image(format!("base64://{image}"));
            }
            // 与 `/md` 同一个口径：页数有上限，超出的说清楚，别让人以为只有这些。
            if total > images.len() {
                card = card.text(format!(
                    "内容较长，共 {total} 页，只画了前 {} 页",
                    images.len()
                ));
            }
            Some(card)
        }
        Err(error) => {
            warn!(target: LOG_TARGET, "长文排版失败，按原文发：{error:#}");
            None
        }
    }
}

/// 逐页出图，返回各页的 base64 与总页数。
async fn render(text: &str, browser: Option<&str>) -> anyhow::Result<(Vec<String>, usize)> {
    // 版式与 `[markdown]` 的默认值一致：能力层不读插件配置，群聊里的长文卡与
    // `/md` 默认长一个样，但不随那边的改动漂移。
    let settings = Settings::default();
    tokio::time::timeout(BUDGET, async {
        let rendered = markdown::render(text, &settings);
        let mut images = Vec::with_capacity(rendered.pages.len());
        for html in &rendered.pages {
            images.push(
                web::shoot(
                    Shot::new(html, settings.width + 40)
                        .scale(SCALE)
                        .max_height(settings.page_height * 8.0)
                        .browser(browser),
                )
                .await?,
            );
        }
        Ok::<_, anyhow::Error>((images, rendered.total_pages))
    })
    .await
    .map_err(|_| anyhow::anyhow!("排版超时（{} 秒）", BUDGET.as_secs()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_text_and_quotes_become_a_card() {
        let quote = Message::new().reply("42").text("字".repeat(300));
        assert!(source(&quote, 240).is_some());
        // 关掉时一条也不转；没到阈值时的短句照旧按文字发。
        assert!(source(&quote, 0).is_none());
        assert!(source(&quote, 400).is_none());
        // @ 与图各有各的落点：转成图就把人弄丢了。
        assert!(source(&Message::new().at("10001").text("字".repeat(300)), 240).is_none());
        assert!(source(&Message::new().image("x.png"), 240).is_none());
        // 只有引用没有正文，不算一条可发的消息。
        assert!(source(&Message::new().reply("42"), 1).is_none());
        // 矩阵这类一行字说不清的公式，短也排成图；简单的行内式照旧发文字。
        assert!(source(&Message::new().text("$$\\begin{pmatrix}1&0\\\\0&1\\end{pmatrix}$$"), 240).is_some());
        assert!(source(&Message::new().text("答案是 $x^2$"), 240).is_none());
        let plain = readable(Message::new().reply("42").text("答案是 $x^2$"));
        assert_eq!(super::super::plain_text(&plain).trim(), "答案是 x²");
    }
}
