//! 画像报告卡（HTML → 截图）。
//!
//! 版式按「一封信」来排，不按「仪表盘」来排：先给判断，再给三面侧写，
//! 侧写后面落一签，最后才是刻度、引语与节律这些旁证。正文 19.5—20.5px、
//! 行高 1.8—1.85、版心 720px，中文一行约 30 字；显示级的文字用衬线（签诗、
//! 代号、引语），余下用无衬线，保证手机上缩略图能读出标题、点开长文不累。
//!
//! 约束与本仓库其它卡片一致：不加载任何外部资源（字体、图片、脚本都不引；
//! 头像由 [`super::avatar`] 先下回来，以 data URL 内嵌），所有动态文本一律转义，
//! 出图走 `TabGuard` 并在 45 秒处兜底。

use super::collect::Material;
use super::persona::Persona;
use crate::render::web::TabGuard;
use anyhow::Result;
use cdp_html_shot::{Browser, CaptureOptions, Viewport};
use chrono::{DateTime, FixedOffset, Timelike, Utc};
use std::time::Duration;

/// 卡片渲染宽度（CSS 像素）。出图宽度 = `WIDTH × scale`。
const WIDTH: u32 = 720;
/// 截图上限，与其它卡片保持一致。
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(45);

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
  --subtle:#5C554C;--muted:#6E675D;--faint:#8A8276;
  --line:rgba(28,26,23,.10);--strong-line:rgba(28,26,23,.15);
  --panel:rgba(28,26,23,.032);--panel-border:rgba(28,26,23,.075);
  --chip:rgba(28,26,23,.05);--track:rgba(28,26,23,.08);
  --pattern:rgba(28,26,23,.028);--shadow:0 18px 44px rgba(40,32,20,.12);
  --glow-alpha:.10;--chip-alpha:.09;--quote-alpha:.05;--bar-alpha:.15"#
            }
            Theme::Dark => {
                r#"color-scheme:dark;
  --canvas:#0D0C0B;--surface:#171614;
  --title:#F2EEE6;--strong:#E6E1D8;--body:#CFC9BF;
  --subtle:#A8A198;--muted:#99928A;--faint:#837C74;
  --line:rgba(240,236,228,.09);--strong-line:rgba(240,236,228,.14);
  --panel:rgba(240,236,228,.05);--panel-border:rgba(240,236,228,.10);
  --chip:rgba(240,236,228,.07);--track:rgba(240,236,228,.10);
  --pattern:rgba(240,236,228,.024);--shadow:0 18px 48px rgba(0,0,0,.34);
  --glow-alpha:.09;--chip-alpha:.12;--quote-alpha:.055;--bar-alpha:.20"#
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

/// 签号写成汉字，第 47 签比第 47 个数字更像一支签。
fn cn_num(value: i64) -> String {
    const DIGITS: [&str; 10] = ["零", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
    let value = value.clamp(1, 100);
    if value == 100 {
        return "一百".to_string();
    }
    let (tens, ones) = (value / 10, value % 10);
    let mut out = String::new();
    if tens > 0 {
        if tens > 1 {
            out.push_str(DIGITS[tens as usize]);
        }
        out.push('十');
    }
    if ones > 0 {
        out.push_str(DIGITS[ones as usize]);
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
    // 显示级的文字用衬线：签诗、代号、引语、赠言。Android/Surface 上常见的
    // 中宋是 Noto Serif CJK 与 OPPO Serif，兜底再退到系统 serif。
    let serif = r#"--serif:"Noto Serif CJK SC","OPPO Serif SC","Source Han Serif SC","Songti SC","Noto Serif SC",Georgia,"Times New Roman",serif;"#;
    let css = format!(":root{{{serif}{}}}\n{CSS}", theme.vars())
        .replace("__ACCENT__", accent.hex(dark))
        .replace("__RGB__", accent.rgb(dark));

    let material = view.material;
    let persona = view.persona;

    format!(
        r#"<!DOCTYPE html><html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><style>{css}</style></head>
<body><div class="shot"><div class="card">
{eyebrow}
{hero}
{codename}
{lead}
{readings}
{facets}
{lot}
{traits}
{interests}
{quotes}
{rhythm}
{advice}
{foot}
</div></div></body></html>"#,
        css = css,
        eyebrow = eyebrow(view),
        hero = hero(view),
        codename = codename(persona),
        lead = lead(&persona.summary),
        readings = readings(material),
        facets = facets(persona),
        lot = lot(persona),
        traits = traits(&persona.traits),
        interests = interests(&persona.interests),
        quotes = quotes(&persona.quotes),
        rhythm = rhythm(view),
        advice = advice(&persona.advice),
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
        r#"<div class="eyebrow"><div class="kicker"><span class="dot"></span>用户画像<span class="kicker-en">PORTRAIT</span></div><div class="stamp">{}</div></div>"#,
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

fn codename(persona: &Persona) -> String {
    let pill = if persona.estimated {
        r#"<span class="pill">纯统计版</span>"#.to_string()
    } else {
        String::new()
    };
    format!(
        r#"<div class="codename-block"><div class="label-row"><span class="label">画像代号</span>{pill}</div><div class="codename">{}</div><div class="tagline">{}</div></div>"#,
        esc(if persona.codename.is_empty() {
            "尚未命名"
        } else {
            &persona.codename
        }),
        esc(&persona.tagline),
    )
}

fn lead(summary: &str) -> String {
    if summary.trim().is_empty() {
        return String::new();
    }
    format!(r#"<div class="lead">{}</div>"#, esc(summary))
}

/// 一行读数。数字只做旁证，不铺成仪表盘，省下的版面留给侧写。
fn readings(material: &Material) -> String {
    let items = [
        ("发言", fmt_num(material.total), ""),
        ("活跃", fmt_num(material.active_days), "天"),
        ("跨度", material.span_days().to_string(), "天"),
        ("均长", format!("{:.1}", material.avg_len()), "字"),
        ("最活跃", material.peak_hour().to_string(), "点"),
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

/// 三面侧写：报告的主体。空的面向不占版面。
fn facets(persona: &Persona) -> String {
    let blocks: String = persona
        .live_facets()
        .map(|facet| {
            let title = if facet.title.trim().is_empty() {
                String::new()
            } else {
                format!(r#"<div class="facet-title">{}</div>"#, esc(&facet.title))
            };
            let body = if facet.body.trim().is_empty() {
                String::new()
            } else {
                format!(r#"<div class="facet-body">{}</div>"#, esc(&facet.body))
            };
            format!(
                r#"<div class="facet"><div class="facet-key">{}</div>{title}{body}</div>"#,
                esc(&facet.key)
            )
        })
        .collect();
    if blocks.is_empty() {
        return String::new();
    }
    format!(
        r#"<div class="sec">{}{blocks}</div>"#,
        sec_head("侧写", "THREE ASPECTS")
    )
}

/// 抽的那一签。排成一张签纸：上下细线、居中，签诗用衬线。
fn lot(persona: &Persona) -> String {
    if !persona.has_lot() {
        return String::new();
    }
    let verse: String = persona
        .lot
        .verse
        .iter()
        .map(|line| format!(r#"<span class="verse-line">{}</span>"#, esc(line)))
        .collect();
    let reading = if persona.lot.reading.trim().is_empty() {
        String::new()
    } else {
        format!(
            r#"<div class="lot-reading">{}</div>"#,
            esc(&persona.lot.reading)
        )
    };
    format!(
        r#"<div class="sec">{head}<div class="lot"><div class="lot-no">第{}签</div><div class="lot-grade">{}</div><div class="lot-verse">{verse}</div>{reading}</div></div>"#,
        esc(&cn_num(persona.lot.no)),
        esc(if persona.lot.grade.is_empty() {
            "中平"
        } else {
            &persona.lot.grade
        }),
        head = sec_head("签", "THE LOT"),
    )
}

/// 心理刻度。条形压细、颜色收暗，是旁证不是成绩单。
fn traits(traits: &[super::persona::Trait]) -> String {
    if traits.is_empty() {
        return String::new();
    }
    let rows: String = traits
        .iter()
        .map(|item| {
            format!(
                r#"<div class="trait"><div class="trait-top"><span class="trait-name">{}</span><span class="trait-score">{:.0}</span></div><div class="track"><i style="width:{:.1}%"></i></div><div class="trait-note">{}</div></div>"#,
                esc(&item.name),
                item.score,
                item.score.clamp(2.0, 100.0),
                esc(&item.note)
            )
        })
        .collect();
    format!(
        r#"<div class="sec">{}{rows}</div>"#,
        sec_head("刻度", "MEASURES")
    )
}

fn interests(words: &[String]) -> String {
    if words.is_empty() {
        return String::new();
    }
    let chips: String = words
        .iter()
        .map(|word| format!(r#"<span class="chip">{}</span>"#, esc(word)))
        .collect();
    format!(
        r#"<div class="sec">{}{}<div class="chips">{chips}</div></div>"#,
        sec_head("常谈的事", "SUBJECTS"),
        r#"<div class="sec-lead">他反复提起的，多半是他放不下的。</div>"#,
    )
}

fn quotes(quotes: &[super::persona::Quote]) -> String {
    if quotes.is_empty() {
        return String::new();
    }
    let items: String = quotes
        .iter()
        .map(|quote| {
            format!(
                r#"<figure class="quote"><blockquote class="quote-text">{}</blockquote><figcaption class="quote-why">{}</figcaption></figure>"#,
                esc(&quote.text),
                esc(&quote.why),
            )
        })
        .collect();
    format!(
        r#"<div class="sec">{}{items}</div>"#,
        sec_head("他自己的话", "IN HIS WORDS")
    )
}

fn rhythm(view: &View<'_>) -> String {
    let material = view.material;
    let peak = material.peak_hour();
    let max = material.hour.iter().copied().max().unwrap_or(0).max(1);
    let columns: String = material
        .hour
        .iter()
        .enumerate()
        .map(|(hour, count)| {
            let ratio = *count as f64 / max as f64;
            let height = (ratio * 100.0).max(if *count > 0 { 4.0 } else { 0.0 });
            let peak_class = if hour == peak { " peak" } else { "" };
            let label = if hour % 6 == 0 {
                format!("{hour}")
            } else {
                String::new()
            };
            format!(
                r#"<div class="col"><div class="col-bar"><i class="{}{}" style="height:{:.1}%"></i></div><div class="col-tick{}">{}</div></div>"#,
                if *count > 0 { "on" } else { "off" },
                peak_class,
                height,
                peak_class,
                esc(&label),
            )
        })
        .collect();
    format!(
        r#"<div class="sec">{head}<div class="rhythm">{columns}</div><div class="caption">{} 前后最活跃，最活跃的一天是{}，夜间（0—6 点）占 {}。</div></div>"#,
        esc(&super::persona::hour_label(peak)),
        esc(super::persona::weekday_label(material.peak_weekday())),
        esc(&super::persona::percent(material.night_ratio())),
        head = sec_head("活跃节律", "RHYTHM"),
    )
}

fn advice(advice: &str) -> String {
    if advice.trim().is_empty() {
        return String::new();
    }
    format!(
        r#"<div class="advice"><div class="advice-label">赠言</div><div class="advice-text">{}</div></div>"#,
        esc(advice)
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
        r#"<div class="foot"><div>统计区间 {range}<span class="sep">·</span>样本 {} 条<span class="sep">·</span>{model}</div><div class="foot-note">由模型读群聊记录推出，仅供娱乐，不作凭据</div></div>"#,
        material.samples.len(),
        range = esc(&range),
        model = esc(view.model),
    )
}

/// 出图。`scale` 是设备像素比，限制在 1—4 倍，与其它卡片一致。
pub async fn capture(html: &str, scale: f64) -> Result<String> {
    let scale = if scale.is_finite() {
        scale.clamp(1.0, 4.0)
    } else {
        3.0
    };
    let browser = Browser::instance().await;
    let guard = TabGuard::new(browser.new_tab().await.map_err(|e| anyhow::anyhow!(e))?);

    let result = tokio::time::timeout(CAPTURE_TIMEOUT, async {
        let tab = guard.tab();
        tab.set_viewport(&Viewport::new(WIDTH, 800).with_device_scale_factor(scale))
            .await?;
        tab.set_content(html).await?;
        // 字体就绪之前量高度会算出偏小的值，底部会被切掉；给一帧让布局落地。
        tokio::time::sleep(Duration::from_millis(200)).await;

        let height = tab
            .evaluate("document.body.scrollHeight")
            .await?
            .as_f64()
            .unwrap_or(1200.0) as u32;
        let viewport =
            Viewport::new(WIDTH, (height + 40).clamp(400, 16_000)).with_device_scale_factor(scale);
        tab.set_viewport(&viewport).await?;
        tokio::time::sleep(Duration::from_millis(120)).await;

        let options = CaptureOptions::new().with_viewport(viewport).with_quality(90);
        let shot = tab
            .find_element(".shot")
            .await?
            .screenshot_with_options(options)
            .await?;
        Ok::<String, anyhow::Error>(shot)
    })
    .await
    .map_err(|_| anyhow::anyhow!("画像卡片截图超时（{} 秒）", CAPTURE_TIMEOUT.as_secs()))?;

    guard.close().await;
    result
}

/// 页面主色取当前的北京时刻，与 `html()` 里的主题判定保持同一条规则。
pub fn now(offset: FixedOffset) -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&offset)
}

const CSS: &str = r#"
*{margin:0;padding:0;box-sizing:border-box}
.shot{padding:22px;background:var(--canvas)}
.card{position:relative;overflow:hidden;border-radius:20px;padding:42px 44px 34px;
  background:var(--surface);border:1px solid var(--strong-line);box-shadow:var(--shadow);
  background-image:radial-gradient(var(--pattern) 1px,transparent 1px);
  background-size:28px 28px;
  font-family:"PingFang SC","Noto Sans CJK SC","Source Han Sans SC","Microsoft YaHei","WenQuanYi Zen Hei","Helvetica Neue",Arial,sans-serif;
  color:var(--body);-webkit-font-smoothing:antialiased;text-rendering:geometricPrecision;
  overflow-wrap:anywhere;word-break:normal}
.card::before{content:"";position:absolute;top:-260px;left:-160px;width:540px;height:540px;
  border-radius:50%;background:rgba(__RGB__,var(--glow-alpha));filter:blur(110px);pointer-events:none}
.card::after{content:"";position:absolute;top:0;left:0;right:0;height:3px;
  background:linear-gradient(90deg,transparent,rgba(__RGB__,.55) 22%,rgba(__RGB__,.55) 78%,transparent)}
.card>*{position:relative}

/* —— 页眉 —— */
.eyebrow{display:flex;align-items:center;justify-content:space-between;margin-bottom:24px}
.kicker{display:flex;align-items:center;gap:11px;font-size:15.5px;font-weight:800;
  letter-spacing:.18em;color:__ACCENT__}
.dot{width:8px;height:8px;border-radius:50%;background:__ACCENT__;
  box-shadow:0 0 0 5px rgba(__RGB__,var(--glow-alpha))}
.kicker-en{font-size:11.5px;font-weight:700;letter-spacing:.26em;color:var(--faint)}
.stamp{font-size:14px;color:var(--faint);letter-spacing:.03em;font-variant-numeric:tabular-nums}

/* —— 主体：头像 + 名字 —— */
.hero{display:flex;align-items:center;gap:20px}
.avatar{flex:none;display:flex;align-items:center;justify-content:center;width:84px;height:84px;
  border-radius:50%;font-size:35px;font-weight:800;color:__ACCENT__;
  background:rgba(__RGB__,var(--chip-alpha));border:2px solid rgba(__RGB__,.28);letter-spacing:0;
  overflow:hidden}
.avatar img{display:block;width:100%;height:100%;object-fit:cover}
.who{min-width:0}
.who-name{font-size:33px;line-height:1.28;font-weight:800;letter-spacing:-.015em;color:var(--title)}
.who-meta{margin-top:9px;font-size:15px;line-height:1.6;font-weight:500;color:var(--muted)}
.sep{margin:0 8px;color:var(--faint)}

/* —— 代号 —— */
.codename-block{margin-top:32px}
.label-row{display:flex;align-items:center;gap:12px}
.label{font-size:13px;font-weight:800;letter-spacing:.22em;color:var(--faint)}
.pill{padding:3px 10px;border-radius:8px;font-size:12.5px;font-weight:700;letter-spacing:.02em;
  color:__ACCENT__;background:rgba(__RGB__,var(--chip-alpha))}
.codename{margin-top:12px;font-family:var(--serif);font-size:46px;line-height:1.26;font-weight:700;
  letter-spacing:.01em;color:var(--title)}
.tagline{margin-top:12px;font-family:var(--serif);font-size:21px;line-height:1.66;font-weight:600;
  color:__ACCENT__}

/* —— 总评 —— */
.lead{margin-top:26px;padding:22px 24px;border-radius:14px;
  background:var(--panel);border:1px solid var(--panel-border);
  font-family:var(--serif);font-size:20.5px;line-height:1.9;font-weight:500;color:var(--strong)}

/* —— 读数 —— */
.readings{display:flex;flex-wrap:wrap;gap:10px 24px;margin-top:24px;padding:16px 2px;
  border-top:1px solid var(--line);border-bottom:1px solid var(--line)}
.reading{display:inline-flex;align-items:baseline;gap:5px;font-size:14.5px;line-height:1.5}
.rk{color:var(--faint);letter-spacing:.06em}
.rv{font-size:17px;font-weight:700;color:__ACCENT__;font-variant-numeric:tabular-nums}
.ru{color:var(--faint)}

/* —— 分节 —— */
.sec{margin-top:34px;padding-top:28px;border-top:1px solid var(--line)}
.sec-head{display:flex;align-items:center;gap:14px;margin-bottom:20px}
.sec-mark{font-family:var(--serif);font-size:22px;font-weight:700;letter-spacing:.04em;
  color:var(--title);white-space:nowrap}
.sec-en{font-size:10.5px;font-weight:700;letter-spacing:.3em;color:var(--faint);white-space:nowrap}
.sec-head::after{content:"";flex:1;height:1px;background:var(--strong-line)}
.sec-lead{margin:-8px 0 16px;font-size:16px;line-height:1.7;color:var(--faint)}

/* —— 三面侧写 —— */
.facet+.facet{padding-top:22px;margin-top:22px;border-top:1px solid var(--line)}
.facet-key{font-family:var(--serif);font-size:15px;font-weight:700;letter-spacing:.28em;
  color:__ACCENT__}
.facet-title{margin-top:11px;font-family:var(--serif);font-size:23px;line-height:1.56;font-weight:700;
  color:var(--title)}
.facet-body{margin-top:12px;font-family:var(--serif);font-size:20px;line-height:1.92;
  color:var(--body)}

/* —— 签 —— */
.lot{position:relative;margin-top:2px;padding:30px 30px 26px;border-radius:14px;
  background:var(--panel);border:1px solid var(--panel-border);text-align:center}
.lot::before,.lot::after{content:"";position:absolute;left:22px;right:22px;height:1px;
  background:var(--strong-line)}
.lot::before{top:11px}
.lot::after{bottom:11px}
.lot-no{font-size:13px;font-weight:700;letter-spacing:.3em;color:var(--faint)}
.lot-grade{margin-top:9px;font-family:var(--serif);font-size:38px;line-height:1.3;font-weight:700;
  letter-spacing:.14em;color:__ACCENT__}
.lot-verse{margin-top:14px;display:flex;flex-direction:column;gap:6px}
.verse-line{font-family:var(--serif);font-size:21px;line-height:1.7;color:var(--strong);
  letter-spacing:.08em}
.lot-reading{margin-top:18px;padding-top:17px;border-top:1px solid var(--line);
  font-size:17.5px;line-height:1.8;color:var(--subtle)}

/* —— 刻度 —— */
.track{height:6px;border-radius:4px;background:var(--track);overflow:hidden}
.track i{display:block;height:100%;border-radius:4px;
  background:linear-gradient(90deg,rgba(__RGB__,var(--bar-alpha)),__ACCENT__)}
.trait{margin-bottom:19px}
.trait:last-child{margin-bottom:2px}
.trait-top{display:flex;align-items:baseline;justify-content:space-between;margin-bottom:9px}
.trait-name{font-size:19px;font-weight:700;color:var(--strong)}
.trait-score{font-size:18px;font-weight:700;color:__ACCENT__;font-variant-numeric:tabular-nums}
.trait-note{margin-top:8px;font-size:16px;line-height:1.68;color:var(--subtle)}

/* —— 常谈的事 —— */
.chips{display:flex;flex-wrap:wrap;gap:10px}
.chip{padding:8px 15px;border-radius:9px;font-size:17px;font-weight:600;
  color:var(--strong);background:var(--chip);border:1px solid var(--panel-border)}

/* —— 节律 —— */
.rhythm{display:grid;grid-template-columns:repeat(24,1fr);gap:3px;align-items:end}
.col{display:flex;flex-direction:column}
.col-bar{display:flex;align-items:flex-end;height:96px}
.col-bar i{display:block;width:100%;border-radius:3px 3px 2px 2px}
.col-bar i.on{background:rgba(__RGB__,var(--bar-alpha))}
.col-bar i.peak.on{background:__ACCENT__;box-shadow:0 0 0 2px rgba(__RGB__,var(--glow-alpha))}
.col-tick{margin-top:7px;height:15px;font-size:11.5px;line-height:15px;text-align:center;
  color:var(--faint);font-variant-numeric:tabular-nums}
.col-tick.peak{font-weight:800;color:__ACCENT__}
.caption{margin-top:14px;font-size:16.5px;line-height:1.72;color:var(--subtle)}

/* —— 他自己的话 —— */
.quote{margin-bottom:20px;padding:4px 0 4px 20px;border-left:2px solid rgba(__RGB__,.5)}
.quote:last-child{margin-bottom:0}
.quote-text{margin:0;font-family:var(--serif);font-size:20px;line-height:1.86;color:var(--body)}
.quote-why{margin-top:10px;font-size:15px;line-height:1.62;color:var(--faint)}

/* —— 赠言 —— */
.advice{margin-top:34px;padding:24px 26px;border-radius:14px;
  background:rgba(__RGB__,var(--quote-alpha));border:1px solid rgba(__RGB__,.22)}
.advice-label{font-size:13px;font-weight:800;letter-spacing:.24em;color:__ACCENT__}
.advice-text{margin-top:12px;font-family:var(--serif);font-size:22px;line-height:1.78;font-weight:600;
  color:var(--title)}

/* —— 页脚 —— */
.foot{display:flex;flex-direction:column;gap:7px;margin-top:32px;padding-top:20px;
  border-top:1px solid var(--strong-line);
  font-size:14px;line-height:1.62;color:var(--faint)}
.foot-note{color:var(--faint);opacity:.85}
.foot .sep{margin:0 7px}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::portrait::collect::{GroupSlice, Kinds};
    use crate::plugins::portrait::persona::{Facet, Lot, Quote, Trait};

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

    fn persona() -> Persona {
        Persona {
            codename: "用忙碌挡空的人".into(),
            tagline: "他把休息也算成一件事".into(),
            summary: "话不多，但每句都落在点上。".into(),
            facets: vec![
                Facet {
                    key: "立身".into(),
                    title: "把手艺当退路".into(),
                    body: "他把手艺当退路。".into(),
                },
                Facet {
                    key: "心相".into(),
                    title: "怕停下来".into(),
                    body: "夜里两点还在改。".into(),
                },
                Facet {
                    key: "人群".into(),
                    title: "不抢话".into(),
                    body: "只在有人问到的时候接一句。".into(),
                },
            ],
            traits: vec![Trait {
                name: "秩序感".into(),
                score: 92.0,
                note: "深夜发言占了近三成".into(),
            }],
            interests: vec!["代码".into(), "咖啡".into()],
            lot: Lot {
                no: 47,
                grade: "中吉".into(),
                verse: vec![
                    "石上栽花".into(),
                    "未开先老".into(),
                    "不如退步".into(),
                    "另有路行".into(),
                ],
                reading: "他等的不是机会，是许可。".into(),
            },
            quotes: vec![Quote {
                text: "凌晨三点还在改代码，明天又要废了".into(),
                why: "很有他".into(),
            }],
            advice: "少熬点夜。".into(),
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
            "用忙碌挡空的人",
            "他把休息也算成一件事",
            r#"<span class="sec-mark">侧写</span>"#,
            "立身",
            "心相",
            "人群",
            "把手艺当退路",
            "第四十七签",
            "中吉",
            "石上栽花",
            r#"<span class="sec-mark">刻度</span>"#,
            "秩序感",
            r#"<span class="sec-mark">常谈的事</span>"#,
            r#"<span class="sec-mark">他自己的话</span>"#,
            r#"<span class="sec-mark">活跃节律</span>"#,
            "赠言",
            "仅供娱乐",
            "1,234",
            "deepseek/deepseek-flash",
        ] {
            assert!(html.contains(needle), "缺少 {needle}");
        }
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
        assert!(without.contains(r#"<div class="avatar">阿</div>"#), "没有头像时用首字");

        let data = "data:image/jpeg;base64,AAAA";
        let with = html(&View {
            avatar: Some(data),
            ..view_at(&material, &persona, MORNING)
        });
        assert!(with.contains(&format!(r#"<div class="avatar"><img src="{data}" alt=""></div>"#)));
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
        assert!(html.contains("纯统计版"));
    }

    #[test]
    fn empty_optional_sections_disappear() {
        let material = material();
        let persona = Persona {
            traits: Vec::new(),
            interests: Vec::new(),
            quotes: Vec::new(),
            facets: Vec::new(),
            lot: Lot::default(),
            advice: String::new(),
            summary: String::new(),
            ..persona()
        };
        let html = html(&view(&material, &persona));
        assert!(!html.contains(r#"<span class="sec-mark">侧写</span>"#));
        assert!(!html.contains(r#"<span class="sec-mark">刻度</span>"#));
        assert!(!html.contains(r#"<span class="sec-mark">常谈的事</span>"#));
        assert!(!html.contains(r#"<span class="sec-mark">他自己的话</span>"#));
        assert!(!html.contains(r#"<div class="advice">"#));
        assert!(!html.contains(r#"<div class="lot">"#));
        // 代号与读数始终在。
        assert!(html.contains("用忙碌挡空的人"));
        assert!(html.contains(r#"<span class="sec-mark">活跃节律</span>"#));
        assert!(html.contains(r#"<div class="readings">"#));
    }

    /// 三面侧写里空掉的那一面不占版面。
    #[test]
    fn a_blank_facet_is_not_printed() {
        let material = material();
        let persona = Persona {
            facets: vec![
                Facet {
                    key: "立身".into(),
                    title: "把手艺当退路".into(),
                    body: String::new(),
                },
                Facet {
                    key: "心相".into(),
                    title: String::new(),
                    body: String::new(),
                },
                Facet {
                    key: "人群".into(),
                    title: "不抢话".into(),
                    body: String::new(),
                },
            ],
            ..persona()
        };
        let html = html(&view(&material, &persona));
        assert_eq!(html.matches(r#"class="facet""#).count(), 2);
        assert!(!html.contains("心相"));
    }

    #[test]
    fn the_lot_number_reads_as_chinese_numerals() {
        assert_eq!(cn_num(1), "一");
        assert_eq!(cn_num(10), "十");
        assert_eq!(cn_num(11), "十一");
        assert_eq!(cn_num(20), "二十");
        assert_eq!(cn_num(47), "四十七");
        assert_eq!(cn_num(100), "一百");
        assert_eq!(cn_num(0), "一");
        assert_eq!(cn_num(480), "一百");
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
    }
}
