//! 网页卡片：共用纸面排版，由 Chromium 完成字体塑形、换行与 PNG 截图。

use super::{Entry, Group, needs_prefix};
use crate::plugins::Cmd;
use crate::render::web::{self, Block, Doc, Fact, Item, State, Theme, Tile, Tone};

/// 成图宽度：卡面 520 加两侧各 20 的相纸，与智能回复卡同宽。
///
/// 总览与详情共用这一档。早先总览放宽到 920 两列并排、一屏扫完，代价是手机里全屏看时
/// 字只剩 8dp 上下；现在宁可图长一些，也让每个字在手机上不必放大就能读。
const WIDTH: f32 = 560.0;

/// `"收 / 偷 / 存表情"` → 主指令 + 别名。清单里的别名用 ` / ` 分隔，
/// 图里把第一个抬成主指令，其余降级成小字，避免一行挤三个同义词。
fn split_aliases(cmd: &str) -> (&str, Vec<&str>) {
    let mut parts = cmd.split(" / ").map(str::trim).filter(|s| !s.is_empty());
    let primary = parts.next().unwrap_or(cmd);
    (primary, parts.collect())
}

/// 带前缀的完整指令。符号指令本身就是完整写法，不再拼前缀。
fn full_cmd(prefix: &str, cmd: &str) -> String {
    if needs_prefix(cmd) {
        format!("{prefix}{cmd}")
    } else {
        cmd.to_string()
    }
}

/// 一张待渲染的卡片：Doc 与它的渲染入口绑在一起
pub struct Card(Doc);

impl Card {
    /// 出图失败时由调用方回退到完整文本。
    pub async fn render(&self, scale: f64, browser_path: Option<&str>) -> anyhow::Result<String> {
        web::capture(&self.0, scale, browser_path).await
    }
}

/// 总览卡：分区 → 分段列表，停用的条目挂徽章，启用是常态不挂。
pub fn overview(groups: &[Group], prefix: &str) -> Card {
    let total = groups.iter().map(|g| g.items.len()).sum::<usize>();
    let on = groups
        .iter()
        .flat_map(|g| g.items.iter())
        .filter(|e| e.enabled)
        .count();
    let mut blocks = vec![
        Block::Title {
            title: "插件总览".into(),
            pill: None,
            sub: format!("按用途查找功能 · 指令前缀 {prefix}"),
        },
        Block::Tiles(vec![
            Tile {
                value: total.to_string(),
                label: "全部".into(),
                state: State::Plain,
            },
            Tile {
                value: on.to_string(),
                label: "已启用".into(),
                state: State::On,
            },
            Tile {
                value: (total - on).to_string(),
                label: "已停用".into(),
                state: State::Off,
            },
        ]),
    ];

    for group in groups {
        blocks.push(Block::Section {
            title: group.title.into(),
            en: group.en.into(),
            count: format!("{} 项", group.items.len()),
        });
        blocks.push(Block::Items(
            group
                .items
                .iter()
                .map(|e| Item {
                    name: e.display.into(),
                    key: e.name.into(),
                    desc: e.desc.into(),
                    on: e.enabled,
                })
                .collect(),
        ));
    }

    blocks.push(Block::Notes(vec![
        "没标「已停用」的插件都是启用状态".into(),
        format!("开关与配置用 {prefix}ctl；首次启用会自动初始化"),
        "定时排期的改动要到下次连接才生效".into(),
    ]));

    Card(Doc {
        theme: Theme::Help,
        width: WIDTH,
        kicker: "ACUMEN · MANUAL".into(),
        blocks,
        foot: "开关状态以当前配置为准".into(),
        hint: (
            "查看某个插件的全部指令".into(),
            format!("{prefix}help <插件名>"),
        ),
    })
}

/// 详情卡：简介一段，指令一条一格；照打的字与占位符分开着色
pub fn detail(entry: &Entry, cmds: &[Cmd], prefix: &str) -> Card {
    let mut blocks = vec![
        Block::Title {
            title: entry.display.into(),
            pill: Some((
                if entry.enabled {
                    "已启用"
                } else {
                    "已停用"
                }
                .into(),
                entry.enabled,
            )),
            sub: format!("配置键 {}", entry.name),
        },
        Block::Lead(entry.desc.into()),
    ];

    if cmds.is_empty() {
        blocks.push(Block::Callout {
            tone: Tone::Empty,
            text: "该插件自动工作，没有需要手动触发的指令".into(),
        });
    } else {
        blocks.push(Block::Section {
            title: "指令".into(),
            en: "COMMANDS".into(),
            count: format!("{} 条", cmds.len()),
        });
        blocks.push(Block::Cmds(
            cmds.iter()
                .map(|c| {
                    let (primary, aliases) = split_aliases(c.cmd);
                    web::Cmd {
                        prefix: if needs_prefix(primary) {
                            prefix.into()
                        } else {
                            String::new()
                        },
                        cmd: primary.into(),
                        note: c.note.into(),
                        aliases: aliases.iter().map(|a| full_cmd(prefix, a)).collect(),
                    }
                })
                .collect(),
        ));
    }

    blocks.push(Block::Section {
        title: "管理".into(),
        en: "MANAGE".into(),
        count: String::new(),
    });
    blocks.push(Block::Facts(vec![
        Fact {
            label: "查看配置".into(),
            command: format!("{prefix}ctl show {}", entry.name),
        },
        Fact {
            label: "启用".into(),
            command: format!("{prefix}ctl on {}", entry.name),
        },
        Fact {
            label: "停用".into(),
            command: format!("{prefix}ctl off {}", entry.name),
        },
    ]));
    blocks.push(Block::Callout {
        tone: Tone::Info,
        text: format!("首次启用自动初始化；定时排期的改动要到下次连接才生效。全部插件的状态见 {prefix}ctl list"),
    });

    Card(Doc {
        theme: Theme::Help,
        width: WIDTH,
        kicker: "MANUAL · PLUGIN".into(),
        blocks,
        foot: "ACUMEN · 插件手册".into(),
        hint: ("回到插件总览".into(), format!("{prefix}help")),
    })
}
