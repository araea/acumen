//! `/#` 智能体列表与 `/%` 模型列表。
//!
//! 数据只在这里整理一次，卡片与纯文本两种输出都从同一份结构里出：版式在
//! [`crate::render::web`] 的 `Block`，文本回退在 [`Listing::markdown`]。图里一套、
//! 文字另一套，迟早会对不上。
//!
//! 两张列表有一条共同的取舍：**只剩一个成员的分组不单独立标题**，并进最后一组
//! 「其他」，成员自己带上原本该挂在标题上的那一项（房间带模型、模型带厂商）。
//! 四十来个模型分在十几家厂商底下，其中一半厂商只有一个模型——每家一个标题，
//! 整张图就成了标题多过内容。

use super::types::Agent;
use super::utils::{escape_markdown_special, model_vendor, truncate_str};
use crate::render::web::{Block, Doc, ModelRow, RoomRow, Theme};
use std::collections::BTreeMap;

/// 卡片上简介最多留多少字；纯文本回退沿用从前的 20 字。
const CARD_DESC_CHARS: usize = 40;
const TEXT_DESC_CHARS: usize = 20;
/// 并进「其他」的那一组叫什么。
const ROOMS_OTHER: &str = "自建房间";

/// 一份可出图的列表。
pub(crate) enum Listing {
    Rooms(Rooms),
    Models(Models),
}

impl Listing {
    /// 卡片的文档模型，交给 [`crate::render::web::capture`] 出图。
    pub(crate) fn doc(&self) -> Doc {
        match self {
            Listing::Rooms(rooms) => rooms.doc(),
            Listing::Models(models) => models.doc(),
        }
    }

    /// 出图关闭或失败时发的文本；与图同一份数据。
    pub(crate) fn markdown(&self) -> String {
        match self {
            Listing::Rooms(rooms) => rooms.markdown(),
            Listing::Models(models) => models.markdown(),
        }
    }
}

// ---------------------------------------------------------------- 房间

struct Room {
    name: String,
    /// 卡片上的简介（较长）。
    desc: String,
    /// 纯文本里的简介（较短）。
    short: String,
    search: bool,
    /// 所在分组的标题没写模型时，这一行自己带。
    model: Option<String>,
}

struct RoomGroup {
    title: String,
    /// 分区标题右侧的模型；没有分区名时模型本身就是标题，这里为空。
    model: Option<String>,
    rooms: Vec<Room>,
}

pub(crate) struct Rooms {
    groups: Vec<RoomGroup>,
    total: usize,
    searching: usize,
}

impl Rooms {
    /// `search_default` 是全局联网开关：没单独设过的房间跟着它走。
    pub(crate) fn new(agents: &[Agent], search_default: bool) -> Self {
        // 分区优先于模型：内置预设那一批的共同点是「预设」而不是「跑哪个模型」，
        // 混进用户自建的同模型房间里就找不着了。带分区的排在前面（false < true）。
        let mut grouped: BTreeMap<(bool, String, String), Vec<&Agent>> = BTreeMap::new();
        for agent in agents {
            let section = agent.section.trim().to_string();
            grouped
                .entry((
                    section.is_empty(),
                    section,
                    super::logic::room_model_label(agent),
                ))
                .or_default()
                .push(agent);
        }

        let room = |agent: &Agent, model: Option<String>| {
            let source = if !agent.description.is_empty() {
                &agent.description
            } else if !agent.system_prompt.is_empty() {
                &agent.system_prompt
            } else {
                ""
            };
            let (desc, short) = if source.is_empty() {
                ("无描述".to_string(), "无描述".to_string())
            } else {
                (
                    truncate_str(source, CARD_DESC_CHARS),
                    truncate_str(source, TEXT_DESC_CHARS),
                )
            };
            Room {
                name: agent.name.clone(),
                desc,
                short,
                // 联网只对内置智能体房间有意义：中转站房间根本没有工具可调。
                search: agent.uses_agent() && agent.web_search(search_default),
                model,
            }
        };

        let mut groups = Vec::new();
        let mut loose: Vec<(String, &Agent)> = Vec::new();
        for ((no_section, section, label), mut members) in grouped {
            members.sort_by_key(|a| a.name.to_lowercase());
            if no_section && members.len() == 1 {
                loose.push((label, members[0]));
                continue;
            }
            let (title, model) = if no_section {
                (label, None)
            } else {
                (section, Some(label))
            };
            let rooms = members.iter().map(|a| room(a, None)).collect();
            groups.push(RoomGroup {
                title,
                model,
                rooms,
            });
        }
        if !loose.is_empty() {
            groups.push(RoomGroup {
                title: ROOMS_OTHER.to_string(),
                model: None,
                rooms: loose
                    .into_iter()
                    .map(|(label, agent)| room(agent, Some(label)))
                    .collect(),
            });
        }

        let searching = groups
            .iter()
            .flat_map(|g| g.rooms.iter())
            .filter(|r| r.search)
            .count();
        Rooms {
            total: agents.len(),
            searching,
            groups,
        }
    }

    fn doc(&self) -> Doc {
        let mut blocks = vec![Block::Title {
            title: "智能体列表".into(),
            pill: None,
            sub: format!("共 {} 个智能体 · {} 个分组", self.total, self.groups.len()),
        }];
        for group in &self.groups {
            let count = format!("{} 个", group.rooms.len());
            blocks.push(match &group.model {
                Some(model) => Block::SectionChip {
                    title: group.title.clone(),
                    chip: model.clone(),
                    count,
                },
                None => Block::Section {
                    title: group.title.clone(),
                    en: String::new(),
                    count,
                },
            });
            blocks.push(Block::Rooms(
                group
                    .rooms
                    .iter()
                    .map(|r| RoomRow {
                        name: r.name.clone(),
                        desc: r.desc.clone(),
                        search: r.search,
                        model: r.model.clone(),
                    })
                    .collect(),
            ));
        }
        Doc {
            theme: Theme::Help,
            width: WIDTH,
            kicker: "ACUMEN · ROOMS".into(),
            blocks,
            foot: if self.searching > 0 {
                "标「联网」的智能体会按需上网查资料，其余只凭模型自己的知识".into()
            } else {
                "智能体名字就是触发词，后面跟内容就是对话".into()
            },
            hint: ("与智能体对话".into(), "<名称> <内容>".into()),
        }
    }

    fn markdown(&self) -> String {
        let mut parts = Vec::new();
        // 序号按展示顺序从 1 编到 N：Markdown 有序列表只认首个编号、其余逐条递增，
        // 拿房间在配置里的下标当序号，分组后会出现「48 个房间标到七八十」。
        let mut seq = 0usize;
        for group in &self.groups {
            let title = match &group.model {
                Some(model) => format!("{} · {model}", group.title),
                None => group.title.clone(),
            };
            parts.push(format!(
                "## {}（{}）\n",
                escape_markdown_special(&title),
                group.rooms.len()
            ));
            for room in &group.rooms {
                seq += 1;
                let mut desc = room.short.clone();
                if room.search {
                    desc = format!("联网 · {desc}");
                }
                if let Some(model) = &room.model {
                    desc = format!("{desc} · {model}");
                }
                parts.push(format!(
                    "{seq}. **{}** — {}\n",
                    escape_markdown_special(&room.name),
                    escape_markdown_special(&desc)
                ));
            }
        }
        parts.join("\n")
    }
}

// ---------------------------------------------------------------- 模型

struct ModelGroup {
    title: String,
    rows: Vec<ModelRow>,
}

pub(crate) struct Models {
    groups: Vec<ModelGroup>,
    total: usize,
    vendors: usize,
}

impl Models {
    /// `models` 是中转站拉回的列表（序号 = 位置 + 1，指令里写序号就是写这个模型）；
    /// `agents` 用来数每个模型被几个智能体用着。
    pub(crate) fn new(models: &[String], agents: &[Agent], default_model: &str) -> Self {
        let mut used: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for agent in agents {
            *used.entry(agent.model.as_str()).or_insert(0) += 1;
        }

        // 分区顺序取各组首次出现的次序：列表本身已按 id 排序，同一厂商的条目天然连在一起，
        // 序号沿着图往下也大体递增。
        let mut order: Vec<&'static str> = Vec::new();
        let mut by_vendor: BTreeMap<&'static str, Vec<ModelRow>> = BTreeMap::new();
        for (i, name) in models.iter().enumerate() {
            let vendor = model_vendor(name);
            if !by_vendor.contains_key(vendor) {
                order.push(vendor);
            }
            by_vendor.entry(vendor).or_default().push(ModelRow {
                index: i + 1,
                name: name.clone(),
                vendor: None,
                used: used.get(name.as_str()).copied().unwrap_or(0),
                default: !default_model.is_empty() && name == default_model,
            });
        }
        let vendors = order.len();

        let mut groups = Vec::new();
        let mut rest: Vec<ModelRow> = Vec::new();
        let singles = order
            .iter()
            .filter(|v| by_vendor[**v].len() == 1)
            .count();
        let has_multi = order.len() > singles;
        for vendor in order {
            let rows = by_vendor.remove(vendor).unwrap_or_default();
            // 认不出厂商的（「其他」）自己够多就单列，不够就一起并进去。
            if rows.len() == 1 && singles >= 2 {
                rest.extend(rows.into_iter().map(|mut row| {
                    row.vendor = (vendor != "其他").then(|| vendor.to_string());
                    row
                }));
            } else {
                groups.push(ModelGroup {
                    title: vendor.to_string(),
                    rows,
                });
            }
        }
        if !rest.is_empty() {
            rest.sort_by_key(|row| row.index);
            groups.push(ModelGroup {
                title: if has_multi { "其他厂商" } else { "全部模型" }.to_string(),
                rows: rest,
            });
        }
        Models {
            groups,
            total: models.len(),
            vendors,
        }
    }

    fn doc(&self) -> Doc {
        let mut blocks = vec![Block::Title {
            title: "模型列表".into(),
            pill: None,
            sub: format!("共 {} 个模型 · {} 个厂商", self.total, self.vendors),
        }];
        for group in &self.groups {
            blocks.push(Block::Section {
                title: group.title.clone(),
                en: String::new(),
                count: format!("{} 个", group.rows.len()),
            });
            blocks.push(Block::Models(
                group
                    .rows
                    .iter()
                    .map(|r| ModelRow {
                        index: r.index,
                        name: r.name.clone(),
                        vendor: r.vendor.clone(),
                        used: r.used,
                        default: r.default,
                    })
                    .collect(),
            ));
        }
        Doc {
            theme: Theme::Help,
            width: WIDTH,
            kicker: "ACUMEN · MODELS".into(),
            blocks,
            foot: "序号随模型列表刷新而变，长期使用请写完整的模型名".into(),
            hint: ("用序号换模型".into(), "<名称>%<序号>".into()),
        }
    }

    fn markdown(&self) -> String {
        let mut out = String::new();
        for group in &self.groups {
            out.push_str(&format!("## {}\n\n", escape_markdown_special(&group.title)));
            for row in &group.rows {
                let mut notes = Vec::new();
                if row.default {
                    notes.push("默认".to_string());
                }
                if let Some(vendor) = &row.vendor {
                    notes.push(vendor.clone());
                }
                if row.used > 0 {
                    notes.push(format!("{} 个智能体使用", row.used));
                }
                let tail = if notes.is_empty() {
                    String::new()
                } else {
                    format!("（{}）", notes.join("，"))
                };
                out.push_str(&format!(
                    "{}. {}{tail}\n",
                    row.index,
                    escape_markdown_special(&row.name)
                ));
            }
            out.push('\n');
        }
        out
    }
}

/// 成图宽度：卡面 520 加两侧各 20 的相纸，与手册卡、控制卡、智能回复卡同宽。
pub(crate) const WIDTH: f32 = 560.0;

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str, model: &str, section: &str) -> Agent {
        let mut agent = Agent::new(name, model, "", "说明");
        agent.section = section.into();
        agent
    }

    /// 序号是指令里能直接写的（`助手%3`），所以并组之后每个模型仍得带着它在列表里的真位置。
    /// 只剩一个成员的厂商并进「其他厂商」，自己带上厂商名；认不出厂商的不挂标签。
    #[test]
    fn single_model_vendors_merge_but_keep_their_list_position() {
        let ids: Vec<String> = ["claude-a", "claude-b", "gpt-5", "kimi-k3", "mystery"]
            .map(String::from)
            .into();
        let models = Models::new(&ids, &[], "kimi-k3");
        let titles: Vec<_> = models.groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["Anthropic", "其他厂商"]);
        let rest = &models.groups[1].rows;
        let seen: Vec<_> = rest
            .iter()
            .map(|r| (r.index, r.vendor.as_deref(), r.default))
            .collect();
        assert_eq!(
            seen,
            [
                (3, Some("OpenAI"), false),
                (4, Some("Moonshot"), true),
                (5, None, false)
            ]
        );
    }

    /// 带分区的各自成组、标题挂模型；没分区的同模型房间按模型成组，落单的并进「自建房间」并自带模型。
    #[test]
    fn rooms_group_by_section_then_model_and_fold_singletons() {
        let agents = [
            agent("画·一", "img", "预设"),
            agent("画·二", "img", "预设"),
            agent("甲", "m1", ""),
            agent("乙", "m1", ""),
            agent("丙", "m2", ""),
        ];
        let rooms = Rooms::new(&agents, false);
        let shape: Vec<_> = rooms
            .groups
            .iter()
            .map(|g| (g.title.as_str(), g.model.as_deref(), g.rooms.len()))
            .collect();
        assert_eq!(
            shape,
            [
                ("预设", Some("img"), 2),
                ("m1", None, 2),
                (ROOMS_OTHER, None, 1)
            ]
        );
        assert_eq!(rooms.groups[2].rooms[0].model.as_deref(), Some("m2"));
    }

    /// 卡片版式把名字、简介当文本转义；回退文本里则是 Markdown，序号从 1 数到底。
    #[test]
    fn text_fallback_numbers_rooms_in_display_order() {
        let agents = [agent("b", "m", "区"), agent("a", "m", "区")];
        let text = Rooms::new(&agents, false).markdown();
        let first = text.find("1. **a**").expect("按名字排序后 a 在前");
        assert!(first < text.find("2. **b**").unwrap());
    }
}
