//! 聊天能力层：在一个真实的 QQ 会话里看现场、查资料、动手。
//!
//! 会话有两种——群聊与私聊，共同点是都有一个 channel id、都能发消息读消息；
//! 群多出来的是群资料、群成员与群管理（见 [`Place`]）。
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
//! - [`history`]：翻本机记录库——窗口之外的旧话，按人、按词、按时间段；
//! - [`trigger`]：房间这一轮是谁在问、引用了谁、@了谁；
//! - [`actions`]：`satori_action` 能做的事的解析与校验；
//! - [`moderation`]：管理动作的记账——做过什么、每天还剩几次；
//! - [`memory`] 长期记忆、[`stickers`] 表情包库、[`identity`] 我在这个群里是谁；
//! - [`pace`] / [`breath`] / [`tone`]：怎么说出来——换气、断句、别复读；
//! - [`protocol`]：出站文字里的协议清洗——模型把 `satori_action` 当正文写出来时摘掉它；
//! - [`attention`]：人格那侧的关注对象，[`vision`]：把群里的图转成模型收得下的图。

pub(crate) mod actions;
pub(crate) mod attention;
pub(crate) mod breath;
pub(crate) mod history;
pub(crate) mod identity;
pub(crate) mod long_text;
pub(crate) mod memory;
pub(crate) mod moderation;
pub(crate) mod pace;
pub(crate) mod protocol;
pub(crate) mod session;
pub(crate) mod stickers;
pub(crate) mod tone;
pub(crate) mod trigger;
pub(crate) mod vision;
pub(crate) mod window;

pub(crate) use session::start;

use crate::event::Context;
use crate::message::Message;
use simd_json::base::ValueAsScalar;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 能力层的日志 target。
///
/// 两层共用同一份实现，日志也就只有一个名字：这一层住在 `oai` 里，
/// 按注册名加一级子模块。
pub(crate) const LOG_TARGET: &str = "Plugin/OAI/Chat";

/// 一轮行动的额度与开关。
///
/// 这里只有「这一轮允许做什么」；在哪个群、用什么接口、说话的是谁，都在 [`ChatEnv`]
/// 里。调用方算好后传进来，能力层不再回头看任何插件配置。
///
/// 它同时就是 `[oai.chat]` 那张表的形状——房间里要用哪几项，写在这里就写进了配置。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct ChatConfig {
    /// 不看账号身份、直接开放群管理动作（踢人、禁言、撤回别人的消息、全员禁言、改群名、
    /// 设精华、改他人名片）的群号。私聊里永远不开放；QQ 那边没给权限的照样会被服务端驳回。
    pub management_groups: Vec<String>,
    /// 账号在群里是群主或管理员时自动开放管理动作。
    ///
    /// 号主把这个号设成管理员，本来就是要它帮着管群；再要逐群抄一遍群号是多此一举。
    pub management_auto: bool,
    /// 永远不开放管理动作的群号，压过上面两条。
    pub management_excluded: Vec<String>,
    /// 每群每天最多几次管理动作（解禁、取消精华这类收手的不算）；0 不限。
    ///
    /// 人格判断失误、或被群友带节奏时，这是兜底：一天之内造成的后果有个上限。
    pub management_daily: usize,
    /// 一次发言最多几条（模型把一段话写长了，切开也算额度）。
    pub messages_budget: usize,
    /// 一轮最多几次写动作。
    pub actions_budget: usize,
    /// 一轮最多写几条记忆。
    pub memo_budget: usize,
    pub draw_budget: usize,
    pub music_budget: usize,
    pub video_budget: usize,
    /// 记住群里的人和旧事（落盘，跨重启），与搭话共用一份。
    /// 关掉之后 `satori_memo` 不再挂上，已经记下的留着。
    pub memory_enabled: bool,
    /// 表情包库上限；0 表示不攒。
    pub sticker_max: usize,
    /// 递给模型的群聊条数。
    pub context_turns: usize,
    /// 一段话切成几条的字数阈值；0 表示不切。
    pub split_chars: usize,
    /// 一条消息里的文字超过多少字就排成图片再发；0 关闭。
    ///
    /// 群里一堵字墙会把别人的话顶走。长文排成一页纸只占一条消息，读起来也体面。
    /// 只对「除了引用之外全是文字」的消息生效——带图、文件、@ 的那些各有各的落点。
    pub long_text_chars: usize,
    /// 发出前的时效窗口（秒）：群里又有人说话就整条不发。0 表示不带时效条件。
    pub freshness_seconds: u64,
    /// 发送前在 QQ 里显示「正在输入」。
    ///
    /// 这是给人看的状态，不是消息：实现端不认、内核不回执都不影响这句话发出去
    /// （见 Session::hint，它跑在自己的任务里）。真人不会凭空冒出一句话。
    pub qq_typing: bool,
    /// 看过会话就把它标成已读（看上下文或开口之前各一次，一轮最多一次）。
    pub qq_mark_read: bool,
    /// 一次媒体生成（图、歌、片）最多等多久（秒）。
    pub media_deadline_seconds: u64,
    /// 房间里的工具白名单；留空表示有什么挂什么（本机工具 + 群聊工具 + 联网）。
    ///
    /// 与搭话的 `[ambient].tools` 是同一套写法：逗号分隔的工具名。
    pub tools: String,
}

impl Default for ChatConfig {
    /// 房间那一侧的内置默认：全都开着，额度取保守的一档。
    fn default() -> Self {
        Self {
            management_groups: Vec::new(),
            management_auto: true,
            management_excluded: Vec::new(),
            management_daily: 20,
            messages_budget: 3,
            actions_budget: 6,
            memo_budget: 3,
            draw_budget: 2,
            music_budget: 1,
            // 拍一段片子一次一块多，比写歌贵一倍；想要就自己在 [oai.chat] 里开。
            video_budget: 0,
            memory_enabled: true,
            sticker_max: 120,
            context_turns: 20,
            split_chars: 60,
            long_text_chars: 240,
            freshness_seconds: 0,
            qq_typing: true,
            qq_mark_read: true,
            media_deadline_seconds: 240,
            tools: String::new(),
        }
    }
}

/// 这一轮在哪儿说话：一个群，还是和一个朋友的私聊。
///
/// 两边的共同点是「一个会话」——发消息、读消息、翻记录、戳一戳、撤回、转发
/// 都只认一个 channel id。差别在群才有的东西：群资料、群成员、群管理、@，
/// 以及表情回应——QQ 私聊没有这个入口。
#[derive(Debug, Clone)]
pub(crate) enum Place {
    Group { id: String },
    /// 私聊：channel 由实现端定（QQ 是 private:<QQ号>，微信就是对方 id），
    /// 所以由调用方从现场取来，不在这里拼。
    Direct { channel: String, user_id: String, name: String },
}

impl Place {
    pub(crate) fn group(id: impl Into<String>) -> Self {
        Self::Group { id: id.into() }
    }

    pub(crate) fn direct(
        channel: impl Into<String>,
        user_id: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self::Direct {
            channel: channel.into(),
            user_id: user_id.into(),
            name: name.into(),
        }
    }

    /// Satori 的 channel id：群里就是群号，私聊由实现端给。
    pub(crate) fn channel(&self) -> String {
        match self {
            Self::Group { id, .. } => id.clone(),
            Self::Direct { channel, .. } => channel.clone(),
        }
    }

    /// 群号；私聊没有。
    pub(crate) fn guild(&self) -> Option<&str> {
        match self {
            Self::Group { id, .. } => Some(id),
            Self::Direct { .. } => None,
        }
    }

    pub(crate) fn is_group(&self) -> bool {
        self.guild().is_some()
    }

    /// 日志与回执里怎么说这个地方。
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Group { id, .. } => format!("群 {id}"),
            Self::Direct { user_id, .. } => format!("私聊 {user_id}"),
        }
    }
}

/// 一次行动的环境：在哪儿、用哪几个目录、有没有人格。
pub(crate) struct ChatEnv<'a> {
    pub ctx: &'a Context,
    pub writer: &'a crate::adapters::satori::LockedWriter,
    /// 这一轮说话的地方：群或私聊。两边共用同一套发消息、读消息的出口，
    /// 群才有的动作在私聊里由能力层按 Place::guild 挡掉。
    pub place: Place,
    pub config: ChatConfig,
    /// 这一轮开着。房间总是开着，搭话看自己的开关与群列表。
    pub enabled: bool,
    /// 动手之前先确认群聊没有往前走。
    ///
    /// 搭话的回复只对刚才那一批消息负责，窗口一动就该重看；房间回答的是一句直接
    /// 请求，中间群里聊了什么与这次回答无关。
    pub require_fresh: bool,
    /// 本轮独占的工作目录：工具写文件、生成图片与视频都落在这儿。
    pub scratch: &'a Path,
    /// 本轮生成物落盘的位置，供随后用 `satori_action` 发出去。
    pub media: &'a Path,
    /// 人格那一层。房间没有，`None`。
    pub persona: Option<Arc<dyn Persona>>,
    /// 房间这一轮是谁在问（搭话没有，`None`）。
    pub trigger: Option<trigger::Trigger>,
    /// 这一轮的群聊现场从哪儿来：搭话读自己的常驻窗口，房间向平台要。
    pub scene: session::Scene,
    /// 这一轮的管理动作听谁的。
    pub authority: Authority,
}

/// 这一轮的管理动作听谁的。
///
/// 搭话是自己在群里看着、自己拿主意，群友的话只是它判断的材料；房间是有人点名要它办事，
/// 这时候「谁在吩咐」就决定了能不能动手——客人一句「把他踢了」不能变成真踢。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Authority {
    /// 自己拿主意（搭话）。
    Own,
    /// 号主，或本群群主、管理员在吩咐。
    Staff,
    /// 普通群友在吩咐：管理动作一律不做。
    Guest,
}

impl Authority {
    /// 按吩咐的人认：号主，或在本群是群主、管理员。
    pub(crate) fn of(owner: bool, role: &str) -> Self {
        if owner || is_staff(role) {
            Self::Staff
        } else {
            Self::Guest
        }
    }
}

pub(crate) use crate::adapters::satori::api::is_staff;

/// 这个群开不开放管理动作：`role` 是账号自己在这个群里的身份。
pub(crate) fn management_open(config: &ChatConfig, group: &str, role: &str) -> bool {
    if config.management_excluded.iter().any(|id| id == group) {
        return false;
    }
    config.management_groups.iter().any(|id| id == group)
        || (config.management_auto && is_staff(role))
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
    fn scene(&self, group: &str, turns: &[window::Turn], rhythm: &str) -> serde_json::Value;

    /// 这一轮真的说出去了一句。搭话用它更新状态曲线；房间不需要。
    fn spoke(&self, _group: &str) {}

    /// 打字与思考的节奏。
    fn pace(&self, _group: &str) -> pace::Pace {
        pace::Pace::default()
    }

    /// 看一眼自己的头像时用的接口。取不到就不看，提示词里少一行而已。
    fn avatar(&self) -> Option<Avatar> {
        None
    }

    /// 模型想引用 `target` 回这一句，这条引用留不留。
    ///
    /// 引用是指路牌，不是回话的固定动作；留不留看现场有多乱（见搭话那边的算法）。
    /// 房间没有人格，永远留着。
    fn keeps_quote(&self, _target: &str, _turns: &[window::Turn]) -> bool {
        true
    }

    /// 一条正文发出去之前最后看一眼：返回 `Some(原因)` 就拦下，原因原样回给模型，
    /// 让它换个说法或者干脆不说。房间不拦。
    fn vet_text(&self, _group: &str, _text: &str, _turns: &[window::Turn]) -> Option<String> {
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

/// 这一轮该挂哪些聊天工具（群与私聊同一份；群管理写在 `satori_action` 里按会话收敛）。
///
/// 一道道闸与能力层真正会拒绝的开关是同一份：记忆关着就不给 `satori_memo`，
/// 画图、写歌、拍片额度为 0 连工具都不挂；额度开着但 `[oai.media]` 解不出接口
/// 与密钥（例如只接了 DeepSeek 官方、手上没有图像渠道）也不挂，见 [`super::media::Slots`]。
/// 调用方（房间与搭话）照它拼白名单，于是「模型看得到的工具」和「它真按得动的按钮」
/// 永远对得上。
pub(crate) fn tool_names(config: &ChatConfig, media: super::media::Slots) -> Vec<&'static str> {
    let mut names = vec![
        "satori_context",
        "satori_read",
        "satori_history",
        "satori_action",
    ];
    if config.draw_budget > 0 && media.image {
        names.push("satori_draw");
    }
    if config.memory_enabled && config.memo_budget > 0 {
        names.push("satori_memo");
    }
    if config.music_budget > 0 && media.music {
        names.push("satori_music");
    }
    if config.video_budget > 0 && media.video {
        names.push("satori_video");
    }
    names
}

/// 私聊会话在实现端叫什么：QQ 是 private:<QQ号>，微信就是对方自己的 id。
///
/// 只有拿不到现场时才用它（定时任务：到点没有事件可读）；有现场事件时，
/// 事件里的 channel_id 永远更准。
pub(crate) fn direct_channel(platform: &str, user_id: &str) -> String {
    if platform == "red" {
        format!("private:{user_id}")
    } else {
        user_id.to_string()
    }
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
            "at" => match segment.data.get("qq").and_then(|v| v.as_str()) {
                // 插件发的 @全体成员 不是人格能用的写法，别让它从自己的记录里学去。
                Some(crate::message::AT_ALL) => out.push_str("@全体成员 "),
                id => out.push_str(&format!("[at:{}] ", id.unwrap_or(""))),
            },
            "face" => out.push_str("[表情]"),
            "poke" => out.push_str("[戳一戳]"),
            "dice" => out.push_str("[骰子]"),
            "rps" => out.push_str("[猜拳]"),
            "image" | "mface" => out.push_str("[图片/表情包]"),
            "file" => out.push_str("[文件]"),
            "record" => out.push_str("[语音]"),
            "video" => out.push_str("[视频]"),
            "node" | "forward" => out.push_str("[合并转发]"),
            "json" => {
                let payload = segment
                    .data
                    .get("data")
                    .or_else(|| segment.data.get("content"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                out.push_str("[卡片");
                if let Some(url) = crate::command::card_target_url(payload).filter(|url| {
                    (url.starts_with("http://") || url.starts_with("https://")) && url.len() <= 2048
                }) {
                    out.push_str(": ");
                    out.push_str(&url);
                }
                out.push(']');
            }
            _ => {}
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_uses_private_channel_and_group_uses_id() {
        let direct = Place::direct("private:114514", "114514", "小明");
        assert_eq!(direct.channel(), "private:114514");
        assert!(!direct.is_group());
        assert_eq!(direct.guild(), None);

        let group = Place::group("818965288");
        assert_eq!(group.channel(), "818965288");
        assert!(group.is_group());
        assert_eq!(group.guild(), Some("818965288"));
    }

    /// 只有拿不到现场时才拼频道；QQ 与微信的私聊 id 不是一种写法。
    #[test]
    fn direct_channel_depends_on_platform() {
        assert_eq!(direct_channel("red", "42"), "private:42");
        assert_eq!(direct_channel("wechat", "wxid_x"), "wxid_x");
    }

    /// 私聊的动作表里不能有群管理：模型看到的就是它真能按的。
    #[test]
    fn direct_actions_have_no_group_management() {
        let kinds = session::action_kinds(&Place::direct("private:1", "1", ""));
        assert!(kinds.contains(&"send"));
        for group_only in ["mute", "kick", "mute_all", "announce", "approve", "essence"] {
            assert!(!kinds.contains(&group_only), "{group_only} 不该出现在私聊");
        }
        let group = session::action_kinds(&Place::group("1"));
        assert!(group.contains(&"mute"));
    }
}

