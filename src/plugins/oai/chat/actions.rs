//! 群聊工具的类型化动作；目标固定在本群，管理动作另按配置启用。
use super::window::Turn;
use crate::message::Segment;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use simd_json::base::ValueAsScalar;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Part {
    Text {
        text: String,
    },
    At {
        user_id: String,
    },
    Face {
        id: String,
    },
    Image {
        source: String,
    },
    File {
        source: String,
        name: String,
    },
    Audio {
        source: String,
    },
    Video {
        source: String,
    },
    /// 表情包：要么从眼前这条记录里偷（`message_id` + `index`），要么从自己攒的库里取
    /// （`id`）。参数只带 QQ 号那种原始元素，保留原样再发一遍。
    Sticker {
        #[serde(default)]
        message_id: String,
        #[serde(default)]
        index: usize,
        /// 库里的编号；与 `message_id` 二选一。
        #[serde(default)]
        id: Option<u32>,
        /// 一句话说明这张是什么、什么时候发。偷的时候顺手写，进库当标签；
        /// 配上 `id` 是给库里那张改名。
        #[serde(default)]
        note: String,
    },
    /// 名片卡：把一个群或一个人推荐到聊天里（QQ 的「推荐给好友」那张卡）。
    Contact {
        kind: String,
        id: String,
    },
    Dice,
    Rps,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Action {
    Send {
        parts: Vec<Part>,
        #[serde(default)]
        reply_to: Option<String>,
    },
    Poke {
        user_id: String,
    },
    /// 给一条消息添加或取消自己的表态。群里的任何消息都行——群友的、自己发的、
    /// 自己在客户端手打的都一样：QQ 的表态只落在账号上，与消息作者无关。
    React {
        message_id: String,
        emoji_id: String,
        #[serde(default)]
        remove: bool,
    },
    /// 撤回自己的消息；本群开放管理时也能撤回别人的（清广告、刷屏）。
    Recall {
        message_id: String,
        #[serde(default)]
        reason: String,
    },
    Sign,
    Card {
        card: String,
        #[serde(default)]
        user_id: Option<String>,
    },
    Title {
        user_id: String,
        title: String,
    },
    Essence {
        message_id: String,
        #[serde(default)]
        remove: bool,
    },
    /// 禁言：一次可以点名好几个人（刷屏、开团的时候不用一条条来）。
    Mute {
        user_ids: Vec<String>,
        duration_seconds: u32,
        /// 为什么动手：记进管理记录，下次看前科用；群里看不到。
        #[serde(default)]
        reason: String,
    },
    /// 移出群成员，同样可以一次点名好几个。
    Kick {
        user_ids: Vec<String>,
        #[serde(default)]
        permanent: bool,
        #[serde(default)]
        reason: String,
    },
    MuteAll {
        duration_seconds: u32,
        #[serde(default)]
        reason: String,
    },
    RenameGroup {
        name: String,
    },
    /// 群设置：入群方式、入群问题、群介绍、发言频率、能不能被搜到、能不能被邀请。
    ///
    /// 只写带了的项，其余不动；一项都不带时读当前值（与 satori_group 的 what=detail 同一份）。
    Settings {
        /// any / verify / deny / question_review / question_answer（或 1–5）。
        #[serde(default)]
        join: Option<String>,
        /// 与 answer 成对写。
        #[serde(default)]
        question: Option<String>,
        #[serde(default)]
        answer: Option<String>,
        #[serde(default)]
        allow_invite: Option<bool>,
        /// 能不能靠群号 / 群名搜到。
        #[serde(default)]
        searchable: Option<bool>,
        /// 每人每分钟能发几条。
        #[serde(default)]
        msg_limit: Option<u32>,
        #[serde(default)]
        intro: Option<String>,
    },
    /// 转让群主：只有群主能做，confirm 必须为 true（会真的换人，不是开玩笑）。
    Transfer {
        user_id: String,
        #[serde(default)]
        confirm: bool,
    },
    /// 解散群：不可撤销，confirm 要再写一遍群号。
    Destroy {
        /// 群号，与当前群一致才会执行。
        confirm: String,
        #[serde(default)]
        delete_local_msg: bool,
    },
    /// 本号对这个群的接收方式：notify / receive / assistant / shield。
    MsgMask {
        mode: String,
    },
    /// 置顶或取消置顶这个会话。
    SessionTop {
        #[serde(default = "yes")]
        top: bool,
    },
    /// 邀请某人入群（须管理）。
    Invite {
        user_id: String,
    },
    /// 把已有消息原样转发到别的群或私聊（target 是群号或 private:QQ号）。
    ForwardTo {
        message_ids: Vec<String>,
        target: String,
    },
    /// 资料卡点赞：QQ 的「赞」。
    Like {
        user_id: String,
        #[serde(default = "one")]
        times: u32,
    },
    /// 处理一条加群申请（`request_id` 是管理栏里列出的申请号，也认申请人 QQ 号）。
    Approve {
        request_id: String,
        #[serde(default = "yes")]
        approve: bool,
        /// 拒绝时告诉申请人的话（QQ 会显示给他）；同意时不用。
        #[serde(default)]
        message: String,
        #[serde(default)]
        reason: String,
    },
    /// 发一条群公告。
    Announce {
        text: String,
        #[serde(default)]
        pinned: bool,
        /// 配图：已核实的图片直链或本轮工作目录里的文件。
        #[serde(default)]
        image: Option<String>,
    },
    /// 转发已有消息保持真实作者；整理新内容时统一署机器人自己。
    Forward {
        #[serde(default)]
        message_ids: Vec<String>,
        #[serde(default)]
        texts: Vec<String>,
    },
    /// 发一条 QQ 空间说说（文本动态）。动态发在空间，不是本群。
    Feed {
        text: String,
    },
    /// 账号资料：昵称、个性签名、性别、生日，一次改几项（账号级，跟群无关）。
    Profile {
        #[serde(default)]
        nick: Option<String>,
        #[serde(default)]
        signature: Option<String>,
        /// male / female（或 1 / 2）。
        #[serde(default)]
        gender: Option<String>,
        /// YYYY-MM-DD。
        #[serde(default)]
        birthday: Option<String>,
    },
    Avatar {
        /// 已核实的图片直链或本轮工作目录里的文件。
        image: String,
    },
    /// QQ 在线状态：online / away / invisible / busy / qme / dnd，可带扩展状态。
    Status {
        status: String,
        #[serde(default)]
        ext_status: Option<i64>,
    },
    /// 群文件：上传 / 删除 / 改名。读（列目录、按名搜）在 `satori_group`。
    FileUpload {
        source: String,
        #[serde(default)]
        name: Option<String>,
    },
    FileDelete {
        file_id: String,
    },
    FileRename {
        file_id: String,
        name: String,
    },
    /// 实现端在 `internal/capabilities` 里声明的扩展能力，原样转发。
    ///
    /// 这一支是「协议端多一个动作，智能体当天就能用」的那条路：名字、用途与参数
    /// 说明都由实现端给（`capabilities.catalog`），不必每加一个 QQ 操作就改一遍这份
    /// 枚举与工具 Schema。代价是没有编译期的参数检查，所以调用前照目录核一遍名字，
    /// 额度、管理授权与「群聊有没有往前走」仍然由 [`super::Session`] 统一把关。
    Capability {
        /// 目录里的名字，如 `group_remark`、`group_file_list`。
        name: String,
        /// 原样交给实现端的参数对象。
        #[serde(default)]
        params: serde_json::Value,
    },
}
/// 一个会被写进 QQ 的短名称：非空、不超长、不带控制字符。
fn label(value: &str, max: usize) -> Result<()> {
    ensure!(
        !value.trim().is_empty()
            && value.chars().count() <= max
            && !value.chars().any(char::is_control),
        "名称为空、过长或含控制字符"
    );
    Ok(())
}

fn yes() -> bool {
    true
}

fn one() -> u32 {
    1
}

/// 一次最多点名几个人：再多就是群发操作，该由管理端脚本做。
const MAX_TARGETS: usize = 20;

/// 一批群成员：每个都得是当前窗口里出现过的人，一次不超过 MAX_TARGETS 个。
fn targets(turns: &[Turn], user_ids: &[String]) -> Result<()> {
    ensure!(
        (1..=MAX_TARGETS).contains(&user_ids.len()),
        "一次只能点名 1–{MAX_TARGETS} 个人"
    );
    for user_id in user_ids {
        user(turns, user_id)?;
    }
    Ok(())
}

pub(crate) fn id(raw: &str) -> Result<String> {
    let value = raw.trim();
    ensure!(!value.is_empty(), "ID 不能是空的");
    Ok(value.to_string())
}
pub(crate) fn message<'a>(turns: &'a [Turn], raw: &str) -> Result<&'a Turn> {
    let id = id(raw)?;
    turns
        .iter()
        .find(|t| t.message_id == id)
        .ok_or_else(|| anyhow::anyhow!("消息不在当前窗口内，先读 satori_context"))
}
pub(crate) fn user(turns: &[Turn], raw: &str) -> Result<String> {
    let id = id(raw)?;
    ensure!(
        turns.iter().any(|t| t.user_id == id),
        "目标取自当前群记录里的成员"
    );
    Ok(id)
}

/// 递归扫一遍动作的 JSON：消息号与群友号各收一栏。
fn collect_ids(value: &serde_json::Value, messages: &mut Vec<String>, users: &mut Vec<String>) {
    let push = |raw: &serde_json::Value, out: &mut Vec<String>| match raw {
        serde_json::Value::String(text) if !text.trim().is_empty() => {
            out.push(text.trim().to_string());
        }
        serde_json::Value::Number(number) => out.push(number.to_string()),
        _ => {}
    };
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                match key.as_str() {
                    "message_id" | "reply_to" => push(value, messages),
                    "user_id" => push(value, users),
                    "message_ids" => {
                        if let serde_json::Value::Array(items) = value {
                            for item in items {
                                push(item, messages);
                            }
                        }
                    }
                    "user_ids" => {
                        if let serde_json::Value::Array(items) = value {
                            for item in items {
                                push(item, users);
                            }
                        }
                    }
                    "id" => {}
                    _ => collect_ids(value, messages, users),
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_ids(item, messages, users);
            }
        }
        _ => {}
    }
}

impl Action {
    /// 动作里按号点到的消息与群友。
    ///
    /// 房间那一侧没有常驻窗口，动手之前要把这些号换成眼前看得见的那份记录，校验与
    /// 执行才仍然只认一条规矩。字段名是这份协议的一部分，照着扫一遍比另维护一张
    /// 对应表更不容易漏——`message_id` / `user_id` 出现在哪个动作里都是同一个意思。
    pub(crate) fn referenced(&self) -> (Vec<String>, Vec<String>) {
        let mut messages = Vec::new();
        let mut users = Vec::new();
        if let Ok(value) = serde_json::to_value(self) {
            collect_ids(&value, &mut messages, &mut users);
        }
        messages.sort_unstable();
        messages.dedup();
        users.sort_unstable();
        users.dedup();
        (messages, users)
    }

    pub(crate) fn validate(&self, turns: &[Turn]) -> Result<()> {
        match self {
            Self::Send { parts, reply_to } => {
                ensure!(
                    !parts.is_empty() && parts.len() <= 16,
                    "消息元素数量须为 1–16"
                );
                if let Some(id) = reply_to {
                    message(turns, id)?;
                }
                let mut chars = 0;
                for part in parts {
                    match part {
                        Part::At { user_id } => {
                            user(turns, user_id)?;
                        }
                        Part::Text { text } => {
                            // 关注是给自己留的记号，写进正文就成了发出去的怪话。
                            ensure!(
                                !text.lines().any(super::attention::is_control),
                                "关注指令写在最终输出里，不随消息正文发出"
                            );
                            // 「[戳一戳]」是记录里对动作的描述；当正文发出去，群里就多一条
                            // 孤零零的文字。要戳人用 poke 动作。
                            ensure!(
                                !super::pace::is_poke_placeholder(text),
                                "「[戳一戳]」只是记录里的占位符，发出去就是一条文字；要戳人用 poke 动作"
                            );
                            chars += text.chars().count();
                        }
                        Part::Face { id } => {
                            ensure!(id.parse::<u32>().is_ok(), "表情 ID 用数字");
                        }
                        Part::Sticker {
                            message_id,
                            index,
                            id,
                            ..
                        } => {
                            ensure!(
                                id.is_some() || !message_id.is_empty(),
                                "表情包要么给 id（自己攒的那张），要么给 message_id（从记录里偷）"
                            );
                            if id.is_none() {
                                sticker(message(turns, message_id)?, *index)?;
                            }
                        }
                        Part::Contact { kind, id: target } => {
                            ensure!(
                                matches!(kind.trim(), "group" | "user"),
                                "名片卡只认 group / user"
                            );
                            id(target)?;
                        }
                        Part::File { name, .. } => {
                            ensure!(
                                !name.is_empty()
                                    && name.len() <= 180
                                    && !name.contains(['/', '\\', '\n', '\r']),
                                "文件名无效"
                            );
                        }
                        _ => {}
                    }
                }
                ensure!(
                    chars <= 4000,
                    "一条消息正文最多 4000 字；长材料请整理为文件或合并转发"
                );
                // 复读不占额度也不该发出去：这里拦下来，模型还有机会换一句。
                let body: String = parts
                    .iter()
                    .filter_map(|part| match part {
                        Part::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                ensure!(
                    !super::tone::echoes(&body, turns),
                    "这句话刚说过，换个说法，或者先放着也行"
                );
            }
            Self::Poke { user_id } => {
                user(turns, user_id)?;
            }
            Self::React {
                message_id,
                emoji_id,
                ..
            } => {
                message(turns, message_id)?;
                ensure!(emoji_id.parse::<u32>().is_ok(), "表态 ID 用数字");
            }
            // 撤回别人的消息算管理动作，开没开放由调用方按 [`Self::requires_management`] 把关。
            Self::Recall { message_id, .. } => {
                message(turns, message_id)?;
            }
            Self::Sign => {}
            Self::Card { user_id, card } => {
                if let Some(uid) = user_id {
                    user(turns, uid)?;
                }
                ensure!(
                    card.chars().count() <= 60 && !card.chars().any(char::is_control),
                    "群名片最多 60 字且不能含控制字符"
                );
            }
            Self::Title { user_id, title } => {
                user(turns, user_id)?;
                ensure!(
                    title.chars().count() <= 18 && !title.chars().any(char::is_control),
                    "群头衔最多 18 字且不能含控制字符"
                );
            }
            Self::Essence { message_id, .. } => {
                message(turns, message_id)?;
            }
            Self::Mute {
                user_ids,
                duration_seconds,
                ..
            } => {
                targets(turns, user_ids)?;
                ensure!(*duration_seconds <= 30 * 86400, "禁言最多 30 天；0 解除");
            }
            Self::Kick { user_ids, .. } => {
                targets(turns, user_ids)?;
            }
            Self::MuteAll {
                duration_seconds, ..
            } => {
                ensure!(
                    *duration_seconds <= 30 * 86400,
                    "全员禁言最多 30 天；0 解除"
                );
            }
            Self::RenameGroup { name } => label(name, 60)?,
            Self::Settings {
                join,
                question,
                answer,
                allow_invite,
                searchable,
                msg_limit,
                intro,
            } => {
                ensure!(
                    join.is_some()
                        || question.is_some()
                        || answer.is_some()
                        || allow_invite.is_some()
                        || searchable.is_some()
                        || msg_limit.is_some()
                        || intro.is_some(),
                    "什么都不改就别调这个动作；要看当前设置用 satori_group 的 what=detail"
                );
                if let Some(join) = join {
                    ensure!(
                        matches!(
                            join.trim(),
                            "any"
                                | "verify"
                                | "deny"
                                | "question_review"
                                | "question_answer"
                                | "1"
                                | "2"
                                | "3"
                                | "4"
                                | "5"
                        ),
                        "join 写 any / verify / deny / question_review / question_answer"
                    );
                }
                ensure!(
                    question.is_none() == answer.is_none(),
                    "入群问题与答案要成对写"
                );
                for text in [question.as_deref(), answer.as_deref()].into_iter().flatten() {
                    ensure!(
                        text.chars().count() <= 200 && !text.chars().any(char::is_control),
                        "入群问题与答案各不超过 200 字"
                    );
                }
                if let Some(limit) = msg_limit {
                    ensure!(*limit <= 600, "发言频率上限写得太大（秒数，0 为不限）");
                }
                if let Some(intro) = intro {
                    ensure!(
                        intro.chars().count() <= 1000 && !intro.chars().any(char::is_control),
                        "群介绍不超过 1000 字"
                    );
                }
            }
            Self::Transfer { user_id, confirm } => {
                user(turns, user_id)?;
                ensure!(
                    *confirm,
                    "转让群主不可撤回：确认要转让就把 confirm 写成 true"
                );
            }
            Self::Destroy { confirm, .. } => {
                ensure!(
                    !confirm.trim().is_empty(),
                    "解散群不可撤销：confirm 要再写一遍群号"
                );
            }
            Self::MsgMask { mode } => {
                ensure!(
                    matches!(
                        mode.trim(),
                        "notify" | "receive" | "assistant" | "shield"
                    ),
                    "mode 写 notify / receive / assistant / shield"
                );
            }
            Self::SessionTop { .. } => {}
            Self::Invite { user_id } => {
                user(turns, user_id)?;
            }
            Self::ForwardTo {
                message_ids,
                target,
            } => {
                ensure!(
                    (1..=20).contains(&message_ids.len()),
                    "一次转发 1–20 条消息"
                );
                for id in message_ids {
                    let turn = message(turns, id)?;
                    ensure!(!turn.elements.0.is_empty(), "该条记录没有可转发的原始内容");
                }
                let target = target.trim();
                let digits = |text: &str| {
                    !text.is_empty() && text.chars().all(|c| c.is_ascii_digit())
                };
                ensure!(
                    digits(target)
                        || target
                            .strip_prefix("private:")
                            .is_some_and(digits),
                    "target 写群号或 private:QQ号"
                );
            }
            Self::Approve { request_id, message, .. } => {
                id(request_id)?;
                ensure!(
                    message.chars().count() <= 60 && !message.chars().any(char::is_control),
                    "拒绝理由最多 60 字且不能含控制字符"
                );
            }
            Self::Announce { text, image, .. } => {
                let count = text.trim().chars().count();
                ensure!((1..=1000).contains(&count), "公告正文 1–1000 字");
                ensure!(
                    !text.lines().any(super::attention::is_control),
                    "关注指令写在最终输出里，不进公告"
                );
                if let Some(image) = image {
                    ensure!(!image.trim().is_empty(), "配图来源是空的；不配图就省略 image");
                }
            }
            Self::Forward { message_ids, texts } => {
                ensure!(
                    (1..=12).contains(&(message_ids.len() + texts.len())),
                    "合并转发须有 1–12 个节点"
                );
                ensure!(
                    texts.iter().map(|s| s.chars().count()).sum::<usize>() <= 16000,
                    "转发正文过长"
                );
                ensure!(
                    !texts
                        .iter()
                        .any(|text| text.lines().any(super::attention::is_control)),
                    "关注指令写在最终输出里，不随转发正文发出"
                );
                for id in message_ids {
                    let turn = message(turns, id)?;
                    ensure!(!turn.elements.0.is_empty(), "该条记录没有可转发的原始内容");
                    ensure!(
                        !turn
                            .elements
                            .0
                            .iter()
                            .any(|s| matches!(s.type_.as_str(), "node" | "forward" | "poke")),
                        "不支持嵌套转发或转发动作"
                    );
                }
            }
            Self::Feed { text } => {
                let count = text.trim().chars().count();
                ensure!((1..=2000).contains(&count), "说说正文 1–2000 字");
                ensure!(
                    !text.lines().any(super::attention::is_control),
                    "关注指令写在最终输出里，不进说说"
                );
            }
            Self::Profile {
                nick,
                signature,
                gender,
                birthday,
            } => {
                ensure!(
                    nick.is_some() || signature.is_some() || gender.is_some() || birthday.is_some(),
                    "profile 至少给一项：nick / signature / gender / birthday"
                );
                if let Some(nick) = nick {
                    label(nick, 30)?;
                }
                if let Some(signature) = signature {
                    label(signature, 120)?;
                }
                if let Some(gender) = gender {
                    ensure!(
                        matches!(gender.trim(), "male" | "female" | "1" | "2" | "男" | "女"),
                        "gender 写 male / female"
                    );
                }
                if let Some(birthday) = birthday {
                    let birthday = birthday.trim();
                    let ok = birthday.len() == 10
                        && birthday.as_bytes()[4] == b'-'
                        && birthday.as_bytes()[7] == b'-'
                        && birthday
                            .split('-')
                            .all(|part| part.chars().all(|c| c.is_ascii_digit()));
                    ensure!(ok, "birthday 写成 YYYY-MM-DD");
                }
            }
            Self::Status { status, .. } => {
                ensure!(
                    matches!(
                        status.trim(),
                        "online" | "away" | "invisible" | "busy" | "qme" | "dnd"
                    ),
                    "status 写 online / away / invisible / busy / qme / dnd"
                );
            }
            Self::Like { user_id, times } => {
                user(turns, user_id)?;
                ensure!((1..=20).contains(times), "点赞 1–20 次");
            }
            Self::Avatar { image } => {
                ensure!(!image.trim().is_empty(), "头像来源是空的");
            }
            Self::FileDelete { file_id } => {
                id(file_id)?;
            }
            Self::FileUpload { source, name } => {
                ensure!(!source.trim().is_empty(), "上传来源是空的");
                if let Some(name) = name {
                    label(name, 180)?;
                }
            }
            Self::FileRename { file_id, name } => {
                id(file_id)?;
                label(name, 120)?;
            }
            // 扩展能力的参数不是类型化的，实现端自己校验；名字在目录里核过。
            Self::Capability { name, .. } => {
                id(name)?;
            }
        }
        Ok(())
    }
    /// 只有群里才有对象的动作。私聊里没有群资料、没有群成员、没有管理。
    pub(crate) fn needs_group(&self) -> bool {
        matches!(
            self,
            // 表态只有群里有：QQ 私聊没有表情回应。
            Self::React { .. }
                | Self::Sign
                | Self::Card { .. }
                | Self::Title { .. }
                | Self::Essence { .. }
                | Self::Mute { .. }
                | Self::Kick { .. }
                | Self::MuteAll { .. }
                | Self::RenameGroup { .. }
                | Self::Announce { .. }
                | Self::FileUpload { .. }
                | Self::FileDelete { .. }
                | Self::FileRename { .. }
                | Self::Settings { .. }
                | Self::Transfer { .. }
                | Self::Destroy { .. }
                | Self::MsgMask { .. }
                | Self::Invite { .. }
        )
    }

    /// 这个动作落在谁头上（可能不止一个）。整群动作与改自己的动作是空的。
    pub(crate) fn management_targets(&self) -> Vec<&str> {
        match self {
            Self::Card { user_id, .. } => user_id.as_deref().into_iter().collect(),
            Self::Title { user_id, .. } | Self::Transfer { user_id, .. } => vec![user_id.as_str()],
            Self::Mute { user_ids, .. } | Self::Kick { user_ids, .. } => {
                user_ids.iter().map(String::as_str).collect()
            }
            _ => Vec::new(),
        }
    }
    /// 这一下算不算群管理：会被全群看见、落在别人头上的写操作。
    ///
    /// 撤回要看是谁的消息：撤自己的是补救，撤别人的是管理（`turns` 里认作者）。
    pub(crate) fn requires_management(&self, me: &str, turns: &[Turn]) -> bool {
        match self {
            Self::Card { user_id, .. } => user_id.as_deref().is_some_and(|id| id != me),
            Self::Recall { message_id, .. } => message(turns, message_id).is_ok_and(|t| !t.from_me),
            Self::Title { .. }
            | Self::Essence { .. }
            | Self::Mute { .. }
            | Self::Kick { .. }
            | Self::MuteAll { .. }
            | Self::RenameGroup { .. }
            | Self::Approve { .. }
            | Self::Announce { .. }
            // 删、改群文件落在别人传的东西上，算管理；列表、下载、上传不算。
            | Self::FileDelete { .. }
            | Self::FileRename { .. }
            | Self::Settings { .. }
            | Self::Transfer { .. }
            | Self::Destroy { .. }
            | Self::Invite { .. } => true,
            _ => false,
        }
    }
    /// 收手的管理动作：解禁、解除全员禁言、取消精华。不算进每天的次数。
    pub(crate) fn lifts(&self) -> bool {
        matches!(
            self,
            Self::Mute {
                duration_seconds: 0,
                ..
            } | Self::MuteAll {
                duration_seconds: 0,
                ..
            } | Self::Essence { remove: true, .. }
        )
    }
    /// 会罚到某个人头上的：禁言、踢出、撤回他的消息。对象是群主、管理员或号主时不做。
    pub(crate) fn punishes(&self) -> bool {
        matches!(
            self,
            Self::Mute {
                duration_seconds: 1..,
                ..
            } | Self::Kick { .. }
                | Self::Recall { .. }
        )
    }
    /// 动手时写的理由（没有理由字段的动作给空串）。
    pub(crate) fn reason(&self) -> &str {
        match self {
            Self::Mute { reason, .. }
            | Self::Kick { reason, .. }
            | Self::MuteAll { reason, .. }
            | Self::Recall { reason, .. }
            | Self::Approve { reason, .. } => reason.trim(),
            _ => "",
        }
    }
    pub(crate) fn is_message(&self) -> bool {
        matches!(
            self,
            Self::Send { .. } | Self::Forward { .. } | Self::ForwardTo { .. }
        )
    }
}

/// 这条消息里的第 `index` 张图或商城表情，原样取出来。
///
/// 取出来是为着再发一遍（偷），或者抄进表情包库（见 [`super::stickers`]）。
pub(crate) fn sticker(turn: &Turn, index: usize) -> Result<Segment> {
    turn.elements
        .0
        .iter()
        .filter(|s| matches!(s.type_.as_str(), "image" | "mface"))
        .nth(index)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("这条消息没有对应的图片/表情包"))
}

/// 图那种段落在网上的位置。商城表情没有这一项——它靠参数重发，不必下载。
pub(crate) fn image_source(segment: &Segment) -> Option<&str> {
    segment
        .data
        .get("url")
        .or_else(|| segment.data.get("file"))
        .and_then(|value| value.as_str())
        .filter(|url| url.starts_with("http"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(value: serde_json::Value) -> Action {
        serde_json::from_value(value).expect("动作解析")
    }

    #[test]
    fn batch_management_parses_and_counts_as_management() {
        let action = parse(json!({"action":"kick","user_ids":["7","8"],"reason":"广告"}));
        assert!(action.needs_group());
        assert!(action.punishes());
        assert!(action.requires_management("1", &[]));
        assert_eq!(action.management_targets(), vec!["7", "8"]);
        assert_eq!(action.reason(), "广告");

        let mute = parse(json!({"action":"mute","user_ids":["7"],"duration_seconds":0}));
        assert!(mute.lifts(), "0 秒是解禁，不算管理次数");
        assert!(!mute.punishes());
    }

    #[test]
    fn account_profile_needs_at_least_one_field() {
        assert!(parse(json!({"action":"profile"})).validate(&[]).is_err());
        assert!(parse(json!({"action":"profile","gender":"male","birthday":"1996-03-02"})).validate(&[]).is_ok());
        assert!(parse(json!({"action":"profile","birthday":"1996/03/02"})).validate(&[]).is_err());
        assert!(parse(json!({"action":"status","status":"隐身"})).validate(&[]).is_err());
        assert!(parse(json!({"action":"status","status":"invisible"})).validate(&[]).is_ok());
    }

    #[test]
    fn forward_to_wants_a_real_target() {
        let turns = vec![Turn {
            message_id: "m1".into(),
            elements: crate::message::Message::new().text("原文"),
            ..Turn::default()
        }];
        let ok = parse(json!({"action":"forward_to","message_ids":["m1"],"target":"private:42"}));
        assert!(ok.validate(&turns).is_ok());
        assert!(ok.is_message());
        let bad = parse(json!({"action":"forward_to","message_ids":["m1"],"target":"abc"}));
        assert!(bad.validate(&turns).is_err());
        let empty = parse(json!({"action":"forward_to","message_ids":[],"target":"42"}));
        assert!(empty.validate(&turns).is_err());
    }

    #[test]
    fn settings_must_change_something() {
        assert!(parse(json!({"action":"settings"})).validate(&[]).is_err());
        assert!(parse(json!({"action":"settings","join":"随便"})).validate(&[]).is_err());
        assert!(parse(json!({"action":"settings","question":"口令"})).validate(&[]).is_err(), "问题与答案要成对");
        let ok = parse(json!({"action":"settings","join":"verify","intro":"只聊技术"}));
        assert!(ok.validate(&[]).is_ok());
        assert!(ok.requires_management("1", &[]));
    }

    #[test]
    fn contact_card_is_a_part_not_a_guess() {
        let action = parse(json!({"action":"send","parts":[{"type":"contact","kind":"group","id":"818965288"}]}));
        match action {
            Action::Send { parts, .. } => assert!(matches!(parts[0], Part::Contact { .. })),
            other => panic!("应解析成 send：{other:?}"),
        }
        let bad = parse(json!({"action":"send","parts":[{"type":"contact","kind":"robot","id":"1"}]}));
        assert!(bad.validate(&[]).is_err());
    }

    #[test]
    fn destroy_and_transfer_ask_before_they_act() {
        let turns = vec![Turn { user_id: "7".into(), ..Turn::default() }];
        assert!(parse(json!({"action":"transfer","user_id":"7","confirm":true})).validate(&turns).is_ok());
        let no = parse(json!({"action":"transfer","user_id":"7"}));
        assert!(no.validate(&turns).is_err(), "没写 confirm 不转让");
        let destroy = parse(json!({"action":"destroy","confirm":"818965288"}));
        assert!(destroy.validate(&[]).is_ok());
        assert!(destroy.requires_management("1", &[]));
        assert!(destroy.needs_group());
    }
}
