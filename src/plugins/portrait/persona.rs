//! 把素材交给模型，换回一份角色档案；模型不接时退回统计直出的那份。
//!
//! 一份画像分两层，从硬到软：
//!
//! - **观测**：语言指纹 [`Style`](super::collect::Style)、活跃节律、群内往来
//!   （[`super::collect::Tie`]）。全部由 [`super::collect`] 从库里数出来，不经过模型，
//!   可核验。这一层永远在，模型接不接都一样。
//! - **档案**：九个维度各一句判定（[`Facet`]），每条配一条依据。模型读观测与样本得出。
//!
//! 这一层只认三件事：
//!
//! 1. **输出是一个 JSON 对象**——靠宽松解析。
//! 2. **每条判定都有依据**——依据为空的整条丢掉。
//! 3. **把握不许冒认**——标了「明说」的，依据必须逐字出自样本；对不上就降成「可推」，
//!    不清空、不报错。这是这份画像「严谨」二字的落点：所有关于这个人的话都分了三档把握，
//!    而且最高的一档由代码验，不由模型自称。
//!
//! 投影层（MBTI、九型）已经整块拿掉。凭一个人打过的字给他定一个四字母的型，是把一次
//! 粗糙的归类说得像一次测量；换成九维档案之后，每一格都要指得出出处，读的人也就知道
//! 哪一句能信到什么程度。

use super::collect::{Material, Style, Tie};
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

/// 档案的十个维度。**不许模型自创**——认不出维度的整条丢掉。
///
/// 次序就是版面上的次序：先是他是个什么样的人，再是他喜欢什么、靠什么过活，
/// 然后是家里、年岁、读到哪儿、过去与将来，最后落在与人的往来上。
pub const FACETS: [(&str, &str); 10] = [
    ("性格", "TEMPERAMENT"),
    ("兴趣", "INTERESTS"),
    ("好恶", "TASTES"),
    ("生计", "LIVELIHOOD"),
    ("家庭", "FAMILY"),
    ("年岁", "AGE"),
    ("学历", "EDUCATION"),
    ("经历", "BIOGRAPHY"),
    ("志向", "ASPIRATION"),
    ("人际", "RELATIONSHIP"),
];

/// 把握的三档。它答的是「这一句话有多少把握」，不是「这条数据从哪来」——
/// 观测层的数据来源在版面上已按分节分开，档案层需要的正是把握。
pub const CERTAINTIES: [(&str, &str); 3] =
    [("明说", "STATED"), ("可推", "INFERRED"), ("待考", "OPEN")];

/// 每一档把握的意思，卡片上的图例与提示词共用这一份说法。
pub fn certainty_note(certainty: &str) -> &'static str {
    match certainty {
        "明说" => "他本人讲过，依据是他的原话",
        "待考" => "只有一处间接线索",
        _ => "多条线索指向同一个结论",
    }
}

/// 认一个维度名。模型常写成「性格特征」「经济状况」这类，含关键字就算它。
///
/// 次序有讲究：先到的先认，所以「家庭关系」落在家庭、「工作经历」落在生计。
/// 这两个都不算误判——模型被要求写正名，这里只是兜底。
fn facet_of(name: &str) -> Option<&'static str> {
    const TABLE: [(&str, &[&str]); 10] = [
        (
            "性格",
            &[
                "性格",
                "脾气",
                "性情",
                "temperament",
                "character",
                "personality",
            ],
        ),
        (
            "兴趣",
            &["兴趣", "爱好", "在意的事", "interests", "interest", "hobby"],
        ),
        (
            "好恶",
            &[
                "好恶", "喜好", "厌恶", "讨厌", "tastes", "taste", "dislike", "prefer",
            ],
        ),
        (
            "生计",
            &[
                "生计",
                "工作",
                "职业",
                "行业",
                "收入",
                "经济",
                "花钱",
                "livelihood",
                "work",
                "job",
                "occupation",
                "income",
            ],
        ),
        ("家庭", &["家庭", "家人", "出身", "住处", "老家", "family"]),
        ("年岁", &["年岁", "年龄", "年纪", "性别", "age", "gender"]),
        (
            "学历",
            &[
                "学历",
                "读书",
                "上学",
                "学校",
                "专业",
                "毕业",
                "education",
                "school",
                "study",
                "major",
            ],
        ),
        (
            "经历",
            &[
                "经历",
                "履历",
                "往事",
                "过去",
                "biography",
                "history",
                "past",
            ],
        ),
        (
            "志向",
            &[
                "志向",
                "目标",
                "梦想",
                "打算",
                "愿望",
                "aspiration",
                "goal",
                "dream",
            ],
        ),
        (
            "人际",
            &["人际", "关系", "朋友", "交往", "relationship", "friend"],
        ),
    ];
    let name = name.trim().to_ascii_lowercase();
    TABLE
        .iter()
        .find_map(|(canon, keys)| keys.iter().any(|key| name.contains(key)).then_some(*canon))
}

/// 认一档把握。认不出的一律算**可推**：没写明「他本人讲过」的，本来就不该按明说读。
fn certainty_of(name: &str) -> &'static str {
    const TABLE: [(&str, &[&str]); 3] = [
        (
            "明说",
            &[
                "明说", "说过", "明讲", "亲口", "stated", "explicit", "quote",
            ],
        ),
        (
            "待考",
            &[
                "待考",
                "存疑",
                "线索",
                "不确定",
                "open",
                "uncertain",
                "guess",
            ],
        ),
        ("可推", &["可推", "推断", "推定", "推测", "infer", "likely"]),
    ];
    let name = name.trim().to_ascii_lowercase();
    TABLE
        .iter()
        .find_map(|(canon, keys)| keys.iter().any(|key| name.contains(key)).then_some(*canon))
        .unwrap_or("可推")
}

/// 档案里的一格：一句判定，一条依据，一档把握。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Facet {
    #[serde(default, alias = "维度", alias = "dim", alias = "name")]
    pub dimension: String,
    #[serde(default, alias = "把握", alias = "confidence", alias = "certain")]
    pub certainty: String,
    #[serde(
        default,
        alias = "判定",
        alias = "一句话",
        alias = "verdict",
        alias = "label"
    )]
    pub verdict: String,
    #[serde(default, alias = "依据", alias = "basis", alias = "proof")]
    pub evidence: String,
    /// 依据本身是不是一句逐字原话（收口时验出来的，不由模型声明）。
    #[serde(skip)]
    pub quoted: bool,
}

impl Facet {
    fn new(dimension: &str, certainty: &str, verdict: String, evidence: String) -> Self {
        Self {
            dimension: dimension.to_string(),
            certainty: certainty.to_string(),
            verdict,
            evidence,
            quoted: false,
        }
    }

    /// 归一之后的维度。认不出的这一条会被丢掉。
    pub fn dim(&self) -> Option<&'static str> {
        facet_of(&self.dimension)
    }

    /// 归一之后的把握。
    pub fn tier(&self) -> &'static str {
        certainty_of(&self.certainty)
    }

    /// 把握对应的样式名，卡片上的图例与徽章都用它。
    pub fn tier_class(&self) -> &'static str {
        certainty_class(self.tier())
    }
}

/// 把握对应的样式名。
pub fn certainty_class(certainty: &str) -> &'static str {
    match certainty {
        "明说" => "stated",
        "待考" => "open",
        _ => "inferred",
    }
}

/// 戏说里的一枚标签：一个短词，配一句为什么。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FunLabel {
    #[serde(default, alias = "标签", alias = "name", alias = "tag")]
    pub label: String,
    #[serde(
        default,
        alias = "为什么",
        alias = "依据",
        alias = "why",
        alias = "basis"
    )]
    pub why: String,
}

/// 模型给某个往来对象写的一句话。
///
/// 只许给往来账上已有的人写：收口时按 `id` 对，名单外的一律丢掉。
/// 往来对象是数出来的，不是模型想出来的，这条钉子就在这儿。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TieReading {
    #[serde(default, alias = "user_id", alias = "qq", alias = "对象")]
    pub id: i64,
    #[serde(
        default,
        alias = "一句话",
        alias = "读法",
        alias = "line",
        alias = "text"
    )]
    pub line: String,
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

/// 上一份画像用过的三样话：称号、一句话、判词。
///
/// 这是这份东西不千篇一律的那一手。模型看不见别人手里的画像，不给它这份名单，
/// 十来个人就会拿到十来个「夜猫子」；判词还会批量套同一个句式
/// （「他不是 A，是 B」连着出现五遍，再好的句子也成了模板）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Recent {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub closing: String,
}

/// 一份可以交给模板渲染的画像。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Persona {
    /// 戏称：一句话把他归成一类，4—12 字。
    #[serde(default, alias = "称谓", alias = "name")]
    pub title: String,
    /// 一句话概括。六十个字，写满不截断。
    #[serde(default, alias = "题记", alias = "一句话", alias = "summary")]
    pub note: String,
    /// 他怎么说话的白描，从语言指纹与样本里读出来。
    #[serde(default, alias = "语言风格", alias = "voice")]
    pub style: String,
    /// 档案十维。收口后一个维度至多一条。
    #[serde(default, alias = "档案", alias = "十维")]
    pub facets: Vec<Facet>,
    /// 口头禅。逐字出自记录，收口时一条条核。
    #[serde(default, alias = "口癖", alias = "phrases")]
    pub catchphrases: Vec<String>,
    /// 戏说的标签墙：3—5 个短词，每个配一句为什么。
    #[serde(default, alias = "标签墙", alias = "fun")]
    pub labels: Vec<FunLabel>,
    /// 戏说小传：一段拿来玩的玩笑话，允许夸张。
    #[serde(default, alias = "小传", alias = "sketch")]
    pub sketch: String,
    /// 对往来对象的一句话读法，按 `id` 对上 [`Material::ties`]。
    #[serde(default, alias = "往来")]
    pub ties: Vec<TieReading>,
    /// 综述：段落与引语按序排列。
    #[serde(default, alias = "综述", alias = "passages")]
    pub profile: Vec<Passage>,
    /// 判词：整份画像收在最锋利的一句上，30—60 字。
    ///
    /// 与 [`Self::note`] 的分工：一句话定位在开头，先给人一个抓手；判词在末尾，
    /// 把整份读下来的东西收成一句。开头那句要准，末尾这句要狠。
    ///
    /// 字段名不叫 `verdict`：那个名字在 [`Facet`] 上是每一格的判定，
    /// 两处同名会让读提示词的人分不清哪句是哪个。
    #[serde(
        default,
        alias = "判词",
        alias = "一句判词",
        alias = "断语",
        alias = "kicker"
    )]
    pub closing: String,
    #[serde(default)]
    pub accent: String,
    /// 这份档案是模型写的，还是统计直出的。
    #[serde(skip)]
    pub estimated: bool,
}

/// 各字段的字数上限。模型偶尔会无视字数要求，这里统一收口，
/// 免得一个超长字段把整张卡的版面顶乱。
///
/// **预算是照「不截断」定的**：这份东西的信息全在字里，卡片上留一个省略号就等于丢一句话。
/// 所以每一档都留够——判定按两行算、依据按三行算、一句话按两行算——再往上才截。
mod limit {
    pub const TITLE: usize = 12;
    pub const NOTE: usize = 60;
    pub const STYLE: usize = 140;
    /// 一句判定与它那条依据。
    pub const VERDICT: usize = 40;
    pub const EVIDENCE: usize = 70;
    /// 戏说的一枚标签与它的理由。
    pub const FUN_LABEL: usize = 10;
    /// 理由按两行给：一条理由写在中间被截掉，比不写还差。
    pub const FUN_WHY: usize = 34;
    pub const MAX_FUN_LABELS: usize = 5;
    /// 戏说小传。
    pub const SKETCH: usize = 140;
    /// 一句口头禅。
    pub const PHRASE: usize = 14;
    pub const MAX_PHRASES: usize = 4;
    /// 给某个往来对象写的那一句。
    pub const TIE_LINE: usize = 40;
    pub const PASSAGE: usize = 200;
    pub const QUOTE: usize = 90;
    pub const QUOTE_NOTE: usize = 24;
    pub const MAX_PASSAGES: usize = 6;
    /// 判词：一句收束。按三行给——它是这份东西的落点，被截断就白写了。
    pub const CLOSING: usize = 60;
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
    samples
        .iter()
        .any(|sample| fingerprint(sample).contains(&finger))
}

/// 一句口头禅是不是真出自他的记录。
///
/// 两条路：逐字出现在样本里，或者正是 [`Material::phrases`] 里数出来的那条重复发言
/// （重复发言按原样比——它就是照抄的）。两条都走不通的丢掉。这条判据是「口头禅」
/// 那一节唯一的底气：卡片上印的那几句，一定是他真说过的原话。
pub fn phrase_is_from_material(text: &str, material: &Material) -> bool {
    let finger = fingerprint(text);
    if finger.chars().count() < 2 {
        return false;
    }
    if quote_is_from_samples(text, &material.samples) {
        return true;
    }
    material
        .phrases
        .iter()
        .any(|(phrase, _)| fingerprint(phrase) == finger)
}

/// 从一条依据里把引号包着的那句话取出来，取不到就是 `None`。
///
/// 依据写的是「他说『…』」这种形状时，真正能拿去比对的是引号里那句。
/// 取出来之后也用它当依据——版面上那句话才干净。
fn quoted_span(evidence: &str) -> Option<String> {
    const PAIRS: [(char, char); 3] = [('「', '」'), ('『', '』'), ('“', '”')];
    for (open, close) in PAIRS {
        let start = evidence.find(open)?;
        let rest = &evidence[start + open.len_utf8()..];
        let end = rest.find(close)?;
        let span = rest[..end].trim();
        if !span.is_empty() {
            return Some(span.to_string());
        }
    }
    None
}

/// 这条依据是不是他的原话。整条对得上算，引号里那句对得上也算。
///
/// 对得上时返回该用来当依据的那句（引号里的原话，或者整条依据本身）。
pub fn evidence_as_quote(evidence: &str, samples: &[String]) -> Option<String> {
    if quote_is_from_samples(evidence, samples) {
        return Some(evidence.trim().to_string());
    }
    let span = quoted_span(evidence)?;
    quote_is_from_samples(&span, samples).then_some(span)
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
    /// 收口：字数、维度、把握、依据、口头禅、引语、往来名单——一条都不放过。
    ///
    /// 四处不丢信息的做法，是这一层的分寸所在：
    /// - 维度认不出、判定或依据为空：整条丢掉（没维度的档案格子在版面上无处可放）。
    /// - 同一个维度给了多条：留第一条，多的丢掉（版面上一个维度只有一格）。
    /// - 标了「明说」但依据对不上原话：**降成「可推」**，不清空。模型可能只是把引号
    ///   写歪了，那句话仍然有信息；冒认的把握才是要修的那个东西。
    /// - 口头禅核不过的丢掉。戏说的标签不核——那一节本来就是拿来玩的；只有「明说」
    ///   的依据与口头禅这两处，代码要替读者把关。
    pub fn sanitize(mut self, material: &Material) -> Self {
        self.title = clip(&self.title, limit::TITLE);
        self.note = clip(&self.note, limit::NOTE);
        self.style = clip(&self.style, limit::STYLE);
        self.closing = clip(&self.closing, limit::CLOSING);

        let mut used: HashMap<&'static str, ()> = HashMap::new();
        let facets = std::mem::take(&mut self.facets);
        self.facets = facets
            .into_iter()
            .filter_map(|facet| {
                let dimension = facet.dim()?;
                if used.insert(dimension, ()).is_some() {
                    return None;
                }
                let verdict = clip(&facet.verdict, limit::VERDICT);
                let evidence = clip(&facet.evidence, limit::EVIDENCE);
                if verdict.is_empty() || evidence.is_empty() {
                    return None;
                }
                // 把握由代码定：标了明说就得拿得出他的原话，拿不出就降一档。
                let as_quote = evidence_as_quote(&evidence, &material.samples);
                let (certainty, evidence) = match (facet.tier(), as_quote) {
                    ("明说", Some(quote)) => ("明说", quote),
                    ("明说", None) => ("可推", evidence),
                    (tier, _) => (tier, evidence),
                };
                Some(Facet {
                    dimension: dimension.to_string(),
                    certainty: certainty.to_string(),
                    verdict,
                    evidence,
                    // 只有「明说」那一格的依据按原话排；「可推」的依据里也许夹着一句原话，
                    // 但整条依据不是他的话，就不该按引语排。
                    quoted: certainty == "明说",
                })
            })
            .collect();

        // 往来读法只许写名单上的人：id 对不上的丢掉，重复的留第一条。
        let known: HashMap<i64, &Tie> =
            material.ties.iter().map(|tie| (tie.user_id, tie)).collect();
        let mut seen: HashMap<i64, ()> = HashMap::new();
        self.ties = std::mem::take(&mut self.ties)
            .into_iter()
            .filter_map(|reading| {
                if !known.contains_key(&reading.id) || seen.insert(reading.id, ()).is_some() {
                    return None;
                }
                let line = clip(&reading.line, limit::TIE_LINE);
                (!line.is_empty()).then_some(TieReading {
                    id: reading.id,
                    line,
                })
            })
            .collect();

        // 口头禅：逐字核过才留下。核不掉的宁可少印几句——这一节的全部价值就在
        // 「他真的这么说过」，掺一句编的整节都不值钱了。
        let mut seen_phrases: HashMap<String, ()> = HashMap::new();
        self.catchphrases = std::mem::take(&mut self.catchphrases)
            .into_iter()
            .filter_map(|phrase| {
                let phrase = clip(&phrase, limit::PHRASE);
                if phrase.is_empty()
                    || !phrase_is_from_material(&phrase, material)
                    || seen_phrases.insert(fingerprint(&phrase), ()).is_some()
                {
                    return None;
                }
                Some(phrase)
            })
            .take(limit::MAX_PHRASES)
            .collect();

        // 戏说的标签墙：短词 + 一句为什么。空标签丢掉，同一个词只留一次。
        let mut seen_labels: HashMap<String, ()> = HashMap::new();
        self.labels = std::mem::take(&mut self.labels)
            .into_iter()
            .filter_map(|item| {
                let label = clip(&item.label, limit::FUN_LABEL);
                if label.is_empty() || seen_labels.insert(label.clone(), ()).is_some() {
                    return None;
                }
                Some(FunLabel {
                    label,
                    why: clip(&item.why, limit::FUN_WHY),
                })
            })
            .take(limit::MAX_FUN_LABELS)
            .collect();
        self.sketch = clip(&self.sketch, limit::SKETCH);

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

        self
    }

    /// 主色：模型挑的在色板里就用它，否则按用户号定色。
    pub fn accent(&self, seed: i64) -> Accent {
        Accent::from_name(&self.accent).unwrap_or_else(|| Accent::pick(seed))
    }

    /// 一个维度下的那一格。
    pub fn facet(&self, dimension: &str) -> Option<&Facet> {
        self.facets
            .iter()
            .find(|facet| facet.dim() == Some(dimension))
    }

    /// 有档案的维度数。版面上用来说「十格里写出了几格」。
    pub fn covered(&self) -> usize {
        FACETS
            .iter()
            .filter(|(name, _)| self.facet(name).is_some())
            .count()
    }

    /// 模型没接上时的兜底。
    ///
    /// 画像的骨头是观测，不是模型：语言指纹、活跃节律、群内往来本来就在手里，
    /// 照它们把报告排满，缺的只是档案那一层。版面上会标出来「这一层这次空着」——
    /// 不用伪精度去补十格没有依据的判定：没读过语义就编不出诚心的档案。
    pub fn from_stats(material: &Material) -> Self {
        let mut profile = vec![Passage {
            kind: "text".to_string(),
            body: format!(
                "这一次模型没有接上，档案十格里一格都没写。下面这些是照着他本人在群里的记录\
                 直接排的：{} 条发言，覆盖 {} 天，活跃 {} 天，单条平均 {:.1} 字，\
                 平均每天 {:.1} 条；{}前后最密，夜间（0—6 点）占 {}。",
                material.total,
                material.span_days(),
                material.active_days,
                material.avg_len(),
                material.per_day(),
                hour_label(material.peak_hour()),
                percent(material.night_ratio()),
            ),
            ..Default::default()
        }];
        // 口头禅是数出来的，不靠模型：原样重复过三遍以上的短句本来就在手里。
        // 这一节因此是模型没接时唯一照旧成立的那一块「读起来有人味」的东西。
        let catchphrases: Vec<String> = material
            .phrases
            .iter()
            .map(|(phrase, _)| clip(phrase, limit::PHRASE))
            .take(limit::MAX_PHRASES)
            .collect();
        if !catchphrases.is_empty() {
            profile.push(Passage {
                kind: "text".to_string(),
                body: format!(
                    "他反复原样说过这几句：{}。\
                     这几句是从记录里数出来的，他真这么说过，但为什么老说这几句，\
                     得读了那些话才知道。",
                    catchphrases
                        .iter()
                        .map(|phrase| format!("「{phrase}」"))
                        .collect::<Vec<_>>()
                        .join("、")
                ),
                ..Default::default()
            });
        }
        if !material.words.is_empty() {
            let top: Vec<String> = material
                .words
                .iter()
                .take(6)
                .map(|(word, count)| format!("{word}×{count}"))
                .collect();
            profile.push(Passage {
                kind: "text".to_string(),
                body: format!(
                    "他反复提到的词是{}。这些词是从同一批记录里数出来的，\
                     只说得出他常聊什么，说不出他对这些事是什么态度。",
                    top.join("、")
                ),
                ..Default::default()
            });
        }
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
            title: "还没看透的人".to_string(),
            note: format!(
                "模型这次没接上，只有 {} 条发言、{} 天记录里数得出来的那些",
                material.total,
                material.span_days()
            ),
            style: String::new(),
            facets: Vec::new(),
            catchphrases,
            labels: Vec::new(),
            sketch: String::new(),
            ties: Vec::new(),
            profile,
            // 判词是读出来的，不是数出来的：没有模型就没有判词。
            // 拿一句观测直出的话冒充判词，正好砸了这一节的招牌。
            closing: String::new(),
            accent: Accent::pick(material.user_id).name().to_string(),
            estimated: true,
        }
    }

    /// 这份画像里会被下一份躲开的那三样。
    ///
    /// 抽成一处是为了让「记什么」只有一个说法：排除表存的是这三样，
    /// 提示词里念的也是这三样，两边不会各记各的。
    pub fn stamp(&self) -> Recent {
        Recent {
            title: self.title.clone(),
            note: self.note.clone(),
            closing: self.closing.clone(),
        }
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

/// 这份东西是什么，先把它说清楚。
const SYSTEM_PROMPT: &str = r#"你在给一个群成员做一份**角色画像**。

这份东西是发在群里给大家看着玩的：群友拿它互相调侃、哈哈一笑，不会有人当真。
所以**你可以大胆下判断，判断偏了也没关系**——但每一句都要有影子（他说过的话、
他出现的时间、他聊的事）。凭空安一个身份不好笑，笑点是「还真是这样」。

它要有两层效果：**笑一下，然后安静一下。** 戏说那一节负责前一半，
综述与判词负责后一半——把那句「还真是这样」往下推一层，推到他自己都没说出口的地方。
一份读完只是好笑的画像，是没做完的。

读的人不认识他。读完要能说出：这是个什么样的人，喜欢什么、不喜欢什么，靠什么过活，
家里什么情况，多大年纪，读到哪儿，过去经历过什么，想做什么，在群里跟谁聊得来。

【档案十格】
十格固定，每格一句判定，配一条依据：
- 性格：脾气与待人的方式。说事先给结论还是先铺垫，对人是客气还是直来直去。
- 兴趣：长期在聊什么、钻研什么、反复回到哪个话题。
- 好恶：明确夸过的、明确嫌弃的。
- 生计：做什么的、在上班还是在读书、手头什么状态（在为什么花钱、在还什么账、有没有进项）。
- 家庭：家里人、成长的地方、如今的住处。
- 年岁：年龄段与性别。
- 学历：在读还是毕业了、学什么、学校什么样。
- 经历：他自己讲过的经历、换过的地方、有过的转变。
- 志向：想做什么、在准备什么、说过的不甘心。
- 人际：现实里的朋友、同事、伴侣，和这些人之间的事。**他在群里跟谁说话不进这一格**——
  那是数出来的，报告另有一节专门讲。

能写的格子尽量写满：十格是这份东西的骨架，空着读者就觉得没写透。实在没有影子的格子
留空，别硬编。有点影子但拿不准的，就照你的直觉写，把把握标成「待考」。

【把握三档】
- 明说：他本人讲过。依据必须是他原话的逐字摘录，标点可以不同、字要一样，依据只写那句
  话本身，前后不要加「他说」、不要加引号。系统会拿它跟原话逐字比对，对不上就自动降成
  「可推」。
- 可推：几条线索指向同一个结论。依据写清是哪几条——数字、时间，或者他的原话。
- 待考：只有一处间接线索，或者你就是在猜。这一档允许你放手猜，标出来就行。

三档都要用得上。他亲口讲过的事本来就是「明说」；猜出来的、但有意思的，标「待考」——
这份东西的好处正在于敢猜，也敢承认自己是在猜。

【可以玩的，与不可以玩的】
可以玩：他说过的职业、收入的样子、年纪、学历、感情、作息、口头禅，都可以拿来开玩笑。
把一个天天半夜修电脑的人说成「赛博流浪汉」、把一个在学校食堂吃到出神入化的人说成
「自助餐学霸」，都对——只要他的记录里有影子。夸张没关系，无中生有不行。

不可以玩的两样：**外貌**（胖瘦、美丑）与**健康**（病、身体缺陷）。这两样不好笑，只伤人。
还有一类不能写：能定位到具体个人的东西——真实姓名、门牌、单位全名、电话。

【称号与一句话】
- title：4 到 12 字的戏称，是这份东西的脸。要**只属于他**——优先用名词性的短标签
  （「赛博流浪汉」「自助餐学霸」「凌晨三点的报错客服」），不要「……的人」这种句式，
  不要「技术宅」「热心群友」这种换谁都能套的通用词。
- note：一句话，不超过 60 字，**写满，不要写一半**。里面至少要有一处只属于他的具体东西：
  一个数字、一个时间，或者一件他说过的事。

【语言】
下面会给一组「语言指纹」，全部从记录里数出来。不要复述数字，用它和样本白描出这个人
怎么说话：标点习惯，语气是冲是缓，爱不爱提问，说给自己听还是对着人说，话是匀称还是
时短时长。140 字内，写他本人，不写通用评语。不用形容词堆砌。

【口头禅】
下面会给一份「重复过的话」——他真的原样说过三遍以上的短句。挑 2 到 4 句当他的口头禅，
**必须逐字照抄**，一个字都不能改（改了就不是他说的话了）。给的候选不够就少给几句，
一条候选都没有就不给。也可以从样本里挑一句他反复说的话，同样要逐字。

【戏说】
- 标签墙：3 到 5 个短标签，每个不超过 10 字，像「赛博流浪汉」「自助餐学霸」
  「泡面美食家」「人形报错手册」。同样要**只属于他**，换个人就不成立。每个标签配一句
  为什么（不超过 34 字），落在他真做过的事上。
- 小传：一段 140 字以内的玩笑话。可以夸张、可以替他编心理活动，但每一句都要踩在他
  真做过的事上。写得像在跟群友讲他，不要写成评语。

【群内往来】
下面会给一份往来账：跟每个人，他 @ 对方几次、对方 @ 他几次，他接住对方的话几条、
对方接住他的几条，以及他对这个人说过的原话。点名是实打实的 @，**谁更主动看点名**；
接话是时间上紧跟在对方之后的那一条，**不等于回复**，读的时候不要当成回复。
给其中几个人各写一句：跟这个人聊什么、什么调子。谁更主动由系统判过了，别自己算比例。
只能给名单上的人写，名单以外的人不许添加。

**这一句里不要用孤零零的「他」**：画像对象写「他本人」，对方一律写「对方」。
两个人共用一个「他」，读的人分不清说的是谁——而卡片上紧挨着的那行数字里，
「他」说的又是对方。

【综述：这是亮刀的地方】
综述四到六段，末尾再收一句判词。档案那一节是拿证据说话的，戏说那一节是逗笑的，
这一节是**让人安静一下**的：把行为背后那个东西说出来，读者会「哦」一声，
然后有点不舒服，因为那说的也是他自己。

可以用的，用足了：
- 隐喻与借代：把他在群里的样子落到一个具体的东西或场景上——排班表、无人认领的公告栏、
  一直在响的取件码。比喻必须踩在一件真事上；踩不住的比喻是空话，删掉。
- 典故与成语：把他做过的事抬进一个大家都熟的框架里。生僻的不要用；
  用了还得解释一遍的，等于没用。
- 反问：把读者拉进来。「一个人把报错贴到凌晨三点，白天在忙什么？」
- 讽刺与反语：字面一层，指的是另一层。张力从这儿来。
- 说穿机制：不只说他做了什么，说**为什么他只能这么做**——这样活换到了什么，又付了什么。
  「给人启发」从这里来：读者认出的是处境，不是他这个人。
- 含沙射影只许影射「现象」：不许暗指群里其他具体的人，也不许影射现实里的第三者。

放开的是**写法**，不是**出处**：每一句判断后面仍然要有东西撑着——他说过的话、数字、
时间。一句话漂亮但没有事实在下面，照样删掉。比喻与典故是把事实照亮，不是拿来替事实。
至少两段把引语单独成段，前后用自己的话接住它。引语必须逐字出自下发的样本，一个字都
不能改；找不到合适的就不给引语。同一句引语不要在档案的依据或口头禅里再用一遍。
数字挑着用，一段里至多两三处。

准星只有一个：**靶子是他的做法与处境，不是他这个人的价值。**
这一份会发在群里，他自己也会看到。可以说一个人把日子过成了排班表，
不可以暗示他这人不行；可以说「舍不得关灯」，不可以说他可怜。

仍然要删的：
- 没有事实垫着的漂亮话。对仗、排比、金句，说了等于没说的，一句都不要。
- 说教的姿态：「希望他能……」「其实他需要的是……」。你不是他的长辈。
- 空洞的收尾：「而这，就是他的故事。」这类句子一句都别写。
- 评价词（很强、非常、厉害）、模糊限定（似乎、某种、大概）、「其实」「说到底」。
- 同一个数字报两遍，同一件事说两遍。

【判词】
整份画像的落点，30 到 60 字，独立成句。它要刺一下，也要留一点暖：说出他这样活着
**换到了什么、付了什么**。允许反问，允许只给一个画面让读者自己往下想。
它必须是整份里最锋利的那一句，不是前文的总结——把前文总结一遍等于没写。
前几份用过的判词会给你，**不要套同一个句式**。

形状是这几种（造句别抄这三句，抄形状）：
- 「他把白天让给了别的事，深夜才回来认领自己。」
- 「一个人能连着三年在同一个点上线，说明这个点有人在等他，或者没人等他。」
- 「群里最常报时间的人，往往是唯一一个还在意时间的人。」

只输出一个 JSON 对象，不要代码块，不要解释，不要前后缀。

JSON 字段：
{
  "title": "戏称，4 到 12 字，只属于他",
  "note": "一句话，不超过 60 字，写满",
  "style": "他怎么说话的白描，不超过 140 字",
  "facets": [
    {"dimension":"性格","certainty":"可推","verdict":"一句判定，不超过 40 字","evidence":"依据，不超过 70 字"}
  ],
  "catchphrases": ["逐字照抄的口头禅，不超过 14 字"],
  "labels": [
    {"label":"短标签，不超过 10 字","why":"为什么，不超过 34 字"}
  ],
  "sketch": "戏说小传，不超过 140 字",
  "ties": [
    {"id": 10001, "line": "跟这个人聊什么、什么调子，不超过 40 字；对方写「对方」，他本人写「他本人」"}
  ],
  "profile": [
    {"kind":"text","body":"一段综述，不超过 200 字"},
    {"kind":"quote","text":"逐字引用的一条发言","note":"这句话说明什么，不超过 24 字"}
  ],
  "closing": "判词，30 到 60 字，整份画像最锋利的那一句",
  "accent": "从 amber / rose / mint / indigo / violet / teal 里选一个当报告主色"
}

facets 的 dimension 只能写 性格 / 兴趣 / 好恶 / 生计 / 家庭 / 年岁 / 学历 / 经历 / 志向 / 人际，
certainty 只能写 明说 / 可推 / 待考，一个维度至多一条，没依据的整条不要给。
ties 的 id 只能取往来账上的号。profile 给 4 到 6 段，其中至少 2 段是 quote。
写完自己看一遍：有没有漂亮话没事实垫着，有没有说教的口气，有没有暗指群里别的人，
有没有同一个数报了两遍，称号和判词跟别人撞了没有，判词是不是把前文又总结了一遍。"#;

/// 组装下发给模型的素材。观测三块（统计、语言指纹、群内往来）加一块样本。
///
/// `recent` 是最近几份画像用过的「称号 + 一句话」，由调用方从进程状态里取。它是这份东西
/// 不千篇一律的那一手：模型看不见别人手里的画像，不给它这份名单，十来个人就会拿到
/// 十来个「技术宅 / 夜猫子」。
pub fn user_prompt(material: &Material, style: &Style, recent: &[Recent]) -> String {
    let mut out = String::with_capacity(16_000);

    out.push_str("【对象】\n");
    out.push_str(&format!("群名片：{}\n", material.name));
    out.push_str(&format!("QQ：{}\n\n", material.user_id));

    if !recent.is_empty() {
        out.push_str(
            "【最近几份画像用过的话】（**换个说法**：称号不许与它们相同或只差一两个字；\
             判词不许套它们用过的句式）\n",
        );
        for entry in recent {
            out.push_str(&format!("- {}｜{}\n", entry.title, entry.note));
            if !entry.closing.trim().is_empty() {
                out.push_str(&format!("  判词：{}\n", entry.closing));
            }
        }
        out.push('\n');
    }

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
        "- 活跃时段：{}前后最密；夜间（0—6 点）占 {}；周末占 {}；最活跃的一天是{}\n",
        hour_label(material.peak_hour()),
        percent(material.night_ratio()),
        percent(material.weekend_ratio()),
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

    out.push_str("\n【重复过的话】（他原样说过三遍以上的短句，逐字照抄才有用）\n");
    if material.phrases.is_empty() {
        out.push_str("（这段记录里没有重复到三遍的短句，口头禅从样本里自己挑，挑不到就不给）\n");
    } else {
        let phrases: Vec<String> = material
            .phrases
            .iter()
            .map(|(phrase, count)| format!("「{phrase}」×{count}"))
            .collect();
        out.push_str(&format!("{}\n", phrases.join("、")));
    }

    out.push_str("\n【群内往来】（数字从记录里数出；点名是 @，接话是紧跟在对方之后的下一条）\n");
    if material.ties.is_empty() {
        out.push_str("（这段记录里没有可辨认的往来对象，这一节不要写）\n");
    } else {
        for tie in &material.ties {
            out.push_str(&format!(
                "- {}（QQ {}）：我叫他 {} 次、他叫我 {} 次；我接他 {} 次、他接我 {} 次（{}）\n",
                tie.name,
                tie.user_id,
                tie.at_out,
                tie.at_in,
                tie.turn_out,
                tie.turn_in,
                tie.initiative(),
            ));
            if !tie.samples.is_empty() {
                let samples: Vec<String> = tie
                    .samples
                    .iter()
                    .map(|sample| format!("「{sample}」"))
                    .collect();
                out.push_str(&format!("  我对他说过：{}\n", samples.join("、")));
            }
        }
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
    use crate::plugins::portrait::collect::{GroupSlice, Kinds, Tie};

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

    fn tie() -> Tie {
        Tie {
            user_id: 10001,
            name: "老张".into(),
            at_out: 12,
            at_in: 4,
            turn_out: 30,
            turn_in: 9,
            last_time: 1_700_000_000,
            samples: vec!["这破依赖装了半天".into()],
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
                group_id: 100,
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
            phrases: vec![("这就去".into(), 5), ("不折腾了".into(), 3)],
            samples: vec![
                "今天这个雨下得没完没了".to_string(),
                "凌晨三点还在改代码，明天又要废了".to_string(),
            ],
            style: style(),
            ties: vec![tie()],
        }
    }

    fn facet(dimension: &str, certainty: &str, verdict: &str, evidence: &str) -> Facet {
        Facet {
            dimension: dimension.into(),
            certainty: certainty.into(),
            verdict: verdict.into(),
            evidence: evidence.into(),
            quoted: false,
        }
    }

    #[test]
    fn json_is_found_behind_fences_and_chatter() {
        let raw = "好的，这是档案：\n```json\n{\"title\":\"夜里的常客\",\"note\":\"白天基本不在\"}\n```\n希望有帮助";
        let persona = parse(raw).unwrap();
        assert_eq!(persona.title, "夜里的常客");
        assert_eq!(persona.note, "白天基本不在");
    }

    #[test]
    fn json_without_braces_is_an_error() {
        assert!(parse("我觉得他挺好的").is_err());
    }

    #[test]
    fn a_full_dossier_sanitises_end_to_end() {
        let raw = r#"{
            "title": "夜里的常客",
            "note": "白天基本不在，夜里话密起来",
            "style": "话短，句尾常带问号和省略号，像自言自语又像追问。",
            "facets": [
                {"dimension":"性格","certainty":"可推","verdict":"说事先给结论","evidence":"三条长发言都是先下判断再补理由"},
                {"dimension":"生计","certainty":"明说","verdict":"在上班，要早起","evidence":"凌晨三点还在改代码，明天又要废了"},
                {"dimension":"志向","certainty":"待考","verdict":"想把副业做起来","evidence":"提过一次副业，此后再没说过"}
            ],
            "ties": [{"id":10001,"line":"跟老张主要聊装机，抬杠居多"}],
            "profile": [
                {"kind":"quote","text":"凌晨三点还在改代码，明天又要废了","note":"拿休息换进度"},
                {"kind":"text","body":"他把手艺当退路。"}
            ],
            "accent": "indigo"
        }"#;
        let persona = parse(raw).unwrap().sanitize(&material());
        assert_eq!(persona.title, "夜里的常客");
        assert_eq!(persona.covered(), 3);
        assert_eq!(persona.facets[1].tier(), "明说");
        assert!(persona.facets[1].quoted, "依据对得上原话，标成引语");
        assert_eq!(persona.ties.len(), 1);
        assert_eq!(persona.profile[0].kind, "quote");
        assert_eq!(persona.profile[1].kind, "text");
    }

    /// 标了「明说」却拿不出原话，降成「可推」——不清空，冒认的是把握。
    #[test]
    fn a_stated_facet_without_a_real_quote_is_downgraded() {
        let persona = Persona {
            facets: vec![
                facet("生计", "明说", "在上班", "他说他每天要早起"),
                facet("家庭", "明说", "有孩子", "他说过要接孩子放学"),
            ],
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(persona.facets.len(), 2, "降级不是删除");
        assert!(persona.facets.iter().all(|f| f.tier() == "可推"));
        assert!(persona.facets.iter().all(|f| !f.quoted));
    }

    /// 依据写成「他说『…』」这种形状时，取出引号里那句来比对，并用它当依据。
    #[test]
    fn a_quote_inside_a_sentence_still_counts_as_stated() {
        let persona = Persona {
            facets: vec![facet(
                "生计",
                "明说",
                "在上班，要早起",
                "他说「凌晨三点还在改代码，明天又要废了」",
            )],
            ..Default::default()
        }
        .sanitize(&material());
        let facet = &persona.facets[0];
        assert_eq!(facet.tier(), "明说");
        assert_eq!(facet.evidence, "凌晨三点还在改代码，明天又要废了");
    }

    /// 维度归一：「性格特征」「经济状况」「年龄性别」都要落到正名上。
    #[test]
    fn dimensions_are_normalised_onto_the_ten() {
        let persona = Persona {
            facets: vec![
                facet("性格特征", "可推", "沉得住气", "从没见他催过"),
                facet("经济状况", "待考", "手头一般", "提过一次在还账"),
                facet("年龄性别", "可推", "二十出头", "提过学校"),
                facet(
                    "学历专业",
                    "可推",
                    "在读，学的是计算机",
                    "提过课表里的专业课",
                ),
                facet("家庭成员", "可推", "跟父母住", "提过他妈"),
                facet("人生目标", "可推", "想换行", "说不想一直做这个"),
            ],
            ..Default::default()
        }
        .sanitize(&material());
        let dims: Vec<&str> = persona.facets.iter().filter_map(|f| f.dim()).collect();
        assert_eq!(dims, vec!["性格", "生计", "年岁", "学历", "家庭", "志向"]);
        // 把握认不出时按可推算。
        let unknown = Persona {
            facets: vec![facet(
                "兴趣",
                "大概是吧",
                "爱折腾硬件",
                "三条记录都在这上头",
            )],
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(unknown.facets[0].tier(), "可推");
    }

    /// 认不出维度的、没判定的、没依据的，一律丢掉。
    #[test]
    fn a_facet_without_a_place_or_a_basis_is_dropped() {
        let persona = Persona {
            facets: vec![
                facet("星座", "可推", "大概是天蝎座", "没来由"),
                facet("性格", "可推", "  ", "有依据但没判定"),
                facet("兴趣", "可推", "爱折腾硬件", "  "),
                facet("志向", "可推", "想换行", "说过不想一直做这个"),
            ],
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(persona.facets.len(), 1);
        assert_eq!(persona.facets[0].dim(), Some("志向"));
    }

    /// 一个维度只有一格，多的丢掉。
    #[test]
    fn one_dimension_has_only_one_slot() {
        let persona = Persona {
            facets: vec![
                facet("性格", "可推", "第一条", "依据一"),
                facet("性格", "可推", "第二条", "依据二"),
                facet("性格", "可推", "第三条", "依据三"),
            ],
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(persona.facets.len(), 1);
        assert_eq!(persona.facets[0].verdict, "第一条");
    }

    /// 往来读法只许写在名单上的人身上。
    #[test]
    fn a_tie_reading_for_a_stranger_is_dropped() {
        let persona = Persona {
            ties: vec![
                TieReading {
                    id: 10001,
                    line: "跟老张主要聊装机".into(),
                },
                TieReading {
                    id: 99999,
                    line: "这个人根本不在名单上".into(),
                },
                TieReading {
                    id: 10001,
                    line: "重复的一条".into(),
                },
            ],
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(persona.ties.len(), 1);
        assert_eq!(persona.ties[0].id, 10001);
        assert_eq!(persona.ties[0].line, "跟老张主要聊装机");
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
    fn overlong_fields_are_clipped() {
        let persona = Persona {
            title: "这是一个特别特别长的综合速写".into(),
            note: "长".repeat(200),
            style: "风".repeat(300),
            closing: "判".repeat(200),
            facets: vec![facet("性格", "可推", &"判".repeat(200), &"依".repeat(300))],
            ties: vec![TieReading {
                id: 10001,
                line: "话".repeat(200),
            }],
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
        // 判词按三行给，不是拿来掐的：上限之外才截。
        assert!(persona.closing.chars().count() <= limit::CLOSING + 1);
        assert!(
            persona.closing.chars().count() > limit::CLOSING,
            "超了就该截"
        );
        assert!(persona.facets[0].verdict.chars().count() <= limit::VERDICT + 1);
        assert!(persona.facets[0].evidence.chars().count() <= limit::EVIDENCE + 1);
        assert!(persona.sketch.chars().count() <= limit::SKETCH + 1);
        assert!(persona.ties[0].line.chars().count() <= limit::TIE_LINE + 1);
        assert_eq!(persona.profile.len(), limit::MAX_PASSAGES);
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

    /// 兜底也是一份真报告：档案与戏说为空，观测层、数出来的口头禅与最长的原话都在。
    #[test]
    fn the_fallback_has_observations_and_an_empty_dossier() {
        let material = material();
        let persona = Persona::from_stats(&material);
        assert!(persona.estimated);
        assert!(persona.facets.is_empty(), "没读过语义就不给判定");
        assert_eq!(persona.covered(), 0);
        assert!(persona.style.is_empty());
        assert!(persona.ties.is_empty());
        assert!(persona.labels.is_empty(), "戏说要有模型才有");
        assert!(persona.sketch.is_empty());
        // 口头禅不靠模型：原样重复过的句子本来就在手里。
        assert_eq!(
            persona.catchphrases,
            vec!["这就去".to_string(), "不折腾了".to_string()]
        );
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

    /// 提示词要把十格、三档把握、戏说、口头禅与两条硬边都说清楚。
    #[test]
    fn the_prompt_states_the_ten_facets_the_grades_and_the_jokes() {
        let system = system_prompt();
        for needle in [
            "性格",
            "兴趣",
            "好恶",
            "生计",
            "家庭",
            "年岁",
            "学历",
            "经历",
            "志向",
            "人际",
            "明说",
            "可推",
            "待考",
            "语言指纹",
            "群内往来",
            "逐字",
            "不等于回复",
            // 娱乐向的三条：允许夸张、戏说、口头禅。
            "大胆下判断",
            "戏说",
            "标签墙",
            "口头禅",
            "赛博流浪汉",
            // 两条硬边。
            "外貌",
            "健康",
            "真实姓名",
            // 锋芒层：允许的五种写法、唯一那条准星、判词。
            "隐喻",
            "典故",
            "反问",
            "反语",
            "含沙射影",
            "靶子是他的做法与处境，不是他这个人的价值",
            "判词",
            "最锋利",
            // 放开写法不等于放开出处——这两条仍然要在。
            "必须踩在一件真事上",
            "每一句判断后面仍然要有东西撑着",
        ] {
            assert!(system.contains(needle), "提示词缺少 {needle}");
        }
        // 投影那一层整块拿掉了，提示词里再提它就是文档没跟上。
        assert!(!system.contains("MBTI"));
        assert!(!system.contains("九型"));
        // 旧的「这一节是白描」那条禁令已经作废，留着它就是把刀又收回鞘里。
        assert!(!system.contains("这一节是白描"));
        assert!(!system.contains("不用比喻"));
    }

    /// 口头禅逐字核验：编的、改过一个字的、不在候选里的，一条都留不下。
    #[test]
    fn catchphrases_must_be_verbatim() {
        let material = material();
        let persona = Persona {
            catchphrases: vec![
                // 候选里数出来的那句。
                "这就去".into(),
                // 样本里的原话（标点不同也算）。
                "今天这个雨，下得没完没了！".into(),
                // 改了一个字。
                "这就去吧".into(),
                // 编的。
                "我从来没说过这一句".into(),
                // 光是标点：他确实老发这一个，但印成「口头禅」念不出来。
                "（）".into(),
                "…".into(),
                // 重复的一条只留一次。
                "这就去".into(),
            ],
            ..Default::default()
        }
        .sanitize(&material);
        assert_eq!(
            persona.catchphrases,
            vec![
                "这就去".to_string(),
                "今天这个雨，下得没完没了！".to_string()
            ]
        );
    }

    /// 光是标点的重复不算口头禅：他确实老发「（）」，但那印出来念不出来。
    /// 这条在 `fingerprint` 抹掉标点之后自然成立——归一化只剩空串，走不到比对那一步。
    #[test]
    fn a_punctuation_only_habit_is_not_a_catchphrase() {
        let material = material();
        assert!(!phrase_is_from_material("（）", &material));
        assert!(!phrase_is_from_material("…", &material));
        assert!(!phrase_is_from_material("、、、", &material));
        // 夹着字的照旧算。
        assert!(phrase_is_from_material("这就去", &material));
    }

    /// 戏说的标签：空标签丢掉、同词去重、超量截断，理由跟着一起收。
    #[test]
    fn fun_labels_are_cleaned_and_capped() {
        let persona = Persona {
            labels: vec![
                FunLabel {
                    label: "赛博流浪汉".into(),
                    why: "天天半夜上线，白天见不着".into(),
                },
                FunLabel {
                    label: "赛博流浪汉".into(),
                    why: "重复的一枚".into(),
                },
                FunLabel {
                    label: "  ".into(),
                    why: "没有名字".into(),
                },
            ],
            sketch: "他的一天从下午四点开始。".into(),
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(persona.labels.len(), 1);
        assert_eq!(persona.labels[0].label, "赛博流浪汉");
        assert_eq!(persona.labels[0].why, "天天半夜上线，白天见不着");
        assert_eq!(persona.sketch, "他的一天从下午四点开始。");
    }

    /// 统计量、语言指纹、往来账与样本都必须随提示词一起下发。
    #[test]
    fn the_prompt_carries_the_numbers_the_ledger_and_the_samples() {
        let material = material();
        let prompt = user_prompt(&material, &style(), &[]);
        assert!(prompt.contains("群名片：甲"));
        assert!(prompt.contains("群聊发言 400 条"));
        assert!(prompt.contains("夜间（0—6 点）占"));
        assert!(prompt.contains("周末占"));
        assert!(prompt.contains("引用别人的消息 60 次"));
        assert!(prompt.contains("语言指纹"));
        assert!(prompt.contains("发言爆发指数"));
        assert!(prompt.contains("【群内往来】"));
        assert!(prompt.contains("我叫他 12 次、他叫我 4 次；我接他 30 次、他接我 9 次"));
        assert!(prompt.contains("我对他说过：「这破依赖装了半天」"));
        assert!(prompt.contains("凌晨三点还在改代码"));
        assert!(!prompt.contains("卦"), "画像里不该再出现筮法的说法");
    }

    /// 没有往来对象的那一节要照实说，不能让模型自己去编几个人。
    #[test]
    fn a_material_without_ties_says_so() {
        let mut material = material();
        material.ties.clear();
        let prompt = user_prompt(&material, &style(), &[]);
        assert!(prompt.contains("没有可辨认的往来对象"));
    }

    /// 重复过的话要随提示词下发——口头禅那一节全靠它。
    #[test]
    fn the_catchphrase_candidates_reach_the_prompt() {
        let material = material();
        let prompt = user_prompt(&material, &style(), &[]);
        assert!(prompt.contains("【重复过的话】"));
        assert!(prompt.contains("「这就去」×5"));
        // 一条候选都没有时要说清楚，别让模型以为漏了。
        let mut bare = material;
        bare.phrases.clear();
        assert!(user_prompt(&bare, &style(), &[]).contains("没有重复到三遍的短句"));
    }

    /// 最近几份用过的话要随提示词下发：这份东西不千篇一律就靠这一手。
    #[test]
    fn recent_titles_are_handed_to_the_model_to_avoid() {
        let material = material();
        let recent = vec![
            Recent {
                title: "赛博流浪汉".to_string(),
                note: "白天不在，夜里冒泡".to_string(),
                closing: "他把自己搬到了别人睡着的那个时段".to_string(),
            },
            Recent {
                title: "自助餐学霸".to_string(),
                note: "食堂三层都吃遍了".to_string(),
                closing: String::new(),
            },
        ];
        let prompt = user_prompt(&material, &style(), &recent);
        assert!(prompt.contains("【最近几份画像用过的话】"));
        assert!(prompt.contains("- 赛博流浪汉｜白天不在，夜里冒泡"));
        assert!(prompt.contains("- 自助餐学霸｜食堂三层都吃遍了"));
        // 判词跟着一起给：不给它，下一份就会套同一个句式。
        assert!(prompt.contains("判词：他把自己搬到了别人睡着的那个时段"));
        assert!(prompt.contains("不许与它们相同或只差一两个字"));
        assert!(prompt.contains("不许套它们用过的句式"));
        // 判词为空的那条不印一行空的。
        assert!(!prompt.contains("判词：\n"));
        // 一份都没发过时整块不出现，省下那段字。
        assert!(!user_prompt(&material, &style(), &[]).contains("最近几份画像用过"));
    }
}
