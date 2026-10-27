//! `edit` 工具的匹配与替换：一次调用改一份文件里的几处，原文对不上时宽容一点。
//!
//! 从前的 `edit` 一次只换一处、只认逐字一致。实际跑下来最常见的两种失败都与模型的
//! 本事无关：
//!
//! 1. **改三处就是三次调用**——每次都要模型把整个文件状态在脑子里推一遍，步数也白白烧掉；
//! 2. **原文差一点点对不上**——行尾多了空格、全角空格、弯引号、长短破折号。模型照着
//!    `read` 的结果抄，抄回来的却是它「以为」的字符，于是报「没有找到」、再 read、再试。
//!
//! 所以照 pi 的做法：`edits` 是一组互不重叠的替换，**全部对着原文件匹配**（不是改一处
//! 再找下一处），一次写回；逐字找不到时再按「宽容的写法」找一遍——去掉行尾空白、
//! 把弯引号、各种破折号、特殊空格归一。宽容匹配只用来**定位**：命中区间之外的原文
//! 一个字节都不动，替换进去的是模型给的新文本。
//!
//! 换行与 BOM 照原样保留：文件是 CRLF 的，模型写的 LF 换行会转回 CRLF。

use serde_json::Value;

/// 一处替换。
pub(crate) struct Replace {
    pub old: String,
    pub new: String,
    /// `old` 出现多处时全部替换（只有旧写法的单处替换才有这个开关）。
    pub all: bool,
}

/// 从参数里取出这一次要做的替换：新写法 `edits: [{old, new}]`，旧写法 `old` / `new` / `replace_all`。
///
/// 模型偶尔把 `edits` 写成一段 JSON 字符串、或者写成单个对象而不是数组，键名也有人写
/// `oldText` / `newText`——都照收，不为格式这种小事驳回一轮。
pub(crate) fn parse(args: &Value) -> anyhow::Result<Vec<Replace>> {
    let one = |item: &Value, all: bool| -> Option<Replace> {
        let pick = |keys: &[&str]| keys.iter().find_map(|key| item.get(*key).and_then(Value::as_str));
        Some(Replace {
            old: pick(&["old", "oldText", "old_text"])?.to_string(),
            new: pick(&["new", "newText", "new_text"]).unwrap_or("").to_string(),
            all,
        })
    };
    let edits = match args.get("edits") {
        Some(Value::String(text)) => serde_json::from_str::<Value>(text).ok(),
        Some(other) => Some(other.clone()),
        None => None,
    };
    let mut out = Vec::new();
    match edits {
        Some(Value::Array(items)) => {
            for (index, item) in items.iter().enumerate() {
                out.push(one(item, false).ok_or_else(|| {
                    anyhow::anyhow!("参数错误：edits[{index}] 要有 old 与 new 两个字符串字段")
                })?);
            }
        }
        Some(item @ Value::Object(_)) => out.extend(one(&item, false)),
        _ => {}
    }
    if out.is_empty()
        && let Some(single) = one(args, args["replace_all"].as_bool().unwrap_or(false))
    {
        out.push(single);
    }
    if out.is_empty() {
        anyhow::bail!("参数错误：要给 edits（[{{old, new}}, …]）；要整份覆盖请用 write");
    }
    if let Some(index) = out.iter().position(|edit| edit.old.is_empty()) {
        anyhow::bail!("参数错误：edits[{index}].old 不能为空；要整份覆盖请用 write");
    }
    Ok(out)
}

/// 宽容匹配里认作同一个的字符。
fn fold(c: char) -> char {
    match c {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
        '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
        '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
        '\t' => '\t',
        other => other,
    }
}

/// 宽容写法：逐字折叠，并去掉每行行尾的空白。返回折叠后的字符，以及每个字符在原文里的字节区间。
fn normalize(text: &str) -> (Vec<char>, Vec<(usize, usize)>) {
    let mut chars = Vec::with_capacity(text.len());
    let mut spans = Vec::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let base = line.as_ptr() as usize - text.as_ptr() as usize;
        let (body, newline) = match line.strip_suffix('\n') {
            Some(body) => (body, true),
            None => (line, false),
        };
        let kept = body.trim_end_matches(|c: char| c.is_whitespace() || fold(c) == ' ');
        for (offset, c) in kept.char_indices() {
            chars.push(fold(c));
            spans.push((base + offset, base + offset + c.len_utf8()));
        }
        if newline {
            chars.push('\n');
            spans.push((base + body.len(), base + body.len() + 1));
        }
    }
    (chars, spans)
}

/// 在 `haystack` 里找 `needle` 的全部起点（不重叠）。
fn find_all<T: PartialEq>(haystack: &[T], needle: &[T]) -> Vec<usize> {
    let mut hits = Vec::new();
    if needle.is_empty() || needle.len() > haystack.len() {
        return hits;
    }
    let mut start = 0;
    while start + needle.len() <= haystack.len() {
        if haystack[start..start + needle.len()] == *needle {
            hits.push(start);
            start += needle.len();
        } else {
            start += 1;
        }
    }
    hits
}

/// 一处命中：原文里的字节区间与换上去的新文本。
struct Hit {
    start: usize,
    end: usize,
    new: String,
    edit: usize,
}

/// 改完的结果：新内容、实际替换了几处、有几处是靠宽容匹配找到的。
pub(crate) struct Outcome {
    pub content: String,
    pub replaced: usize,
    pub fuzzy: usize,
    /// 改动落在新文件的哪些行（从 1 起，含两端），给回执列出改后的样子。
    pub touched: Vec<(usize, usize)>,
}

/// 对着原文（已统一成 LF）把这一组替换一次做完。
///
/// 每一处都先逐字找，找不到再宽容地找；命中多处而又没说 `all` 时报错让模型写具体些。
/// 各处之间不许重叠：重叠的两处说明模型该把它们并成一处。
pub(crate) fn apply(content: &str, edits: &[Replace]) -> anyhow::Result<Outcome> {
    let label = |index: usize| {
        if edits.len() == 1 { "要替换的原文".to_string() } else { format!("edits[{index}] 的原文") }
    };
    let mut hits: Vec<Hit> = Vec::new();
    let mut fuzzy = 0;
    let mut folded: Option<(Vec<char>, Vec<(usize, usize)>)> = None;
    for (index, edit) in edits.iter().enumerate() {
        let old = edit.old.replace("\r\n", "\n");
        let new = edit.new.replace("\r\n", "\n");
        let exact: Vec<usize> = content.match_indices(old.as_str()).map(|(at, _)| at).collect();
        let spans: Vec<(usize, usize)> = if !exact.is_empty() {
            exact.iter().map(|&at| (at, at + old.len())).collect()
        } else {
            let (chars, map) = folded.get_or_insert_with(|| normalize(content));
            let (needle, _) = normalize(&old);
            // 原文以换行结尾时宽容写法也要保住这个换行，`normalize` 已逐行处理过。
            let found: Vec<(usize, usize)> = find_all(chars, &needle)
                .into_iter()
                .map(|at| (map[at].0, map[at + needle.len() - 1].1))
                .collect();
            if !found.is_empty() {
                fuzzy += 1;
            }
            found
        };
        match spans.len() {
            0 => anyhow::bail!(
                "{}没有在文件里找到（连行尾空白、引号、破折号这些都宽容地比过了）。先 read 看一眼那一段现在的样子，照原样抄；\
                 文件可能已经被改过",
                label(index)
            ),
            1 => {}
            _ if edit.all => {}
            count => anyhow::bail!(
                "{}在文件里出现了 {count} 次，连同上下文多写一两行让它唯一；确实要每处都换就用旧写法加 replace_all=true",
                label(index)
            ),
        }
        hits.extend(spans.into_iter().map(|(start, end)| Hit { start, end, new: new.clone(), edit: index }));
    }
    hits.sort_by_key(|hit| hit.start);
    if let Some(pair) = hits.windows(2).find(|pair| pair[1].start < pair[0].end) {
        anyhow::bail!(
            "edits[{}] 与 edits[{}] 改的是同一段原文（互相重叠），把它们并成一处再改",
            pair[0].edit.min(pair[1].edit),
            pair[0].edit.max(pair[1].edit)
        );
    }
    let mut out = String::with_capacity(content.len());
    let mut touched = Vec::with_capacity(hits.len());
    let mut cursor = 0;
    for hit in &hits {
        out.push_str(&content[cursor..hit.start]);
        let first = out.matches('\n').count() + 1;
        out.push_str(&hit.new);
        let last = first + hit.new.trim_end_matches('\n').matches('\n').count();
        touched.push((first, last));
        cursor = hit.end;
    }
    out.push_str(&content[cursor..]);
    if out == content {
        anyhow::bail!("新内容与原文一模一样，文件没有变化；确认一下要改成什么");
    }
    Ok(Outcome {
        content: out,
        replaced: hits.len(),
        fuzzy,
        touched,
    })
}

/// 回执里列出改后的样子：每处前后各带一行上下文，行号与 `read` 同一种写法。
///
/// 模型拿它当场核对（缩进对不对、有没有多删一行），不必再 read 一遍整份文件。
pub(crate) fn preview(content: &str, touched: &[(usize, usize)]) -> String {
    /// 每处最多列这么多行；再长的改动看头尾就够了。
    const MAX_LINES: usize = 16;
    /// 整份回执最多列几处。
    const MAX_SPANS: usize = 6;
    let lines: Vec<&str> = content.lines().collect();
    let mut out = String::new();
    for &(first, last) in touched.iter().take(MAX_SPANS) {
        let from = first.saturating_sub(1).max(1);
        let to = (last + 1).min(lines.len());
        if from > to {
            continue;
        }
        if !out.is_empty() {
            out.push_str("  ⋯\n");
        }
        let numbers: Vec<usize> = (from..=to).collect();
        let shown: Vec<usize> = if numbers.len() > MAX_LINES {
            let half = MAX_LINES / 2;
            numbers[..half].iter().chain(&numbers[numbers.len() - half..]).copied().collect()
        } else {
            numbers
        };
        let mut previous = None;
        for number in shown {
            if previous.is_some_and(|prev: usize| number > prev + 1) {
                out.push_str(&format!("  …（省略 {} 行）\n", number - previous.unwrap_or(0) - 1));
            }
            out.push_str(&format!("{number:>6}\t{}\n", lines.get(number - 1).copied().unwrap_or("")));
            previous = Some(number);
        }
    }
    if touched.len() > MAX_SPANS {
        out.push_str(&format!("  …另有 {} 处改动未列出\n", touched.len() - MAX_SPANS));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(old: &str, new: &str) -> Replace {
        Replace { old: old.into(), new: new.into(), all: false }
    }

    #[test]
    fn several_edits_match_the_original() {
        let done = apply("a = 1\nb = 2\nc = 3\n", &[edit("a = 1", "a = 10"), edit("c = 3", "c = 30")]).unwrap();
        assert_eq!(done.content, "a = 10\nb = 2\nc = 30\n");
        assert_eq!(done.touched, vec![(1, 1), (3, 3)]);
        assert_eq!(done.fuzzy, 0);
    }

    #[test]
    fn fuzzy_match_touches_only_the_hit() {
        // 原文行尾有空格、用弯引号；模型抄成了直引号、没有行尾空格。
        let original = "keep  \nsay(\u{201C}hi\u{201D})   \nnext\n";
        let done = apply(original, &[edit("say(\"hi\")\n", "say(\"bye\")\n")]).unwrap();
        assert_eq!(done.content, "keep  \nsay(\"bye\")\nnext\n");
        assert_eq!(done.fuzzy, 1);
    }

    #[test]
    fn ambiguous_and_overlapping_edits_are_refused() {
        assert!(apply("x\nx\n", &[edit("x", "y")]).is_err());
        assert!(apply("abcdef", &[edit("abc", "1"), edit("cde", "2")]).is_err());
        let all = Replace { old: "x".into(), new: "y".into(), all: true };
        assert_eq!(apply("x\nx\n", &[all]).unwrap().content, "y\ny\n");
    }

    #[test]
    fn parse_accepts_loose_shapes() {
        let args = serde_json::json!({"edits": "[{\"oldText\":\"a\",\"newText\":\"b\"}]"});
        assert_eq!(parse(&args).unwrap()[0].new, "b");
        let args = serde_json::json!({"old": "a", "new": "b", "replace_all": true});
        assert!(parse(&args).unwrap()[0].all);
        assert!(parse(&serde_json::json!({"edits": []})).is_err());
    }
}
