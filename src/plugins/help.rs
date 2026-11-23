//! 帮助中心：插件总览与单插件详情。
//!
//! 两条展示线并存：
//!   - **卡片图**（默认）：把清单排版成一张图发出去，长内容不再刷屏，
//!     版式见 [`card`]，使用 Chromium 网页截图，字体与换行交给浏览器。
//!     总览是「目录」，条目两列并排以便一屏看全；详情是单栏，逐条列指令；
//!   - **纯文本**：`image_enabled = false` 或渲染失败时自动接管，
//!     内容与图一致，绝不出现「图里一套、文字另一套」。
//!
//! **清单只有一份来源：插件注册表**。每个插件的 `summary`、`commands`、
//! `section` 都写在 `plugins/registry.rs` 的注册项里，帮助中心只负责按
//! [`SECTIONS`] 分组和排版。新增一个插件不必改动本文件——它会自动出现在
//! 总览里；忘了填 `section` 会落到「其他」而不是消失，忘了填 `summary`
//! 则由单元测试当场拦下。

mod card;

use crate::adapters::satori::{LockedWriter, send_msg};
use crate::command::{extract_text_arg, get_prefixes, match_command};
use crate::event::Context;
use crate::message::Message;
use crate::plugins::{Cmd, PluginConfig, PluginError, get_config_or_default, get_plugins};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

const LOG_TARGET: &str = "Plugin/Help";

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    enabled: bool,
    /// 仅在私聊应答。帮助属于「自己翻手册」，留在群里只会刷屏；
    /// 打开后群里的 help 原样放行，不消耗事件。
    private_only: bool,
    /// 是否把帮助排版成卡片图；关掉或渲染失败时退回纯文本
    image_enabled: bool,
    /// 卡片图渲染倍率（1.0—4.0）。倍率越高出图越清晰，3.0 在手机上放大也不糊
    image_scale: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            private_only: false,
            image_enabled: true,
            image_scale: 3.0,
        }
    }
}

impl PluginConfig for Config {
    const NAME: &'static str = "help";
}


const TRIGGERS: &[&str] = &["help", "帮助", "插件列表"];

/// 总览里的一个插件条目
struct Entry {
    display: &'static str,
    name: &'static str,
    desc: &'static str,
    enabled: bool,
}

/// 总览的一个分区（按用途分组，而不是把二十来个插件堆成一长串）
struct Group {
    title: &'static str,
    /// 英文代号，只作版式上的次要标识
    en: &'static str,
    items: Vec<Entry>,
}

/// 一个分区：中文名、短名、代号与一句人话说明。
///
/// 控制台直接读这几项来画筛选芯片、分区标题与搜索热词；`/help` 的文本与卡片图
/// 也读同一份。分类的「这个插件为什么在这一栏」写在 `hint` 里，不在界面另编。
pub(crate) struct Section {
    /// 分区代号：注册表里 `section:` 认领的就是它。
    pub code: &'static str,
    /// 完整名字：列表标题、详情面包屑、`/help` 分区标题。
    pub name: &'static str,
    /// 两三个字的短名：筛选芯片位置窄，长名会被省略号吃掉。
    pub short: &'static str,
    /// 英文代号，只作版式上的次要标识。
    pub en: &'static str,
    /// Material Symbols 名字，与控制台图标精灵同一套。
    pub icon: &'static str,
    /// 一句人话：这一栏放什么、不放什么。
    pub hint: &'static str,
}

/// 分区表：决定总览的分组与顺序。
///
/// 按「用户在做什么」分，不按实现分：聊天互动（说话）、群务管理（管人）、
/// AI 对话（模型）、图片与表情 / 影音与网页（处理内容）、数据与统计（回看）、
/// 资讯与提醒（把外面的新鲜事搬进来）、系统与运维（让机器人跑好）。
///
/// 插件在注册表里用 `section` 认领分区代号；代号对不上的（或没填的）
/// 统一落到最后一个分区，不会从帮助里消失。同一分区内先按 `rank` 排，
/// 再沿用注册表顺序，因此新增插件不需要在这里登记第二遍。
const SECTIONS: &[Section] = &[
    Section {
        code: "chat",
        name: "聊天互动",
        short: "聊天",
        en: "CHAT",
        icon: "forum",
        hint: "接话、复读、回显与撤回：让机器人像群友一样说话",
    },
    Section {
        code: "group",
        name: "群务管理",
        short: "群务",
        en: "GROUP",
        icon: "groups",
        hint: "进群、留证、立规矩：欢迎、潜水、违禁词与群精华",
    },
    Section {
        code: "ai",
        name: "AI 对话",
        short: "AI",
        en: "AI",
        icon: "smart_toy",
        hint: "模型对话与群聊搭话：要先配好一把模型钥匙",
    },
    Section {
        code: "image",
        name: "图片与表情",
        short: "图片",
        en: "IMAGE",
        icon: "image",
        hint: "收表情、切图、做码与排版出图：最后都得到一张图",
    },
    Section {
        code: "media",
        name: "影音与网页",
        short: "影音",
        en: "MEDIA",
        icon: "smart_display",
        hint: "取视频、点歌、网页截图与直链互转：进出的是音视频和网页",
    },
    Section {
        code: "data",
        name: "数据与统计",
        short: "数据",
        en: "DATA",
        icon: "leaderboard",
        hint: "记录、统计与词云：先有记录，才有排行榜",
    },
    Section {
        code: "feed",
        name: "资讯与提醒",
        short: "资讯",
        en: "FEED",
        icon: "campaign",
        hint: "AI 资讯与主播开播：外面有新鲜事就主动搬进群",
    },
    Section {
        code: "system",
        name: "系统与运维",
        short: "系统",
        en: "SYSTEM",
        icon: "tune",
        hint: "运行、配置、日志与升级：机器人本身的事",
    },
    Section {
        code: "misc",
        name: "其他",
        short: "其他",
        en: "MISC",
        icon: "extension",
        hint: "还没归类的插件：新插件的临时落脚处",
    },
];

/// 分区表，顺序即展示顺序。
///
/// 帮助总览与控制台的插件页读的是同一份：控制台里那几栏筛选项与 `/help` 上
/// 那几栏是同一件事，分成两份迟早对不上。
pub(crate) fn sections() -> &'static [Section] {
    SECTIONS
}

fn is_enabled(ctx: &Context, name: &str) -> bool {
    let guard = ctx.config.read().unwrap();
    guard
        .plugins
        .get(name)
        .and_then(|v| v.get("enabled"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(false)
}

/// 符号指令（`/#`、`~名`、`##`、`-#` 等）本身就是完整指令，不再拼接前缀。
/// `oai` 开头的词指令两样都认（见 oai::parser），图上写裸写法，短、且与实际用法一致。
pub(crate) fn needs_prefix(cmd: &str) -> bool {
    !cmd.starts_with(['/', '#', '~', '-', '<'])
        && !cmd.starts_with("画·")
        && !cmd.starts_with("oai")
}

fn prefix_of(ctx: &Context) -> String {
    get_prefixes(ctx).first().cloned().unwrap_or_default()
}

/// 按 [`SECTIONS`] 把已注册插件分组；代号对不上的落到最后一个分区，不会消失。
fn grouped(ctx: &Context) -> Vec<Group> {
    let known: Vec<&str> = SECTIONS.iter().map(|section| section.code).collect();
    let fallback = known.last().copied().unwrap_or("misc");

    SECTIONS
        .iter()
        .map(|section| {
            let mut items: Vec<(u8, Entry)> = get_plugins()
                .iter()
                .filter(|p| {
                    let sec = if known.contains(&p.section) {
                        p.section
                    } else {
                        fallback
                    };
                    sec == section.code
                })
                .map(|p| {
                    (
                        p.rank,
                        Entry {
                            display: p.display_name,
                            name: p.name,
                            desc: p.summary,
                            enabled: is_enabled(ctx, p.name),
                        },
                    )
                })
                .collect();
            // 稳定排序：同 rank 的保持注册表顺序，新增插件不用回来调顺序。
            items.sort_by_key(|(rank, _)| *rank);
            Group {
                title: section.name,
                en: section.en,
                items: items.into_iter().map(|(_, entry)| entry).collect(),
            }
        })
        .filter(|g| !g.items.is_empty())
        .collect()
}

/// 纯文本总览：分区与卡片图一致，只是把版式换成缩进
fn render_overview(ctx: &Context, groups: &[Group]) -> String {
    let prefix = prefix_of(ctx);
    let total: usize = groups.iter().map(|g| g.items.len()).sum();
    let enabled = groups
        .iter()
        .flat_map(|g| g.items.iter())
        .filter(|e| e.enabled)
        .count();

    // 不再用横线分隔：分组有 ▍、下一步有 💡，横线只是多占一行。
    let mut out =
        format!("acumen 插件总览\n已启用 {enabled} / {total} 个插件 · 指令前缀 {prefix}\n");

    // 一条两行：首行是身份与开关，次行是它到底做什么，扫读时不必在长句里找边界
    for group in groups {
        out.push_str(&format!("\n▍{}（{} 项）\n", group.title, group.items.len()));
        for item in &group.items {
            let mark = if item.enabled {
                "已启用"
            } else {
                "已停用"
            };
            out.push_str(&format!(
                "{} {}（{}）\n   {}\n",
                mark, item.display, item.name, item.desc
            ));
        }
    }

    out.push_str(&format!(
        "\n💡 看全部指令：{prefix}help <插件名>\n管理开关与配置：{prefix}ctl（聊天）\n连接：Satori v1；状态为配置开关，首次启用自动初始化；排期修改下次连接生效。"
    ));
    out
}

/// 纯文本详情
fn render_detail(ctx: &Context, entry: &Entry, cmds: &[Cmd]) -> String {
    let prefix = prefix_of(ctx);
    let status = if entry.enabled {
        "已启用"
    } else {
        "已停用"
    };

    let mut out = format!(
        "{}（{}）\n状态：{}\n\n{}\n\n",
        entry.display, entry.name, status, entry.desc
    );

    if cmds.is_empty() {
        out.push_str("该插件自动工作，没有需要手动触发的指令。");
        out.push_str(&format!(
            "\n管理：{prefix}ctl show {}；{prefix}ctl on/off {}",
            entry.name, entry.name
        ));
        return out;
    }

    out.push_str("指令\n");
    for c in cmds {
        let full = if needs_prefix(c.cmd) {
            format!("{}{}", prefix, c.cmd)
        } else {
            c.cmd.to_string()
        };
        if c.note.is_empty() {
            out.push_str(&format!("· {full}\n"));
        } else {
            out.push_str(&format!("· {}\n   {}\n", full, c.note));
        }
    }
    out.pop();
    out.push_str(&format!("\n管理：{prefix}ctl show {}；{prefix}ctl on/off {}\n首次启用自动初始化；定时排期修改下次连接生效；详见 {prefix}ctl list", entry.name, entry.name));
    out
}

fn not_found(ctx: &Context, name: &str) -> String {
    format!(
        "❌ 没有找到插件「{}」\n发送 {}help 可以查看全部插件",
        name,
        prefix_of(ctx)
    )
}

/// 插件的指令清单（来自注册表）
fn commands_of(name: &str) -> &'static [Cmd] {
    get_plugins()
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.commands)
        .unwrap_or(&[])
}

/// 按配置键或中文显示名查插件——图上两个名字都印着，用哪个都该找得到
fn lookup(ctx: &Context, arg: &str) -> Option<Entry> {
    let arg = arg.trim();
    get_plugins()
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(arg) || p.display_name == arg)
        .map(|p| Entry {
            display: p.display_name,
            name: p.name,
            desc: p.summary,
            enabled: is_enabled(ctx, p.name),
        })
}

/// 一次回复的内容：文本必备，卡片可选（渲染失败时就靠文本兜底）
struct Reply {
    text: String,
    card: Option<card::Card>,
}

fn build_reply(ctx: &Context, arg: &str) -> Reply {
    let prefix = prefix_of(ctx);

    if arg.is_empty() {
        let groups = grouped(ctx);
        return Reply {
            text: render_overview(ctx, &groups),
            card: Some(card::overview(&groups, &prefix)),
        };
    }

    match lookup(ctx, arg) {
        Some(entry) => {
            let cmds = commands_of(entry.name);
            Reply {
                text: render_detail(ctx, &entry, cmds),
                card: Some(card::detail(&entry, cmds, &prefix)),
            }
        }
        // 找不到时只回一行提示，没必要为一句话专门出图
        None => Reply {
            text: not_found(ctx, arg),
            card: None,
        },
    }
}

/// 这条消息该不该由帮助插件应答。
///
/// `private_only` 打开后只在私聊开口；群聊里的 help 是普通消息，原样放行给
/// 后面的插件，而不是被静默吃掉。抽成纯函数是为了能不启浏览器就测这条门禁。
fn answers(config: &Config, group_id: Option<&str>) -> bool {
    !(config.private_only && group_id.is_some())
}

pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let Some(msg) = ctx.as_message() else { return Ok(Some(ctx)) };
        let config: Config = get_config_or_default(&ctx);
        // 私聊专用时，群里的 help 当普通消息放行，交给后面的插件。
        if !answers(&config, msg.group_id()) {
            return Ok(Some(ctx));
        }

        for trigger in TRIGGERS {
            if let Some(matched) = match_command(&ctx, trigger) {
                let arg = extract_text_arg(&matched.args);
                let reply = build_reply(&ctx, &arg);

                let mut out = Message::new().reply(msg.message_id());
                let browser_path = ctx.browser_path();
                let image = match (&reply.card, config.image_enabled) {
                    (Some(c), true) => {
                        match c.render(config.image_scale, browser_path.as_deref()).await {
                            Ok(b64) => Some(b64),
                            Err(e) => {
                                warn!(target: LOG_TARGET, "帮助网页卡片出图失败，改发纯文本：{e}");
                                None
                            }
                        }
                    }
                    _ => None,
                };
                out = match image {
                    Some(b64) => out.image_described(format!("base64://{b64}"), "帮助卡片"),
                    None => out.text(&reply.text),
                };

                send_msg(
                    &ctx,
                    writer,
                    msg.group_id(),
                    Some(msg.user_id()),
                    out,
                )
                .await?;
                return Ok(None);
            }
        }

        Ok(Some(ctx))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 分区表本身要自洽：代号不重复、名字与说明都不空；注册表里每个插件都能在
    /// 分区表里找到自己的那一栏（写错代号会悄悄落到「其他」，控制台上看着就像丢了）。
    #[test]
    fn sections_cover_every_registered_plugin() {
        let mut seen = std::collections::HashSet::new();
        for section in SECTIONS {
            assert!(seen.insert(section.code), "分区代号重复：{}", section.code);
            for (what, value) in [
                ("name", section.name),
                ("short", section.short),
                ("en", section.en),
                ("icon", section.icon),
                ("hint", section.hint),
            ] {
                assert!(!value.trim().is_empty(), "分区 {} 缺 {what}", section.code);
            }
        }
        for plugin in get_plugins() {
            assert!(
                seen.contains(plugin.section),
                "{} 的 section「{}」不在 help::SECTIONS 里",
                plugin.name,
                plugin.section
            );
            assert!(plugin.rank > 0, "{} 的 rank 要大于 0", plugin.name);
        }
    }
}
