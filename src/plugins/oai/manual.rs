//! `oai` 的帮助卡：接入状态、工具三档与全部符号指令。
//!
//! 从前这张帮助是一段 Markdown，交给回复卡去排——那是给模型写的长文准备的版式，
//! 状态、档位、指令全挤成一样的段落与列表。这里改用手册卡那套文档模型（[`crate::render::web`]）：
//! 页首一眼看到「能不能用、默认模型是谁、联网走哪儿」，接着是三档工具的刻度，再是按场景分组的指令，
//! 照打的字与 `<占位>` 分两种颜色。
//!
//! 卡片与纯文本回退从同一份数据出（[`GROUPS`] 与 [`Overview`]），图里一套、字里另一套迟早对不上。
//! 出图走 [`crate::render::web::capture`] 的内容缓存：状态没变就直接发上次那张。

use super::setup::Overview;
use super::types::Tools;
use crate::render::web::{Block, Cmd, Doc, Fact, Theme, Tier, Tone};

/// 一组指令：标题、英文代号，以及（指令、说明）。
///
/// 指令里要原样敲的字写在外面，要换成自己内容的占位符用 `{…}`、可省的用 `[…] `。
/// 这一组命令本身就是 `oai` 打头的两条指令名，不再另加指令前缀——前缀是可省的装饰，
/// 图上多一个 `/` 反而让人以为必须带。
struct Group {
    title: &'static str,
    en: &'static str,
    cmds: &'static [(&'static str, &'static str)],
}

const GROUPS: &[Group] = &[
    Group {
        title: "对话",
        en: "CHAT",
        cmds: &[
            ("{房间} {内容}", "和房间对话；引用也算"),
            ("~{房间} {内容}", "一次性，说过就忘"),
            ("\"{房间} {内容}", "只回文字，不排成图"),
            ("&{房间} {内容}", "只用自己的私有历史"),
            ("{房间}~", "重新生成上一条"),
            ("{房间}!", "让它停下来"),
        ],
    },
    Group {
        title: "房间",
        en: "ROOMS",
        cmds: &[
            ("##{名称} [模型] [提示词]", "新建或更新；后两项可省"),
            ("{房间}%{模型}", "换模型；%默认 回跟随"),
            ("{房间}${提示词}", "改提示词；只写 $ 清空"),
            ("{房间}~={新名}", "重命名；~# 复制一间"),
            ("{房间}:{描述}", "写描述；##: 批量生成"),
            ("-#{名称}", "删除房间"),
            ("/#", "房间列表；/% 模型表"),
        ],
    },
    Group {
        title: "模型与接入",
        en: "MODELS",
        cmds: &[
            ("oai {供应商} {密钥}", "接上一家，存好撤回消息"),
            ("oai 用 {模型}", "换默认，跟随的一起换"),
            ("oai {名字} {地址} {密钥}", "接自己的中转站"),
            ("oai {名字} 清除", "清掉一家的密钥"),
        ],
    },
    Group {
        title: "历史",
        en: "HISTORY",
        cmds: &[
            ("{房间}/*", "全部；/1-5 看一段"),
            ("{房间}_*", "导出成 .txt"),
            ("{房间}'{序号} {内容}", "改第几条"),
            ("{房间}-{序号}", "删第几条；-* 清空"),
        ],
    },
];

/// 画图那一段只有几句用法，没有可列的指令。
const MEDIA: &[&str] = &[
    "建一间用图像模型的房间（##画画 gpt-image-2.5），之后「画画 {提示词}」就出图",
    "发图、引用图片，或在房间名后 @某人，那张图当垫图；可加 --size、--quality",
    "MJ 四宫格引用后回 1–4 放大；写歌、拍片同理，模型命中 [oai] 的关键字就行",
];

/// 档位名旁「谁能切」的说法。
fn who(tools: Tools) -> &'static str {
    match tools {
        Tools::Full => "仅号主与管理员",
        _ => "任何人",
    }
}

fn level(tools: Tools) -> u8 {
    match tools {
        Tools::Off => 1,
        Tools::Web => 2,
        Tools::Full => 3,
    }
}

/// 「还差什么」那一句；能用时为 `None`。
///
/// 例子里的指令一律不写前缀：这条卡片讲的正是前缀可以省。
fn gap(overview: &Overview) -> Option<String> {
    if overview.ready {
        return None;
    }
    Some(match &overview.switch_to {
        Some(name) => format!(
            "默认模型 {} 这家还没接。已经接上的 {name} 能用：发 oai 用 {name}",
            overview.default_model
        ),
        None => format!(
            "还没接模型供应商：号主发 oai deepseek 密钥，验过就能用（默认模型 {}）",
            overview.default_model
        ),
    })
}

fn providers_line(overview: &Overview) -> String {
    if overview.providers.is_empty() {
        return "一家都没配".to_string();
    }
    overview
        .providers
        .iter()
        .map(|(name, state)| if state == "已接上" { name.clone() } else { format!("{name}（{state}）") })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// 房间数与其中挂着工具（联网或全量）的几间。
pub(super) struct Rooms {
    pub(super) total: usize,
    pub(super) tooled: usize,
}

/// 帮助卡的文档模型。
pub(super) fn doc(overview: &Overview, rooms: &Rooms) -> Doc {
    let mut blocks = vec![Block::Title {
        title: "智能对话".into(),
        pill: Some(if overview.ready { ("可以用了".into(), true) } else { ("还没接上".into(), false) }),
        // 三个数并成副题里的一行，不再各占一格：省下的高度全给下面的指令表。
        sub: format!(
            "{} 间房 · {} 间挂着工具 · {} 家供应商已接",
            rooms.total,
            rooms.tooled,
            overview.connected
        ),
    }];
    let mut facts = vec![Fact { label: "默认模型".into(), command: overview.default_model.clone() }];
    if let Some(chain) = overview.search.as_ref().filter(|chain| !chain.is_empty()) {
        facts.push(Fact { label: "联网".into(), command: chain.join(" → ") });
    }
    facts.push(Fact { label: "供应商".into(), command: providers_line(overview) });
    if !overview.relay.is_empty() {
        facts.push(Fact {
            label: "默认接口".into(),
            command: if overview.relay_key { overview.relay.clone() } else { format!("{}（缺密钥）", overview.relay) },
        });
    }
    blocks.push(Block::Facts(facts));
    if let Some(text) = gap(overview) {
        blocks.push(Block::Callout { tone: Tone::Info, text });
    }

    blocks.push(Block::Section {
        title: "工具三档".into(),
        en: "TOOLS".into(),
        count: format!("{} 档", Tools::ALL.len()),
    });
    blocks.push(Block::Tiers(
        Tools::ALL
            .iter()
            .map(|&tools| Tier {
                level: level(tools),
                name: tools.label().into(),
                summary: tools.summary().into(),
                command: format!("{{房间}}?{}", tools.label()),
                who: who(tools).into(),
                default: tools == Tools::default(),
            })
            .collect(),
    ));

    for group in GROUPS {
        blocks.push(Block::Section {
            title: group.title.into(),
            en: group.en.into(),
            count: format!("{} 条", group.cmds.len()),
        });
        blocks.push(Block::Cmds(
            group
                .cmds
                .iter()
                .map(|(cmd, note)| Cmd {
                    // 指令前缀可省，图上不画：oai 开头的那组本身就是指令名。
                    prefix: String::new(),
                    cmd: (*cmd).into(),
                    note: (*note).into(),
                    aliases: Vec::new(),
                })
                .collect(),
        ));
    }
    blocks.push(Block::Section {
        title: "画图".into(),
        en: "MEDIA".into(),
        count: format!("{} 条", MEDIA.len()),
    });
    blocks.push(Block::Notes(MEDIA.iter().map(|line| (*line).to_string()).collect()));
    blocks.push(Block::Callout {
        tone: Tone::Info,
        text: "慎用：-* 清空所有房间的公有历史，-*! 连私有历史一起清（都要再发一次确认）".into(),
    });

    Doc {
        theme: Theme::Help,
        width: super::lists::WIDTH,
        kicker: "ACUMEN · OAI".into(),
        blocks,
        foot: format!(
            "花括号里换成自己的内容，方括号里可省；指令前缀可省，~ \" & 可以叠加。认得的供应商：{}",
            super::setup::known_list()
        ),
        hint: ("和房间对话".into(), "{房间} {内容}".into()),
    }
}

/// 出图关掉或失败时发的文本；与卡片同一份数据。
pub(super) fn markdown(overview: &Overview, rooms: &Rooms) -> String {
    let mut out = String::from("## 接入状态\n");
    out.push_str(&format!(
        "- 默认模型：{}（{}）\n",
        overview.default_model,
        if overview.ready { "可以用了" } else { "这家还没接" }
    ));
    match &overview.search {
        Some(chain) if !chain.is_empty() => out.push_str(&format!("- 联网：{}\n", chain.join(" → "))),
        Some(_) => out.push_str("- 联网：没有可用的后端\n"),
        None => out.push_str("- 联网：已关闭\n"),
    }
    out.push_str(&format!("- 供应商：{}\n", providers_line(overview)));
    if !overview.relay.is_empty() {
        out.push_str(&format!("- 默认接口：{}{}\n", overview.relay, if overview.relay_key { "" } else { "（缺密钥）" }));
    }
    out.push_str(&format!("- 房间：{} 间，{} 间挂着工具\n", rooms.total, rooms.tooled));
    if let Some(text) = gap(overview) {
        out.push_str(&format!("\n💡 {text}\n"));
    }

    out.push_str("\n## 工具三档\n");
    for tools in Tools::ALL {
        out.push_str(&format!(
            "- `{{房间}}?{}` {}（{}{}）\n",
            tools.label(),
            tools.summary(),
            who(tools),
            if tools == Tools::default() { "；新房间默认" } else { "" }
        ));
    }
    for group in GROUPS {
        out.push_str(&format!("\n## {}\n", group.title));
        for (cmd, note) in group.cmds {
            out.push_str(&format!("- `{cmd}` {note}\n"));
        }
    }
    out.push_str("\n## 画图\n");
    for line in MEDIA {
        out.push_str(&format!("- {line}\n"));
    }
    out.push_str("\n慎用：`-*` 清空所有房间的公有历史，`-*!` 连私有历史一起清。\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::web;

    fn sample(ready: bool) -> Overview {
        Overview {
            default_model: "deepseek/deepseek-flash".into(),
            ready,
            switch_to: (!ready).then(|| "mimo".to_string()),
            relay: "https://api.example.com/v1".into(),
            relay_key: true,
            providers: vec![("DeepSeek".into(), "已接上".into()), ("小米 MiMo".into(), "缺密钥".into())],
            connected: 1,
            search: Some(vec!["deepseek".into(), "bing".into(), "duckduckgo".into()]),
        }
    }

    #[test]
    fn card_and_text_list_every_tier_and_group() {
        let rooms = Rooms { total: 4, tooled: 1 };
        let html = web::html(&doc(&sample(true), &rooms));
        let text = markdown(&sample(true), &rooms);
        for tools in Tools::ALL {
            assert!(html.contains(tools.label()) && text.contains(tools.label()));
        }
        for group in GROUPS {
            assert!(html.contains(group.title) && text.contains(group.title));
        }
        assert!(text.contains("`oai 用 {模型}`"));
        assert!(markdown(&sample(false), &Rooms { total: 0, tooled: 0 }).contains("oai 用 mimo"));
    }

    /// 出一份 HTML 看版式：`ACUMEN_DUMP=目录 cargo test --release manual::tests::dump -- --ignored`。
    #[test]
    #[ignore]
    fn dump() {
        let dir = std::env::var("ACUMEN_DUMP").expect("ACUMEN_DUMP");
        let rooms = Rooms { total: 4, tooled: 1 };
        std::fs::write(format!("{dir}/oai-help.html"), web::html(&doc(&sample(true), &rooms))).unwrap();
        let empty = Rooms { total: 0, tooled: 0 };
        std::fs::write(format!("{dir}/oai-help-gap.html"), web::html(&doc(&sample(false), &empty))).unwrap();
    }
}
