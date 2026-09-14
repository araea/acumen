//! 画像报告卡（HTML → 截图）。
//!
//! 版式按「一卦一判」来排，不按仪表盘来排：先落卦象，再给总断，然后是详批，
//! 末了说一句变。读数的数字只做旁证，收在最上面一行，不另占版面。
//!
//! 卦象是这张卡上唯一的图形：六爻自下而上，阳爻一整划、阴爻断开，动的那一爻上色。
//! 它替代了从前的刻度条与 24 小时柱状图——那些是把一个人拆成指标，这一张是把一个人
//! 收成一件事。
//!
//! 约束与本仓库其它卡片一致：不加载任何外部资源（字体、图片、脚本都不引；
//! 头像由 [`super::avatar`] 先下回来，以 data URL 内嵌），所有动态文本一律转义，
//! 出图走 `TabGuard` 并在 45 秒处兜底。

use super::collect::Material;
use super::divine::{Cast, POSITION_SENSE};
use super::persona::Persona;
use crate::render::web::TabGuard;
use anyhow::Result;
use cdp_html_shot::{Browser, CaptureOptions, Viewport};
use chrono::{DateTime, FixedOffset, Timelike, Utc};
use std::fmt::Write as _;
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
  --track:rgba(28,26,23,.08);
  --pattern:rgba(28,26,23,.028);--shadow:0 18px 44px rgba(40,32,20,.12);
  --glow-alpha:.10;--chip-alpha:.09;--bar-alpha:.15"#
            }
            Theme::Dark => {
                r#"color-scheme:dark;
  --canvas:#0D0C0B;--surface:#171614;
  --title:#F2EEE6;--strong:#E6E1D8;--body:#CFC9BF;
  --subtle:#A8A198;--muted:#99928A;--faint:#837C74;
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
    /// 起好的那一卦。卦不来自模型，由 [`super::divine`] 从素材起出。
    pub cast: &'a Cast,
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
    // 显示级的文字用衬线：卦名、爻题、代号、卦辞、批语、引语。Android/Surface 上常见的
    // 中宋是 Noto Serif CJK 与 OPPO Serif，兜底再退到系统 serif。
    let serif = r#"--serif:"Noto Serif CJK SC","OPPO Serif SC","Source Han Serif SC","Songti SC","Noto Serif SC",Georgia,"Times New Roman",serif;"#;
    let css = format!(":root{{{serif}{}}}\n{CSS}", theme.vars())
        .replace("__ACCENT__", accent.hex(dark))
        .replace("__RGB__", accent.rgb(dark));

    format!(
        r#"<!DOCTYPE html><html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><style>{css}</style></head>
<body><div class="shot"><div class="card">
{eyebrow}
{hero}
{codename}
{readings}
{hexagram}
{verdict}
{passages}
{turn}
{advice}
{foot}
</div></div></body></html>"#,
        css = css,
        eyebrow = eyebrow(view),
        hero = hero(view),
        codename = codename(view.persona),
        readings = readings(view.material),
        hexagram = hexagram(view.cast),
        verdict = verdict(&view.persona.verdict),
        passages = passages(view.persona),
        turn = turn(&view.persona.turn),
        advice = advice(&view.persona.advice),
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
        r#"<div class="eyebrow"><div class="kicker"><span class="dot"></span>易经画像<span class="kicker-en">THE BOOK OF CHANGES</span></div><div class="stamp">{}</div></div>"#,
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
        r#"<span class="pill">批语未落</span>"#.to_string()
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

/// 一行读数。数字只做旁证，不铺成仪表盘，省下的版面留给批语。
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

/// 卦象。六爻自下而上排，画出来却是从上往下读——所以显示时把初爻放在最底下。
fn hexagram(cast: &Cast) -> String {
    let lines = cast.drawn();
    let rows: String = (0..6)
        .rev()
        .map(|index| {
            let line = lines[index];
            let class = match (line.yang, line.changing) {
                (true, true) => "yang moving",
                (true, false) => "yang",
                (false, true) => "yin moving",
                (false, false) => "yin",
            };
            let segments = if line.yang {
                r#"<i></i>"#.to_string()
            } else {
                r#"<i></i><i></i>"#.to_string()
            };
            let row_class = if line.changing { " moving" } else { "" };
            format!(
                r#"<div class="hex-row{row_class}"><span class="hex-pos">{}</span><span class="hex-bar {class}">{segments}</span></div>"#,
                esc(&super::divine::line_title(index, line.yang)),
            )
        })
        .collect();

    let mut foot = String::new();
    let _ = write!(
        foot,
        r#"<div class="hex-item"><span class="hex-k">起卦</span><span>大衍筮法，四十九策三变成爻，得策 {}（初爻至上爻）</span></div>"#,
        esc(&cast.stalks_text())
    );
    if cast.changing.is_empty() {
        foot.push_str(
            r#"<div class="hex-item"><span class="hex-k">变爻</span><span>六爻皆静，无动</span></div>"#,
        );
    } else {
        let moving: Vec<String> = cast
            .changing
            .iter()
            .map(|index| {
                let title = super::divine::line_title(*index, cast.lines[*index].yang());
                format!("{title}（{}）", POSITION_SENSE[*index])
            })
            .collect();
        let _ = write!(
            foot,
            r#"<div class="hex-item"><span class="hex-k">变爻</span><span>{}</span></div>"#,
            esc(&moving.join("；"))
        );
    }
    if let Some(changed) = cast.changed {
        let _ = write!(
            foot,
            r#"<div class="hex-item"><span class="hex-k">之卦</span><span>{} —— {}</span></div>"#,
            esc(changed.full),
            esc(changed.judgment)
        );
    }
    let _ = write!(
        foot,
        r#"<div class="hex-item"><span class="hex-k">占法</span><span>{}</span></div>"#,
        esc(cast.rule())
    );

    format!(
        r#"<div class="sec">{head}<div class="hex"><div class="hex-figure">{rows}</div><div class="hex-body"><div class="hex-name">{name}</div><div class="hex-meta">第 {number} 卦<span class="sep">·</span>{trigrams}</div><div class="hex-judgment">{judgment}</div><div class="hex-sense">{sense}</div></div></div><div class="hex-foot">{foot}</div></div>"#,
        head = sec_head("卦象", "THE HEXAGRAM"),
        name = esc(cast.primary.full),
        number = cast.primary.number,
        trigrams = esc(&cast.primary.trigrams()),
        judgment = esc(cast.primary.judgment),
        sense = esc(cast.primary.sense),
    )
}

/// 总断。卦与人接上的那一段，给足分量。
fn verdict(text: &str) -> String {
    if text.trim().is_empty() {
        return String::new();
    }
    format!(
        r#"<div class="sec">{head}<div class="verdict">{text}</div></div>"#,
        head = sec_head("总断", "THE VERDICT"),
        text = esc(text)
    )
}

/// 详批。段落与引语同列一队，按序渲染——引语落在论证里，不贴到文末。
fn passages(persona: &Persona) -> String {
    let blocks: String = persona
        .live_passages()
        .map(|passage| {
            if passage.is_quote() {
                let note = if passage.note.trim().is_empty() {
                    String::new()
                } else {
                    format!(r#"<figcaption class="quote-note">{}</figcaption>"#, esc(&passage.note))
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
        head = sec_head("详批", "THE READING")
    )
}

/// 之变。到现在还卡着的地方，与要去的方向。
fn turn(text: &str) -> String {
    if text.trim().is_empty() {
        return String::new();
    }
    format!(
        r#"<div class="sec">{head}<div class="turn">{text}</div></div>"#,
        head = sec_head("之变", "THE TURNING"),
        text = esc(text)
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
        r#"<div class="foot"><div>统计区间 {range}<span class="sep">·</span>样本 {} 条<span class="sep">·</span>{model}</div><div class="foot-note">卦由本人在群里的发言起出，批语由模型写成，仅供娱乐，不作凭据</div></div>"#,
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
.eyebrow{display:flex;align-items:flex-end;justify-content:space-between;gap:16px;margin-bottom:24px}
.kicker{display:flex;align-items:center;gap:11px;font-size:15.5px;font-weight:800;
  letter-spacing:.18em;color:__ACCENT__;white-space:nowrap}
.dot{flex:none;width:8px;height:8px;border-radius:50%;background:__ACCENT__;
  box-shadow:0 0 0 5px rgba(__RGB__,var(--glow-alpha))}
.kicker-en{font-size:10px;font-weight:700;letter-spacing:.2em;color:var(--faint);
  line-height:1.4;max-width:190px}
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
.who-name{font-size:33px;line-height:1.28;font-weight:800;letter-spacing:-.015em;color:var(--title)}
.who-meta{margin-top:9px;font-size:15px;line-height:1.6;font-weight:500;color:var(--muted)}
.sep{margin:0 8px;color:var(--faint)}

/* —— 代号 —— */
.codename-block{margin-top:30px}
.label-row{display:flex;align-items:center;gap:12px}
.label{font-size:13px;font-weight:800;letter-spacing:.22em;color:var(--faint)}
.pill{padding:3px 10px;border-radius:8px;font-size:12.5px;font-weight:700;letter-spacing:.02em;
  color:__ACCENT__;background:rgba(__RGB__,var(--chip-alpha))}
.codename{margin-top:12px;font-family:var(--serif);font-size:46px;line-height:1.26;font-weight:700;
  letter-spacing:.01em;color:var(--title)}
.tagline{margin-top:12px;font-family:var(--serif);font-size:21px;line-height:1.66;font-weight:600;
  color:__ACCENT__}

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
.sec-mark{font-family:var(--serif);font-size:22px;font-weight:700;letter-spacing:.14em;
  color:var(--title);white-space:nowrap}
.sec-en{font-size:10.5px;font-weight:700;letter-spacing:.3em;color:var(--faint);white-space:nowrap}
.sec-head::after{content:"";flex:1;height:1px;background:var(--strong-line)}

/* —— 卦象 —— */
.hex{display:flex;gap:26px;padding:26px 26px 24px;border-radius:14px;
  background:var(--panel);border:1px solid var(--panel-border)}
.hex-figure{flex:none;width:104px;display:flex;flex-direction:column;justify-content:center;gap:7px}
.hex-row{display:flex;align-items:center;gap:8px}
.hex-pos{flex:none;width:28px;text-align:right;font-family:var(--serif);font-size:12px;
  letter-spacing:.02em;color:var(--faint);white-space:nowrap}
.hex-bar{flex:1;display:flex;gap:7px;height:11px}
.hex-bar i{flex:1;display:block;height:100%;border-radius:2px;background:var(--strong)}
.hex-bar.moving i{background:__ACCENT__;box-shadow:0 0 0 2px rgba(__RGB__,var(--glow-alpha))}
.hex-row.moving .hex-pos{font-weight:700;color:__ACCENT__}
.hex-body{flex:1;min-width:0}
.hex-name{font-family:var(--serif);font-size:32px;line-height:1.3;font-weight:700;
  letter-spacing:.06em;color:var(--title)}
.hex-meta{margin-top:8px;font-size:14.5px;font-weight:600;letter-spacing:.04em;color:var(--faint)}
.hex-judgment{margin-top:15px;font-family:var(--serif);font-size:19px;line-height:1.78;
  color:var(--strong)}
.hex-sense{margin-top:11px;font-size:17px;line-height:1.76;color:var(--subtle)}
.hex-foot{margin-top:20px;padding-top:16px;border-top:1px solid var(--line);
  display:flex;flex-direction:column;gap:8px}
.hex-item{display:flex;gap:12px;font-size:15px;line-height:1.68;color:var(--subtle)}
.hex-k{flex:none;width:38px;font-weight:800;letter-spacing:.08em;color:__ACCENT__}

/* —— 总断 —— */
.verdict{padding:24px 26px;border-radius:14px;background:var(--panel);
  border:1px solid var(--panel-border);font-family:var(--serif);font-size:20.5px;
  line-height:1.94;font-weight:500;color:var(--strong)}

/* —— 详批 —— */
.prose{margin-bottom:18px;font-family:var(--serif);font-size:20px;line-height:1.98;
  color:var(--body);text-indent:2em}
.prose:last-child{margin-bottom:0}
.quote{margin:22px 0;padding:20px 22px;border-radius:12px;background:rgba(__RGB__,var(--chip-alpha));
  border-left:3px solid rgba(__RGB__,.55)}
.quote-text{margin:0;font-family:var(--serif);font-size:20.5px;line-height:1.86;color:var(--strong);
  text-indent:0}
.quote-note{margin-top:11px;font-size:15px;line-height:1.62;color:var(--faint)}

/* —— 之变 —— */
.turn{padding:22px 24px;border-radius:14px;background:rgba(__RGB__,var(--chip-alpha));
  border:1px solid rgba(__RGB__,.22);font-family:var(--serif);font-size:19.5px;
  line-height:1.9;color:var(--strong)}

/* —— 赠言 —— */
.advice{margin-top:34px;padding:24px 26px;border-radius:14px;
  background:var(--panel);border:1px solid var(--panel-border)}
.advice-label{font-size:13px;font-weight:800;letter-spacing:.24em;color:var(--faint)}
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
    use crate::plugins::portrait::divine;
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

    fn cast(material: &Material) -> Cast {
        divine::cast(material)
    }

    fn persona() -> Persona {
        Persona {
            codename: "用忙碌挡空的人".into(),
            tagline: "他把休息也算成一件事".into(),
            verdict: "屯是开头难。他卡在开头已经很久，久到他自己都不再提。".into(),
            passages: vec![
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
            turn: "他不动的那一爻在最底下，是开头。".into(),
            advice: "少熬点夜。".into(),
            accent: "indigo".into(),
            estimated: false,
        }
    }

    /// 北京时间 10:13，用来让 `auto` 落在日读一侧。
    const MORNING: i64 = 1_700_014_400;
    /// 北京时间 00:13，用来让 `auto` 落在夜读一侧。
    const MIDNIGHT: i64 = 1_700_064_800;

    fn view_at<'a>(
        material: &'a Material,
        persona: &'a Persona,
        cast: &'a Cast,
        timestamp: i64,
    ) -> View<'a> {
        View {
            material,
            persona,
            cast,
            avatar: None,
            model: "deepseek/deepseek-flash",
            theme: "auto",
            offset: offset(),
            now: DateTime::from_timestamp(timestamp, 0)
                .unwrap()
                .with_timezone(&offset()),
        }
    }

    fn view<'a>(material: &'a Material, persona: &'a Persona, cast: &'a Cast) -> View<'a> {
        view_at(material, persona, cast, MORNING)
    }

    #[test]
    fn every_section_renders_with_its_content() {
        let material = material();
        let persona = persona();
        let cast = cast(&material);
        let html = html(&view(&material, &persona, &cast));
        for needle in [
            "易经画像",
            "用忙碌挡空的人",
            "他把休息也算成一件事",
            r#"<span class="sec-mark">卦象</span>"#,
            r#"<span class="sec-mark">总断</span>"#,
            r#"<span class="sec-mark">详批</span>"#,
            r#"<span class="sec-mark">之变</span>"#,
            "大衍筮法",
            "四十九策三变成爻",
            cast.primary.full,
            cast.primary.judgment,
            cast.primary.sense,
            cast.rule(),
            "他把每件事都当成一件要交的活。",
            "凌晨三点还在改代码，明天又要废了",
            "他自己知道在拿什么换",
            "屯是开头难",
            "少熬点夜。",
            "不作凭据",
            "1,234",
            "deepseek/deepseek-flash",
        ] {
            assert!(html.contains(needle), "缺少 {needle}");
        }
    }

    /// 卦象要画满六爻，动的那一爻单独上色。
    #[test]
    fn the_hexagram_is_drawn_line_by_line() {
        let material = material();
        let persona = persona();
        let cast = cast(&material);
        let html = html(&view(&material, &persona, &cast));
        assert_eq!(html.matches(r#"class="hex-row"#).count(), 6);
        // 阳爻一整划，阴爻断开：两种数量加起来就是六爻里的阴爻数×2 + 阳爻数。
        let yang = cast.drawn().iter().filter(|line| line.yang).count();
        assert_eq!(html.matches("<i></i>").count(), yang + (6 - yang) * 2);
        // 变爻都带上 moving（样式表里也有一处 .moving，所以只数爻上的）。
        let moving = cast.drawn().iter().filter(|line| line.changing).count();
        assert_eq!(
            html.matches(r#"class="hex-bar yang moving""#).count()
                + html.matches(r#"class="hex-bar yin moving""#).count(),
            moving
        );
        // 爻题从下往上写全。
        for index in 0..6 {
            let title = divine::line_title(index, cast.lines[index].yang());
            assert!(html.contains(&title), "缺少爻题 {title}");
        }
    }

    /// 有变爻时版面要写出之卦；没有变爻时不写。
    #[test]
    fn the_changed_hexagram_only_shows_when_there_is_one() {
        let material = material();
        let persona = persona();
        let mut seed = 0u64;
        let still = loop {
            let candidate = divine::cast_from(seed);
            if candidate.changing.is_empty() {
                break candidate;
            }
            seed += 1;
        };
        let still_html = html(&view(&material, &persona, &still));
        assert!(still_html.contains("六爻皆静，无动"));
        assert!(!still_html.contains(r#"<span class="hex-k">之卦</span>"#));

        let moved = loop {
            let candidate = divine::cast_from(seed);
            if !candidate.changing.is_empty() {
                break candidate;
            }
            seed += 1;
        };
        let changed = moved.changed.unwrap();
        let moved_html = html(&view(&material, &persona, &moved));
        assert!(moved_html.contains(r#"<span class="hex-k">之卦</span>"#));
        assert!(moved_html.contains(changed.full));
    }

    /// 昵称来自群聊，必须转义；模型给的文本同理。
    #[test]
    fn external_text_is_escaped() {
        let material = material();
        let persona = persona();
        let cast = cast(&material);
        let html = html(&view(&material, &persona, &cast));
        assert!(!html.contains("阿<甲>"));
        assert!(html.contains("阿&lt;甲&gt;"));
    }

    /// 有头像时嵌图，没有时退回名字首字，两种都不改版位。
    #[test]
    fn the_avatar_replaces_the_initial_when_available() {
        let material = material();
        let persona = persona();
        let cast = cast(&material);
        let base = view_at(&material, &persona, &cast, MORNING);

        let without = html(&base);
        assert!(without.contains(r#"<div class="avatar">阿</div>"#), "没有头像时用首字");

        let data = "data:image/jpeg;base64,AAAA";
        let with = html(&View {
            avatar: Some(data),
            ..view_at(&material, &persona, &cast, MORNING)
        });
        assert!(with.contains(&format!(r#"<div class="avatar"><img src="{data}" alt=""></div>"#)));
        assert!(!with.contains(r#"<div class="avatar">阿</div>"#));
    }

    #[test]
    fn estimated_reports_are_labelled() {
        let material = material();
        let cast = cast(&material);
        let persona = Persona {
            estimated: true,
            ..persona()
        };
        let html = html(&view(&material, &persona, &cast));
        assert!(html.contains("批语未落"));
    }

    #[test]
    fn empty_optional_sections_disappear() {
        let material = material();
        let cast = cast(&material);
        let persona = Persona {
            passages: Vec::new(),
            verdict: String::new(),
            turn: String::new(),
            advice: String::new(),
            ..persona()
        };
        let html = html(&view(&material, &persona, &cast));
        assert!(!html.contains(r#"<span class="sec-mark">总断</span>"#));
        assert!(!html.contains(r#"<span class="sec-mark">详批</span>"#));
        assert!(!html.contains(r#"<span class="sec-mark">之变</span>"#));
        assert!(!html.contains(r#"<div class="advice">"#));
        // 卦象与读数始终在：卦不来自模型，模型不接也有一卦。
        assert!(html.contains(r#"<span class="sec-mark">卦象</span>"#));
        assert!(html.contains(&cast.primary.full));
        assert!(html.contains("用忙碌挡空的人"));
        assert!(html.contains(r#"<div class="readings">"#));
    }

    /// 批语里空掉的那一段不占版面。
    #[test]
    fn blank_passages_are_not_printed() {
        let material = material();
        let cast = cast(&material);
        let persona = Persona {
            passages: vec![
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
        let html = html(&view(&material, &persona, &cast));
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
        let cast = cast(&material);
        let pinned = View {
            theme: "light",
            ..view_at(&material, &persona, &cast, MIDNIGHT)
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
        let cast = cast(&material);
        std::fs::write(
            format!("{dir}/portrait-light.html"),
            html(&view_at(&material, &persona, &cast, MORNING)),
        )
        .unwrap();
        std::fs::write(
            format!("{dir}/portrait-dark.html"),
            html(&view_at(&material, &persona, &cast, MIDNIGHT)),
        )
        .unwrap();
        let fallback = Persona::from_stats(&material, &cast);
        std::fs::write(
            format!("{dir}/portrait-fallback.html"),
            html(&view_at(&material, &fallback, &cast, MORNING)),
        )
        .unwrap();
    }

    #[tokio::test]
    #[ignore = "需要本地 Chrome/Chromium"]
    async fn captures_complete_cards() {
        let material = material();
        let persona = persona();
        let cast = cast(&material);
        let fallback = Persona::from_stats(&material, &cast);
        let cases = [
            ("light", view_at(&material, &persona, &cast, MORNING)),
            ("dark", view_at(&material, &persona, &cast, MIDNIGHT)),
            ("fallback", view_at(&material, &fallback, &cast, MORNING)),
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
