//! 把素材交给模型，换回一份人格画像；模型不接时退回统计直出的标签与语言指纹。
//!
//! 一份画像分三层，从硬到软，读者对它们的信任度该一路往下走：
//!
//! - **仪器读数**：语言指纹 [`Style`](super::collect::Style)、活跃与交互统计。全部由
//!   [`super::collect`] 从库里数出来，不经过模型，可核验。
//! - **读法**：四维行为标签（活跃/内容/交互/表达，事实/统计/推断三层）加一段语言风格白描。
//!   模型读那些事实，产出这一层；引语必须逐字出自样本，对不上就丢掉。
//! - **投影**：MBTI 四轴与九型核心（[`super::models`]）。把行为往两套既有框架上做的读数，
//!   是讨论的起点，不是结论——它们只在模型接上时才有，模型不接就整块消失，不用伪精度去补。
//!
//! 这一层只认三件事：**输出是一个 JSON 对象**、**引语必须是原话**、**标签分得清层级**。
//! 第一条靠宽松解析，第二条靠归一化比对，第三条落在 [`Tag`] 的形状上——维度只有四个，
//! 层级只有三层，认不出的那一维直接不印。

use super::collect::{Material, Style};
use super::models::{Enneagram, Mbti};
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt::Write as _;

/// 报告主色。色板固定，模型只能从中挑，省得它挑出一组刺眼或看不清的组合。
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

    pub fn seed_class(self) -> &'static str {
        match self {
            Accent::Amber => "seed-amber",
            Accent::Rose => "seed-rose",
            Accent::Mint => "seed-mint",
            Accent::Indigo => "seed-indigo",
            Accent::Violet => "seed-violet",
            Accent::Teal => "seed-teal",
        }
    }
}

/// 标签的四个维度。画像的骨架，**不许模型自创**——多出来的维度一律不印。
pub const DIMENSIONS: [(&str, &str); 4] = [
    ("活跃", "ACTIVITY"),
    ("内容", "CONTENT"),
    ("交互", "INTERACTION"),
    ("表达", "EXPRESSION"),
];

/// 标签的三层抽象。事实观测即得，统计按阈值归纳，推断从语义里读出来；从硬到软。
pub const LAYERS: [(&str, &str); 3] = [
    ("事实", "OBSERVED"),
    ("统计", "DERIVED"),
    ("推断", "INFERRED"),
];

/// 认一个维度名。模型常写成「活跃度」「内容偏好」这类，含关键字就算它。
fn dimension_of(name: &str) -> Option<&'static str> {
    const TABLE: [(&str, &[&str]); 4] = [
        ("活跃", &["活跃", "作息", "出现", "activity", "active", "schedule"]),
        ("内容", &["内容", "话题", "兴趣", "聊什么", "content", "topic"]),
        (
            "交互",
            &["交互", "互动", "社交", "关系", "接话", "interaction", "social"],
        ),
        ("表达", &["表达", "风格", "语言", "文风", "媒介", "expression", "style"]),
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
        ("统计", &["统计", "阈值", "归纳", "derived", "statis"]),
        ("推断", &["推断", "推测", "语义", "infer", "predict", "guess"]),
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
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub text: String,
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
    /// 综合速写：一句话把他归成一类，4—9 字。
    #[serde(default, alias = "称谓", alias = "name")]
    pub title: String,
    /// 一句话概括，不超过 30 字。
    #[serde(default, alias = "题记", alias = "一句话", alias = "summary")]
    pub note: String,
    /// 语言风格白描：这个人怎么说话，从语言指纹与样本里读出来。模型的读法，不是事实。
    #[serde(default, alias = "语言风格", alias = "voice")]
    pub style: String,
    /// MBTI 四轴光谱。只在模型接上时有。
    #[serde(default, alias = "人格光谱")]
    pub mbti: Option<Mbti>,
    /// 九型核心。只在模型接上、且主型合法时有。
    #[serde(default, alias = "九型")]
    pub enneagram: Option<Enneagram>,
    /// 四维行为标签，收口后按维度分组渲染。
    #[serde(default, alias = "标签", alias = "labels")]
    pub tags: Vec<Tag>,
    /// 综述：段落与引语按序排列。
    #[serde(default, alias = "综述", alias = "passages")]
    pub profile: Vec<Passage>,
    /// 讨论钩子：群里能拿来吵的切入点，落在具体行为或某个模型读数上。
    #[serde(default, alias = "讨论", alias = "hooks")]
    pub discuss: Vec<String>,
    #[serde(default)]
    pub accent: String,
    /// 这份画像是模型写的，还是从统计量与语言指纹直接拼出来的。
    #[serde(skip)]
    pub estimated: bool,
}

/// 各字段的字数上限。模型偶尔会无视字数要求，这里统一收口，
/// 免得一个超长字段把整张卡的版面顶乱。
mod limit {
    pub const TITLE: usize = 9;
    pub const NOTE: usize = 30;
    pub const STYLE: usize = 90;
    pub const LABEL: usize = 12;
    pub const EVIDENCE: usize = 44;
    /// 一个维度最多印几条。四个维度都得有位置，不能由着一个维度铺满。
    pub const TAGS_PER_DIM: usize = 4;
    pub const MAX_TAGS: usize = 16;
    pub const PASSAGE: usize = 200;
    pub const QUOTE: usize = 90;
    pub const QUOTE_NOTE: usize = 24;
    pub const MAX_PASSAGES: usize = 6;
    /// 讨论钩子一条的长度与总条数。
    pub const DISCUSS: usize = 40;
    pub const MAX_DISCUSS: usize = 3;
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

/// 一句引语是否「出自样本」：归一化（去标点空白、忽略大小写）后，是某条样本的子串。
///
/// [`Persona::sanitize`] 与端到端测试共用这一条判据——报告里印的是模型那句读来顺口的
/// 引语，它只在这条归一化比对通过时留下，所以「逐字出自样本」的判据前后必须同一把尺。
pub fn quote_is_from_samples(text: &str, samples: &[String]) -> bool {
    let finger = fingerprint(text);
    if finger.chars().count() < 4 {
        return false;
    }
    samples.iter().any(|sample| fingerprint(sample).contains(&finger))
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
        self.style = clip(&self.style, limit::STYLE);

        // 标签：维度认不出的丢掉，一个维度超额的丢掉。
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

        // 投影：归一 MBTI；主型不合法的九型整块丢掉，其余看清况。
        if let Some(mbti) = self.mbti {
            self.mbti = Some(mbti.sanitized());
        }
        if let Some(enneagram) = self.enneagram {
            self.enneagram = enneagram.sanitized();
        }

        // 引语逐字比对样本；综述不受此限。
        let passages = std::mem::take(&mut self.profile);
        self.profile = passages
            .into_iter()
            .filter_map(|passage| {
                if passage.is_quote() {
                    if !quote_is_from_samples(&passage.text, &material.samples) {
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

        // 讨论钩子：去空、去重、收长度。不许它引用新数据，只落在已有行为或读数上。
        let mut seen = std::collections::HashSet::new();
        self.discuss = std::mem::take(&mut self.discuss)
            .into_iter()
            .map(|hook| clip(&hook, limit::DISCUSS))
            .filter(|hook| !hook.is_empty() && seen.insert(hook.clone()))
            .take(limit::MAX_DISCUSS)
            .collect();

        self
    }

    /// 主色：模型挑的在色板里就用它，否则按用户号定色。
    pub fn accent(&self, seed: i64) -> Accent {
        Accent::from_name(&self.accent).unwrap_or_else(|| Accent::pick(seed))
    }

    /// 一个人格的「投影层」是否成篇：两个模型至少接上了一个。
    pub fn has_models(&self) -> bool {
        self.mbti.is_some() || self.enneagram.is_some()
    }

    /// 模型完全没接上时的兜底。
    ///
    /// 画像的骨头是数据，不是模型：语言指纹与统计量本来就在手里，照它把标签打出来，
    /// 仍然是一份画像，缺的只是读法与投影两层。版面上会标出「模型未接」——
    /// 这一层不用伪精度去补 MBTI 或九型：没读过语义就编不出诚心的投影。
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
            format!("图与表情占 {}", percent(material.media_ratio())),
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
                "这一次模型没有接上，没有推断出来的标签，也没有语言风格的读法。\
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
            style: String::new(),
            mbti: None,
            enneagram: None,
            tags,
            profile,
            discuss: Vec::new(),
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

/// 这份东西是什么，先把它说清楚——画像有三个面：仪器读数可核验，读法有据，投影是话题。
const SYSTEM_PROMPT: &str = r#"你在做一份用户画像。

用户画像是一个人留在群聊里的行为被整理成的一份读数。它分三层，从硬到软：

- 仪器读数：统计量与语言指纹，全部能从原始记录里数出来，可核验。你不用算，下面会给你。
- 读法：你读那些事实，得出四维标签、一段语言风格白描、一段综述。这一层要有依据。
- 投影：MBTI 四轴与九型，是把行为往两套既有框架上做的读数。它是讨论的起点，不是结论。

这份画像有损：没观测到的部分一句话也不许有。它更不等于本人，只是一份能拿去聊的参考。

读你这份东西的人不认识他。他读完要能说出：这个人怎么说话，什么时候来，说什么，
跟谁说话，以及一个人大概的行事与动机底色。

【语言指纹怎么用】
下面给一组「语言指纹」：提问率、感叹率、省略号、笑声、语气词、自称与对称呼密度、
长度起伏、长短占比、发言的爆发指数、重复率。它们都是数出来的事实。你要做的不是复述
数字，而是从它们和样本里白描这个人怎么说话：标点习惯，语气是冲是缓，是不是爱提问，
说给自己听还是说给别人听，话是匀称还是时短时长。风格字段控制在 90 字内，一两句白描，
写他本人，不写成通用评语。不用形容词堆砌，不写「很有个性」这种空话。

【四维标签】
标签是读法层的骨架。四个维度都要给，每个 2 到 4 条，整个画像 8 到 16 条：
- 活跃：什么时候出现，去得勤不勤，密度如何。
- 内容：说什么，反复说什么，哪些话题从来不碰。
- 交互：跟谁说话，主动发起还是接话，他说的时候别人接不接。
- 表达：怎么说。长短、句读、用不用图片表情语音。

每条标签标出抽象层级，只许三种：
- 事实：观测即得。条数、时刻、占比、媒介构成、原话。照下发的统计量写，不另编数字。
- 统计：把观测按一个阈值归成一类，阈值要一起写出来。
- 推断：从样本的意思里读出来的。一条必须有一条原话或一组数字顶着，顶不住的不要写。
  推断标签不超过总数的一半。标签要说得出依据，别写成绰号。

【投影：MBTI 四轴】
给四轴各一个 -100 到 100 的整数：正 = 靠向 E（外向）/ S（实感）/ T（思考）/ J（判断），
负 = 靠向 I（内向）/ N（直觉）/ F（情感）/ P（知觉）。绝对值越大越偏。多数人贴近中线，
拿不准就给接近 0 的数，只在证据清楚时才给大数。这不是给人定型，是标出他往哪一侧使劲。
四轴推出来的参考码由系统自己算，你不用给。

【投影：九型】
给一个 1 到 9 的主型和一个相邻侧翼（一号两翼 9 与 2，九号两翼 8 与 1，中间取相邻）。
这一套答的是「他为什么这样行事」——核心的怕与求。同样按证据来，证据不足就挑最接近的。

【讨论钩子】
给 1 到 3 条群里能拿来聊的钩子。每条一句，落在具体的行为或某个投影读数上，让人能
同意也能反驳。不许编新的数字或引语，不许反问，不评判人。

【综述笔法】
综述是血肉，四到六段，白描：照着事实写，不加修饰。
- 一句话说一件事，写成完整的陈述句。句子短，主语清楚。
- 每一句判断后面要有东西撑着：他说过的话、数字、时间。只有判断没有事实的句子，删掉。
- 至少两段把引语单独成段，前后用自己的话接住它。引语必须逐字出自下发的样本，一个字
  都不能改；找不到合适的就不给引语。
- 不用比喻、对仗、金句、格言体，不用「不是……而是……」「既……又……」这种句式。
- 不用评价词（很强、非常、厉害），不用模糊限定（似乎、某种、大概），不用「其实」「说到底」。
- 数字挑着用，一段里至多两三处。同一件事只说一遍。
- 冷静、克制。说穿，但不羞辱。

不写外貌、性别、年龄、地域、收入、健康、政治立场；不臆断他做什么工作、住在哪里、
跟谁是什么关系。

只输出一个 JSON 对象，不要代码块，不要解释，不要前后缀。

JSON 字段：
{
  "title": "综合速写，4 到 9 字，是概括不是夸赞",
  "note": "一句话概括，不超过 30 字",
  "style": "语言风格白描，不超过 90 字",
  "mbti": {"energy": -40, "perceiving": -55, "deciding": 30, "lifestyle": -20},
  "enneagram": {"number": 5, "wing": 4},
  "tags": [
    {"dimension":"活跃","layer":"事实","label":"标签，不超过 12 字","evidence":"撑住它的数字、时刻或原话，不超过 44 字"}
  ],
  "profile": [
    {"kind":"text","body":"一段综述，不超过 200 字"},
    {"kind":"quote","text":"逐字引用的一条发言","note":"这句话说明什么，不超过 24 字"}
  ],
  "discuss": ["讨论钩子，不超过 40 字"],
  "accent": "从 amber / rose / mint / indigo / violet / teal 里选一个当报告主色"
}

tags 的 dimension 只能写 活跃 / 内容 / 交互 / 表达，layer 只能写 事实 / 统计 / 推断。
mbti 四轴、enneagram 必填。profile 给 4 到 6 段，其中 2 段是 quote。discuss 给 1 到 3 条。
写完自己看一遍：有没有对仗，有没有只下判断不给事实的句子，有没有空收尾，
有没有同一个数字报了两遍，有没有把没观测到的事当事实写。"#;

/// 组装下发给模型的素材。三段：统计量、语言指纹、发言样本——前两段喂事实，样本喂语义。
pub fn user_prompt(material: &Material, style: &Style) -> String {
    let mut out = String::with_capacity(13_824);

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

    out.push_str("\n【语言指纹】（全部由原始记录数出，可核验）\n");
    out.push_str(&format!(
        "- 以问号收尾 {}，以感叹号收尾 {}，含省略号 {}，含笑声词 {}，含语气词 {}\n",
        percent(style.question_rate),
        percent(style.exclaim_rate),
        percent(style.ellipsis_rate),
        percent(style.laugh_rate),
        percent(style.modal_rate),
    ));
    out.push_str(&format!(
        "- 平均每条逗号 {:.1}；长度起伏系数 {}；长消息（≥40 字）{}、短消息（≤5 字）{}\n",
        style.comma_per_msg,
        style.len_cv,
        percent(style.long_rate),
        percent(style.short_rate),
    ));
    out.push_str(&format!(
        "- 每百字自称 {:.1} 次、对称呼 {:.1} 次；发言爆发指数（-1 极匀、0 随机、1 极爆发）{:.2}；相邻重复 {}\n",
        style.self_per100,
        style.you_per100,
        style.burstiness,
        percent(style.repeat_rate)
    ));

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

    fn style() -> Style {
        Style {
            readable: 100,
            question_rate: 0.2,
            exclaim_rate: 0.1,
            ellipsis_rate: 0.05,
            comma_per_msg: 1.8,
            laugh_rate: 0.3,
            modal_rate: 0.25,
            self_per100: 2.4,
            you_per100: 3.1,
            len_cv: 0.6,
            long_rate: 0.15,
            short_rate: 0.4,
            burstiness: 0.62,
            repeat_rate: 0.08,
        }
    }

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
            style: style(),
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

    #[test]
    fn a_full_persona_sanitises_end_to_end() {
        let raw = r#"{
            "title": "夜里的常客",
            "note": "白天基本不在，夜里话密起来",
            "style": "话短，句尾常带问号和省略号，像自言自语又像追问。",
            "mbti": {"EI": -72, "SN": -55, "TF": 40, "JP": -20},
            "enneagram": {"type": 5, "wing": 4},
            "tags": [ {"dimension":"活跃","layer":"事实","label":"夜里出现"} ],
            "profile": [
                {"kind":"quote","text":"凌晨三点还在改代码，明天又要废了","note":"拿休息换进度"},
                {"kind":"text","body":"他把手艺当退路。"}
            ],
            "discuss": ["他嘴上说无所谓，其实每条都改到半夜", "这个 IN 的底子，你觉得准吗"],
            "accent": "indigo"
        }"#;
        let persona = parse(raw).unwrap().sanitize(&material());
        assert_eq!(persona.title, "夜里的常客");
        assert_eq!(persona.mbti.as_ref().unwrap().code(), "INTP");
        assert_eq!(persona.enneagram.unwrap().short(), "5w4");
        assert!(persona.has_models());
        assert_eq!(persona.discuss.len(), 2);
        assert_eq!(persona.profile[0].kind, "quote");
        assert_eq!(persona.profile[1].kind, "text");
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

    /// 认不出维度的标签直接丢掉。
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

    /// 一个维度最多四条。
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

    /// 九型主型不合法时整块丢掉；MBTI 越界被夹回。
    #[test]
    fn models_are_cleaned_or_dropped() {
        let persona = Persona {
            mbti: Some(Mbti {
                energy: 999,
                perceiving: -999,
                deciding: 0,
                lifestyle: 0,
            }),
            enneagram: Some(Enneagram { number: 12, wing: 0 }),
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(persona.mbti.as_ref().unwrap().energy, 100);
        assert_eq!(persona.mbti.as_ref().unwrap().perceiving, -100);
        assert!(persona.enneagram.is_none(), "主型 12 非法，整块丢掉");
        assert!(!persona.has_models() == false, "MBTI 仍在，投影层不算空");
    }

    /// 引语必须逐字出自样本；编出来的一句都留不下。
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
    fn overlong_fields_are_clipped_and_discuss_is_capped() {
        let persona = Persona {
            title: "这是一个特别特别长的综合速写".into(),
            note: "长".repeat(200),
            style: "风".repeat(300),
            discuss: (0..10).map(|i| format!("钩子 {i}")).collect(),
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
        assert!(persona.style.chars().count() <= limit::STYLE + 1);
        assert_eq!(persona.profile.len(), limit::MAX_PASSAGES);
        assert_eq!(persona.discuss.len(), limit::MAX_DISCUSS);
    }

    /// 讨论钩子去重：内容相同的两条只留一条。
    #[test]
    fn duplicate_discussion_hooks_collapse() {
        let persona = Persona {
            discuss: vec!["同一句钩子".into(), "同一句钩子".into(), "另一句".into()],
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(persona.discuss, vec!["同一句钩子".to_string(), "另一句".to_string()]);
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
    }

    /// 兜底画像也是一份真画像：模型不接，投影层为空，但事实标签与引语仍在。
    #[test]
    fn the_fallback_has_facts_and_no_projection() {
        let material = material();
        let persona = Persona::from_stats(&material);
        assert!(persona.estimated);
        assert!(!persona.has_models(), "模型没接，不给投影");
        assert!(persona.mbti.is_none());
        assert!(persona.enneagram.is_none());
        assert!(persona.style.is_empty());
        assert!(persona.tags.len() >= 5);
        for dimension in ["活跃", "交互", "表达", "内容"] {
            assert!(
                !persona.tags_of(dimension).is_empty(),
                "{dimension} 没有标签"
            );
        }
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

    /// 提示词要把三层说清楚、两把尺子的口径、语言指纹与逐字引语都交待到。
    #[test]
    fn the_prompt_states_the_three_layers_and_the_two_models() {
        let system = system_prompt();
        assert!(system.contains("仪器读数"));
        assert!(system.contains("读法"));
        assert!(system.contains("投影"));
        assert!(system.contains("MBTI"));
        assert!(system.contains("九型"));
        assert!(system.contains("语言指纹"));
        assert!(system.contains("逐字"));
        assert!(system.contains("讨论的起点，不是结论"));
    }

    /// 统计量、语言指纹与样本都必须随提示词一起下发。
    #[test]
    fn the_prompt_carries_the_numbers_and_the_fingerprint() {
        let material = material();
        let prompt = user_prompt(&material, &style());
        assert!(prompt.contains("群名片：甲"));
        assert!(prompt.contains("群聊发言 400 条"));
        assert!(prompt.contains("夜间（0—6 点）占"));
        assert!(prompt.contains("引用别人的消息 60 次"));
        assert!(prompt.contains("语言指纹"));
        assert!(prompt.contains("以问号收尾"));
        assert!(prompt.contains("发言爆发指数"));
        assert!(prompt.contains("凌晨三点还在改代码"));
        assert!(!prompt.contains("卦"), "画像里不该再出现筮法的说法");
    }
}
