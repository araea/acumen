//! Markdown → 卡片页 HTML。本仓库所有把 Markdown 画成图的地方（`markdown` 插件、
//! `oai` 回复卡）都走这一处，不各写一套。
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
/// 正文字号与行高（`body-large` 与 1.75 倍行距）。
const BODY: f64 = 20.0;
const LINE: f64 = 35.0;
const GAP: f64 = 16.0;

pub struct Settings {
    /// 卡面宽度（CSS 像素）。
    pub width: u32,
    pub dark: bool,
    /// 单个换行按换行显示（聊天里的写法），而不是按 CommonMark 合成一个空格。
    pub keep_breaks: bool,
    /// 每页的目标高度（CSS 像素）。软上限：块不拆开时可以略超。
    pub page_height: f64,
    pub max_pages: usize,
    /// 页眉左端的卡片名与其英文眉标。
    pub kicker: String,
    pub kicker_en: String,
    /// 页眉右端的说明（模型、耗时之类，纯文本）；多页时后面接页码。
    pub stamp: String,
    /// 正文前的标题行（纯文本），空则不画。
    pub title: String,
    /// 末页正文之后的附录（来源、轨迹之类）。调用方负责转义；样式用 `extra_css`。
    pub footer_html: String,
    pub extra_css: &'static str,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            width: 480,
            dark: false,
            keep_breaks: true,
            page_height: 2000.0,
            max_pages: 6,
            kicker: "Markdown".into(),
            kicker_en: "ACUMEN".into(),
            stamp: String::new(),
            title: String::new(),
            footer_html: String::new(),
            extra_css: "",
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
                let h = inl.height(BODY, LINE, avail) + GAP;
                out.push(Block::body(format!("<p>{}</p>", inl.html), h).with_refs(&inl));
            }
            Tag::Heading { level, .. } => {
                let inl = self.inline(it);
                if inl.is_empty() {
                    return;
                }
                let (n, font, line, extra) = match level {
                    HeadingLevel::H1 => (1, 35.0, 45.0, 44.0),
                    HeadingLevel::H2 => (2, 27.5, 35.0, 36.0),
                    HeadingLevel::H3 => (3, 20.0, 30.0, 28.0),
                    HeadingLevel::H4 => (4, 17.5, 28.0, 24.0),
                    HeadingLevel::H5 => (5, 15.0, 24.0, 20.0),
                    HeadingLevel::H6 => (6, 15.0, 24.0, 20.0),
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
                let lines = raw
                    .lines()
                    .map(|l| {
                        (text_em(l) * 15.0 / (avail - 32.0).max(40.0))
                            .ceil()
                            .max(1.0)
                    })
                    .sum::<f64>();
                out.push(Block::body(
                    format!("<pre class=\"raw\">{}</pre>", esc(raw)),
                    lines * 24.0 + 32.0 + GAP,
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
                            h += inl.height(BODY, LINE, avail) + 4.0;
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
                    let title_h = ((text_em(&front.title) * 35.0 / avail).ceil().max(1.0)) * 45.0;
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
                    let h = inl.height(BODY, LINE, self.avail(inset)) + GAP;
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
                        h += inl.height(BODY, LINE, avail);
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
        // 等宽 15px：拉丁约 0.6em、汉字 1em，换行处按容器宽折。
        let cols_em = ((avail - 34.0) / 15.0).max(8.0);
        let rows_of = |line: &str| -> f64 {
            let em: f64 = line
                .chars()
                .map(|c| if c.is_ascii() { 0.6 } else { 1.0 })
                .sum();
            (em / cols_em).ceil().max(1.0)
        };
        let lines: Vec<&str> = code.split('\n').collect();
        let budget_rows = (self.s.page_height * 0.8 / 24.0).max(12.0);

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
                rows * 24.0 + 28.0 + cap + GAP,
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
                        h: h.max(LINE) + 6.0,
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
        let font = if dense { 15.0 } else { 17.5 };
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
    let stamp = match (doc.s.stamp.trim(), total > 1) {
        ("", false) => String::new(),
        ("", true) => format!("{index} / {total}"),
        (text, false) => text.to_string(),
        (text, true) => format!("{text} · {index} / {total}"),
    };
    let pager = if stamp.is_empty() {
        String::new()
    } else {
        format!("<span class=\"md-stamp\">{}</span>", esc(&stamp))
    };
    let title = if doc.s.title.trim().is_empty() || index > 1 {
        String::new()
    } else {
        format!(
            "<div class=\"head md-title md-type-title-small\">{}</div><hr class=\"md-divider\">",
            esc(doc.s.title.trim())
        )
    };
    let footer = if index == total {
        doc.s.footer_html.as_str()
    } else {
        ""
    };
    let width = doc.s.width;
    format!(
        r#"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; font-src data:">
<title>Markdown</title><style>{DESIGN_SYSTEM}{CSS}{extra}</style></head>
<body class="md-text{dark}" style="width:{outer}px"><main class="shot"><article class="card md-card" style="width:{width}px">
<div class="inner"><div class="md-eyebrow"><div class="md-kicker"><span class="md-dot"></span>{kicker}<span class="md-kicker-en">{kicker_en}</span></div>{pager}</div>
{title}<div class="doc">{body}</div></div>{refs}{footer}</article></main></body></html>"#,
        dark = if doc.s.dark { " dark" } else { "" },
        outer = width + 40,
        extra = doc.s.extra_css,
        kicker = esc(&doc.s.kicker),
        kicker_en = esc(&doc.s.kicker_en),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings::default()
    }

    fn one_page(source: &str) -> String {
        let rendered = render(source, &settings());
        assert_eq!(rendered.pages.len(), 1, "{source:?} 应当只有一页");
        rendered.pages.into_iter().next().unwrap()
    }

    #[test]
    fn stylesheet_stays_embeddable() {
        crate::render::web::assert_embeddable("markdown.css", CSS);
    }

    #[test]
    fn headings_lists_and_emphasis_render_as_structure() {
        let html = one_page("# 标题\n\n## 小节\n\n- 甲\n- **乙**\n\n3. 三\n4. 四\n");
        assert!(html.contains("<h1>标题</h1>"), "{html}");
        assert!(html.contains("<h2>小节</h2>"), "{html}");
        assert!(
            html.contains("<ul><li>甲</li><li><strong>乙</strong></li></ul>"),
            "{html}"
        );
        assert!(
            html.contains("<ol start=\"3\"><li>三</li><li>四</li></ol>"),
            "{html}"
        );
    }

    #[test]
    fn raw_html_and_links_never_reach_the_page_as_markup() {
        let html = one_page(
            "<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n\n[点我](javascript:alert(1)) ![图](http://127.0.0.1/a.png)",
        );
        assert!(!html.contains("<script>alert"), "{html}");
        assert!(!html.contains("<img"), "{html}");
        assert!(!html.contains("href="), "{html}");
        assert!(!html.contains("<img src"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
        // 图片只留占位与替代文字。
        assert!(html.contains("img-ph"), "{html}");
    }

    #[test]
    fn links_are_numbered_and_listed_with_their_full_address() {
        let html = one_page(
            "见[文档](https://example.com/a?b=1&c=2)与[再看](https://example.com/a?b=1&c=2)，<https://x.y/z>",
        );
        assert_eq!(
            html.matches("class=\"lk-n\">1<").count(),
            2,
            "同一地址同一个编号：{html}"
        );
        assert!(
            html.contains("<section class=\"refs\"><h2>链接</h2>"),
            "{html}"
        );
        assert!(html.contains("https://example.com/a?b=1&amp;c=2"), "{html}");
        // 自动链接本身就是地址，不再编号。
        assert_eq!(html.matches("class=\"lk-n\"").count(), 2, "{html}");
    }

    #[test]
    fn tildes_between_numbers_are_not_strikethrough() {
        let html = one_page("价格 10~20 元，约 30~40 分钟，~~作废~~");
        assert!(html.contains("10~20"), "{html}");
        assert!(html.contains("30~40"), "{html}");
        assert!(html.contains("<del>作废</del>"), "{html}");
        assert_eq!(html.matches("<del>").count(), 1, "{html}");
    }

    #[test]
    fn single_newlines_stay_line_breaks_unless_told_otherwise() {
        assert!(one_page("第一行\n第二行").contains("第一行<br>第二行"));
        let mut s = settings();
        s.keep_breaks = false;
        let html = render("第一行\n第二行", &s).pages.remove(0);
        assert!(html.contains("第一行 第二行"), "{html}");
    }

    #[test]
    fn tables_keep_alignment_and_a_header() {
        let html = one_page("| 名 | 数 | 备注 |\n|:--|--:|:-:|\n| a | 1 | x |\n");
        assert!(html.contains("<thead><tr><th>名</th><th class=\"al-r\">数</th><th class=\"al-c\">备注</th></tr></thead>"), "{html}");
        assert!(html.contains("<td class=\"al-r\">1</td>"), "{html}");
    }

    #[test]
    fn task_lists_quotes_alerts_and_footnotes() {
        let html = one_page(
            "- [x] 做完\n- [ ] 待办\n\n> [!WARNING]\n> 小心\n\n> 普通引用\n\n正文[^1]\n\n[^1]: 脚注内容\n",
        );
        assert!(html.contains("class=\"task done\""), "{html}");
        assert!(html.contains("class=\"task\""), "{html}");
        assert!(html.contains("alert alert-warning"), "{html}");
        assert!(
            html.contains("<span>注意</span>"),
            "提示块要有文字标签，不能只靠颜色：{html}"
        );
        assert!(
            html.contains("<blockquote><p>普通引用</p></blockquote>"),
            "{html}"
        );
        assert!(html.contains("<sup class=\"fn-ref\">1</sup>"), "{html}");
        assert!(html.contains("<h2>脚注</h2>"), "{html}");
        assert!(html.contains("脚注内容"), "{html}");
    }

    #[test]
    fn code_is_highlighted_labelled_and_escaped() {
        let html = one_page("```rust\nfn main() { let a = \"<b>\"; }\n```\n\n```\n纯文本 <i>\n```");
        assert!(html.contains("class=\"code-lang\">Rust<"), "{html}");
        assert!(html.contains("tk-k\">fn</span>"), "{html}");
        assert!(!html.contains("<b>"), "{html}");
        assert!(html.contains("纯文本 &lt;i&gt;"), "{html}");
    }

    #[test]
    fn a_whole_message_fenced_as_markdown_is_unwrapped() {
        assert_eq!(normalize("```markdown\n# 好\n\n正文\n```"), "# 好\n\n正文");
        assert_eq!(normalize("~~~md\n**x**\n~~~"), "**x**");
        // 别的语言的围栏是代码，不脱。
        assert_eq!(
            normalize("```rust\nfn f() {}\n```"),
            "```rust\nfn f() {}\n```"
        );
        // 围栏没收尾就不认。
        assert_eq!(normalize("```md\n# 好"), "```md\n# 好");
        assert_eq!(normalize("\u{feff}\r\n\r\na\r\nb  \n"), "a\nb");
    }

    #[test]
    fn front_matter_becomes_a_title_block() {
        let html = one_page("---\ntitle: 周报\nauthor: 小明\ndate: 2026-09-29\n---\n\n正文");
        assert!(
            html.contains("<header class=\"doc-head\"><h1>周报</h1>"),
            "{html}"
        );
        assert!(html.contains("小明 · 2026-09-29"), "{html}");
        // 没有标题的头不出声。
        let html = one_page("---\nfoo: bar\n---\n\n正文");
        assert!(!html.contains("class=\"doc-head\""), "{html}");
    }

    #[test]
    fn empty_and_comment_only_input_has_no_pages() {
        assert!(render("", &settings()).pages.is_empty());
        assert!(render("<!-- 什么也没有 -->", &settings()).pages.is_empty());
    }

    #[test]
    fn long_documents_break_between_blocks_and_keep_headings_with_their_text() {
        let mut source = String::new();
        for i in 0..40 {
            source.push_str(&format!(
                "## 第 {i} 节\n\n{}\n\n",
                "这是一段用来撑高度的正文。".repeat(20)
            ));
        }
        let rendered = render(&source, &settings());
        assert!(rendered.total_pages > 2, "{}", rendered.total_pages);
        assert_eq!(rendered.pages.len(), settings().max_pages);
        for page in &rendered.pages {
            // 每页都是完整文档，且标题后面跟着正文（页尾不是标题）。
            let doc = page.split("<div class=\"doc\">").nth(1).unwrap();
            let last = doc.rfind("</h2>");
            let last_p = doc.rfind("</p>");
            assert!(
                last < last_p,
                "标题落在了页尾：{}",
                &doc[doc.len().saturating_sub(200)..]
            );
        }
        assert!(rendered.pages[1].contains("2 / "), "多页要标页码");
    }

    #[test]
    fn oversized_code_blocks_and_tables_split_and_tables_repeat_their_header() {
        let code = (0..400)
            .map(|i| format!("let x{i} = {i};"))
            .collect::<Vec<_>>()
            .join("\n");
        let rendered = render(&format!("```rust\n{code}\n```"), &settings());
        assert!(rendered.total_pages > 1, "长代码要拆开");
        assert!(
            rendered.pages[0].contains("class=\"code-part\">1/"),
            "{}",
            rendered.pages[0]
        );

        let rows = (0..200)
            .map(|i| format!("| r{i} | v{i} |"))
            .collect::<Vec<_>>()
            .join("\n");
        let rendered = render(&format!("| 名 | 值 |\n|---|---|\n{rows}\n"), &settings());
        assert!(rendered.total_pages > 1);
        for page in &rendered.pages {
            assert!(page.contains("<thead>"), "表头要随页重复");
        }
    }

    #[test]
    fn long_top_level_lists_continue_their_numbering_across_pages() {
        let items = (1..=120)
            .map(|i| format!("{i}. 事项 {i} 需要写得稍微长一点以便占高度"))
            .collect::<Vec<_>>()
            .join("\n");
        let rendered = render(&items, &settings());
        assert!(rendered.total_pages > 1);
        let second = &rendered.pages[1];
        let start: u32 = second
            .split("<ol start=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .and_then(|s| s.parse().ok())
            .expect("第二页的列表要带 start");
        assert!(start > 1);
        assert!(
            second.contains(&format!("事项 {start} ")),
            "序号要与内容对得上"
        );
    }

    #[test]
    fn appendix_title_and_stamp_come_from_the_caller() {
        let mut s = settings();
        s.title = "研究 #3".into();
        s.stamp = "模型 · 8 秒".into();
        s.kicker = "智能回复".into();
        s.footer_html = "<div class=\"foot\">尾</div>".into();
        let html = render("正文", &s).pages.remove(0);
        assert!(html.contains("研究 #3"));
        assert!(html.contains("模型 · 8 秒"));
        assert!(html.contains("智能回复"));
        assert!(html.contains("<div class=\"foot\">尾</div>"));
    }

    #[test]
    fn dark_theme_only_flips_the_body_class() {
        let mut s = settings();
        s.dark = true;
        assert!(render("x", &s).pages[0].contains("class=\"md-text dark\""));
    }

    // ---------- 对比度：按 WCAG 2.2 AA 把用到的前景/背景对卡在 4.5:1 ----------

    type Rgb = (f64, f64, f64);

    fn hex(value: &str) -> Rgb {
        let v = value.trim().trim_start_matches('#');
        let n = u32::from_str_radix(v, 16).unwrap();
        (
            ((n >> 16) & 255) as f64,
            ((n >> 8) & 255) as f64,
            (n & 255) as f64,
        )
    }

    fn luminance((r, g, b): Rgb) -> f64 {
        let lin = |c: f64| {
            let c = c / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
    }

    fn contrast(a: Rgb, b: Rgb) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    fn mix(fg: Rgb, bg: Rgb, alpha: f64) -> Rgb {
        (
            fg.0 * alpha + bg.0 * (1.0 - alpha),
            fg.1 * alpha + bg.1 * (1.0 - alpha),
            fg.2 * alpha + bg.2 * (1.0 - alpha),
        )
    }

    /// 取 tokens.css 里某个色板块（`body.dark` 或 `:root`）的 `--md-sys-color-*`。
    fn palette(block: &str) -> HashMap<String, Rgb> {
        let sheet = include_str!("../../res/cards/tokens.css");
        let start = sheet.find(&format!("{block} {{")).expect(block);
        let body = &sheet[start..];
        let body = &body[..body.find('}').unwrap()];
        body.lines()
            .filter_map(|line| {
                let (key, value) = line
                    .trim()
                    .strip_prefix("--md-sys-color-")?
                    .split_once(':')?;
                let value = value.trim().trim_end_matches(';');
                value
                    .starts_with('#')
                    .then(|| (key.to_string(), hex(value)))
            })
            .collect()
    }

    #[test]
    fn every_foreground_pair_the_stylesheet_uses_meets_wcag_aa() {
        for (name, block) in [("浅色", "body.scheme-reply"), ("深色", "body.dark")] {
            let p = palette(block);
            let c = |k: &str| *p.get(k).unwrap_or_else(|| panic!("{name} 缺 {k}"));
            let surfaces = ["surface", "surface-container-low", "surface-container"];
            let mut pairs: Vec<(String, Rgb, Rgb)> = Vec::new();
            for s in surfaces {
                for fg in ["on-surface", "on-surface-variant", "primary"] {
                    pairs.push((format!("{fg} on {s}"), c(fg), c(s)));
                }
            }
            // 代码面（surface-container）上的着色与页底附录（surface-container-low）上的字。
            for fg in ["success", "warning", "tertiary", "secondary", "error"] {
                pairs.push((format!("{fg} on code"), c(fg), c("surface-container")));
            }
            pairs.push((
                "on-surface on table stripe".into(),
                c("on-surface"),
                c("surface-container-low"),
            ));
            // 成对的容器色；
            for role in [
                "primary",
                "secondary",
                "tertiary",
                "success",
                "warning",
                "error",
            ] {
                pairs.push((
                    format!("on-{role}-container on {role}-container"),
                    c(&format!("on-{role}-container")),
                    c(&format!("{role}-container")),
                ));
            }
            // 提示块：底是角色色 14% 对卡面，正文 on-surface，标题与图标着角色色。
            for role in ["primary", "success", "tertiary", "warning", "error"] {
                let tint = mix(c(role), c("surface"), 0.14);
                pairs.push((format!("{role} title on alert"), c(role), tint));
                pairs.push((format!("on-surface on {role} alert"), c("on-surface"), tint));
                pairs.push((format!("primary link on {role} alert"), c("primary"), tint));
            }
            pairs.push((
                "tertiary on math".into(),
                c("tertiary"),
                c("surface-container-high"),
            ));
            pairs.push((
                "on-surface-variant on high".into(),
                c("on-surface-variant"),
                c("surface-container-high"),
            ));
            pairs.push((
                "on-primary on primary".into(),
                c("on-primary"),
                c("primary"),
            ));
            // 带透明度的底：链接编号（主色 15%）与 diff 行（成功/错误 14%）。
            let card = c("surface");
            pairs.push((
                "primary on link badge".into(),
                c("primary"),
                mix(c("primary"), card, 0.15),
            ));
            let code = c("surface-container");
            pairs.push((
                "on-surface on diff add".into(),
                c("on-surface"),
                mix(c("success"), code, 0.18),
            ));
            pairs.push((
                "on-surface on diff del".into(),
                c("on-surface"),
                mix(c("error"), code, 0.18),
            ));
            pairs.push(("tertiary on diff hunk".into(), c("tertiary"), code));
            for (label, fg, bg) in pairs {
                let ratio = contrast(fg, bg);
                assert!(ratio >= 4.5, "{name}：{label} 只有 {ratio:.2}:1");
            }
        }
    }

    // ---------- 肉眼核对：`MD_DUMP=<目录> CHROME_BIN=... cargo test markdown_dump -- --ignored` ----------

    const SAMPLE: &str = include_str!("markdown/sample.md");

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "writes card images for visual review"]
    async fn markdown_dump() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let dir = std::env::var("MD_DUMP").expect("MD_DUMP 指向输出目录");
        std::fs::create_dir_all(&dir).unwrap();
        let path = std::env::var("CHROME_BIN").ok();
        let scale: f64 = std::env::var("MD_SCALE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2.0);
        let width: u32 = std::env::var("MD_WIDTH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(480);
        // `MD_SRC` 指向另一份 Markdown 时按它出图，方便拿真实内容核对。
        let sample = std::env::var("MD_SRC")
            .ok()
            .map(|path| std::fs::read_to_string(path).expect("读 MD_SRC"))
            .unwrap_or_else(|| SAMPLE.to_string());
        for (name, dark) in [("light", false), ("dark", true)] {
            let s = Settings {
                dark,
                width,
                ..Settings::default()
            };
            let rendered = render(&sample, &s);
            for (i, html) in rendered.pages.iter().enumerate() {
                std::fs::write(format!("{dir}/{name}-{}.html", i + 1), html).unwrap();
                let b64 = crate::render::web::shoot(
                    crate::render::web::Shot::new(html, s.width + 40)
                        .scale(scale)
                        .browser(path.as_deref()),
                )
                .await
                .expect("出图");
                std::fs::write(
                    format!("{dir}/{name}-{}.png", i + 1),
                    STANDARD.decode(b64).unwrap(),
                )
                .unwrap();
            }
            println!("{name}: {} 页", rendered.total_pages);
        }
        cdp_html_shot::Browser::shutdown_global().await;
    }
}
