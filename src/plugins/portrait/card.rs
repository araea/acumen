//! 画像卡（HTML → 截图）。
//!
//! 版式按「一份档案」来排，不按仪表盘来排。它有两层，从硬到软，版面也照着这个次序走：
//!
//! 1. **观测**：怎么说话（语言指纹）、什么时候来（24 小时与一周的分布）、群内往来
//!    （跟谁聊得来）。全篇算得出来的东西，排在档案之后紧接着铺开。
//! 2. **档案**：九个维度各一句判定，每条带一档把握与一条依据，是全篇唯一「读出来」的部分，
//!    所以每条都挂着把握徽章，顶上写明三档各是什么意思。
//!
//! 一句话定位压在最前——它是这份东西的脸。往下先立档案（他是谁）、再摆戏说（拿来玩的），
//! 然后是观测（凭什么这么说），
//! 最后是综述与页脚。
//!
//! 版面有两处图形，都是真数据：一排 24 小时的柱（什么时候来）与每条往来的双向条
//! （谁更主动）。不画没有出处的图。
//!
//! 约束与本仓库其它卡片一致：不加载任何外部资源（字体、图片、脚本都不引；头像由
//! [`super::avatar`] 先下回来，以 data URL 内嵌），所有动态文本一律转义，出图交给
//! [`crate::render::web::shoot`]——量高、等字体、尺寸护栏与闸门都在那一处。
//! 本文件里的版式只摆位置，不写色值、字号、圆角、阴影的字面量，一律取令牌。

use super::collect::Material;
use super::persona::{CERTAINTIES, FACETS, Persona, certainty_class, certainty_note};
use anyhow::Result;
use chrono::{DateTime, FixedOffset, Timelike, Utc};
use std::collections::HashMap;

/// 卡片渲染宽度（CSS 像素）。出图宽度 = `WIDTH × scale`。
const WIDTH: u32 = 720;
/// 高度上限（CSS 像素），与其它卡片一致。
const CAPTURE_MAX_HEIGHT: f64 = 16_000.0;
/// 柱状图里最矮的一根（百分比）。有数但很少的那几个小时也要看得见。
const MIN_BAR: u64 = 6;

/// 明暗两套主题。切换只动明度与文字三档灰，不动版式。两套都是纸色：日读偏暖白，
/// 夜读偏墨黑，衬线字落在上面才不显生。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
}

impl Theme {
    /// `auto` 在北京时间 07:00—18:59 用日读，其余时间用夜读。
    pub fn resolve(mode: &str, now: DateTime<FixedOffset>) -> Self {
        match mode.trim().to_ascii_lowercase().as_str() {
            "light" | "day" | "白天" | "日间" => Theme::Light,
            "dark" | "night" | "夜晚" | "夜间" => Theme::Dark,
            _ if (7..19).contains(&now.hour()) => Theme::Light,
            _ => Theme::Dark,
        }
    }

    fn vars(self) -> &'static str {
        match self {
            Theme::Light => "",
            Theme::Dark => " dark",
        }
    }
}

fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// 四位数以上的计数加千分位。
fn fmt_num(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn stamp(now: DateTime<FixedOffset>) -> String {
    now.format("%Y-%m-%d %H:%M").to_string()
}

fn date_of(timestamp: i64, offset: FixedOffset) -> String {
    DateTime::from_timestamp(timestamp, 0)
        .map(|utc| utc.with_timezone(&offset).format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// 头像用的那个字：取名字里第一个汉字或字母数字，挑不出就用「群」。
fn initial(name: &str) -> String {
    name.chars()
        .find(|ch| ch.is_alphanumeric())
        .or_else(|| name.chars().find(|ch| !ch.is_whitespace()))
        .map(|ch| ch.to_string())
        .unwrap_or_else(|| "群".to_string())
}

/// 一份画像报告要用到的全部信息。
pub struct View<'a> {
    pub material: &'a Material,
    pub persona: &'a Persona,
    /// 对象头像的 data URL，见 [`super::avatar`]；取不到时是 `None`，改用名字首字。
    pub avatar: Option<&'a str>,
    /// 往来对象各自的头像，键是 QQ 号，见 [`super::avatar`]。少一个就少一个人用首字顶着。
    pub faces: &'a HashMap<i64, String>,
    pub model: &'a str,
    /// 主题模式：`auto` / `light` / `dark`，见 [`Theme::resolve`]。
    pub theme: &'a str,
    /// 页脚那条能立刻执行的下一步（`画像 @某人`），前缀取当前配置。空的就整条不印。
    pub command: &'a str,
    pub offset: FixedOffset,
    pub now: DateTime<FixedOffset>,
}

/// 渲染成整页 HTML（截图用）。
pub fn html(view: &View<'_>) -> String {
    let theme = Theme::resolve(view.theme, view.now);
    let accent = view.persona.accent(view.material.user_id);
    let css = format!("{}{}", crate::render::web::DESIGN_SYSTEM, CSS);

    format!(
        r#"<!DOCTYPE html><html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:"><style>{css}</style></head>
<body class="scheme-portrait md-text {seed}{theme}"><div class="shot"><div class="card md-card">
{eyebrow}
{hero}
{headline}
{dossier}
{fun}
{voice}
{catchphrases}
{rhythm}
{ties}
{profile}
{foot}
</div></div></body></html>"#,
        css = css,
        seed = accent.seed_class(),
        theme = theme.vars(),
        eyebrow = eyebrow(view),
        hero = hero(view),
        headline = headline(view.persona),
        dossier = dossier(view.persona),
        fun = fun(view.persona),
        voice = voice(view.material, view.persona),
        catchphrases = catchphrases(view.persona),
        rhythm = rhythm(view.material),
        ties = ties(view.material, view.persona, view.faces),
        profile = profile(view.persona),
        foot = foot(view),
    )
}

fn sec_head(mark: &str, en: &str) -> String {
    format!(
        r#"<div class="sec-head"><span class="sec-mark">{}</span><span class="sec-en">{}</span></div>"#,
        esc(mark),
        esc(en)
    )
}

fn eyebrow(view: &View<'_>) -> String {
    format!(
        r#"<div class="md-eyebrow"><div class="md-kicker"><span class="md-dot"></span>角色画像<span class="md-kicker-en">CHARACTER DOSSIER</span></div><span class="md-stamp">{}</span></div>"#,
        esc(&stamp(view.now)),
    )
}

fn hero(view: &View<'_>) -> String {
    let material = view.material;
    let bits = [
        format!("QQ {}", material.user_id),
        format!("{} 个群", material.groups.len().max(1)),
        format!("{} 起", date_of(material.first_time, view.offset)),
    ];
    // 有头像用头像，没有就用名字首字顶着，版位不变。
    let face = match view.avatar {
        Some(src) => format!(r#"<img src="{}" alt="">"#, esc(src)),
        None => esc(&initial(&material.name)),
    };
    format!(
        r#"<div class="hero"><div class="avatar">{face}</div><div class="who"><div class="who-name">{}</div><div class="who-meta">{}</div><div class="md-readings readings-top">{readings}</div></div></div>"#,
        esc(&material.name),
        bits.join(r#"<span class="md-sep">·</span>"#),
        readings = readings(view.material),
    )
}

/// 一行读数。数字只做旁证，不铺成仪表盘。
///
/// 只留四格：「均长」不在这里——它属于怎么说话，同一件事不说两遍。
fn readings(material: &Material) -> String {
    let items = [
        ("发言", fmt_num(material.total), ""),
        ("活跃", fmt_num(material.active_days), "天"),
        ("跨度", material.span_days().to_string(), "天"),
        ("充分性", material.sufficiency().label().to_string(), ""),
    ];
    items
        .into_iter()
        .map(|(label, value, unit)| {
            let unit = if unit.is_empty() {
                String::new()
            } else {
                format!(r#"<span class="md-reading-unit">{}</span>"#, esc(unit))
            };
            format!(
                r#"<span class="md-reading"><span class="md-reading-key">{}</span><b class="md-reading-value">{}</b>{unit}</span>"#,
                esc(label),
                esc(&value)
            )
        })
        .collect()
}

/// 一句话定位——这份东西的脸：一个戏称加一句话。模型没接时挂一枚筹码说明。
fn headline(persona: &Persona) -> String {
    let pill = if persona.estimated {
        r#"<span class="md-chip">模型未接，档案由观测直出</span>"#.to_string()
    } else {
        String::new()
    };
    let note = if persona.note.trim().is_empty() {
        String::new()
    } else {
        format!(r#"<div class="note">{}</div>"#, esc(&persona.note))
    };
    format!(
        r#"<div class="headline"><div class="label-row"><span class="label">一句话定位</span>{pill}</div><div class="title">{}</div>{note}</div>"#,
        esc(if persona.title.is_empty() {
            "尚未归纳"
        } else {
            &persona.title
        }),
    )
}

/// 人物档案：九个维度各一格，每格一句判定、一档把握、一条依据。
///
/// 三档把握是这份东西的诚实所在，所以图例放在最前，徽章跟着每一格走——
/// 判断一个人是什么样，读者得先看得见这句话有多少把握。
fn dossier(persona: &Persona) -> String {
    let legend: String = CERTAINTIES
        .iter()
        .map(|(name, en)| {
            format!(
                r#"<div class="legend-item"><b class="{}">{}</b><i>{}</i><span>{}</span></div>"#,
                certainty_class(name),
                esc(name),
                esc(en),
                esc(certainty_note(name)),
            )
        })
        .collect();

    let rows: String = FACETS
        .iter()
        .filter_map(|(name, _)| {
            let facet = persona.facet(name)?;
            let evidence = if facet.quoted {
                format!(r#"<blockquote class="facet-quote">{}</blockquote>"#, esc(&facet.evidence))
            } else {
                format!(r#"<div class="facet-ev">{}</div>"#, esc(&facet.evidence))
            };
            Some(format!(
                r#"<div class="facet"><div class="facet-name">{}</div><div class="facet-body"><div class="facet-verdict">{}</div>{evidence}</div><span class="cert {}">{}</span></div>"#,
                esc(name),
                esc(&facet.verdict),
                certainty_class(facet.tier()),
                esc(facet.tier()),
            ))
        })
        .collect();

    // 十格全空：模型没接上，或这次素材里确实没有能落格的话。照实说，不拿观测冒充档案。
    let body = if rows.is_empty() {
        r#"<div class="md-callout-empty">这次十格都没写出东西：模型没接上，或者下发的记录里没有能落进这十格的话。下面三节照旧，它们全部由记录数出。</div>"#
            .to_string()
    } else {
        format!(r#"<div class="facets">{rows}</div>"#)
    };

    format!(
        r#"<div class="sec">{head}<div class="legend">{legend}</div>{body}</div>"#,
        head = sec_head("人物档案", "DOSSIER"),
    )
}

/// 戏说：标签墙与小传。这一节是拿来玩的，所以顶上先把口径写清楚——
/// 允许夸张、也允许说偏；笑点落在他真做过的事上，不是胡说八道。
///
/// 版面与档案分开：标签是一枚枚短词，小传是一段衬线；档案那边是带把握徽章的行，
/// 两节摆在一起，读者一眼分得清哪一半能拿去引用、哪一半只能拿去笑。
fn fun(persona: &Persona) -> String {
    let labels: String = persona
        .labels
        .iter()
        .map(|item| {
            let why = if item.why.trim().is_empty() {
                String::new()
            } else {
                format!(r#"<span class="fun-why">{}</span>"#, esc(&item.why))
            };
            format!(
                r#"<div class="fun-label"><span class="fun-word">{}</span>{why}</div>"#,
                esc(&item.label)
            )
        })
        .collect();
    let sketch = if persona.sketch.trim().is_empty() {
        String::new()
    } else {
        format!(r#"<div class="fun-sketch">{}</div>"#, esc(&persona.sketch))
    };
    if labels.is_empty() && sketch.is_empty() {
        return String::new();
    }
    format!(
        r#"<div class="sec">{head}<div class="fp-frame-note">这一节是拿来玩的：允许夸张，也允许说偏。笑点在他真做过的事上。</div><div class="fun-labels">{labels}</div>{sketch}</div>"#,
        head = sec_head("戏说", "JUST FOR FUN"),
    )
}

/// 口头禅：原样说过三遍以上的那几句，逐字核过，一个字没改。
fn catchphrases(persona: &Persona) -> String {
    if persona.catchphrases.is_empty() {
        return String::new();
    }
    let items: String = persona
        .catchphrases
        .iter()
        .map(|phrase| {
            format!(
                r#"<li class="phrase"><span class="phrase-mark"></span><blockquote>{}</blockquote></li>"#,
                esc(phrase)
            )
        })
        .collect();
    format!(
        r#"<div class="sec">{head}<div class="fp-frame-note">这几句是他自己反复说的，逐字照抄，一个字没改。</div><ul class="phrases">{items}</ul></div>"#,
        head = sec_head("口头禅", "CATCHPHRASES"),
    )
}

/// 怎么说话：一排事实筹码 + 模型对它们的一段读法。筹码是观测，读法是读法。
fn voice(material: &Material, persona: &Persona) -> String {
    let s = &material.style;
    let chips = [
        ("提问", super::persona::percent(s.question_rate)),
        ("感叹", super::persona::percent(s.exclaim_rate)),
        ("笑声", super::persona::percent(s.laugh_rate)),
        ("语气词", super::persona::percent(s.modal_rate)),
        ("省略", super::persona::percent(s.ellipsis_rate)),
        ("均长", format!("{:.1} 字", material.avg_len())),
        ("长度起伏", format!("{:.2}", s.len_cv)),
        ("长消息", super::persona::percent(s.long_rate)),
        ("短消息", super::persona::percent(s.short_rate)),
        ("爆发指数", format!("{:.2}", s.burstiness)),
    ];
    let grid: String = chips
        .into_iter()
        .map(|(key, value)| {
            format!(
                r#"<span class="fp"><i>{}</i><b>{}</b></span>"#,
                esc(key),
                esc(&value)
            )
        })
        .collect();
    let reading = if persona.style.trim().is_empty() {
        r#"<div class="style-reading faint">模型未接，怎么说话暂只有上面这些数得出来的筹码。</div>"#
            .to_string()
    } else {
        format!(
            r#"<div class="style-reading">{}</div>"#,
            esc(&persona.style)
        )
    };
    format!(
        r#"<div class="sec">{head}<div class="fp-grid">{grid}</div>{reading}</div>"#,
        head = sec_head("怎么说话", "VOICE"),
    )
}

/// 什么时候来：24 小时的柱，加一周的柱。全篇唯一画得出的形状，就画在这儿。
///
/// 两根柱子都不用颜色区分信息：峰值那根同时由主色与下面那行字点出来，
/// 0—6 时那一段同样既换了底色、也在说明里写了数。
fn rhythm(material: &Material) -> String {
    let hours: String = bars(&material.hour, material.peak_hour(), 0..6);
    let weekdays: String = bars(&material.weekday, material.peak_weekday(), 0..0);
    let ticks: String = (0..24)
        .map(|hour| {
            let label = matches!(hour, 0 | 6 | 12 | 18).then(|| format!("{hour}"));
            format!(r#"<span class="tick">{}</span>"#, label.unwrap_or_default())
        })
        .collect();
    let day_names: String = ["日", "一", "二", "三", "四", "五", "六"]
        .iter()
        .map(|name| format!(r#"<span class="tick">{name}</span>"#))
        .collect();

    format!(
        r#"<div class="sec">{head}<div class="rhythm"><div class="bars">{hours}</div><div class="ticks">{ticks}</div></div>{caption}<div class="rhythm rhythm-week"><div class="bars">{weekdays}</div><div class="ticks">{day_names}</div></div></div>"#,
        head = sec_head("什么时候来", "RHYTHM"),
        caption = rhythm_caption(material),
    )
}

/// 一根柱代表一个格子。高度按峰值归一；峰值那根上主色，0—6 时那一段的槽换一档底色。
fn bars(values: &[u64], peak: usize, night: std::ops::Range<usize>) -> String {
    let max = values.iter().copied().max().unwrap_or(0);
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let height = if max == 0 || *value == 0 {
                0
            } else {
                (value * 100 / max).max(MIN_BAR)
            };
            let mut class = String::from("bar");
            if index == peak && *value > 0 {
                class.push_str(" bar-peak");
            }
            if night.contains(&index) {
                class.push_str(" bar-night");
            }
            format!(
                r#"<span class="{class}"><span class="bar-fill" style="height:{height}%"></span></span>"#
            )
        })
        .collect()
}

/// 柱下面那行说明。峰值、夜里的占比、周末的占比与最活跃的一天都在这儿落成字。
fn rhythm_caption(material: &Material) -> String {
    let night = material.night_ratio();
    let weekend = material.weekend_ratio();
    format!(
        r#"<div class="rhythm-note">{}前后最密<span class="md-sep">·</span>夜间（0—6 点）{}<span class="md-sep">·</span>周末 {}<span class="md-sep">·</span>最活跃的一天是{}</div>"#,
        esc(&super::persona::hour_label(material.peak_hour())),
        esc(&super::persona::percent(night)),
        esc(&super::persona::percent(weekend)),
        esc(super::persona::weekday_label(material.peak_weekday())),
    )
}

/// 群内往来：跟谁聊得来。数字是数出来的，一句话是读出来的。
///
/// 一组往来分四行：谁、点名（方向条就在这一行下面）、接话、一句读法。
/// 方向条只画点名——接话是两个人一起把话接下去的，两边本来就接近对半，
/// 画出来只会让人以为「差不多」是读出来的结论。
fn ties(material: &Material, persona: &Persona, faces: &HashMap<i64, String>) -> String {
    if material.ties.is_empty() {
        return String::new();
    }
    let blocks: String = material
        .ties
        .iter()
        .map(|tie| {
            let reading = persona
                .ties
                .iter()
                .find(|reading| reading.id == tie.user_id)
                .map(|reading| format!(r#"<div class="tie-line">{}</div>"#, esc(&reading.line)))
                .unwrap_or_default();
            // 方向条：左边是他叫对方的次数，右边是对方叫他的次数，中缝是平手。
            let lean = tie.at_lean();
            let left = (lean * 100.0).round() as u64;
            let right = if tie.at_out + tie.at_in == 0 { 0 } else { 100 - left };
            let track = if tie.at_out + tie.at_in == 0 {
                String::new()
            } else {
                format!(
                    r#"<div class="tie-track"><span class="tie-out" style="width:{left}%"></span><span class="tie-in" style="width:{right}%"></span><span class="tie-center"></span></div>"#
                )
            };
            // 头像：取回来就嵌图，没取到就用名字首字顶着，版位不变。
            let face = match faces.get(&tie.user_id) {
                Some(src) => format!(r#"<img src="{}" alt="">"#, esc(src)),
                None => esc(&initial(&tie.name)),
            };
            format!(
                r#"<div class="tie"><div class="tie-head"><span class="tie-face">{face}</span><span class="tie-name">{name}</span><span class="tie-initiative">{initiative}</span></div><div class="tie-num"><i>点名</i> 我叫他 {at_out} · 他叫我 {at_in}</div>{track}<div class="tie-num"><i>接话</i> 我接他 {turn_out} · 他接我 {turn_in}</div>{reading}</div>"#,
                face = face,
                name = esc(&tie.name),
                initiative = esc(tie.initiative()),
                at_out = tie.at_out,
                at_in = tie.at_in,
                turn_out = tie.turn_out,
                turn_in = tie.turn_in,
                reading = reading,
            )
        })
        .collect();
    format!(
        r#"<div class="sec">{head}<div class="fp-frame-note">数字由记录数出：点名是 @，条上画的就是谁叫谁多；接话是紧跟在对方之后的下一条。两者都不等于回复。一句话是读出来的。</div><div class="ties">{blocks}</div></div>"#,
        head = sec_head("群内往来", "GROUP TIES"),
    )
}

/// 综述。段落与引语同列一队，按序渲染——引语落在论证里，不贴到文末。
fn profile(persona: &Persona) -> String {
    let blocks: String = persona
        .live_passages()
        .map(|passage| {
            if passage.is_quote() {
                let note = if passage.note.trim().is_empty() {
                    String::new()
                } else {
                    format!(
                        r#"<figcaption class="quote-note">{}</figcaption>"#,
                        esc(&passage.note)
                    )
                };
                format!(
                    r#"<figure class="quote"><blockquote class="quote-text">{}</blockquote>{note}</figure>"#,
                    esc(&passage.text)
                )
            } else {
                format!(r#"<p class="prose">{}</p>"#, esc(&passage.body))
            }
        })
        .collect();
    let closing = persona.closing.trim();
    // 综述与判词都空时整节不印。有一个就印一个——判词是这份东西的落点，
    // 不该因为别的段落都空了就跟着消失。
    if blocks.is_empty() && closing.is_empty() {
        return String::new();
    }
    let closing = if closing.is_empty() {
        String::new()
    } else {
        format!(
            r#"<div class="closing"><hr class="rule md-rule"><span class="label">判词</span><p class="closing-line">{}</p></div>"#,
            esc(closing)
        )
    };
    format!(
        r#"<div class="sec">{head}{blocks}{closing}</div>"#,
        head = sec_head("画像综述", "THE PROFILE"),
    )
}

fn foot(view: &View<'_>) -> String {
    let material = view.material;
    let range = format!(
        "{} — {}",
        date_of(material.first_time, view.offset),
        date_of(material.last_time, view.offset)
    );
    let covered = view.persona.covered();
    // 一张卡读完要能回答四件事：这是什么、看的是谁、数字从哪来、接下来做什么。
    // 前三件在上面，这一件落在页脚；指令带着当前环境的前缀，能整条抄走。
    let hint = if view.command.trim().is_empty() {
        String::new()
    } else {
        format!(
            r#"<div class="md-hint"><span>换一个人看</span><code>{}</code></div>"#,
            esc(view.command)
        )
    };
    format!(
        r#"<div class="foot md-foot"><div>观测区间 {range}<span class="md-sep">·</span>样本 {} 条<span class="md-sep">·</span>档案 {covered}/{total} 格<span class="md-sep">·</span>模型 {model}</div>{hint}<div class="foot-note">画像是对行为的抽象，有损：只含他在群里说过的部分，不等于本人。档案每一格都标了把握，标「明说」的依据是他本人的原话；口头禅那几句是原样照抄的；戏说与判词是读法，不是事实。仅供参考，不作凭据。</div></div>"#,
        material.samples.len(),
        total = FACETS.len(),
        range = esc(&range),
        model = esc(view.model),
    )
}

/// 出图。`scale` 是设备像素比，限制在 1—4 倍，与其它卡片一致。
pub async fn capture(html: &str, scale: f64) -> Result<String> {
    crate::render::web::shoot(
        crate::render::web::Shot::new(html, WIDTH)
            .scale(scale)
            .jpeg(90)
            .max_height(CAPTURE_MAX_HEIGHT),
    )
    .await
}

/// 页面主色取当前的北京时刻，与 `html()` 里的主题判定保持同一条规则。
pub fn now(offset: FixedOffset) -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&offset)
}

const CSS: &str = r#"
/* 画像卡的版式。
   令牌与组件基元在 `res/cards/m3e.css`（`crate::render::web::DESIGN_SYSTEM`），
   这里只写这张卡自己的位置，**不写色值与字号字面量**。

   与另外几张卡一样只用纸色；显示级文字用衬线（一句话定位、档案的判定与依据、语言读法、
   往来读法与综述）。这是版面选择不是设计系统的分歧——衬线落在纸色上才像一份「写下来的
   东西」。衬线在 46px 上要把字重收到 700：Black(800) 的字脚在纸上会糊成一团。 */
body{width:720px}
/* `.shot`（相纸的底色与内边距）由 m3e.css 的组件基元给，这里不再写一遍 */
.card{padding:var(--md-space-9) 44px var(--md-space-8)}
/* 顶沿一条主色细线：这张卡与手册、资讯两张同尺寸的卡一眼分开 */
.card::after{content:"";position:absolute;top:0;left:0;right:0;height:3px;
  background:linear-gradient(90deg,transparent,var(--md-sys-color-primary-line) 22%,
    var(--md-sys-color-primary-line) 78%,transparent)}

/* —— 主体：头像 + 名字 + 读数 —— */
.hero{display:flex;align-items:center;gap:var(--md-space-5)}
.avatar{flex:none;display:flex;align-items:center;justify-content:center;width:84px;height:84px;
  border-radius:var(--md-shape-full);font-size:var(--md-type-headline-medium-size);
  font-weight:800;letter-spacing:0;overflow:hidden;
  color:var(--md-sys-color-primary);
  background:var(--md-sys-color-primary-container);
  border:2px solid var(--md-sys-color-primary-line)}
.avatar img{display:block;width:100%;height:100%;object-fit:cover}
.who{min-width:0;flex:1}
.who-name{font-family:var(--md-font-display);font-size:var(--md-type-headline-medium-size);
  line-height:var(--md-type-headline-medium-line);
  font-weight:var(--md-type-headline-medium-weight);letter-spacing:var(--md-type-headline-medium-track);
  color:var(--md-sys-color-on-surface)}
.who-meta{margin-top:6px;font-size:var(--md-type-label-medium-size);line-height:1.6;
  font-weight:500;color:var(--md-sys-color-on-surface-variant)}
.readings-top{margin-top:10px;padding-top:0}

/* —— 一句话定位 —— */
.headline{margin-top:var(--md-space-6)}
.label-row{display:flex;align-items:center;gap:var(--md-space-3);flex-wrap:wrap}
.label{font-size:var(--md-type-label-small-size);font-weight:var(--md-type-label-small-weight);
  letter-spacing:.22em;color:var(--md-sys-color-on-surface-faint)}
.title{margin-top:var(--md-space-3);font-family:var(--md-font-display);
  font-size:var(--md-type-display-medium-size);line-height:var(--md-type-display-medium-line);
  font-weight:700;letter-spacing:-.005em;color:var(--md-sys-color-on-surface)}
/* 一句话概括用主色衬线，是这份画像里唯一「说出来的话」。
   留到六十个字、不给省略号：这一行就是这份东西的题眼，截断了看的人什么也拿不到。
   两行到头，行距按段落的量级给。 */
.note{margin-top:var(--md-space-4);font-family:var(--md-font-display);
  font-size:var(--md-type-title-small-size);line-height:1.8;font-weight:600;
  color:var(--md-sys-color-primary)}

/* —— 分节 —— */
.sec{margin-top:var(--md-space-8);padding-top:var(--md-space-7);
  border-top:1px solid var(--md-sys-color-outline-variant)}
.sec-head{display:flex;align-items:center;gap:var(--md-space-4);margin-bottom:var(--md-space-5)}
.sec-mark{font-family:var(--md-font-display);font-size:var(--md-type-title-medium-size);
  line-height:var(--md-type-title-medium-line);font-weight:700;letter-spacing:.14em;
  color:var(--md-sys-color-on-surface);white-space:nowrap}
.sec-en{font-size:var(--md-type-label-small-size);font-weight:var(--md-type-label-small-weight);
  letter-spacing:.3em;color:var(--md-sys-color-on-surface-faint);white-space:nowrap}
.sec-head::after{content:"";flex:1;height:1px;background:var(--md-sys-color-outline-variant)}
.fp-frame-note{margin-bottom:var(--md-space-5);font-size:var(--md-type-body-small-size);
  line-height:1.7;color:var(--md-sys-color-on-surface-variant)}

/* ==================== 人物档案 ==================== */
/* 三档把握的图例与徽章同一套形态：明说实心主色（他本人讲过）、可推淡主底、
   待考只描一道虚线。从硬到软一条线，而徽章里始终有字——颜色不是唯一通道。
   图例三行纵排而不是横铺：三句话横着放会挤成一团，纵排还能顺着从硬到软读下来。 */
.legend{display:flex;flex-direction:column;gap:7px;margin-bottom:var(--md-space-6)}
.legend-item{display:flex;align-items:baseline;gap:var(--md-space-3)}
.legend-item b,.cert{display:inline-block;padding:2px 9px;border-radius:var(--md-shape-s);
  font-size:var(--md-type-label-medium-size);font-weight:800;letter-spacing:.06em}
.legend-item b{flex:none;width:56px;text-align:center}
.legend-item i{flex:none;width:74px;font-style:normal;
  font-size:var(--md-type-label-small-size);
  font-weight:var(--md-type-label-small-weight);letter-spacing:.2em;
  color:var(--md-sys-color-on-surface-faint)}
.legend-item span{font-size:var(--md-type-body-small-size);line-height:1.6;
  color:var(--md-sys-color-on-surface-variant)}
.stated{color:var(--md-sys-color-on-primary);background:var(--md-sys-color-primary)}
.inferred{color:var(--md-sys-color-on-primary-container);
  background:var(--md-sys-color-primary-container)}
.open{color:var(--md-sys-color-on-surface-variant);background:transparent;
  box-shadow:inset 0 0 0 1px var(--md-sys-color-outline)}

/* 十格：左边一列维度名，中间判定与依据，右边一列把握。
   两列标签把中间夹住，读者的眼睛可以只扫左边找维度，或只扫右边看把握。
   分隔靠间距不靠线——每一行都以一个粗体的维度名起头，关系本来就清楚。 */
.facets{display:flex;flex-direction:column;gap:var(--md-space-6)}
.facet{display:grid;grid-template-columns:76px 1fr auto;gap:0 var(--md-space-4);
  align-items:start}
.facet-name{font-family:var(--md-font-display);font-size:var(--md-type-title-small-size);
  line-height:var(--md-type-title-small-line);font-weight:700;letter-spacing:.14em;
  color:var(--md-sys-color-on-surface)}
.facet-body{min-width:0}
.facet-verdict{font-family:var(--md-font-display);font-size:var(--md-type-title-small-size);
  line-height:1.62;font-weight:600;color:var(--md-sys-color-on-surface)}
.facet-ev{margin-top:5px;font-size:var(--md-type-body-small-size);line-height:1.75;
  color:var(--md-sys-color-on-surface-variant)}
/* 标了「明说」的那几格，依据就是他本人的原话：加一道主色竖线，与综述里的引语同一种处理 */
.facet-quote{margin:7px 0 0;padding-left:var(--md-space-4);
  border-left:2px solid var(--md-sys-color-primary-line);
  font-family:var(--md-font-display);font-size:var(--md-type-body-medium-size);
  line-height:1.8;color:var(--md-sys-color-primary);text-indent:0}
.cert{flex:none;align-self:start;margin-top:2px}

/* ==================== 怎么说话 ==================== */
/* 一排事实筹码：每一枚都是数出来的，不是读出来的。 */
.fp-grid{display:grid;grid-template-columns:repeat(5,minmax(0,1fr));gap:var(--md-space-3)}
.fp{display:flex;flex-direction:column;gap:5px;padding:var(--md-space-3) var(--md-space-2);
  border-radius:var(--md-shape-m);background:var(--md-sys-color-surface-container-low);
  border:1px solid var(--md-sys-color-outline-variant);align-items:center}
.fp i{font-style:normal;font-size:var(--md-type-label-small-size);
  font-weight:var(--md-type-label-small-weight);letter-spacing:.1em;
  color:var(--md-sys-color-on-surface-faint)}
.fp b{font-size:var(--md-type-title-small-size);font-weight:700;
  color:var(--md-sys-color-primary);font-variant-numeric:tabular-nums}
/* 模型对筹码的一段读法，衬线；模型没接就是一句灰底交代 */
.style-reading{margin-top:var(--md-space-5);font-family:var(--md-font-display);
  font-size:var(--md-type-body-large-size);line-height:1.86;
  color:var(--md-sys-color-on-surface-variant)}
.style-reading.faint{font-family:var(--md-font-plain);font-size:var(--md-type-body-medium-size);
  color:var(--md-sys-color-on-surface-faint)}

/* ==================== 什么时候来 ==================== */
/* 24 个小时的柱，加一周七天的柱。有数的那几格从底部长上来，峰值的柱单独上主色，
   0—6 时那一段的槽换一档底色——三样都另有文字交代，颜色不独自承担信息。
   非峰值的柱面走 outline 而不是主色系：它是「同层的普通数据」，主色只留给峰值那一个；
   outline 本来就是按图形元素（3∶1）而不是按文字调出来的，做柱面正合适。 */
.rhythm{margin-bottom:var(--md-space-5)}
.rhythm-week{margin-bottom:0}
.bars{display:flex;align-items:flex-end;gap:4px;height:64px}
.bar{position:relative;flex:1;height:100%;border-radius:var(--md-shape-xs);
  background:var(--md-sys-color-surface-container-low);
  border:1px solid var(--md-sys-color-outline-variant);overflow:hidden}
.bar-night{background:var(--md-sys-color-surface-container-high)}
.bar-fill{position:absolute;left:0;right:0;bottom:0;border-radius:var(--md-shape-xs);
  background:var(--md-sys-color-outline)}
.bar-peak .bar-fill{background:var(--md-sys-color-primary)}
.ticks{display:flex;gap:4px;margin-top:7px}
.tick{flex:1;text-align:center;font-size:var(--md-type-label-small-size);
  line-height:1.5;font-weight:600;color:var(--md-sys-color-on-surface-faint);
  font-variant-numeric:tabular-nums}
.rhythm-note{font-size:var(--md-type-body-small-size);line-height:1.7;
  color:var(--md-sys-color-on-surface-variant)}

/* ==================== 群内往来 ==================== */
/* 每一份往来一块：名字、点名、方向条、接话、一句读法。
   条只画点名，从中间往两边长：左边是他叫对方，右边是对方叫他，谁长谁更常主动开口。
   条的底色用 outline 而不是主色系：这是「同层的普通数据」，主色留给峰值与真要紧的地方；
   outline 本来就是按图形元素（3∶1）而不是按文字调出来的，做柱面正合适。 */
.ties{display:flex;flex-direction:column;gap:var(--md-space-4)}
.tie{padding:var(--md-space-5);border-radius:var(--md-shape-l);
  background:var(--md-sys-color-surface-container-low);
  border:1px solid var(--md-sys-color-outline-variant)}
.tie-head{display:flex;align-items:center;gap:var(--md-space-3)}
/* 圆框要真的把图关进去：头像原图是 140 像素，只给 border-radius 不剪裁，图会照着
   自身尺寸撑出来，压掉名字和后面的版面。`overflow:hidden` 是这条的全部。
   没取到头像时这一格装的是名字首字，同一套居中规则管两种内容。
   外圈一道细边，与顶上的大头像同一角色；那边是主色，因为那是画像对象本身。 */
.tie-face{flex:none;display:flex;align-items:center;justify-content:center;width:34px;height:34px;
  border-radius:var(--md-shape-full);overflow:hidden;
  font-size:var(--md-type-label-large-size);font-weight:800;
  color:var(--md-sys-color-primary);background:var(--md-sys-color-primary-container);
  box-shadow:0 0 0 1px var(--md-sys-color-outline-variant)}
.tie-face img{display:block;width:100%;height:100%;object-fit:cover}
.tie-name{font-family:var(--md-font-display);font-size:var(--md-type-title-small-size);
  line-height:var(--md-type-title-small-line);font-weight:700;
  color:var(--md-sys-color-on-surface)}
.tie-initiative{margin-left:auto;font-size:var(--md-type-label-medium-size);
  font-weight:700;letter-spacing:.04em;color:var(--md-sys-color-primary);white-space:nowrap}
.tie-num{margin-top:var(--md-space-3);font-size:var(--md-type-body-small-size);line-height:1.7;
  color:var(--md-sys-color-on-surface-variant);font-variant-numeric:tabular-nums}
.tie-num i{font-style:normal;font-weight:700;color:var(--md-sys-color-on-surface-faint);
  letter-spacing:.1em;margin-right:5px}
.tie-track{position:relative;display:flex;height:10px;margin-top:var(--md-space-2);
  border-radius:var(--md-shape-full);background:var(--md-sys-color-surface-container-high);
  overflow:hidden}
.tie-out{background:var(--md-sys-color-primary)}
.tie-in{background:var(--md-sys-color-outline)}
.tie-center{position:absolute;left:50%;top:0;bottom:0;width:1px;
  background:var(--md-sys-color-surface)}
.tie-line{margin-top:var(--md-space-4);font-family:var(--md-font-display);
  font-size:var(--md-type-body-medium-size);line-height:1.78;
  color:var(--md-sys-color-on-surface-variant)}

/* ==================== 戏说 ==================== */
/* 标签墙：一枚短词 + 一句为什么，一行一条、理由左对齐成一列，扫起来像一张牌面。
   短词用主色芯片，理由用次级前景色——这一节是玩笑，但玩笑也有出处，出处就得看得清。 */
.fun-labels{display:flex;flex-direction:column;gap:var(--md-space-3)}
.fun-label{display:flex;align-items:baseline;gap:var(--md-space-4)}
.fun-word{flex:none;padding:3px var(--md-space-3);border-radius:var(--md-shape-s);
  background:var(--md-sys-color-primary-container);color:var(--md-sys-color-on-primary-container);
  font-family:var(--md-font-display);font-size:var(--md-type-title-small-size);
  line-height:1.5;font-weight:700;letter-spacing:.04em;white-space:nowrap}
.fun-why{flex:1;min-width:0;font-size:var(--md-type-body-small-size);line-height:1.7;
  color:var(--md-sys-color-on-surface-variant)}
/* 戏说小传：一段衬线，落在主色淡层上——版面上它与「档案」那些带徽章的行一眼分开 */
.fun-sketch{margin-top:var(--md-space-5);padding:var(--md-space-5) 22px;
  border-radius:var(--md-shape-m);background:var(--md-sys-color-primary-tint);
  border-left:3px solid var(--md-sys-color-primary-line);
  font-family:var(--md-font-display);font-size:var(--md-type-body-large-size);
  line-height:1.86;color:var(--md-sys-color-on-surface-variant)}

/* ==================== 口头禅 ==================== */
/* 逐字照抄的那几句。排成一列短引语，左边一枚主色点，右边衬线。 */
.phrases{list-style:none;display:flex;flex-direction:column;gap:var(--md-space-2)}
.phrase{position:relative;display:flex;align-items:baseline;gap:var(--md-space-4);
  padding:var(--md-space-2) 0}
.phrase-mark{flex:none;width:6px;height:6px;border-radius:var(--md-shape-full);
  background:var(--md-sys-color-primary);align-self:center}
.phrase blockquote{margin:0;font-family:var(--md-font-display);
  font-size:var(--md-type-title-small-size);line-height:1.62;font-weight:600;
  color:var(--md-sys-color-on-surface)}
.phrase blockquote::before{content:"「"}
.phrase blockquote::after{content:"」"}

/* ==================== 画像综述 ==================== */
.prose{margin-bottom:18px;font-family:var(--md-font-display);
  font-size:var(--md-type-body-large-size);line-height:1.86;
  color:var(--md-sys-color-on-surface-variant);text-indent:2em}
.prose:last-child{margin-bottom:0}
.quote{margin:22px 0;padding:var(--md-space-5) 22px;border-radius:var(--md-shape-m);
  background:var(--md-sys-color-primary-tint);
  border-left:3px solid var(--md-sys-color-primary-line)}
.quote-text{margin:0;font-family:var(--md-font-display);
  font-size:var(--md-type-body-large-size);line-height:1.86;
  color:var(--md-sys-color-on-surface);text-indent:0}
.quote-note{margin-top:11px;font-size:var(--md-type-label-medium-size);line-height:1.62;
  color:var(--md-sys-color-on-surface-faint)}

/* —— 判词 —— */
/* 整份画像的落点，收在综述末尾。一条主色渐变的细线把它与前文隔开，字号上到
   headline-small、颜色用正文最重的 on-surface：读者扫到最后一眼，先看见的是它。
   这里刻意不借用引语那条左侧竖线——引语是「他说过什么」，判词是「这是什么」，
   两种东西长得不一样，读者才不会把判词当成又一句原话。 */
.closing{margin-top:var(--md-space-7)}
.closing .md-rule{margin:0 0 var(--md-space-5)}
.closing-line{margin:var(--md-space-4) 0 0;font-family:var(--md-font-display);
  font-size:var(--md-type-headline-small-size);line-height:var(--md-type-headline-small-line);
  font-weight:600;letter-spacing:-.005em;color:var(--md-sys-color-on-surface);
  text-indent:0;text-wrap:pretty}

/* —— 页脚 —— */
.foot-note{color:var(--md-sys-color-on-surface-variant)}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::portrait::collect::{GroupSlice, Kinds, Style, Tie};
    use crate::plugins::portrait::persona::{Facet, FunLabel, Passage, TieReading};

    fn offset() -> FixedOffset {
        FixedOffset::east_opt(8 * 3600).unwrap()
    }

    fn style() -> Style {
        Style {
            readable: 100,
            question_rate: 0.2,
            exclaim_rate: 0.1,
            ellipsis_rate: 0.05,
            comma_per_msg: 1.5,
            laugh_rate: 0.3,
            modal_rate: 0.25,
            self_per100: 2.0,
            you_per100: 3.0,
            len_cv: 0.7,
            long_rate: 0.15,
            short_rate: 0.4,
            burstiness: 0.62,
            repeat_rate: 0.08,
        }
    }

    fn tie() -> Tie {
        Tie {
            user_id: 10002,
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
            user_id: 10001,
            name: "阿<甲>".into(),
            total: 1234,
            first_time: 1_700_000_000,
            last_time: 1_700_000_000 + 86_400 * 121,
            active_days: 96,
            hour: {
                let mut hour = [0u64; 24];
                hour[23] = 220;
                hour[9] = 120;
                hour[2] = 20;
                hour
            },
            weekday: {
                let mut weekday = [0u64; 7];
                weekday[4] = 300;
                weekday[0] = 120;
                weekday
            },
            groups: vec![
                GroupSlice {
                    group_id: 1,
                    name: "测试<群>".into(),
                    count: 900,
                },
                GroupSlice {
                    group_id: 2,
                    name: "另一个群".into(),
                    count: 300,
                },
            ],
            kinds: Kinds {
                text: 1000,
                image: 100,
                anim_emoji: 80,
                face: 40,
                voice: 4,
                video: 0,
                reply: 200,
                at: 60,
            },
            longest: 320,
            avg_len: 17.6,
            words: vec![("天气".into(), 40)],
            phrases: vec![("这就去".into(), 6), ("不折腾了".into(), 4)],
            samples: vec!["凌晨三点还在改代码，明天又要废了".into()],
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

    fn persona() -> Persona {
        Persona {
            title: "用忙碌挡空的人".into(),
            note: "他把休息也算成一件事".into(),
            style: "话短，句尾常带问号，像自言自语又像追问。".into(),
            catchphrases: vec!["这就去".into(), "不折腾了".into()],
            labels: vec![
                FunLabel {
                    label: "赛博流浪汉".into(),
                    why: "天天半夜上线，白天见不着".into(),
                },
                FunLabel {
                    label: "自助餐学霸".into(),
                    why: "食堂三层都吃遍了".into(),
                },
            ],
            sketch: "他的一天从下午四点开始，到凌晨三点结束。".into(),
            facets: vec![
                facet(
                    "性格",
                    "可推",
                    "说事先给结论",
                    "三条长发言都是先下判断再补理由",
                ),
                facet(
                    "好恶",
                    "可推",
                    "不爱听人劝",
                    "别人劝他早点睡，他回了一句「睡什么睡」",
                ),
                Facet {
                    dimension: "生计".into(),
                    certainty: "明说".into(),
                    verdict: "在上班，第二天要早起".into(),
                    evidence: "凌晨三点还在改代码，明天又要废了".into(),
                    quoted: true,
                },
            ],
            ties: vec![TieReading {
                id: 10002,
                line: "跟老张主要聊装机，抬杠居多".into(),
            }],
            profile: vec![
                Passage {
                    kind: "text".into(),
                    body: "他把每件事都当成一件要交的活。".into(),
                    ..Default::default()
                },
                Passage {
                    kind: "quote".into(),
                    text: "凌晨三点还在改代码，明天又要废了".into(),
                    note: "他自己知道在拿什么换".into(),
                    ..Default::default()
                },
            ],
            closing: "他把休息也算成一件事，于是从来没有真正休息过。".into(),
            accent: "indigo".into(),
            estimated: false,
        }
    }

    /// 没有往来头像时的空表：一张卡里少几个头像不该影响版位。
    fn no_faces() -> &'static HashMap<i64, String> {
        static EMPTY: std::sync::OnceLock<HashMap<i64, String>> = std::sync::OnceLock::new();
        EMPTY.get_or_init(HashMap::new)
    }

    /// 样张里用的那张头像：40×40 的实色 PNG，四象限四种颜色。
    ///
    /// 尺寸是这条的关键——**必须比 34 像素的框大**。用一张比框小的图，
    /// 圆框剪不剪裁都看不出差别，布局审计就成了空跑：这个坑第一次就是这么埋进去的。
    /// 色块还能顺带看出 `object-fit:cover` 在中间裁、没把图拉变形。
    const FACE: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAACgAAAAoCAIAAAADnC86AAAAQUlEQVR42u3NIREAIBAAMNKh0a+JQxJCoNGEIAcd3rK7+ZXTWlrtO62IxWKxWCwWfxTfFWkxR5pYLBaLxWLxR/ED5/MStxS3PWsAAAAASUVORK5CYII=";

    /// 有头像的样张视图：老张有头像，旁边那个没有，两种状态同框。
    fn view_with_faces<'a>(material: &'a Material, persona: &'a Persona) -> View<'a> {
        static FACES: std::sync::OnceLock<HashMap<i64, String>> = std::sync::OnceLock::new();
        let faces = FACES.get_or_init(|| {
            let mut faces = HashMap::new();
            faces.insert(10002, FACE.to_string());
            faces
        });
        View {
            faces,
            ..view_at(material, persona, MORNING)
        }
    }

    const MORNING: i64 = 1_700_014_400;
    const MIDNIGHT: i64 = 1_700_064_800;

    fn view_at<'a>(material: &'a Material, persona: &'a Persona, timestamp: i64) -> View<'a> {
        View {
            material,
            persona,
            avatar: None,
            faces: no_faces(),
            model: "deepseek/deepseek-flash",
            theme: "auto",
            command: "/画像 @某人",
            offset: offset(),
            now: DateTime::from_timestamp(timestamp, 0)
                .unwrap()
                .with_timezone(&offset()),
        }
    }

    fn view<'a>(material: &'a Material, persona: &'a Persona) -> View<'a> {
        view_at(material, persona, MORNING)
    }

    #[test]
    fn every_section_renders_with_its_content() {
        let material = material();
        let persona = persona();
        let html = html(&view(&material, &persona));
        for needle in [
            "角色画像",
            "CHARACTER DOSSIER",
            "一句话定位",
            "用忙碌挡空的人",
            "他把休息也算成一件事",
            r#"<span class="sec-mark">人物档案</span>"#,
            r#"<span class="sec-mark">戏说</span>"#,
            r#"<span class="sec-mark">怎么说话</span>"#,
            r#"<span class="sec-mark">口头禅</span>"#,
            r#"<span class="sec-mark">什么时候来</span>"#,
            r#"<span class="sec-mark">群内往来</span>"#,
            r#"<span class="sec-mark">画像综述</span>"#,
            // 档案：九维的维度名跟着每一格出现，判定、依据与把握都在。
            "说事先给结论",
            "三条长发言都是先下判断再补理由",
            r#"<span class="cert stated">明说</span>"#,
            r#"<span class="cert inferred">可推</span>"#,
            // 明说的依据按原话处理，与综述里的引语同一种版式。
            r#"<blockquote class="facet-quote">凌晨三点还在改代码，明天又要废了</blockquote>"#,
            // 什么时候来：峰值、夜间、周末与最活跃的一天都落成字。
            "深夜 23 点前后最密",
            "夜间（0—6 点）",
            "周末",
            "最活跃的一天是周四",
            // 群内往来：两组计数、主动方与那句读法。
            "我叫他 12",
            "他叫我 4",
            "我接他 30",
            "他接我 9",
            "我这边主动",
            "跟老张主要聊装机，抬杠居多",
            "都不等于回复",
            // 戏说：标签、理由与小传，口径写在节头上。
            "赛博流浪汉",
            "天天半夜上线，白天见不着",
            "自助餐学霸",
            "他的一天从下午四点开始，到凌晨三点结束。",
            "允许夸张，也允许说偏",
            // 口头禅：逐字照抄的那几句。
            "这就去",
            "不折腾了",
            "一个字没改",
            "他把每件事都当成一件要交的活。",
            "凌晨三点还在改代码，明天又要废了",
            // 判词：综述末尾那一块，细线隔开，单独一个标签。
            r#"<hr class="rule md-rule">"#,
            r#"<span class="label">判词</span>"#,
            r#"<p class="closing-line">他把休息也算成一件事，于是从来没有真正休息过。</p>"#,
            "不等于本人",
            "1,234",
            "充分",
            "档案 3/10 格",
            "deepseek/deepseek-flash",
            // 页脚那条能立刻执行的下一步，带着当前环境的前缀。
            r#"<span>换一个人看</span><code>/画像 @某人</code>"#,
        ] {
            assert!(html.contains(needle), "缺少 {needle}");
        }
    }

    /// 档案十格固定，没有的格子不占版面；有格子时也不该多出别的维度。
    #[test]
    fn the_dossier_has_exactly_the_ten_slots() {
        let material = material();
        let persona = persona();
        let html = html(&view(&material, &persona));
        assert_eq!(html.matches(r#"class="facet""#).count(), 3);
        for needle in ["性格", "好恶", "生计"] {
            assert!(html.contains(needle), "缺少 {needle}");
        }
        // 三档把握各有一个图例，颜色之外还带着字。
        for needle in ["明说", "可推", "待考"] {
            assert!(
                html.contains(&format!(r#">{needle}</b>"#)),
                "缺少图例 {needle}"
            );
        }
        // 没写的格子不印。
        assert!(!html.contains(r#"class="facet-name">家庭"#));
    }

    /// 24 根柱与 7 根柱各就各位；峰值那根上主色，0—6 时那一段换底色。
    #[test]
    fn the_rhythm_draws_the_hours_and_the_week() {
        let html = html(&view(&material(), &persona()));
        // 每一根柱里恰好一个填充块：24 小时加一周七天 = 31。
        assert_eq!(html.matches(r#"class="bar-fill""#).count(), 31);
        assert_eq!(
            html.matches(r#"class="bar bar-peak""#).count(),
            2,
            "小时与星期各有一根峰值"
        );
        assert_eq!(
            html.matches(r#"class="bar bar-night""#).count(),
            6,
            "0—6 时那一段"
        );
        // 峰值那根按最大值算满高，别的是它的比例。
        assert!(html.contains(r#"style="height:100%""#), "峰值应当满格");
        assert!(
            html.contains(r#"style="height:54%""#),
            "9 点的 120 是 220 的 54%"
        );
        // 一根柱都没有的格子是空的，而不是一条最小高度的假柱：21 个小时加 5 天。
        assert_eq!(html.matches(r#"style="height:0%""#).count(), 26);
    }

    /// 往来条按点名的两个方向分，接话不进这条——它本来就两边接近。
    #[test]
    fn the_tie_bar_splits_by_who_does_the_summoning() {
        let page = html(&view(&material(), &persona()));
        // 我叫他 12 次、他叫我 4 次 → 75% / 25%。
        assert!(
            page.contains(r#"class="tie-out" style="width:75%""#),
            "{page}"
        );
        assert!(page.contains(r#"class="tie-in" style="width:25%""#));
        // 一次点名都没有时不画条，方向那栏照实说。
        let mut material = material();
        material.ties[0].at_out = 0;
        material.ties[0].at_in = 0;
        let bare = html(&view(&material, &persona()));
        assert!(!bare.contains(r#"class="tie-track""#));
        assert!(bare.contains("只看接话"));
    }

    /// 往来对象也要头像：取回来嵌图，没取到用名字首字，两种都不改版位。
    #[test]
    fn a_tie_carries_the_partners_face() {
        let material = material();
        let persona = persona();
        let without = html(&view(&material, &persona));
        assert!(
            without.contains(r#"<span class="tie-face">老</span>"#),
            "没取到头像时用名字首字"
        );

        let mut faces = HashMap::new();
        faces.insert(10002, "data:image/jpeg;base64,AAAA".to_string());
        let with = html(&View {
            faces: &faces,
            ..view_at(&material, &persona, MORNING)
        });
        assert!(with.contains(
            r#"<span class="tie-face"><img src="data:image/jpeg;base64,AAAA" alt=""></span>"#
        ));
        assert!(!with.contains(r#"<span class="tie-face">老</span>"#));
        // 表里有别人、没有他时，他仍然用首字。
        let mut others = HashMap::new();
        others.insert(999_999, "data:image/jpeg;base64,BBBB".to_string());
        let mixed = html(&View {
            faces: &others,
            ..view_at(&material, &persona, MORNING)
        });
        assert!(mixed.contains(r#"<span class="tie-face">老</span>"#));
        assert!(!mixed.contains("BBBB"));
    }

    /// 圆框必须真的剪裁。头像原图比框大得多，只给圆角不关溢出，图会撑破版面——
    /// 这条是那个 bug 的钉子：框上要有 overflow:hidden，图要铺满并居中裁。
    #[test]
    fn the_face_frame_clips_what_it_holds() {
        let page = html(&view(&material(), &persona()));
        let frame = page
            .split(".tie-face{")
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .expect("往来头像的圆框样式没了");
        assert!(
            frame.contains("border-radius:var(--md-shape-full)"),
            "{frame}"
        );
        assert!(
            frame.contains("overflow:hidden"),
            "圆框不剪裁，头像会溢出：{frame}"
        );

        let img = page
            .split(".tie-face img{")
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .expect("往来头像的图片样式没了");
        assert!(img.contains("width:100%"), "{img}");
        assert!(img.contains("height:100%"), "{img}");
        assert!(
            img.contains("object-fit:cover"),
            "图要居中裁，不能被拉变形：{img}"
        );
    }

    /// 戏说与口头禅没有内容时整块不出现——空着不是失败。
    #[test]
    fn empty_fun_and_catchphrases_drop_the_section() {
        let material = material();
        let persona = Persona {
            labels: Vec::new(),
            sketch: String::new(),
            catchphrases: Vec::new(),
            ..persona()
        };
        let page = html(&view(&material, &persona));
        assert!(!page.contains(r#"<span class="sec-mark">戏说</span>"#));
        assert!(!page.contains(r#"<span class="sec-mark">口头禅</span>"#));
        assert!(page.contains(r#"<span class="sec-mark">人物档案</span>"#));
    }

    /// 模型没接时：档案整块留空并说清为什么，观测三节照旧。
    #[test]
    fn a_model_down_report_keeps_the_observations() {
        let material = material();
        let persona = Persona::from_stats(&material);
        let html = html(&view(&material, &persona));
        assert!(html.contains("这次十格都没写出东西"));
        assert!(!html.contains(r#"class="cert"#));
        assert!(html.contains("怎么说话"));
        assert!(html.contains(r#"class="fp-grid""#));
        assert!(html.contains("什么时候来"));
        assert!(html.contains(r#"class="bars""#));
        assert!(html.contains("群内往来"), "往来是数出来的，模型没接也还在");
        assert!(html.contains("模型未接，怎么说话暂只有上面这些"));
        assert!(html.contains("模型未接，档案由观测直出"));
        assert!(html.contains("档案 0/10 格"));
        // 口头禅是数出来的，模型没接也照旧在；戏说要模型写，这一层空着。
        assert!(html.contains(r#"<span class="sec-mark">口头禅</span>"#));
        assert!(html.contains("这就去"));
        assert!(!html.contains(r#"<span class="sec-mark">戏说</span>"#));
    }

    /// 没有往来对象时，那一节整块不出现。
    #[test]
    fn a_material_without_ties_drops_the_section() {
        let mut material = material();
        material.ties.clear();
        let html = html(&view(&material, &persona()));
        assert!(!html.contains(r#"<span class="sec-mark">群内往来</span>"#));
        assert!(!html.contains(r#"class="tie""#));
    }

    /// 昵称来自群聊，必须转义；模型给的文本同理。
    #[test]
    fn external_text_is_escaped() {
        let material = material();
        let persona = persona();
        let html = html(&view(&material, &persona));
        assert!(!html.contains("阿<甲>"));
        assert!(html.contains("阿&lt;甲&gt;"));
        assert!(!html.contains("测试<群>"));
    }

    /// 有头像时嵌图，没有时退回名字首字，两种都不改版位。
    #[test]
    fn the_avatar_replaces_the_initial_when_available() {
        let material = material();
        let persona = persona();
        let base = view_at(&material, &persona, MORNING);

        let without = html(&base);
        assert!(
            without.contains(r#"<div class="avatar">阿</div>"#),
            "没有头像时用首字"
        );

        let data = "data:image/jpeg;base64,AAAA";
        let with = html(&View {
            avatar: Some(data),
            ..view_at(&material, &persona, MORNING)
        });
        assert!(with.contains(&format!(
            r#"<div class="avatar"><img src="{data}" alt=""></div>"#
        )));
        assert!(!with.contains(r#"<div class="avatar">阿</div>"#));
    }

    /// 空的综述不占版面；一句话定位、怎么说话、什么时候来始终在——画像的骨头是观测。
    #[test]
    fn empty_passages_disappear_and_the_bones_stay() {
        let material = material();
        let persona = Persona {
            profile: Vec::new(),
            note: String::new(),
            style: String::new(),
            facets: Vec::new(),
            ties: Vec::new(),
            closing: String::new(),
            ..persona()
        };
        let html = html(&view(&material, &persona));
        assert!(!html.contains(r#"<span class="sec-mark">画像综述</span>"#));
        assert!(html.contains("用忙碌挡空的人"));
        assert!(html.contains("怎么说话"));
        assert!(html.contains("模型未接，怎么说话暂只有上面这些"));
    }

    /// 综述的段落全空了，判词还在——这一节能只收一句判词，不该整节消失。
    /// 判词是这份东西的落点，它的去留不该由别的段落决定。
    #[test]
    fn the_closing_line_survives_without_any_passages() {
        let material = material();
        let persona = Persona {
            profile: Vec::new(),
            ..persona()
        };
        let html = html(&view(&material, &persona));
        assert!(html.contains(r#"<span class="sec-mark">画像综述</span>"#));
        assert!(html.contains(r#"<span class="label">判词</span>"#));
        assert!(html.contains("他把休息也算成一件事，于是从来没有真正休息过。"));
        // 只有判词时不该留一条孤零零的细线在段落的位置上。
        assert_eq!(html.matches(r#"<hr class="rule md-rule">"#).count(), 1);
    }

    #[test]
    fn blank_passages_are_not_printed() {
        let material = material();
        let persona = Persona {
            profile: vec![
                Passage {
                    kind: "text".into(),
                    body: "第一段。".into(),
                    ..Default::default()
                },
                Passage {
                    kind: "quote".into(),
                    text: "  ".into(),
                    ..Default::default()
                },
                Passage {
                    kind: "text".into(),
                    body: String::new(),
                    ..Default::default()
                },
            ],
            ..persona()
        };
        let html = html(&view(&material, &persona));
        assert_eq!(html.matches(r#"class="prose""#).count(), 1);
        assert!(!html.contains(r#"class="quote""#));
    }

    #[test]
    fn theme_follows_beijing_reading_hours() {
        let at = |timestamp: i64| {
            DateTime::from_timestamp(timestamp, 0)
                .unwrap()
                .with_timezone(&offset())
        };
        assert_eq!(Theme::resolve("auto", at(MORNING)), Theme::Light);
        assert_eq!(Theme::resolve("auto", at(MIDNIGHT)), Theme::Dark);
        assert_eq!(Theme::resolve("light", at(MIDNIGHT)), Theme::Light);
        assert_eq!(Theme::resolve("dark", at(MORNING)), Theme::Dark);
        assert_eq!(Theme::resolve("乱写", at(MIDNIGHT)), Theme::Dark);
    }

    #[test]
    fn a_pinned_theme_overrides_the_clock() {
        let material = material();
        let persona = persona();
        let pinned = View {
            theme: "light",
            ..view_at(&material, &persona, MIDNIGHT)
        };
        let body_class = |page: &str| {
            let head = "<body class=\"";
            let start = page.find(head).expect("页面应当有 body") + head.len();
            let end = start + page[start..].find('"').expect("class 属性应当闭合");
            page[start..end].to_string()
        };
        let light = body_class(&html(&pinned));
        assert!(light.contains("scheme-portrait md-text seed-"), "{light}");
        assert!(!light.contains("dark"), "固定日读时不该带深色类名：{light}");
        let dark = body_class(&html(&View {
            theme: "dark",
            ..view_at(&material, &persona, MIDNIGHT)
        }));
        assert!(dark.contains(" dark"), "固定夜读时应当带深色类名：{dark}");
        assert!(!body_class(&html(&view_at(&material, &persona, MORNING))).contains("dark"));
        assert!(body_class(&html(&view_at(&material, &persona, MIDNIGHT))).contains("dark"));
    }

    /// 把日读、夜读与降级三份报告写到 `PORTRAIT_CARD_DUMP` 指定的目录，肉眼校版用：
    ///   PORTRAIT_CARD_DUMP=$PREFIX/tmp/portrait cargo test portrait::card::tests::dump -- --ignored
    #[test]
    #[ignore = "仅用于人工核对排版"]
    fn dump_sample_cards() {
        let Ok(dir) = std::env::var("PORTRAIT_CARD_DUMP") else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();
        let material = material();
        let persona = persona();
        // 日读这份带上有头像的往来：布局审计要跑在真实的头像上，
        // 圆框剪裁这条只在这一份里看得见。
        std::fs::write(
            format!("{dir}/portrait-light.html"),
            html(&view_with_faces(&material, &persona)),
        )
        .unwrap();
        std::fs::write(
            format!("{dir}/portrait-dark.html"),
            html(&view_at(&material, &persona, MIDNIGHT)),
        )
        .unwrap();
        let fallback = Persona::from_stats(&material);
        std::fs::write(
            format!("{dir}/portrait-fallback.html"),
            html(&view_at(&material, &fallback, MORNING)),
        )
        .unwrap();
    }

    #[tokio::test]
    #[ignore = "需要本地 Chrome/Chromium"]
    async fn captures_complete_cards() {
        let material = material();
        let persona = persona();
        let fallback = Persona::from_stats(&material);
        let cases = [
            ("light", view_at(&material, &persona, MORNING)),
            ("dark", view_at(&material, &persona, MIDNIGHT)),
            ("fallback", view_at(&material, &fallback, MORNING)),
        ];
        for (name, view) in cases {
            let base64 = capture(&html(&view), 2.0).await.unwrap();
            use base64::Engine as _;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&base64)
                .unwrap();
            let image = image::load_from_memory(&bytes).unwrap();
            assert_eq!(image.width(), WIDTH * 2, "{name}");
            assert!(image.height() > 2000, "{name} height = {}", image.height());
            let path = std::env::temp_dir().join(format!("acumen-portrait-{name}.jpg"));
            std::fs::write(&path, &bytes).ok();
            println!("出图已写入 {}", path.display());
        }
        cdp_html_shot::Browser::shutdown_global().await;
    }
}
