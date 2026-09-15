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
            Theme::Light => {
                r#"color-scheme:light;
  --canvas:#E9E3D9;--surface:#FCFAF6;
  --title:#1C1A17;--strong:#2E2A24;--body:#45403A;
  --subtle:#5C554C;--muted:#6E675D;--faint:#736C61;
  --line:rgba(28,26,23,.10);--strong-line:rgba(28,26,23,.15);
  --panel:rgba(28,26,23,.032);--panel-border:rgba(28,26,23,.075);
  --track:rgba(28,26,23,.08);
  --pattern:rgba(28,26,23,.028);--shadow:0 18px 44px rgba(40,32,20,.12);
  --glow-alpha:.10;--chip-alpha:.09;--bar-alpha:.15"#
            }
            Theme::Dark => {
                r#"color-scheme:dark;
  --canvas:#0D0C0B;--surface:#171614;
  --title:#F2EEE6;--strong:#E6E1D8;--body:#CFC9BF;
  --subtle:#A8A198;--muted:#99928A;--faint:#A19A90;
  --line:rgba(240,236,228,.09);--strong-line:rgba(240,236,228,.14);
  --panel:rgba(240,236,228,.05);--panel-border:rgba(240,236,228,.10);
  --track:rgba(240,236,228,.10);
  --pattern:rgba(240,236,228,.024);--shadow:0 18px 48px rgba(0,0,0,.34);
  --glow-alpha:.09;--chip-alpha:.12;--bar-alpha:.20"#
            }
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
    let dark = theme == Theme::Dark;
    // 显示级的文字用衬线：综合标签、维度名、综述与引语。Android/Surface 上常见的
    // 中宋是 Noto Serif CJK 与 OPPO Serif，兜底再退到系统 serif。
    let serif = r#"--serif:"Noto Serif CJK SC","OPPO Serif SC","Source Han Serif SC","Songti SC","Noto Serif SC",Georgia,"Times New Roman",serif;"#;
    let css = format!(":root{{{serif}{}}}\n{CSS}", theme.vars())
        .replace("__ACCENT__", accent.hex(dark))
        .replace("__RGB__", accent.rgb(dark));

    format!(
        r#"<!DOCTYPE html><html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:"><style>{css}</style></head>
<body><div class="shot"><div class="card">
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
        r#"<div class="eyebrow"><div class="kicker"><span class="dot"></span>用户画像<span class="kicker-en">USER PROFILE</span></div><div class="stamp">{}</div></div>"#,
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
        bits.join(r#"<span class="sep">·</span>"#),
    )
}

/// 这份东西是什么。画像的定义写在这儿，读者才知道手里拿的不是一份判决。
fn deck() -> String {
    r#"<div class="deck">从本人群聊行为里抽象出的<b>标签化用户模型</b>——可核验，也有损</div>"#
        .to_string()
}

fn composite(persona: &Persona) -> String {
    let pill = if persona.estimated {
        r#"<span class="pill">模型未接，标签由统计直出</span>"#.to_string()
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
                format!(r#"<span class="ru">{}</span>"#, esc(unit))
            };
            format!(
                r#"<span class="reading"><span class="rk">{}</span><span class="rv">{}</span>{unit}</span>"#,
                esc(label),
                esc(&value)
            )
        })
        .collect();
    format!(r#"<div class="readings">{cells}</div>"#)
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
        r#"<div class="foot"><div>观测区间 {range}<span class="sep">·</span>样本 {} 条<span class="sep">·</span>模型 {model}</div><div class="foot-note">画像是对行为的抽象，有损：只含他在群里说过的部分，不等于本人。仅供娱乐，不作凭据。</div></div>"#,
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
*{margin:0;padding:0;box-sizing:border-box}
body{width:720px}
.shot{padding:22px;background:var(--canvas)}
.card{position:relative;overflow:hidden;border-radius:20px;padding:42px 44px 34px;
  background:var(--surface);border:1px solid var(--strong-line);box-shadow:var(--shadow);
  background-image:linear-gradient(145deg,var(--panel),transparent 420px);
  font-family:"PingFang SC","Noto Sans CJK SC","Source Han Sans SC","Microsoft YaHei","WenQuanYi Zen Hei","Helvetica Neue",Arial,sans-serif;
  color:var(--body);-webkit-font-smoothing:antialiased;text-rendering:geometricPrecision;
  overflow-wrap:anywhere;word-break:normal}
.card::before{content:"";position:absolute;top:-260px;left:-160px;width:540px;height:540px;
  border-radius:50%;background:radial-gradient(circle,rgba(__RGB__,var(--glow-alpha)),transparent 70%);filter:blur(110px);pointer-events:none}
.card::after{content:"";position:absolute;top:0;left:0;right:0;height:3px;
  background:linear-gradient(90deg,transparent,rgba(__RGB__,.55) 22%,rgba(__RGB__,.55) 78%,transparent)}
.card>*{position:relative}

/* —— 页眉 —— */
.eyebrow{display:flex;align-items:flex-end;justify-content:space-between;gap:16px;margin-bottom:24px}
.kicker{display:flex;align-items:center;gap:11px;font-size:15.5px;font-weight:800;
  letter-spacing:.18em;color:__ACCENT__;white-space:nowrap}
.dot{flex:none;width:8px;height:8px;border-radius:50%;background:__ACCENT__;
  box-shadow:0 0 0 5px rgba(__RGB__,var(--glow-alpha))}
.kicker-en{font-size:10px;font-weight:700;letter-spacing:.2em;color:var(--faint);
  line-height:1.4}
.stamp{font-size:14px;color:var(--faint);letter-spacing:.03em;white-space:nowrap;
  font-variant-numeric:tabular-nums}

/* —— 主体：头像 + 名字 —— */
.hero{display:flex;align-items:center;gap:20px}
.avatar{flex:none;display:flex;align-items:center;justify-content:center;width:84px;height:84px;
  border-radius:50%;font-size:35px;font-weight:800;color:__ACCENT__;
  background:rgba(__RGB__,var(--chip-alpha));border:2px solid rgba(__RGB__,.28);letter-spacing:0;
  overflow:hidden}
.avatar img{display:block;width:100%;height:100%;object-fit:cover}
.who{min-width:0}
.who-name{text-wrap:balance;font-size:33px;line-height:1.28;font-weight:800;letter-spacing:-.015em;color:var(--title)}
.who-meta{margin-top:9px;font-size:15px;line-height:1.6;font-weight:500;color:var(--muted)}
.sep{margin:0 8px;color:var(--faint)}

/* —— 定义行 —— */
.deck{margin-top:22px;font-size:15.5px;line-height:1.72;color:var(--muted)}
.deck b{font-weight:700;color:var(--subtle)}

/* —— 综合标签 —— */
.title-block{margin-top:26px}
.label-row{display:flex;align-items:center;gap:12px;flex-wrap:wrap}
.label{font-size:13px;font-weight:800;letter-spacing:.22em;color:var(--faint)}
.pill{padding:3px 10px;border-radius:8px;font-size:12.5px;font-weight:700;letter-spacing:.02em;
  color:__ACCENT__;background:rgba(__RGB__,var(--chip-alpha))}
.composite{text-wrap:balance;margin-top:12px;font-family:var(--serif);font-size:46px;line-height:1.26;font-weight:700;
  letter-spacing:.01em;color:var(--title)}
.note{margin-top:12px;font-family:var(--serif);font-size:21px;line-height:1.66;font-weight:600;
  color:__ACCENT__}

/* —— 读数 —— */
/* 只画上缘那一条。下缘再画一条的话，紧跟着的分节自己还有一条上缘线，中间空着
   34px 的两道平行细线，看着像漏了一行内容。全篇的分隔线统一「块首一条」。 */
.readings{display:flex;flex-wrap:wrap;gap:10px 24px;margin-top:24px;padding:16px 2px;
  border-top:1px solid var(--line)}
.reading{display:inline-flex;align-items:baseline;gap:5px;font-size:14.5px;line-height:1.5}
.rk{color:var(--faint);letter-spacing:.06em}
.rv{font-size:17px;font-weight:700;color:__ACCENT__;font-variant-numeric:tabular-nums}
.ru{color:var(--faint)}

/* —— 分节 —— */
.sec{margin-top:34px;padding-top:28px;border-top:1px solid var(--line)}
.sec-head{display:flex;align-items:center;gap:14px;margin-bottom:20px}
.sec-mark{font-family:var(--serif);font-size:22px;font-weight:700;letter-spacing:.14em;
  color:var(--title);white-space:nowrap}
.sec-en{font-size:10.5px;font-weight:700;letter-spacing:.3em;color:var(--faint);white-space:nowrap}
.sec-head::after{content:"";flex:1;height:1px;background:var(--strong-line)}

/* —— 标签体系的图例 —— */
.legend{display:flex;flex-wrap:wrap;gap:8px 22px;margin-bottom:18px}
.legend-item{display:inline-flex;align-items:baseline;gap:8px}
.legend-item b{display:inline-block;padding:2px 9px;border-radius:6px;font-size:12px;
  font-weight:800;letter-spacing:.06em}
.legend-item b.observed{color:var(--muted);background:var(--track)}
.legend-item b.derived{color:__ACCENT__;background:rgba(__RGB__,var(--chip-alpha))}
.legend-item b.inferred{color:var(--surface);background:rgba(__RGB__,.82)}
.legend-item i{font-style:normal;font-size:10px;font-weight:700;letter-spacing:.2em;
  color:var(--faint)}

/* —— 标签体系 —— */
.dims{display:flex;flex-direction:column;gap:16px}
.dim{padding:16px 22px 4px;border-radius:14px;background:var(--panel);
  border:1px solid var(--panel-border)}
.dim-head{display:flex;align-items:baseline;gap:11px;padding-bottom:11px;
  border-bottom:1px solid var(--line)}
.dim-name{font-family:var(--serif);font-size:19px;font-weight:700;letter-spacing:.14em;
  color:var(--title)}
.dim-en{font-size:10px;font-weight:700;letter-spacing:.26em;color:var(--faint)}
.tag{display:flex;gap:13px;padding:12px 0;border-bottom:1px dashed var(--line)}
.tag:last-child{border-bottom:none}
.tag-layer{flex:none;align-self:flex-start;width:50px;padding:3px 0;margin-top:2px;
  text-align:center;border-radius:7px;font-size:13px;font-weight:700;letter-spacing:.06em}
.tag-layer.observed{color:var(--muted);background:var(--track)}
.tag-layer.derived{color:__ACCENT__;background:rgba(__RGB__,var(--chip-alpha))}
.tag-layer.inferred{color:var(--surface);background:rgba(__RGB__,.82)}
.tag-main{flex:1;min-width:0}
.tag-label{display:block;font-size:19px;line-height:1.6;font-weight:700;color:var(--strong)}
.tag-ev{display:block;margin-top:5px;font-size:17px;line-height:1.75;color:var(--muted)}

/* —— 综述 —— */
.prose{margin-bottom:18px;font-family:var(--serif);font-size:21px;line-height:1.88;
  color:var(--body);text-indent:2em}
.prose:last-child{margin-bottom:0}
.quote{margin:22px 0;padding:20px 22px;border-radius:12px;background:rgba(__RGB__,var(--chip-alpha));
  border-left:3px solid rgba(__RGB__,.55)}
.quote-text{margin:0;font-family:var(--serif);font-size:20.5px;line-height:1.86;color:var(--strong);
  text-indent:0}
.quote-note{margin-top:11px;font-size:15px;line-height:1.62;color:var(--faint)}

/* —— 页脚 —— */
.foot{display:flex;flex-direction:column;gap:7px;margin-top:32px;padding-top:20px;
  border-top:1px solid var(--strong-line);
  font-size:14px;line-height:1.62;color:var(--faint)}
.foot-note{color:var(--muted)}
.foot .sep{margin:0 7px}
"#;

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(html.contains(r#"<div class="readings">"#));
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
        let html = html(&pinned);
        assert!(html.contains("--canvas:#E9E3D9"), "应当用日读配色");
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
