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
/// 设备像素比。2 倍即 1040px 位图，与 520px 的版心配在一起最省体积又不糊。
const DEVICE_SCALE: f64 = 2.0;
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

/// 渲染成 base64 JPEG。
pub(crate) async fn render_card(card: Card<'_>) -> anyhow::Result<String> {
    let html = build_html(&card);
    // 量高度、等字体、尺寸护栏与并发闸门都在 `render::web::shoot` 一处。
    // 出图范围是 `.card`（正文的 20px 留白由 body 提供，不进图）。
    render::shoot(
        render::Shot::new(&html, VIEWPORT_WIDTH)
            .selector(".card")
            .scale(DEVICE_SCALE)
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

    format!(
        r#"<!DOCTYPE html><html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:"><style>{CSS}</style></head>
<body><div class="card"><div class="head">{title}</div><div class="body">{body}</div>{sources}{footer}</div></body></html>"#,
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

fn render_footer(footer: &Footer) -> String {
    let meta = footer.meta.trim();
    if meta.is_empty() && footer.trace.is_empty() {
        return String::new();
    }
    let mut out = String::from(r#"<div class="foot">"#);
    if !meta.is_empty() {
        out.push_str(&format!(
            r#"<div class="foot-meta">{}</div>"#,
            escape_html(meta)
        ));
    }
    if !footer.trace.is_empty() {
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
                out.push_str(&format!(
                    r#"<span class="trace-rep">×{}</span>"#,
                    step.repeats
                ));
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
    }
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

const CSS: &str = r#"
*{box-sizing:border-box;margin:0;padding:0}
body{background:#edf1ed;font-family:-apple-system,BlinkMacSystemFont,"Segoe UI","PingFang SC","Noto Sans CJK SC","Source Han Sans SC","Hiragino Sans GB","Microsoft YaHei",Helvetica,Arial,sans-serif;-webkit-font-smoothing:antialiased;padding:20px}
.card{width:520px;background:#fffefa;border-radius:18px;overflow:hidden;box-shadow:0 2px 14px rgba(15,23,42,.08)}
.head{padding:16px 24px;background:#f3f6f2;border-bottom:1px solid #e6eaf0;font-size:14px;font-weight:600;color:#52685f;letter-spacing:.02em}
.body{padding:22px 24px 24px;font-size:17px;line-height:1.82;color:#24292f;word-wrap:break-word;overflow-wrap:anywhere}
.body>*:first-child{margin-top:0}
.body>*:last-child{margin-bottom:0}
p{margin:10px 0}
h1,h2,h3,h4{margin:20px 0 10px;font-weight:650;line-height:1.45;color:#0f172a}
h1{font-size:25px;padding-bottom:8px;border-bottom:2px solid #eef1f5}
h2{font-size:21px;padding-left:9px;border-left:4px solid #327763}
h3{font-size:18px;color:#1e293b}
h4{font-size:17px;color:#334155}
ul,ol{margin:10px 0;padding-left:22px}
li{margin:5px 0}
li>p{margin:4px 0}
li::marker{color:#68778a}
input[type=checkbox]{margin-right:6px;accent-color:#327763}
strong{font-weight:650;color:#0f172a}
em{color:#334155}
del{color:#68778a}
a{color:#2563eb;text-decoration:none;border-bottom:1px solid #bfdbfe}
code{padding:1.5px 5px;background:#f1f5f9;border-radius:5px;font-family:"SF Mono",Consolas,"Liberation Mono",Menlo,monospace;font-size:13px;color:#be185d}
pre{position:relative;margin:12px 0;padding:13px 14px;background:#161b22;border-radius:9px;overflow-wrap:anywhere}
pre[data-lang]{padding-top:26px}
pre[data-lang]::before{content:attr(data-lang);position:absolute;top:6px;left:14px;font-size:10.5px;letter-spacing:.06em;text-transform:uppercase;color:#b3bdc8}
pre code{display:block;padding:0;background:none;color:#e6edf3;font-size:14px;line-height:1.75;white-space:pre-wrap;word-break:break-word}
blockquote{margin:12px 0;padding:8px 12px;background:#f8fafc;border-left:3px solid #cbd5e1;border-radius:0 6px 6px 0;color:#475569}
blockquote p{margin:4px 0}
table{width:100%;margin:12px 0;border-collapse:collapse;font-size:14px;table-layout:fixed;overflow-wrap:anywhere}
th,td{padding:10px 8px;vertical-align:top;border:1px solid #e2e8f0;text-align:left}
th{background:#f1f5f9;font-weight:650;color:#334155;white-space:normal}
tr:nth-child(2n) td{background:#fafbfc}
hr{margin:16px 0;border:none;border-top:1px solid #eef1f5}
img{max-width:100%;height:auto;margin:8px 0;border-radius:8px}
.footnote-definition{margin:6px 0;font-size:12.5px;color:#64748b}
.footnote-definition p{display:inline;margin:0}
.sources{padding:12px 18px;background:#f8fafc;border-top:1px solid #eef1f5}
.src-head{font-size:11.5px;font-weight:650;color:#68778a;letter-spacing:.08em;margin-bottom:7px}
.sources ol{list-style:none;padding:0;margin:0}
.sources li{display:grid;grid-template-columns:20px minmax(0,1fr);align-items:baseline;gap:3px 8px;margin:4px 0;font-size:12.5px;line-height:1.5}
.src-idx{flex:none;min-width:17px;height:17px;border-radius:5px;background:#e0e7ff;color:#4338ca;font-size:10px;font-weight:700;display:flex;align-items:center;justify-content:center}
.src-title{min-width:0;color:#334155;overflow-wrap:anywhere}
.src-host{grid-column:2;color:#68778a;font-size:12px;overflow-wrap:anywhere}
.foot{padding:9px 18px;background:#f8fafc;border-top:1px solid #eef1f5;font-size:11px;color:#68778a;line-height:1.6;overflow-wrap:anywhere}
.foot-meta{font-weight:600;letter-spacing:.01em}
.trace{margin-top:5px;display:flex;flex-direction:column;gap:3px}
.trace-row{display:flex;align-items:baseline;gap:6px}
.trace-name{flex:none;padding:0 5px;border-radius:4px;background:#eef2ff;color:#6366f1;font-family:"SF Mono",Consolas,Menlo,monospace;font-size:10px;font-weight:650}
.trace-arg{flex:1;min-width:0;color:#68778a;font-family:"SF Mono",Consolas,Menlo,monospace;font-size:10.5px;line-height:1.5;overflow-wrap:anywhere;word-break:break-word}
.trace-rep{flex:none;color:#68778a;font-size:10px}
.trace-more{margin-top:2px;color:#68778a;font-size:10.5px}
/* 智能体与模型清单用的紧凑卡片；这些片段以裸 HTML 形式嵌在 Markdown 里。 */
.agent-card{margin:10px 0;padding:12px;background:#f8fafc;border:1px solid #eef1f5;border-radius:9px}
.agent-name{margin-bottom:7px;font-size:15px;font-weight:650;color:#0f172a}
.agent-info{font-size:12.5px;line-height:1.85;color:#64748b}
.agent-info code{font-size:11.5px}
.model-group{margin-bottom:15px;break-inside:avoid}
.model-header{display:flex;align-items:center;justify-content:space-between;margin-bottom:8px;padding:6px 10px;border-left:3px solid #327763;border-radius:6px;background:#f1f5f9;font-size:12.5px;font-weight:650;color:#334155}
.model-count{padding:1px 6px;border-radius:4px;background:#e2e8f0;color:#64748b;font-size:10.5px}
.agent-grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:7px}
.agent-mini{padding:8px;border:1px solid #eef1f5;border-radius:7px;background:#fff}
.agent-mini-top{display:flex;align-items:center;margin-bottom:3px}
.agent-idx{flex:none;display:flex;align-items:center;justify-content:center;min-width:18px;height:18px;margin-right:6px;border-radius:5px;background:#e0e7ff;color:#4338ca;font-size:10px;font-weight:700}
.agent-mini-name{overflow:hidden;white-space:nowrap;text-overflow:ellipsis;font-size:13.5px;font-weight:600;color:#1e293b}
.agent-mini-desc{overflow:hidden;white-space:nowrap;text-overflow:ellipsis;font-size:11px;color:#68778a}
.mod-group{margin-bottom:15px;break-inside:avoid}
.mod-title{margin-bottom:8px;padding-left:7px;border-left:3px solid #327763;font-size:12.5px;font-weight:700;letter-spacing:.05em;color:#475569;text-transform:uppercase}
.chip-box,.chip-container{display:flex;flex-wrap:wrap;gap:7px}
.chip{display:flex;align-items:center;padding:5px 9px;border:1px solid #e2e8f0;border-radius:7px;background:#fff;font-size:12.5px;color:#334155}
.chip-idx{margin-right:7px;padding:1px 5px;border-radius:4px;background:#f1f5f9;color:#68778a;font-family:"SF Mono",Consolas,monospace;font-size:10.5px;font-weight:650}
.chip-name{font-weight:500}
.chip-bad,.chip-badge{margin-left:7px;padding:1px 6px;border-radius:9px;background:#e0e7ff;color:#4338ca;font-size:10px;font-weight:650}
.provider-section{margin-bottom:18px;break-inside:avoid}
.provider-title{margin-bottom:8px;padding-left:6px;border-left:3px solid #94a3b8;font-size:13px;font-weight:700;color:#475569}
.head,.agent-mini,.chip,.trace-name{min-width:0;overflow-wrap:anywhere}
.chip{flex-wrap:wrap;gap:4px}
.body p{text-wrap:pretty}
h1,h2,h3,h4{text-wrap:balance}

"#;

#[cfg(test)]
mod tests {
    use super::*;
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
        let base64 = render_card(Card {
            title: "研究 #1回复",
            markdown,
            sources: &sources,
            footer: Some(Footer {
                meta: "gpt-5.6-luna · 3.4秒".into(),
                trace: vec![TraceStep::new("bash", "测试")],
                trace_overflow: 0,
            }),
        })
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
