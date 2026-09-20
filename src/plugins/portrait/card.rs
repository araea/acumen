//! 画像卡（HTML → 截图）。
//!
//! 版式按「一份读数」来排，不按仪表盘来排。它有三层，从硬到软，版面也照着这个次序走：
//!
//! 1. **仪器读数**：语言指纹。全篇唯一算得出来的东西，用一排事实筹码铺开。
//! 2. **读法**：四维行为标签（事实/统计/推断三层徽章）加一段语言风格白描。
//! 3. **投影**：MBTI 四轴光谱与九型核心。全篇唯一「不硬」的部分，所以单独关在一格里，
//!    顶上就写明它是投影，不是定论。
//!
//! 综合速写压在最前——它是这份东西的脸。往下是两把尺子与一排筹码，再往下才是标签与综述。
//! 版面唯一的图形是光谱那条从中间往一侧填的槽和事实筹码：光谱是投影层全部说服力所在，
//! 筹码是仪器读数可核验的那一层。
//!
//! 约束与本仓库其它卡片一致：不加载任何外部资源（字体、图片、脚本都不引；头像由
//! [`super::avatar`] 先下回来，以 data URL 内嵌），所有动态文本一律转义，出图交给
//! [`crate::render::web::shoot`]——量高、等字体、尺寸护栏与闸门都在那一处。
//! 本文件里的版式只摆位置，不写色值、字号、圆角、阴影的字面量，一律取令牌。

use super::collect::Material;
use super::models::{Mbti, axis_lean, pole_short};
use super::persona::{DIMENSIONS, LAYERS, Persona, Tag};
use anyhow::Result;
use chrono::{DateTime, FixedOffset, Timelike, Utc};

/// 卡片渲染宽度（CSS 像素）。出图宽度 = `WIDTH × scale`。
const WIDTH: u32 = 720;
/// 高度上限（CSS 像素），与其它卡片一致。
const CAPTURE_MAX_HEIGHT: f64 = 16_000.0;

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
    pub model: &'a str,
    /// 主题模式：`auto` / `light` / `dark`，见 [`Theme::resolve`]。
    pub theme: &'a str,
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
{models}
{fingerprint}
{taxonomy}
{profile}
{discuss}
{foot}
</div></div></body></html>"#,
        css = css,
        eyebrow = eyebrow(view),
        hero = hero(view),
        seed = accent.seed_class(),
        theme = theme.vars(),
        headline = headline(view.persona),
        models = models(view.persona),
        fingerprint = fingerprint(view.material, view.persona),
        taxonomy = taxonomy(view.persona),
        profile = profile(view.persona),
        discuss = discuss(view.persona),
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
        r#"<div class="md-eyebrow"><div class="md-kicker"><span class="md-dot"></span>用户画像<span class="md-kicker-en">USER PROFILE</span></div><span class="md-stamp">{}</span></div>"#,
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
/// 只留四格：「均长」不在这里——它属于语言指纹，「同一件事只说一遍」。
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

/// 综合速写——这份东西的脸。模型没接时挂一枚筹码说明。
fn headline(persona: &Persona) -> String {
    let pill = if persona.estimated {
        r#"<span class="md-chip">模型未接，标签由统计直出</span>"#.to_string()
    } else {
        String::new()
    };
    let note = if persona.note.trim().is_empty() {
        String::new()
    } else {
        format!(r#"<div class="note">{}</div>"#, esc(&persona.note))
    };
    format!(
        r#"<div class="headline"><div class="label-row"><span class="label">综合速写</span>{pill}</div><div class="title">{}</div>{note}</div>"#,
        esc(if persona.title.is_empty() {
            "尚未归纳"
        } else {
            &persona.title
        }),
    )
}

/// 投影层：两把尺子。模型没接就整块留空，并说清为什么。
fn models(persona: &Persona) -> String {
    let body = if persona.mbti.is_some() || persona.enneagram.is_some() {
        format!(
            r#"<div class="models">{}{}</div>"#,
            mbti_card(persona),
            ennea_card(persona),
        )
    } else {
        r#"<div class="md-callout-empty fp-empty">模型未接，人格投影这一层空着。投影要把行为读进框架，得等模型接上。</div>"#
            .to_string()
    };
    format!(
        r#"<div class="sec">{head}<div class="fp-frame-note">两把尺子都是把群聊行为往既有框架上做的投影，是讨论的起点，不是定论。</div>{body}</div>"#,
        head = sec_head("人格投影", "PERSONALITY PROJECTION"),
    )
}

fn mbti_card(persona: &Persona) -> String {
    let Some(mbti) = persona.mbti else {
        return String::new();
    };
    let code = mbti.code();
    let epithet = mbti.epithet();
    let epithet = if epithet.is_empty() {
        String::new()
    } else {
        format!(r#"<i>{}</i>"#, esc(epithet))
    };
    format!(
        r#"<div class="model model-mbti"><div class="model-head"><span class="model-name">MBTI 四轴</span><span class="model-code">{code}{epithet}</span></div>{axes}</div>"#,
        code = esc(&code),
        epithet = epithet,
        axes = spectrum(&mbti),
    )
}

/// 四条光谱，从中间往更强的一侧填。槽越长，偏得越远；贴中即贴近中线。
fn spectrum(mbti: &Mbti) -> String {
    mbti.axes()
        .iter()
        .map(|(left_letter, right_letter, score)| {
            let pct = axis_lean(*score); // 0..100，靠左极的百分比
            let half = (pct as i32 - 50).unsigned_abs() as u32;
            let (fill_left, fill_width) = if pct >= 50 {
                (50 - half, half)
            } else {
                (50, half)
            };
            let win = if *score >= 0 { pct } else { 100 - pct };
            let lc = left_letter.chars().next().unwrap_or('?');
            let rc = right_letter.chars().next().unwrap_or('?');
            let left_name = pole_short(lc);
            let right_name = pole_short(rc);
            // 赢的那一极把它偏到的百分比写在名字后面。
            let (left_suffix, right_suffix) = if *score >= 0 {
                (format!(" {win}"), String::new())
            } else {
                (String::new(), format!(" {win}"))
            };
            format!(
                r#"<div class="axis"><div class="axis-poles"><span class="pole">{lc} {left_name}{left_suffix}</span><span class="pole pole-right">{rc} {right_name}{right_suffix}</span></div><div class="axis-track"><span class="axis-center"></span><span class="axis-fill" style="left:{fill_left}%;width:{fill_width}%"></span></div></div>"#,
                lc = lc,
                rc = rc,
                left_name = esc(left_name),
                right_name = esc(right_name),
                left_suffix = esc(&left_suffix),
                right_suffix = esc(&right_suffix),
                fill_left = fill_left,
                fill_width = fill_width,
            )
        })
        .collect()
}

fn ennea_card(persona: &Persona) -> String {
    let Some(ennea) = persona.enneagram else {
        return String::new();
    };
    let name = ennea.name();
    let name_line = if name.is_empty() {
        String::new()
    } else {
        format!(r#"<div class="ennea-name">{}</div>"#, esc(name))
    };
    format!(
        r#"<div class="model model-ennea"><div class="model-head"><span class="model-name">九型核心</span><span class="model-code ennea-code">{short}</span></div>{name_line}<div class="ennea-motif">{motif}</div></div>"#,
        short = esc(&ennea.short()),
        name_line = name_line,
        motif = esc(ennea.motif()),
    )
}

/// 语言指纹：一排事实筹码 + 模型对它们的一段读法。筹码是仪器读数，读法是读法层。
fn fingerprint(material: &Material, persona: &Persona) -> String {
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
        r#"<div class="style-reading faint">模型未接，语言风格暂只有上面这些数得出来的筹码。</div>"#
            .to_string()
    } else {
        format!(r#"<div class="style-reading">{}</div>"#, esc(&persona.style))
    };
    format!(
        r#"<div class="sec">{head}<div class="fp-grid">{grid}</div>{reading}</div>"#,
        head = sec_head("语言指纹", "LANGUAGE FINGERPRINT"),
        grid = grid,
        reading = reading,
    )
}

/// 四维标签体系。四个维度按固定次序排，空的维度不占版面；每条先给层级徽章，再给标签与证据。
fn taxonomy(persona: &Persona) -> String {
    let dims: String = DIMENSIONS
        .iter()
        .filter_map(|(name, en)| {
            let tags = persona.tags_of(name);
            if tags.is_empty() {
                return None;
            }
            let rows: String = tags.iter().map(|tag| tag_row(tag)).collect();
            Some(format!(
                r#"<div class="dim"><div class="dim-head"><span class="dim-name">{}</span><span class="dim-en">{}</span></div>{rows}</div>"#,
                esc(name),
                esc(en),
            ))
        })
        .collect();
    if dims.is_empty() {
        return String::new();
    }

    let legend: String = LAYERS
        .iter()
        .map(|(name, en)| {
            format!(
                r#"<span class="legend-item"><b class="{}">{}</b><i>{}</i></span>"#,
                super::persona::tier_class(name),
                esc(name),
                esc(en),
            )
        })
        .collect();

    format!(
        r#"<div class="sec">{head}<div class="legend">{legend}</div><div class="dims">{dims}</div></div>"#,
        head = sec_head("行为标签", "BEHAVIOUR TAGS"),
    )
}

fn tag_row(tag: &Tag) -> String {
    let class = tag.tier_class();
    let evidence = if tag.evidence.trim().is_empty() {
        String::new()
    } else {
        format!(r#"<span class="tag-ev">{}</span>"#, esc(&tag.evidence))
    };
    format!(
        r#"<div class="tag"><span class="tag-layer {class}">{}</span><span class="tag-main"><span class="tag-label">{}</span>{evidence}</span></div>"#,
        esc(tag.tier()),
        esc(&tag.label),
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
    if blocks.is_empty() {
        return String::new();
    }
    format!(
        r#"<div class="sec">{head}{blocks}</div>"#,
        head = sec_head("画像综述", "THE PROFILE"),
    )
}

/// 群聊钩子：能拿去吵的切入点。模型没给就整块隐去。
fn discuss(persona: &Persona) -> String {
    let items: String = persona
        .discuss
        .iter()
        .map(|hook| format!(r#"<li>{}</li>"#, esc(hook)))
        .collect();
    if items.is_empty() {
        return String::new();
    }
    format!(
        r#"<div class="sec">{head}<ul class="discuss">{items}</ul></div>"#,
        head = sec_head("群聊钩子", "FOR DISCUSSION"),
    )
}

fn foot(view: &View<'_>) -> String {
    let material = view.material;
    let range = format!(
        "{} — {}",
        date_of(material.first_time, view.offset),
        date_of(material.last_time, view.offset)
    );
    format!(
        r#"<div class="foot md-foot"><div>观测区间 {range}<span class="md-sep">·</span>样本 {} 条<span class="md-sep">·</span>模型 {model}</div><div class="foot-note">画像是对行为的抽象，有损：只含他在群里说过的部分，不等于本人。MBTI 与九型是行为往框架上的投影，是讨论的起点，不是定论。仅供娱乐，不作凭据。</div></div>"#,
        material.samples.len(),
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

   与另外几张卡一样只用纸色；显示级文字用衬线（综合速写、九型名、语言读法与综述）。
   这是版面选择不是设计系统的分歧——衬线落在纸色上才像一份「写下来的东西」。
   衬线在 46px 上要把字重收到 700：Black(800) 的字脚在纸上会糊成一团。 */
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

/* —— 综合速写 —— */
.headline{margin-top:var(--md-space-6)}
.label-row{display:flex;align-items:center;gap:var(--md-space-3);flex-wrap:wrap}
.label{font-size:var(--md-type-label-small-size);font-weight:var(--md-type-label-small-weight);
  letter-spacing:.22em;color:var(--md-sys-color-on-surface-faint)}
.title{margin-top:var(--md-space-3);font-family:var(--md-font-display);
  font-size:var(--md-type-display-medium-size);line-height:var(--md-type-display-medium-line);
  font-weight:700;letter-spacing:-.005em;color:var(--md-sys-color-on-surface)}
/* 一句话概括用主色衬线：它是这份画像里唯一「说出来的话」 */
.note{margin-top:var(--md-space-3);font-family:var(--md-font-display);
  font-size:var(--md-type-title-small-size);line-height:1.66;font-weight:600;
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

/* ==================== 人格投影 ==================== */
/* 投影层顶上先声明它是投影不是定论，再摆两把尺子。 */
.fp-frame-note{margin-bottom:var(--md-space-5);font-size:var(--md-type-body-small-size);
  line-height:1.7;color:var(--md-sys-color-on-surface-variant)}
.models{display:grid;grid-template-columns:1fr 1fr;gap:var(--md-space-4)}
.model{padding:var(--md-space-5) var(--md-space-5) var(--md-space-4);
  border-radius:var(--md-shape-l);background:var(--md-sys-color-surface-container-low);
  border:1px solid var(--md-sys-color-outline-variant)}
.model-head{display:flex;align-items:baseline;justify-content:space-between;gap:var(--md-space-3);
  padding-bottom:var(--md-space-4);margin-bottom:var(--md-space-2);
  border-bottom:1px solid var(--md-sys-color-outline-variant)}
.model-name{font-size:var(--md-type-label-medium-size);font-weight:var(--md-type-label-medium-weight);
  letter-spacing:.14em;color:var(--md-sys-color-on-surface-faint)}
.model-code{font-family:var(--md-font-display);font-size:var(--md-type-title-medium-size);
  font-weight:700;letter-spacing:.06em;color:var(--md-sys-color-primary);white-space:nowrap}
.model-code i{font-style:normal;margin-left:var(--md-space-2);font-size:var(--md-type-label-medium-size);
  font-weight:600;letter-spacing:.04em;color:var(--md-sys-color-on-surface-variant)}

/* —— MBTI 四轴光谱 ——
   一条从中间往一侧填的槽：中线居中，往更强的一侧填，长度即偏幅。贴近中线
   的人画出来是一条贴中的短线，那才是诚实的。 */
.axis{padding:var(--md-space-3) 0}
.axis-poles{display:flex;justify-content:space-between;align-items:baseline;
  margin-bottom:var(--md-space-2);font-size:var(--md-type-label-medium-size);
  font-weight:600;color:var(--md-sys-color-on-surface-variant)}
.pole{font-variant-numeric:tabular-nums}
.pole-right{text-align:right}
.axis-track{position:relative;height:12px;border-radius:var(--md-shape-full);
  background:var(--md-sys-color-surface-container-high)}
.axis-center{position:absolute;left:50%;top:-4px;bottom:-4px;width:2px;margin-left:-1px;
  background:var(--md-sys-color-outline);border-radius:var(--md-shape-full)}
.axis-fill{position:absolute;top:0;bottom:0;border-radius:var(--md-shape-full);
  background:var(--md-sys-color-primary);min-width:3px}

/* —— 九型核心 —— */
.ennea-code{font-size:var(--md-type-headline-small-size);font-weight:800;letter-spacing:.02em}
.ennea-name{margin-top:var(--md-space-4);font-family:var(--md-font-display);
  font-size:var(--md-type-title-large-size);line-height:var(--md-type-title-large-line);
  font-weight:700;letter-spacing:.06em;color:var(--md-sys-color-on-surface)}
.ennea-motif{margin-top:var(--md-space-3);font-size:var(--md-type-body-small-size);
  line-height:1.8;color:var(--md-sys-color-on-surface-variant)}
.fp-empty{margin-top:0}

/* ==================== 语言指纹 ==================== */
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
/* 模型对指纹的一段读法，衬线；模型没接就是一句灰底交代 */
.style-reading{margin-top:var(--md-space-5);font-family:var(--md-font-display);
  font-size:var(--md-type-body-large-size);line-height:1.86;
  color:var(--md-sys-color-on-surface-variant)}
.style-reading.faint{font-family:var(--md-font-plain);font-size:var(--md-type-body-medium-size);
  color:var(--md-sys-color-on-surface-faint)}

/* ==================== 行为标签 ==================== */
/* 三层抽象的图例与徽章同一套形态：事实浅底、统计淡主底、推断主色实心。
   从硬到软一条线，读者一眼知道哪几条能拿去用、哪几条只是读出来的。 */
.legend{display:flex;flex-wrap:wrap;gap:var(--md-space-2) 22px;margin-bottom:18px}
.legend-item{display:inline-flex;align-items:baseline;gap:var(--md-space-2)}
.legend-item b,.tag-layer{display:inline-block;padding:2px 9px;border-radius:var(--md-shape-s);
  font-size:var(--md-type-label-medium-size);font-weight:800;letter-spacing:.06em}
.legend-item i{font-style:normal;font-size:var(--md-type-label-small-size);
  font-weight:var(--md-type-label-small-weight);letter-spacing:.2em;
  color:var(--md-sys-color-on-surface-faint)}
.observed{color:var(--md-sys-color-on-surface-variant);
  background:var(--md-sys-color-surface-container-high)}
.derived{color:var(--md-sys-color-on-primary-container);
  background:var(--md-sys-color-primary-container)}
.inferred{color:var(--md-sys-color-on-primary);background:var(--md-sys-color-primary)}

.dims{display:flex;flex-direction:column;gap:var(--md-space-4)}
.dim{padding:var(--md-space-4) 22px var(--md-space-1);border-radius:var(--md-shape-l);
  background:var(--md-sys-color-surface-container-low);
  border:1px solid var(--md-sys-color-outline-variant)}
.dim-head{display:flex;align-items:baseline;gap:11px;padding-bottom:11px;
  border-bottom:1px solid var(--md-sys-color-outline-variant)}
.dim-name{font-family:var(--md-font-display);font-size:var(--md-type-title-small-size);
  line-height:var(--md-type-title-small-line);font-weight:700;letter-spacing:.14em;
  color:var(--md-sys-color-on-surface)}
.dim-en{font-size:var(--md-type-label-small-size);font-weight:var(--md-type-label-small-weight);
  letter-spacing:.26em;color:var(--md-sys-color-on-surface-faint)}
.tag{display:flex;gap:13px;padding:var(--md-space-3) 0;
  border-bottom:1px dashed var(--md-sys-color-outline-variant)}
.tag:last-child{border-bottom:none}
.tag-layer{flex:none;align-self:flex-start;width:56px;padding:3px 0;margin-top:2px;
  text-align:center}
.tag-main{flex:1;min-width:0}
.tag-label{display:block;font-size:var(--md-type-title-small-size);
  line-height:var(--md-type-title-small-line);font-weight:700;
  color:var(--md-sys-color-on-surface)}
.tag-ev{display:block;margin-top:5px;font-size:var(--md-type-body-small-size);
  line-height:1.75;color:var(--md-sys-color-on-surface-variant)}

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

/* ==================== 群聊钩子 ==================== */
/* 能拿去吵的切入点。左侧一枚引导性质的圆点，右侧是话。 */
.discuss{list-style:none;display:flex;flex-direction:column;gap:var(--md-space-3)}
.discuss li{position:relative;padding:var(--md-space-3) var(--md-space-5) var(--md-space-3) calc(var(--md-space-5) + 16px);
  border-radius:var(--md-shape-m);background:var(--md-sys-color-secondary-container);
  color:var(--md-sys-color-on-secondary-container);
  font-family:var(--md-font-display);font-size:var(--md-type-body-medium-size);line-height:1.7}
.discuss li::before{content:"";position:absolute;left:var(--md-space-4);top:50%;
  width:6px;height:6px;margin-top:-3px;border-radius:var(--md-shape-full);
  background:var(--md-sys-color-primary)}

/* —— 页脚 —— */
.foot-note{color:var(--md-sys-color-on-surface-variant)}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::portrait::collect::{GroupSlice, Kinds};
    use crate::plugins::portrait::models::Enneagram;
    use crate::plugins::portrait::persona::Passage;

    fn offset() -> FixedOffset {
        FixedOffset::east_opt(8 * 3600).unwrap()
    }

    fn style() -> crate::plugins::portrait::collect::Style {
        crate::plugins::portrait::collect::Style {
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
                hour
            },
            weekday: {
                let mut weekday = [0u64; 7];
                weekday[4] = 300;
                weekday
            },
            groups: vec![
                GroupSlice {
                    name: "测试<群>".into(),
                    count: 900,
                },
                GroupSlice {
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
            samples: vec!["凌晨三点还在改代码，明天又要废了".into()],
            style: style(),
        }
    }

    fn tag(dimension: &str, layer: &str, label: &str, evidence: &str) -> Tag {
        Tag {
            dimension: dimension.into(),
            layer: layer.into(),
            label: label.into(),
            evidence: evidence.into(),
        }
    }

    fn mbti() -> Mbti {
        Mbti {
            energy: -70,
            perceiving: -60,
            deciding: 35,
            lifestyle: -25,
        }
    }

    fn persona() -> Persona {
        Persona {
            title: "用忙碌挡空的人".into(),
            note: "他把休息也算成一件事".into(),
            style: "话短，句尾常带问号，像自言自语又像追问。".into(),
            mbti: Some(mbti()),
            enneagram: Some(Enneagram { number: 6, wing: 5 }),
            tags: vec![
                tag("活跃", "事实", "夜里出现", "夜间发言占 41%，白天基本不在"),
                tag("活跃", "统计", "作息偏晚", "23 点前后最密"),
                tag("内容", "推断", "说事就说事", "样本里没有一句闲聊"),
            ],
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
            discuss: vec![
                "他嘴上说无所谓，其实每条都改到半夜".into(),
                "这个内省的底子，你觉得准吗".into(),
            ],
            accent: "indigo".into(),
            estimated: false,
        }
    }

    const MORNING: i64 = 1_700_014_400;
    const MIDNIGHT: i64 = 1_700_064_800;

    fn view_at<'a>(material: &'a Material, persona: &'a Persona, timestamp: i64) -> View<'a> {
        View {
            material,
            persona,
            avatar: None,
            model: "deepseek/deepseek-flash",
            theme: "auto",
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
            "用户画像",
            "USER PROFILE",
            "综合速写",
            "用忙碌挡空的人",
            "他把休息也算成一件事",
            r#"<span class="sec-mark">人格投影</span>"#,
            r#"<span class="sec-mark">语言指纹</span>"#,
            r#"<span class="sec-mark">行为标签</span>"#,
            r#"<span class="sec-mark">画像综述</span>"#,
            r#"<span class="sec-mark">群聊钩子</span>"#,
            "MBTI 四轴",
            "INTP",
            "逻辑学家",
            "九型核心",
            "6w5",
            "忠诚者",
            "怕失去依靠",
            "讨论的起点，不是定论",
            "夜里出现",
            "夜间发言占 41%",
            "他把每件事都当成一件要交的活。",
            "凌晨三点还在改代码，明天又要废了",
            "他嘴上说无所谓，其实每条都改到半夜",
            "不等于本人",
            "1,234",
            "充分",
            "deepseek/deepseek-flash",
        ] {
            assert!(html.contains(needle), "缺少 {needle}");
        }
    }

    /// 四条光谱各有一槽；内向那一侧（负分）应往右填，外向往左填。
    #[test]
    fn the_spectrum_draws_four_axes() {
        let html = html(&view(&material(), &persona()));
        assert_eq!(html.matches(r#"class="axis-poles""#).count(), 4);
        assert_eq!(html.matches(r#"class="axis-track""#).count(), 4);
        assert!(html.contains("E 外向"));
        assert!(html.contains("I 内向"));
        // 内向偏 -70 → 靠右极 85%，槽应从中间往右
        assert!(html.contains("left:50%;width:35%"), "负轴应从中间向右填 35%");
        // 思考偏 +35 → 靠左极 67%，槽应从中间往左 17%
        assert!(html.contains("left:33%;width:17%"), "正轴应从中间向左填 17%");
    }

    /// 模型没接时投影层整块留空，并说清为什么；但语言指纹的筹码仍是事实。
    #[test]
    fn a_model_down_report_drops_the_projection() {
        let material = material();
        let persona = Persona::from_stats(&material);
        let html = html(&view(&material, &persona));
        assert!(html.contains("模型未接，人格投影这一层空着"));
        assert!(!html.contains(r#"class="model-mbti""#));
        assert!(html.contains("语言指纹"));
        assert!(html.contains(r#"class="fp-grid""#));
        assert!(html.contains("语言风格暂只有上面这些"));
        assert!(html.contains("模型未接，标签由统计直出"));
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

    /// 空的维度、综述与钩子都不占版面；综合速写与语言指纹始终在——画像的骨头是数据。
    #[test]
    fn empty_sections_disappear_and_the_bones_stay() {
        let material = material();
        let persona = Persona {
            tags: Vec::new(),
            profile: Vec::new(),
            discuss: Vec::new(),
            note: String::new(),
            mbti: None,
            enneagram: None,
            style: String::new(),
            ..persona()
        };
        let html = html(&view(&material, &persona));
        assert!(!html.contains(r#"<span class="sec-mark">行为标签</span>"#));
        assert!(!html.contains(r#"<span class="sec-mark">画像综述</span>"#));
        assert!(!html.contains(r#"<span class="sec-mark">群聊钩子</span>"#));
        assert!(html.contains("用忙碌挡空的人"));
        assert!(html.contains("语言指纹"));
        assert!(html.contains("语言风格暂只有上面这些"));
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
        std::fs::write(
            format!("{dir}/portrait-light.html"),
            html(&view_at(&material, &persona, MORNING)),
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
