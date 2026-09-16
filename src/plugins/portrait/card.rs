//! 画像卡（HTML → 截图）。
//!
//! 版式按「一份用户画像」来排，不按仪表盘来排：先把这份东西是什么说清楚，再给综合标签与
//! 一句话概括，然后是三件事——读过多少（读数）、从里面抽出了什么（标签体系）、把它串起来
//! 的那一段（综述）。
//!
//! 版面上唯一的图形是标签的层级徽章：事实是浅底、统计是描边、推断是实心。它是这份报告
//! 全部的说服力所在——三层从硬到软，读者一眼就知道哪几条能拿去用，哪几条只是读出来的。
//!
//! 约束与本仓库其它卡片一致：不加载任何外部资源（字体、图片、脚本都不引；头像由
//! [`super::avatar`] 先下回来，以 data URL 内嵌），所有动态文本一律转义，出图交给
//! [`crate::render::web::shoot`]——量高、等字体、尺寸护栏与闸门都在那一处。

use super::collect::Material;
use super::persona::{DIMENSIONS, LAYERS, Persona, Tag};
use anyhow::Result;
use chrono::{DateTime, FixedOffset, Timelike, Utc};

/// 卡片渲染宽度（CSS 像素）。出图宽度 = `WIDTH × scale`。
const WIDTH: u32 = 720;
/// 高度上限（CSS 像素），与其它卡片一致。
const CAPTURE_MAX_HEIGHT: f64 = 16_000.0;

/// 明暗两套主题。切换只动明度与文字三档灰，不动版式，切换后仍像同一份东西。
/// 两套都是纸色：日读偏暖白，夜读偏墨黑，衬线字落在上面才不显生。
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

/// 四位数以上的计数加千分位，扫一眼就知道量级。
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
<body class="scheme-portrait md-text {seed}{theme_class}"><div class="shot"><div class="card md-card">
{eyebrow}
{hero}
{deck}
{composite}
{readings}
{taxonomy}
{profile}
{foot}
</div></div></body></html>"#,
        css = css,
        eyebrow = eyebrow(view),
        hero = hero(view),
        deck = deck(),
        seed = accent.seed_class(),
        theme_class = theme.vars(),
        composite = composite(view.persona),
        readings = readings(view.material),
        taxonomy = taxonomy(view.persona),
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
        r#"<div class="hero"><div class="avatar">{face}</div><div class="who"><div class="who-name">{}</div><div class="who-meta">{}</div></div></div>"#,
        esc(&material.name),
        bits.join(r#"<span class="md-sep">·</span>"#),
    )
}

/// 这份东西是什么。画像的定义写在这儿，读者才知道手里拿的不是一份判决。
fn deck() -> String {
    r#"<div class="deck">从本人群聊行为里抽象出的<b>标签化用户模型</b>——可核验，也有损</div>"#
        .to_string()
}

fn composite(persona: &Persona) -> String {
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
        r#"<div class="title-block"><div class="label-row"><span class="label">综合标签</span>{pill}</div><div class="composite">{}</div>{note}</div>"#,
        esc(if persona.title.is_empty() {
            "尚未归纳"
        } else {
            &persona.title
        }),
    )
}

/// 一行读数。数字只做旁证，不铺成仪表盘，省下的版面留给标签与综述。
///
/// 只留五个格子：最密的钟点在活跃维度的标签里本来就写着，这里再放一格只会把这一行挤断。
fn readings(material: &Material) -> String {
    let items = [
        ("发言", fmt_num(material.total), ""),
        ("活跃", fmt_num(material.active_days), "天"),
        ("跨度", material.span_days().to_string(), "天"),
        ("均长", format!("{:.1}", material.avg_len()), "字"),
        ("充分性", material.sufficiency().label().to_string(), ""),
    ];
    let cells: String = items
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
        .collect();
    format!(r#"<div class="md-readings md-readings-bordered">{cells}</div>"#)
}

/// 标签体系。四个维度按固定次序排，空的维度不占版面；每条标签先给层级徽章，再给标签与证据。
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
        head = sec_head("标签体系", "TAG SYSTEM"),
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
        head = sec_head("画像综述", "THE PROFILE")
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
        r#"<div class="foot md-foot"><div>观测区间 {range}<span class="md-sep">·</span>样本 {} 条<span class="md-sep">·</span>模型 {model}</div><div class="foot-note">画像是对行为的抽象，有损：只含他在群里说过的部分，不等于本人。仅供娱乐，不作凭据。</div></div>"#,
        material.samples.len(),
        range = esc(&range),
        model = esc(view.model),
    )
}

/// 出图。`scale` 是设备像素比，限制在 1—4 倍，与其它卡片一致。
///
/// 量高度、等字体、尺寸护栏与并发闸门都在 [`crate::render::web::shoot`] 里，
/// 这里只声明这张卡片自己的宽度、格式与选择器。
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

   与另外五张卡的唯一差别是字体：显示级的文字用衬线（综合标签、维度名、
   综述与引语）。这是版面选择不是设计系统的分歧——衬线落在纸色上才像一份
   「写下来的东西」，而这份报告正是要读成一份东西，不是一块仪表盘。
   衬线在 46px 上要把字重收到 700：Black(800) 的字脚在纸上会糊成一团。 */
body{width:720px}
/* `.shot`（相纸的底色与内边距）由 m3e.css 的组件基元给，这里不再写一遍 */
.card{padding:var(--md-space-9) 44px var(--md-space-8)}
/* 顶沿一条主色细线：这张卡与手册、资讯两张同尺寸的卡一眼分开 */
.card::after{content:"";position:absolute;top:0;left:0;right:0;height:3px;
  background:linear-gradient(90deg,transparent,var(--md-sys-color-primary-line) 22%,
    var(--md-sys-color-primary-line) 78%,transparent)}

/* —— 主体：头像 + 名字 —— */
.hero{display:flex;align-items:center;gap:var(--md-space-5)}
.avatar{flex:none;display:flex;align-items:center;justify-content:center;width:84px;height:84px;
  border-radius:var(--md-shape-full);font-size:var(--md-type-headline-medium-size);
  font-weight:800;letter-spacing:0;overflow:hidden;
  color:var(--md-sys-color-primary);
  background:var(--md-sys-color-primary-container);
  border:2px solid var(--md-sys-color-primary-line)}
.avatar img{display:block;width:100%;height:100%;object-fit:cover}
.who{min-width:0}
.who-name{font-size:var(--md-type-headline-medium-size);line-height:var(--md-type-headline-medium-line);
  font-weight:var(--md-type-headline-medium-weight);letter-spacing:var(--md-type-headline-medium-track);
  color:var(--md-sys-color-on-surface)}
.who-meta{margin-top:9px;font-size:var(--md-type-label-medium-size);line-height:1.6;
  font-weight:500;color:var(--md-sys-color-on-surface-variant)}

/* —— 定义行 —— */
/* 这句话是这份东西的定义（它是什么、边界在哪），不是标题的补充说明，
   所以压在标题下方一档，不抢读，但要读得到。 */
.deck{margin-top:22px;font-size:var(--md-type-label-large-size);line-height:1.72;
  color:var(--md-sys-color-on-surface-variant)}
.deck b{font-weight:700;color:var(--md-sys-color-on-surface)}

/* —— 综合标签 —— */
.title-block{margin-top:26px}
.label-row{display:flex;align-items:center;gap:var(--md-space-3);flex-wrap:wrap}
.label{font-size:var(--md-type-label-small-size);font-weight:var(--md-type-label-small-weight);
  letter-spacing:.22em;color:var(--md-sys-color-on-surface-faint)}
.composite{margin-top:var(--md-space-3);font-family:var(--md-font-display);
  font-size:var(--md-type-display-medium-size);line-height:var(--md-type-display-medium-line);
  font-weight:700;letter-spacing:-.005em;color:var(--md-sys-color-on-surface)}
/* 模型的补充说明用主色衬线：它是这份画像里唯一「说出来的话」 */
.note{margin-top:var(--md-space-3);font-family:var(--md-font-display);
  font-size:var(--md-type-title-small-size);line-height:1.66;font-weight:600;
  color:var(--md-sys-color-primary)}

/* —— 读数 —— */
.readings{margin-top:var(--md-space-6)}

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

/* —— 标签的层级：图例与徽章同一套三种形态 ——
   事实＝浅底（观测到的）、统计＝主色淡底（算出来的）、推断＝主色实心（读出来的）。
   从硬到软一条线，读者一眼知道哪几条能拿去用、哪几条只是读出来的。 */
.legend{display:flex;flex-wrap:wrap;gap:var(--md-space-2) 22px;margin-bottom:18px}
.legend-item{display:inline-flex;align-items:baseline;gap:var(--md-space-2)}
.legend-item b,.tag-layer{display:inline-block;padding:2px 9px;border-radius:var(--md-shape-s);
  font-size:var(--md-type-label-medium-size);font-weight:800;letter-spacing:.06em}
.legend-item i{font-style:normal;font-size:var(--md-type-label-small-size);
  font-weight:var(--md-type-label-small-weight);letter-spacing:.2em;
  color:var(--md-sys-color-on-surface-faint)}
/* 浅底那一档用 container-high 而不是 container：它要与卡面分得开，
   差一档的话在纸色上几乎看不见。 */
.observed{color:var(--md-sys-color-on-surface-variant);
  background:var(--md-sys-color-surface-container-high)}
.derived{color:var(--md-sys-color-on-primary-container);
  background:var(--md-sys-color-primary-container)}
.inferred{color:var(--md-sys-color-on-primary);background:var(--md-sys-color-primary)}

/* —— 标签体系 —— */
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

/* —— 综述 —— */
/* 衬线长文的行高要比无衬线再放一格（1.86），字面小一档也更耐读 */
.prose{margin-bottom:18px;font-family:var(--md-font-display);
  font-size:var(--md-type-body-large-size);line-height:1.86;
  color:var(--md-sys-color-on-surface-variant);text-indent:2em}
.prose:last-child{margin-bottom:0}
/* 引语：主色淡底 + 左界 + 收一个角，与资讯卡的「推荐理由」同一形 */
.quote{margin:22px 0;padding:var(--md-space-5) 22px;border-radius:var(--md-shape-m);
  background:var(--md-sys-color-primary-tint);
  border-left:3px solid var(--md-sys-color-primary-line)}
.quote-text{margin:0;font-family:var(--md-font-display);
  font-size:var(--md-type-body-large-size);line-height:1.86;
  color:var(--md-sys-color-on-surface);text-indent:0}
.quote-note{margin-top:11px;font-size:var(--md-type-label-medium-size);line-height:1.62;
  color:var(--md-sys-color-on-surface-faint)}

/* —— 页脚 —— */
.foot-note{color:var(--md-sys-color-on-surface-variant)}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// 版式里不许出现 HTML 的成对标签——样式表塞进 `style` 元素时会被截断，
    /// 而页面不会报错（见 [`crate::render::web::assert_embeddable`]）。
    #[test]
    fn stylesheet_stays_embeddable() {
        crate::render::web::assert_embeddable("portrait", CSS);
    }
    use crate::plugins::portrait::collect::{GroupSlice, Kinds};
    use crate::plugins::portrait::persona::Passage;

    fn offset() -> FixedOffset {
        FixedOffset::east_opt(8 * 3600).unwrap()
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

    fn persona() -> Persona {
        Persona {
            title: "用忙碌挡空的人".into(),
            note: "他把休息也算成一件事".into(),
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
                Passage {
                    kind: "text".into(),
                    body: "说这话的时候他不带怨气，像在报账。".into(),
                    ..Default::default()
                },
            ],
            accent: "indigo".into(),
            estimated: false,
        }
    }

    /// 北京时间 10:13，用来让 `auto` 落在日读一侧。
    const MORNING: i64 = 1_700_014_400;
    /// 北京时间 00:13，用来让 `auto` 落在夜读一侧。
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
            "标签化用户模型",
            "综合标签",
            "用忙碌挡空的人",
            "他把休息也算成一件事",
            r#"<span class="sec-mark">标签体系</span>"#,
            r#"<span class="sec-mark">画像综述</span>"#,
            "夜里出现",
            "夜间发言占 41%",
            "他把每件事都当成一件要交的活。",
            "凌晨三点还在改代码，明天又要废了",
            "他自己知道在拿什么换",
            "不等于本人",
            "1,234",
            "充分",
            "deepseek/deepseek-flash",
        ] {
            assert!(html.contains(needle), "缺少 {needle}");
        }
    }

    /// 四个维度按固定次序排，缺的维度不占版面。
    #[test]
    fn dimensions_are_ordered_and_empty_ones_disappear() {
        let material = material();
        let base = persona();
        let one = html(&view(&material, &base));
        assert!(one.contains(r#"<span class="dim-name">活跃</span>"#));
        assert!(one.contains(r#"<span class="dim-name">内容</span>"#));
        assert!(!one.contains(r#"<span class="dim-name">交互</span>"#));
        assert!(!one.contains(r#"<span class="dim-name">表达</span>"#));
        let at = |name: &str| one.find(&format!(r#"<span class="dim-name">{name}</span>"#));
        assert!(
            at("活跃").unwrap() < at("内容").unwrap(),
            "维度要先活跃后内容"
        );

        let all = Persona {
            tags: vec![
                tag("表达", "事实", "句子短", ""),
                tag("交互", "事实", "爱接话", ""),
                tag("活跃", "事实", "夜里出现", ""),
                tag("内容", "事实", "只聊技术", ""),
            ],
            ..base
        };
        let html = html(&view(&material, &all));
        let order: Vec<usize> = ["活跃", "内容", "交互", "表达"]
            .iter()
            .map(|name| {
                html.find(&format!(r#"<span class="dim-name">{name}</span>"#))
                    .unwrap()
            })
            .collect();
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");
    }

    /// 三层抽象各有各的徽章：事实浅底、统计描边、推断实心，认不出来的一律按推断走。
    #[test]
    fn the_three_layers_get_three_badges() {
        let material = material();
        let base = persona();
        let one = html(&view(&material, &base));
        assert!(one.contains(r#"<span class="tag-layer observed">事实</span>"#));
        assert!(one.contains(r#"<span class="tag-layer derived">统计</span>"#));
        assert!(one.contains(r#"<span class="tag-layer inferred">推断</span>"#));
        // 图例把三层的意思写在版面上，读者不用猜徽章是什么。
        for en in ["OBSERVED", "DERIVED", "INFERRED"] {
            assert!(one.contains(en), "图例缺少 {en}");
        }

        let unknown = Persona {
            tags: vec![tag("活跃", "感觉", "说不清", "")],
            ..base
        };
        let html = html(&view(&material, &unknown));
        assert!(html.contains(r#"<span class="tag-layer inferred">推断</span>"#));
    }

    /// 昵称来自群聊，必须转义；模型给的文本同理。
    #[test]
    fn external_text_is_escaped() {
        let material = material();
        let persona = persona();
        let html = html(&view(&material, &persona));
        assert!(!html.contains("阿<甲>"));
        assert!(html.contains("阿&lt;甲&gt;"));
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

    #[test]
    fn estimated_reports_are_labelled() {
        let material = material();
        let persona = Persona {
            estimated: true,
            ..persona()
        };
        let html = html(&view(&material, &persona));
        assert!(html.contains("模型未接，标签由统计直出"));
    }

    /// 空的维度与空的段落都不占版面；综合标签与读数始终在——画像的骨头是数据。
    #[test]
    fn empty_sections_disappear_and_the_numbers_stay() {
        let material = material();
        let persona = Persona {
            tags: Vec::new(),
            profile: Vec::new(),
            note: String::new(),
            ..persona()
        };
        let html = html(&view(&material, &persona));
        assert!(!html.contains(r#"<span class="sec-mark">标签体系</span>"#));
        assert!(!html.contains(r#"<span class="sec-mark">画像综述</span>"#));
        assert!(html.contains(r#"<div class="md-readings md-readings-bordered">"#));
        assert!(html.contains("用忙碌挡空的人"));
    }

    /// 综述里空掉的那一段不占版面。
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
        // 认不出来的写法按自动走。
        assert_eq!(Theme::resolve("乱写", at(MIDNIGHT)), Theme::Dark);
    }

    /// 配置里写死主题时，出图时刻不再影响用哪一套。
    #[test]
    fn a_pinned_theme_overrides_the_clock() {
        let material = material();
        let persona = persona();
        let pinned = View {
            theme: "light",
            ..view_at(&material, &persona, MIDNIGHT)
        };
        // 主题现在落成 body 上的一个类名（配色方案在 res/cards/m3e.css 里按它分档），
        // 不再是往页面里塞一段变量。整个页面里也含样式表，「dark」在 CSS 里到处都是，
        // 所以只取 body 那一段来判。
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
        // 自动档：同一份素材在上午与午夜应当落在两套配色上
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
            // 报告一定比占位视口高，否则说明高度没量到、底部被切。
            assert!(image.height() > 2000, "{name} height = {}", image.height());
            let path = std::env::temp_dir().join(format!("ayjx-portrait-{name}.jpg"));
            std::fs::write(&path, &bytes).ok();
            println!("出图已写入 {}", path.display());
        }
        cdp_html_shot::Browser::shutdown_global().await;
    }
}
