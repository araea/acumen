//! 把素材交给模型，换回一份用户画像；模型不接时退回统计直出的标签。
//!
//! 用户画像是从行为数据里抽象出来的**标签化模型**：一组能被数据撑住的标签，加一段把
//! 它们串起来的话。这一层只认三件事——**输出是一个 JSON 对象**、**引语必须是原话**、
//! **标签分得清层级**。第一条靠宽松解析（模型爱在 JSON 外面裹一句「好的」或者一层
//! 代码块），第二条靠归一化比对，对不上就丢掉；第三条落在 [`Tag`] 的形状上：维度只有
//! 四个，层级只有三层，认不出的那一维直接不印——版面宁可少一块，也不印一条没有出处的标签。

use super::collect::Material;
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt::Write as _;

/// 报告主色。色板是固定的，模型只能从中挑，省得它挑出一组刺眼或看不清的组合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accent {
    Amber,
    Rose,
    Mint,
    Indigo,
    Violet,
    Teal,
}

impl Accent {
    pub const ALL: [Accent; 6] = [
        Accent::Amber,
        Accent::Rose,
        Accent::Mint,
        Accent::Indigo,
        Accent::Violet,
        Accent::Teal,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Accent::Amber => "amber",
            Accent::Rose => "rose",
            Accent::Mint => "mint",
            Accent::Indigo => "indigo",
            Accent::Violet => "violet",
            Accent::Teal => "teal",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|accent| accent.name() == name)
    }

    /// 兜底选色：同一个用户每次都得到同一种颜色，换人换色。
    pub fn pick(seed: i64) -> Self {
        Self::ALL[(seed.unsigned_abs() as usize) % Self::ALL.len()]
    }

    /// 主色（深色主题用亮一档的色，浅色主题用深一档的）。
    /// 色值都往灰里压了一档，落在纸色上不跳，配得上这份报告的语速。
    pub fn hex(self, dark: bool) -> &'static str {
        match (self, dark) {
            (Accent::Amber, false) => "#8A5A1E",
            (Accent::Amber, true) => "#C9A063",
            (Accent::Rose, false) => "#9E2F4C",
            (Accent::Rose, true) => "#D98BA1",
            (Accent::Mint, false) => "#1F6F5C",
            (Accent::Mint, true) => "#7FBFA8",
            (Accent::Indigo, false) => "#3E4E9E",
            (Accent::Indigo, true) => "#9AA6DD",
            (Accent::Violet, false) => "#5F3A96",
            (Accent::Violet, true) => "#B49BDD",
            (Accent::Teal, false) => "#14606E",
            (Accent::Teal, true) => "#79B8C2",
        }
    }

    /// 同色的 `r,g,b` 字面量，供 CSS 里调透明度用，省得写死多份色值。
    pub fn rgb(self, dark: bool) -> &'static str {
        match (self, dark) {
            (Accent::Amber, false) => "138,90,30",
            (Accent::Amber, true) => "201,160,99",
            (Accent::Rose, false) => "158,47,76",
            (Accent::Rose, true) => "217,139,161",
            (Accent::Mint, false) => "31,111,92",
            (Accent::Mint, true) => "127,191,168",
            (Accent::Indigo, false) => "62,78,158",
            (Accent::Indigo, true) => "154,166,221",
            (Accent::Violet, false) => "95,58,150",
            (Accent::Violet, true) => "180,155,221",
            (Accent::Teal, false) => "20,96,110",
            (Accent::Teal, true) => "121,184,194",
        }
    }
}

/// 标签的四个维度。这一层是画像的骨架，**不许模型自创**——多出来的维度一律不印。
pub const DIMENSIONS: [(&str, &str); 4] = [
    ("活跃", "ACTIVITY"),
    ("内容", "CONTENT"),
    ("交互", "INTERACTION"),
    ("表达", "EXPRESSION"),
];

/// 标签的三层抽象。事实是观测即得，统计是按阈值归纳，推断是从语义里读出来的；
/// 三层从硬到软，读者对它们的信任度该跟着往下走，版式上也是这么排的。
pub const LAYERS: [(&str, &str); 3] = [
    ("事实", "OBSERVED"),
    ("统计", "DERIVED"),
    ("推断", "INFERRED"),
];

/// 认一个维度名。模型常写成「活跃度」「内容偏好」这类，含关键字就算它。
fn dimension_of(name: &str) -> Option<&'static str> {
    const TABLE: [(&str, &[&str]); 4] = [
        ("活跃", &["活跃", "activity", "active"]),
        ("内容", &["内容", "话题", "兴趣", "content"]),
        (
            "交互",
            &["交互", "互动", "社交", "关系", "interaction", "social"],
        ),
        (
            "表达",
            &["表达", "风格", "语言", "媒介", "expression", "style"],
        ),
    ];
    let name = name.trim().to_ascii_lowercase();
    TABLE
        .iter()
        .find_map(|(canon, keys)| keys.iter().any(|key| name.contains(key)).then_some(*canon))
}

/// 认一层抽象。认不出来的一律算**推断**——没标「观测即得」的，本来就不该按事实读。
fn layer_of(name: &str) -> &'static str {
    const TABLE: [(&str, &[&str]); 3] = [
        ("事实", &["事实", "观测", "fact", "observ"]),
        ("统计", &["统计", "模型", "阈值", "归纳", "derived", "statis"]),
        ("推断", &["推断", "预测", "语义", "infer", "predict"]),
    ];
    let name = name.trim().to_ascii_lowercase();
    TABLE
        .iter()
        .find_map(|(canon, keys)| keys.iter().any(|key| name.contains(key)).then_some(*canon))
        .unwrap_or("推断")
}

/// 一条标签。`dimension` 与 `layer` 由模型写，收口时归一到白名单里的取值。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Tag {
    #[serde(default, alias = "维度", alias = "dim")]
    pub dimension: String,
    #[serde(default, alias = "类别", alias = "level", alias = "tier")]
    pub layer: String,
    #[serde(default, alias = "名称", alias = "标签", alias = "tag")]
    pub label: String,
    #[serde(default, alias = "证据", alias = "basis")]
    pub evidence: String,
}

impl Tag {
    fn new(dimension: &str, layer: &str, label: String, evidence: String) -> Self {
        Self {
            dimension: dimension.to_string(),
            layer: layer.to_string(),
            label,
            evidence,
        }
    }

    /// 归一之后的维度。认不出的这一条会被丢掉。
    pub fn dim(&self) -> Option<&'static str> {
        dimension_of(&self.dimension)
    }

    /// 归一之后的层级。
    pub fn tier(&self) -> &'static str {
        layer_of(&self.layer)
    }

    /// 层级对应的样式名，卡片按它给徽章上色。
    pub fn tier_class(&self) -> &'static str {
        tier_class(self.tier())
    }
}

/// 层级对应的样式名，卡片的图例与徽章都用它。
pub fn tier_class(tier: &str) -> &'static str {
    match tier {
        "事实" => "observed",
        "统计" => "derived",
        _ => "inferred",
    }
}

/// 综述里的一段。`kind` 决定它是自己的话还是他的话。
///
/// 段落与引语排在同一列里，是为了让引语落在该落的地方：写完一段判断，紧接着把那句原话
/// 放上来当证，再接着往下说。这样报告是一篇，不是「正文一段 + 文末三条引语」。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Passage {
    /// `text` 是综述，`quote` 是引语；认不出来的一律当综述。
    #[serde(default)]
    pub kind: String,
    /// 综述的正文；引语时为空。
    #[serde(default)]
    pub body: String,
    /// 引语的原话，逐字出自样本。
    #[serde(default)]
    pub text: String,
    /// 为什么把这句话放在这里。
    #[serde(default)]
    pub note: String,
}

impl Passage {
    /// 这一段的 kind 认不认得出是引语。认不出的按综述处理。
    pub fn is_quote(&self) -> bool {
        const WORDS: [&str; 4] = ["quote", "引语", "引文", "他的话"];
        let kind = self.kind.trim().to_ascii_lowercase();
        WORDS.iter().any(|word| kind.contains(word))
    }
}

/// 一份可以交给模板渲染的画像。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Persona {
    /// 综合标签：一句话把他归成一类，2—7 字。
    #[serde(default, alias = "称谓", alias = "name")]
    pub title: String,
    /// 一句话概括。
    #[serde(default, alias = "题记", alias = "一句话", alias = "summary")]
    pub note: String,
    /// 四个维度下的标签，收口后按维度分组渲染。
    #[serde(default, alias = "标签", alias = "labels")]
    pub tags: Vec<Tag>,
    /// 综述：段落与引语按序排列。
    #[serde(default, alias = "综述", alias = "passages")]
    pub profile: Vec<Passage>,
    #[serde(default)]
    pub accent: String,
    /// 这份画像是模型写的，还是从统计量直接拼出来的。
    #[serde(skip)]
    pub estimated: bool,
}

/// 各字段的字数上限。模型偶尔会无视字数要求，这里统一收口，
/// 免得一个超长字段把整张卡的版面顶乱。
mod limit {
    pub const TITLE: usize = 9;
    pub const NOTE: usize = 30;
    pub const LABEL: usize = 12;
    pub const EVIDENCE: usize = 44;
    /// 一个维度最多印几条。四个维度都得有位置，不能由着一个维度铺满。
    pub const TAGS_PER_DIM: usize = 4;
    pub const MAX_TAGS: usize = 16;
    pub const PASSAGE: usize = 220;
    pub const QUOTE: usize = 90;
    pub const QUOTE_NOTE: usize = 24;
    pub const MAX_PASSAGES: usize = 6;
}

/// 截到上限并补省略号。省略号前面不留空白，否则会变成「手机 root …」这种断口。
fn clip(value: &str, max: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max).collect();
    while out.ends_with(char::is_whitespace) {
        out.pop();
    }
    out.push('…');
    out
}

/// 逐字比对用的归一化：只留字母数字与汉字，去掉标点、空白和大小写差异。
fn fingerprint(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_whitespace() && !is_punctuation(*ch))
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn is_punctuation(ch: char) -> bool {
    const EXTRA: &str = "，。！？；：、“”‘’（）《》〈〉【】〔〕…—～·「」『』／＼｜＋－×÷＝＜＞";
    ch.is_ascii_punctuation() || EXTRA.contains(ch)
}

/// 宽松解析模型输出：取第一个花括号到最后一个花括号之间的内容。
///
/// 开 JSON 模式能把可用模型限制在支持该参数的那几个上，为了一个字段的整洁
/// 换掉整个模型池不划算，取花括号块就够了。
pub fn parse(raw: &str) -> anyhow::Result<Persona> {
    let start = raw
        .find('{')
        .ok_or_else(|| anyhow::anyhow!("模型没有返回 JSON：{}", clip(raw, 120)))?;
    let end = raw
        .rfind('}')
        .ok_or_else(|| anyhow::anyhow!("模型返回的 JSON 不完整：{}", clip(raw, 120)))?;
    if end <= start {
        anyhow::bail!("模型返回的 JSON 不完整：{}", clip(raw, 120));
    }
    let persona: Persona = serde_json::from_str(&raw[start..=end])
        .map_err(|error| anyhow::anyhow!("模型返回的 JSON 解析失败：{error}"))?;
    Ok(persona)
}

impl Persona {
    /// 收口：字数、条数、维度与层级，以及引语必须出自样本。
    pub fn sanitize(mut self, material: &Material) -> Self {
        self.title = clip(&self.title, limit::TITLE);
        self.note = clip(&self.note, limit::NOTE);

        // 标签：维度认不出的丢掉，一个维度超额的丢掉，第一条必须是「事实」——
        // 没有观测打底的推断不配印在画像上。
        let mut used: HashMap<&'static str, usize> = HashMap::new();
        let tags = std::mem::take(&mut self.tags);
        self.tags = tags
            .into_iter()
            .filter_map(|tag| {
                let dimension = tag.dim()?;
                let label = clip(&tag.label, limit::LABEL);
                if label.is_empty() {
                    return None;
                }
                let slot = used.entry(dimension).or_insert(0);
                if *slot >= limit::TAGS_PER_DIM {
                    return None;
                }
                *slot += 1;
                Some(Tag::new(
                    dimension,
                    tag.tier(),
                    label,
                    clip(&tag.evidence, limit::EVIDENCE),
                ))
            })
            .take(limit::MAX_TAGS)
            .collect();

        let samples: Vec<String> = material.samples.iter().map(|s| fingerprint(s)).collect();
        let passages = std::mem::take(&mut self.profile);
        self.profile = passages
            .into_iter()
            .filter_map(|passage| {
                if passage.is_quote() {
                    let finger = fingerprint(&passage.text);
                    // 引语要像一句话，也要真的在样本里。
                    if finger.chars().count() < 4
                        || !samples.iter().any(|sample| sample.contains(&finger))
                    {
                        return None;
                    }
                    return Some(Passage {
                        kind: "quote".to_string(),
                        body: String::new(),
                        text: clip(&passage.text, limit::QUOTE),
                        note: clip(&passage.note, limit::QUOTE_NOTE),
                    });
                }
                let body = clip(&passage.body, limit::PASSAGE);
                if body.is_empty() {
                    return None;
                }
                Some(Passage {
                    kind: "text".to_string(),
                    body,
                    text: String::new(),
                    note: String::new(),
                })
            })
            .take(limit::MAX_PASSAGES)
            .collect();

        self
    }

    /// 主色：模型挑的在色板里就用它，否则按用户号定色。
    pub fn accent(&self, seed: i64) -> Accent {
        Accent::from_name(&self.accent).unwrap_or_else(|| Accent::pick(seed))
    }

    /// 模型完全没接上时的兜底。
    ///
    /// 画像的骨头是数据，不是模型：统计量本来就在手里，照它把标签打出来，仍然是一份
    /// 用户画像，缺的只是推断层与那段综述。版面上会标出「标签由统计直出」。
    pub fn from_stats(material: &Material) -> Self {
        let mut tags = vec![
            Tag::new(
                "活跃",
                "事实",
                format!("发言 {} 条", material.total),
                format!(
                    "覆盖 {} 天，活跃 {} 天",
                    material.span_days(),
                    material.active_days
                ),
            ),
            Tag::new(
                "活跃",
                "统计",
                format!("日均 {:.1} 条", material.per_day()),
                format!(
                    "{}最密；夜间（0—6 点）占 {}",
                    hour_label(material.peak_hour()),
                    percent(material.night_ratio())
                ),
            ),
            Tag::new(
                "交互",
                "事实",
                format!("引用 {} 次", material.kinds.reply),
                format!("@ 别人 {} 次", material.kinds.at),
            ),
            Tag::new(
                "表达",
                "事实",
                format!("平均 {:.1} 字", material.avg_len()),
                format!("单条最长 {} 字", material.longest),
            ),
        ];
        // 媒介这一条是「统计」层的样子：一条观测加一条归出来的类，阈值一并写上。
        tags.push(Tag::new(
            "表达",
            "统计",
            if material.media_ratio() >= 0.5 {
                "图与表情过半".to_string()
            } else {
                "以文字为主".to_string()
            },
            format!(
                "图与表情占 {}；纯文字 {} 条、图片 {}、表情包 {}、小表情 {}、语音 {}",
                percent(material.media_ratio()),
                material.kinds.text,
                material.kinds.image,
                material.kinds.anim_emoji,
                material.kinds.face,
                material.kinds.voice
            ),
        ));
        if !material.words.is_empty() {
            let top: Vec<String> = material
                .words
                .iter()
                .take(3)
                .map(|(word, count)| format!("{word}×{count}"))
                .collect();
            tags.push(Tag::new(
                "内容",
                "事实",
                format!("常提 {}", top.join("、")),
                "同一批记录里的高频词，按次数排".to_string(),
            ));
        }

        let mut profile = vec![Passage {
            kind: "text".to_string(),
            body: format!(
                "这一次模型没有接上，所以没有推断出来的标签，也没有综述。\
                 下面这些标签是照着他本人在群里的统计量直接排的：{} 条发言，\
                 覆盖 {} 天，单条平均 {:.1} 字，平均每天 {:.1} 条。",
                material.total,
                material.span_days(),
                material.avg_len(),
                material.per_day(),
            ),
            ..Default::default()
        }];
        // 最长的那句原话当引语：它必然出自样本，用来撑住版面最省事。
        if let Some(text) = material
            .samples
            .iter()
            .filter(|text| !text.trim().is_empty())
            .max_by_key(|text| text.chars().count())
        {
            profile.push(Passage {
                kind: "quote".to_string(),
                text: clip(text, limit::QUOTE),
                note: "他写得最长的一条".to_string(),
                ..Default::default()
            });
        }

        Self {
            title: "未归纳的观测".to_string(),
            note: format!(
                "{} 条发言、{} 天的观测，模型这次没接上",
                material.total,
                material.span_days()
            ),
            tags,
            profile,
            accent: Accent::pick(material.user_id).name().to_string(),
            estimated: true,
        }
    }

    /// 一个维度下的标签，按模型给的先后。
    pub fn tags_of(&self, dimension: &str) -> Vec<&Tag> {
        self.tags
            .iter()
            .filter(|tag| tag.dim() == Some(dimension))
            .collect()
    }

    /// 综述里真正有内容的那几段。空的会在版面上隐去。
    pub fn live_passages(&self) -> impl Iterator<Item = &Passage> {
        self.profile.iter().filter(|passage| {
            if passage.is_quote() {
                !passage.text.trim().is_empty()
            } else {
                !passage.body.trim().is_empty()
            }
        })
    }
}

pub fn percent(ratio: f64) -> String {
    format!("{:.0}%", ratio * 100.0)
}

/// 「23 点前后」这样的说法比「23:00」更像人话。
pub fn hour_label(hour: usize) -> String {
    let hour = hour % 24;
    match hour {
        0 => "午夜".to_string(),
        1..=4 => format!("凌晨 {hour} 点"),
        5..=7 => format!("清晨 {hour} 点"),
        8..=11 => format!("上午 {hour} 点"),
        12 => "中午".to_string(),
        13..=17 => format!("下午 {hour} 点"),
        18..=22 => format!("晚上 {hour} 点"),
        _ => "深夜 23 点".to_string(),
    }
}

pub fn weekday_label(weekday: usize) -> &'static str {
    const NAMES: [&str; 7] = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];
    NAMES[weekday % 7]
}

/// 这份东西是什么，先把它说清楚——画像有两个面：可核验，也有损。
const SYSTEM_PROMPT: &str = r#"你在做一份用户画像。

用户画像是从用户的行为数据里抽象出来的一个标签化模型。它不等于这个人：它只是一组能被
数据撑住的标签，加一段把这些标签串起来的话。它的好处是可核验，每一条都能回到他做过的
事上；它的短处是有损，没观测到的部分一句话也不许有。

读你这份东西的人不认识他。他读完要能说出：这个人什么时候来，说什么，跟谁说话，怎么说话。

【标签】
标签是这份画像的骨架。四个维度都要给，每个 2 到 4 条，整个画像 8 到 16 条：
- 活跃：什么时候出现，去得勤不勤，密度如何。
- 内容：说什么，反复说什么，哪些话题从来不碰。
- 交互：跟谁说话，主动发起还是接话，他说的时候别人接不接。
- 表达：怎么说。长短、句读、用不用图片表情语音。

每条标签都要标出它的抽象层级，只许用这三种：
- 事实：观测即得，不做推断。条数、时刻、占比、媒介构成、原话都算这一类。照着下发的统计量
  写，不要另编数字。
- 统计：把观测按一个阈值归成一类。写的时候把阈值带出来，「夜里发言占四成」是观测，
  「作息偏晚」是归出来的类，两个都要有。
- 推断：从样本的意思里读出来的。这一类最容易编，所以一条必须有一条原话或一组数字顶在下面；
  顶不住的不要写。推断标签不超过总数的一半。

标签要说得出依据，别写成绰号。写「夜里出现，白天基本不在」，不写「夜猫子」。

【综述】
综述是血肉，四到六段，笔法是白描：照着事实写，不加修饰。
- 说今天的话。不用文言词，不用成语连堆。
- 一句话说一件事，写成完整的陈述句。句子短，主语清楚。
- 每一句判断后面要有东西撑着：他说过的话、他做过的动作、数字、时间。只有判断没有事实的
  句子，删掉。
- 至少两段把引语单独成段，前后用自己的话接住它，让这句话落在论证里。引语是证据，不是
  装饰。引语必须逐字出自下发的样本，一个字都不能改；找不到合适的就不给引语。
- 不用比喻，不用对仗，不用金句，不用格言体。不把一句话写成两半互相对照，不用「不是……
  而是……」「既……又……」这种句式。
- 不用评价词（很强、非常、厉害），不用模糊限定（似乎、某种、大概）。
- 不用「其实」「说到底」「值得一提的是」这类垫话。
- 一段的末尾说完事就停，不要加一句总结性的判断。
- 数字挑着用，一段里至多两三处，只留撑得住判断的；不要一串串罗列。
- 同一件事只说一遍，同一个数字不报两遍。
- 冷静、克制。说穿，但不羞辱；不留情面，也不刻薄。
- 不写外貌、性别、年龄、地域、收入、健康、政治立场；不臆断他做什么工作、住在哪里、
  跟谁是什么关系。

只输出一个 JSON 对象，不要代码块，不要解释，不要前后缀。

JSON 字段：
{
  "title": "综合标签，2 到 7 个字。是概括，不是夸赞",
  "note": "一句话概括，不超过 30 字",
  "tags": [
    {"dimension":"活跃","layer":"事实","label":"标签，不超过 12 字","evidence":"撑住它的数字、时刻或原话，不超过 44 字"}
  ],
  "profile": [
    {"kind":"text","body":"一段综述，不超过 220 字"},
    {"kind":"quote","text":"逐字引用的一条发言","note":"这句话说明什么，不超过 24 字"}
  ],
  "accent": "从 amber / rose / mint / indigo / violet / teal 里选一个当报告主色"
}

tags 的 dimension 只能写 活跃 / 内容 / 交互 / 表达，layer 只能写 事实 / 统计 / 推断。
profile 给 4 到 6 段，其中 2 段是 kind 为 quote 的引语，其余是 text。
写完自己看一遍：有没有对仗的句子，有没有只下判断不给事实的句子，有没有一句空收尾，
有没有同一个数字报了两遍，有没有把没观测到的事当事实写进去。"#;

/// 组装下发给模型的素材。统计在前，样本在后——标签的「事实」与「统计」两层都从统计量里取，
/// 「推断」那一层才用得着样本。
pub fn user_prompt(material: &Material) -> String {
    let mut out = String::with_capacity(12_288);

    out.push_str("【对象】\n");
    out.push_str(&format!("群名片：{}\n", material.name));
    out.push_str(&format!("QQ：{}\n\n", material.user_id));

    out.push_str("【统计】\n");
    out.push_str(&format!(
        "- 群聊发言 {} 条，覆盖 {} 天（活跃 {} 天），平均每天 {:.1} 条\n",
        material.total,
        material.span_days(),
        material.active_days,
        material.per_day()
    ));
    out.push_str(&format!(
        "- 单条最长 {} 字，平均 {:.1} 字\n",
        material.longest,
        material.avg_len()
    ));
    out.push_str(&format!(
        "- 活跃时段：{}前后最密；夜间（0—6 点）占 {}；最活跃的一天是{}\n",
        hour_label(material.peak_hour()),
        percent(material.night_ratio()),
        weekday_label(material.peak_weekday())
    ));
    out.push_str(&format!(
        "- 纯文字 {} 条；图片 {}、表情包 {}、小表情 {}、语音 {}、视频 {}\n",
        material.kinds.text,
        material.kinds.image,
        material.kinds.anim_emoji,
        material.kinds.face,
        material.kinds.voice,
        material.kinds.video
    ));
    out.push_str(&format!(
        "- 引用别人的消息 {} 次，@ 别人 {} 次\n",
        material.kinds.reply, material.kinds.at
    ));
    if !material.groups.is_empty() {
        let groups: Vec<String> = material
            .groups
            .iter()
            .map(|group| format!("{}（{} 条）", group.name, group.count))
            .collect();
        out.push_str(&format!("- 常在的群：{}\n", groups.join("、")));
    }
    if !material.words.is_empty() {
        let words: Vec<String> = material
            .words
            .iter()
            .take(18)
            .map(|(word, count)| format!("{word}×{count}"))
            .collect();
        out.push_str(&format!("- 高频词：{}\n", words.join("、")));
    }

    out.push_str("\n【发言样本】（按时间由近及远，长句已截断）\n");
    if material.samples.is_empty() {
        out.push_str("（没有可读的发言样本）\n");
    } else {
        for (index, sample) in material.samples.iter().enumerate() {
            let _ = writeln!(out, "{}. {}", index + 1, sample);
        }
    }
    out
}

pub fn system_prompt() -> &'static str {
    SYSTEM_PROMPT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::portrait::collect::{GroupSlice, Kinds};

    fn material() -> Material {
        Material {
            user_id: 3373167460,
            name: "甲".into(),
            total: 400,
            first_time: 1_700_000_000,
            last_time: 1_700_000_000 + 86_400 * 29,
            active_days: 20,
            hour: {
                let mut hour = [0u64; 24];
                hour[23] = 90;
                hour[2] = 40;
                hour
            },
            weekday: {
                let mut weekday = [0u64; 7];
                weekday[4] = 120;
                weekday
            },
            groups: vec![GroupSlice {
                name: "测试群".into(),
                count: 300,
            }],
            kinds: Kinds {
                text: 300,
                image: 40,
                anim_emoji: 40,
                face: 10,
                voice: 5,
                video: 5,
                reply: 60,
                at: 20,
            },
            longest: 210,
            avg_len: 18.0,
            words: vec![("天气".into(), 12), ("代码".into(), 8)],
            samples: vec![
                "今天这个雨下得没完没了".to_string(),
                "凌晨三点还在改代码，明天又要废了".to_string(),
            ],
        }
    }

    fn tag(dimension: &str, layer: &str, label: &str) -> Tag {
        Tag {
            dimension: dimension.into(),
            layer: layer.into(),
            label: label.into(),
            evidence: "夜间占 41%".into(),
        }
    }

    #[test]
    fn json_is_found_behind_fences_and_chatter() {
        let raw = "好的，这是画像：\n```json\n{\"title\":\"夜里的常客\",\"note\":\"白天基本不在\"}\n```\n希望有帮助";
        let persona = parse(raw).unwrap();
        assert_eq!(persona.title, "夜里的常客");
        assert_eq!(persona.note, "白天基本不在");
    }

    #[test]
    fn json_without_braces_is_an_error() {
        assert!(parse("我觉得他挺好的").is_err());
    }

    /// 维度只认白名单里的四个，写成「活跃度」「内容偏好」也要归到正名上。
    #[test]
    fn dimensions_are_normalised_onto_the_four() {
        let persona = Persona {
            tags: vec![
                tag("活跃度", "观测", "夜里出现"),
                tag("内容偏好", "statistical", "常聊天气"),
                tag("社交关系", "推断", "爱接别人的话"),
                tag("表达风格", "inferred", "句子短"),
            ],
            ..Default::default()
        }
        .sanitize(&material());
        let dims: Vec<&str> = persona.tags.iter().filter_map(|tag| tag.dim()).collect();
        assert_eq!(dims, vec!["活跃", "内容", "交互", "表达"]);
        assert_eq!(
            persona.tags.iter().map(|tag| tag.tier()).collect::<Vec<_>>(),
            vec!["事实", "统计", "推断", "推断"],
            "层级要归一到事实/统计/推断"
        );
    }

    /// 认不出维度的标签直接丢掉，版面宁可少一块，也不印一条没有归处的标签。
    #[test]
    fn a_tag_outside_the_taxonomy_is_dropped() {
        let persona = Persona {
            tags: vec![
                tag("活跃", "事实", "夜里出现"),
                tag("星座", "推断", "大概是天蝎座"),
                Tag {
                    dimension: "内容".into(),
                    layer: "事实".into(),
                    label: "   ".into(),
                    evidence: String::new(),
                },
            ],
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(persona.tags.len(), 1);
        assert_eq!(persona.tags[0].label, "夜里出现");
    }

    /// 一个维度最多四条：四个维度都得有位置，不能由着一个维度铺满。
    #[test]
    fn one_dimension_cannot_fill_the_whole_card() {
        let persona = Persona {
            tags: (0..9)
                .map(|index| tag("活跃", "事实", &format!("第{index}条")))
                .collect(),
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(persona.tags.len(), limit::TAGS_PER_DIM);
    }

    /// 引语必须逐字出自样本；编出来的一句都留不下。综述不受这条限制。
    #[test]
    fn quotes_must_appear_in_the_samples() {
        let persona = Persona {
            profile: vec![
                Passage {
                    kind: "text".into(),
                    body: "他说话像在收尾。".into(),
                    ..Default::default()
                },
                Passage {
                    kind: "quote".into(),
                    text: "凌晨三点还在改代码，明天又要废了".into(),
                    note: "原话".into(),
                    ..Default::default()
                },
                Passage {
                    kind: "quote".into(),
                    text: "我从来没说过这句话".into(),
                    note: "编的".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(persona.profile.len(), 2);
        assert_eq!(persona.profile[0].kind, "text");
        assert_eq!(persona.profile[1].kind, "quote");
        assert!(persona.profile[1].text.starts_with("凌晨三点"));
    }

    /// 标点与空白的差别不该让一条真原话被误判成编的。
    #[test]
    fn quotes_tolerate_punctuation_differences() {
        let persona = Persona {
            profile: vec![Passage {
                kind: "引语".into(),
                text: "今天这个雨，下得没完没了！".into(),
                note: "原话".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(persona.profile.len(), 1);
        assert_eq!(persona.profile[0].kind, "quote");
    }

    /// 认不出来的 kind 当综述处理；空的段落一律丢掉。
    #[test]
    fn unknown_kinds_become_prose_and_blanks_disappear() {
        let persona = Persona {
            profile: vec![
                Passage {
                    kind: "段落".into(),
                    body: "他说事就说事。".into(),
                    ..Default::default()
                },
                Passage {
                    kind: "text".into(),
                    body: "   ".into(),
                    ..Default::default()
                },
                Passage {
                    kind: String::new(),
                    body: "下一段接着说。".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(persona.profile.len(), 2);
        assert!(persona.profile.iter().all(|p| p.kind == "text"));
        assert_eq!(persona.profile[1].body, "下一段接着说。");
    }

    #[test]
    fn overlong_fields_are_clipped_and_paragraphs_are_capped() {
        let persona = Persona {
            title: "这是一个特别特别长的综合标签".into(),
            note: "长".repeat(200),
            profile: (0..20)
                .map(|index| Passage {
                    kind: "text".into(),
                    body: format!("第{index}段"),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(persona.title.chars().count(), limit::TITLE + 1);
        assert!(persona.note.chars().count() <= limit::NOTE + 1);
        assert_eq!(persona.profile.len(), limit::MAX_PASSAGES);

        let clipped = Persona {
            tags: vec![Tag {
                dimension: "表达".into(),
                layer: "事实".into(),
                label: "字".repeat(60),
                evidence: "据".repeat(200),
            }],
            profile: vec![Passage {
                kind: "text".into(),
                body: "字".repeat(900),
                ..Default::default()
            }],
            ..Default::default()
        }
        .sanitize(&material());
        assert!(clipped.tags[0].label.chars().count() <= limit::LABEL + 1);
        assert!(clipped.tags[0].evidence.chars().count() <= limit::EVIDENCE + 1);
        assert!(clipped.profile[0].body.chars().count() <= limit::PASSAGE + 1);
    }

    #[test]
    fn accent_uses_the_palette_and_falls_back_deterministically() {
        let chosen = Persona {
            accent: "rose".into(),
            ..Default::default()
        };
        assert_eq!(chosen.accent(1), Accent::Rose);
        let garbage = Persona {
            accent: "chartreuse".into(),
            ..Default::default()
        };
        assert_eq!(garbage.accent(1), Accent::pick(1));
        assert_eq!(garbage.accent(1), garbage.accent(1));
        assert_eq!(Accent::ALL.len(), 6);
    }

    /// 兜底画像也是一份真画像：四个维度里至少有三个有标签，引语一定出自样本。
    #[test]
    fn the_fallback_is_still_a_profile_made_of_tags() {
        let material = material();
        let persona = Persona::from_stats(&material);
        assert!(persona.estimated);
        assert!(persona.tags.len() >= 5);
        for dimension in ["活跃", "交互", "表达", "内容"] {
            assert!(
                !persona.tags_of(dimension).is_empty(),
                "{dimension} 没有标签"
            );
        }
        assert!(persona.note.contains("400"));
        assert!(persona.profile[0].body.contains("400"));
        let quotes: Vec<&Passage> = persona
            .profile
            .iter()
            .filter(|passage| passage.is_quote())
            .collect();
        assert_eq!(quotes.len(), 1);
        assert!(
            material
                .samples
                .iter()
                .any(|sample| sample.contains(&quotes[0].text))
        );
    }

    /// 提示词要把画像是什么、四个维度、三层抽象与白描笔法都写清楚。
    #[test]
    fn the_prompt_states_the_model_of_a_user_profile() {
        let system = system_prompt();
        assert!(system.contains("标签化模型"));
        assert!(system.contains("可核验"));
        assert!(system.contains("有损"));
        assert!(system.contains("活跃"));
        assert!(system.contains("交互"));
        assert!(system.contains("推断标签不超过总数的一半"));
        assert!(system.contains("白描"));
        assert!(system.contains("逐字"));
    }

    /// 统计量必须随提示词一起下发——标签里的「事实」与「统计」两层全靠它。
    #[test]
    fn the_prompt_carries_the_numbers_the_facts_come_from() {
        let material = material();
        let prompt = user_prompt(&material);
        assert!(prompt.contains("群名片：甲"));
        assert!(prompt.contains("群聊发言 400 条"));
        assert!(prompt.contains("夜间（0—6 点）占"));
        assert!(prompt.contains("引用别人的消息 60 次"));
        assert!(prompt.contains("高频词"));
        assert!(prompt.contains("凌晨三点还在改代码"));
        assert!(!prompt.contains("卦"), "画像里不该再出现筮法的说法");
    }
}
