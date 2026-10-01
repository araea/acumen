//! 网页卡片：共用纸面排版，由 Chromium 完成字体塑形、换行与 PNG 截图。

use crate::plugins::help::needs_prefix;
use crate::render::web::{self, Block, Cell, DiffRow, Doc, Fact, State, Theme, Tile, Tone};

/// 成图宽度：卡面 520 加两侧各 20 的相纸，与手册卡、智能回复卡同宽。
/// 手机里全屏看时正文仍有 13dp 上下，不必放大。
const WIDTH: f32 = 560.0;

/// 一张待渲染的控制卡
pub struct Card(Doc);

impl Card {
    /// 出图失败时由调用方回退到完整文本。
    pub async fn render(&self, scale: f64, browser_path: Option<&str>) -> anyhow::Result<String> {
        web::capture(&self.0, scale, browser_path).await
    }
}

fn doc(kicker: &str, blocks: Vec<Block>, foot: &str, hint: (String, String)) -> Card {
    Card(Doc {
        theme: Theme::Control,
        width: WIDTH,
        kicker: kicker.into(),
        blocks,
        foot: foot.into(),
        hint,
    })
}

/// 用法卡：指令清单直接取自注册表里 ctl 自己的 `commands`，
/// 改指令只改注册表一处，图、纯文本用法与 `/help ctl` 三处同步。
pub fn usage(prefix: &str, cmds: &[crate::plugins::Cmd]) -> Card {
    let blocks = vec![
        Block::Title {
            title: "插件控制".into(),
            pill: None,
            sub: format!("全局开关与配置的统一入口 · 别名 {prefix}控制 {prefix}插件"),
        },
        Block::Section {
            title: "指令".into(),
            en: "COMMANDS".into(),
            count: format!("{} 条", cmds.len()),
        },
        Block::Cmds(
            cmds.iter()
                .map(|c| {
                    let primary = c.cmd.split(" / ").next().unwrap_or(c.cmd).trim();
                    let aliases: Vec<String> = c
                        .cmd
                        .split(" / ")
                        .skip(1)
                        .map(|a| format!("{prefix}{}", a.trim()))
                        .collect();
                    web::Cmd {
                        prefix: if needs_prefix(primary) {
                            prefix.into()
                        } else {
                            String::new()
                        },
                        cmd: primary.into(),
                        note: c.note.into(),
                        aliases,
                    }
                })
                .collect(),
        ),
        Block::Section {
            title: "示例".into(),
            en: "EXAMPLES".into(),
            count: String::new(),
        },
        Block::Code {
            lang: None,
            lines: vec![
                format!("{prefix}ctl on 帮助中心 echo"),
                format!("{prefix}ctl set repeater channel.white [123456]"),
                format!("{prefix}ctl set oai plain_text_max_chars 120"),
                format!("{prefix}ctl reset ai_news --confirm"),
            ],
        },
        Block::Section {
            title: "须知".into(),
            en: "NOTES".into(),
            count: String::new(),
        },
        Block::Notes(vec![
            "中文操作：列表、开启、关闭、查看、默认、设置、重置、差异".into(),
            "插件名支持英文名与中文显示名，多个名称以空格或逗号分隔".into(),
            "查看与修改配置仅限 ctl.admins；本机控制台始终可管理".into(),
            "ctl 保留管理入口；修改它的 admins 请在私聊或控制台执行".into(),
            "首次启用自动初始化；定时排期的改动要到下次连接才生效，其余下一条消息即生效".into(),
        ]),
    ];
    doc(
        "ACUMEN · CONTROL",
        blocks,
        "状态以当前配置为准 · 标「待重启」的等下次启动",
        ("查看全局状态".into(), format!("{prefix}ctl list")),
    )
}

/// 一行插件状态
pub struct Status {
    pub name: &'static str,
    pub display: &'static str,
    pub on: bool,
    /// 已开启但要等重启才真正跑起来
    pub pending: bool,
}

/// 状态卡：三格读数 + 按状态分组的两列小格。
///
/// 分组而不是逐个插件一行：「待重启」与「已停用」是要被看到的例外，排在最前；
/// 占大多数的「已启用」收在最后，两列并排。三个分组互不相交（待重启本身也是已开启，
/// 但在这里只算待重启），所以三格读数加起来正好是插件总数。
pub fn list(prefix: &str, filter: &str, rows: &[Status]) -> Card {
    let on = rows.iter().filter(|r| r.on).count();
    let pending = rows.iter().filter(|r| r.pending).count();
    let sub = if filter.is_empty() {
        format!("全局配置 · 共 {} 个插件 · 已开启 {on} 个", rows.len())
    } else {
        format!("筛选「{filter}」· 命中 {} 个 · 已开启 {on} 个", rows.len())
    };

    let mut blocks = vec![
        Block::Title {
            title: "插件状态".into(),
            pill: None,
            sub,
        },
        Block::Tiles(vec![
            Tile {
                value: (on - pending).to_string(),
                label: "已启用".into(),
                state: State::On,
            },
            Tile {
                value: pending.to_string(),
                label: "待重启".into(),
                state: State::Pending,
            },
            Tile {
                value: (rows.len() - on).to_string(),
                label: "已停用".into(),
                state: State::Off,
            },
        ]),
    ];

    if rows.is_empty() {
        blocks.push(Block::Callout {
            tone: Tone::Empty,
            text: "没有匹配的插件；去掉筛选词可以看到全部。".into(),
        });
    } else {
        let groups = [
            ("待重启", "PENDING", State::Pending),
            ("已停用", "OFF", State::Off),
            ("已启用", "ON", State::On),
        ];
        for (title, en, state) in groups {
            let cells: Vec<Cell> = rows
                .iter()
                .filter(|r| match state {
                    State::Pending => r.pending,
                    State::Off => !r.on,
                    _ => r.on && !r.pending,
                })
                .map(|r| Cell {
                    main: r.display.into(),
                    sub: r.name.into(),
                    state,
                })
                .collect();
            if cells.is_empty() {
                continue;
            }
            blocks.push(Block::Section {
                title: title.into(),
                en: en.into(),
                count: format!("{} 项", cells.len()),
            });
            blocks.push(Block::Grid(cells));
        }
    }

    blocks.push(Block::Section {
        title: "操作".into(),
        en: "ACTIONS".into(),
        count: String::new(),
    });
    blocks.push(Block::Facts(vec![
        Fact {
            label: "开关".into(),
            command: format!("{prefix}ctl on/off <插件…>"),
        },
        Fact {
            label: "配置".into(),
            command: format!("{prefix}ctl show <插件>"),
        },
    ]));
    blocks.push(Block::Callout {
        tone: Tone::Info,
        text: "「待重启」＝已开启，但生命周期钩子要等下次启动才跑起来。".into(),
    });

    doc(
        "CONTROL · STATUS",
        blocks,
        "状态以当前配置为准 · 标「待重启」的等下次启动",
        ("查看用法".into(), format!("{prefix}ctl")),
    )
}

/// 配置卡：`show` 与 `defaults` 共用；`path` 为空表示整份配置
pub fn config(
    prefix: &str,
    plugin: &str,
    path: &str,
    defaults: bool,
    body: &str,
    effect: &str,
) -> Card {
    let title = if defaults {
        "默认配置"
    } else {
        "当前配置"
    };
    // 整张表或表里的子表是 `键 = 值`，按 TOML 着色；单个值（数组、字符串、数字）
    // 按 JSON 着色——数组若走 TOML 会被当成表头。
    let table_like = body.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("[[") || (!line.starts_with(['"', '[']) && line.contains(" = "))
    });
    let blocks = vec![
        Block::Title {
            title: title.into(),
            pill: None,
            sub: if path.is_empty() {
                format!("{plugin} · 全部字段")
            } else {
                format!("{plugin} · {path}")
            },
        },
        Block::Code {
            lang: Some(if table_like { "toml" } else { "json" }),
            lines: body.lines().map(str::to_string).collect(),
        },
        Block::Callout {
            tone: Tone::Info,
            text: effect.into(),
        },
    ];
    doc(
        "CONTROL · CONFIG",
        blocks,
        "含 token / secret / password 的字段一律显示为「已隐藏」",
        (
            "修改某一项".into(),
            format!("{prefix}ctl set {plugin} <路径> <值>"),
        ),
    )
}

/// 把 `differences` 产出的一行拆成「键 / 默认值 / 当前值」。
///
/// 两种形状：`路径：旧 → 新`，与默认值里没有的 `路径 = 值（额外项）`。
/// 值是 TOML 写法，字符串带引号，所以拆 `→` 时跳过引号里的。
fn diff_row(line: &str) -> DiffRow {
    if let Some(rest) = line.strip_suffix("（额外项）")
        && let Some((key, value)) = rest.split_once(" = ")
    {
        return DiffRow {
            key: key.into(),
            old: None,
            new: value.into(),
        };
    }
    let Some((key, rest)) = line.split_once('：') else {
        return DiffRow {
            key: line.into(),
            old: None,
            new: String::new(),
        };
    };
    let mut quote = false;
    let mut escaped = false;
    let arrow = " → ";
    for (i, ch) in rest.char_indices() {
        if quote {
            match ch {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => quote = false,
                _ => {}
            }
        } else if ch == '"' {
            quote = true;
        } else if rest[i..].starts_with(arrow) {
            return DiffRow {
                key: key.into(),
                old: Some(rest[..i].into()),
                new: rest[i + arrow.len()..].into(),
            };
        }
    }
    DiffRow {
        key: key.into(),
        old: None,
        new: rest.into(),
    }
}

/// 差异卡：只列与默认值不同的项
pub fn diff(prefix: &str, plugin: &str, lines: &[String]) -> Card {
    let mut blocks = vec![Block::Title {
        title: "配置差异".into(),
        pill: None,
        sub: format!("{plugin} · 当前值与默认值的出入"),
    }];
    if lines.is_empty() {
        blocks.push(Block::Callout {
            tone: Tone::Empty,
            text: "配置与默认值完全一致，没有改动过的项。".into(),
        });
    } else {
        blocks.push(Block::Section {
            title: "改动项".into(),
            en: "CHANGED".into(),
            count: format!("{} 项", lines.len()),
        });
        blocks.push(Block::Diff(lines.iter().map(|l| diff_row(l)).collect()));
    }
    blocks.push(Block::Callout {
        tone: Tone::Info,
        text: format!(
            "恢复默认：{prefix}ctl reset {plugin} [路径] --confirm\n整插件重置会保留开关与 ctl.admins。"
        ),
    });
    doc(
        "CONTROL · DIFF",
        blocks,
        "每项先列默认值，再列当前值",
        (
            "查看默认配置".into(),
            format!("{prefix}ctl defaults {plugin}"),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::diff_row;

    /// 字符串值里自带 `→` 时不能把它当成「旧 → 新」的分界。
    #[test]
    fn diff_arrow_inside_a_quoted_value_is_not_the_separator() {
        let row = diff_row("persona.title：\"甲 → 乙\" → \"丙\"");
        assert_eq!(row.key, "persona.title");
        assert_eq!(row.old.as_deref(), Some("\"甲 → 乙\""));
        assert_eq!(row.new, "\"丙\"");
    }

    #[test]
    fn diff_extra_item_has_no_default() {
        let row = diff_row("extra.key = [1, 2]（额外项）");
        assert_eq!((row.key.as_str(), row.old, row.new.as_str()), ("extra.key", None, "[1, 2]"));
    }
}
