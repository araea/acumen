//! 代码块的语法着色：不引外部依赖的轻量扫描器。
//!
//! 只认「注释 / 字符串 / 数字 / 关键字 / 字面量 / 函数与宏 / 类型 / 键名」几类，
//! 够读、不追求词法完备。认不出的语言返回 `None`，由调用方原样转义——
//! 宁可素着，也不给错误的颜色。
//!
//! 产出的类名（`tk-*`）在 `res/cards/markdown.css` 里落到 M3 角色色上，
//! 每一对前景与代码面的对比度由 `markdown::render` 的测试按 WCAG AA 卡住。

use crate::render::web::esc;

/// 语言族。
enum Family {
    /// 通用扫描：注释、字符串、数字、标识符。
    Generic(&'static Spec),
    /// JSON：键与值分色，无关键字。
    Json {
        comments: bool,
    },
    /// YAML / TOML / INI 一类逐行「键 = 值」。
    Config {
        toml: bool,
        ini: bool,
    },
    Html,
    Css,
    Diff,
}

struct Spec {
    line: &'static [&'static str],
    block: Option<(&'static str, &'static str)>,
    quotes: &'static [char],
    triple: bool,
    keywords: &'static [&'static str],
    literals: &'static [&'static str],
    /// 「关键字 + 名字」里，名字着函数色的关键字。
    fn_kw: &'static [&'static str],
    /// 「关键字 + 名字」里，名字着类型色的关键字。
    type_kw: &'static [&'static str],
    /// `名字!(` 是宏调用。
    macros: bool,
    /// `#include` 一类预处理指令。
    directive: bool,
    /// `$名字` 是变量。
    dollar: bool,
    /// Rust 的生命周期 `'a`。
    lifetime: bool,
    /// 关键字不分大小写（SQL）。
    fold: bool,
}

const NONE: &[&str] = &[];

const RUST: Spec = Spec {
    line: &["//"],
    block: Some(("/*", "*/")),
    quotes: &['"', '\''],
    triple: false,
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
        "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "type",
        "unsafe", "use", "where", "while",
    ],
    literals: &["true", "false", "None"],
    fn_kw: &["fn"],
    type_kw: &["struct", "enum", "trait", "type", "impl", "mod"],
    macros: true,
    directive: false,
    dollar: false,
    lifetime: true,
    fold: false,
};

const PYTHON: Spec = Spec {
    line: &["#"],
    block: None,
    quotes: &['"', '\''],
    triple: true,
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is",
        "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with",
        "yield", "match", "case",
    ],
    literals: &["True", "False", "None"],
    fn_kw: &["def"],
    type_kw: &["class"],
    macros: false,
    directive: false,
    dollar: false,
    lifetime: false,
    fold: false,
};

const JS: Spec = Spec {
    line: &["//"],
    block: Some(("/*", "*/")),
    quotes: &['"', '\'', '`'],
    triple: false,
    keywords: &[
        "abstract",
        "as",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "declare",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "finally",
        "for",
        "from",
        "function",
        "if",
        "implements",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "namespace",
        "new",
        "of",
        "private",
        "protected",
        "public",
        "readonly",
        "return",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "try",
        "type",
        "typeof",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ],
    literals: &["true", "false", "null", "undefined", "NaN", "Infinity"],
    fn_kw: &["function"],
    type_kw: &["class", "interface", "enum", "type", "namespace"],
    macros: false,
    directive: false,
    dollar: false,
    lifetime: false,
    fold: false,
};

const GO: Spec = Spec {
    line: &["//"],
    block: Some(("/*", "*/")),
    quotes: &['"', '\'', '`'],
    triple: false,
    keywords: &[
        "break",
        "case",
        "chan",
        "const",
        "continue",
        "default",
        "defer",
        "else",
        "fallthrough",
        "for",
        "func",
        "go",
        "goto",
        "if",
        "import",
        "interface",
        "map",
        "package",
        "range",
        "return",
        "select",
        "struct",
        "switch",
        "type",
        "var",
    ],
    literals: &["true", "false", "nil", "iota"],
    fn_kw: &["func"],
    type_kw: &["type"],
    macros: false,
    directive: false,
    dollar: false,
    lifetime: false,
    fold: false,
};

const C: Spec = Spec {
    line: &["//"],
    block: Some(("/*", "*/")),
    quotes: &['"', '\''],
    triple: false,
    keywords: &[
        "auto", "break", "case", "char", "const", "continue", "default", "do", "double", "else",
        "enum", "extern", "float", "for", "goto", "if", "inline", "int", "long", "register",
        "restrict", "return", "short", "signed", "sizeof", "static", "struct", "switch", "typedef",
        "union", "unsigned", "void", "volatile", "while",
    ],
    literals: &["NULL", "true", "false"],
    fn_kw: NONE,
    type_kw: &["struct", "enum", "union"],
    macros: false,
    directive: true,
    dollar: false,
    lifetime: false,
    fold: false,
};

const CPP: Spec = Spec {
    line: &["//"],
    block: Some(("/*", "*/")),
    quotes: &['"', '\''],
    triple: false,
    keywords: &[
        "auto",
        "bool",
        "break",
        "case",
        "catch",
        "char",
        "class",
        "const",
        "constexpr",
        "continue",
        "default",
        "delete",
        "do",
        "double",
        "else",
        "enum",
        "explicit",
        "extern",
        "final",
        "float",
        "for",
        "friend",
        "goto",
        "if",
        "inline",
        "int",
        "long",
        "mutable",
        "namespace",
        "new",
        "noexcept",
        "operator",
        "override",
        "private",
        "protected",
        "public",
        "return",
        "short",
        "signed",
        "sizeof",
        "static",
        "static_cast",
        "struct",
        "switch",
        "template",
        "this",
        "throw",
        "try",
        "typedef",
        "typename",
        "union",
        "unsigned",
        "using",
        "virtual",
        "void",
        "volatile",
        "while",
    ],
    literals: &["nullptr", "NULL", "true", "false"],
    fn_kw: NONE,
    type_kw: &["class", "struct", "enum", "union", "namespace"],
    macros: false,
    directive: true,
    dollar: false,
    lifetime: false,
    fold: false,
};

const JAVA: Spec = Spec {
    line: &["//"],
    block: Some(("/*", "*/")),
    quotes: &['"', '\''],
    triple: false,
    keywords: &[
        "abstract",
        "assert",
        "boolean",
        "break",
        "byte",
        "case",
        "catch",
        "char",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "double",
        "else",
        "enum",
        "extends",
        "final",
        "finally",
        "float",
        "for",
        "if",
        "implements",
        "import",
        "instanceof",
        "int",
        "interface",
        "long",
        "native",
        "new",
        "package",
        "private",
        "protected",
        "public",
        "record",
        "return",
        "short",
        "static",
        "super",
        "switch",
        "synchronized",
        "this",
        "throw",
        "throws",
        "transient",
        "try",
        "var",
        "void",
        "volatile",
        "while",
    ],
    literals: &["true", "false", "null"],
    fn_kw: NONE,
    type_kw: &["class", "interface", "enum", "record"],
    macros: false,
    directive: false,
    dollar: false,
    lifetime: false,
    fold: false,
};

const KOTLIN: Spec = Spec {
    line: &["//"],
    block: Some(("/*", "*/")),
    quotes: &['"', '\''],
    triple: true,
    keywords: &[
        "abstract",
        "as",
        "break",
        "by",
        "catch",
        "class",
        "companion",
        "const",
        "continue",
        "data",
        "do",
        "else",
        "enum",
        "final",
        "finally",
        "for",
        "fun",
        "if",
        "import",
        "in",
        "inline",
        "interface",
        "internal",
        "is",
        "lateinit",
        "object",
        "open",
        "override",
        "package",
        "private",
        "protected",
        "public",
        "return",
        "sealed",
        "super",
        "suspend",
        "this",
        "throw",
        "try",
        "typealias",
        "val",
        "var",
        "when",
        "while",
    ],
    literals: &["true", "false", "null"],
    fn_kw: &["fun"],
    type_kw: &["class", "interface", "object", "enum"],
    macros: false,
    directive: false,
    dollar: false,
    lifetime: false,
    fold: false,
};

const CSHARP: Spec = Spec {
    line: &["//"],
    block: Some(("/*", "*/")),
    quotes: &['"', '\''],
    triple: false,
    keywords: &[
        "abstract",
        "as",
        "async",
        "await",
        "base",
        "bool",
        "break",
        "byte",
        "case",
        "catch",
        "char",
        "class",
        "const",
        "continue",
        "decimal",
        "default",
        "delegate",
        "do",
        "double",
        "else",
        "enum",
        "event",
        "extern",
        "finally",
        "float",
        "for",
        "foreach",
        "if",
        "in",
        "int",
        "interface",
        "internal",
        "is",
        "lock",
        "long",
        "namespace",
        "new",
        "object",
        "out",
        "override",
        "params",
        "private",
        "protected",
        "public",
        "readonly",
        "record",
        "ref",
        "return",
        "sealed",
        "short",
        "static",
        "string",
        "struct",
        "switch",
        "this",
        "throw",
        "try",
        "typeof",
        "uint",
        "ulong",
        "using",
        "var",
        "virtual",
        "void",
        "while",
    ],
    literals: &["true", "false", "null"],
    fn_kw: NONE,
    type_kw: &[
        "class",
        "interface",
        "enum",
        "struct",
        "record",
        "namespace",
    ],
    macros: false,
    directive: true,
    dollar: false,
    lifetime: false,
    fold: false,
};

const SWIFT: Spec = Spec {
    line: &["//"],
    block: Some(("/*", "*/")),
    quotes: &['"'],
    triple: true,
    keywords: &[
        "actor",
        "as",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "continue",
        "default",
        "defer",
        "do",
        "else",
        "enum",
        "extension",
        "fallthrough",
        "fileprivate",
        "for",
        "func",
        "guard",
        "if",
        "import",
        "in",
        "init",
        "inout",
        "internal",
        "is",
        "let",
        "open",
        "operator",
        "private",
        "protocol",
        "public",
        "repeat",
        "return",
        "self",
        "Self",
        "static",
        "struct",
        "super",
        "switch",
        "throw",
        "throws",
        "try",
        "typealias",
        "var",
        "where",
        "while",
    ],
    literals: &["true", "false", "nil"],
    fn_kw: &["func"],
    type_kw: &["class", "struct", "enum", "protocol", "extension", "actor"],
    macros: false,
    directive: false,
    dollar: false,
    lifetime: false,
    fold: false,
};

const PHP: Spec = Spec {
    line: &["//", "#"],
    block: Some(("/*", "*/")),
    quotes: &['"', '\''],
    triple: false,
    keywords: &[
        "abstract",
        "as",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "echo",
        "else",
        "elseif",
        "extends",
        "final",
        "finally",
        "fn",
        "for",
        "foreach",
        "function",
        "global",
        "if",
        "implements",
        "include",
        "interface",
        "namespace",
        "new",
        "private",
        "protected",
        "public",
        "require",
        "return",
        "static",
        "switch",
        "throw",
        "trait",
        "try",
        "use",
        "var",
        "while",
        "yield",
    ],
    literals: &["true", "false", "null", "TRUE", "FALSE", "NULL"],
    fn_kw: &["function"],
    type_kw: &["class", "interface", "trait"],
    macros: false,
    directive: false,
    dollar: true,
    lifetime: false,
    fold: false,
};

const RUBY: Spec = Spec {
    line: &["#"],
    block: None,
    quotes: &['"', '\''],
    triple: false,
    keywords: &[
        "and", "begin", "break", "case", "class", "def", "do", "else", "elsif", "end", "ensure",
        "for", "if", "in", "module", "next", "not", "or", "raise", "redo", "require", "rescue",
        "retry", "return", "self", "super", "then", "unless", "until", "when", "while", "yield",
    ],
    literals: &["true", "false", "nil"],
    fn_kw: &["def"],
    type_kw: &["class", "module"],
    macros: false,
    directive: false,
    dollar: false,
    lifetime: false,
    fold: false,
};

const LUA: Spec = Spec {
    line: &["--"],
    block: Some(("--[[", "]]")),
    quotes: &['"', '\''],
    triple: false,
    keywords: &[
        "and", "break", "do", "else", "elseif", "end", "for", "function", "goto", "if", "in",
        "local", "not", "or", "repeat", "return", "then", "until", "while",
    ],
    literals: &["true", "false", "nil"],
    fn_kw: &["function"],
    type_kw: NONE,
    macros: false,
    directive: false,
    dollar: false,
    lifetime: false,
    fold: false,
};

const SQL: Spec = Spec {
    line: &["--"],
    block: Some(("/*", "*/")),
    quotes: &['\'', '"', '`'],
    triple: false,
    keywords: &[
        "add",
        "all",
        "alter",
        "and",
        "as",
        "asc",
        "begin",
        "between",
        "by",
        "case",
        "check",
        "commit",
        "constraint",
        "create",
        "cross",
        "database",
        "default",
        "delete",
        "desc",
        "distinct",
        "drop",
        "else",
        "end",
        "exists",
        "foreign",
        "from",
        "full",
        "group",
        "having",
        "if",
        "in",
        "index",
        "inner",
        "insert",
        "into",
        "is",
        "join",
        "key",
        "left",
        "like",
        "limit",
        "not",
        "offset",
        "on",
        "or",
        "order",
        "outer",
        "primary",
        "references",
        "returning",
        "right",
        "rollback",
        "select",
        "set",
        "table",
        "then",
        "union",
        "unique",
        "update",
        "values",
        "view",
        "when",
        "where",
        "with",
    ],
    literals: &["true", "false", "null"],
    fn_kw: NONE,
    type_kw: NONE,
    macros: false,
    directive: false,
    dollar: false,
    lifetime: false,
    fold: true,
};

const BASH: Spec = Spec {
    line: &["#"],
    block: None,
    quotes: &['"', '\''],
    triple: false,
    keywords: &[
        "alias", "case", "do", "done", "elif", "else", "esac", "exit", "export", "fi", "for",
        "function", "if", "in", "local", "readonly", "return", "select", "shift", "source", "then",
        "unset", "until", "while",
    ],
    literals: &["true", "false"],
    fn_kw: &["function"],
    type_kw: NONE,
    macros: false,
    directive: false,
    dollar: true,
    lifetime: false,
    fold: false,
};

const VALUE: Spec = Spec {
    line: &["#"],
    block: None,
    quotes: &['"', '\''],
    triple: false,
    keywords: NONE,
    literals: &["true", "false", "null", "yes", "no", "on", "off", "~"],
    fn_kw: NONE,
    type_kw: NONE,
    macros: false,
    directive: false,
    dollar: false,
    lifetime: false,
    fold: false,
};

fn family(lang: &str) -> Option<Family> {
    let lang = lang
        .trim()
        .trim_start_matches('.')
        .split(|c: char| c.is_whitespace() || c == '{' || c == ',')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    Some(match lang.as_str() {
        "rust" | "rs" => Family::Generic(&RUST),
        "python" | "py" | "python3" | "py3" => Family::Generic(&PYTHON),
        "javascript" | "js" | "jsx" | "mjs" | "cjs" | "typescript" | "ts" | "tsx" | "node" => {
            Family::Generic(&JS)
        }
        "go" | "golang" => Family::Generic(&GO),
        "c" | "h" => Family::Generic(&C),
        "cpp" | "c++" | "cc" | "cxx" | "hpp" | "hh" => Family::Generic(&CPP),
        "java" => Family::Generic(&JAVA),
        "kotlin" | "kt" | "kts" => Family::Generic(&KOTLIN),
        "csharp" | "cs" | "c#" => Family::Generic(&CSHARP),
        "swift" => Family::Generic(&SWIFT),
        "php" => Family::Generic(&PHP),
        "ruby" | "rb" => Family::Generic(&RUBY),
        "lua" => Family::Generic(&LUA),
        "sql" | "mysql" | "postgresql" | "postgres" | "sqlite" | "pgsql" => Family::Generic(&SQL),
        "bash" | "sh" | "shell" | "zsh" | "console" | "shellsession" | "terminal" => {
            Family::Generic(&BASH)
        }
        "json" => Family::Json { comments: false },
        "jsonc" | "json5" => Family::Json { comments: true },
        "yaml" | "yml" => Family::Config {
            toml: false,
            ini: false,
        },
        "toml" => Family::Config {
            toml: true,
            ini: false,
        },
        "ini" | "conf" | "cfg" | "properties" | "env" | "dotenv" => Family::Config {
            toml: false,
            ini: true,
        },
        "html" | "xml" | "svg" | "vue" | "xhtml" | "htm" => Family::Html,
        "css" | "scss" | "less" => Family::Css,
        "diff" | "patch" => Family::Diff,
        _ => return None,
    })
}

/// 语言标签的展示名：`js` → `JavaScript` 这类别名归一，认不出的原样大写。
pub fn display_name(lang: &str) -> String {
    let raw = lang
        .trim()
        .split(|c: char| c.is_whitespace() || c == '{' || c == ',')
        .next()
        .unwrap_or("");
    let name = match raw.to_ascii_lowercase().as_str() {
        "rs" | "rust" => "Rust",
        "py" | "python" | "python3" | "py3" => "Python",
        "js" | "javascript" | "mjs" | "cjs" | "node" => "JavaScript",
        "jsx" => "JSX",
        "ts" | "typescript" => "TypeScript",
        "tsx" => "TSX",
        "go" | "golang" => "Go",
        "c" | "h" => "C",
        "cpp" | "c++" | "cc" | "cxx" | "hpp" | "hh" => "C++",
        "cs" | "csharp" | "c#" => "C#",
        "kt" | "kts" | "kotlin" => "Kotlin",
        "rb" | "ruby" => "Ruby",
        "sh" | "bash" | "shell" | "zsh" | "console" | "shellsession" | "terminal" => "Shell",
        "yml" | "yaml" => "YAML",
        "md" | "markdown" => "Markdown",
        "json" => "JSON",
        "jsonc" => "JSONC",
        "toml" => "TOML",
        "sql" => "SQL",
        "html" | "htm" | "xhtml" => "HTML",
        "xml" => "XML",
        "svg" => "SVG",
        "css" => "CSS",
        "scss" => "SCSS",
        "less" => "Less",
        "php" => "PHP",
        "lua" => "Lua",
        "swift" => "Swift",
        "java" => "Java",
        "diff" | "patch" => "Diff",
        "" => return String::new(),
        _ => return raw.to_string(),
    };
    name.to_string()
}

/// 着色。认不出语言、或代码太长（着色只为好看，不值得为它多花时间）时返回 `None`。
pub fn highlight(lang: &str, code: &str) -> Option<String> {
    if code.len() > 60_000 {
        return None;
    }
    Some(match family(lang)? {
        Family::Generic(spec) => scan(spec, code, None),
        Family::Json { comments } => scan_json(code, comments),
        Family::Config { toml, ini } => scan_config(code, toml, ini),
        Family::Html => scan_html(code),
        Family::Css => scan_css(code),
        Family::Diff => scan_diff(code),
    })
}

fn put(out: &mut String, class: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    out.push_str("<span class=\"tk-");
    out.push_str(class);
    out.push_str("\">");
    out.push_str(&esc(text));
    out.push_str("</span>");
}

fn plain(out: &mut String, text: &str) {
    out.push_str(&esc(text));
}

fn starts(chars: &[char], at: usize, pat: &str) -> bool {
    let mut i = at;
    for p in pat.chars() {
        if chars.get(i) != Some(&p) {
            return false;
        }
        i += 1;
    }
    true
}

fn ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}
fn ident_part(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn text_of(chars: &[char], from: usize, to: usize) -> String {
    chars[from..to].iter().collect()
}

/// 一个引号串的结尾位置（不含）。`multiline` 为假时遇到换行就收。
fn string_end(chars: &[char], start: usize, quote: char, triple: bool, multiline: bool) -> usize {
    let n = chars.len();
    let mut i = start + 1;
    if triple && starts(chars, start, &quote.to_string().repeat(3)) {
        i = start + 3;
        while i < n {
            if chars[i] == '\\' {
                i += 2;
                continue;
            }
            if starts(chars, i, &quote.to_string().repeat(3)) {
                return i + 3;
            }
            i += 1;
        }
        return n;
    }
    while i < n {
        match chars[i] {
            '\\' if quote != '`' => i += 2,
            c if c == quote => return i + 1,
            '\n' if !multiline => return i,
            _ => i += 1,
        }
    }
    n
}

fn number_end(chars: &[char], start: usize) -> usize {
    let n = chars.len();
    let mut i = start;
    while i < n {
        let c = chars[i];
        if c.is_ascii_alphanumeric() || c == '_' {
            // 科学计数法的符号：1e-3 / 2E+8
            i += 1;
            if matches!(c, 'e' | 'E')
                && !chars[start..i].iter().any(|d| matches!(d, 'x' | 'X'))
                && matches!(chars.get(i), Some('+') | Some('-'))
                && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())
            {
                i += 1;
            }
        } else if c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit()) {
            i += 1;
        } else {
            break;
        }
    }
    i
}

/// 通用扫描。`hint` 是 JSON 的键判定（值为真时，后跟冒号的字符串着键色）。
fn scan(spec: &Spec, code: &str, keys: Option<()>) -> String {
    let chars: Vec<char> = code.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(code.len() * 2);
    let mut i = 0;
    let mut plain_from = 0;
    // 上一个关键字，用来给「fn 名字」「class 名字」着色。
    let mut last_kw: Option<&'static str> = None;

    macro_rules! flush {
        ($upto:expr) => {
            if plain_from < $upto {
                plain(&mut out, &text_of(&chars, plain_from, $upto));
            }
        };
    }

    while i < n {
        let c = chars[i];

        // 行注释。`#` 只在词首才算（`$#`、`a#b` 都不是注释）。
        if let Some(pat) = spec.line.iter().find(|p| starts(&chars, i, p)) {
            let boundary = *pat != "#" || i == 0 || chars[i - 1].is_whitespace();
            // 块注释起始优先于同前缀的行注释（Lua 的 `--[[`）。
            let block_first = spec.block.is_some_and(|(open, _)| starts(&chars, i, open));
            if boundary && !block_first {
                flush!(i);
                let end = chars[i..]
                    .iter()
                    .position(|&d| d == '\n')
                    .map_or(n, |p| i + p);
                put(&mut out, "c", &text_of(&chars, i, end));
                i = end;
                plain_from = i;
                continue;
            }
        }
        // 块注释。
        if let Some((open, close)) = spec.block
            && starts(&chars, i, open)
        {
            flush!(i);
            let mut end = i + open.chars().count();
            while end < n && !starts(&chars, end, close) {
                end += 1;
            }
            end = (end + close.chars().count()).min(n);
            put(&mut out, "c", &text_of(&chars, i, end));
            i = end;
            plain_from = i;
            continue;
        }
        // C 系预处理指令：行首的 `#include` / `#define`。
        if spec.directive
            && c == '#'
            && chars[..i]
                .iter()
                .rev()
                .take_while(|&&d| d != '\n')
                .all(|d| d.is_whitespace())
        {
            flush!(i);
            let mut end = i + 1;
            while end < n && (chars[end].is_whitespace() && chars[end] != '\n') {
                end += 1;
            }
            while end < n && ident_part(chars[end]) {
                end += 1;
            }
            put(&mut out, "t", &text_of(&chars, i, end));
            i = end;
            plain_from = i;
            continue;
        }
        // 字符串。
        if spec.quotes.contains(&c) {
            // Rust 的 'a 生命周期：引号后紧跟标识符、且两三个字符内没有收尾引号。
            if spec.lifetime && c == '\'' {
                let is_char = match (chars.get(i + 1), chars.get(i + 2), chars.get(i + 3)) {
                    (Some('\\'), _, _) => true,
                    (Some(_), Some('\''), _) => true,
                    _ => false,
                };
                if !is_char && chars.get(i + 1).is_some_and(|&d| ident_start(d)) {
                    flush!(i);
                    let mut end = i + 1;
                    while end < n && ident_part(chars[end]) {
                        end += 1;
                    }
                    put(&mut out, "t", &text_of(&chars, i, end));
                    i = end;
                    plain_from = i;
                    continue;
                }
            }
            flush!(i);
            let end = string_end(&chars, i, c, spec.triple, c == '`');
            let class = if keys.is_some() && string_is_key(&chars, end) {
                "a"
            } else {
                "s"
            };
            put(&mut out, class, &text_of(&chars, i, end));
            i = end;
            plain_from = i;
            continue;
        }
        // 变量：$name / ${name}
        if spec.dollar
            && c == '$'
            && chars
                .get(i + 1)
                .is_some_and(|&d| ident_start(d) || d == '{')
        {
            flush!(i);
            let mut end = i + 1;
            if chars[end] == '{' {
                while end < n && chars[end] != '}' && chars[end] != '\n' {
                    end += 1;
                }
                end = (end + 1).min(n);
            } else {
                while end < n && ident_part(chars[end]) {
                    end += 1;
                }
            }
            put(&mut out, "a", &text_of(&chars, i, end));
            i = end;
            plain_from = i;
            continue;
        }
        // 数字（前一个字符不是标识符的一部分）。
        if c.is_ascii_digit() && (i == 0 || !ident_part(chars[i - 1])) {
            flush!(i);
            let end = number_end(&chars, i);
            put(&mut out, "n", &text_of(&chars, i, end));
            i = end;
            plain_from = i;
            last_kw = None;
            continue;
        }
        // 标识符。
        if ident_start(c) {
            let mut end = i + 1;
            while end < n && ident_part(chars[end]) {
                end += 1;
            }
            let word = text_of(&chars, i, end);
            let lookup = |list: &[&str]| {
                if spec.fold {
                    list.iter().any(|k| k.eq_ignore_ascii_case(&word))
                } else {
                    list.contains(&word.as_str())
                }
            };
            let mut next = end;
            while next < n && (chars[next] == ' ' || chars[next] == '\t') {
                next += 1;
            }
            let after = chars.get(next).copied();
            let class = if lookup(spec.keywords) {
                last_kw = spec
                    .fn_kw
                    .iter()
                    .chain(spec.type_kw)
                    .find(|k| **k == word)
                    .copied();
                Some("k")
            } else if lookup(spec.literals) {
                last_kw = None;
                Some("n")
            } else {
                let by_kw = last_kw.take();
                if by_kw.is_some_and(|k| spec.fn_kw.contains(&k)) {
                    Some("f")
                } else if by_kw.is_some_and(|k| spec.type_kw.contains(&k)) {
                    Some("t")
                } else if after == Some('(') {
                    Some("f")
                } else if spec.macros && after == Some('!') && chars.get(next + 1) != Some(&'=') {
                    Some("f")
                } else if word.chars().next().is_some_and(char::is_uppercase)
                    && word.chars().any(char::is_lowercase)
                {
                    Some("t")
                } else {
                    None
                }
            };
            flush!(i);
            match class {
                Some(class) => put(&mut out, class, &word),
                None => plain(&mut out, &word),
            }
            i = end;
            plain_from = i;
            continue;
        }
        // 其余（空白与标点）原样。关键字后跟标点就不再给下一个名字着色。
        if !c.is_whitespace() {
            last_kw = None;
        }
        i += 1;
    }
    flush!(n);
    out
}

fn string_is_key(chars: &[char], end: usize) -> bool {
    let mut i = end;
    while chars.get(i).is_some_and(|c| *c == ' ' || *c == '\t') {
        i += 1;
    }
    chars.get(i) == Some(&':')
}

const JSON_SPEC: Spec = Spec {
    line: NONE,
    block: None,
    quotes: &['"'],
    triple: false,
    keywords: NONE,
    literals: &["true", "false", "null"],
    fn_kw: NONE,
    type_kw: NONE,
    macros: false,
    directive: false,
    dollar: false,
    lifetime: false,
    fold: false,
};

const JSONC_SPEC: Spec = Spec {
    line: &["//"],
    block: Some(("/*", "*/")),
    ..JSON_SPEC
};

fn scan_json(code: &str, comments: bool) -> String {
    scan(
        if comments { &JSONC_SPEC } else { &JSON_SPEC },
        code,
        Some(()),
    )
}

/// 逐行「键 = 值」：YAML、TOML、INI。
fn scan_config(code: &str, toml: bool, ini: bool) -> String {
    let mut out = String::with_capacity(code.len() * 2);
    let mut lines = code.split('\n').peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let indent = &line[..line.len() - trimmed.len()];
        plain(&mut out, indent);
        if trimmed.starts_with('#') || (ini && trimmed.starts_with(';')) {
            put(&mut out, "c", trimmed);
        } else if (toml || ini) && trimmed.starts_with('[') {
            put(&mut out, "t", trimmed);
        } else if let Some((lead, key, rest)) = split_key(trimmed, toml || ini) {
            plain(&mut out, lead);
            put(&mut out, "a", key);
            out.push_str(&scan(&VALUE, rest, None));
        } else {
            out.push_str(&scan(&VALUE, trimmed, None));
        }
        if lines.peek().is_some() {
            out.push('\n');
        }
    }
    out
}

/// 拆出「行首缀 / 键 / 其后」。YAML 认 `key:`（冒号后是空白或行尾）与 `- key:`；
/// TOML 与 INI 认 `key =`。
fn split_key(line: &str, equals: bool) -> Option<(&str, &str, &str)> {
    let lead_len = if !equals && line.starts_with("- ") {
        2
    } else {
        0
    };
    let body = &line[lead_len..];
    let mut end = 0;
    let mut in_quote: Option<char> = None;
    for (i, c) in body.char_indices() {
        match in_quote {
            Some(q) => {
                if c == q {
                    in_quote = None;
                }
            }
            None if (c == '"' || c == '\'') && i == 0 => in_quote = Some(c),
            None if equals && c == '=' => {
                end = i;
                break;
            }
            None if !equals && c == ':' => {
                let after = &body[i + 1..];
                if after.is_empty() || after.starts_with([' ', '\t']) {
                    end = i;
                    break;
                }
            }
            None if c.is_whitespace() && !equals => return None,
            None => {}
        }
    }
    if end == 0 {
        return None;
    }
    let key = body[..end].trim_end();
    if key.is_empty() {
        return None;
    }
    Some((&line[..lead_len], key, &body[key.len()..]))
}

fn scan_html(code: &str) -> String {
    let chars: Vec<char> = code.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(code.len() * 2);
    let mut i = 0;
    while i < n {
        if starts(&chars, i, "<!--") {
            let mut end = i + 4;
            while end < n && !starts(&chars, end, "-->") {
                end += 1;
            }
            end = (end + 3).min(n);
            put(&mut out, "c", &text_of(&chars, i, end));
            i = end;
        } else if chars[i] == '<'
            && chars
                .get(i + 1)
                .is_some_and(|c| c.is_ascii_alphabetic() || *c == '/' || *c == '!' || *c == '?')
        {
            // 标签：`<name attr="v" ...>`
            let mut j = i + 1;
            if chars[j] == '/' || chars[j] == '!' || chars[j] == '?' {
                j += 1;
            }
            let name_start = j;
            while j < n && (chars[j].is_alphanumeric() || matches!(chars[j], '-' | ':' | '_' | '.'))
            {
                j += 1;
            }
            plain(&mut out, &text_of(&chars, i, name_start));
            put(&mut out, "k", &text_of(&chars, name_start, j));
            while j < n && chars[j] != '>' {
                let c = chars[j];
                if c == '"' || c == '\'' {
                    let end = string_end(&chars, j, c, false, true);
                    put(&mut out, "s", &text_of(&chars, j, end));
                    j = end;
                } else if ident_start(c) {
                    let mut end = j + 1;
                    while end < n
                        && (chars[end].is_alphanumeric()
                            || matches!(chars[end], '-' | ':' | '_' | '.' | '@'))
                    {
                        end += 1;
                    }
                    put(&mut out, "a", &text_of(&chars, j, end));
                    j = end;
                } else {
                    plain(&mut out, &c.to_string());
                    j += 1;
                }
            }
            let end = (j + 1).min(n);
            plain(&mut out, &text_of(&chars, j, end));
            i = end;
        } else {
            let start = i;
            while i < n && chars[i] != '<' {
                i += 1;
            }
            if i == start {
                i += 1;
            }
            plain(&mut out, &text_of(&chars, start, i));
        }
    }
    out
}

fn scan_css(code: &str) -> String {
    let chars: Vec<char> = code.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(code.len() * 2);
    let mut i = 0;
    let mut depth = 0i32;
    while i < n {
        let c = chars[i];
        if starts(&chars, i, "/*") {
            let mut end = i + 2;
            while end < n && !starts(&chars, end, "*/") {
                end += 1;
            }
            end = (end + 2).min(n);
            put(&mut out, "c", &text_of(&chars, i, end));
            i = end;
        } else if c == '"' || c == '\'' {
            let end = string_end(&chars, i, c, false, false);
            put(&mut out, "s", &text_of(&chars, i, end));
            i = end;
        } else if c == '{' {
            depth += 1;
            plain(&mut out, "{");
            i += 1;
        } else if c == '}' {
            depth = (depth - 1).max(0);
            plain(&mut out, "}");
            i += 1;
        } else if c == '@' {
            let mut end = i + 1;
            while end < n && (ident_part(chars[end]) || chars[end] == '-') {
                end += 1;
            }
            put(&mut out, "k", &text_of(&chars, i, end));
            i = end;
        } else if c == '#' && chars.get(i + 1).is_some_and(|d| d.is_ascii_hexdigit()) && depth > 0 {
            let mut end = i + 1;
            while end < n && chars[end].is_ascii_hexdigit() {
                end += 1;
            }
            put(&mut out, "n", &text_of(&chars, i, end));
            i = end;
        } else if c.is_ascii_digit()
            || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit()) && depth > 0)
        {
            let mut end = i;
            while end < n && (chars[end].is_ascii_digit() || chars[end] == '.') {
                end += 1;
            }
            while end < n && (chars[end].is_ascii_alphabetic() || chars[end] == '%') {
                end += 1;
            }
            put(&mut out, "n", &text_of(&chars, i, end));
            i = end;
        } else if ident_start(c) || c == '-' || c == '.' || c == '#' {
            // 标识符（含 `-`）：块外是选择器，块内冒号前是属性，后跟括号是函数。
            let mut end = i + 1;
            while end < n && (ident_part(chars[end]) || chars[end] == '-') {
                end += 1;
            }
            let word = text_of(&chars, i, end);
            let mut next = end;
            while next < n && chars[next] == ' ' {
                next += 1;
            }
            let after = chars.get(next).copied();
            if depth == 0 {
                put(&mut out, "t", &word);
            } else if after == Some(':') && word.len() > 1 {
                put(&mut out, "a", &word);
            } else if after == Some('(') {
                put(&mut out, "f", &word);
            } else {
                plain(&mut out, &word);
            }
            i = end;
        } else {
            plain(&mut out, &c.to_string());
            i += 1;
        }
    }
    out
}

/// diff：整行着色，行块自带底色，所以每行是一个块级 `span`，换行收在块内。
fn scan_diff(code: &str) -> String {
    let mut out = String::with_capacity(code.len() * 2);
    let mut lines = code.split('\n').peekable();
    while let Some(line) = lines.next() {
        let class = if line.starts_with("+++")
            || line.starts_with("---")
            || line.starts_with("diff ")
            || line.starts_with("index ")
        {
            "meta"
        } else if line.starts_with("@@") {
            "hunk"
        } else if line.starts_with('+') {
            "add"
        } else if line.starts_with('-') {
            "del"
        } else {
            "ctx"
        };
        out.push_str("<span class=\"dl dl-");
        out.push_str(class);
        out.push_str("\">");
        if line.is_empty() {
            // 空行也要占一行高：块里放一个零宽空格，换行才不会被吞。
            out.push('\u{200b}');
        } else {
            out.push_str(&esc(line));
        }
        if lines.peek().is_some() {
            out.push('\n');
        }
        out.push_str("</span>");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_strings_numbers_and_comments_get_their_own_class() {
        let html = highlight("rust", "let x = 42; // 注释\nprintln!(\"hi\");").unwrap();
        assert!(html.contains(r#"<span class="tk-k">let</span>"#), "{html}");
        assert!(html.contains(r#"<span class="tk-n">42</span>"#), "{html}");
        assert!(
            html.contains(r#"<span class="tk-c">// 注释</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="tk-f">println</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="tk-s">&quot;hi&quot;</span>"#),
            "{html}"
        );
    }

    #[test]
    fn unknown_languages_stay_plain() {
        assert!(highlight("brainfuck", "+++").is_none());
        assert!(highlight("", "x").is_none());
    }

    #[test]
    fn markup_in_code_is_always_escaped() {
        for lang in [
            "rust", "python", "html", "json", "yaml", "css", "diff", "bash",
        ] {
            let html = highlight(lang, "<script>alert(1)</script> & \"x\" <b>").unwrap();
            assert!(!html.contains("<script>"), "{lang}: {html}");
            assert!(!html.contains("<b>"), "{lang}: {html}");
        }
    }

    #[test]
    fn rust_lifetimes_are_not_strings() {
        let html = highlight("rust", "fn f<'a>(x: &'a str) -> char { 'x' }").unwrap();
        assert!(
            html.contains(r#"<span class="tk-t">&#39;a</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="tk-s">&#39;x&#39;</span>"#),
            "{html}"
        );
        assert!(html.contains(r#"<span class="tk-f">f</span>"#), "{html}");
    }

    #[test]
    fn python_docstrings_span_lines() {
        let html = highlight("py", "def f():\n    \"\"\"a\nb\"\"\"\n    return 1").unwrap();
        assert!(
            html.contains("tk-s\">&quot;&quot;&quot;a\nb&quot;&quot;&quot;"),
            "{html}"
        );
        assert!(html.contains(r#"<span class="tk-f">f</span>"#), "{html}");
    }

    #[test]
    fn hash_is_a_comment_only_at_a_word_start() {
        let html = highlight("bash", "echo $#  # 后面是注释\nx=a#b").unwrap();
        assert!(
            html.contains(r#"<span class="tk-c"># 后面是注释</span>"#),
            "{html}"
        );
        assert!(!html.contains(r#"tk-c">#b"#), "{html}");
    }

    #[test]
    fn json_keys_and_values_are_told_apart() {
        let html = highlight("json", r#"{"name": "acumen", "n": 3, "ok": true}"#).unwrap();
        assert!(
            html.contains(r#"<span class="tk-a">&quot;name&quot;</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="tk-s">&quot;acumen&quot;</span>"#),
            "{html}"
        );
        assert!(html.contains(r#"<span class="tk-n">true</span>"#), "{html}");
    }

    #[test]
    fn config_files_color_keys_and_sections() {
        let toml = highlight("toml", "[plugin]\n# c\nname = \"x\"\nn = 3").unwrap();
        assert!(
            toml.contains(r#"<span class="tk-t">[plugin]</span>"#),
            "{toml}"
        );
        assert!(toml.contains(r#"<span class="tk-a">name</span>"#), "{toml}");
        let yaml = highlight("yaml", "a:\n  - b: 1\n  - c").unwrap();
        assert!(yaml.contains(r#"<span class="tk-a">b</span>"#), "{yaml}");
        // URL 里的冒号不是键。
        let url = highlight("yaml", "see: http://x.y/z").unwrap();
        assert!(url.contains(r#"<span class="tk-a">see</span>"#), "{url}");
    }

    #[test]
    fn diff_lines_are_blocks_and_keep_their_newlines_inside() {
        let html = highlight("diff", "@@ -1 +1 @@\n-old\n+new\n same").unwrap();
        for class in ["hunk", "del", "add", "ctx"] {
            assert!(html.contains(&format!("dl-{class}")), "{class}: {html}");
        }
        assert!(!html.contains("</span>\n<span"), "换行要收在块内：{html}");
    }

    #[test]
    fn html_and_css_have_tag_attr_and_property_classes() {
        let html = highlight("html", r#"<a href="x">t</a><!-- c -->"#).unwrap();
        assert!(html.contains(r#"<span class="tk-k">a</span>"#), "{html}");
        assert!(html.contains(r#"<span class="tk-a">href</span>"#), "{html}");
        assert!(
            html.contains(r#"<span class="tk-c">&lt;!-- c --&gt;</span>"#),
            "{html}"
        );
        let css = highlight("css", ".a { color: #fff; margin: 0 4px }").unwrap();
        assert!(css.contains(r#"<span class="tk-a">color</span>"#), "{css}");
        assert!(css.contains(r#"<span class="tk-n">#fff</span>"#), "{css}");
        assert!(css.contains(r#"<span class="tk-n">4px</span>"#), "{css}");
    }

    #[test]
    fn language_labels_are_normalised() {
        assert_eq!(display_name("js"), "JavaScript");
        assert_eq!(display_name("rs {.numberLines}"), "Rust");
        assert_eq!(display_name("weird"), "weird");
        assert_eq!(display_name(""), "");
    }

    #[test]
    fn a_stray_quote_cannot_swallow_the_rest_of_the_block() {
        let html = highlight("js", "let s = 'oops\nlet t = 1").unwrap();
        assert!(html.contains(r#"<span class="tk-n">1</span>"#), "{html}");
    }
}
