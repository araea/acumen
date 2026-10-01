//! 网页卡片：共用纸面排版，由 Chromium 完成字体塑形、换行与 PNG 截图。

use super::{Entry, Group, needs_prefix};
use crate::plugins::Cmd;
use crate::render::web::{self, Block, Doc, Item, Theme, Tone};

/// 总览版心宽度。总览是「目录」，条目多、每条约两句，
/// 版心放宽到两列并排，高度随之减半，一屏能扫完。
const OVERVIEW_WIDTH: f32 = 920.0;
/// 总览的列数：两列网格并排，条目左右对齐，同行的分隔线也齐平
const OVERVIEW_COLS: usize = 2;
/// 详情版心宽度（单栏，长指令自动换行）
const DETAIL_WIDTH: f32 = 640.0;

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

/// 总览卡：分区 → 两列目录，状态文字与颜色同时呈现。
///
/// 条目按两列网格排，而不是拉成一长串：目录的意义是「一眼扫全」，
/// 单列会把 20 来个插件堆成一张需要往下翻很久的长图。
pub fn overview(groups: &[Group], prefix: &str) -> Card {
    let mut blocks = vec![
        Block::Title {
            title: "插件总览".into(),
            pill: None,
            sub: format!("按用途查找功能 · 指令前缀 {prefix}"),
        },
        // 汇总启用与停用数量，状态清单在下方逐项展开
        Block::Meter(
            groups
                .iter()
                .flat_map(|g| g.items.iter())
                .map(|e| e.enabled)
                .collect(),
        ),
        Block::Rule,
    ];

    for group in groups {
        blocks.push(Block::Section {
            title: group.title.into(),
            en: group.en.into(),
            count: format!("{} 项", group.items.len()),
        });
        blocks.push(Block::Items {
            items: group
                .items
                .iter()
                .map(|e| Item {
                    name: e.display.into(),
                    key: e.name.into(),
                    desc: e.desc.into(),
                    on: e.enabled,
                })
                .collect(),
            cols: OVERVIEW_COLS,
        });
    }

    blocks.push(Block::Callout {
        tone: Tone::Info,
        text: format!(
            "Satori v1 · 管理开关与配置：{prefix}ctl（聊天）\n状态为配置开关；首次启用自动初始化；排期修改下次连接生效"
        ),
    });

    Card(Doc {
        theme: Theme::Help,
        width: OVERVIEW_WIDTH,
        kicker: "ACUMEN · MANUAL".into(),
        blocks,
        foot: "开关状态以当前配置为准".into(),
        hint: (
            "查看某个插件的全部指令".into(),
            format!("{prefix}help <插件名>"),
        ),
    })
}

/// 详情卡：一条指令一格，主指令抬到 chip 里，别名与说明依次下沉
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
        Block::Callout {
            tone: Tone::Info,
            text: entry.desc.into(),
        },
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

    blocks.push(Block::Callout {
        tone: Tone::Info,
        text: format!(
            "管理：{p}ctl show {n}\n开关：{p}ctl on/off {n}\n首次启用自动初始化；定时排期修改下次连接生效；详见 {p}ctl list",
            p = prefix,
            n = entry.name
        ),
    });

    Card(Doc {
        theme: Theme::Help,
        width: DETAIL_WIDTH,
        kicker: "MANUAL · PLUGIN".into(),
        blocks,
        foot: "ACUMEN · 插件手册".into(),
        hint: ("回到插件总览".into(), format!("{prefix}help")),
    })
}
