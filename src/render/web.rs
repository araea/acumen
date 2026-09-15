//! help / ctl 共用的网页阅读卡片。动态内容始终作为文本转义，不加载外部资源。

use anyhow::{Result, anyhow, ensure};
use cdp_html_shot::{Browser, CaptureOptions, ClipRegion, ImageFormat, Viewport};
use futures_util::FutureExt;
use std::{panic::AssertUnwindSafe, time::Duration};
use tokio::{sync::Semaphore, time::timeout};

pub enum Theme {
    Help,
    Control,
}
pub struct Item {
    pub name: String,
    pub key: String,
    pub desc: String,
    pub on: bool,
}
pub struct Cmd {
    pub prefix: String,
    pub cmd: String,
    pub note: String,
    pub aliases: Vec<String>,
}
pub struct Row {
    pub on: bool,
    pub main: String,
    pub sub: String,
    pub tail: String,
}
pub struct Tile {
    pub value: String,
    pub label: String,
}
pub enum Tone {
    Info,
    Empty,
}
pub enum Block {
    Title {
        title: String,
        pill: Option<(String, bool)>,
        sub: String,
    },
    Meter(Vec<bool>),
    Rule,
    Section {
        title: String,
        en: String,
        count: String,
    },
    /// 条目清单。`cols` 为 1 时逐条竖排；大于 1 时排成多列网格
    /// （目前样式只定义了 `.cols-2`），让「一眼看全」的目录不被撑成一张长图。
    Items {
        items: Vec<Item>,
        cols: usize,
    },
    Cmds(Vec<Cmd>),
    Rows(Vec<Row>),
    Code(Vec<String>),
    Tiles(Vec<Tile>),
    Callout {
        tone: Tone,
        text: String,
    },
}
pub struct Doc {
    pub theme: Theme,
    pub width: f32,
    pub kicker: String,
    pub blocks: Vec<Block>,
    pub foot: String,
    pub hint: (String, String),
}

fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn badge(label: &str, state: &str) -> String {
    format!(
        r#"<span class="badge {state}"><i></i>{}</span>"#,
        esc(label)
    )
}
fn status(on: bool) -> String {
    badge(
        if on { "已启用" } else { "已停用" },
        if on { "on" } else { "off" },
    )
}

pub fn html(doc: &Doc) -> String {
    let mut body = String::new();
    for block in &doc.blocks {
        match block {
            Block::Title { title, pill, sub } => {
                body.push_str(&format!("<header><div class=heading><h1>{}</h1>{}</div><p class=subtitle>{}</p></header>",
                    esc(title), pill.as_ref().map(|(label, on)| badge(label, if *on { "on" } else { "off" })).unwrap_or_default(), esc(sub)));
            }
            Block::Meter(states) => {
                let on = states.iter().filter(|s| **s).count();
                body.push_str(&format!("<div class=summary><span>全部 <b>{}</b></span><span>已启用 <b>{on}</b></span><span>已停用 <b>{}</b></span></div>", states.len(), states.len() - on));
            }
            Block::Rule => body.push_str("<hr>"),
            Block::Section { title, en, count } => body.push_str(&format!(
                "<div class=section><h2>{}</h2><span class=section-en>{}</span><span class=count>{}</span></div>", esc(title), esc(en), esc(count))),
            Block::Items { items, cols } => {
                let cols = (*cols).max(1);
                body.push_str(&format!("<div class=\"items cols-{cols}\">"));
                for item in items {
                    body.push_str(&format!("<article class=item><div class=item-heading><h3>{}</h3><span class=key>{}</span>{}</div><p class=description>{}</p></article>",
                        esc(&item.name), esc(&item.key), status(item.on), esc(&item.desc)));
                }
                body.push_str("</div>");
            }
            Block::Cmds(cmds) => {
                body.push_str("<ol class=commands>");
                for cmd in cmds {
                    body.push_str(&format!("<li><code class=command>{}{}</code>", esc(&cmd.prefix), esc(&cmd.cmd)));
                    if !cmd.note.is_empty() { body.push_str(&format!("<p class=description>{}</p>", esc(&cmd.note))); }
                    if !cmd.aliases.is_empty() {
                        body.push_str("<div class=aliases><span>别名</span>");
                        for alias in &cmd.aliases { body.push_str(&format!("<code>{}</code>", esc(alias))); }
                        body.push_str("</div>");
                    }
                    body.push_str("</li>");
                }
                body.push_str("</ol>");
            }
            Block::Rows(rows) => {
                body.push_str("<div class=status-list>");
                for row in rows {
                    body.push_str(&format!("<div class=status-row><div class=identity><h3>{}</h3><div class=key>{}</div></div><div class=states>{}{}</div></div>",
                        esc(&row.main), esc(&row.sub), status(row.on),
                        if row.tail.is_empty() { String::new() } else { badge(&row.tail, "pending") }));
                }
                body.push_str("</div>");
            }
            Block::Code(lines) => {
                body.push_str("<div class=code-panel>");
                for line in lines {
                    let class = if line.trim().starts_with('[') { "code-line table-key" } else { "code-line" };
                    body.push_str(&format!("<div class=\"{class}\"><code>{}</code></div>", if line.is_empty() { "&#8203;".into() } else { esc(line) }));
                }
                body.push_str("</div>");
            }
            Block::Tiles(tiles) => {
                body.push_str("<div class=tiles>");
                for tile in tiles { body.push_str(&format!("<div><b>{}</b><span>{}</span></div>", esc(&tile.value), esc(&tile.label))); }
                body.push_str("</div>");
            }
            Block::Callout { tone, text } => body.push_str(&format!("<aside class=\"callout {}\">{}</aside>",
                match tone { Tone::Info => "info", Tone::Empty => "empty" }, esc(text))),
        }
    }
    let theme = match doc.theme {
        Theme::Help => "help",
        Theme::Control => "control",
    };
    format!(
        r#"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; font-src data:">
<title>AYJX · {title}</title><style>{css}</style></head>
<body class="{theme}" style="width:{width}px"><main class="shot"><div class="card">
<div class="eyebrow"><span class="brand">AYJX</span><span>{kicker}</span></div>
{body}<footer><div class="next"><span>{hint}</span><code>{command}</code></div><p>{foot}</p></footer>
</div></main></body></html>"#,
        title = esc(&doc.kicker),
        css = include_str!("../../res/cards/reading.css"),
        width = doc.width,
        kicker = esc(&doc.kicker.replace("AYJX · ", "")),
        hint = esc(&doc.hint.0),
        command = esc(&doc.hint.1),
        foot = esc(&doc.foot)
    )
}

/// 网页卡片的并发闸门。同一时刻最多三张卡片在渲染。
///
/// 一张卡片要占几十 MB（页面 + 位图），群聊里同时触发时不能各自开一页。三个是
/// 本机（手机，可用内存约 900 MB）实测安全的档位：再多也只是排队，还挤占截图本身。
///
/// **闸门按「一类工作」划分，不按调用点划分**。此前 help/ctl 一道、oai 一道、
/// ai_news 与 portrait 没有闸门，同一个 Chromium 进程被五套口径同时使唤。现在
/// 只有两道：这一道管**自家生成的卡片**（help / ctl / portrait / ai_news / oai，
/// 页面简单、渲染有 45 秒上限），[`crate::plugins::webshot`] 那道管**真实网页**
/// （不可信内容、可能加载几 MB 资源、预算可到两分钟）。两者混进同一道会让一条
/// 慢网页把一张帮助卡挡住两分钟。
pub(crate) static CARD_GATE: Semaphore = Semaphore::const_new(3);

/// 单张卡片的渲染预算。**排队时间不计入**——闸门在超时之外获取，排在后面的请求
/// 不会因为前面那张慢而被判超时。
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(45);

/// 等网页字体就绪的上限（毫秒）。字体没到位时 CJK 行高会算小，卡片底部被切。
const FONT_WAIT_MS: u32 = 900;

/// 浏览器标签页的清理守卫。
///
/// `cdp_html_shot::Tab` 没有 `Drop`：只有显式调用 `close()` 才会关掉页面。而持有它的
/// future 一旦被 `timeout` 取消（或调用方提前放手），那句 `close()` 就永远执行不到，
/// 页面会一直留在浏览器里。并发一高，攒下的空白页与已加载页面既吃内存，也让后续
/// 截图越来越慢。
///
/// 守卫把关闭挪进 `Drop`：正常返回、报错、被取消三条路都从这里收尾。`close()` 需要
/// await 而 `Drop` 只能同步，所以 `Drop` 里派一个独立任务去关——即便当前 future 正被
/// 取消，关闭照样发生。
pub(crate) struct TabGuard(Option<cdp_html_shot::Tab>);

impl TabGuard {
    pub(crate) fn new(tab: cdp_html_shot::Tab) -> Self {
        Self(Some(tab))
    }

    /// 借出标签页做操作；关闭只经 [`TabGuard::close`] 或 `Drop`。
    pub(crate) fn tab(&self) -> &cdp_html_shot::Tab {
        self.0.as_ref().expect("标签页已被关闭")
    }

    /// 主动关闭。正常情况下走这里，让清理发生在当前任务里。
    pub(crate) async fn close(mut self) {
        if let Some(tab) = self.0.take() {
            let _ = timeout(Duration::from_secs(3), tab.close()).await;
        }
    }
}

impl Drop for TabGuard {
    fn drop(&mut self) {
        let Some(tab) = self.0.take() else { return };
        // `Drop` 不能 await，交给运行时上的独立任务收尾。运行时已在关闭时 spawn 会
        // 失败，那种情形进程也快退了，页面随进程一起消失。
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = timeout(Duration::from_secs(3), tab.close()).await;
            });
        }
    }
}

fn scale_factor(scale: f64) -> f64 {
    if scale.is_finite() {
        scale.clamp(1.0, 4.0)
    } else {
        3.0
    }
}

/// 一张网页卡片的出图参数。
///
/// 出图范围由 [`Shot::selector`] 命中的元素决定：它的外接矩形就是成图边界。
/// 这样调用方只要保证页面里有那一个元素，不必关心视口该开多大、高度怎么量。
pub struct Shot<'a> {
    pub html: &'a str,
    /// 决定出图范围的选择器，必须命中一个元素。
    pub selector: &'a str,
    /// 页面布局宽度（CSS 像素）。文字按它换行，成图宽度也是它。
    pub width: u32,
    pub scale: f64,
    pub format: ImageFormat,
    /// 仅 JPEG / WebP 生效，PNG 忽略。
    pub quality: u8,
    /// 高度上限（CSS 像素），超过就报错交给调用方回退文本。
    pub max_height: f64,
    pub browser_path: Option<&'a str>,
}

impl<'a> Shot<'a> {
    /// 默认出一张 PNG，选择器 `.shot`（本仓库所有卡片的统一约定）。
    pub fn new(html: &'a str, width: u32) -> Self {
        Self {
            html,
            selector: ".shot",
            width,
            scale: 3.0,
            format: ImageFormat::Png,
            quality: 92,
            max_height: 16_000.0,
            browser_path: None,
        }
    }

    pub fn selector(mut self, selector: &'a str) -> Self {
        self.selector = selector;
        self
    }

    pub fn scale(mut self, scale: f64) -> Self {
        self.scale = scale;
        self
    }

    /// 改出 JPEG（体积小一个量级，长报告走这条）。
    pub fn jpeg(mut self, quality: u8) -> Self {
        self.format = ImageFormat::Jpeg;
        self.quality = quality;
        self
    }

    pub fn max_height(mut self, max_height: f64) -> Self {
        self.max_height = max_height;
        self
    }

    pub fn browser(mut self, browser_path: Option<&'a str>) -> Self {
        self.browser_path = browser_path;
        self
    }
}

pub async fn capture(doc: &Doc, scale: f64, browser_path: Option<&str>) -> Result<String> {
    shoot(
        Shot::new(&html(doc), doc.width as u32)
            .scale(scale)
            .browser(browser_path),
    )
    .await
}

/// 把一段自带样式的整页 HTML 截成图片的 base64。
///
/// 本仓库所有网页卡片（help / ctl / 画像 / 资讯 / 智能体回复）都走这一条，只有
/// [`Shot`] 的取值不同。三件事只在这里做一次：
///
/// 1. **量一次就够**。旧写法是「设占位视口 → 注入 → 固定睡 150–320 ms → 量高度 →
///    再设一次视口 → 再睡一觉 → 截元素」，两次固定睡眠与两趟视口往返全花在等一个
///    本来可以观测到的状态上。[`oai::render`](crate::plugins::oai::render) 的写法
///    被证明更稳：把「等字体」和「量盒子」合并成一次 `evaluate`，字体用
///    `document.fonts.ready` 与一个定时器赛跑（headless 下 `requestAnimationFrame`
///    不保证触发，不能拿它等布局），量完直接用 clip 整页截图。少了两次睡眠，
///    也少了「量到的高度偏小、卡片底部被切」这一类偶发问题。
/// 2. **尺寸有护栏**。高度上限与像素预算都在这儿收口，超了返回错误，由调用方回退
///    纯文本，不产出一张截掉一半的图。
/// 3. **闸门在超时之外**。排队不是渲染失败。
pub async fn shoot(shot: Shot<'_>) -> Result<String> {
    let Shot {
        html,
        selector,
        width,
        scale,
        format,
        quality,
        max_height,
        browser_path,
    } = shot;
    let scale = scale_factor(scale);
    // 排队在超时之外：等闸门的时间不算渲染预算，否则高峰期排在后头的必然失败。
    let _permit = CARD_GATE
        .acquire()
        .await
        .map_err(|_| anyhow!("卡片渲染闸门不可用"))?;

    let mut page = None;
    // cdp-html-shot 的全局实例初始化失败会 panic，转换为可回退的普通错误。
    let result = AssertUnwindSafe(timeout(CAPTURE_TIMEOUT, async {
        let browser = match browser_path.filter(|p| !p.is_empty()) {
            Some(path) => Browser::instance_with_path(path).await,
            None => Browser::instance().await,
        };
        page = Some(TabGuard::new(browser.new_tab().await?));
        let tab = page.as_ref().unwrap().tab();
        // 视口只设一次：布局宽度要准，高度由后面的 clip 决定。
        tab.set_viewport(&Viewport::new(width, 600).with_device_scale_factor(scale))
            .await?;
        tab.set_content(html).await?;

        let measured = tab
            .evaluate(&format!(
                r#"(async () => {{
                    const deadline = new Promise(resolve => setTimeout(resolve, {FONT_WAIT_MS}));
                    try {{ await Promise.race([document.fonts.ready, deadline]); }} catch (_) {{}}
                    // 让出一轮宏任务，把上一步的样式与布局提交掉。
                    await new Promise(resolve => setTimeout(resolve, 0));
                    const el = document.querySelector({selector});
                    if (!el) return null;
                    const box = el.getBoundingClientRect();
                    return {{
                        x: box.left + window.scrollX,
                        y: box.top + window.scrollY,
                        width: box.width,
                        height: box.height,
                    }};
                }})()"#,
                selector = serde_json::to_string(selector)?
            ))
            .await?;

        let number = |key: &str| measured.get(key).and_then(|value| value.as_f64());
        let box_width = number("width").ok_or_else(|| anyhow!("无法测量卡片宽度"))?;
        let height = number("height").ok_or_else(|| anyhow!("无法测量卡片高度"))?;
        ensure!(
            box_width.is_finite() && box_width > 1.0,
            "卡片宽度异常，改用完整文本"
        );
        ensure!(
            height.is_finite()
                && height > 0.0
                && height <= max_height
                && box_width * height * scale * scale <= 64_000_000.0,
            "卡片超出安全出图尺寸，改用完整文本"
        );

        let clip = ClipRegion::new(
            number("x").unwrap_or(0.0),
            number("y").unwrap_or(0.0),
            box_width,
            height,
        );
        tab.screenshot(
            CaptureOptions::new()
                .with_format(format)
                .with_quality(quality)
                .with_full_page(true)
                .with_clip(clip),
        )
        .await
    }))
    .catch_unwind()
    .await;

    // 成功、错误和超时均清理页面；不能在 timeout 的 ? 之后才安排清理。
    // 即便整个 future 被外层取消，TabGuard 的 Drop 也会把关闭补上。
    if let Some(guard) = page {
        guard.close().await;
    }
    match result {
        // 内层错误原样往外抛，调用方按文案回退（尺寸超限、量不到盒子等）。
        Ok(Ok(image)) => image,
        Ok(Err(_elapsed)) => Err(anyhow!("网页卡片截图超时（45 秒）")),
        Err(_panic) => Err(anyhow!("浏览器初始化失败")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dynamic_content_is_text_and_long_content_is_complete() {
        let payload = "<script>alert('x')</script> & \"配置\"";
        let long = "超长字段".repeat(500);
        let doc = Doc {
            theme: Theme::Control,
            width: 640.0,
            kicker: payload.into(),
            blocks: vec![
                Block::Code(vec![payload.into(), long.clone()]),
                Block::Cmds(vec![Cmd {
                    prefix: "&".into(),
                    cmd: "help <插件>".into(),
                    note: payload.into(),
                    aliases: vec![payload.into()],
                }]),
                Block::Rows(vec![Row {
                    on: false,
                    main: payload.into(),
                    sub: payload.into(),
                    tail: "待重启".into(),
                }]),
            ],
            foot: payload.into(),
            hint: (payload.into(), payload.into()),
        };
        let out = html(&doc);
        assert!(!out.contains("<script>"));
        assert!(out.contains("&lt;script&gt;"));
        assert!(out.contains("&amp;help &lt;插件&gt;"));
        assert!(out.contains(&long));
        assert!(out.contains("已停用") && out.contains("待重启"));
        assert!(out.contains("default-src 'none'"));
    }
    /// 用真实浏览器覆盖尺寸超限回退与失败后的下一次截图。
    #[tokio::test]
    #[ignore = "需要 Chromium；运行时与其他截图测试串行"]
    async fn oversized_card_fails_without_breaking_the_next_capture() {
        let mut doc = Doc {
            theme: Theme::Help,
            width: 640.0,
            kicker: "TEST".into(),
            blocks: vec![Block::Code(vec!["完整保留内容".into(); 600])],
            foot: "末尾".into(),
            hint: ("返回".into(), "/help".into()),
        };
        let path = std::env::var("CHROME_BIN").ok();
        let error = capture(&doc, 3.0, path.as_deref()).await.unwrap_err();
        assert!(error.to_string().contains("安全出图尺寸"), "{error}");
        doc.blocks = vec![Block::Code(vec!["正常卡片".into()])];
        let image = capture(&doc, 1.0, path.as_deref()).await.unwrap();
        assert!(image.starts_with("iVBOR"));
        Browser::shutdown_global().await;
        let error = capture(&doc, 1.0, Some("/nonexistent/ayjx-test-chromium"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("浏览器初始化失败"), "{error}");
    }

    #[test]
    fn scale_is_bounded_even_for_non_finite_values() {
        assert_eq!(scale_factor(f64::NAN), 3.0);
        assert_eq!(scale_factor(f64::INFINITY), 3.0);
        assert_eq!(scale_factor(0.0), 1.0);
        assert_eq!(scale_factor(9.0), 4.0);
    }

    /// 调试端口上的页面（`type == "page"`）个数。
    async fn page_targets(port: u16) -> usize {
        let targets: serde_json::Value = crate::http::client()
            .get(format!("http://127.0.0.1:{port}/json/list"))
            .send()
            .await
            .expect("浏览器调试端口无响应")
            .json()
            .await
            .expect("调试端口返回的不是 JSON");
        targets
            .as_array()
            .map(|list| {
                list.iter()
                    .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
                    .count()
            })
            .unwrap_or(0)
    }

    /// 守卫析构时必须把页面关掉——future 被 `timeout` 取消时，这是唯一还在生效的
    /// 清理路径。用真实浏览器开一个固定调试端口，直接数页面个数。
    #[tokio::test]
    #[ignore = "需要 Chromium；占用 9222 调试端口"]
    async fn dropping_the_guard_closes_the_page() {
        const PORT: u16 = 9222;
        let browser = cdp_html_shot::Browser::launch_with(
            cdp_html_shot::LaunchOptions::new().arg(format!("--remote-debugging-port={PORT}")),
        )
        .await
        .unwrap();

        let before = page_targets(PORT).await;
        {
            let _guard = TabGuard::new(browser.new_tab().await.unwrap());
            assert_eq!(page_targets(PORT).await, before + 1, "新开的页面应当可见");
        }
        // Drop 里派发的是独立任务，给它一点时间落地。
        let mut after = before + 1;
        for _ in 0..30 {
            after = page_targets(PORT).await;
            if after == before {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(after, before, "守卫析构后页面应当被关掉");

        let _ = browser.close_async().await;
    }
}
