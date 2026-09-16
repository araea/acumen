//! Markdown → 回复卡片图片。
//!
//! **出图**——量高度、等字体、尺寸护栏与并发闸门都收在
//! [`crate::render::web::shoot`] 一处，本模块只声明这张卡片的宽度、格式与出图范围。
//! 早年那套「设视口 → 注入 → 睡 200ms → 量高度 → 再设视口 → 睡 100ms → 查元素 →
//! 取盒模型 → 截图」现在已无处可寻：两次固定睡眠与多次 CDP 往返都花在等一个本来
//! 可以被观测到的状态上。
//!
//! **可读性**——排版按聊天里「一屏读完」来调：正文行高放宽、层级用色块而非纯字号
//! 区分、代码块深色高对比、表格斑马纹、长 URL 强制断行；正文之外还能挂来源列表
//! 与耗时页脚，让读者一眼看清结论出处与代价。

use crate::render::web as render;
use pulldown_cmark::{Options, Parser, html};
use regex::Regex;
use std::sync::OnceLock;

/// 卡片 CSS 宽度；配合 2 倍像素密度即 1040px 位图，在手机聊天窗口里既清晰又不糊。
const CARD_WIDTH: u32 = 520;
/// 视口留出的左右留白。
const VIEWPORT_WIDTH: u32 = CARD_WIDTH + 40;
/// 设备像素比的默认值。2 倍即 1040px 位图，与 520px 的版心配在一起最省体积又不糊；
/// 部署者可以用 `[oai] image_scale` 改它。
pub(crate) const DEVICE_SCALE: f64 = 2.0;
/// 单张图片的高度上限（CSS 像素），超出就退回纯文本，避免超大图拖垮发送。
const MAX_CARD_HEIGHT: f64 = 20_000.0;

/// 卡片内容。
pub(crate) struct Card<'a> {
    pub title: &'a str,
    pub markdown: &'a str,
    /// 参考来源，渲染成正文后的编号列表。
    pub sources: &'a [super::types::Source],
    /// 页脚：模型、耗时与工具轨迹。
    pub footer: Option<Footer>,
}

/// 卡片页脚。
///
/// 轨迹保持结构化而不是先拼成一行：拼成一行就只能靠截断收尾，而工具参数
/// 被砍掉的那一半往往正是要看的内容。分行渲染后长参数自然折行，不再丢字。
pub(crate) struct Footer {
    /// 模型与耗时。
    pub meta: String,
    /// 工具调用轨迹，每步一行。
    pub trace: Vec<super::types::TraceStep>,
    /// 未列出的调用次数。
    pub trace_overflow: usize,
}

/// 渲染成 base64 JPEG。`scale` 是出图倍率（1—4），由 `[oai] image_scale` 给。
pub(crate) async fn render_card(card: Card<'_>, scale: f64) -> anyhow::Result<String> {
    let html = build_html(&card);
    // 量高度、等字体、尺寸护栏与并发闸门都在 `render::web::shoot` 一处。
    // 出图范围是 `.card`（正文的 20px 留白由 body 提供，不进图）。
    render::shoot(
        render::Shot::new(&html, VIEWPORT_WIDTH)
            .selector(".card")
            .scale(scale)
            .jpeg(88)
            .max_height(MAX_CARD_HEIGHT),
    )
    .await
}

fn build_html(card: &Card<'_>) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);
    let parser = Parser::new_ext(card.markdown, options);
    let mut body = String::new();
    html::push_html(&mut body, parser);
    let body = label_code_blocks(&body);

    let sources = render_sources(card.sources);
    let footer = card.footer.as_ref().map(render_footer).unwrap_or_default();
    // 模型与耗时挪到页眉右端。它与页眉左边那句「谁在几点回的谁」是同一类信息
    // （这条回复的出处），放在页脚则在最不显眼处；页脚只留工具轨迹。
    let stamp = card
        .footer
        .as_ref()
        .map(|footer| footer.meta.trim().to_string())
        .unwrap_or_default();
    let stamp = if stamp.is_empty() {
        String::new()
    } else {
        format!(r#"<span class="md-stamp">{}</span>"#, escape_html(&stamp))
    };

    format!(
        r#"<!DOCTYPE html><html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:"><style>{SYSTEM}{CSS}</style></head>
<body class="scheme-reply md-text"><div class="card md-card"><div class="inner"><div class="md-eyebrow"><div class="md-kicker"><span class="md-dot"></span>智能回复<span class="md-kicker-en">REPLY</span></div>{stamp}</div><div class="head md-title md-type-title-small">{title}</div><hr class="md-divider"><div class="body">{body}</div></div>{sources}{footer}</div></body></html>"#,
        SYSTEM = crate::render::web::DESIGN_SYSTEM,
        title = escape_html(card.title),
    )
}

/// 把 `<pre><code class="language-x">` 改写成 `<pre data-lang="x">`，
/// 让 CSS 能用 `attr()` 在代码块角上标出语言——纯 CSS 拿不到子元素的类名。
fn label_code_blocks(html: &str) -> String {
    static CODE: OnceLock<Regex> = OnceLock::new();
    CODE.get_or_init(|| Regex::new(r#"(?is)<pre><code class="language-([^"]+)">"#).unwrap())
        .replace_all(html, |caps: &regex::Captures| {
            format!(r#"<pre data-lang="{}"><code>"#, escape_html(&caps[1]))
        })
        .into_owned()
}

/// 页脚只承载工具轨迹。
///
/// 模型与耗时挪去了页眉右端（与页眉左边那句「这是谁回的」同属「这条回复的出处」），
/// 所以这里没有 meta 可画时整块不出现——不为了「页脚有页脚的样子」留一条空边。
fn render_footer(footer: &Footer) -> String {
    if footer.trace.is_empty() {
        return String::new();
    }
    let mut out = String::from(r#"<div class="foot">"#);
    out.push_str(r#"<div class="trace">"#);
    for step in &footer.trace {
        out.push_str(&format!(
            r#"<div class="trace-row"><span class="trace-name">{}</span>"#,
            escape_html(&step.name)
        ));
        if !step.detail.is_empty() {
            out.push_str(&format!(
                r#"<span class="trace-arg">{}</span>"#,
                escape_html(&step.detail)
            ));
        }
        if step.repeats > 1 {
            out.push_str(&format!(r#"<span class="trace-rep">×{}</span>"#, step.repeats));
        }
        out.push_str("</div>");
    }
    if footer.trace_overflow > 0 {
        out.push_str(&format!(
            r#"<div class="trace-more">另有 {} 次调用</div>"#,
            footer.trace_overflow
        ));
    }
    out.push_str("</div>");
    out.push_str("</div>");
    out
}

fn render_sources(sources: &[super::types::Source]) -> String {
    if sources.is_empty() {
        return String::new();
    }
    let items = sources
        .iter()
        .take(12)
        .enumerate()
        .map(|(index, source)| {
            format!(
                r#"<li><span class="src-idx">{}</span><span class="src-title">{}</span><span class="src-host">{}</span></li>"#,
                index + 1,
                escape_html(&source.title),
                escape_html(&host_of(&source.url)),
            )
        })
        .collect::<String>();
    format!(r#"<div class="sources"><div class="src-head">参考来源</div><ol>{items}</ol></div>"#)
}

fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| {
            parsed
                .host_str()
                .map(|host| host.trim_start_matches("www.").to_string())
        })
        .unwrap_or_else(|| super::utils::truncate_chars(url, 40))
}

pub(crate) fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
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

/// 回复卡的版式。
///
/// 令牌与组件基元在 `res/cards/m3e.css`（`crate::render::web::DESIGN_SYSTEM`），
/// 这里只写这张卡自己的位置与 Markdown 的元素样式，**不写色值与字号字面量**。
/// 与那五张卡的分工差别只有一条：这张卡的内容是 Markdown，元素由解析器产出，
/// 所以多出一段「HTML 元素 → 令牌」的映射，其余版式语言完全一致。
///
/// 卡上的小字只有两档：页脚轨迹、来源序号、智能体小卡走 `label-small`（11px），
/// 稍大一号的小标题走 `label-medium`（14px）。这两处原先是 10 / 10.5 / 11 / 11.5 /
/// 13 / 13.5 / 14 七个值——比整支字阶还密，等于在系统之外又养了一套字阶。
/// 行内代码的 `0.86em` 是例外：它相对父级字号，不属于这支字阶。
const CSS: &str = r#"
body{padding:20px;background:var(--md-sys-color-surface-dim)}
.card{width:520px}
/* 内芯吃左右内边距，来源与页脚两块「附录」则通栏铺到卡片边缘——
   附录是另一张纸，边界要看得见，不能和正文一样缩在版心内。 */
.inner{padding:var(--md-space-5) var(--md-space-6) var(--md-space-6)}
.head{margin-bottom:var(--md-space-3)}
.inner>.md-divider{margin-bottom:var(--md-space-5)}

/* —— 正文 —— */
.body{font-size:var(--md-type-body-medium-size);line-height:var(--md-type-body-medium-line);
  font-weight:var(--md-type-body-medium-weight);color:var(--md-sys-color-on-surface)}
.body>*:first-child{margin-top:0}
.body>*:last-child{margin-bottom:0}
p{margin:var(--md-space-3) 0;text-wrap:pretty}
h1,h2,h3,h4{margin:var(--md-space-5) 0 var(--md-space-3);line-height:1.45;
  font-weight:700;color:var(--md-sys-color-on-surface);text-wrap:balance}
h1{font-size:var(--md-type-title-large-size);padding-bottom:var(--md-space-2);
  border-bottom:1px solid var(--md-sys-color-outline-variant)}
/* h2 用一条主色短竖扛起「新一节」：字号只比 h3 大一档，要让人一眼看出层次 */
h2{font-size:var(--md-type-title-medium-size);padding-left:10px;
  border-left:4px solid var(--md-sys-color-primary)}
h3{font-size:var(--md-type-title-small-size)}
h4{font-size:var(--md-type-body-medium-size)}
ul,ol{margin:var(--md-space-3) 0;padding-left:22px}
li{margin:5px 0}
li>p{margin:var(--md-space-1) 0}
li::marker{color:var(--md-sys-color-outline)}
input[type=checkbox]{margin-right:6px;accent-color:var(--md-sys-color-primary)}
strong{font-weight:700;color:var(--md-sys-color-on-surface)}
em{color:var(--md-sys-color-on-surface-variant)}
del{color:var(--md-sys-color-on-surface-faint)}
a{color:var(--md-sys-color-primary);text-decoration:none;
  border-bottom:1px solid var(--md-sys-color-primary-line)}
code{padding:1px 6px;border-radius:var(--md-shape-xs);
  background:var(--md-sys-color-secondary-container);color:var(--md-sys-color-on-secondary-container);
  font-family:var(--md-font-mono);font-size:0.86em;font-weight:600}
pre{position:relative;margin:var(--md-space-3) 0;padding:13px var(--md-space-4);
  background:var(--md-sys-color-inverse-surface);border-radius:var(--md-shape-m);
  overflow-wrap:anywhere}
pre[data-lang]{padding-top:26px}
pre[data-lang]::before{content:attr(data-lang);position:absolute;top:6px;left:var(--md-space-4);
  font-size:var(--md-type-label-small-size);font-weight:700;letter-spacing:.06em;text-transform:uppercase;
  color:color-mix(in srgb,var(--md-sys-color-on-inverse-surface) 55%,transparent)}
pre code{display:block;padding:0;background:none;color:var(--md-sys-color-on-inverse-surface);
  font-size:var(--md-type-label-medium-size);line-height:1.75;white-space:pre-wrap;word-break:break-word}
/* 引语与「推荐理由」同形：主色淡底 + 一条主色左界 + 收一个角 */
blockquote{margin:var(--md-space-3) 0;padding:var(--md-space-3) var(--md-space-4);
  background:var(--md-sys-color-primary-tint);border-left:3px solid var(--md-sys-color-primary);
  border-radius:var(--md-corner-notched);color:var(--md-sys-color-on-surface-variant)}
blockquote p{margin:var(--md-space-1) 0}
table{width:100%;margin:var(--md-space-3) 0;border-collapse:collapse;
  font-size:var(--md-type-label-large-size);table-layout:fixed;overflow-wrap:anywhere}
th,td{padding:10px var(--md-space-2);vertical-align:top;text-align:left;
  border:1px solid var(--md-sys-color-outline-variant)}
th{background:var(--md-sys-color-surface-container);font-weight:700;
  color:var(--md-sys-color-on-surface);white-space:normal}
tr:nth-child(2n) td{background:var(--md-sys-color-surface-container-low)}
hr{margin:var(--md-space-4) 0;border:0;border-top:1px solid var(--md-sys-color-outline-variant)}
img{max-width:100%;height:auto;margin:var(--md-space-2) 0;border-radius:var(--md-shape-s)}
.footnote-definition{margin:6px 0;font-size:var(--md-type-label-medium-size);
  color:var(--md-sys-color-on-surface-variant)}
.footnote-definition p{display:inline;margin:0}

/* —— 参考来源 —— */
/* 中性面 + 顶线：它是正文之外的附录，不该和正文抢同一张纸的亮度 */
.sources{margin-top:var(--md-space-5);padding:var(--md-space-3) var(--md-space-6);
  background:var(--md-sys-color-surface-container-low);
  border-top:1px solid var(--md-sys-color-outline-variant)}
.src-head{font-size:var(--md-type-label-small-size);font-weight:var(--md-type-label-small-weight);
  letter-spacing:var(--md-type-label-small-track);color:var(--md-sys-color-on-surface-faint);
  margin-bottom:7px}
.sources ol{list-style:none;padding:0;margin:0}
.sources li{display:grid;grid-template-columns:20px minmax(0,1fr);align-items:baseline;
  gap:3px var(--md-space-2);margin:var(--md-space-1) 0;
  font-size:var(--md-type-label-medium-size);line-height:1.5}
/* 序号走 tertiary 容器：正文的主色已经用在链接上了，序号要另一支色才不混 */
.src-idx{flex:none;min-width:17px;height:17px;border-radius:var(--md-shape-xs);
  background:var(--md-sys-color-tertiary-container);color:var(--md-sys-color-on-tertiary-container);
  font-size:var(--md-type-label-small-size);font-weight:800;display:flex;align-items:center;justify-content:center}
.src-title{min-width:0;color:var(--md-sys-color-on-surface-variant);overflow-wrap:anywhere}
.src-host{grid-column:2;color:var(--md-sys-color-on-surface-faint);
  font-size:var(--md-type-label-medium-size);overflow-wrap:anywhere}

/* —— 页脚：只有工具轨迹 —— */
.foot{margin-top:var(--md-space-5);padding:9px var(--md-space-6);
  background:var(--md-sys-color-surface-container-low);
  border-top:1px solid var(--md-sys-color-outline-variant);
  font-size:var(--md-type-label-small-size);color:var(--md-sys-color-on-surface-faint);
  line-height:1.6;overflow-wrap:anywhere}
.trace{margin-top:5px;display:flex;flex-direction:column;gap:3px}
.trace-row{display:flex;align-items:baseline;gap:6px}
.trace-name{flex:none;padding:0 5px;border-radius:var(--md-shape-xs);
  background:var(--md-sys-color-secondary-container);
  color:var(--md-sys-color-on-secondary-container);font-family:var(--md-font-mono);
  font-size:var(--md-type-label-small-size);font-weight:700}
.trace-arg{flex:1;min-width:0;color:var(--md-sys-color-on-surface-faint);
  font-family:var(--md-font-mono);font-size:var(--md-type-label-small-size);line-height:1.5;
  overflow-wrap:anywhere;word-break:break-word}
.trace-rep{flex:none;color:var(--md-sys-color-on-surface-faint);font-size:var(--md-type-label-small-size)}
.trace-more{margin-top:2px;color:var(--md-sys-color-on-surface-faint);font-size:var(--md-type-label-small-size)}

/* —— 智能体与模型清单用的紧凑片段（以裸 HTML 嵌在 Markdown 里） —— */
.agent-card{margin:var(--md-space-3) 0;padding:var(--md-space-3);
  background:var(--md-sys-color-surface-container-low);
  border:1px solid var(--md-sys-color-outline-variant);border-radius:var(--md-shape-m)}
.agent-name{margin-bottom:7px;font-size:var(--md-type-label-large-size);font-weight:700;
  color:var(--md-sys-color-on-surface)}
.agent-info{font-size:var(--md-type-label-medium-size);line-height:1.85;
  color:var(--md-sys-color-on-surface-variant)}
.agent-info code{font-size:var(--md-type-label-small-size)}
.model-group{margin-bottom:15px;break-inside:avoid}
.model-header{display:flex;align-items:center;justify-content:space-between;
  margin-bottom:var(--md-space-2);padding:6px 10px;border-left:3px solid var(--md-sys-color-primary);
  border-radius:var(--md-shape-s);background:var(--md-sys-color-surface-container);
  font-size:var(--md-type-label-medium-size);font-weight:700;color:var(--md-sys-color-on-surface-variant)}
.model-count{padding:1px 6px;border-radius:var(--md-shape-xs);
  background:var(--md-sys-color-surface-container-high);
  color:var(--md-sys-color-on-surface-faint);font-size:var(--md-type-label-small-size)}
.agent-grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:7px}
.agent-mini{padding:var(--md-space-2);border:1px solid var(--md-sys-color-outline-variant);
  border-radius:var(--md-shape-s);background:var(--md-sys-color-surface)}
.agent-mini-top{display:flex;align-items:center;margin-bottom:3px}
.agent-idx{flex:none;display:flex;align-items:center;justify-content:center;min-width:18px;
  height:18px;margin-right:6px;border-radius:var(--md-shape-xs);
  background:var(--md-sys-color-tertiary-container);color:var(--md-sys-color-on-tertiary-container);
  font-size:var(--md-type-label-small-size);font-weight:800}
.agent-mini-name{overflow:hidden;white-space:nowrap;text-overflow:ellipsis;
  font-size:var(--md-type-label-medium-size);font-weight:700;color:var(--md-sys-color-on-surface)}
.agent-mini-desc{overflow:hidden;white-space:nowrap;text-overflow:ellipsis;font-size:var(--md-type-label-small-size);
  color:var(--md-sys-color-on-surface-faint)}
.mod-group{margin-bottom:15px;break-inside:avoid}
.mod-title{margin-bottom:var(--md-space-2);padding-left:7px;
  border-left:3px solid var(--md-sys-color-primary);font-size:var(--md-type-label-medium-size);
  font-weight:800;letter-spacing:.05em;color:var(--md-sys-color-on-surface-variant);
  text-transform:uppercase}
.chip-box,.chip-container{display:flex;flex-wrap:wrap;gap:7px}
.chip{display:flex;align-items:center;flex-wrap:wrap;gap:var(--md-space-1);padding:5px 9px;
  border:1px solid var(--md-sys-color-outline-variant);border-radius:var(--md-shape-s);
  background:var(--md-sys-color-surface);font-size:var(--md-type-label-medium-size);
  color:var(--md-sys-color-on-surface-variant)}
.chip-idx{margin-right:7px;padding:1px 5px;border-radius:var(--md-shape-xs);
  background:var(--md-sys-color-surface-container);
  color:var(--md-sys-color-on-surface-faint);font-family:var(--md-font-mono);
  font-size:var(--md-type-label-small-size);font-weight:700}
.chip-name{font-weight:500}
.chip-bad,.chip-badge{margin-left:7px;padding:1px 6px;border-radius:var(--md-shape-full);
  background:var(--md-sys-color-tertiary-container);color:var(--md-sys-color-on-tertiary-container);
  font-size:var(--md-type-label-small-size);font-weight:700}
.provider-section{margin-bottom:18px;break-inside:avoid}
.provider-title{margin-bottom:var(--md-space-2);padding-left:6px;
  border-left:3px solid var(--md-sys-color-outline);font-size:var(--md-type-label-medium-size);font-weight:800;
  color:var(--md-sys-color-on-surface-variant)}
.head,.agent-mini,.chip,.trace-name{min-width:0;overflow-wrap:anywhere}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// 版式里不许出现 HTML 的成对标签——样式表塞进 `style` 元素时会被截断，
    /// 而页面不会报错（见 [`crate::render::web::assert_embeddable`]）。
    #[test]
    fn stylesheet_stays_embeddable() {
        crate::render::web::assert_embeddable("oai", CSS);
    }
    use crate::plugins::oai::types::{Source, TraceStep};

    #[test]
    fn code_blocks_carry_their_language_label() {
        let html =
            label_code_blocks(r#"<pre><code class="language-rust">fn main(){}</code></pre>"#);
        assert_eq!(
            html,
            r#"<pre data-lang="rust"><code>fn main(){}</code></pre>"#
        );
    }

    #[test]
    fn renders_markdown_structure_and_sources() {
        let sources = [Source {
            title: "OpenAI 官网".into(),
            url: "https://www.openai.com/index/a?utm=1".into(),
        }];
        let html = build_html(&Card {
            title: "研究 #3回复",
            markdown: "## 结论\n\n- 要点\n\n```rust\nfn main() {}\n```\n",
            sources: &sources,
            footer: Some(Footer {
                meta: "gpt-5.6-luna · 8.2秒".into(),
                trace: vec![TraceStep::new("bash", "cargo test oai 工具轨迹 渲染")],
                trace_overflow: 0,
            }),
        });
        assert!(html.contains("<h2>结论</h2>"), "{html}");
        assert!(html.contains(r#"<pre data-lang="rust">"#), "{html}");
        assert!(html.contains("openai.com"), "{html}");
        assert!(!html.contains("utm=1"), "来源只展示域名");
        assert!(html.contains("gpt-5.6-luna · 8.2秒"), "{html}");
        assert!(html.contains("bash"), "{html}");
        assert!(
            html.contains("cargo test oai 工具轨迹 渲染"),
            "工具参数完整出现在页脚：{html}"
        );
    }

    #[test]
    fn title_and_footer_are_escaped() {
        let html = build_html(&Card {
            title: "<script>x</script>",
            markdown: "hi",
            sources: &[],
            footer: Some(Footer {
                meta: "a & b".into(),
                trace: Vec::new(),
                trace_overflow: 0,
            }),
        });
        assert!(!html.contains("<script>x</script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("a &amp; b"));
    }

    #[test]
    fn sources_block_is_omitted_when_empty() {
        assert!(render_sources(&[]).is_empty());
    }

    #[test]
    fn host_falls_back_to_the_raw_value() {
        assert_eq!(host_of("https://www.example.com/a"), "example.com");
        assert_eq!(host_of("not a url"), "not a url");
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use crate::plugins::oai::types::{Source, TraceStep};

    #[test]
    #[ignore = "仅生成本地排版样张"]
    fn dump_sample_cards() {
        let Ok(dir) = std::env::var("OAI_CARD_DUMP") else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();
        let sources = [Source {
            title: "一份标题很长、仍需完整展示的参考资料：设计与实现细节".repeat(2),
            url: "https://docs.example.com/reference".into(),
        }];
        let markdown = "## 把复杂问题讲清楚\n\n舒适的阅读从清晰的层次开始。先说明结论，再展开细节，给文字留出呼吸的空间。\n\n- 稳定的留白，让重点更容易被找到。\n- 数字、单位与说明，应当各就各位。\n\n> 好的排版让人专注于内容。\n\n### 配置示例\n\n```rust\nlet message = \"这里是一条完整显示、不需要横向滚动的长消息\";\n```\n\n| 项目 | 原因 | 验证方式 |\n| --- | --- | --- |\n| 图片工作池 | 控制同时计算的任务数量 | 检查取消后的许可释放 |\n| 表格换行 | 静态图片无法滚动 | 检查全部单元格 |\n";
        let normal = build_html(&Card {
            title: "智能回复 · 阅读示例",
            markdown,
            sources: &sources,
            footer: Some(Footer {
                meta: "本地排版样张 · 3.4 秒".into(),
                trace: vec![TraceStep::new("render", "检查图像的完整性与边界")],
                trace_overflow: 0,
            }),
        });
        std::fs::write(format!("{dir}/reply.html"), normal).unwrap();
        let long = "LongToken中文".repeat(24);
        let edge = format!(
            "## 长文本与宽表格\n\n| 很长的第一列表头 | 第二列 | 第三列 | 第四列 | 第五列 |\n|---|---|---|---|---|\n| {long} | {long} | 内容 | 内容 | 完整末列 |\n\n```text\n{long}\n```\n\n<script>window.cardScriptRan=true</script><img src=\"https://example.invalid/tracking.png\" alt=\"外部图片\">"
        );
        std::fs::write(
            format!("{dir}/reply-boundary.html"),
            build_html(&Card {
                title: &long,
                markdown: &edge,
                sources: &sources,
                footer: None,
            }),
        )
        .unwrap();
    }

    /// 真跑一次浏览器截图，确认卡片被完整量到（宽度按 2 倍像素密度出图，
    /// 高度不该退化成占位视口高度）。
    #[tokio::test]
    #[ignore = "需要本地 Chrome/Chromium"]
    async fn renders_a_complete_card() {
        let markdown = "## 标题\n\n正文一段，包含 `行内代码` 与 [链接](https://example.com)。\n\n\
                        - 第一点\n- 第二点\n\n```rust\nfn main() { println!(\"hi\"); }\n```\n\n\
                        | 列 A | 列 B |\n| --- | --- |\n| 1 | 2 |\n";
        let sources = [Source {
            title: "示例来源".into(),
            url: "https://example.com/a".into(),
        }];
        let base64 = render_card(
            Card {
                title: "研究 #1回复",
                markdown,
                sources: &sources,
                footer: Some(Footer {
                    meta: "gpt-5.6-luna · 3.4秒".into(),
                    trace: vec![TraceStep::new("bash", "测试")],
                    trace_overflow: 0,
                }),
            },
            DEVICE_SCALE,
        )
        .await
        .unwrap();

        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&base64)
            .unwrap();
        let image = image::load_from_memory(&bytes).unwrap();
        assert_eq!(image.width(), (f64::from(CARD_WIDTH) * DEVICE_SCALE) as u32);
        // 占位视口是 800，真实卡片必须比它高出一截才说明测量生效。
        assert!(image.height() > 900, "height = {}", image.height());
        std::fs::write(std::env::temp_dir().join("ayjx-card.jpg"), &bytes).unwrap();
        cdp_html_shot::Browser::shutdown_global().await;
    }
}
