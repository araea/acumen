//! 多模态大模型复核：本地模型觉得像的图，交给 oai 里配好的多模态模型（默认 DeepSeek V4.1 Flash）确认。
//!
//! 本地模型快、离线、不花钱，但对没见过的图会误判（群里的截图、幻灯片能打到 0.99）；大模型认得准
//! （2026-10 实测 50 张皮卡丘、小火龙、小黄人、黄色表情包一张没认错），可每张要两三秒、要花 token。
//! 所以分工：本地分数很低的直接放，觉得像的才问大模型（默认一律问，`sure` 可设成本地够高就直接拦）。
//! 问过的图按内容摘要记下结论，同一张图不问第二次；
//! 每天问多少张有上限；用完了、接口出错或超时，这张先放过（本地单独认错的太多，宁漏勿错）。

use super::LOG_TARGET;
use super::model::Kind;
use crate::event::Context;
use crate::plugins::oai::{self, llm};
use crate::storage::JsonState;
use base64::{Engine, engine::general_purpose::STANDARD};
use rig_core::completion::Message;
use rig_core::completion::message::{DocumentSourceKind, Image, Text, UserContent};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// 大模型的结论。
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Opinion {
    pub kind: Kind,
    pub confidence: f32,
}

const PROMPT: &str = r#"判断这张图里有没有「奶龙」或「奶蛙」，只回一行 JSON：{"kind":"nailong|naiwa|other","confidence":0到1}

- 奶龙：国产 3D 动画里的黄色圆胖小恐龙，白肚皮、大圆眼、短手短脚，憨萌卡通风。它的表情包、截图、玩偶、手办、贴纸、蛋糕、周边、手绘二创都算。
- 奶蛙：2026 年流行的 AI 生成「变异奶龙」，又叫大笑奶龙：写实皮肤质感的黄色胖人形生物，脸像青蛙、小眼睛长在头顶两侧，常捧腹大笑，常被 P 进各种照片、名画、场景里。
- other：其余一切，包括皮卡丘、小火龙、小黄人、小黄鸭、海绵宝宝、黄色的其他卡通、穿黄衣服的人或动物。
主体之外只是角落里很小、看不清的，算 other。"#;

/// 同一时间最多几张图在问，免得刷屏时一下子打出几十个请求。
static ASKING: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(3);

// ==================== 用量与结论缓存 ====================

#[derive(Serialize, Deserialize, Clone, Default)]
struct Usage {
    /// 北京时间日期（`2026-10-21`）。
    day: String,
    count: u32,
}

static USAGE: JsonState<Usage> = JsonState::new(LOG_TARGET, "nailong", "ai_usage.json", "奶龙复核用量");

fn today() -> String {
    crate::clock::beijing_now().format("%Y-%m-%d").to_string()
}

/// 今天已经问了几张。
pub async fn used_today() -> u32 {
    let day = today();
    USAGE.with(|u| if u.day == day { u.count } else { 0 }).await
}

/// 占一个名额；今天用完了返回 `false`。
async fn take_quota(limit: u32) -> bool {
    let day = today();
    USAGE
        .with(|u| {
            if u.day != day {
                *u = Usage { day: day.clone(), count: 0 };
            }
            if u.count >= limit {
                return false;
            }
            u.count += 1;
            true
        })
        .await
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct Remembered {
    /// (内容摘要, 结论)，先来后到，满了丢最早的。
    items: Vec<(String, Opinion)>,
}

static REMEMBERED: JsonState<Remembered> = JsonState::new(LOG_TARGET, "nailong", "ai_verdicts.json", "奶龙复核结论");
const MAX_REMEMBERED: usize = 5000;

pub async fn recall(digest: &str) -> Option<Opinion> {
    REMEMBERED
        .with(|r| r.items.iter().rev().find(|(d, _)| d == digest).map(|(_, o)| *o))
        .await
}

async fn remember(digest: &str, opinion: Opinion) {
    REMEMBERED
        .with(|r| {
            r.items.retain(|(d, _)| d != digest);
            r.items.push((digest.to_string(), opinion));
            let overflow = r.items.len().saturating_sub(MAX_REMEMBERED);
            r.items.drain(..overflow);
        })
        .await;
}

// ==================== 提问 ====================

/// 复核用的模型现在能不能用：`供应商/模型` 在 oai 里解析得到接口与密钥。
pub async fn available(ctx: &Context, model: &str) -> bool {
    endpoint(ctx, model).await.is_some()
}

async fn endpoint(ctx: &Context, model: &str) -> Option<(String, String, String)> {
    oai::model_endpoint(ctx, model)
        .await
        .filter(|(base, key, id)| !base.trim().is_empty() && !key.trim().is_empty() && !id.trim().is_empty())
}

/// 问一张图（PNG）。`digest` 是原图的内容摘要：问过就直接用上次的；`limit` 是每天的名额。
/// 没配模型、名额用完、出错、超时都返回 `Err`，调用方退回本地判断。
pub async fn ask(ctx: &Context, model: &str, png: &[u8], digest: &str, limit: u32, timeout: Duration) -> Result<Opinion, String> {
    if let Some(opinion) = recall(digest).await {
        return Ok(opinion);
    }
    let (base, key, model_id) = endpoint(ctx, model).await.ok_or("没配复核模型（oai 里没有这个供应商或没填密钥）")?;
    let _permit = ASKING.acquire().await.map_err(|e| e.to_string())?;
    if !take_quota(limit).await {
        return Err(format!("今天的复核名额（{limit} 张）用完了"));
    }
    let messages = vec![Message::User {
        content: vec![
            UserContent::Text(Text::new(PROMPT)),
            UserContent::Image(Image {
                data: DocumentSourceKind::Url(format!("data:image/png;base64,{}", STANDARD.encode(png))),
                media_type: None,
                detail: None,
                additional_params: None,
            }),
        ],
    }];
    let reply = tokio::time::timeout(timeout, llm::complete(&base, &key, &model_id, messages, Some("low")))
        .await
        .map_err(|_| "复核超时".to_string())?
        .map_err(|e| format!("复核失败：{e}"))?;
    let opinion = parse(&reply).ok_or_else(|| format!("复核回复看不懂：{}", reply.chars().take(80).collect::<String>()))?;
    remember(digest, opinion).await;
    Ok(opinion)
}

/// 从回复里取 `{"kind": …, "confidence": …}`；模型偶尔会在 JSON 前后多说几句。
fn parse(reply: &str) -> Option<Opinion> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    let value: serde_json::Value = serde_json::from_str(reply.get(start..=end)?).ok()?;
    let kind = match value.get("kind")?.as_str()?.trim().to_ascii_lowercase().as_str() {
        "nailong" | "奶龙" => Kind::Nailong,
        "naiwa" | "奶蛙" => Kind::Naiwa,
        "other" | "其他" | "none" => Kind::Other,
        _ => return None,
    };
    let confidence = value.get("confidence").and_then(serde_json::Value::as_f64).unwrap_or(0.8) as f32;
    Some(Opinion { kind, confidence: confidence.clamp(0.0, 1.0) })
}
