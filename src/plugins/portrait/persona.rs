//! 把一卦与素材交给模型，换回一份画像；模型不接时只留卦象与数字。
//!
//! 卦不在这里起——它由 [`super::divine`] 从这个人留下的话里起出来，是既定的。
//! 这一层只认三件事：**模型的输出是一个 JSON 对象**、**引语必须是原话**、
//! **批语是一篇文章的段落而不是条目**。第一条靠宽松解析（模型爱在 JSON 外面裹一句
//! 「好的」或一层代码块），第二条靠归一化比对——对不上就丢掉，宁可少一段引语，
//! 也不让报告里出现一句编出来的「他说过」；第三条落在 [`Passage`] 的形状上：
//! 段落与引语同列一队，按序渲染，引语落在论证中间，不是贴在文末。

use super::collect::Material;
use super::divine::{self, Cast};
use serde::Deserialize;
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

/// 批语里的一段。`kind` 决定它是自己的话还是自己的话。
///
/// 段落与引语排在同一列里，是为了让引语落在该落的地方：模型写完一段判断，
/// 紧接着把那个人说过的一句原话放上来当证，再接着往下说。这样报告是一篇，
/// 不是「正文一段 + 文末三条引语」。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Passage {
    /// `text` 是批语，`quote` 是引语；认不出来的一律当批语。
    #[serde(default)]
    pub kind: String,
    /// 批语的正文；引语时为空。
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
    /// 这一段的 kind 认不认得出是引语。认不出的按批语处理。
    pub fn is_quote(&self) -> bool {
        const WORDS: [&str; 4] = ["quote", "引语", "引文", "他的话"];
        let kind = self.kind.trim().to_ascii_lowercase();
        WORDS.iter().any(|word| kind.contains(word))
    }
}

/// 一份可以交给模板渲染的画像。卦不在其中——它由代码起出来，另行带进模板。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Persona {
    #[serde(default)]
    pub codename: String,
    #[serde(default)]
    pub tagline: String,
    /// 总断：把这一卦与他接上的那一段。
    #[serde(default)]
    pub verdict: String,
    /// 长批：段落与引语按序排列。
    #[serde(default)]
    pub passages: Vec<Passage>,
    /// 变：他现在卡在哪、往哪动。
    #[serde(default)]
    pub turn: String,
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
    pub const VERDICT: usize = 220;
    pub const PASSAGE: usize = 400;
    pub const QUOTE: usize = 90;
    pub const QUOTE_NOTE: usize = 30;
    pub const TURN: usize = 180;
    pub const ADVICE: usize = 34;
    pub const MAX_PASSAGES: usize = 9;
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
    /// 收口：字数、段数，以及引语必须出自样本。
    pub fn sanitize(mut self, material: &Material) -> Self {
        self.codename = clip(&self.codename, limit::CODENAME);
        self.tagline = clip(&self.tagline, limit::TAGLINE);
        self.verdict = clip(&self.verdict, limit::VERDICT);
        self.turn = clip(&self.turn, limit::TURN);
        self.advice = clip(&self.advice, limit::ADVICE);

        let samples: Vec<String> = material.samples.iter().map(|s| fingerprint(s)).collect();
        self.passages = self
            .passages
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

    /// 模型完全没接上时的兜底：卦已经起好了，交给版面的只剩数字与一句实话。
    ///
    /// 卦不依赖模型，所以哪怕模型整个不接，用户拿到的仍是一张真卦——
    /// 缺的只是把卦落在他身上的那几段批语。
    pub fn from_stats(material: &Material, cast: &Cast) -> Self {
        let verdict = format!(
            "这一卦是真的：以他 {} 条发言、{} 天的记录起出来，{}。数字也都在——\
             平均每天 {:.1} 条，单条平均 {:.1} 字，{} 的发言带着图或表情。\
             缺的是把卦落在他身上的那段批语，这一次没有落下。",
            material.total,
            material.span_days(),
            cast.primary.full,
            material.per_day(),
            material.avg_len(),
            percent(material.media_ratio()),
        );

        let mut passages = vec![
            Passage {
                kind: "text".to_string(),
                body: format!(
                    "统计窗口内共 {} 条群聊发言，覆盖 {} 天，活跃 {} 天。\
                     单条最长 {} 字，平均 {:.1} 字，{} 前后最活跃。",
                    material.total,
                    material.span_days(),
                    material.active_days,
                    material.longest,
                    material.avg_len(),
                    hour_label(material.peak_hour()),
                ),
                ..Default::default()
            },
            Passage {
                kind: "text".to_string(),
                body: format!(
                    "{}：{}按卦看，这是他要走的一段路；但为什么要走、走到哪儿去，\
                     要靠他说过的话才能答。这一版只有数目，答不了，也就不猜。",
                    cast.primary.full, cast.primary.sense,
                ),
                ..Default::default()
            },
        ];
        // 最长的那句原话当引语：它必然出自样本，用来撑住版面最省事。
        if let Some(text) = material
            .samples
            .iter()
            .filter(|text| !text.trim().is_empty())
            .max_by_key(|text| text.chars().count())
        {
            passages.push(Passage {
                kind: "quote".to_string(),
                text: clip(text, limit::QUOTE),
                note: "他写得最长的一条".to_string(),
                ..Default::default()
            });
        }

        Self {
            codename: "卦在，批未成".to_string(),
            tagline: format!("以 {} 条发言起出{}", material.total, cast.primary.full),
            verdict,
            passages,
            turn: String::new(),
            advice: "这次模型没接上，先给你一卦。过会儿再来一次。".to_string(),
            accent: Accent::pick(material.user_id).name().to_string(),
            estimated: true,
        }
    }

    /// 批语里真正有内容的那几段。空的会在版面上隐去。
    pub fn live_passages(&self) -> impl Iterator<Item = &Passage> {
        self.passages.iter().filter(|passage| {
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

const SYSTEM_PROMPT: &str = r#"你替人看卦。卦已经起好了，是他自己的卦——由他留下的话与数目推出来，
不是抽的签，也不是你挑的。你只做一件事：用这一卦的道理，把这个人说清楚。

下笔之前先立三条：
一、卦就是人。卦辞与义理说的不是旁人的吉凶，是他此刻的处境与性情。他哪里像这一卦、
   哪里不像，都要落在他真说过的话上。不许把卦套在他头上当帽子和标签。
二、卦是变化，不是判决。六爻里的变爻是正在动的地方，之卦是动的方向。不许他富贵，
   不许他祸福，只说这一卦的道理在他身上怎么应、该往哪儿使劲。
三、宁少说不空说。每一句都要能从下发的样本与数字里找出处。看不出就不写。

写出来的是一篇，不是一份清单：
- 总断：把卦与他接上，先给判断，再给依据。别复述卦辞，要说这一卦为什么是他的卦。
- 长批：五到八段，段与段之间要承接，像一个人把一件事从头说下来，不许写成并列的条目。
  不要把三个侧面拆成三块，也不要一二三四地分点。
- 批里至少两段是自己的话：把引语单独成段，前后用自己的话接住它，让这句话落在论证里。
  引语是证据，不是装饰。
- 变：他现在卡在哪一处，往哪儿动。变爻动的地方就是那处。
- 赠言：不劝善，不祝福，给他一样能带走的东西。

笔法：
- 白描。写你看得见的，不写你感叹的。不用比喻堆叠，不用排比，不用感叹号。
- 冷静、克制、精准。说穿，但不羞辱；不留情面，也不刻薄。
- 不用网络流行语，不用「其实」「说到底」「值得一提的是」这类垫话。
- 可以指出他未必愿意承认的事，但不下道德判断。
- 不写外貌、性别、年龄、地域、收入、健康、政治立场；不臆断他做什么工作、住在哪里、
  跟谁是什么关系。
- 引语必须逐字出自下发的样本，一个字都不能改；找不到合适的就不给引语。
- 只输出一个 JSON 对象，不要代码块，不要解释，不要前后缀。

JSON 字段：
{
  "codename": "代号，2 到 7 个字。要准，不要好听，像熟人背后对他的称呼",
  "tagline": "题记，不超过 22 字。是结论，不是形容",
  "verdict": "总断，2 到 4 句，不超过 200 字。先把这一卦与他的关系说定，再给依据",
  "passages": [
    {"kind":"text","body":"一段批语，不超过 300 字"},
    {"kind":"quote","text":"逐字引用的一条发言","note":"把这句话放在这里说明什么，不超过 24 字"},
    {"kind":"text","body":"接着往下说，与上一段接得上"}
  ],
  "turn": "说变，2 到 3 句，不超过 160 字",
  "advice": "赠言，不超过 30 字",
  "accent": "从 amber / rose / mint / indigo / violet / teal 里选一个当报告主色"
}

passages 给 5 到 8 段，其中至少 2 段是 kind 为 quote 的引语，其余是 text。
段落是一篇文章的段落，前一段的末尾要能接上后一段的开头。"#;

/// 组装下发给模型的素材。卦在最前——它先给这件事定框，其余的都在框里读。
pub fn user_prompt(material: &Material, cast: &Cast) -> String {
    let mut out = String::with_capacity(12_288);

    out.push_str("【卦】卦已经起好，是照他留下的话与数目起的，不用你再起\n");
    out.push_str(&format!(
        "本卦：{}（第 {} 卦，{}）\n",
        cast.primary.full,
        cast.primary.number,
        cast.primary.trigrams()
    ));
    out.push_str(&format!("卦辞：{}\n", cast.primary.judgment));
    out.push_str(&format!("义理：{}\n", cast.primary.sense));
    out.push_str(&format!(
        "筮法：大衍筮法，四十九策三变成爻，初爻到上爻的策数为 {}\n",
        cast.stalks_text()
    ));
    let mut titles = cast.changing_titles();
    if titles.is_empty() {
        out.push_str("变爻：无\n");
    } else {
        for (index, title) in titles.drain(..).enumerate() {
            let position = cast.changing[index];
            out.push_str(&format!(
                "变爻：{}，{}（{}）\n",
                title, divine::POSITION_SENSE[position], position_label(position)
            ));
        }
    }
    out.push_str(&format!("占法：{}\n", cast.rule()));
    if let Some(changed) = cast.changed {
        out.push_str(&format!(
            "之卦：{}（第 {} 卦，{}）\n",
            changed.full, changed.number, changed.trigrams()
        ));
        out.push_str(&format!("之卦卦辞：{}\n", changed.judgment));
        out.push_str(&format!("之卦义理：{}\n", changed.sense));
    }

    out.push_str("\n【对象】\n");
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
            let _ = writeln!(out, "{}. {}", index + 1, sample);
        }
    }
    out
}

fn position_label(index: usize) -> &'static str {
    const LABELS: [&str; 6] = ["初爻", "二爻", "三爻", "四爻", "五爻", "上爻"];
    LABELS[index.min(5)]
}

pub fn system_prompt() -> &'static str {
    SYSTEM_PROMPT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::portrait::collect::{GroupSlice, Kinds};
    use crate::plugins::portrait::divine;

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

    fn cast() -> Cast {
        divine::cast(&material())
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

    /// 引语必须逐字出自样本；编出来的一句都留不下。批语不受这条限制。
    #[test]
    fn quotes_must_appear_in_the_samples() {
        let persona = Persona {
            passages: vec![
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
        assert_eq!(persona.passages.len(), 2);
        assert_eq!(persona.passages[0].kind, "text");
        assert_eq!(persona.passages[1].kind, "quote");
        assert!(persona.passages[1].text.starts_with("凌晨三点"));
    }

    /// 标点与空白的差别不该让一条真原话被误判成编的。
    #[test]
    fn quotes_tolerate_punctuation_differences() {
        let persona = Persona {
            passages: vec![Passage {
                kind: "引语".into(),
                text: "今天这个雨，下得没完没了！".into(),
                note: "原话".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(persona.passages.len(), 1);
        assert_eq!(persona.passages[0].kind, "quote");
    }

    /// 认不出来的 kind 当批语处理；空的段落一律丢掉。
    #[test]
    fn unknown_kinds_become_prose_and_blanks_disappear() {
        let persona = Persona {
            passages: vec![
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
        assert_eq!(persona.passages.len(), 2);
        assert!(persona.passages.iter().all(|p| p.kind == "text"));
        assert_eq!(persona.passages[1].body, "下一段接着说。");
    }

    #[test]
    fn overlong_fields_are_clipped_and_passages_are_capped() {
        let persona = Persona {
            codename: "这是一个特别特别长的代号".into(),
            verdict: "长".repeat(600),
            passages: (0..20)
                .map(|index| Passage {
                    kind: "text".into(),
                    body: format!("第{index}段"),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
        .sanitize(&material());
        assert_eq!(persona.codename.chars().count(), limit::CODENAME + 1);
        assert!(persona.verdict.chars().count() <= limit::VERDICT + 1);
        assert_eq!(persona.passages.len(), limit::MAX_PASSAGES);

        let clipped = Persona {
            passages: vec![Passage {
                kind: "text".into(),
                body: "字".repeat(900),
                ..Default::default()
            }],
            ..Default::default()
        }
        .sanitize(&material());
        assert!(clipped.passages[0].body.chars().count() <= limit::PASSAGE + 1);
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

    /// 兜底画像带着真卦：模型不接，卦仍在，且引语一定出自样本。
    #[test]
    fn the_fallback_keeps_the_hexagram_and_a_real_quote() {
        let material = material();
        let cast = cast();
        let persona = Persona::from_stats(&material, &cast);
        assert!(persona.estimated);
        assert!(persona.verdict.contains(&cast.primary.full));
        assert!(persona.passages.len() >= 2);
        assert!(persona.live_passages().count() >= 2);
        assert!(persona.verdict.contains("400"));
        let quotes: Vec<&Passage> = persona
            .passages
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

    /// 提示词要把卦摆在最前，连同卦辞、义理、策数、变爻与占法一起下发。
    #[test]
    fn the_prompt_opens_with_the_hexagram() {
        let material = material();
        let cast = cast();
        let prompt = user_prompt(&material, &cast);
        assert!(prompt.starts_with("【卦】"));
        assert!(prompt.contains(&cast.primary.full));
        assert!(prompt.contains(cast.primary.judgment));
        assert!(prompt.contains("大衍筮法"));
        assert!(prompt.contains(&cast.stalks_text()));
        assert!(prompt.contains(cast.rule()));
        assert!(prompt.contains("群聊发言 400 条"));
        assert!(prompt.contains("凌晨三点还在改代码"));
        assert!(prompt.contains("高频词"));

        let system = system_prompt();
        // 提示词要点：卦是人、卦是变化、一篇不是清单、逐字引用、白描。
        assert!(system.contains("卦就是人"));
        assert!(system.contains("卦是变化"));
        assert!(system.contains("不是一份清单"));
        assert!(system.contains("逐字"));
        assert!(system.contains("白描"));
    }

    /// 有变爻时，变爻的爻题与爻位之义都要下发；有之卦时，之卦的卦辞也要。
    #[test]
    fn moving_lines_and_the_changed_hexagram_are_spelled_out() {
        let material = material();
        // 找一个有变爻的种子。
        let mut seed = 1u64;
        let cast = loop {
            let candidate = divine::cast_from(seed);
            if !candidate.changing.is_empty() && candidate.changed.is_some() {
                break candidate;
            }
            seed += 1;
        };
        let prompt = user_prompt(&material, &cast);
        for title in cast.changing_titles() {
            assert!(prompt.contains(&title), "缺少变爻 {title}");
        }
        let changed = cast.changed.unwrap();
        assert!(prompt.contains(&format!("之卦：{}", changed.full)));
        assert!(prompt.contains(changed.judgment));
    }
}
