//! 把采集到的素材交给模型，换回一份结构化的画像；模型不接时用统计量兜底。
//!
//! 这一层只认三件事：**模型的输出是一个 JSON 对象**、**引语必须是原话**、
//! **三面侧写要落在固定的三个位置上**。前者靠宽松解析（模型喜欢在 JSON 外面裹一句
//! 「好的」或一层代码块），第二条靠归一化比对——对不上就丢掉，宁可少一条引语，
//! 也不让报告里出现一句编出来的「他说过」；第三条靠键名归一，模型给出的键名认不出
//! 时按出现顺序补位，免得整块侧写因为一个键名写错而消失。

use super::collect::Material;
use serde::Deserialize;

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

/// 一个心理维度上的刻度。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Trait {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub score: f64,
    #[serde(default)]
    pub note: String,
}

/// 作者给出的原话，以及挑它的理由。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Quote {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub why: String,
}

/// 侧写的一面。`key` 由本层归一成固定三个之一，不采信模型自己写的标签。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Facet {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
}

/// 抽的那一签。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Lot {
    #[serde(default)]
    pub no: i64,
    #[serde(default)]
    pub grade: String,
    #[serde(default)]
    pub verse: Vec<String>,
    #[serde(default)]
    pub reading: String,
}

/// 三面侧写的固定位置与键名。顺序就是报告里的呈现顺序。
pub const FACET_KEYS: [&str; 3] = ["立身", "心相", "人群"];

/// 签等只收这几个说法，模型写出别的（比如「凶」）就退回「中平」。
const GRADES: [&str; 5] = ["上上", "上吉", "中吉", "中平", "中下"];

/// 一份可以交给模板渲染的画像。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Persona {
    #[serde(default)]
    pub codename: String,
    #[serde(default)]
    pub tagline: String,
    #[serde(default)]
    pub summary: String,
    /// 三面侧写，恒定是「立身 / 心相 / 人群」这个顺序与这三个名字。
    #[serde(default)]
    pub facets: Vec<Facet>,
    #[serde(default)]
    pub traits: Vec<Trait>,
    #[serde(default)]
    pub interests: Vec<String>,
    #[serde(default)]
    pub lot: Lot,
    #[serde(default)]
    pub quotes: Vec<Quote>,
    #[serde(default)]
    pub advice: String,
    #[serde(default)]
    pub accent: String,
    /// 这份画像是模型写的，还是从统计量拼出来的。
    #[serde(skip)]
    pub estimated: bool,
}

/// 各字段的字数上限。模型偶尔会无视字数要求，这里统一收口，
/// 免得一个超长字段把整张卡的版面顶乱。
mod limit {
    pub const CODENAME: usize = 9;
    pub const TAGLINE: usize = 26;
    pub const SUMMARY: usize = 110;
    pub const FACET_TITLE: usize = 14;
    pub const FACET_BODY: usize = 110;
    pub const TRAIT_NAME: usize = 6;
    pub const TRAIT_NOTE: usize = 34;
    pub const INTEREST: usize = 10;
    pub const VERSE: usize = 12;
    pub const READING: usize = 80;
    pub const QUOTE: usize = 90;
    pub const QUOTE_WHY: usize = 26;
    pub const ADVICE: usize = 40;
    pub const MAX_TRAITS: usize = 4;
    pub const MAX_INTERESTS: usize = 6;
    pub const MAX_QUOTES: usize = 3;
    pub const MAX_VERSE: usize = 4;
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

/// 把模型写的面名认到固定的三个位置上。
///
/// 模型有时写「立身」、有时写「立身：与劳作的关系」，也可能干脆写成别的词。
/// 认得出来按名归位，认不出来就按出现顺序补位——位置总比标签可靠。
fn facet_slot(key: &str) -> Option<usize> {
    const ALIASES: [&[&str]; 3] = [
        &["立身", "做事", "谋事", "事业", "劳作", "职业", "工作"],
        &["心相", "心理", "内心", "本心", "性情", "动机"],
        &["人群", "人际", "社交", "人堆", "关系", "位置"],
    ];
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    ALIASES
        .iter()
        .position(|names| names.iter().any(|name| key.contains(name)))
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

/// 把三面侧写摆回固定的三个位置，缺的补空。
fn settle_facets(incoming: Vec<Facet>) -> Vec<Facet> {
    let mut slots: [Option<Facet>; 3] = [None, None, None];
    let mut leftover: Vec<Facet> = Vec::new();
    for facet in incoming {
        match facet_slot(&facet.key) {
            Some(index) if slots[index].is_none() => slots[index] = Some(facet),
            _ => leftover.push(facet),
        }
    }
    let mut spare = leftover.into_iter();
    let mut out = Vec::with_capacity(FACET_KEYS.len());
    for (index, key) in FACET_KEYS.iter().enumerate() {
        let mut facet = slots[index]
            .take()
            .or_else(|| spare.next())
            .unwrap_or_default();
        facet.key = (*key).to_string();
        facet.title = clip(&facet.title, limit::FACET_TITLE);
        facet.body = clip(&facet.body, limit::FACET_BODY);
        out.push(facet);
    }
    out
}

impl Persona {
    /// 收口：字数、条数、分数范围、签等，以及引语必须出自样本。
    pub fn sanitize(mut self, material: &Material) -> Self {
        self.codename = clip(&self.codename, limit::CODENAME);
        self.tagline = clip(&self.tagline, limit::TAGLINE);
        self.summary = clip(&self.summary, limit::SUMMARY);
        self.advice = clip(&self.advice, limit::ADVICE);
        self.facets = settle_facets(std::mem::take(&mut self.facets));

        self.traits = self
            .traits
            .into_iter()
            .filter(|item| !item.name.trim().is_empty())
            .take(limit::MAX_TRAITS)
            .map(|mut item| {
                item.name = clip(&item.name, limit::TRAIT_NAME);
                item.note = clip(&item.note, limit::TRAIT_NOTE);
                item.score = if item.score.is_finite() {
                    item.score.clamp(0.0, 100.0)
                } else {
                    0.0
                };
                item
            })
            .collect();

        let mut seen = std::collections::HashSet::new();
        self.interests = self
            .interests
            .into_iter()
            .map(|word| clip(&word, limit::INTEREST))
            .filter(|word| !word.is_empty())
            .filter(|word| seen.insert(word.clone()))
            .take(limit::MAX_INTERESTS)
            .collect();

        // 签：签号夹回 1—100，签等只认白名单，签诗按四句收口。
        self.lot.no = if self.lot.no > 0 {
            self.lot.no.min(100)
        } else {
            (material.user_id.unsigned_abs() % 100 + 1) as i64
        };
        self.lot.grade = match GRADES
            .iter()
            .find(|grade| self.lot.grade.trim().contains(**grade))
        {
            Some(grade) => (*grade).to_string(),
            None => "中平".to_string(),
        };
        self.lot.verse = self
            .lot
            .verse
            .into_iter()
            .map(|line| clip(&line, limit::VERSE))
            .filter(|line| !line.is_empty())
            .take(limit::MAX_VERSE)
            .collect();
        self.lot.reading = clip(&self.lot.reading, limit::READING);

        let samples: Vec<String> = material.samples.iter().map(|s| fingerprint(s)).collect();
        self.quotes = self
            .quotes
            .into_iter()
            .filter(|quote| {
                let finger = fingerprint(&quote.text);
                finger.chars().count() >= 4
                    && samples.iter().any(|sample| sample.contains(&finger))
            })
            .take(limit::MAX_QUOTES)
            .map(|mut quote| {
                quote.text = clip(&quote.text, limit::QUOTE);
                quote.why = clip(&quote.why, limit::QUOTE_WHY);
                quote
            })
            .collect();

        self
    }

    /// 主色：模型挑的在色板里就用它，否则按用户号定色。
    pub fn accent(&self, seed: i64) -> Accent {
        Accent::from_name(&self.accent).unwrap_or_else(|| Accent::pick(seed))
    }

    /// 模型完全没接上时的兜底画像：全部由统计量拼出来。
    ///
    /// 与其回一句「生成失败」，不如把数字本身排成一张能看的图——用户要的信息
    /// 大半都在数字里，缺的只是文字评论。三面侧写照旧占位，好让版式不至于塌掉。
    pub fn from_stats(material: &Material) -> Self {
        let night = material.night_ratio();
        let media = material.media_ratio();
        let per_day = material.per_day();
        let avg = material.avg_len();

        let traits = vec![
            meter("话量", (per_day / 40.0 * 100.0).min(99.0), "看平均每天发几条"),
            meter("夜行", night * 100.0, "0 点到 6 点的发言占比"),
            meter("图文", media * 100.0, "图片、表情包与小表情的占比"),
            meter("长句", (avg / 40.0 * 100.0).min(99.0), "单条发言的平均字数"),
        ]
        .into_iter()
        .map(|(name, score, note)| Trait {
            name: name.to_string(),
            score: score.round(),
            note: note.to_string(),
        })
        .collect();

        let longest: Vec<Quote> = {
            let mut sorted: Vec<&String> = material.samples.iter().collect();
            sorted.sort_by_key(|text| std::cmp::Reverse(text.chars().count()));
            sorted
                .into_iter()
                .take(2)
                .map(|text| Quote {
                    text: text.clone(),
                    why: "他写得最长的一条".to_string(),
                })
                .collect()
        };

        let groups = material
            .groups
            .first()
            .map(|group| group.name.as_str())
            .unwrap_or("未知群");
        let facets = vec![
            Facet {
                key: FACET_KEYS[0].to_string(),
                title: format!("{} 天，{} 句话", material.span_days(), material.total),
                body: format!(
                    "平均每天 {:.1} 条，单条平均 {:.1} 字，最长 {} 字。{} 的发言带图片或表情。",
                    per_day,
                    avg,
                    material.longest,
                    percent(media)
                ),
            },
            Facet {
                key: FACET_KEYS[1].to_string(),
                title: "这次没有读到他的话".to_string(),
                body: "模型没接上，只有数字可用。他的句子是什么样、为什么这样说话，这一版答不了，\
                       不作推断。"
                    .to_string(),
            },
            Facet {
                key: FACET_KEYS[2].to_string(),
                title: format!("在 {} 个群里说话", material.groups.len()),
                body: format!(
                    "最常出没的是「{groups}」。{} 的发言引用了别人，{} 的发言直接 @ 了别人。",
                    percent(material.reply_ratio()),
                    percent(if material.total > 0 {
                        material.kinds.at as f64 / material.total as f64
                    } else {
                        0.0
                    })
                ),
            },
        ];

        Self {
            codename: "只按数字画的一张".to_string(),
            tagline: format!("{} 天里说了 {} 句话", material.span_days(), material.total),
            summary: format!(
                "统计窗口内共 {} 条群聊发言，覆盖 {} 天，活跃 {} 天，平均每天 {:.1} 条。\
                 单条最长 {} 字，平均 {:.1} 字。",
                material.total,
                material.span_days(),
                material.active_days,
                per_day,
                material.longest,
                avg
            ),
            facets,
            traits,
            interests: Vec::new(),
            lot: Lot {
                no: (material.user_id.unsigned_abs() % 100 + 1) as i64,
                grade: "中平".to_string(),
                verse: vec![
                    "未见其言".to_string(),
                    "难断其心".to_string(),
                    "且留数目".to_string(),
                    "他日再问".to_string(),
                ],
                reading: "这次只有数字，没有他的话，签不作数。等他再开口。".to_string(),
            },
            quotes: longest,
            advice: "这次模型没接上，先按数字给你画了一张，过会儿再试一次。".to_string(),
            accent: Accent::pick(material.user_id).name().to_string(),
            estimated: true,
        }
    }

    /// 三面侧写里真正有内容的那几面。空的会在版面上隐去。
    pub fn live_facets(&self) -> impl Iterator<Item = &Facet> {
        self.facets
            .iter()
            .filter(|facet| !facet.title.trim().is_empty() || !facet.body.trim().is_empty())
    }

    /// 有没有一签可看。签诗与解语都空时不占版面。
    pub fn has_lot(&self) -> bool {
        !self.lot.verse.is_empty() || !self.lot.reading.trim().is_empty()
    }
}

fn meter<'a>(name: &'a str, score: f64, note: &'a str) -> (&'a str, f64, &'a str) {
    // 分数太贴近 0 时条形几乎看不见，也给不出信息量，抬到 4 起步。
    (name, score.clamp(4.0, 99.0), note)
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

const SYSTEM_PROMPT: &str = r#"你替人看相。看得慢，说得准，不奉承，也不吓人。
你手里只有一个人在一个群里说过的话，和几组冷冰冰的数字。你从这些字句里读他的性情、处境与去处。

用三种眼光看他：
- 立身：他怎样对待做事、秩序、体面与成就。不猜他做什么工作，只看他与劳作、规矩、输赢的关系。
- 心相：他的欲望、恐惧、自欺，与他不肯承认的那一部分。他为什么这样说话，他在防什么，他在等什么。
- 人群：他在人堆里的位置。他怎样靠近，怎样退开，拿什么换被需要，付出与索取各占多少。

笔法：
- 白描。写你看得见的，不写你感叹的。不用比喻堆叠，不用排比，不用感叹号。
- 冷静、克制、精准。力道到了就停手。说穿，但不羞辱；不留情面，也不刻薄。
- 不用网络流行语，不用「其实」「说到底」「值得一提的是」这类垫话。
- 每句话都要有出处，出处就是下发的样本与数字。看不出就不写，宁可短。
- 可以指出他未必愿意承认的事，但不下道德判断。
- 不写外貌、性别、年龄、地域、收入、健康、政治立场；不臆断他做什么工作、住在哪里、跟谁是什么关系。
- 引语必须逐字出自下发的样本，一个字都不能改；找不到合适的就不给引语。
- 只输出一个 JSON 对象，不要代码块，不要解释，不要前后缀。

JSON 字段：
{
  "codename": "代号，2 到 7 个字。要准，不要好听，像熟人背后对他的称呼",
  "tagline": "题记，不超过 22 字。是结论，不是形容",
  "summary": "总评，3 到 4 句，不超过 100 字。先给判断，再给依据",
  "facets": [
    {"key":"立身","title":"这一面的结论，不超过 12 字","body":"不超过 100 字，白描，要有细节"},
    {"key":"心相","title":"这一面的结论，不超过 12 字","body":"不超过 100 字，白描，要有细节"},
    {"key":"人群","title":"这一面的结论，不超过 12 字","body":"不超过 100 字，白描，要有细节"}
  ],
  "traits": [{"name":"心理维度，2 到 4 字，如 秩序感 / 表达欲 / 防御 / 攻击性","score":0 到 100 的整数,"note":"这个分数从哪个细节看出来，不超过 30 字"}],
  "interests": ["他反复谈到的事，2 到 6 字，最多 6 个，按分量排序"],
  "lot": {
    "no": 1 到 100 的整数,
    "grade": "从 上上 / 上吉 / 中吉 / 中平 / 中下 里选一个",
    "verse": ["签诗四句，每句 5 到 9 个字，有古意，不掉书袋", "第二句", "第三句", "第四句"],
    "reading": "解签，不超过 70 字。把它接回这个人的处境，别说吉祥话"
  },
  "quotes": [{"text":"逐字引用的一条发言","why":"为什么挑它，不超过 24 字"}],
  "advice": "赠言，不超过 30 字。不劝善，不祝福，给他一样能带走的东西",
  "accent": "从 amber / rose / mint / indigo / violet / teal 里选一个当报告主色"
}

traits 给 4 个，分数要拉开，别都堆在七八十分。
签要像抽出来的，不像量身定做的吉利话；中平、中下也可以，解语要落到实处。"#;

/// 组装下发给模型的素材。统计在前、样本在后，模型先拿到骨架再看原文。
pub fn user_prompt(material: &Material) -> String {
    let mut out = String::with_capacity(8_192);
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
        "- 活跃时段：{} 前后最活跃；夜间（0—6 点）占 {}；最活跃的一天是{}\n",
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
            out.push_str(&format!("{}. {}\n", index + 1, sample));
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
            words: vec![("天气".into(), 12)],
            samples: vec![
                "今天这个雨下得没完没了".to_string(),
                "凌晨三点还在改代码，明天又要废了".to_string(),
            ],
        }
    }

    #[test]
    fn json_is_found_behind_fences_and_chatter() {
        let raw = "好的，这是画像：\n```json\n{\"codename\":\"夜行改稿人\",\"tagline\":\"白天潜水夜里冒泡\"}\n```\n希望有帮助";
        let persona = parse(raw).unwrap();
        assert_eq!(persona.codename, "夜行改稿人");
    }

    #[test]
    fn json_without_braces_is_an_error() {
        assert!(parse("我觉得他挺好的").is_err());
    }

    #[test]
    fn quotes_must_appear_in_the_samples() {
        let persona = Persona {
            quotes: vec![
                Quote {
                    text: "凌晨三点还在改代码，明天又要废了".into(),
                    why: "原话".into(),
                },
                Quote {
                    text: "我从来没说过这句话".into(),
                    why: "编的".into(),
                },
            ],
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(persona.quotes.len(), 1);
        assert!(persona.quotes[0].text.starts_with("凌晨三点"));
    }

    /// 标点与空白的差别不该让一条真原话被误判成编的。
    #[test]
    fn quotes_tolerate_punctuation_differences() {
        let persona = Persona {
            quotes: vec![Quote {
                text: "今天这个雨，下得没完没了！".into(),
                why: "原话".into(),
            }],
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(persona.quotes.len(), 1);
    }

    #[test]
    fn overlong_fields_are_clipped_and_scores_clamped() {
        let persona = Persona {
            codename: "这是一个特别特别长的代号".into(),
            traits: vec![
                Trait {
                    name: "话痨".into(),
                    score: 480.0,
                    note: "长".repeat(80),
                },
                Trait {
                    score: 50.0,
                    ..Default::default()
                },
            ],
            interests: vec!["同一个词".into(), "同一个词".into(), " ".into()],
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(persona.codename.chars().count(), limit::CODENAME + 1);
        // 空名字的特质被丢掉。
        assert_eq!(persona.traits.len(), 1);
        assert_eq!(persona.traits[0].score, 100.0);
        assert!(persona.traits[0].note.chars().count() <= limit::TRAIT_NOTE + 1);
        assert_eq!(persona.interests, vec!["同一个词".to_string()]);
        // 省略号前不留空白，别剪出「手机 root …」这种断口。
        let clipped = Persona {
            interests: vec!["abcdefghijk lmnop".into()],
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(
            clipped.interests,
            vec!["abcdefghij…".to_string()],
            "按 {} 字收口，省略号前不留空白",
            limit::INTEREST
        );
    }

    /// 三面侧写无论如何都要落在固定的三个位置上：键名写对、写别名、乱写都能兜住。
    #[test]
    fn facets_land_on_the_three_fixed_slots() {
        let named = Persona {
            facets: vec![
                Facet {
                    key: "人群".into(),
                    title: "他不抢话".into(),
                    body: "只在有人问到的时候接一句。".into(),
                },
                Facet {
                    key: "立身：与劳作的关系".into(),
                    title: "把手艺当退路".into(),
                    body: "写代码的时候最安静。".into(),
                },
            ],
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(named.facets.len(), 3);
        // 名字认得出，就按名字归位，不按顺序。
        assert_eq!(named.facets[0].title, "把手艺当退路");
        assert_eq!(named.facets[2].title, "他不抢话");
        // 缺的一面留空，版面自己会隐去。
        assert!(named.facets[1].title.is_empty());
        assert_eq!(named.live_facets().count(), 2);

        // 键名全认不出时按出现顺序补位。
        let unnamed = Persona {
            facets: vec![
                Facet {
                    key: String::new(),
                    title: "甲".into(),
                    body: String::new(),
                },
                Facet {
                    key: String::new(),
                    title: "乙".into(),
                    body: String::new(),
                },
                Facet {
                    key: String::new(),
                    title: "丙".into(),
                    body: String::new(),
                },
                Facet {
                    key: String::new(),
                    title: "丁".into(),
                    body: String::new(),
                },
            ],
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(
            unnamed
                .facets
                .iter()
                .map(|facet| facet.title.as_str())
                .collect::<Vec<_>>(),
            vec!["甲", "乙", "丙"]
        );
        // 键名由本层统一写死，不采信模型。
        assert!(unnamed.facets.iter().all(|f| FACET_KEYS.contains(&f.key.as_str())));
    }

    /// 签等只认白名单，乱写的退回中平；签号越界会被夹回来。
    #[test]
    fn lots_are_whitelisted_and_clamped() {
        let persona = Persona {
            lot: Lot {
                no: 480,
                grade: "大凶".into(),
                verse: (0..6).map(|i| format!("第{i}句")).collect(),
                reading: "长".repeat(200),
            },
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(persona.lot.no, 100);
        assert_eq!(persona.lot.grade, "中平");
        assert_eq!(persona.lot.verse.len(), limit::MAX_VERSE);
        assert!(persona.lot.reading.chars().count() <= limit::READING + 1);
        assert!(persona.has_lot());

        // 认得出签等就留原样。
        let graded = Persona {
            lot: Lot {
                grade: "上吉".into(),
                ..Default::default()
            },
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(graded.lot.grade, "上吉");
        // 没写签号时按用户号定一个，1 到 100 之间。
        assert!((1..=100).contains(&graded.lot.no));
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

    #[test]
    fn the_statistical_fallback_is_readable_on_its_own() {
        let persona = Persona::from_stats(&material());
        assert!(persona.estimated);
        assert_eq!(persona.traits.len(), 4);
        assert!(persona.traits.iter().all(|t| (4.0..=99.0).contains(&t.score)));
        assert_eq!(persona.quotes.len(), 2);
        assert_eq!(persona.facets.len(), 3);
        assert_eq!(
            persona
                .facets
                .iter()
                .map(|facet| facet.key.as_str())
                .collect::<Vec<_>>(),
            FACET_KEYS.to_vec()
        );
        assert!(persona.facets.iter().all(|facet| !facet.body.is_empty()));
        assert!(persona.summary.contains("400"));
        assert!(persona.has_lot());
        assert!(Accent::from_name(&persona.accent).is_some());
    }

    #[test]
    fn the_prompt_carries_both_numbers_and_samples() {
        let prompt = user_prompt(&material());
        assert!(prompt.contains("群聊发言 400 条"));
        assert!(prompt.contains("凌晨三点还在改代码"));
        assert!(prompt.contains("高频词"));
        // 提示词要点：三种眼光、逐字引用、白描。
        let system = system_prompt();
        assert!(system.contains("立身"));
        assert!(system.contains("心相"));
        assert!(system.contains("人群"));
        assert!(system.contains("逐字"));
        assert!(system.contains("白描"));
    }
}
