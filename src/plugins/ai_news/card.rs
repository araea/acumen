//! 把资讯排版成一张卡片图（HTML → 截图）。
//!
//! 设计取向：随北京时间自动切换「日读 / 夜读」，也可在配置中固定主题。
//! 两套主题共用字号、间距与信息层级，只替换明度和主色，避免切换后像两套产品。
//! 四类内容各有一个主色——速递靛蓝、热点橙、日报薄荷绿、模型榜琥珀金，
//! 主色只出现在序号、分类、顶部微光与引用左界这几处。
//!
//! 排版参数是按「手机上读」这个真实场景定的，缩略图与点开全屏各一条要求：
//!   - 版心 608 CSS px 配合 `image_scale`（默认 3 倍）出图：相纸边距 12、卡面四周一律 24，
//!     正文宽 534。从前用 720，全屏时整张图缩到 0.57 倍，正文只剩 10dp；608 是 0.68 倍，
//!     正文 11.8dp、标题 18.6dp，群聊里被缩成缩略图时标题仍然读得出来。
//!     摘要每行 29 字，与 720 时相当——一眼看到的信息量不比从前少，字却大了一档；
//!   - **同一条正文列**：眉行、标题、摘要、补充说明的容器、分隔线共用同一条左缘与右缘，
//!     四周的内边距也只有一个值（24）。边距不必大，但各处必须对齐、成一个数；
//!   - 一条一格，自上而下是「眉行（序号 · 分类 · 信源 · 时间）→ 标题 → 摘要」。
//!     标题是全格最大最重的字，眉行与摘要依次退后，扫读只需看标题；
//!   - **同一件事只说一遍**：分类是眉行里的字、不再另做一枚芯片；推荐理由、
//!     最新进展与导语若只是在复述摘要就不画（见 [`restates`]）；推荐理由默认不画；热点榜不再重列信源名，
//!     「N 个报道来源」已经说了；
//!   - 格与格之间只留 20px 上下留白加一条细线，不用背景块。
//!
//! 图片只承载「读」的部分：链接一概不画进图里。需要查看全文时，用户引用
//! 卡片后直接回复序号，再按需取得正文与链接。

use super::api::{HotTopic, Item, Report, ReportItem, Story, category_label};
use super::cluster::{Cluster, overlap};
use super::leaderboard::Board;
use super::render::models_meta;
use super::render::{
    Digest, RenderOptions, STORY_NEIGHBORS, STORY_TIMELINE, clip, digest, fmt_time,
    report_item_meta, story_meta, story_report_meta,
};
use crate::render::web::esc;
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
    resolve_theme_at(mode, Utc::now().with_timezone(&crate::clock::beijing()))
}

fn resolve_theme_at(mode: &str, now: DateTime<chrono::FixedOffset>) -> CardTheme {
    match mode.trim().to_ascii_lowercase().as_str() {
        "light" | "day" | "白天" | "日间" => CardTheme::Light,
        "dark" | "night" | "夜晚" | "夜间" => CardTheme::Dark,
        _ if (7..19).contains(&now.hour()) => CardTheme::Light,
        _ => CardTheme::Dark,
    }
}

/// 卡片渲染宽度（CSS 像素）：相纸两侧各 12，卡面四周 24，正文宽 534。
/// 实际出图宽度 = WIDTH × `image_scale`（默认 3 倍 → 1824px）
const WIDTH: u32 = 608;

/// 一段补充说明（推荐理由、最新进展、导语）与已写出的正文词元重合到这个比例，就算复述。
/// 拿 2026-10-19 热点榜十条的「最新进展」量过：照抄标题或综述的都在 0.65 以上，
/// 补了新信息的最高 0.60——这条线宁可放过，不可错杀。
const RESTATES: f64 = 0.65;

/// 补充说明是不是在复述已经写出来的正文（标题、摘要等，可有多段）
fn restates(note: &str, shown: &[&str]) -> bool {
    overlap(note, &shown.join(" ")) >= RESTATES
}

/// 出图时刻（北京时间），放在页眉右上角
fn stamp() -> String {
    crate::clock::beijing_now().format("%Y-%m-%d %H:%M").to_string()
}

/// 资讯卡的版式。
///
/// 令牌与组件基元在 `res/cards/m3e.css`（`crate::render::web::DESIGN_SYSTEM`），
/// 这里只写这张卡自己的位置，**不写色值与字号字面量**。四类内容的主色由页面的
/// `seed-*` 类名换，这个文件不参与——所以「换分类」与「换版式」是两件互不牵连的事。
const CSS: &str = r#"
body{width:608px}
/* 相纸的底色由 m3e.css 的 `.shot` 给；内边距这张卡收到 12px——资讯卡要的是「一行多放字」。
   卡面四周只用一个值：上下与左右不一样，页眉贴着圆角的那一角就显得偏，
   各处也没法拿同一条边去对齐 */
.shot{padding:var(--md-space-3)}
.card{padding:var(--md-space-6)}
.card .md-eyebrow{margin-bottom:var(--md-space-4)}

/* —— 标题块 —— */
.title{margin-top:0}
.rule{margin:var(--md-space-5) 0 0}

/* —— 条目 —— */
/* 一条一格：眉行 → 标题 → 摘要，自上而下一级比一级退后。格与格之间靠 20px 留白
   与一条细线分开，不靠背景块——资讯卡是一条条读下去的，每格都上底色会把整页切成瓦。 */
.row{padding:var(--md-space-5) 0;border-bottom:1px solid var(--md-sys-color-outline-variant)}
.row:has(+ .foot){border-bottom:none;padding-bottom:var(--md-space-1)}
/* 眉行：序号牌 + 一串元信息。分隔点由 CSS 补，前后缺哪一项都不会留下孤点。
   序号牌是一条的入口（引用图片回复序号取链接），所以是牌，不是灰字。 */
.ey{display:flex;flex-wrap:wrap;align-items:center;gap:var(--md-space-1) 10px;
  font-size:var(--md-type-label-medium-size);line-height:1.5;font-weight:500;
  color:var(--md-sys-color-on-surface-variant)}
.ey>span+span:not(.sc)::before{content:"·";margin-right:10px;color:var(--md-sys-color-outline)}
.ey>.no+span:not(.sc)::before{content:none}
.ey .cat{font-weight:700;color:var(--md-sys-color-primary)}
.ey .sc{margin-left:auto;font-weight:700;font-variant-numeric:tabular-nums}
/* 序号牌：前三名填实，其余只用容器色。靠「有没有实底」分梯队，不靠换颜色 */
.no{min-width:34px;padding:1px 9px;border-radius:var(--md-shape-full);text-align:center;
  font-size:var(--md-type-label-large-size);font-weight:800;letter-spacing:.02em;
  font-variant-numeric:tabular-nums;
  color:var(--md-sys-color-on-primary-container);background:var(--md-sys-color-primary-container)}
.no.top{color:var(--md-sys-color-on-primary);background:var(--md-sys-color-primary)}
/* 标题：全格最大最重的字。缩略图里别的字都糊了，这一行还要读得出来 */
.h{margin-top:var(--md-space-2);font-size:var(--md-type-title-large-size);line-height:1.38;
  font-weight:800;color:var(--md-sys-color-on-surface);text-wrap:balance}
/* 标题只在词界换行：词与词之间由 [`phrase`] 放了 wbr，keep-all 去掉汉字之间的默认断点 */
.h,.lt,.title{word-break:keep-all}
.sum{margin-top:var(--md-space-2);font-size:var(--md-type-body-medium-size);
  line-height:1.62;color:var(--md-sys-color-on-surface-variant);text-wrap:pretty}
/* 「另有 N 家信源报道」：官网卡片上同一位置的折叠行，这里直接展开成名字 */
.also{margin-top:var(--md-space-2);display:flex;flex-wrap:wrap;align-items:baseline;
  gap:2px 10px;font-size:var(--md-type-label-medium-size);line-height:1.6;
  color:var(--md-sys-color-on-surface-faint)}
.also b{font-weight:700;color:var(--md-sys-color-on-surface-variant)}
.also span+span::before{content:"·";margin-right:10px;color:var(--md-sys-color-outline)}
/* 补充一句（推荐理由 / 最新进展）：一块主色淡底的圆角容器，容器的左右缘就是正文列的
   左右缘（与标题、摘要、分隔线对齐），字在容器里再缩进一档。
   从前是一道左界加缩进的字：字比上面的摘要缩进了 15px，两段对不齐，
   那道细线又贴着正文左缘，读起来像是正文挤出了框。
   这里不用 balance：它会把两三行的短块匀成三分之二宽，容器右侧空出一大块。 */
.why{margin-top:var(--md-space-3);padding:10px var(--md-space-4) 11px;
  border-radius:var(--md-shape-l);background:var(--md-sys-color-primary-tint);
  font-size:var(--md-type-body-medium-size);line-height:1.6;
  color:var(--md-sys-color-on-surface-variant);text-wrap:pretty}
/* 夜间的黑底上 7% 的主色淡底几乎看不出边界，换强一档（文字对比度 6.3:1） */
.dark .why{background:var(--md-sys-color-primary-tint-strong)}
.why b{color:var(--md-sys-color-primary);font-weight:800;letter-spacing:.02em;margin-right:.5em}

/* —— 日报、周报、月报与事件详情 —— */
.lead{margin:var(--md-space-5) 0 0;padding:var(--md-space-4);
  border-radius:var(--md-shape-l);background:var(--md-sys-color-surface-container-low);
  border:1px solid var(--md-sys-color-outline-variant);
  font-size:var(--md-type-body-medium-size);line-height:1.68;
  color:var(--md-sys-color-on-surface-variant)}
.lead p{margin:0}
.lead p+p{margin-top:var(--md-space-3)}
.sec{padding:var(--md-space-5) 0 var(--md-space-4);border-bottom:1px solid var(--md-sys-color-outline-variant)}
.sec:has(+ .foot){border-bottom:none}
.sec-h{display:flex;align-items:center;gap:var(--md-space-3);
  font-size:var(--md-type-title-large-size);line-height:1.3;font-weight:800;
  color:var(--md-sys-color-on-surface)}
.bar{width:5px;height:22px;border-radius:var(--md-shape-xs);background:var(--md-sys-color-primary)}
/* 栏目导语（周报、月报每栏有一两句）：压在栏目名下面，比条目更轻 */
.sec-s{margin-top:var(--md-space-2);font-size:var(--md-type-label-medium-size);line-height:1.62;
  color:var(--md-sys-color-on-surface-variant);text-wrap:pretty}
/* 条目前面是序号，不是圆点：引用本图回复序号就能取这一条的链接，序号得看得见。
   标题单独成行、是条目里最大最重的字；信源时间次之；摘要最后。 */
.li{margin-top:var(--md-space-4);padding-left:34px;position:relative}
.li:first-child,.sec-h+.li,.sec-s+.li{margin-top:var(--md-space-3)}
.li .n{position:absolute;left:0;top:1px;width:26px;text-align:right;white-space:nowrap;
  font-size:var(--md-type-title-medium-size);line-height:1.45;font-weight:800;
  color:var(--md-sys-color-primary);font-variant-numeric:tabular-nums}
.li .lt{font-size:var(--md-type-title-medium-size);line-height:1.45;font-weight:800;
  color:var(--md-sys-color-on-surface);text-wrap:balance}
.li .m{margin-top:2px;font-size:var(--md-type-label-medium-size);line-height:1.5;
  color:var(--md-sys-color-on-surface-variant)}
.li .t{margin-top:var(--md-space-1);font-size:var(--md-type-body-medium-size);line-height:1.6;
  color:var(--md-sys-color-on-surface-variant);text-wrap:pretty}

/* —— 模型榜 —— */
/* 行内三列都从顶端对齐：名次方块与分数跟标题的第一行齐平，而不是各自在行高里
   垂直居中——居中会让名次掉到来源那一行去，读起来像标错了对象。 */
.mrow{display:grid;grid-template-columns:40px minmax(0,1fr) 104px;gap:var(--md-space-3);
  align-items:start;padding:var(--md-space-5) 0;
  border-bottom:1px solid var(--md-sys-color-outline-variant)}
.mrow:has(+ .note){border-bottom:none}
/* 名次牌：前三名填实，其余仅描边。靠「有没有底」分梯队，不靠换个颜色 */
.rank{display:flex;align-items:center;justify-content:center;width:40px;height:40px;
  border-radius:var(--md-shape-m);font-size:var(--md-type-title-medium-size);
  font-weight:800;font-variant-numeric:tabular-nums;
  color:var(--md-sys-color-on-primary-container);
  background:var(--md-sys-color-primary-container);
  border:1px solid var(--md-sys-color-primary-line)}
.rank.top{color:var(--md-sys-color-on-primary);background:var(--md-sys-color-primary);
  border-color:transparent}
.mname{display:flex;align-items:baseline;flex-wrap:wrap;gap:10px;
  font-size:var(--md-type-title-large-size);line-height:1.3;font-weight:800;
  color:var(--md-sys-color-on-surface)}
.mmeta{margin-top:var(--md-space-2);display:flex;flex-wrap:wrap;align-items:center;gap:var(--md-space-1) 10px;
  font-size:var(--md-type-label-medium-size);line-height:1.5;font-weight:500;
  color:var(--md-sys-color-on-surface-variant)}
.meter{margin-top:var(--md-space-3);height:7px;border-radius:var(--md-shape-full);
  background:var(--md-sys-color-surface-container-high);overflow:hidden}
/* 分数条：浅头深尾的一条主色渐变。长度即分数，不做二次拉伸。 */
.meter i{display:block;height:100%;border-radius:var(--md-shape-full);
  background:linear-gradient(90deg,var(--md-sys-color-primary-line),var(--md-sys-color-primary))}
.mscore{text-align:right}
/* AIHOT 评分是这张图唯一要「一眼看到」的数字，给到 headline-medium，其余都压在灰阶里 */
.mscore strong{display:block;font-size:var(--md-type-headline-medium-size);line-height:1.1;
  font-weight:800;letter-spacing:-.02em;color:var(--md-sys-color-primary);
  font-variant-numeric:tabular-nums}
.mscore small{display:block;margin-top:4px;font-size:var(--md-type-label-medium-size);
  line-height:1.4;color:var(--md-sys-color-on-surface-variant);white-space:nowrap}
.note{margin-top:var(--md-space-4);font-size:var(--md-type-label-medium-size);line-height:1.7;
  color:var(--md-sys-color-on-surface-variant)}

/* —— 页脚 —— */
.foot{margin-top:var(--md-space-5);padding-top:var(--md-space-4)}
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
/// 英文是眉标的固定装饰，六张卡片都这么配（手册卡用的是品牌名 ACUMEN）。
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
    // 日报、事件的标题是一整句话：卡面 520 里 display 档一行只放得下十来个字，
    // 超过这个长度降到 headline 档，三行以内放得下
    let title_size = if title.chars().count() > 12 { "headline-medium" } else { "display-small" };
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
<div class="title md-title md-type-{title_size} md-balance">{title}</div>{subtitle}
<hr class="rule md-rule">
{body}
<div class="foot md-foot md-foot-row"><div class="src"><span class="mark"></span>AIHOT · aihot.news</div><div>{foot_note}</div></div>
</div></div></body></html>"#,
        css = css,
        seed = accent.0,
        theme_class = theme.class(),
        kicker = esc(kicker.0),
        kicker_en = esc(kicker.1),
        stamp = stamp(),
        title = phrase(title),
        title_size = title_size,
        subtitle = subtitle,
        body = body,
        foot_note = esc(foot_note),
    )
}

/// 一级推送只发图；用户引用图片后直接回复序号提取链接（0 表示全部）。
const FOOT_LINKS: &str = "引用本图回复 0全部｜序号 取链接";

/// 序号牌。`podium` 为真时前三名填实（榜单的名次），否则一律用容器色（只是个序号）
fn badge(index: u32, label: &str, podium: bool) -> String {
    let top = if podium && index <= 3 { " top" } else { "" };
    format!(r#"<span class="no{top}">{}</span>"#, esc(label))
}

/// 标题的断行：把中文按词切开，词与词之间放一个 `<wbr>`，再配合 CSS 的
/// `word-break: keep-all`，换行只落在词界，不会把「发布」「内部」拆在两行。
/// 返回的已经是转义过的 HTML。
fn phrase(text: &str) -> String {
    let word_like = |ch: Option<char>| ch.is_some_and(char::is_alphanumeric);
    let mut out = String::with_capacity(text.len() * 2);
    let mut prev_last: Option<char> = None;
    for token in crate::text::jieba().cut(text, true) {
        let word = token.word;
        let first = word.chars().next();
        let han = |ch: Option<char>| ch.is_some_and(|c| !c.is_ascii() && c.is_alphanumeric());
        // 两边都是「词」、且至少一边是汉字时才给断点：英文之间本来就有空格与连字符
        if word_like(prev_last) && word_like(first) && (han(prev_last) || han(first)) {
            out.push_str("<wbr>");
        }
        out.push_str(&esc(word));
        prev_last = word.chars().next_back();
    }
    out
}

/// 一条资讯的眉行：序号 · 分类 · 信源 · 时间 · 评分。
///
/// 分类是眉行里的一个主色字，不另做芯片——芯片的底色会把它抬得比信源与时间都重，
/// 而它与标题说的是同一件事，不该比标题还抢眼。
fn eyebrow(index: usize, item: &Item) -> String {
    let mut parts = vec![badge(index as u32, &format!("{index:02}"), false)];

    if let Some(cat) = item.category.as_deref().filter(|c| !c.trim().is_empty()) {
        parts.push(format!(r#"<span class="cat">{}</span>"#, esc(category_label(cat))));
    }
    if let Some(name) = item.source_label() {
        parts.push(format!("<span>{}</span>", esc(&name)));
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
                .map(|t| format!("{t} 收录"))
        });
    if let Some(t) = time {
        parts.push(format!("<span>{}</span>", esc(&t)));
    }
    // 官网卡片上的「AI 评分」
    if let Some(score) = item.score.filter(|s| *s > 0.0) {
        parts.push(format!(r#"<span class="sc">评分 {score:.0}</span>"#));
    }
    format!(r#"<div class="ey">{}</div>"#, parts.concat())
}

/// 资讯列表卡片（速递 / 搜索结果共用）。一行一个事件，与官网首页的卡片一一对应：
/// 代表报道的标题与摘要，其后是「另有 N 家信源报道」，最后是推荐理由。
pub fn items_card(
    title: &str,
    subtitle: &str,
    clusters: &[Cluster],
    opts: &RenderOptions,
    theme: CardTheme,
) -> String {
    let mut body = String::new();

    for (idx, cluster) in clusters.iter().enumerate() {
        let item = &cluster.lead;
        let title = item.title.as_deref().unwrap_or("(无标题)").trim();
        let summary = item.summary.as_deref().map(str::trim).filter(|s| !s.is_empty());

        body.push_str(&format!(
            r#"<div class="row">{}<div class="h">{}</div>"#,
            eyebrow(idx + 1, item),
            phrase(title)
        ));
        if let Some(summary) = summary {
            body.push_str(&format!(r#"<div class="sum">{}</div>"#, esc(&clip(summary, 110))));
        }
        let others = cluster.other_reports();
        if !others.is_empty() {
            const SHOWN: usize = 4;
            let names: Vec<String> = others
                .iter()
                .take(SHOWN)
                .map(|(name, _)| format!("<span>{}</span>", esc(name)))
                .collect();
            let etc = if others.len() > SHOWN { "<span>等</span>" } else { "" };
            body.push_str(&format!(
                r#"<div class="also"><b>另有 {} 家信源报道</b>{}{}</div>"#,
                others.len(),
                names.join(""),
                etc
            ));
        }
        // 推荐理由常常只是把摘要换个说法再讲一遍；真补充了东西才画
        if opts.show_reason
            && let Some(reason) = item.reason.as_deref().map(str::trim).filter(|s| !s.is_empty())
            && !restates(reason, &[title, summary.unwrap_or_default()])
        {
            body.push_str(&format!(
                r#"<div class="why"><b>推荐理由</b>{}</div>"#,
                esc(&clip(reason, 90))
            ));
        }
        body.push_str("</div>");
    }

    let subtitle = match subtitle.is_empty() {
        true => format!("共 {} 条", clusters.len()),
        false => format!("{} · 共 {} 条", subtitle, clusters.len()),
    };
    shell(BRIEF, theme, ("AI 资讯", "AI NEWS"), title, &subtitle, &body, FOOT_LINKS)
}

/// 热点榜卡片：前三名用实心序号牌，其余描边，一眼看出梯队
pub fn hot_topics_card(topics: &[HotTopic], theme: CardTheme) -> String {
    let mut body = String::new();

    for (idx, topic) in topics.iter().enumerate() {
        let rank = topic.rank.unwrap_or((idx + 1) as u32);
        let title = topic.title.as_deref().unwrap_or("(无标题)").trim();
        let summary = topic.digest_lead();

        // 眉行：名次 · 热度 · 最新时间。信源名不再重列——「N 个报道来源」已经说了
        let mut ey = vec![badge(rank, &rank.to_string(), true)];
        if let Some(count) = topic.source_count.filter(|c| *c > 0) {
            ey.push(format!(r#"<span class="cat">{count} 个报道来源</span>"#));
        }
        if let Some(count) = topic.participant_count.filter(|c| *c > 0) {
            ey.push(format!("<span>{count} 人讨论</span>"));
        }
        if let Some(t) = topic.latest_at.as_deref().and_then(fmt_time) {
            ey.push(format!("<span>最新 {}</span>", esc(&t)));
        }
        body.push_str(&format!(
            r#"<div class="row"><div class="ey">{}</div><div class="h">{}</div>"#,
            ey.concat(),
            phrase(title)
        ));

        if let Some(summary) = summary {
            body.push_str(&format!(r#"<div class="sum">{}</div>"#, esc(&clip(summary, 110))));
        }
        // 最新进展与标题、综述说的是同一句话时不再重复一遍
        if let Some(latest) = topic.latest.as_deref().map(str::trim).filter(|s| !s.is_empty())
            && !restates(latest, &[title, summary.unwrap_or_default()])
        {
            body.push_str(&format!(
                r#"<div class="why"><b>最新进展</b>{}</div>"#,
                esc(&clip(latest, 90))
            ));
        }
        body.push_str("</div>");
    }

    let subtitle = format!("过去 48 小时 · 跨信源聚合 · TOP {}", topics.len());
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

/// 模型榜卡片：一行一个模型，左名次、中模型与价格、右 AIHOT 评分。
///
/// 评分是这张图唯一需要「一眼看到」的数字，所以给到 34px 主色 + 一条同色进度条；
/// 其余信息（厂商、上线日期、价格、评测覆盖）压到 15px 的灰阶里，不与之争。
pub fn models_card(board: &Board, max_items: usize, theme: CardTheme) -> String {
    let shown = &board.entries[..board.entries.len().min(max_items.max(1))];
    let mut body = String::new();

    for (idx, model) in shown.iter().enumerate() {
        let rank = model.rank.unwrap_or((idx + 1) as u32);
        let rank_cls = if rank <= 3 { "rank top" } else { "rank" };

        body.push_str(&format!(
            r#"<div class="mrow"><div><div class="{rank_cls}">{rank}</div></div><div>"#
        ));
        body.push_str(&format!(
            r#"<div class="mname">{}</div>"#,
            esc(model.display_name())
        ));

        let mut meta: Vec<String> = Vec::new();
        if let Some(provider) = model.provider_name() {
            meta.push(format!(r#"<span class="md-chip">{}</span>"#, esc(provider)));
        }
        if let Some(date) = model.released_date() {
            meta.push(format!(r"<span>上线 {}</span>", esc(date)));
        }
        if let Some(price) = model.price_text() {
            meta.push(format!(r"<span>{}</span>", esc(&price)));
        }
        if !meta.is_empty() {
            body.push_str(&format!(r#"<div class="mmeta">{}</div>"#, meta.join("")));
        }

        // 分数条按 0—100 直接映射，长度即分数，不做二次拉伸
        if let Some(score) = model.score {
            body.push_str(&format!(
                r#"<div class="meter"><i style="width:{:.1}%"></i></div>"#,
                score.clamp(0.0, 100.0)
            ));
        }
        body.push_str("</div>");

        // 评测项数与覆盖度各占一行：右栏窄，挤在一行会折得七零八落
        let mut note = String::new();
        if let Some(count) = model.evaluations_text() {
            note.push_str(&format!("<small>{}</small>", esc(&count)));
        }
        if let Some(coverage) = model.coverage_text() {
            note.push_str(&format!("<small>{}</small>", esc(&coverage)));
        }
        body.push_str(&format!(
            r#"<div class="mscore"><strong>{}</strong>{}</div></div>"#,
            esc(&model.score_text().unwrap_or_else(|| "—".to_string())),
            note
        ));
    }

    let pending = match board.pending.filter(|n| *n > 0) {
        Some(n) => format!("另有 {n} 个模型评测还不够宽，暂不排名，补齐后自动给名次。"),
        None => String::new(),
    };
    body.push_str(&format!(
        r#"<div class="note">只用第三方评测机构统一实测的成绩，厂商自报的分数不计。评分 50 分是参照模型的平均水平，每 15 分约一个标准差，不是正确率；名次按分数排。价格为厂商官网参考价，人民币／百万 Token。{}</div>"#,
        esc(&pending)
    ));

    let mut subtitle = models_meta(board);
    subtitle.push(format!("TOP {}", shown.len()));

    shell(
        MODELS,
        theme,
        ("模型榜", "AI LEADERBOARD"),
        &format!("AIHOT {}", board.title()),
        &subtitle.join(" · "),
        &body,
        FOOT_LINKS,
    )
}

/// 报告里的一条：序号、标题，其后是「信源 · 发布时间」与摘要。
/// 摘要若只是把导语复述了一遍（日报头条常这样）就不再画。
fn report_li(out: &mut String, number: usize, item: &ReportItem, lead: Option<&str>) {
    out.push_str(&format!(
        r#"<div class="li"><span class="n">{number}</span><div class="lt">{}</div>"#,
        phrase(item.title.trim())
    ));
    if let Some(meta) = report_item_meta(item) {
        out.push_str(&format!(r#"<div class="m">{}</div>"#, esc(&meta)));
    }
    if let Some(summary) = item.summary.as_deref()
        && !lead.is_some_and(|lead| restates(summary, &[lead]))
    {
        out.push_str(&format!(r#"<div class="t">{}</div>"#, esc(&clip(summary, 110))));
    }
    out.push_str("</div>");
}

/// 事件详情卡：完整的 AI 综述、最新进展，再是按时间排好的报道。
/// 序号与 [`super::render::render_story`] 的条目一一对应，最后一个序号是事件页。
pub fn story_card(story: &Story, rank: Option<u32>, theme: CardTheme) -> String {
    let mut body = String::new();

    let paragraphs = story.digest_paragraphs();
    if !paragraphs.is_empty() {
        body.push_str(r#"<div class="lead">"#);
        for paragraph in paragraphs {
            body.push_str(&format!("<p>{}</p>", esc(&clip(paragraph, 420))));
        }
        body.push_str("</div>");
    }
    if let Some(latest) = story.latest.as_deref() {
        let when = story.latest_at.as_deref().and_then(fmt_time);
        let tail = when.map(|t| format!(" · {t}")).unwrap_or_default();
        body.push_str(&format!(
            r#"<div class="why"><b>最新进展</b>{}{}</div>"#,
            esc(latest),
            esc(&tail)
        ));
    }

    let timeline = story.timeline(STORY_TIMELINE);
    body.push_str(r#"<div class="sec"><div class="sec-h"><span class="bar"></span>报道时间线</div>"#);
    if timeline.len() < story.reports.len() {
        body.push_str(&format!(
            r#"<div class="sec-s">共 {} 篇，这里取最早的一篇与最近的几篇</div>"#,
            story.reports.len()
        ));
    }
    let mut number = 0usize;
    for report in &timeline {
        number += 1;
        let item = &report.item;
        body.push_str(&format!(
            r#"<div class="li"><span class="n">{number}</span><div class="lt">{}</div>"#,
            phrase(item.title.trim())
        ));
        if let Some(meta) = story_report_meta(report) {
            body.push_str(&format!(r#"<div class="m">{}</div>"#, esc(&meta)));
        }
        body.push_str("</div>");
    }
    body.push_str("</div>");

    for (label, neighbors) in [("故事线", &story.storyline), ("相关事件", &story.related)] {
        let shown: Vec<_> = neighbors.iter().take(STORY_NEIGHBORS).collect();
        if shown.is_empty() {
            continue;
        }
        body.push_str(&format!(
            r#"<div class="sec"><div class="sec-h"><span class="bar"></span>{label}</div>"#
        ));
        for neighbor in shown {
            number += 1;
            body.push_str(&format!(
                r#"<div class="li"><span class="n">{number}</span><div class="lt">{}</div>"#,
                phrase(neighbor.title.trim())
            ));
            if !neighbor.relation.trim().is_empty() {
                body.push_str(&format!(
                    r#"<div class="m">关联：{}</div>"#,
                    esc(&clip(neighbor.relation.trim(), 80))
                ));
            }
            body.push_str("</div>");
        }
        body.push_str("</div>");
    }

    if story.link.is_some() {
        number += 1;
        body.push_str(&format!(
            r#"<div class="sec"><div class="li"><span class="n">{number}</span><div class="lt">阅读事件页</div><div class="m">报道全貌、热度变化与 AI 综述</div></div></div>"#
        ));
    }

    let meta = story_meta(story);
    let subtitle = match rank {
        Some(rank) => format!("第 {rank} 名 · {meta}"),
        None => meta,
    };
    shell(HOT, theme, ("热点事件", "AI STORY"), &story.title, &subtitle, &body, FOOT_LINKS)
}

/// 日报、周报、月报共用的卡片：导语、按栏目分好的条目、日报的快讯，最后一个序号是整期的阅读页。
/// 序号与 [`super::render::render_report`] 的条目一一对应，引用本图回复序号即可取链接。
pub fn report_card(report: &Report, budget: usize, theme: CardTheme) -> String {
    let digest: Digest = digest(report, budget);
    let mut body = String::new();

    if let Some(lead) = report.lead.as_deref() {
        body.push_str(&format!(r#"<div class="lead">{}</div>"#, esc(&clip(lead, 320))));
    }

    let mut number = 0usize;
    for (section, items) in &digest.sections {
        body.push_str(r#"<div class="sec">"#);
        if let Some(label) = section.label.as_deref() {
            body.push_str(&format!(
                r#"<div class="sec-h"><span class="bar"></span>{}</div>"#,
                esc(label)
            ));
        }
        if let Some(summary) = section.summary.as_deref() {
            body.push_str(&format!(r#"<div class="sec-s">{}</div>"#, esc(&clip(summary, 160))));
        }
        for item in items {
            number += 1;
            report_li(&mut body, number, item, report.lead.as_deref());
        }
        body.push_str("</div>");
    }

    if !digest.flashes.is_empty() {
        body.push_str(
            r#"<div class="sec"><div class="sec-h"><span class="bar"></span>快讯</div>"#,
        );
        for item in &digest.flashes {
            number += 1;
            report_li(&mut body, number, item, report.lead.as_deref());
        }
        body.push_str("</div>");
    }

    if report.link.is_some() {
        number += 1;
        body.push_str(&format!(
            r#"<div class="sec"><div class="li"><span class="n">{number}</span><div class="lt">阅读完整{}</div><div class="m">{}</div></div></div>"#,
            report.kind.noun(),
            esc(&report.edition())
        ));
    }

    let title = report.headline.as_deref().unwrap_or(report.kind.title());
    let count = if digest.shown() < digest.total {
        format!("摘选 {} / {} 条", digest.shown(), digest.total)
    } else {
        format!("共 {} 条", digest.total)
    };
    let subtitle = match digest.total {
        0 => report.subtitle(),
        _ => format!("{} · {count}", report.subtitle()),
    };

    shell(
        DAILY,
        theme,
        (report.kind.title(), report.kind.kicker_en()),
        title,
        &subtitle,
        &body,
        FOOT_LINKS,
    )
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
