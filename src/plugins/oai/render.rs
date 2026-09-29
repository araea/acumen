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

use crate::render::markdown::{self, Rendered, Settings};
use crate::render::web as render;

/// 卡片 CSS 宽度；配合 2 倍像素密度即 960px 位图，在手机聊天窗口里既清晰又不糊。
/// 与 `markdown` 插件的默认卡宽一致：同一套排版，不因为出处不同而两个样子。
const CARD_WIDTH: u32 = 480;
/// 视口留出的左右留白。
const VIEWPORT_WIDTH: u32 = CARD_WIDTH + 40;
/// 设备像素比的默认值。2 倍即 960px 位图，与 480px 的版心配在一起最省体积又不糊；
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
    let rendered = build_pages(&card);
    // 一条回复只出一张图：分页是 `markdown` 插件给「用户要看的长文」用的，
    // 这里超长就退回文本，由调用方按段发送。
    anyhow::ensure!(!rendered.pages.is_empty(), "回复里没有可渲染的内容");
    anyhow::ensure!(
        rendered.total_pages == 1,
        "回复超出单张卡片的高度，改用完整文本"
    );
    // 量高度、等字体、尺寸护栏与并发闸门都在 `render::web::shoot` 一处。
    // 出图范围是默认的 `.shot`：与其余几张卡片同一处边界（卡面 + 一圈相纸）。
    render::shoot(
        render::Shot::new(&rendered.pages[0], VIEWPORT_WIDTH)
            .scale(scale)
            .jpeg(88)
            .max_height(MAX_CARD_HEIGHT),
    )
    .await
}

/// Markdown 的解析、排版与分页都在 [`crate::render::markdown`]，这里只交代这张卡的
/// 眉标、标题与页底附录（来源、工具轨迹）。
fn build_pages(card: &Card<'_>) -> Rendered {
    // 模型与耗时挪到页眉右端。它与页眉左边那句「谁在几点回的谁」是同一类信息
    // （这条回复的出处），放在页脚则在最不显眼处；页脚只留工具轨迹。
    let stamp = card
        .footer
        .as_ref()
        .map(|footer| footer.meta.trim().to_string())
        .unwrap_or_default();
    let settings = Settings {
        width: CARD_WIDTH,
        page_height: MAX_CARD_HEIGHT,
        max_pages: 2,
        kicker: "智能回复".into(),
        kicker_en: "REPLY".into(),
        stamp,
        title: card.title.to_string(),
        footer_html: format!(
            "{}{}",
            render_sources(card.sources),
            card.footer.as_ref().map(render_footer).unwrap_or_default()
        ),
        extra_css: CSS,
        ..Settings::default()
    };
    markdown::render(card.markdown, &settings)
}

#[cfg(test)]
fn build_html(card: &Card<'_>) -> String {
    build_pages(card).pages.remove(0)
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
                escape_html(&source.url),
            )
        })
        .collect::<String>();
    format!(r#"<div class="sources"><div class="src-head">参考来源</div><ol>{items}</ol></div>"#)
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

/// 回复卡自己的版式：只有页底两块附录（参考来源、工具轨迹）。
///
/// 正文的 Markdown 元素、令牌与组件基元在 `res/cards/markdown.css` 与 `m3e.css`，
/// 这里不写色值与字号字面量。附录是另一张纸：通栏铺到卡片边缘、中性面加顶线，
/// 不和正文抢同一张纸的亮度。小字只有两档：`label-small`（轨迹、序号）与
/// `label-medium`（来源条目）。
const CSS: &str = r#"
.sources{padding:var(--md-space-3) var(--md-space-7);
  background:var(--md-sys-color-surface-container-low);
  border-top:1px solid var(--md-sys-color-outline-variant)}
.src-head{font-size:var(--md-type-label-small-size);font-weight:var(--md-type-label-small-weight);
  letter-spacing:var(--md-type-label-small-track);color:var(--md-sys-color-on-surface-variant);
  margin-bottom:7px}
.sources ol{list-style:none;padding:0;margin:0}
.sources li{display:grid;grid-template-columns:20px minmax(0,1fr);align-items:baseline;
  gap:3px var(--md-space-2);margin:var(--md-space-1) 0;padding:0;
  font-size:var(--md-type-label-medium-size);line-height:1.5}
/* 序号走 primary 容器：正文的主色已经用在链接上，序号取它的容器色才分得开 */
.src-idx{flex:none;min-width:17px;height:17px;border-radius:var(--md-shape-xs);
  background:var(--md-sys-color-primary-container);color:var(--md-sys-color-on-primary-container);
  font-size:var(--md-type-label-small-size);font-weight:800;display:flex;align-items:center;justify-content:center}
.src-title{min-width:0;color:var(--md-sys-color-on-surface-variant);overflow-wrap:anywhere}
.src-host{grid-column:2;color:var(--md-sys-color-on-surface-variant);
  font-size:var(--md-type-label-medium-size);overflow-wrap:anywhere}

.foot{margin-top:0;padding:9px var(--md-space-7);
  background:var(--md-sys-color-surface-container-low);
  border-top:1px solid var(--md-sys-color-outline-variant);
  font-size:var(--md-type-label-small-size);color:var(--md-sys-color-on-surface-variant);
  line-height:1.6;overflow-wrap:anywhere}
.trace{margin-top:5px;display:flex;flex-direction:column;gap:3px}
.trace-row{display:flex;align-items:baseline;gap:6px}
.trace-name{flex:none;padding:0 5px;border-radius:var(--md-shape-xs);
  background:var(--md-sys-color-secondary-container);
  color:var(--md-sys-color-on-secondary-container);font-family:var(--md-font-mono);
  font-size:var(--md-type-label-small-size);font-weight:700}
.trace-arg{flex:1;min-width:0;color:var(--md-sys-color-on-surface-variant);
  font-family:var(--md-font-mono);font-size:var(--md-type-label-small-size);line-height:1.5;
  overflow-wrap:anywhere;word-break:break-word}
.trace-rep{flex:none;color:var(--md-sys-color-on-surface-variant);font-size:var(--md-type-label-small-size)}
.trace-more{margin-top:2px;color:var(--md-sys-color-on-surface-variant);font-size:var(--md-type-label-small-size)}
"#;

#[cfg(test)]
mod tests {
    #[test]
    fn model_html_cannot_restyle_or_hide_card() {
        let card = super::Card {
            title: "test",
            markdown: "<style>body{display:none}</style><div style=\"color:red\">content</div>",
            sources: &[],
            footer: None,
        };
        let html = super::build_html(&card);
        assert!(!html.contains("<style>body{display:none}"));
        assert!(!html.contains("<div style=\"color:red\">"));
        assert!(html.contains("&lt;style&gt;"));
    }

    use super::*;

    /// 版式里不许出现 HTML 的成对标签——样式表塞进 `style` 元素时会被截断，
    /// 而页面不会报错（见 [`crate::render::web::assert_embeddable`]）。
    #[test]
    fn stylesheet_stays_embeddable() {
        crate::render::web::assert_embeddable("oai", CSS);
    }
    use crate::plugins::oai::types::{Source, TraceStep};

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
        assert!(html.contains("class=\"code-lang\">Rust<"), "{html}");
        assert!(html.contains("openai.com"), "{html}");
        assert!(
            html.contains("https://www.openai.com/index/a?utm=1"),
            "来源保留完整地址"
        );
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
        // 出图范围是 `.shot`：卡面 520 加两侧各 20 的相纸。
        assert_eq!(
            image.width(),
            (f64::from(VIEWPORT_WIDTH) * DEVICE_SCALE) as u32
        );
        // 占位视口是 800，真实卡片必须比它高出一截才说明测量生效。
        assert!(image.height() > 900, "height = {}", image.height());
        std::fs::write(std::env::temp_dir().join("acumen-card.jpg"), &bytes).unwrap();
        cdp_html_shot::Browser::shutdown_global().await;
    }
}
