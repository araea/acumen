//! 群聊能力层：在一个真实的 QQ 群里看现场、查资料、动手。
//!
//! 这一层不属于任何一个人格。内置智能体房间与群聊搭话共用它，区别只在调用方给的
//! [`ChatConfig`] 与有没有 [`Persona`]：
//!
//! - **房间**（`engine = agent`）拿到的是一套完整的工具集——本机文件、联网，加上这里
//!   的 `satori_*`；用户问一句它答一句。
//! - **搭话**（`ambient`）只挂其中一部分，另外带着自己的人格、状态与判定节奏。
//!
//! 分工写在两边各自的注释里，一条分界线是：**能力层不读任何插件配置**。这一轮在哪个
//! 群、允许做什么、额度多少、打字多快，全部由调用方算好塞进 [`ChatConfig`]；人格的
//! 口吻与状态通过 [`Persona`] 问一句。于是同一份实现既服务「用户直接找它」的场景，
//! 也服务「它自己插话」的场景，而两条路上的行为差异全在调用方那一侧看得见。
//!
//! 子模块的分工：
//!
//! - [`window`]：群聊现场——一条消息长什么样、按群滚动的窗口、递给模型读的记录；
//! - [`session`]：一轮对话的工具出口（`satori_*` 的十个 op）与动作执行；
//! - [`actions`]：`satori_action` 能做的事的解析与校验；
//! - [`memory`] 长期记忆、[`stickers`] 表情包库、[`identity`] 我在这个群里是谁；
//! - [`pace`] / [`breath`] / [`tone`]：怎么说出来——换气、断句、别复读；
//! - [`attention`]：人格那侧的关注对象，[`vision`]：把群里的图转成模型收得下的图。

pub(crate) mod actions;
pub(crate) mod attention;
pub(crate) mod breath;
pub(crate) mod identity;
pub(crate) mod memory;
pub(crate) mod pace;
pub(crate) mod session;
pub(crate) mod stickers;
pub(crate) mod tone;
pub(crate) mod vision;
pub(crate) mod window;

pub(crate) use session::{LOOKUP_KINDS, PROFILE_KINDS, start};

use crate::event::Context;
use simd_json::base::ValueAsScalar;
use crate::message::Message;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 能力层的日志 target。房间与搭话共用同一份实现，日志也就同一个名字。
pub(crate) const LOG_TARGET: &str = "Plugin/Chat";

/// 一轮行动的额度与开关。
///
/// 这里只有「这一轮允许做什么」；在哪个群、用什么接口、说话的是谁，都在 [`ChatEnv`]
/// 里。调用方算好后传进来，能力层不再回头看任何插件配置。
#[derive(Debug, Clone)]
pub(crate) struct ChatConfig {
    /// 这一层在当下这个群开着吗。房间总是开着；搭话看自己的开关与群列表。
    pub enabled: bool,
    /// 允许执行群管理动作：踢人、禁言、全员禁言、改群名、设精华、改他人名片、
    /// 群文件的改名/移动/删除。这些是会被全群看见的写操作，默认要单独授权。
    pub management: bool,
    /// 要求「群聊没有往前走」才允许动手。
    ///
    /// 搭话的回复只对刚才那一批消息负责，窗口一动就该重看；房间回答的是一句直接
    /// 请求，中间群里聊了什么与这次回答无关。
    pub require_fresh: bool,
    /// 一次发言最多几条（模型把一段话写长了，切开也算额度）。
    pub max_messages: usize,
    /// 一轮最多几次写动作。
    pub max_actions: usize,
    /// 一轮最多写几条记忆。
    pub memo_budget: usize,
    /// 一轮最多查几次资料（群资料、个人资料、旧消息共用这一份）。
    pub lookup_budget: usize,
    pub draw_budget: usize,
    pub music_budget: usize,
    pub video_budget: usize,
    /// 长期记忆库开着吗。
    pub memory_enabled: bool,
    /// 表情包库上限；0 表示不攒。
    pub sticker_max: usize,
    /// 递给模型的群聊条数。
    pub context_turns: usize,
    /// 一段话按换气切成几条的字数阈值；0 表示不切。
    pub split_chars: usize,
    /// 发出前的时效窗口：群里又有人说话就整条不发。
    pub freshness: std::time::Duration,
    /// 一次媒体生成（图/歌/片）最多等多久。
    pub media_deadline: std::time::Duration,
}

impl Default for ChatConfig {
    /// 房间那一侧的内置默认：全都开着，额度取保守的一档。
    fn default() -> Self {
        Self {
            enabled: true,
            management: false,
            require_fresh: false,
            max_messages: 3,
            max_actions: 6,
            memo_budget: 3,
            lookup_budget: 4,
            draw_budget: 2,
            music_budget: 1,
            // 拍一段片子一次一块多，比写歌贵一倍；想要就自己在 [oai.chat] 里开。
            video_budget: 0,
            memory_enabled: true,
            sticker_max: 120,
            context_turns: 20,
            split_chars: 60,
            freshness: std::time::Duration::ZERO,
            media_deadline: std::time::Duration::from_secs(240),
        }
    }
}

/// 一次行动的环境：在哪个群、用哪几个目录、有没有人格。
pub(crate) struct ChatEnv<'a> {
    pub ctx: &'a Context,
    pub writer: &'a crate::adapters::satori::LockedWriter,
    /// 群号。能力层只在群里工作——私聊没有群资料、没有群动作，也就没有这一层。
    pub group: i64,
    pub config: ChatConfig,
    /// 本轮独占的工作目录：工具写文件、生成图片与视频都落在这儿。
    pub scratch: &'a Path,
    /// 本轮生成物落盘的位置，供随后用 `satori_action` 发出去。
    pub media: &'a Path,
    /// 人格那一层。房间没有，`None`。
    pub persona: Option<Arc<dyn Persona>>,
    /// 这一轮的群聊现场从哪儿来：搭话读自己的常驻窗口，房间向平台要。
    pub scene: session::Scene,
}

/// 人格那一层：能力层不碰人格，只在两三处问一句。
///
/// 房间没有这一层，于是 `satori_context` 里少了口吻与状态、打字按默认节奏、说出去
/// 一句话也不必通知谁。
pub(crate) trait Persona: Send + Sync {
    /// 现场里属于人格的那一份，塞进 `satori_context` 的回执给模型看。
    ///
    /// 三个键：`register`（这个群此刻怎么说话）、`state`（自己什么精神头）、
    /// `remember`（记得的人和旧事）。没有的项给空串。
    fn scene(&self, group: i64, turns: &[window::Turn], rhythm: &str) -> serde_json::Value;

    /// 这一轮真的说出去了一句。搭话用它更新状态曲线；房间不需要。
    fn spoke(&self, _group: i64) {}

    /// 打字与思考的节奏。
    fn pace(&self, _group: i64) -> pace::Pace {
        pace::Pace::default()
    }

    /// 看一眼自己的头像时用的接口。取不到就不看，提示词里少一行而已。
    fn avatar(&self) -> Option<Avatar> {
        None
    }
}

/// 给模型看头像用的接口与模型。
#[derive(Clone)]
pub(crate) struct Avatar {
    pub api_base: String,
    pub api_key: String,
    pub model: String,
}

/// 能力层的数据根：记忆、表情包库、群身份缓存。
///
/// 放在内置智能体插件的数据目录下（`data/oai/chat/`）——它属于这层能力，
/// 不属于任何一个人格。搭话也读同一份，于是房间里记住的人与梗，人格同样认得。
pub(crate) fn data_root(base: &Path) -> PathBuf {
    base.join("chat")
}

/// 铺开能力层的数据目录并挂上各处存储。启动时调用一次，重复调用是安全的。
pub(crate) async fn attach(base: &Path) -> std::io::Result<()> {
    let root = data_root(base);
    tokio::fs::create_dir_all(root.join("memory")).await?;
    tokio::fs::create_dir_all(root.join("stickers")).await?;
    memory::attach(&root);
    stickers::attach(&root);
    identity::attach(&root);
    Ok(())
}

/// 当前本机日期、星期与时刻，让模型感知「现在几点」。
///
/// 群聊语境里早晚、工作日与周末是有信息量的——模型拿不到真实时钟，只能靠文本。
pub(crate) fn now_context() -> String {
    use chrono::Datelike as _;
    let now = chrono::Local::now();
    let weekday = match now.weekday() {
        chrono::Weekday::Mon => "周一",
        chrono::Weekday::Tue => "周二",
        chrono::Weekday::Wed => "周三",
        chrono::Weekday::Thu => "周四",
        chrono::Weekday::Fri => "周五",
        chrono::Weekday::Sat => "周六",
        chrono::Weekday::Sun => "周日",
    };
    format!(
        "现在：{} {} {}（本机时间）",
        now.format("%Y-%m-%d"),
        weekday,
        now.format("%H:%M")
    )
}

/// 模型偶尔把换行写成字面的 `\n`（人设与 skill 的示例里就是这么写的），
/// 原样发出去，群里看到的是一个反斜杠加一个 n。这里把它还原成真换行。
///
/// 只管这一个转义。代价是 `C:\new` 这种路径会被掰成两行，群里没人真打一个
/// 反斜杠加 n，两害相权取轻。
pub(crate) fn literal_newlines(text: &str) -> String {
    if !text.contains("\\n") {
        return text.to_string();
    }
    text.replace("\\r\\n", "\n").replace("\\n", "\n")
}

/// 从字节认图片的扩展名。
///
/// 生成图与偷来的表情包都按这个给落盘的文件起名：存下来的这份要跟内容对得上，
/// 上传给 QQ 与本地回看都靠它。
pub(crate) fn image_extension(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "gif"
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "webp"
    } else if bytes.starts_with(b"BM") {
        "bmp"
    } else {
        "png"
    }
}

/// 消息链 → 记进窗口的文字形态。
pub(crate) fn plain_text(message: &Message) -> String {
    let mut out = String::new();
    for segment in &message.0 {
        match segment.type_.as_str() {
            "text" => out.push_str(
                segment
                    .data
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            ),
            // 与入站记录、发言标记同一个写法：自己过去那句里的 @ 也读成 `[at:QQ号]`，
            // 人格在自己说过的话里学到的就是能用的那一种。
            "at" => out.push_str(&format!(
                "[at:{}] ",
                segment
                    .data
                    .get("qq")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
            )),
            "face" => out.push_str("[表情]"),
            "poke" => out.push_str("[戳一戳]"),
            "dice" => out.push_str("[骰子]"),
            "rps" => out.push_str("[猜拳]"),
            "image" | "mface" => out.push_str("[图片/表情包]"),
            "file" => out.push_str("[文件]"),
            "record" => out.push_str("[语音]"),
            "video" => out.push_str("[视频]"),
            "node" | "forward" => out.push_str("[合并转发]"),
            _ => {}
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 模型把换行写成字面的 `\n` 时，群里不该看见一个反斜杠加一个 n。
    #[test]
    fn literal_backslash_n_becomes_a_real_newline() {
        assert_eq!(
            literal_newlines("先看第一步\\n1. 关掉自动更新"),
            "先看第一步\n1. 关掉自动更新"
        );
        assert_eq!(literal_newlines("a\\r\\nb"), "a\nb");
        // 没有转义的正文不动，包括普通的反斜杠。
        assert_eq!(literal_newlines("就这?没了"), "就这?没了");
        assert_eq!(literal_newlines("路径 C:\\Users"), "路径 C:\\Users");
    }
}
