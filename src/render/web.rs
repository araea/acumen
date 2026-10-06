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

/// 卡片设计系统（令牌与组件基元）。
///
/// 本仓库四种卡片图共用这一份样式：手册、控制、回复、资讯（日读／夜读）。
/// 各卡的版式文件只写「摆在哪儿」，色值、字号、圆角、
/// 阴影一律从这里的 `--md-*` 令牌取——两层的分工与三条刻意偏离都写在
/// 这个文件的开头，改版式前先读那一段。
///
/// 用它拼 `<style>` 时顺序不能换，版式在后：
/// `format!("{}{}", render::DESIGN_SYSTEM, 本卡版式)`。
pub(crate) const DESIGN_SYSTEM: &str = concat!(
    include_str!("../../res/cards/tokens.css"),
    "\n",
    include_str!("../../res/cards/m3e.css")
);

/// 总览里的一个插件条目。
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
/// 状态：圆点的形状与颜色、读数格的底色都由它定。
///
/// 颜色从来不单独传信息——同一处总有文字（分组标题、徽章、读数标签）说出同一件事，
/// 圆点另外用实心 / 空心 / 带环三种形状区分。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum State {
    On,
    Off,
    Pending,
    /// 中性：不代表任何状态，只是一个数。
    Plain,
}
impl State {
    fn class(self) -> &'static str {
        match self {
            State::On => "on",
            State::Off => "off",
            State::Pending => "pending",
            State::Plain => "plain",
        }
    }
}
/// 状态清单里的一格：名称、配置键与状态。
pub struct Cell {
    pub main: String,
    pub sub: String,
    pub state: State,
}
pub struct Tile {
    pub value: String,
    pub label: String,
    pub state: State,
}
/// 「标签 — 指令」的一行，用于「管理」这类告诉读者下一步怎么打的清单。
pub struct Fact {
    pub label: String,
    pub command: String,
}
/// 配置差异的一项。`old` 为空表示默认值里没有这一项（额外项）。
pub struct DiffRow {
    pub key: String,
    pub old: Option<String>,
    pub new: String,
}
/// 房间列表的一行：名称、简介，以及要不要挂「联网」与所用模型。
pub struct RoomRow {
    pub name: String,
    pub desc: String,
    pub search: bool,
    /// 分区标题里没写模型的行才自己带一行模型。
    pub model: Option<String>,
}
/// 模型列表的一行：序号（可直接当指令里的模型写）、名称与几枚标签。
pub struct ModelRow {
    pub index: usize,
    pub name: String,
    pub vendor: Option<String>,
    pub used: usize,
    pub default: bool,
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
    /// 标题下的一段导语。
    Lead(String),
    /// 一排读数格。
    Tiles(Vec<Tile>),
    Rule,
    Section {
        title: String,
        en: String,
        count: String,
    },
    /// 分区标题的另一种写法：右边不是英文代号，而是一枚等宽芯片（模型名）。
    SectionChip {
        title: String,
        chip: String,
        count: String,
    },
    /// 插件条目：分段列表，名称、配置键与简介；停用的才挂徽章。
    Items(Vec<Item>),
    /// 房间列表：名称一列、简介一列，对着读。
    Rooms(Vec<RoomRow>),
    /// 模型列表：序号、名称、标签。
    Models(Vec<ModelRow>),
    Cmds(Vec<Cmd>),
    /// 状态清单：两列并排的小格，一格一个插件。
    Grid(Vec<Cell>),
    /// 等宽面板。`lang` 给了就按该语言着色（认不出则原样转义）。
    Code {
        lang: Option<&'static str>,
        lines: Vec<String>,
    },
    Diff(Vec<DiffRow>),
    Facts(Vec<Fact>),
    /// 几条并列的说明，每条一个圆点。
    Notes(Vec<String>),
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

/// 动态内容进 HTML 之前一律先过这里：卡片的文字、属性值都靠它转义。
pub(crate) fn esc(text: &str) -> String {
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
fn badge(label: &str, state: &str) -> String {
    format!(
        r#"<span class="md-badge md-badge-{state}"><i></i>{}</span>"#,
        esc(label)
    )
}

/// 出图时刻，落在页眉右端。
///
/// 四张卡的页眉同形：左边「品牌 + 这张卡是什么」，右边「什么时候出的」。
/// 手册与控制这两张从前右边空着——补上时刻是为了让「开关状态以当前配置为准」
/// 这句有个可核对的落点：图是什么时候出的，一眼就知道该不该重新查一遍。
fn stamp() -> String {
    crate::clock::beijing_now()
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

/// 页眉右侧的眉标。调用方传的是完整眉标（`ACUMEN · MANUAL`），这里剥掉品牌前缀，
/// 品牌由版式固定写在左边，眉标只留后面那截。
fn kicker_en(kicker: &str) -> &str {
    kicker
        .strip_prefix("ACUMEN · ")
        .or_else(|| kicker.strip_prefix("ACUMEN·"))
        .unwrap_or(kicker)
        .trim()
}

/// 指令写成 HTML：`<名称>`、`[路径]` 这类占位符与照打的字分两种样式。
///
/// 一条指令里哪些字要原样敲、哪些要换成自己的内容，是读指令时最先要分清的事；
/// 全写成一个颜色，读者得回头去数尖括号。占位符降一档、字重放轻，照打的部分
/// 保持主色加粗。前缀（`/`）本身就是要打的，算照打的一部分。
fn command_html(prefix: &str, cmd: &str) -> String {
    let mut out = String::new();
    let mut literal = String::from(prefix);
    let flush = |literal: &mut String, out: &mut String| {
        if !literal.is_empty() {
            out.push_str(&esc(literal));
            literal.clear();
        }
    };
    let mut chars = cmd.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        let close = match ch {
            '<' => '>',
            '[' => ']',
            _ => {
                literal.push(ch);
                continue;
            }
        };
        // 找不到收尾的括号就当普通字符，不吞掉后面的内容。
        let Some(len) = cmd[start..].find(close) else {
            literal.push(ch);
            continue;
        };
        flush(&mut literal, &mut out);
        let end = start + len + close.len_utf8();
        out.push_str(&format!(
            r#"<span class="ph">{}</span>"#,
            esc(&cmd[start..end])
        ));
        while chars.peek().is_some_and(|(i, _)| *i < end) {
            chars.next();
        }
    }
    flush(&mut literal, &mut out);
    out
}

/// 逐行着色后的面板内容。着色器认得的语言走 `tk-*`，其余原样转义；
/// 每行单独成块，折行时才能悬挂缩进。
fn code_lines(lang: Option<&str>, lines: &[String]) -> String {
    let joined = lines.join("\n");
    let colored = lang
        .and_then(|lang| crate::render::markdown::highlight::highlight(lang, &joined))
        .unwrap_or_else(|| esc(&joined));
    colored
        .split('\n')
        .map(|line| {
            let line = if line.is_empty() { "&#8203;" } else { line };
            format!("<span class=\"code-line\">{line}</span>")
        })
        .collect()
}

pub fn html(doc: &Doc) -> String {
    let mut body = String::new();
    for block in &doc.blocks {
        match block {
            Block::Title { title, pill, sub } => {
                body.push_str(&format!("<header class=head><div class=heading><h1 class=\"md-title md-type-headline-large md-balance\">{}</h1>{}</div><p class=\"md-subtitle md-type-title-small\">{}</p></header>",
                    esc(title), pill.as_ref().map(|(label, on)| badge(label, if *on { "on" } else { "off" })).unwrap_or_default(), esc(sub)));
            }
            Block::Lead(text) => body.push_str(&format!("<p class=\"lead md-type-body-large\">{}</p>", esc(text))),
            Block::Tiles(tiles) => {
                body.push_str("<div class=\"tiles md-tiles\">");
                for tile in tiles {
                    body.push_str(&format!("<div class=\"md-tile tile-{}\"><b>{}</b><span>{}</span></div>", tile.state.class(), esc(&tile.value), esc(&tile.label)));
                }
                body.push_str("</div>");
            }
            Block::Rule => body.push_str("<hr class=\"md-divider\">"),
            Block::Section { title, en, count } => body.push_str(&format!(
                "<div class=\"section md-section\"><h2 class=\"md-title md-type-title-medium md-balance\">{}</h2>{}{}</div>",
                esc(title),
                if en.is_empty() { String::new() } else { format!("<span class=md-section-en>{}</span>", esc(en)) },
                if count.is_empty() { String::new() } else { format!("<span class=md-count>{}</span>", esc(count)) })),
            Block::SectionChip { title, chip, count } => body.push_str(&format!(
                "<div class=\"section md-section\"><h2 class=\"md-title md-type-title-medium md-balance\">{}</h2><code class=section-chip>{}</code><span class=md-count>{}</span></div>",
                esc(title), esc(chip), esc(count))),
            Block::Rooms(rooms) => {
                body.push_str("<ul class=\"md-seg rooms\">");
                for room in rooms {
                    body.push_str(&format!("<li><div class=room-name><h3>{}</h3>{}</div><div class=room-body><p class=room-desc>{}</p>{}</div></li>",
                        esc(&room.name),
                        if room.search { "<span class=room-tag>联网</span>" } else { "" },
                        esc(&room.desc),
                        room.model.as_ref().map(|m| format!("<code class=room-model>{}</code>", esc(m))).unwrap_or_default()));
                }
                body.push_str("</ul>");
            }
            Block::Models(models) => {
                body.push_str("<ul class=\"md-seg models\">");
                for model in models {
                    let mut tags = String::new();
                    if model.default { tags.push_str("<span class=\"model-tag tag-default\">默认</span>"); }
                    if let Some(vendor) = &model.vendor { tags.push_str(&format!("<span class=\"model-tag\">{}</span>", esc(vendor))); }
                    if model.used > 0 { tags.push_str(&format!("<span class=\"model-tag tag-used\">{} 个智能体使用</span>", model.used)); }
                    body.push_str(&format!("<li><span class=model-index>{}</span><span class=model-name>{}</span>{}</li>",
                        model.index, esc(&model.name),
                        if tags.is_empty() { String::new() } else { format!("<span class=model-tags>{tags}</span>") }));
                }
                body.push_str("</ul>");
            }
            Block::Items(items) => {
                body.push_str("<ul class=\"md-seg items\">");
                for item in items {
                    body.push_str(&format!("<li><div class=item-head><h3 class=item-name>{}</h3><code class=item-key>{}</code>{}</div><p class=item-desc>{}</p></li>",
                        esc(&item.name), esc(&item.key),
                        if item.on { String::new() } else { badge("已停用", "off") },
                        esc(&item.desc)));
                }
                body.push_str("</ul>");
            }
            Block::Cmds(cmds) => {
                body.push_str("<ol class=\"md-seg commands\">");
                for cmd in cmds {
                    body.push_str(&format!("<li><code class=command>{}</code>", command_html(&cmd.prefix, &cmd.cmd)));
                    if !cmd.note.is_empty() { body.push_str(&format!("<p class=item-desc>{}</p>", esc(&cmd.note))); }
                    if !cmd.aliases.is_empty() {
                        body.push_str("<div class=aliases><span>别名</span>");
                        for alias in &cmd.aliases { body.push_str(&format!("<code>{}</code>", esc(alias))); }
                        body.push_str("</div>");
                    }
                    body.push_str("</li>");
                }
                body.push_str("</ol>");
            }
            Block::Grid(cells) => {
                body.push_str("<ul class=cells>");
                for cell in cells {
                    body.push_str(&format!("<li class=\"cell cell-{}\"><i class=cell-dot></i><div class=cell-text><h3 class=cell-main>{}</h3><code class=cell-sub>{}</code></div></li>",
                        cell.state.class(), esc(&cell.main), esc(&cell.sub)));
                }
                body.push_str("</ul>");
            }
            Block::Code { lang, lines } => {
                body.push_str(&format!("<div class=code-panel>{}</div>", code_lines(*lang, lines)));
            }
            Block::Diff(rows) => {
                body.push_str("<ul class=\"md-seg diffs\">");
                for row in rows {
                    body.push_str(&format!("<li><code class=diff-key>{}</code>", esc(&row.key)));
                    if let Some(old) = &row.old {
                        body.push_str(&format!(
                            "<div class=\"diff-line diff-old\"><span class=diff-tag>默认</span><code>{}</code></div>", esc(old)));
                    }
                    body.push_str(&format!(
                        "<div class=\"diff-line diff-new\"><span class=diff-tag>{}</span><code>{}</code></div></li>",
                        if row.old.is_some() { "当前" } else { "额外" }, esc(&row.new)));
                }
                body.push_str("</ul>");
            }
            Block::Facts(facts) => {
                body.push_str("<dl class=facts>");
                for fact in facts {
                    body.push_str(&format!("<div class=fact><dt>{}</dt><dd><code>{}</code></dd></div>", esc(&fact.label), esc(&fact.command)));
                }
                body.push_str("</dl>");
            }
            Block::Notes(notes) => {
                body.push_str("<aside class=\"callout md-callout notes\"><ul>");
                for note in notes { body.push_str(&format!("<li>{}</li>", esc(note))); }
                body.push_str("</ul></aside>");
            }
            Block::Callout { tone, text } => body.push_str(&format!("<aside class=\"callout md-callout md-type-body-medium {}\">{}</aside>",
                match tone { Tone::Info => "info", Tone::Empty => "md-callout-empty" }, esc(text))),
        }
    }
    let scheme = match doc.theme {
        Theme::Help => "scheme-manual",
        Theme::Control => "scheme-control",
    };
    format!(
        r#"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; font-src data:">
<title>ACUMEN · {title}</title><style>{system}{css}</style></head>
<body class="{scheme} md-text" style="width:{width}px"><main class=shot><div class="card md-card">
<div class=md-eyebrow><div class=md-kicker><span class=md-dot></span>ACUMEN<span class=md-kicker-en>{kicker}</span></div><span class=md-stamp>{stamp}</span></div>
{body}<footer class=md-foot><div class=md-hint><span>{hint}</span><code>{command}</code></div><p class=md-note>{foot}</p></footer>
</div></main></body></html>"#,
        system = DESIGN_SYSTEM,
        title = esc(&doc.kicker),
        css = include_str!("../../res/cards/reading.css"),
        width = doc.width,
        kicker = esc(kicker_en(&doc.kicker)),
        stamp = esc(&stamp()),
        hint = esc(&doc.hint.0),
        command = esc(&doc.hint.1),
        foot = esc(&doc.foot)
    )
}

/// 断言一张样式表能安全地塞进 `style` 元素里。
///
/// 样式表是塞在 `style` 元素里的，HTML 的 raw text 解析遇到闭合标签就结束——
/// 如果哪份注释里写了一个完整的闭合标签，整张样式表会被截成半句话，页面
/// **不报错**、只是静悄悄退回无样式。这个坑踩过一次：整批卡片全部变成裸 HTML，
/// 而出图的宽度与溢出检查还是全绿。
///
/// 供各卡自己的测试模块调用（它们的版式常量是模块私有的）。
/// 网页卡片的并发闸门。同一时刻最多三张卡片在渲染。
///
/// 一张卡片要占几十 MB（页面 + 位图），群聊里同时触发时不能各自开一页。三个是
/// 本机（手机，可用内存约 900 MB）实测安全的档位：再多也只是排队，还挤占截图本身。
///
/// **闸门按「一类工作」划分，不按调用点划分**。此前 help/ctl 一道、oai 一道、
/// ai_news 没有闸门，同一个 Chromium 进程被四套口径同时使唤。现在
/// 只有两道：这一道管**自家生成的卡片**（help / ctl / ai_news / oai，
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
        if let Some(tab) = self.0.as_ref() {
            let _ = timeout(Duration::from_secs(3), tab.close()).await;
            // await 期间仍保留所有权，外层取消 close 时 Drop 才能继续清理。
            self.0.take();
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
/// 本仓库所有网页卡片（help / ctl / 资讯 / 智能体回复）都走这一条，只有
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
                r"(async () => {{
                    const deadline = new Promise(resolve => setTimeout(resolve, {FONT_WAIT_MS}));
                    const assets = Promise.all([
                        document.fonts.ready,
                        ...Array.from(document.images, img => img.decode().catch(() => {{}}))
                    ]);
                    try {{ await Promise.race([assets, deadline]); }} catch (_) {{}}
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
                }})()",
                selector = serde_json::to_string(selector)?
            ))
            .await?;

        let number = |key: &str| measured.get(key).and_then(sea_orm::JsonValue::as_f64);
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
