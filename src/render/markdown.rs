//! Markdown → 卡片页 HTML，`markdown` 插件专用。`oai` 的回复卡有自己的渲染与取舍
//! （单张卡、来源与工具轨迹附录），两边职责不同，不共用。
//!
//! 这一层只做「文本 → 若干页 HTML」，不碰浏览器与消息：解析、分块、估高、分页、
//! 拼页全在这里，所以可以脱离机器人单测，也可以把每页 HTML 存下来肉眼核对。
//!
//! **安全**：卡片里没有任何来自用户的标记。原始 HTML 一律当文本转义；链接与图片
//! 都不生成 `href` / `src`——页面是静态位图，读者点不了，浏览器也不该去取用户指定的地址
//! （那等于让机器人替群友访问内网）。图片留一个带替代文字的占位，链接在页底列出完整地址。
//!
//! **分页**：聊天里一张几万像素的长图既难读也难发。这里按块（段落、代码、表、列表项）
//! 估高，超出 `page_height` 就另起一页；标题不落在页尾，过长的代码块、表格与列表在块内按行、
//! 按行、按项拆开（表头随页重复）。估高是估的，只决定「在哪里断页」，不影响排版本身。

mod highlight;

use crate::render::web::{DESIGN_SYSTEM, esc};
use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, LinkType, MetadataBlockKind,
    OffsetIter, Options, Parser, Tag, TagEnd,
};
use std::collections::HashMap;
use std::iter::Peekable;
use std::ops::Range;

/// 版式表。令牌与组件基元在 `res/cards/{tokens,m3e}.css`（`DESIGN_SYSTEM`），
/// 这里只写 Markdown 元素的位置与样式，不写色值、字号与圆角的字面量。
pub(crate) const CSS: &str = include_str!("../../res/cards/markdown.css");

/// 卡面左右内边距（与 `markdown.css` 的 `.card` 一致，估高要用）。
const PAD: f64 = 28.0;
/// 设计基准字号：`tokens.css` 把 `1rem` 定在 20px，版面上的字号都按它标定。
/// `Settings::font_size` 改的是这个基准，估高把版面上的字号按 `font_size / 20`
/// 缩放；间距令牌是 px 定值，不跟着缩，所以字越小屏上越密。
const BASE_FONT: f64 = 20.0;
/// 正文行高倍数，与 `markdown.css` 的 `.card` 保持一致（估高要用）。
const LINE_RATIO: f64 = 1.6;
/// 块与块之间的间距（`--md-space-3`），与各块自己的下边距一致。
const GAP: f64 = 12.0;

pub struct Settings {
    /// 卡面宽度（CSS 像素）。
    pub width: u32,
    /// 正文字号（CSS 像素）。整套 `rem` 字阶跟着它缩放，间距令牌不缩：
    /// 卡片要在手机上全屏看，字号相对卡宽越小、每行塞得下的字越多、扫读越快。
    pub font_size: f64,
    pub dark: bool,
    /// 单个换行按换行显示（聊天里的写法），而不是按 CommonMark 合成一个空格。
    pub keep_breaks: bool,
    /// 每页的目标高度（CSS 像素）。软上限：块不拆开时可以略超。
    pub page_height: f64,
    pub max_pages: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            width: 560,
            font_size: 18.0,
            dark: false,
            keep_breaks: true,
            page_height: 2000.0,
            max_pages: 6,
        }
    }
}

pub struct Rendered {
    /// 每页一份完整的 HTML 文档，至多 `max_pages` 页。
    pub pages: Vec<String>,
    /// 分完页的总页数（超出 `max_pages` 时大于 `pages.len()`）。
    pub total_pages: usize,
}

// ==================== 预处理 ====================

/// 统一换行、去掉 BOM 与首尾空行；整段被一个 ```markdown 围栏包着时脱掉围栏
/// （群友常把要渲染的文本整段包在代码块里发，为的是防止聊天软件吃掉星号）。
pub fn normalize(source: &str) -> String {
    let text = source.replace("\r\n", "\n").replace('\r', "\n");
    let text = text.trim_start_matches('\u{feff}');
    let text = text.trim_start_matches(['\n']).trim_end();
    unwrap_fence(text).unwrap_or(text).to_string()
}

fn unwrap_fence(text: &str) -> Option<&str> {
    let (first, rest) = text.split_once('\n')?;
    let first = first.trim_end();
    let fence_char = first.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let fence_len = first.chars().take_while(|c| *c == fence_char).count();
    if fence_len < 3 {
        return None;
    }
    let info = first[fence_len..].trim();
    if !matches!(info.to_ascii_lowercase().as_str(), "markdown" | "md") {
        return None;
    }
    let (body, last) = rest.rsplit_once('\n').unwrap_or(("", rest));
    let last = last.trim();
    let close_len = last.chars().take_while(|c| *c == fence_char).count();
    if close_len < fence_len || last.chars().any(|c| c != fence_char) {
        return None;
    }
    Some(body)
}

// ==================== 度量 ====================

/// 一个字符占几个 em。只用来估高：CJK 与全角 1，拉丁字母约 0.55，emoji 略宽。
fn em_width(c: char) -> f64 {
    match c {
        ' ' => 0.3,
        'i' | 'l' | 'j' | '.' | ',' | ':' | ';' | '\'' | '!' | '|' | '`' => 0.3,
        'm' | 'w' | 'M' | 'W' => 0.85,
        'A'..='Z' => 0.66,
        '0'..='9' => 0.58,
        c if c.is_ascii() => 0.55,
        '\u{2018}'..='\u{201f}' | '\u{2014}' | '\u{2026}' => 1.0,
        '\u{1f000}'..='\u{1faff}' | '\u{2600}'..='\u{27bf}' => 1.15,
        '\u{2e80}'..='\u{9fff}'
        | '\u{ac00}'..='\u{d7af}'
        | '\u{f900}'..='\u{faff}'
        | '\u{fe30}'..='\u{fe4f}'
        | '\u{ff00}'..='\u{ffef}' => 1.0,
        _ => 0.6,
    }
}

fn text_em(text: &str) -> f64 {
    text.chars().map(em_width).sum()
}

/// 行内内容：HTML、每个硬行的宽度（em）、用到的链接与脚注编号。
#[derive(Default)]
struct Inl {
    html: String,
    lines: Vec<f64>,
    links: Vec<usize>,
    notes: Vec<usize>,
    /// 当前不含空白、汉字的连续拉丁串宽度，与见过的最长者（em）。
    /// 表格用它判断列宽够不够放下整词，够就不在词中间断行。
    run: f64,
    token: f64,
}

impl Inl {
    fn new() -> Self {
        Self {
            lines: vec![0.0],
            ..Self::default()
        }
    }

    fn text(&mut self, text: &str) {
        self.html.push_str(&esc(text));
        *self.lines.last_mut().unwrap() += text_em(text);
        for c in text.chars() {
            if c.is_ascii() && !c.is_whitespace() {
                self.run += em_width(c);
                self.token = self.token.max(self.run);
            } else {
                self.run = 0.0;
            }
        }
    }

    fn raw(&mut self, html: &str, em: f64) {
        self.html.push_str(html);
        *self.lines.last_mut().unwrap() += em;
        self.token = self.token.max(em);
    }

    fn line_break(&mut self) {
        self.html.push_str("<br>");
        self.lines.push(0.0);
    }

    /// 并入子行内内容（强调、链接的内部）。子内容的首行接在当前行后面。
    fn absorb(&mut self, open: &str, sub: Inl, close: &str) {
        self.html.push_str(open);
        self.html.push_str(&sub.html);
        self.html.push_str(close);
        let mut lines = sub.lines.into_iter();
        if let Some(first) = lines.next() {
            *self.lines.last_mut().unwrap() += first;
        }
        self.lines.extend(lines);
        self.links.extend(sub.links);
        self.notes.extend(sub.notes);
        self.token = self.token.max(sub.token);
    }

    fn is_empty(&self) -> bool {
        self.html.trim().is_empty()
    }

    /// 排到 `avail` 像素宽时占的高度。
    fn height(&self, font: f64, line: f64, avail: f64) -> f64 {
        self.lines
            .iter()
            .map(|em| (em * font / avail.max(40.0)).ceil().max(1.0))
            .sum::<f64>()
            * line
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Heading,
    Body,
}

struct Block {
    html: String,
    /// 估算高度（含下边距）。
    h: f64,
    kind: Kind,
    links: Vec<usize>,
    notes: Vec<usize>,
}

impl Block {
    fn body(html: String, h: f64) -> Self {
        Self {
            html,
            h,
            kind: Kind::Body,
            links: Vec::new(),
            notes: Vec::new(),
        }
    }

    fn with_refs(mut self, inl: &Inl) -> Self {
        self.links = inl.links.clone();
        self.notes = inl.notes.clone();
        self
    }
}

type Events<'a> = Peekable<OffsetIter<'a>>;

// ==================== 解析与分块 ====================

struct Front {
    title: String,
    meta: Vec<String>,
}

struct Doc<'a> {
    src: &'a str,
    s: &'a Settings,
    links: Vec<String>,
    /// 脚注标签，按首次引用的顺序。
    notes: Vec<String>,
    note_defs: HashMap<String, String>,
}

fn is_inline(event: &Event) -> bool {
    matches!(
        event,
        Event::Text(_)
            | Event::Code(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_)
            | Event::InlineHtml(_)
            | Event::SoftBreak
            | Event::HardBreak
            | Event::FootnoteReference(_)
            | Event::Start(
                Tag::Emphasis
                    | Tag::Strong
                    | Tag::Strikethrough
                    | Tag::Superscript
                    | Tag::Subscript
                    | Tag::Link { .. }
                    | Tag::Image { .. }
            )
    )
}

impl<'a> Doc<'a> {
    fn avail(&self, inset: f64) -> f64 {
        self.s.width as f64 - PAD * 2.0 - inset
    }

    /// 字号相对设计基准的倍率：版面上的 `rem` 字号（标题、代码、表格、页脚）
    /// 都乘它，估高因此与实际排出来的高度同步。
    fn k(&self) -> f64 {
        self.s.font_size / BASE_FONT
    }

    /// 正文字号与行高（CSS 像素）。
    fn body(&self) -> f64 {
        self.s.font_size
    }

    fn line(&self) -> f64 {
        self.s.font_size * LINE_RATIO
    }

    /// 吃掉当前容器直到与之配对的 `End`（含）。
    fn skip(&self, it: &mut Events<'a>) {
        let mut depth = 0usize;
        for (event, _) in it.by_ref() {
            match event {
                Event::Start(_) => depth += 1,
                Event::End(_) if depth == 0 => return,
                Event::End(_) => depth -= 1,
                _ => {}
            }
        }
    }

    // ---------- 行内 ----------

    /// 读行内内容，直到当前容器的 `End`（含）。
    fn inline(&mut self, it: &mut Events<'a>) -> Inl {
        let mut inl = Inl::new();
        while let Some((event, range)) = it.next() {
            if matches!(event, Event::End(_)) {
                break;
            }
            self.inline_event(event, range, it, &mut inl);
        }
        inl
    }

    /// 把行内事件（及其子事件）追加进 `inl`。
    fn inline_event(
        &mut self,
        event: Event<'a>,
        range: Range<usize>,
        it: &mut Events<'a>,
        inl: &mut Inl,
    ) {
        match event {
            Event::Text(text) => inl.text(&text),
            Event::Code(code) => {
                let em = code
                    .chars()
                    .map(|c| if c.is_ascii() { 0.6 } else { 1.0 })
                    .sum::<f64>();
                inl.raw(
                    &format!("<code class=\"md-code-inline\">{}</code>", esc(&code)),
                    em + 0.8,
                );
            }
            Event::InlineMath(tex) => {
                inl.raw(
                    &format!("<span class=\"math\">{}</span>", esc(tex.trim())),
                    text_em(&tex) + 0.8,
                );
            }
            Event::DisplayMath(tex) => {
                // 块级公式：样式里是 display:block，自成一行（或几行）。
                inl.raw(
                    &format!("<span class=\"math-block\">{}</span>", esc(tex.trim())),
                    text_em(&tex) + 4.0,
                );
            }
            Event::SoftBreak if self.s.keep_breaks => inl.line_break(),
            Event::SoftBreak => inl.text(" "),
            Event::HardBreak => inl.line_break(),
            Event::InlineHtml(html) | Event::Html(html) => {
                let tag = html.trim().to_ascii_lowercase();
                if matches!(tag.as_str(), "<br>" | "<br/>" | "<br />") {
                    inl.line_break();
                } else if !tag.starts_with("<!--") {
                    // 其余标签不解释，当文字显示：模型与群友写的 HTML 不该改卡片的样子。
                    inl.text(html.trim_end_matches('\n'));
                }
            }
            Event::FootnoteReference(label) => {
                let index = match self.notes.iter().position(|l| l.as_str() == &*label) {
                    Some(index) => index,
                    None => {
                        self.notes.push(label.to_string());
                        self.notes.len() - 1
                    }
                };
                inl.notes.push(index);
                inl.raw(&format!("<sup class=\"fn-ref\">{}</sup>", index + 1), 0.9);
            }
            Event::Start(Tag::Emphasis) => {
                let sub = self.inline(it);
                inl.absorb("<em>", sub, "</em>");
            }
            Event::Start(Tag::Strong) => {
                let sub = self.inline(it);
                inl.absorb("<strong>", sub, "</strong>");
            }
            Event::Start(Tag::Strikethrough) => {
                // GFM 允许单个 `~` 划线；聊天里的「10~20」「约~5 分钟」不是划线。
                // 只有源码里真是 `~~` 才当划线，否则把波浪线还给读者。
                let double = self.src.get(range).is_some_and(|s| s.starts_with("~~"));
                let sub = self.inline(it);
                if double {
                    inl.absorb("<del>", sub, "</del>");
                } else {
                    inl.text("~");
                    inl.absorb("", sub, "");
                    inl.text("~");
                }
            }
            Event::Start(Tag::Superscript | Tag::Subscript) => {
                let sub = self.inline(it);
                inl.absorb("", sub, "");
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                ..
            }) => {
                let sub = self.inline(it);
                self.link(inl, link_type, &dest_url, sub);
            }
            Event::Start(Tag::Image { .. }) => {
                let alt = self.plain_text(it);
                let label = if alt.trim().is_empty() {
                    "图片".to_string()
                } else {
                    alt.trim().to_string()
                };
                inl.raw(
                    &format!(
                        "<span class=\"img-ph\">{ICON_IMAGE}<span>{}</span></span>",
                        esc(&label)
                    ),
                    text_em(&label) + 2.0,
                );
            }
            Event::Start(_) => self.skip(it),
            _ => {}
        }
    }

    /// 读到当前容器结束为止的纯文字（图片的替代文字）。
    fn plain_text(&self, it: &mut Events<'a>) -> String {
        let mut text = String::new();
        let mut depth = 0usize;
        for (event, _) in it.by_ref() {
            match event {
                Event::Start(_) => depth += 1,
                Event::End(_) if depth == 0 => break,
                Event::End(_) => depth -= 1,
                Event::Text(t) | Event::Code(t) => text.push_str(&t),
                Event::SoftBreak | Event::HardBreak => text.push(' '),
                _ => {}
            }
        }
        text
    }

    /// 链接：文字保留、加下划线；带说明文字的链接在文字后标编号，页底列出完整地址。
    fn link(&mut self, inl: &mut Inl, link_type: LinkType, dest: &str, sub: Inl) {
        let shown = matches!(link_type, LinkType::Autolink | LinkType::Email);
        let scheme_ok = {
            let lower = dest.trim().to_ascii_lowercase();
            lower.starts_with("http://")
                || lower.starts_with("https://")
                || lower.starts_with("mailto:")
        };
        if shown || !scheme_ok || dest.trim().is_empty() {
            inl.absorb("<span class=\"lk\">", sub, "</span>");
            return;
        }
        let dest = dest.trim().to_string();
        let index = match self.links.iter().position(|l| *l == dest) {
            Some(index) => index,
            None => {
                self.links.push(dest);
                self.links.len() - 1
            }
        };
        inl.links.push(index);
        inl.absorb(
            "<span class=\"lk\">",
            sub,
            &format!("<sup class=\"lk-n\">{}</sup></span>", index + 1),
        );
        *inl.lines.last_mut().unwrap() += 0.9;
    }

    // ---------- 块 ----------

    /// 处理一个块级事件（`Start` 或 `Rule`），产出的块追加到 `out`。
    fn block_event(
        &mut self,
        event: Event<'a>,
        it: &mut Events<'a>,
        inset: f64,
        out: &mut Vec<Block>,
    ) {
        match event {
            Event::Rule => out.push(Block::body("<hr>".into(), 33.0)),
            Event::Start(tag) => self.block(tag, it, inset, out),
            _ => {}
        }
    }

    fn block(&mut self, tag: Tag<'a>, it: &mut Events<'a>, inset: f64, out: &mut Vec<Block>) {
        let avail = self.avail(inset);
        match tag {
            Tag::Paragraph => {
                let inl = self.inline(it);
                if inl.is_empty() {
                    return;
                }
                let h = inl.height(self.body(), self.line(), avail) + GAP;
                out.push(Block::body(format!("<p>{}</p>", inl.html), h).with_refs(&inl));
            }
            Tag::Heading { level, .. } => {
                let inl = self.inline(it);
                if inl.is_empty() {
                    return;
                }
                // 字号与行高随正文缩放；页边距是 px 令牌，不缩。
                let k = self.k();
                let (n, font, line, extra) = match level {
                    HeadingLevel::H1 => (1, 35.0 * k, 45.0 * k, 44.0),
                    HeadingLevel::H2 => (2, 27.5 * k, 35.0 * k, 36.0),
                    HeadingLevel::H3 => (3, 20.0 * k, 30.0 * k, 28.0),
                    HeadingLevel::H4 => (4, 17.5 * k, 28.0 * k, 24.0),
                    HeadingLevel::H5 => (5, 15.0 * k, 24.0 * k, 20.0),
                    HeadingLevel::H6 => (6, 15.0 * k, 24.0 * k, 20.0),
                };
                let h = inl.height(font, line, avail) + extra;
                let mut block =
                    Block::body(format!("<h{n}>{}</h{n}>", inl.html), h).with_refs(&inl);
                block.kind = Kind::Heading;
                out.push(block);
            }
            Tag::BlockQuote(kind) => {
                let mut kids = Vec::new();
                self.blocks_until_end(it, inset + 34.0, &mut kids);
                let mut block = merge(kids);
                let (open, extra) = match kind {
                    Some(kind) => {
                        let (class, label, icon) = alert(kind);
                        (
                            format!(
                                "<blockquote class=\"alert alert-{class}\"><div class=\"alert-title\">{icon}<span>{label}</span></div>"
                            ),
                            32.0 + 34.0,
                        )
                    }
                    None => ("<blockquote>".to_string(), 32.0),
                };
                block.html = format!("{open}{}</blockquote>", block.html);
                block.h += extra;
                block.kind = Kind::Body;
                out.push(block);
            }
            Tag::CodeBlock(kind) => {
                let mut code = String::new();
                for (event, _) in it.by_ref() {
                    match event {
                        Event::Text(text) => code.push_str(&text),
                        Event::End(_) => break,
                        _ => {}
                    }
                }
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                let code = code.strip_suffix('\n').unwrap_or(&code).to_string();
                self.code(&lang, &code, avail, out);
            }
            Tag::List(start) => self.list(start, it, inset, out),
            Tag::Table(aligns) => self.table(&aligns, it, avail, out),
            Tag::HtmlBlock => {
                let mut raw = String::new();
                for (event, _) in it.by_ref() {
                    match event {
                        Event::Html(text) | Event::Text(text) => raw.push_str(&text),
                        Event::End(_) => break,
                        _ => {}
                    }
                }
                let raw = raw.trim_end();
                if raw.trim().is_empty() || raw.trim_start().starts_with("<!--") {
                    return;
                }
                let k = self.k();
                let lines = raw
                    .lines()
                    .map(|l| {
                        (text_em(l) * 15.0 * k / (avail - 32.0).max(40.0))
                            .ceil()
                            .max(1.0)
                    })
                    .sum::<f64>();
                out.push(Block::body(
                    format!("<pre class=\"raw\">{}</pre>", esc(raw)),
                    lines * 24.0 * k + 32.0 + GAP,
                ));
            }
            Tag::FootnoteDefinition(label) => {
                let (html, ..) = self.children(it, inset);
                self.note_defs.insert(label.to_string(), html);
            }
            Tag::DefinitionList => {
                let mut html = String::from("<dl>");
                let mut h = GAP;
                let mut links = Vec::new();
                let mut notes = Vec::new();
                while let Some((event, _)) = it.next() {
                    match event {
                        Event::End(_) => break,
                        Event::Start(Tag::DefinitionListTitle) => {
                            let inl = self.inline(it);
                            h += inl.height(self.body(), self.line(), avail) + 4.0;
                            html.push_str(&format!("<dt>{}</dt>", inl.html));
                            links.extend(inl.links);
                            notes.extend(inl.notes);
                        }
                        Event::Start(Tag::DefinitionListDefinition) => {
                            let (body, height, l, n) = self.children(it, inset + 24.0);
                            h += height + 6.0;
                            html.push_str(&format!("<dd>{body}</dd>"));
                            links.extend(l);
                            notes.extend(n);
                        }
                        Event::Start(_) => self.skip(it),
                        _ => {}
                    }
                }
                html.push_str("</dl>");
                let mut block = Block::body(html, h);
                block.links = links;
                block.notes = notes;
                out.push(block);
            }
            Tag::MetadataBlock(kind) => {
                let mut text = String::new();
                for (event, _) in it.by_ref() {
                    match event {
                        Event::Text(t) => text.push_str(&t),
                        Event::End(_) => break,
                        _ => {}
                    }
                }
                if matches!(kind, MetadataBlockKind::YamlStyle)
                    && let Some(front) = parse_front(&text)
                {
                    let meta = if front.meta.is_empty() {
                        String::new()
                    } else {
                        format!("<p class=\"doc-meta\">{}</p>", esc(&front.meta.join(" · ")))
                    };
                    let k = self.k();
                    let title_h =
                        ((text_em(&front.title) * 35.0 * k / avail).ceil().max(1.0)) * 45.0 * k;
                    let mut block = Block::body(
                        format!(
                            "<header class=\"doc-head\"><h1>{}</h1>{meta}</header>",
                            esc(&front.title)
                        ),
                        title_h + 56.0,
                    );
                    block.kind = Kind::Heading;
                    out.push(block);
                }
            }
            _ => self.skip(it),
        }
    }

    /// 读容器里的块，直到它的 `End`（含）。
    fn blocks_until_end(&mut self, it: &mut Events<'a>, inset: f64, out: &mut Vec<Block>) {
        while let Some((event, _)) = it.next() {
            if matches!(event, Event::End(_)) {
                return;
            }
            // 引用里也可能夹着不在段落里的行内事件（松散的宽松写法），当段落收。
            if is_inline(&event) {
                let mut inl = Inl::new();
                self.inline_event(event, 0..0, it, &mut inl);
                self.drain_inline(it, &mut inl);
                if !inl.is_empty() {
                    let h = inl.height(self.body(), self.line(), self.avail(inset)) + GAP;
                    out.push(Block::body(format!("<p>{}</p>", inl.html), h).with_refs(&inl));
                }
                continue;
            }
            self.block_event(event, it, inset, out);
        }
    }

    /// 把接下来连续的行内事件并入 `inl`（不吃 `End`）。
    fn drain_inline(&mut self, it: &mut Events<'a>, inl: &mut Inl) {
        while it.peek().is_some_and(|(event, _)| is_inline(event)) {
            let (event, range) = it.next().unwrap();
            self.inline_event(event, range, it, inl);
        }
    }

    /// 列表项、定义、脚注的内容：行内与块可以混排（紧凑列表的文字直接挂在项下）。
    /// 返回 HTML、高度、链接与脚注编号；吃掉容器的 `End`。
    fn children(
        &mut self,
        it: &mut Events<'a>,
        inset: f64,
    ) -> (String, f64, Vec<usize>, Vec<usize>) {
        let avail = self.avail(inset);
        let mut html = String::new();
        let mut h = 0.0;
        let mut links = Vec::new();
        let mut notes = Vec::new();
        let mut run: Option<Inl> = None;

        macro_rules! flush {
            () => {
                if let Some(inl) = run.take() {
                    if !inl.is_empty() {
                        h += inl.height(self.body(), self.line(), avail);
                        html.push_str(&inl.html);
                        links.extend(inl.links);
                        notes.extend(inl.notes);
                    }
                }
            };
        }

        loop {
            match it.peek() {
                None => break,
                Some((Event::End(_), _)) => {
                    it.next();
                    break;
                }
                Some((event, _)) if is_inline(event) => {
                    let (event, range) = it.next().unwrap();
                    let inl = run.get_or_insert_with(Inl::new);
                    self.inline_event(event, range, it, inl);
                }
                Some(_) => {
                    flush!();
                    let (event, _) = it.next().unwrap();
                    let mut kids = Vec::new();
                    self.block_event(event, it, inset, &mut kids);
                    for kid in kids {
                        html.push_str(&kid.html);
                        h += kid.h;
                        links.extend(kid.links);
                        notes.extend(kid.notes);
                    }
                }
            }
        }
        flush!();
        (html, h, links, notes)
    }

    fn code(&mut self, lang: &str, code: &str, avail: f64, out: &mut Vec<Block>) {
        let label = highlight::display_name(lang);
        let is_md = matches!(lang.trim().to_ascii_lowercase().as_str(), "md" | "markdown");
        // 等宽 body-small（设计基准 15px）：拉丁约 0.6em、汉字 1em，换行处按容器宽折。
        let (code_font, code_line) = (15.0 * self.k(), 24.0 * self.k());
        let cols_em = ((avail - 34.0) / code_font).max(8.0);
        let rows_of = |line: &str| -> f64 {
            let em: f64 = line
                .chars()
                .map(|c| if c.is_ascii() { 0.6 } else { 1.0 })
                .sum();
            (em / cols_em).ceil().max(1.0)
        };
        let lines: Vec<&str> = code.split('\n').collect();
        let budget_rows = (self.s.page_height * 0.8 / code_line).max(12.0);

        // 按可见行数把过长的代码切成几段，各段自成一块。
        let mut chunks: Vec<(usize, usize, f64)> = Vec::new();
        let (mut from, mut rows) = (0usize, 0.0);
        for (i, line) in lines.iter().enumerate() {
            let r = rows_of(line);
            if rows + r > budget_rows && i > from {
                chunks.push((from, i, rows));
                from = i;
                rows = 0.0;
            }
            rows += r;
        }
        chunks.push((from, lines.len(), rows));

        let total = chunks.len();
        for (index, (from, to, rows)) in chunks.into_iter().enumerate() {
            let part = lines[from..to].join("\n");
            let body = if is_md {
                None
            } else {
                highlight::highlight(lang, &part)
            }
            .unwrap_or_else(|| esc(&part));
            let tag = match (label.is_empty(), total) {
                (true, 1) => String::new(),
                (_, 1) => format!(
                    "<figcaption><span class=\"code-lang\">{}</span></figcaption>",
                    esc(&label)
                ),
                _ => format!(
                    "<figcaption><span class=\"code-lang\">{}</span><span class=\"code-part\">{}/{}</span></figcaption>",
                    esc(&label),
                    index + 1,
                    total
                ),
            };
            let cap = if tag.is_empty() { 0.0 } else { 30.0 };
            out.push(Block::body(
                format!("<figure class=\"code\">{tag}<pre><code>{body}</code></pre></figure>"),
                rows * code_line + 28.0 + cap + GAP,
            ));
        }
    }

    fn list(&mut self, start: Option<u64>, it: &mut Events<'a>, inset: f64, out: &mut Vec<Block>) {
        struct Item {
            html: String,
            h: f64,
            links: Vec<usize>,
            notes: Vec<usize>,
        }
        let mut items: Vec<Item> = Vec::new();
        while let Some((event, _)) = it.next() {
            match event {
                Event::End(_) => break,
                Event::Start(Tag::Item) => {
                    let task = match it.peek() {
                        Some((Event::TaskListMarker(done), _)) => Some(*done),
                        _ => None,
                    };
                    if task.is_some() {
                        it.next();
                    }
                    let (body, h, links, notes) = self.children(it, inset + 30.0);
                    let html = match task {
                        Some(done) => format!(
                            "<li class=\"task{}\"><span class=\"check\">{ICON_CHECK}</span><span class=\"task-text\">{body}</span></li>",
                            if done { " done" } else { "" }
                        ),
                        None => format!("<li>{body}</li>"),
                    };
                    items.push(Item {
                        html,
                        h: h.max(self.line()) + 6.0,
                        links,
                        notes,
                    });
                }
                Event::Start(_) => self.skip(it),
                _ => {}
            }
        }
        if items.is_empty() {
            return;
        }
        let first = start.unwrap_or(1);
        let budget = self.s.page_height * 0.6;
        // 顶层的长列表按项拆成几块，每块从正确的序号接着排。
        let mut groups: Vec<(usize, usize, f64)> = Vec::new();
        let (mut from, mut acc) = (0usize, 0.0);
        for (i, item) in items.iter().enumerate() {
            if inset == 0.0 && acc + item.h > budget && i > from {
                groups.push((from, i, acc));
                from = i;
                acc = 0.0;
            }
            acc += item.h;
        }
        groups.push((from, items.len(), acc));
        for (from, to, acc) in groups {
            let mut html = match start {
                Some(_) => format!("<ol start=\"{}\">", first + from as u64),
                None => "<ul>".to_string(),
            };
            let mut block = Block::body(String::new(), acc + GAP);
            for item in &items[from..to] {
                html.push_str(&item.html);
                block.links.extend(item.links.iter().copied());
                block.notes.extend(item.notes.iter().copied());
            }
            html.push_str(if start.is_some() { "</ol>" } else { "</ul>" });
            block.html = html;
            out.push(block);
        }
    }

    fn table(
        &mut self,
        aligns: &[Alignment],
        it: &mut Events<'a>,
        avail: f64,
        out: &mut Vec<Block>,
    ) {
        struct Cell {
            html: String,
            em: f64,
            token: f64,
        }
        let cols = aligns.len().max(1);
        let mut head: Vec<Cell> = Vec::new();
        let mut rows: Vec<Vec<Cell>> = Vec::new();
        let mut links = Vec::new();
        let mut notes = Vec::new();
        let mut row: Vec<Cell> = Vec::new();
        while let Some((event, _)) = it.next() {
            match event {
                Event::End(TagEnd::Table) => break,
                Event::Start(Tag::TableHead) => {}
                Event::End(TagEnd::TableHead) => {
                    head = std::mem::take(&mut row);
                }
                Event::Start(Tag::TableRow) => row = Vec::new(),
                Event::End(TagEnd::TableRow) => rows.push(std::mem::take(&mut row)),
                Event::Start(Tag::TableCell) => {
                    let inl = self.inline(it);
                    links.extend(inl.links.iter().copied());
                    notes.extend(inl.notes.iter().copied());
                    let em = inl.lines.iter().cloned().fold(0.0, f64::max);
                    row.push(Cell {
                        html: inl.html,
                        em,
                        token: inl.token,
                    });
                }
                Event::Start(_) => self.skip(it),
                _ => {}
            }
        }
        if head.is_empty() && rows.is_empty() {
            return;
        }
        let dense = cols >= 5;
        let k = self.k();
        // body-medium / body-small，随正文缩放；单元格内边距是 px 定值，不缩。
        let font = if dense { 15.0 * k } else { 17.5 * k };
        let col_w = ((avail - 2.0) / cols as f64 - 26.0).max(24.0);
        // 每列最长的不可断词加起来放得下，就只在词与汉字之间断行；放不下才允许词中间断，
        // 否则表格会撑出卡面被裁掉。
        let mut widest = vec![0.0f64; cols];
        for cells in std::iter::once(&head).chain(rows.iter()) {
            for (i, cell) in cells.iter().enumerate().take(cols) {
                widest[i] = widest[i].max(cell.token);
            }
        }
        let fits = widest.iter().sum::<f64>() * font + 26.0 * cols as f64 <= avail - 2.0;
        let row_h = |cells: &[Cell]| -> f64 {
            cells
                .iter()
                .map(|c| (c.em * font / col_w).ceil().max(1.0))
                .fold(1.0, f64::max)
                * (font * 1.5)
                + 20.0
        };
        let render_row = |cells: &[Cell], tag: &str| -> String {
            let mut html = String::from("<tr>");
            for (i, cell) in cells.iter().enumerate() {
                let class = match aligns.get(i) {
                    Some(Alignment::Center) => " class=\"al-c\"",
                    Some(Alignment::Right) => " class=\"al-r\"",
                    _ => "",
                };
                html.push_str(&format!("<{tag}{class}>{}</{tag}>", cell.html));
            }
            html.push_str("</tr>");
            html
        };
        let head_h = if head.is_empty() { 0.0 } else { row_h(&head) };
        let head_html = if head.is_empty() {
            String::new()
        } else {
            format!("<thead>{}</thead>", render_row(&head, "th"))
        };
        let budget = self.s.page_height * 0.8;
        let mut from = 0usize;
        let mut acc = head_h;
        let mut groups: Vec<(usize, usize, f64)> = Vec::new();
        for (i, r) in rows.iter().enumerate() {
            let h = row_h(r);
            if acc + h > budget && i > from {
                groups.push((from, i, acc));
                from = i;
                acc = head_h;
            }
            acc += h;
        }
        groups.push((from, rows.len(), acc));
        for (from, to, acc) in groups {
            let body: String = rows[from..to].iter().map(|r| render_row(r, "td")).collect();
            let mut block = Block::body(
                format!(
                    "<div class=\"table{}{}\"><table>{head_html}<tbody>{body}</tbody></table></div>",
                    if dense { " dense" } else { "" },
                    if fits { " whole-words" } else { "" }
                ),
                acc + 2.0 + GAP,
            );
            block.links = links.clone();
            block.notes = notes.clone();
            out.push(block);
        }
    }
}

fn alert(kind: BlockQuoteKind) -> (&'static str, &'static str, &'static str) {
    match kind {
        BlockQuoteKind::Note => ("note", "说明", ICON_INFO),
        BlockQuoteKind::Tip => ("tip", "提示", ICON_TIP),
        BlockQuoteKind::Important => ("important", "重要", ICON_IMPORTANT),
        BlockQuoteKind::Warning => ("warning", "注意", ICON_WARNING),
        BlockQuoteKind::Caution => ("caution", "小心", ICON_CAUTION),
    }
}

// 图标是自己画的几何形状（圆、三角、八边形加感叹号镂空），不引用任何图标库。
const ICON_INFO: &str = "<svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path fill-rule=\"evenodd\" d=\"M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20zM11 10.5h2V18h-2zM12 6a1.3 1.3 0 1 0 0 2.6A1.3 1.3 0 0 0 12 6z\"/></svg>";
const ICON_TIP: &str = "<svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"M12 1.5l2.6 7.9 7.9 2.6-7.9 2.6L12 22.5l-2.6-7.9L1.5 12l7.9-2.6z\"/></svg>";
const ICON_IMPORTANT: &str = "<svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path fill-rule=\"evenodd\" d=\"M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20zM11 6h2v8h-2zM12 15.6a1.3 1.3 0 1 0 0 2.6 1.3 1.3 0 0 0 0-2.6z\"/></svg>";
const ICON_WARNING: &str = "<svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path fill-rule=\"evenodd\" d=\"M12 2.2L23 21.5H1zM11 9h2v6h-2zM12 16.3a1.3 1.3 0 1 0 0 2.6 1.3 1.3 0 0 0 0-2.6z\"/></svg>";
const ICON_CAUTION: &str = "<svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path fill-rule=\"evenodd\" d=\"M8.3 2h7.4L22 8.3v7.4L15.7 22H8.3L2 15.7V8.3zM11 6.5h2v6.5h-2zM12 14.7a1.3 1.3 0 1 0 0 2.6 1.3 1.3 0 0 0 0-2.6z\"/></svg>";
const ICON_CHECK: &str = "<svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"M5.5 12.5l4.2 4.2 8.8-9.4\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"3.2\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/></svg>";
const ICON_IMAGE: &str = "<svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"M4 5h16v14H4zM4 16.5l4.5-5 4 4.5 2.5-2.5 5 4.5\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linejoin=\"round\" stroke-linecap=\"round\"/></svg>";

fn merge(kids: Vec<Block>) -> Block {
    let mut block = Block::body(String::new(), 0.0);
    for kid in kids {
        block.html.push_str(&kid.html);
        block.h += kid.h;
        block.links.extend(kid.links);
        block.notes.extend(kid.notes);
    }
    block
}

/// YAML 头里只认几个常见的键，其余忽略。没有标题就当没有头。
fn parse_front(text: &str) -> Option<Front> {
    let (mut title, mut author, mut date, mut desc) = (None, None, None, None);
    for line in text.lines() {
        let Some((key, value)) = line.split_once([':', '=']) else {
            continue;
        };
        let value = value.trim().trim_matches(['"', '\'']).trim();
        if value.is_empty() {
            continue;
        }
        match key.trim().to_ascii_lowercase().as_str() {
            "title" | "标题" => title = Some(value.to_string()),
            "author" | "作者" => author = Some(value.to_string()),
            "date" | "日期" => date = Some(value.to_string()),
            "description" | "subtitle" | "summary" | "摘要" => desc = Some(value.to_string()),
            _ => {}
        }
    }
    let title = title?;
    Some(Front {
        title,
        meta: [desc, author, date].into_iter().flatten().collect(),
    })
}

// ==================== 分页与拼页 ====================

fn paginate(blocks: Vec<Block>, target: f64) -> Vec<Vec<Block>> {
    let mut pages: Vec<Vec<Block>> = vec![Vec::new()];
    let mut acc = 0.0;
    for block in blocks {
        if acc > 0.0 && acc + block.h > target {
            let cur = pages.last_mut().unwrap();
            // 标题不落在页尾：连同紧随其后要来的内容一起挪到下一页。
            let mut carry = Vec::new();
            while cur.len() > 1 && cur.last().is_some_and(|b| b.kind == Kind::Heading) {
                carry.push(cur.pop().unwrap());
            }
            carry.reverse();
            acc = carry.iter().map(|b| b.h).sum();
            pages.push(carry);
        }
        acc += block.h;
        pages.last_mut().unwrap().push(block);
    }
    // 末页只剩一点点时并回上一页：一张几乎空白的页不值得单发一张图。
    if pages.len() > 1 {
        let last: f64 = pages.last().unwrap().iter().map(|b| b.h).sum();
        let prev: f64 = pages[pages.len() - 2].iter().map(|b| b.h).sum();
        if last < target * 0.18 && prev + last <= target * 1.3 {
            let tail = pages.pop().unwrap();
            pages.last_mut().unwrap().extend(tail);
        }
    }
    pages.retain(|page| !page.is_empty());
    pages
}

/// 渲染。返回空页表示没有可渲染的内容（全是空白或注释）。
pub fn render(source: &str, s: &Settings) -> Rendered {
    let text = normalize(source);
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_GFM);
    options.insert(Options::ENABLE_MATH);
    options.insert(Options::ENABLE_DEFINITION_LIST);
    options.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    let mut it = Parser::new_ext(&text, options)
        .into_offset_iter()
        .peekable();

    let mut doc = Doc {
        src: &text,
        s,
        links: Vec::new(),
        notes: Vec::new(),
        note_defs: HashMap::new(),
    };
    let mut blocks = Vec::new();
    while let Some((event, _)) = it.next() {
        doc.block_event(event, &mut it, 0.0, &mut blocks);
    }
    let pages = paginate(blocks, s.page_height);
    let total = pages.len();
    let shown = total.min(s.max_pages.max(1));
    let html = pages
        .iter()
        .take(shown)
        .enumerate()
        .map(|(index, page)| page_html(&doc, page, index + 1, total))
        .collect();
    Rendered {
        pages: html,
        total_pages: total,
    }
}

fn page_html(doc: &Doc, blocks: &[Block], index: usize, total: usize) -> String {
    let mut body = String::new();
    let mut links: Vec<usize> = Vec::new();
    let mut notes: Vec<usize> = Vec::new();
    for block in blocks {
        body.push_str(&block.html);
        links.extend(&block.links);
        notes.extend(&block.notes);
    }
    links.sort_unstable();
    links.dedup();
    notes.sort_unstable();
    notes.dedup();

    let mut refs = String::new();
    if !links.is_empty() {
        refs.push_str("<section class=\"refs\"><h2>链接</h2><ol>");
        for index in links.iter().take(40) {
            refs.push_str(&format!(
                "<li><span class=\"ref-n\">{}</span><span class=\"ref-url\">{}</span></li>",
                index + 1,
                esc(&doc.links[*index])
            ));
        }
        refs.push_str("</ol></section>");
    }
    if !notes.is_empty() {
        refs.push_str("<section class=\"refs\"><h2>脚注</h2><ol>");
        for index in &notes {
            let html = doc
                .note_defs
                .get(&doc.notes[*index])
                .cloned()
                .unwrap_or_default();
            refs.push_str(&format!(
                "<li><span class=\"ref-n\">{}</span><span class=\"ref-text\">{html}</span></li>",
                index + 1
            ));
        }
        refs.push_str("</ol></section>");
    }
    let pager = if total > 1 {
        format!("<span class=\"md-stamp\">{index} / {total}</span>")
    } else {
        String::new()
    };
    let width = doc.s.width;
    // 字阶的根：`tokens.css` 把 `1rem` 定在 20px（`:root` 的 font-size），
    // 这里在 html 上以内联样式盖掉它，整套 `rem` 字号就按正文字号缩放。
    // 内联样式优先于样式表的 `:root` 规则，不必与令牌表争顺序。
    format!(
        r#"<!doctype html><html lang="zh-CN" style="font-size:{font}px"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; font-src data:">
<title>Markdown</title><style>{DESIGN_SYSTEM}{CSS}</style></head>
<body class="md-text{dark}" style="width:{outer}px"><main class="shot"><article class="card md-card" style="width:{width}px">
<div class="inner"><div class="md-eyebrow"><div class="md-kicker"><span class="md-dot"></span>Markdown<span class="md-kicker-en">ACUMEN</span></div>{pager}</div>
<div class="doc">{body}</div></div>{refs}</article></main></body></html>"#,
        dark = if doc.s.dark { " dark" } else { "" },
        font = doc.s.font_size,
        outer = width + 40,
    )
}
