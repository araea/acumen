//! 公式：LaTeX → 卡片里的 MathML，或 → 聊天里能直接读的 Unicode 文字。
//!
//! **卡片**走浏览器原生的 MathML（Chromium 109 起支持 MathML Core），转换交给
//! `math-core`，不跑 KaTeX 那类脚本：页面的 CSP 本来就不许脚本，排版也不必等
//! 一轮 JS。字形用内嵌的 New Computer Modern Math Book 子集（`res/fonts`，GUST
//! 字体许可）——系统里的数学字体因机而异，没有它分式线、根号与大括号都会走样。
//! 字体只在页面真有公式时才写进样式表，平常的卡片不背这两百多 KB。
//!
//! **聊天文字**里的 `$\frac{1}{2}$` 群友读不了，发出去之前换成 `1/2`、`x²`、`α`
//! 这类能直接读的写法（[`plain`]）；换不成一行字的（矩阵、多行推导、嵌套分式）由
//! [`is_heavy`] 认出来，交给调用方排成图。
//!
//! 模型写公式的定界符五花八门：`$…$`、`$$…$$` 之外还有 `\(…\)`、`\[…\]` 与
//! ```` ```math ```` 围栏。前两种 CommonMark 当成转义括号吃掉，[`normalize_delimiters`]
//! 先把它们统一成美元符号。

use base64::Engine as _;
use math_core::{LatexToMathML, MathCoreConfig, MathDisplay};
use std::sync::OnceLock;

/// 公式的版式（不含字体）。与字体分开放，是为了没有公式的页面一个字节都不多带。
const CSS: &str = include_str!("../../res/cards/math.css");
const FONT: &[u8] = include_bytes!("../../res/fonts/newcm-math.woff2");

/// 有公式的页面要追加的样式：字体 + 版式。
pub fn css() -> &'static str {
    static FULL: OnceLock<String> = OnceLock::new();
    FULL.get_or_init(|| {
        format!(
            "@font-face{{font-family:\"Acumen Math\";src:url(data:font/woff2;base64,{}) format(\"woff2\");font-display:block}}{CSS}",
            base64::engine::general_purpose::STANDARD.encode(FONT)
        )
    })
}

fn converter() -> &'static LatexToMathML {
    static CONVERTER: OnceLock<LatexToMathML> = OnceLock::new();
    CONVERTER.get_or_init(|| {
        let config = MathCoreConfig {
            // 不认识的命令就地标红，公式其余部分照常排；整条退回源码对读者更糟。
            ignore_unknown_commands: true,
            ..MathCoreConfig::default()
        };
        LatexToMathML::new(config, Vec::new()).expect("空宏表不会解析失败")
    })
}

/// LaTeX → `<math>` 元素；语法错（括号不配对之类）时返回 `None`，调用方显示源码。
pub fn mathml(tex: &str, display: bool) -> Option<String> {
    let tex = tex.trim();
    if tex.is_empty() {
        return None;
    }
    let mode = if display { MathDisplay::Block } else { MathDisplay::Inline };
    converter()
        .convert_with_local_state(&wrap_cjk(tex), mode)
        .ok()
        .map(|result| result.mathml)
}

/// 行内公式的 HTML：转换失败退回等宽源码。
pub fn inline_html(tex: &str) -> String {
    match mathml(tex, false) {
        Some(math) => format!("<span class=\"math\">{math}</span>"),
        None => source_html(tex, false),
    }
}

/// 块级公式的 HTML。外层带 `data-fit`：截图前按可用宽度把过宽的公式缩小（见
/// [`crate::render::web::shoot`]），公式不能折行，不缩就会被卡边裁掉。
pub fn display_html(tex: &str) -> String {
    match mathml(tex, true) {
        Some(math) => format!("<span class=\"math-block\" data-fit>{math}</span>"),
        None => source_html(tex, true),
    }
}

fn source_html(tex: &str, display: bool) -> String {
    let class = if display { "math-src block" } else { "math-src" };
    format!(
        "<code class=\"{class}\">{}</code>",
        crate::render::web::esc(tex.trim())
    )
}

/// 公式里的汉字包进 `\text{}`：`<mi>` 会把单字排成斜体，汉字没有斜体，
/// 浏览器只能硬拧，难看也难认。已经在 `\text{…}` 一类参数里的不动。
fn wrap_cjk(tex: &str) -> String {
    const TEXT_CMDS: &[&str] = &[
        "text", "textrm", "textbf", "textit", "textsf", "texttt", "mbox", "operatorname",
        "mathrm", "mathbf", "mathit", "mathsf", "mathtt", "hbox",
    ];
    if !tex.chars().any(is_cjk) {
        return tex.to_string();
    }
    let chars: Vec<char> = tex.chars().collect();
    let mut out = String::with_capacity(tex.len() + 16);
    let mut depth = 0usize;
    // 打开文字命令参数时的花括号深度；栈不空就在参数里，不再包。
    let mut text_opened_at: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            let start = i + 1;
            let mut end = start;
            while end < chars.len() && chars[end].is_ascii_alphabetic() {
                end += 1;
            }
            if end == start && end < chars.len() {
                end += 1;
            }
            let name: String = chars[start..end].iter().collect();
            out.push('\\');
            out.push_str(&name);
            i = end;
            if TEXT_CMDS.contains(&name.as_str()) {
                while i < chars.len() && chars[i] == ' ' {
                    i += 1;
                }
                if i < chars.len() && chars[i] == '{' {
                    out.push('{');
                    depth += 1;
                    text_opened_at.push(depth);
                    i += 1;
                }
            }
            continue;
        }
        match c {
            '{' => {
                depth += 1;
                out.push(c);
            }
            '}' => {
                if text_opened_at.last() == Some(&depth) {
                    text_opened_at.pop();
                }
                depth = depth.saturating_sub(1);
                out.push(c);
            }
            c if is_cjk(c) && text_opened_at.is_empty() => {
                out.push_str("\\text{");
                while i < chars.len() && is_cjk(chars[i]) {
                    out.push(chars[i]);
                    i += 1;
                }
                out.push('}');
                continue;
            }
            c => out.push(c),
        }
        i += 1;
    }
    out
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{2e80}'..='\u{9fff}'
        | '\u{f900}'..='\u{faff}'
        | '\u{ff00}'..='\u{ffef}'
        | '\u{ac00}'..='\u{d7af}')
}

// ==================== 定界符 ====================

/// `\(…\)` → `$…$`，`\[…\]` → `$$…$$`，```` ```math ```` 围栏 → `$$…$$`。
/// 代码块与行内代码里的原样保留。
pub fn normalize_delimiters(markdown: &str) -> String {
    if !markdown.contains("\\(") && !markdown.contains("\\[") && !markdown.contains("```math") {
        return markdown.to_string();
    }
    let mut out = String::with_capacity(markdown.len());
    let mut fence: Option<(char, usize, bool)> = None;
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let marker = trimmed.chars().next().filter(|c| *c == '`' || *c == '~');
        if let Some((ch, len, is_math)) = fence {
            let run = trimmed.chars().take_while(|c| *c == ch).count();
            if marker == Some(ch) && run >= len && trimmed[run * ch.len_utf8()..].trim().is_empty() {
                fence = None;
                if is_math {
                    out.push_str("$$");
                    out.push_str(if line.ends_with('\n') { "\n" } else { "" });
                    continue;
                }
            }
            out.push_str(line);
            continue;
        }
        if let Some(ch) = marker {
            let run = trimmed.chars().take_while(|c| *c == ch).count();
            if run >= 3 {
                let info = trimmed[run * ch.len_utf8()..].trim().to_ascii_lowercase();
                let is_math = info == "math";
                fence = Some((ch, run, is_math));
                if is_math {
                    out.push_str("$$\n");
                } else {
                    out.push_str(line);
                }
                continue;
            }
        }
        out.push_str(&inline_delimiters(line));
    }
    out
}

/// 一行里（不在行内代码中）的 `\(` `\)` `\[` `\]` 换成美元符号。
fn inline_delimiters(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut ticks = 0usize;
    while let Some(c) = chars.next() {
        if c == '`' {
            let mut run = 1;
            while chars.peek() == Some(&'`') {
                chars.next();
                run += 1;
            }
            ticks = if ticks == 0 { run } else if ticks == run { 0 } else { ticks };
            out.extend(std::iter::repeat_n('`', run));
            continue;
        }
        if c == '\\' && ticks == 0 {
            match chars.peek() {
                Some('(') | Some(')') => {
                    chars.next();
                    out.push('$');
                    continue;
                }
                Some('[') | Some(']') => {
                    chars.next();
                    out.push_str("$$");
                    continue;
                }
                Some('\\') => {
                    // `\\` 是转义的反斜杠（或公式里的换行），整对照抄，别让第二个
                    // 反斜杠和后面的括号凑成定界符。
                    chars.next();
                    out.push_str("\\\\");
                    continue;
                }
                _ => {}
            }
        }
        out.push(c);
    }
    out
}

// ==================== 文字里的公式 ====================

/// 一段文字里的公式片段（不含定界符）及其字节范围。
struct Span {
    range: std::ops::Range<usize>,
    tex: String,
    display: bool,
}

/// 找出文字里的公式：`$$…$$`、`\[…\]`、`\(…\)`、`$…$`。
///
/// 单个美元符号按 pandoc / GFM 的口径认：开头后面不是空白、结尾前面不是空白、
/// 结尾后面不紧跟数字——「$5 到 $10」「花了$5和$10」都不是公式。
fn spans(text: &str) -> Vec<Span> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut in_code = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'`' {
            in_code = !in_code;
            i += 1;
            continue;
        }
        if in_code {
            i += 1;
            continue;
        }
        let pair = |open: &str, close: &str, display: bool, out: &mut Vec<Span>| -> Option<usize> {
            let body_start = i + open.len();
            let end = text[body_start..].find(close)? + body_start;
            let tex = text[body_start..end].trim();
            if tex.is_empty() {
                return None;
            }
            out.push(Span {
                range: i..end + close.len(),
                tex: tex.to_string(),
                display,
            });
            Some(end + close.len())
        };
        if text[i..].starts_with("$$") {
            if let Some(next) = pair("$$", "$$", true, &mut out) {
                i = next;
                continue;
            }
            i += 2;
            continue;
        }
        if text[i..].starts_with("\\[") {
            if let Some(next) = pair("\\[", "\\]", true, &mut out) {
                i = next;
                continue;
            }
        }
        if text[i..].starts_with("\\(") {
            if let Some(next) = pair("\\(", "\\)", false, &mut out) {
                i = next;
                continue;
            }
        }
        if b == b'\\' {
            // 转义的美元符号等：跳过下一个字符。
            i += 1 + text[i + 1..].chars().next().map_or(0, char::len_utf8);
            continue;
        }
        if b == b'$'
            && let Some(next) = text[i + 1..].chars().next()
            && !next.is_whitespace()
        {
            let mut j = i + 1;
            let mut found = None;
            while j < bytes.len() {
                match bytes[j] {
                    b'\\' => j += 2,
                    b'\n' if bytes.get(j + 1) == Some(&b'\n') => break,
                    b'$' => {
                        let before = text[..j].chars().next_back();
                        let after = text[j + 1..].chars().next();
                        if before.is_some_and(|c| !c.is_whitespace())
                            && !after.is_some_and(|c| c.is_ascii_digit())
                        {
                            found = Some(j);
                        }
                        break;
                    }
                    _ => j += 1,
                }
            }
            if let Some(end) = found {
                out.push(Span {
                    range: i..end + 1,
                    tex: text[i + 1..end].to_string(),
                    display: false,
                });
                i = end + 1;
                continue;
            }
        }
        i += text[i..].chars().next().map_or(1, char::len_utf8);
    }
    out
}

/// 文字里有没有公式。
pub fn has_math(text: &str) -> bool {
    !spans(text).is_empty()
}

/// 公式重到一行字说不清：多行、矩阵/方程组、嵌套分式，或者模型自己用了独立成行的
/// 公式（`$$`、`\[`）——那是要排版看的，换成一行字就丢了结构。
pub fn is_heavy(text: &str) -> bool {
    spans(text).iter().any(|span| {
        span.display
            || span.tex.contains("\\begin")
            || span.tex.contains("\\\\")
            || nested_fraction(&span.tex)
    })
}

fn nested_fraction(tex: &str) -> bool {
    let mut rest = tex;
    while let Some(at) = rest.find("frac") {
        let after = &rest[at + 4..];
        // 分式的两个参数里还有分式。
        if let Some((num, tail)) = group(after)
            && (num.contains("frac") || group(tail).is_some_and(|(den, _)| den.contains("frac")))
        {
            return true;
        }
        rest = after;
    }
    false
}

/// 把文字里的公式换成能直接读的 Unicode 写法；没有公式时原样返回。
pub fn plain(text: &str) -> String {
    let spans = spans(text);
    if spans.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for span in spans {
        out.push_str(&text[last..span.range.start]);
        let converted = to_unicode(&span.tex);
        if span.display {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(converted.trim());
            if !text[span.range.end..].starts_with('\n') && span.range.end < text.len() {
                out.push('\n');
            }
        } else {
            out.push_str(converted.trim());
        }
        last = span.range.end;
    }
    out.push_str(&text[last..]);
    out
}

// ==================== LaTeX → Unicode ====================

/// 读一个参数：`{…}` 整组，或单个字符 / 单个命令。返回参数与剩余部分。
fn group(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    let mut chars = s.char_indices();
    let (_, first) = chars.next()?;
    match first {
        '{' => {
            let mut depth = 0usize;
            let mut escaped = false;
            for (index, c) in s.char_indices() {
                if escaped {
                    escaped = false;
                    continue;
                }
                match c {
                    '\\' => escaped = true,
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some((&s[1..index], &s[index + 1..]));
                        }
                    }
                    _ => {}
                }
            }
            Some((&s[1..], ""))
        }
        '\\' => {
            let name_end = s[1..]
                .char_indices()
                .find(|(_, c)| !c.is_ascii_alphabetic())
                .map_or(s.len(), |(i, _)| i + 1);
            let end = if name_end == 1 {
                1 + s[1..].chars().next().map_or(0, char::len_utf8)
            } else {
                name_end
            };
            Some((&s[..end], &s[end..]))
        }
        c => Some((&s[..c.len_utf8()], &s[c.len_utf8()..])),
    }
}

/// 可选参数 `[…]`。
fn optional(s: &str) -> (Option<&str>, &str) {
    let trimmed = s.trim_start();
    if let Some(body) = trimmed.strip_prefix('[')
        && let Some(end) = body.find(']')
    {
        return (Some(&body[..end]), &body[end + 1..]);
    }
    (None, s)
}

fn symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        "alpha" => "α", "beta" => "β", "gamma" => "γ", "delta" => "δ", "epsilon" => "ϵ",
        "varepsilon" => "ε", "zeta" => "ζ", "eta" => "η", "theta" => "θ", "vartheta" => "ϑ",
        "iota" => "ι", "kappa" => "κ", "lambda" => "λ", "mu" => "μ", "nu" => "ν", "xi" => "ξ",
        "pi" => "π", "varpi" => "ϖ", "rho" => "ρ", "varrho" => "ϱ", "sigma" => "σ",
        "varsigma" => "ς", "tau" => "τ", "upsilon" => "υ", "phi" => "ϕ", "varphi" => "φ",
        "chi" => "χ", "psi" => "ψ", "omega" => "ω", "Gamma" => "Γ", "Delta" => "Δ",
        "Theta" => "Θ", "Lambda" => "Λ", "Xi" => "Ξ", "Pi" => "Π", "Sigma" => "Σ",
        "Upsilon" => "Υ", "Phi" => "Φ", "Psi" => "Ψ", "Omega" => "Ω",
        "times" => "×", "cdot" => "·", "div" => "÷", "pm" => "±", "mp" => "∓",
        "le" | "leq" | "leqslant" => "≤", "ge" | "geq" | "geqslant" => "≥", "ne" | "neq" => "≠",
        "approx" => "≈", "equiv" => "≡", "sim" => "∼", "simeq" => "≃", "cong" => "≅",
        "propto" => "∝", "ll" => "≪", "gg" => "≫", "infty" => "∞", "partial" => "∂",
        "nabla" => "∇", "sum" => "∑", "prod" => "∏", "coprod" => "∐", "int" => "∫",
        "iint" => "∬", "iiint" => "∭", "oint" => "∮", "to" | "rightarrow" => "→",
        "leftarrow" | "gets" => "←", "Rightarrow" => "⇒", "Leftarrow" => "⇐",
        "Leftrightarrow" => "⇔", "leftrightarrow" => "↔", "mapsto" => "↦",
        "implies" | "Longrightarrow" => "⟹", "iff" | "Longleftrightarrow" => "⟺",
        "longrightarrow" => "⟶", "longleftarrow" => "⟵", "uparrow" => "↑",
        "downarrow" => "↓", "in" => "∈", "notin" => "∉", "ni" => "∋", "subset" => "⊂",
        "subseteq" => "⊆", "supset" => "⊃", "supseteq" => "⊇", "subsetneq" => "⊊",
        "cup" => "∪", "cap" => "∩", "bigcup" => "⋃", "bigcap" => "⋂", "setminus" => "∖",
        "emptyset" | "varnothing" => "∅", "forall" => "∀", "exists" => "∃",
        "nexists" => "∄", "neg" | "lnot" => "¬", "land" | "wedge" => "∧",
        "lor" | "vee" => "∨", "oplus" => "⊕", "otimes" => "⊗", "circ" => "∘",
        "bullet" => "•", "star" => "⋆", "ast" => "∗", "cdots" => "⋯",
        "ldots" | "dots" | "dotsc" | "dotsb" => "…", "vdots" => "⋮", "ddots" => "⋱",
        "angle" => "∠", "perp" => "⊥", "parallel" => "∥", "mid" => "∣", "nmid" => "∤",
        "degree" => "°", "prime" => "′", "hbar" => "ℏ", "ell" => "ℓ", "Re" => "ℜ",
        "Im" => "ℑ", "aleph" => "ℵ", "wp" => "℘", "langle" => "⟨", "rangle" => "⟩",
        "lfloor" => "⌊", "rfloor" => "⌋", "lceil" => "⌈", "rceil" => "⌉", "|" | "Vert" => "‖",
        "vert" => "|", "therefore" => "∴", "because" => "∵", "triangle" => "△",
        "square" | "Box" => "□", "checkmark" => "✓", "top" => "⊤", "bot" => "⊥",
        "vdash" => "⊢", "models" => "⊨", "dagger" => "†", "S" => "§", "colon" => ":",
        "lbrace" | "{" => "{", "rbrace" | "}" => "}", "%" => "%", "$" => "$", "&" => "&",
        "#" => "#", "_" => "_", "lt" => "<", "gt" => ">", "backslash" => "\\",
        "," | ":" | ";" | ">" | " " | "space" | "thinspace" | "medspace" => " ",
        "quad" => "  ", "qquad" => "    ", "!" | "negthinspace" => "",
        _ => return None,
    })
}

/// 直接写成名字的函数：\sin x → sin x。
fn is_function(name: &str) -> bool {
    matches!(
        name,
        "sin" | "cos" | "tan" | "cot" | "sec" | "csc" | "arcsin" | "arccos" | "arctan"
            | "sinh" | "cosh" | "tanh" | "coth" | "log" | "ln" | "lg" | "exp" | "lim"
            | "liminf" | "limsup" | "max" | "min" | "sup" | "inf" | "det" | "gcd" | "lcm"
            | "deg" | "dim" | "ker" | "arg" | "Pr" | "hom" | "mod" | "bmod" | "tr" | "rank"
            | "sgn" | "span"
    )
}

/// 只认识这些修饰：名字照抄、版式不要。
fn is_font_command(name: &str) -> bool {
    matches!(
        name,
        "text" | "textrm" | "textbf" | "textit" | "textsf" | "texttt" | "textnormal" | "mbox"
            | "hbox" | "mathrm" | "mathbf" | "mathit" | "mathsf" | "mathtt" | "mathnormal"
            | "boldsymbol" | "bm" | "mathcal" | "mathscr" | "mathfrak" | "operatorname"
            | "displaystyle" | "textstyle" | "emph" | "pmb"
    )
}

fn double_struck(c: char) -> char {
    match c {
        'N' => 'ℕ', 'Z' => 'ℤ', 'Q' => 'ℚ', 'R' => 'ℝ', 'C' => 'ℂ', 'P' => 'ℙ', 'H' => 'ℍ',
        'A'..='Z' => char::from_u32(0x1D538 + (c as u32 - 'A' as u32)).unwrap_or(c),
        'a'..='z' => char::from_u32(0x1D552 + (c as u32 - 'a' as u32)).unwrap_or(c),
        '0'..='9' => char::from_u32(0x1D7D8 + (c as u32 - '0' as u32)).unwrap_or(c),
        _ => c,
    }
}

fn superscript(c: char) -> Option<char> {
    Some(match c {
        '0' => '⁰', '1' => '¹', '2' => '²', '3' => '³', '4' => '⁴', '5' => '⁵', '6' => '⁶',
        '7' => '⁷', '8' => '⁸', '9' => '⁹', '+' => '⁺', '-' | '−' => '⁻', '=' => '⁼',
        '(' => '⁽', ')' => '⁾', 'n' => 'ⁿ', 'i' => 'ⁱ', 'a' => 'ᵃ', 'b' => 'ᵇ', 'c' => 'ᶜ',
        'd' => 'ᵈ', 'e' => 'ᵉ', 'f' => 'ᶠ', 'g' => 'ᵍ', 'h' => 'ʰ', 'j' => 'ʲ', 'k' => 'ᵏ',
        'l' => 'ˡ', 'm' => 'ᵐ', 'o' => 'ᵒ', 'p' => 'ᵖ', 'r' => 'ʳ', 's' => 'ˢ', 't' => 'ᵗ',
        'u' => 'ᵘ', 'v' => 'ᵛ', 'w' => 'ʷ', 'x' => 'ˣ', 'y' => 'ʸ', 'z' => 'ᶻ', 'T' => 'ᵀ',
        '′' => '′', '°' => '°', '*' => '*', ' ' => ' ',
        _ => return None,
    })
}

fn subscript(c: char) -> Option<char> {
    Some(match c {
        '0' => '₀', '1' => '₁', '2' => '₂', '3' => '₃', '4' => '₄', '5' => '₅', '6' => '₆',
        '7' => '₇', '8' => '₈', '9' => '₉', '+' => '₊', '-' | '−' => '₋', '=' => '₌',
        '(' => '₍', ')' => '₎', 'a' => 'ₐ', 'e' => 'ₑ', 'h' => 'ₕ', 'i' => 'ᵢ', 'j' => 'ⱼ',
        'k' => 'ₖ', 'l' => 'ₗ', 'm' => 'ₘ', 'n' => 'ₙ', 'o' => 'ₒ', 'p' => 'ₚ', 'r' => 'ᵣ',
        's' => 'ₛ', 't' => 'ₜ', 'u' => 'ᵤ', 'v' => 'ᵥ', 'x' => 'ₓ', ' ' => ' ',
        _ => return None,
    })
}

/// 上下标：每个字都有 Unicode 上/下标形式就换；否则写成 `^(…)` / `_(…)`。
fn script(body: &str, sup: bool) -> String {
    let body = body.trim();
    let map = if sup { superscript } else { subscript };
    if let Some(mapped) = body.chars().map(map).collect::<Option<String>>() {
        return mapped;
    }
    let mark = if sup { '^' } else { '_' };
    if body.chars().count() == 1 {
        format!("{mark}{body}")
    } else {
        format!("{mark}({body})")
    }
}

/// 一个分式参数要不要加括号：单个记号（数字、字母、带上下标的单字母）不加。
fn needs_parens(text: &str) -> bool {
    let text = text.trim();
    if text.chars().count() <= 1 {
        return false;
    }
    let numeric = text.chars().all(|c| c.is_ascii_digit() || c == '.');
    let word = text.chars().all(char::is_alphanumeric);
    let scripted = {
        let mut chars = text.chars();
        chars.next().is_some_and(char::is_alphanumeric)
            && chars.all(|c| superscript(c).is_none() && subscript(c).is_none() && !c.is_ascii())
    };
    let wrapped = text.starts_with('(') && text.ends_with(')');
    !(numeric || word || scripted || wrapped)
}

fn accent(body: &str, mark: char) -> String {
    let body = body.trim();
    if body.chars().count() == 1 {
        format!("{body}{mark}")
    } else {
        body.to_string()
    }
}

/// LaTeX 片段 → Unicode 文字。只求能读，不求等价：认不得的命令留下名字。
pub fn to_unicode(tex: &str) -> String {
    let mut out = String::new();
    let mut rest = tex;
    while let Some(c) = rest.chars().next() {
        match c {
            '\\' => {
                let (cmd, tail) = group(rest).unwrap_or((rest, ""));
                rest = tail;
                let name = &cmd[1..];
                if let Some(sym) = symbol(name) {
                    out.push_str(sym);
                    continue;
                }
                if is_function(name) {
                    if !out.is_empty() && !out.ends_with([' ', '(']) {
                        out.push(' ');
                    }
                    out.push_str(name);
                    if rest.starts_with(|c: char| c.is_alphanumeric() || c == '\\') {
                        out.push(' ');
                    }
                    continue;
                }
                match name {
                    "frac" | "dfrac" | "tfrac" | "cfrac" => {
                        let (num, tail) = group(rest).unwrap_or(("", ""));
                        let (den, tail) = group(tail).unwrap_or(("", ""));
                        rest = tail;
                        let (num, den) = (to_unicode(num), to_unicode(den));
                        let wrap = |s: String| if needs_parens(&s) { format!("({})", s.trim()) } else { s.trim().to_string() };
                        out.push_str(&format!("{}/{}", wrap(num), wrap(den)));
                    }
                    "binom" | "dbinom" | "tbinom" => {
                        let (n, tail) = group(rest).unwrap_or(("", ""));
                        let (k, tail) = group(tail).unwrap_or(("", ""));
                        rest = tail;
                        out.push_str(&format!("C({}, {})", to_unicode(n).trim(), to_unicode(k).trim()));
                    }
                    "sqrt" => {
                        let (index, tail) = optional(rest);
                        let (body, tail) = group(tail).unwrap_or(("", ""));
                        rest = tail;
                        let body = to_unicode(body);
                        let root = match index.map(str::trim) {
                            None | Some("2") => "√".to_string(),
                            Some("3") => "∛".to_string(),
                            Some("4") => "∜".to_string(),
                            Some(n) => format!("{}√", script(&to_unicode(n), true)),
                        };
                        out.push_str(&root);
                        if body.trim().chars().count() > 1 && !(body.starts_with('(') && body.ends_with(')')) {
                            out.push_str(&format!("({})", body.trim()));
                        } else {
                            out.push_str(body.trim());
                        }
                    }
                    "mathbb" | "Bbb" => {
                        let (body, tail) = group(rest).unwrap_or(("", ""));
                        rest = tail;
                        out.extend(body.trim().chars().map(double_struck));
                    }
                    "bar" | "overline" => {
                        let (body, tail) = group(rest).unwrap_or(("", ""));
                        rest = tail;
                        out.push_str(&accent(&to_unicode(body), '\u{0305}'));
                    }
                    "hat" | "widehat" => {
                        let (body, tail) = group(rest).unwrap_or(("", ""));
                        rest = tail;
                        out.push_str(&accent(&to_unicode(body), '\u{0302}'));
                    }
                    "vec" | "overrightarrow" => {
                        let (body, tail) = group(rest).unwrap_or(("", ""));
                        rest = tail;
                        out.push_str(&accent(&to_unicode(body), '\u{20D7}'));
                    }
                    "dot" => {
                        let (body, tail) = group(rest).unwrap_or(("", ""));
                        rest = tail;
                        out.push_str(&accent(&to_unicode(body), '\u{0307}'));
                    }
                    "ddot" => {
                        let (body, tail) = group(rest).unwrap_or(("", ""));
                        rest = tail;
                        out.push_str(&accent(&to_unicode(body), '\u{0308}'));
                    }
                    "tilde" | "widetilde" => {
                        let (body, tail) = group(rest).unwrap_or(("", ""));
                        rest = tail;
                        out.push_str(&accent(&to_unicode(body), '\u{0303}'));
                    }
                    "boxed" | "underline" | "underbrace" | "overbrace" | "cancel" | "phantom" => {
                        let (body, tail) = group(rest).unwrap_or(("", ""));
                        rest = tail;
                        if name != "phantom" {
                            out.push_str(&to_unicode(body));
                        }
                    }
                    "begin" | "end" => {
                        // 环境名不读出来；矩阵与方程组的结构交给 `&` 与 `\\` 的替换。
                        let (env, tail) = group(rest).unwrap_or(("", ""));
                        rest = tail;
                        // `\begin{array}{cc}` 的列格式不是内容。
                        if name == "begin" && matches!(env, "array" | "tabular") {
                            rest = group(rest).map_or(rest, |(_, tail)| tail);
                        }
                    }
                    "\\" | "newline" | "cr" => out.push_str("; "),
                    "left" | "right" | "big" | "Big" | "bigg" | "Bigg" | "bigl" | "bigr"
                    | "Bigl" | "Bigr" | "biggl" | "biggr" | "middle" | "limits" | "nolimits"
                    | "nonumber" | "notag" => {
                        // `\left.` 的点是空定界符。
                        if matches!(name, "left" | "right" | "middle") && rest.starts_with('.') {
                            rest = &rest[1..];
                        }
                    }
                    "tag" => {
                        let (body, tail) = group(rest).unwrap_or(("", ""));
                        rest = tail;
                        out.push_str(&format!("  ({})", to_unicode(body).trim()));
                    }
                    name if is_font_command(name) => {
                        if matches!(name, "displaystyle" | "textstyle") {
                            continue;
                        }
                        let (body, tail) = group(rest).unwrap_or(("", ""));
                        rest = tail;
                        if name.starts_with("text") || name == "mbox" || name == "hbox" {
                            out.push_str(body);
                        } else {
                            out.push_str(&to_unicode(body));
                        }
                    }
                    other => out.push_str(other),
                }
            }
            '^' | '_' => {
                let (body, tail) = group(&rest[1..]).unwrap_or(("", ""));
                rest = tail;
                let converted = match body.trim() {
                    "\\circ" if c == '^' => "°".to_string(),
                    "\\prime" if c == '^' => "′".to_string(),
                    "\\top" | "T" if c == '^' => "ᵀ".to_string(),
                    other => script(&to_unicode(other), c == '^'),
                };
                out.push_str(&converted);
            }
            '{' => {
                let (body, tail) = group(rest).unwrap_or(("", ""));
                rest = tail;
                out.push_str(&to_unicode(body));
            }
            '}' => rest = &rest[1..],
            '&' => {
                rest = &rest[1..];
                out.push(' ');
            }
            '~' => {
                rest = &rest[1..];
                out.push(' ');
            }
            '\'' => {
                rest = &rest[1..];
                out.push('′');
            }
            '-' => {
                rest = &rest[1..];
                out.push('−');
            }
            c => {
                rest = &rest[c.len_utf8()..];
                out.push(c);
            }
        }
    }
    tidy(&out)
}

/// 压掉连续空白、运算符两边留一个空格之外的多余空格。
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c == ' ' {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    out.trim_end_matches([';', ' ']).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_unicode_for_chat() {
        assert_eq!(to_unicode(r"x^2 + y^2 = r^2"), "x² + y² = r²");
        assert_eq!(to_unicode(r"\frac{1}{2}"), "1/2");
        assert_eq!(to_unicode(r"\frac{a+b}{c}"), "(a+b)/c");
        assert_eq!(to_unicode(r"\sqrt{2}"), "√2");
        assert_eq!(to_unicode(r"\sqrt[3]{x+1}"), "∛(x+1)");
        assert_eq!(to_unicode(r"\alpha \leq \beta"), "α ≤ β");
        assert_eq!(to_unicode(r"a_{n+1} = a_n \cdot q"), "aₙ₊₁ = aₙ · q");
        assert_eq!(to_unicode(r"\mathbb{R}^n"), "ℝⁿ");
        assert_eq!(to_unicode(r"90^\circ"), "90°");
        assert_eq!(to_unicode(r"\sin x + \cos^2 x"), "sin x + cos² x");
        assert_eq!(to_unicode(r"\text{速度} = \frac{s}{t}"), "速度 = s/t");
        assert_eq!(to_unicode(r"e^{i\pi} + 1 = 0"), "e^(iπ) + 1 = 0");
        assert_eq!(to_unicode(r"\left( x \right)"), "( x )");
    }

    #[test]
    fn finds_math_in_chat_text() {
        assert_eq!(plain("面积是 $\\pi r^2$。"), "面积是 π r²。");
        assert_eq!(plain("答案 \\(x=2\\) 对吗"), "答案 x=2 对吗");
        // 钱不是公式。
        assert_eq!(plain("$5 到 $10"), "$5 到 $10");
        assert_eq!(plain("花了$5和$10"), "花了$5和$10");
        assert_eq!(plain("`$x$` 原样"), "`$x$` 原样");
        assert!(is_heavy("$$\\int_0^1 x\\,dx$$"));
        assert!(is_heavy("$\\begin{pmatrix}1&2\\end{pmatrix}$"));
        assert!(is_heavy("$\\frac{\\frac{1}{2}}{3}$"));
        assert!(!is_heavy("$\\frac{1}{2}$ 和 $x^2$"));
    }

    #[test]
    fn delimiters_become_dollars_outside_code() {
        assert_eq!(normalize_delimiters("a \\(x\\) b"), "a $x$ b");
        assert_eq!(normalize_delimiters("\\[\nx\n\\]"), "$$\nx\n$$");
        assert_eq!(normalize_delimiters("`\\(x\\)`"), "`\\(x\\)`");
        assert_eq!(
            normalize_delimiters("```math\nx^2\n```\n"),
            "$$\nx^2\n$$\n"
        );
        assert_eq!(
            normalize_delimiters("```rust\nlet a = \"\\(\";\n```\n"),
            "```rust\nlet a = \"\\(\";\n```\n"
        );
        // 转义的反斜杠后面跟括号不是定界符。
        assert_eq!(normalize_delimiters("a\\\\[b] \\(y\\)"), "a\\\\[b] $y$");
    }

    #[test]
    fn mathml_wraps_cjk_and_survives_errors() {
        let math = mathml(r"\text{面积} = 长 \times 宽", false).unwrap();
        assert!(math.starts_with("<math"), "{math}");
        assert!(!math.contains("<mi>长</mi>"), "{math}");
        assert!(display_html(r"\sum_{i=1}^n i").contains("display=\"block\""));
        assert!(inline_html(r"\unknowncmd x").contains("<math"));
    }
}
