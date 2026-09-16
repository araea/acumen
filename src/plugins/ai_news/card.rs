//! 把资讯排版成一张卡片图（HTML → 截图）。
//!
//! 设计取向：随北京时间自动切换「日读 / 夜读」，也可在配置中固定主题。
//! 两套主题共用字号、间距与信息层级，只替换明度和主色，避免切换后像两套产品。
//! 四类内容各有一个主色——速递靛蓝、热点橙、日报薄荷绿、模型榜琥珀金，
//! 主色只出现在序号、标签、顶部微光与引用左界这几处。
//!
//! 排版参数是按「群聊里被缩略图裹一层再点开看」这个真实场景定的：
//!   - 版心 720 CSS px 配合 `image_scale`（默认 3 倍）出图，2160px 宽，放大不糊；
//!   - 标题 26px、正文 18.5px——相对版心足够大，缩略图状态下也能读出标题；
//!   - 夜间文字采用偏暖的中性灰，标题、正文、元信息保持稳定的三级明度，
//!     一眼扫过先看到标题与数字，细节再往下沉；
//!   - 一条一格，格与格之间用 26px 上下留白而非重分割线。
//!
//! 图片只承载「读」的部分：链接一概不画进图里。需要查看全文时，用户引用
//! 卡片后直接回复序号，再按需取得正文与链接。

use super::api::{DailyBlock, DailyReport, HotTopic, Item, category_label};
use super::leaderboard::{Board, Trend};
use super::render::{RenderOptions, fmt_time, truncate};
use anyhow::Result;
use chrono::{DateTime, Timelike, Utc};

/// 卡片主色：四类内容各一个种子。
///
/// 色值本身不在这里——`res/cards/m3e.css` 的 `--md-seed-*` 里写着日读与夜读
/// 各一份，这个类型只带一个类名过去。从前是「浅色一份十六进制 + 一份 `r,g,b`
/// 字面量、深色再来一份」，于是透明度要另立变量、容器色与淡色叠层没法自动跟着
/// 主色走，换一次主色得在四处对同样的值。现在透明度一律 `color-mix()` 现算。
#[derive(Clone, Copy)]
pub struct Accent(&'static str);

const BRIEF: Accent = Accent("seed-brief");
const HOT: Accent = Accent("seed-hot");
const DAILY: Accent = Accent("seed-daily");
const MODELS: Accent = Accent("seed-models");

/// 最终用于截图的主题。`auto` 在 07:00—18:59 使用日读，其余时间使用夜读。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardTheme {
    Light,
    Dark,
}

impl CardTheme {
    /// 深色主题在页面上落成 `dark` 这个类名，配色方案在 `m3e.css` 里按它分档。
    fn class(self) -> &'static str {
        match self {
            Self::Light => "",
            Self::Dark => " dark",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Light => "白天",
            Self::Dark => "夜晚",
        }
    }
}

/// 解析主题配置。未知值安全回退到自动，避免手改配置导致渲染失败。
pub fn resolve_theme(mode: &str) -> CardTheme {
    resolve_theme_at(mode, Utc::now().with_timezone(&super::render::beijing()))
}

fn resolve_theme_at(mode: &str, now: DateTime<chrono::FixedOffset>) -> CardTheme {
    match mode.trim().to_ascii_lowercase().as_str() {
        "light" | "day" | "白天" | "日间" => CardTheme::Light,
        "dark" | "night" | "夜晚" | "夜间" => CardTheme::Dark,
        _ if (7..19).contains(&now.hour()) => CardTheme::Light,
        _ => CardTheme::Dark,
    }
}

/// 卡片渲染宽度（CSS 像素）。实际出图宽度 = WIDTH × `image_scale`（默认 3 倍 → 2160px）
const WIDTH: u32 = 720;

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

/// 出图时刻（北京时间），放在页眉右上角
fn stamp() -> String {
    crate::render::beijing_now().format("%Y-%m-%d %H:%M").to_string()
}

/// 资讯卡的版式。
///
/// 令牌与组件基元在 `res/cards/m3e.css`（`crate::render::web::DESIGN_SYSTEM`），
/// 这里只写这张卡自己的位置，**不写色值与字号字面量**。四类内容的主色由页面的
/// `seed-*` 类名换，这个文件不参与——所以「换分类」与「换版式」是两件互不牵连的事。
const CSS: &str = r#"
*{margin:0;padding:0;box-sizing:border-box}
body{width:720px}
.shot{padding:var(--md-space-5)}
.card{padding:var(--md-space-9) 40px var(--md-space-8)}

/* —— 标题块 —— */
.title{margin-top:0}
.rule{margin:var(--md-space-7) 0 2px}

/* —— 条目 —— */
/* 一条一格，格与格之间靠留白与一条细线分开，不靠背景块——资讯卡是一条条
   读下去的，每格都上底色会把整页切成一片瓦。 */
.row{display:grid;grid-template-columns:50px minmax(0,1fr);gap:17px;
  padding:27px 0;border-bottom:1px solid var(--md-sys-color-outline-variant)}
.row:last-child{border-bottom:none;padding-bottom:var(--md-space-2)}
/* 序号：比标题还大一档，只上主色不加底——它是一条的入口，不是内容 */
.idx{font-size:var(--md-type-headline-small-size);line-height:1.4;font-weight:800;
  color:var(--md-sys-color-primary);font-variant-numeric:tabular-nums;
  text-align:right;letter-spacing:-.02em}
/* 名次牌：前三名填实，其余仅描边。靠「有没有底」分梯队，不靠换个颜色 */
.rank{display:flex;align-items:center;justify-content:center;width:42px;height:42px;
  margin-left:auto;border-radius:var(--md-shape-m);font-size:var(--md-type-title-small-size);
  font-weight:800;font-variant-numeric:tabular-nums;
  color:var(--md-sys-color-on-primary-container);
  background:var(--md-sys-color-primary-container);
  border:1px solid var(--md-sys-color-primary-line)}
.rank.top{color:var(--md-sys-color-on-primary);background:var(--md-sys-color-primary);
  border-color:transparent}

.h{font-size:var(--md-type-headline-small-size);line-height:1.5;font-weight:700;
  color:var(--md-sys-color-on-surface);text-wrap:pretty}
.meta{margin-top:var(--md-space-3);display:flex;flex-wrap:wrap;align-items:center;
  gap:10px;font-size:var(--md-type-body-small-size);font-weight:500;
  color:var(--md-sys-color-on-surface-variant)}
.sum{margin-top:var(--md-space-3);font-size:var(--md-type-body-medium-size);
  line-height:1.76;color:var(--md-sys-color-on-surface-variant);text-wrap:pretty}
/* 推荐理由用引语块：主色淡底 + 左界 + 收一个角（M3 的角形处理） */
.why{margin-top:var(--md-space-3);padding:var(--md-space-3) var(--md-space-4);
  border-left:4px solid var(--md-sys-color-primary);
  border-radius:0 var(--md-shape-m) var(--md-shape-m) 0;
  background:var(--md-sys-color-primary-tint);
  font-size:var(--md-type-body-medium-size);line-height:1.7;
  color:var(--md-sys-color-on-surface-variant);text-wrap:pretty}
.why b{color:var(--md-sys-color-primary);font-weight:800;letter-spacing:.02em}

/* —— 日报 —— */
.lead{margin:var(--md-space-7) 0 var(--md-space-1);padding:var(--md-space-5) 22px;
  border-radius:var(--md-shape-l);background:var(--md-sys-color-surface-container-low);
  border:1px solid var(--md-sys-color-outline-variant);
  font-size:var(--md-type-body-large-size);line-height:1.78;
  color:var(--md-sys-color-on-surface-variant)}
.sec{padding:27px 0 7px;border-bottom:1px solid var(--md-sys-color-outline-variant)}
.sec:last-of-type{border-bottom:none}
.sec-h{display:flex;align-items:center;gap:var(--md-space-3);
  font-size:var(--md-type-title-large-size);font-weight:800;
  color:var(--md-sys-color-on-surface)}
.bar{width:5px;height:22px;border-radius:3px;background:var(--md-sys-color-primary)}
.li{margin-top:17px;padding-left:var(--md-space-5);position:relative;
  font-size:var(--md-type-body-medium-size);line-height:1.68;
  color:var(--md-sys-color-on-surface-variant);text-wrap:pretty}
.li::before{content:"";position:absolute;left:2px;top:12px;width:7px;height:7px;
  border-radius:var(--md-shape-full);background:var(--md-sys-color-primary)}
.li b{color:var(--md-sys-color-on-surface);font-weight:700}
.li .t{margin-top:6px;font-size:var(--md-type-body-small-size);line-height:1.72;
  color:var(--md-sys-color-on-surface-faint)}

/* —— 模型榜 —— */
/* 行内三列都从顶端对齐：名次方块与分数跟标题的第一行齐平，而不是各自在行高里
   垂直居中——居中会让名次掉到来源那一行去，读起来像标错了对象。 */
.mrow{display:grid;grid-template-columns:50px minmax(0,1fr) 142px;gap:17px;
  align-items:start;padding:23px 0;
  border-bottom:1px solid var(--md-sys-color-outline-variant)}
.mrow:last-of-type{border-bottom:none}
.mname{display:flex;align-items:baseline;flex-wrap:wrap;gap:10px;
  font-size:var(--md-type-headline-small-size);line-height:1.4;font-weight:700;
  color:var(--md-sys-color-on-surface)}
.trend{font-size:var(--md-type-label-large-size);font-weight:800;letter-spacing:.02em;
  font-variant-numeric:tabular-nums;color:var(--md-sys-color-on-surface-faint)}
.trend.up{color:var(--md-sys-color-success)}
.trend.down{color:var(--md-sys-color-error)}
.trend.new{color:var(--md-sys-color-primary)}
.meter{margin-top:var(--md-space-3);height:7px;border-radius:var(--md-shape-full);
  background:var(--md-sys-color-surface-container-high);overflow:hidden}
/* 分数条：浅头深尾的一条主色渐变。长度即分数，不做二次拉伸。 */
.meter i{display:block;height:100%;border-radius:var(--md-shape-full);
  background:linear-gradient(90deg,var(--md-sys-color-primary-line),var(--md-sys-color-primary))}
.mscore{text-align:right}
/* 共识分是这张图唯一要「一眼看到」的数字，给到 headline-medium，其余都压在灰阶里 */
.mscore strong{display:block;font-size:var(--md-type-headline-medium-size);line-height:1;
  font-weight:800;letter-spacing:-.02em;color:var(--md-sys-color-primary);
  font-variant-numeric:tabular-nums}
.mscore small{display:block;margin-top:6px;font-size:var(--md-type-label-medium-size);
  line-height:1.4;color:var(--md-sys-color-on-surface-faint);white-space:nowrap}
.note{margin-top:22px;font-size:var(--md-type-body-small-size);line-height:1.72;
  color:var(--md-sys-color-on-surface-variant)}

/* —— 页脚 —— */
.foot>div:last-child{text-align:right}
.foot .src{display:flex;align-items:center;gap:10px;white-space:nowrap}
/* 出处前的一串三级点：主色一颗，往后两档渐弱，是「这张图有出处」的记认 */
.mark{width:5px;height:5px;margin-right:18px;flex:0 0 5px;border-radius:50%;
  background:color-mix(in srgb,var(--md-sys-color-primary) 70%,transparent);
  box-shadow:9px 0 0 var(--md-sys-color-outline),18px 0 0 var(--md-sys-color-outline-variant)}
"#;

/// 套上统一的卡片外壳：页眉（主色标签 + 出图时间）、大标题、正文、页脚。
///
/// `kicker` 是页眉左边那两行字——`(中文, 英文简写)`。中文说「这是什么」，
/// 英文是眉标的固定装饰，六张卡片都这么配（手册卡用的是品牌名 AYJX）。
/// `foot_note` 是页脚右侧的操作提示。
fn shell(
    accent: Accent,
    theme: CardTheme,
    kicker: (&str, &str),
    title: &str,
    subtitle: &str,
    body: &str,
    foot_note: &str,
) -> String {
    let css = format!("{}{}", crate::render::web::DESIGN_SYSTEM, CSS);
    let subtitle = if subtitle.is_empty() {
        String::new()
    } else {
        format!(
            r#"<div class="subtitle md-subtitle md-type-title-small">{}</div>"#,
            esc(subtitle)
        )
    };

    format!(
        r#"<!DOCTYPE html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:"><style>{css}</style></head>
<body class="scheme-news md-text {seed}{theme_class}"><div class="shot"><div class="card md-card">
<div class="md-eyebrow"><div class="md-kicker"><span class="md-dot"></span>{kicker}<span class="md-kicker-en">{kicker_en}</span></div><span class="md-stamp">{stamp}</span></div>
<div class="title md-title md-type-display-small md-balance">{title}</div>{subtitle}
<hr class="rule md-rule">
{body}
<div class="foot md-foot md-foot-row"><div class="src"><span class="mark"></span>AIHOT · aihot.virxact.com</div><div>{foot_note}</div></div>
</div></div></body></html>"#,
        css = css,
        seed = accent.0,
        theme_class = theme.class(),
        kicker = esc(kicker.0),
        kicker_en = esc(kicker.1),
        stamp = stamp(),
        title = esc(title),
        subtitle = subtitle,
        body = body,
        foot_note = esc(foot_note),
    )
}

/// 一级推送只发图；用户引用图片后直接回复序号提取链接（0 表示全部）。
const FOOT_LINKS: &str = "引用本图回复 0全部｜序号 取链接";

/// 一条资讯的元信息行：来源 · 分类 · 时间
fn meta_html(item: &Item) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(name) = item
        .source
        .as_ref()
        .and_then(|s| s.name.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(format!(r#"<span>{}</span>"#, esc(name)));
    }
    if let Some(cat) = item.category.as_deref().filter(|c| !c.trim().is_empty()) {
        parts.push(format!(r#"<span class="md-chip">{}</span>"#, esc(category_label(cat))));
    }
    // 拿不到原文发布时间时退回收录时间，并如实标注，不冒充发布时间
    let time = item
        .published_at
        .as_deref()
        .and_then(fmt_time)
        .or_else(|| {
            item.discovered_at
                .as_deref()
                .and_then(fmt_time)
                .map(|t| format!("{} 收录", t))
        });
    if let Some(t) = time {
        parts.push(format!(r#"<span>{}</span>"#, esc(&t)));
    }

    if parts.is_empty() {
        return String::new();
    }
    format!(
        r#"<div class="meta">{}</div>"#,
        parts.join(r#"<span class="md-sep">·</span>"#)
    )
}

/// 资讯列表卡片（速递 / 搜索结果共用）
pub fn items_card(
    title: &str,
    subtitle: &str,
    items: &[Item],
    opts: &RenderOptions,
    theme: CardTheme,
) -> String {
    let mut body = String::new();

    for (idx, item) in items.iter().enumerate() {
        body.push_str(&format!(
            r#"<div class="row"><div class="idx">{:02}</div><div>"#,
            idx + 1
        ));
        body.push_str(&format!(
            r#"<div class="h">{}</div>"#,
            esc(item.title.as_deref().unwrap_or("(无标题)").trim())
        ));
        body.push_str(&meta_html(item));

        if let Some(summary) = item.summary.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            body.push_str(&format!(
                r#"<div class="sum">{}</div>"#,
                esc(&truncate(summary, 100))
            ));
        }
        if opts.show_reason
            && let Some(reason) = item.reason.as_deref().map(str::trim).filter(|s| !s.is_empty())
        {
            // 给「推荐理由」一个主色小标签：这是全卡最该被先看见的一句
            body.push_str(&format!(
                r#"<div class="why"><b>推荐理由</b> {}</div>"#,
                esc(&truncate(reason, 80))
            ));
        }
        body.push_str("</div></div>");
    }

    let subtitle = match subtitle.is_empty() {
        true => format!("共 {} 条", items.len()),
        false => format!("{} · 共 {} 条", subtitle, items.len()),
    };
    shell(BRIEF, theme, ("AI 资讯", "AI NEWS"), title, &subtitle, &body, FOOT_LINKS)
}

/// 热点榜卡片：前三名用实心序号牌，其余描边，一眼看出梯队
pub fn hot_topics_card(topics: &[HotTopic], theme: CardTheme) -> String {
    let mut body = String::new();

    for (idx, topic) in topics.iter().enumerate() {
        let rank = topic.rank.unwrap_or((idx + 1) as u32);
        let cls = if rank <= 3 { "rank top" } else { "rank" };

        body.push_str(&format!(
            r#"<div class="row"><div class="idx"><div class="{}">{}</div></div><div>"#,
            cls, rank
        ));
        body.push_str(&format!(
            r#"<div class="h">{}</div>"#,
            esc(topic.title.as_deref().unwrap_or("(无标题)").trim())
        ));

        let mut meta: Vec<String> = Vec::new();
        if let Some(count) = topic.source_count.filter(|c| *c > 0) {
            meta.push(format!(r#"<span class="md-chip">{} 个信源</span>"#, count));
        }
        let names: Vec<String> = topic
            .source_names
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .take(3)
            .map(|s| format!(r#"<span class="md-chip md-chip-plain">{}</span>"#, esc(s)))
            .collect();
        meta.extend(names);
        if let Some(t) = topic.latest_at.as_deref().and_then(fmt_time) {
            meta.push(format!(r#"<span>最新 {}</span>"#, esc(&t)));
        }
        if !meta.is_empty() {
            body.push_str(&format!(r#"<div class="meta">{}</div>"#, meta.join("")));
        }

        if let Some(summary) = topic.summary.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            body.push_str(&format!(
                r#"<div class="sum">{}</div>"#,
                esc(&truncate(summary, 100))
            ));
        }
        body.push_str("</div></div>");
    }

    let subtitle = format!("跨信源聚合 · TOP {}", topics.len());
    shell(
        HOT,
        theme,
        ("热点榜", "AI HOTLIST"),
        "当前热点榜",
        &subtitle,
        &body,
        FOOT_LINKS,
    )
}

/// 模型榜卡片：一行一个模型，左名次、中模型与价格、右共识分。
///
/// 共识分是这张图唯一需要「一眼看到」的数字，所以给到 34px 主色 + 一条同色进度条；
/// 其余信息（厂商、上线日期、价格）压到 15px 的灰阶里，不与之争。
pub fn models_card(board: &Board, max_items: usize, theme: CardTheme) -> String {
    let shown = &board.entries[..board.entries.len().min(max_items.max(1))];
    let mut body = String::new();

    for (idx, model) in shown.iter().enumerate() {
        let rank = model.rank.unwrap_or((idx + 1) as u32);
        let rank_cls = if rank <= 3 { "rank top" } else { "rank" };

        body.push_str(&format!(
            r#"<div class="mrow"><div><div class="{}">{}</div></div><div>"#,
            rank_cls, rank
        ));

        let trend = model.trend();
        let trend_html = match trend {
            Trend::Flat => String::new(),
            Trend::Up(_) => format!(r#"<span class="trend up">{}</span>"#, esc(&trend.marker())),
            Trend::Down(_) => format!(r#"<span class="trend down">{}</span>"#, esc(&trend.marker())),
            Trend::New => format!(r#"<span class="trend new">{}</span>"#, esc(&trend.marker())),
        };
        body.push_str(&format!(
            r#"<div class="mname">{}{}</div>"#,
            esc(model.display_name()),
            trend_html
        ));

        let mut meta: Vec<String> = Vec::new();
        if let Some(provider) = model.provider_name() {
            meta.push(format!(r#"<span class="md-chip">{}</span>"#, esc(provider)));
        }
        if let Some(date) = model.released_date() {
            meta.push(format!(r#"<span>上线 {}</span>"#, esc(date)));
        }
        if let Some(ctx_len) = model.context_text() {
            meta.push(format!(r#"<span>上下文 {}</span>"#, esc(&ctx_len)));
        }
        if let Some(price) = model.price_text() {
            meta.push(format!(r#"<span>{}</span>"#, esc(&price)));
        }
        if !meta.is_empty() {
            body.push_str(&format!(r#"<div class="meta">{}</div>"#, meta.join("")));
        }

        // 分数条按 0—100 直接映射，长度即分数，不做二次拉伸
        if let Some(score) = model.score {
            body.push_str(&format!(
                r#"<div class="meter"><i style="width:{:.1}%"></i></div>"#,
                score.clamp(0.0, 100.0)
            ));
        }
        body.push_str("</div>");

        // 完整度与可信度各占一行：右栏窄，挤在一行会在「可信度」和「高」之间折行
        let mut note = String::new();
        if let Some(coverage) = model.coverage_text() {
            note.push_str(&format!("<small>完整度 {}</small>", esc(&coverage)));
        }
        if let Some(level) = model.confidence_label() {
            note.push_str(&format!("<small>可信度 {}</small>", level));
        }
        body.push_str(&format!(
            r#"<div class="mscore"><strong>{}</strong>{}</div></div>"#,
            esc(&model.score_text().unwrap_or_else(|| "—".to_string())),
            note
        ));
    }

    body.push_str(
        r#"<div class="note">共识分由多家公开评测榜单统一折算，只反映公开评测的汇总结果；价格为厂商官网参考价，人民币／百万 Token。</div>"#,
    );

    let mut subtitle: Vec<String> = Vec::new();
    if let Some(count) = board.source_count {
        subtitle.push(format!("综合 {} 家公开榜单", count));
    }
    if let Some(updated) = board.updated_at.as_deref().filter(|s| !s.is_empty()) {
        subtitle.push(format!("更新于 {}", updated));
    }
    subtitle.push(format!("TOP {}", shown.len()));

    shell(
        MODELS,
        theme,
        ("模型榜", "AI MODEL CONSENSUS"),
        "AIHOT 大模型排行榜",
        &subtitle.join(" · "),
        &body,
        FOOT_LINKS,
    )
}

/// 日报里的一个条目（含其子条目），扁平成带圆点的列表行
fn daily_items(out: &mut String, block: &DailyBlock, budget: &mut usize) {
    if *budget == 0 {
        return;
    }
    let title = block.title.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let text = block.text.as_deref().map(str::trim).filter(|s| !s.is_empty());

    if title.is_some() || text.is_some() {
        out.push_str(r#"<div class="li">"#);
        if let Some(t) = title {
            out.push_str(&format!("<b>{}</b>", esc(t)));
        }
        if let Some(t) = text {
            let cls = if title.is_some() { r#" class="t""# } else { "" };
            out.push_str(&format!("<div{}>{}</div>", cls, esc(&truncate(t, 150))));
        }
        out.push_str("</div>");
        *budget -= 1;
    }

    for child in &block.children {
        daily_items(out, child, budget);
    }
}

/// AI 日报卡片：保留 lead / sections / flashes 的分栏结构
pub fn daily_card(report: &DailyReport, max_blocks: usize, theme: CardTheme) -> String {
    let mut body = String::new();

    if let Some(lead) = report.lead.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        body.push_str(&format!(
            r#"<div class="lead">{}</div>"#,
            esc(&truncate(lead, 260))
        ));
    }

    let mut budget = max_blocks.max(1);
    for section in &report.sections {
        if budget == 0 {
            break;
        }
        let mut items = String::new();
        for child in &section.children {
            daily_items(&mut items, child, &mut budget);
        }
        // 没有子条目的段落，把自身正文当作内容
        if items.is_empty() {
            daily_items(&mut items, section, &mut budget);
        }
        if items.is_empty() {
            continue;
        }

        body.push_str(r#"<div class="sec">"#);
        if let Some(t) = section.title.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            body.push_str(&format!(
                r#"<div class="sec-h"><span class="bar"></span>{}</div>"#,
                esc(t)
            ));
        }
        body.push_str(&items);
        body.push_str("</div>");
    }

    if budget > 0 && !report.flashes.is_empty() {
        let mut items = String::new();
        for flash in &report.flashes {
            daily_items(&mut items, flash, &mut budget);
        }
        if !items.is_empty() {
            body.push_str(
                r#"<div class="sec"><div class="sec-h"><span class="bar"></span>快讯</div>"#,
            );
            body.push_str(&items);
            body.push_str("</div>");
        }
    }

    let title = match report.title.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(t) => t.to_string(),
        None => "AI 日报".to_string(),
    };
    let subtitle = report.date.as_deref().unwrap_or_default();

    shell(DAILY, theme, ("AI 日报", "AI DAILY"), &title, subtitle, &body, FOOT_LINKS)
}

/// 把卡片 HTML 截成图，返回 base64（JPEG）。
///
/// `scale` 是设备像素比：版心宽度不变、出图分辨率翻倍，字形边缘更实，
/// 群里放大看不至于发虚。取值过大只会把图撑肥，限制在 1—4 倍。
///
/// 量高度、等字体与尺寸护栏都在 [`crate::render::web::shoot`] 里；这条路径曾经
/// 自己写「先量高度再重设视口、中间睡两觉」，改走同一处之后少了两趟视口往返与
/// 230 ms 固定等待，也顺带接上了那一道并发闸门（此前没有）。
pub async fn capture(html: &str, scale: f64) -> Result<String> {
    crate::render::web::shoot(
        crate::render::web::Shot::new(html, WIDTH)
            .scale(scale)
            .jpeg(92)
            .max_height(12_000.0),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 版式里不许出现 HTML 的成对标签——样式表塞进 `style` 元素时会被截断，
    /// 而页面不会报错（见 [`crate::render::web::assert_embeddable`]）。
    #[test]
    fn stylesheet_stays_embeddable() {
        crate::render::web::assert_embeddable("ai_news", CSS);
    }

    /// 把三种卡片的 HTML 落到 `AI_NEWS_CARD_DUMP` 指定的目录，方便肉眼校版：
    ///   AI_NEWS_CARD_DUMP=/tmp/cards cargo test card::tests::dump -- --ignored
    #[test]
    #[ignore = "仅用于人工核对排版"]
    fn dump_sample_cards() {
        use crate::plugins::ai_news::api::{DailyBlock, Links, Source};

        let Ok(dir) = std::env::var("AI_NEWS_CARD_DUMP") else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();

        let opts = RenderOptions {
            summary_max_chars: 90,
            show_reason: true,
            show_original_link: false,
        };
        let titles = [
            ("Anthropic 发布 Claude 新一代模型，长上下文推理能力显著提升", "Anthropic 官方博客", "ai-models"),
            ("OpenAI 开放 Realtime API 正式版，延迟降至 300ms 以内", "OpenAI Blog", "ai-products"),
            ("研究显示大模型在多步数学推理中仍依赖模式匹配而非符号演算", "arXiv", "paper"),
            ("英伟达下季度数据中心营收指引超预期，AI 芯片需求未见放缓", "路透社", "industry"),
            ("实践总结：用结构化输出把 LLM 接入既有业务系统的六个要点", "少数派", "tip"),
        ];
        let items: Vec<Item> = titles
            .iter()
            .map(|(t, src, cat)| Item {
                id: Some((*t).into()),
                title: Some((*t).into()),
                summary: Some("模型在数学、代码与长文档理解等基准上取得明显提升，官方同时公布了新的定价方案与迁移指南，开发者可即刻通过 API 调用。".into()),
                reason: Some("发布节奏与竞品形成直接对位，对下游应用的选型有实际影响。".into()),
                source: Some(Source { name: Some((*src).into()) }),
                links: Links { aihot: Some("https://aihot.virxact.com/i/1".into()), ..Default::default() },
                published_at: Some("2026-08-21T01:20:00Z".into()),
                discovered_at: None,
                category: Some((*cat).into()),
            })
            .collect();

        let topics: Vec<HotTopic> = titles
            .iter()
            .enumerate()
            .map(|(i, (t, src, _))| HotTopic {
                rank: Some(i as u32 + 1),
                title: Some((*t).into()),
                summary: Some("多家媒体在同一时间窗内跟进报道，讨论集中在能力边界与落地成本两方面。".into()),
                source_count: Some(12 - i as u32),
                source_names: vec![(*src).into(), "机器之心".into(), "量子位".into()],
                latest_at: Some("2026-08-21T03:00:00Z".into()),
                links: Links::default(),
            })
            .collect();

        let leaf = |t: &str, s: &str| DailyBlock {
            title: Some(t.into()),
            text: Some(s.into()),
            url: None,
            children: vec![],
        };
        let report = DailyReport {
            date: Some("2026-08-21".into()),
            title: Some("模型更新密集，推理成本继续下探".into()),
            lead: Some("今日焦点集中在两处：头部厂商的模型迭代节奏进一步压缩，以及推理侧价格在一周内出现第二轮下调。".into()),
            links: Links::default(),
            sections: vec![
                DailyBlock {
                    title: Some("模型与研究".into()),
                    text: None,
                    url: None,
                    children: vec![
                        leaf(titles[0].0, "长上下文与工具调用是本次迭代的重点。"),
                        leaf(titles[2].0, "作者用一组对照实验区分了记忆与推理的贡献。"),
                    ],
                },
                DailyBlock {
                    title: Some("产品与行业".into()),
                    text: None,
                    url: None,
                    children: vec![
                        leaf(titles[1].0, "语音场景的端到端延迟首次进入可用区间。"),
                        leaf(titles[3].0, "指引隐含的产能假设值得关注。"),
                    ],
                },
            ],
            flashes: vec![leaf("多家云厂商同步下调推理单价", "降幅集中在 15%—30% 区间。")],
        };

        let models = [
            ("Claude Fable 5", "Anthropic", 89.4, 0.88, "HIGH", Some(1), 67.206, 336.03),
            ("Claude Opus 5", "Anthropic", 86.2, 0.845, "HIGH", Some(0), 33.603, 168.015),
            ("GPT-5.6 Sol", "OpenAI", 83.3, 0.845, "HIGH", Some(-1), 33.603, 201.62),
            ("Kimi K3", "Moonshot AI", 79.9, 0.845, "MEDIUM", None, 8.0, 32.0),
            ("GLM-5.3", "Z.ai", 76.9, 0.60, "LOW", Some(2), 4.0, 12.0),
        ];
        let board = Board {
            updated_at: Some("8月21日 20:00".into()),
            source_count: Some(7),
            entries: models
                .iter()
                .enumerate()
                .map(|(i, (name, provider, score, coverage, confidence, change, input, output))| {
                    crate::plugins::ai_news::leaderboard::ModelEntry {
                        rank: Some(i as u32 + 1),
                        previous_rank: change.map(|_| i as u32 + 1),
                        rank_change: *change,
                        name: Some((*name).into()),
                        provider: Some((*provider).into()),
                        released_at: Some("2026-06-09T00:00:00.000Z".into()),
                        context_window_tokens: Some(1_000_000),
                        input_price_per_million_cny: Some(*input),
                        output_price_per_million_cny: Some(*output),
                        score: Some(*score),
                        coverage: Some(*coverage),
                        confidence: Some((*confidence).into()),
                        ..Default::default()
                    }
                })
                .collect(),
        };

        for (name, html) in [
            (
                "brief-light.html",
                items_card(
                    "AI 资讯速递",
                    "过去 24 小时",
                    &items,
                    &opts,
                    CardTheme::Light,
                ),
            ),
            (
                "brief-dark.html",
                items_card(
                    "AI 资讯速递",
                    "过去 24 小时",
                    &items,
                    &opts,
                    CardTheme::Dark,
                ),
            ),
            ("hot.html", hot_topics_card(&topics, CardTheme::Dark)),
            ("daily.html", daily_card(&report, 12, CardTheme::Light)),
            ("models.html", models_card(&board, 12, CardTheme::Dark)),
        ] {
            std::fs::write(format!("{}/{}", dir, name), html).unwrap();
        }
    }

    /// 端到端跑一次截图，确认 HTML 能被无头浏览器吃下并出图：
    ///   AI_NEWS_CARD_DUMP=/tmp/cards cargo test card::tests::captures -- --ignored
    #[tokio::test]
    #[ignore = "需要可用的无头浏览器"]
    async fn captures_a_card_to_jpeg() {
        let Ok(dir) = std::env::var("AI_NEWS_CARD_DUMP") else {
            return;
        };
        dump_sample_cards();
        let html = std::fs::read_to_string(format!("{}/brief-light.html", dir)).unwrap();

        let b64 = capture(&html, 3.0).await.expect("截图应当成功");
        assert!(!b64.is_empty());

        use base64::{Engine, engine::general_purpose::STANDARD};
        let bytes = STANDARD.decode(&b64).expect("截图应是合法 base64");
        std::fs::write(format!("{}/captured.jpg", dir), &bytes).unwrap();
        println!("出图 {} 字节", bytes.len());
        cdp_html_shot::Browser::shutdown_global().await;
    }

    #[test]
    fn escapes_html_in_external_content() {
        let html = esc(r#"<script>alert("x")</script>"#);
        assert!(!html.contains('<'));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn items_card_embeds_titles_and_drops_links() {
        use crate::plugins::ai_news::api::{Links, Source};
        let item = Item {
            id: Some("1".into()),
            title: Some("某模型发布".into()),
            summary: Some("摘要".into()),
            reason: None,
            source: Some(Source {
                name: Some("官方博客".into()),
            }),
            links: Links {
                aihot: Some("https://aihot.virxact.com/i/1".into()),
                ..Default::default()
            },
            published_at: Some("2026-08-21T01:00:00Z".into()),
            discovered_at: None,
            category: Some("ai-models".into()),
        };
        let opts = RenderOptions {
            summary_max_chars: 100,
            show_reason: true,
            show_original_link: false,
        };
        let html = items_card("过去 24 小时", "", &[item], &opts, CardTheme::Dark);

        assert!(html.contains("某模型发布"));
        assert!(html.contains("官方博客"));
        assert!(html.contains("08-21 09:00"), "时间应换算为北京时间");
        assert!(html.contains("0全部") && html.contains("取链接"));
        // 链接只走文本消息，不画进图里
        assert!(!html.contains("aihot.virxact.com/i/1"));
    }

    #[test]
    fn auto_theme_follows_beijing_reading_hours() {
        use chrono::TimeZone;

        let tz = super::super::render::beijing();
        let morning = tz.with_ymd_and_hms(2026, 9, 5, 7, 0, 0).unwrap();
        let evening = tz.with_ymd_and_hms(2026, 9, 5, 19, 0, 0).unwrap();

        assert_eq!(resolve_theme_at("auto", morning), CardTheme::Light);
        assert_eq!(resolve_theme_at("auto", evening), CardTheme::Dark);
        assert_eq!(resolve_theme_at("light", evening), CardTheme::Light);
        assert_eq!(resolve_theme_at("dark", morning), CardTheme::Dark);
        assert_eq!(resolve_theme_at("unexpected", morning), CardTheme::Light);
    }
}
