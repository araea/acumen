//! 提示模板：把带 `{占位符}` 的一段话切成消息段。入群欢迎与自动回复共用这一套写法：
//!
//! - `{at}` 真的 @（@ 谁由调用方定）、`{avatar}` 头像、`{face:14}` QQ 表情；
//! - `{image}` 配图的位置（配图是哪张由调用方定），`{image:文件名或链接}` 就地插一张指定的图；
//! - `{甲|乙|丙}` 每次随机换成其中一个；
//! - 其余 `{键}` 的值由调用方给（[`Values`]）；认不出的原样留着——宁可让人看到自己写错了，
//!   也不悄悄吞掉。
//!
//! 「内容本身也是模板」的占位符（入群欢迎的自定义占位符）经 [`Values::snippet`] 给出，就地
//! 展开一层：里面可以用别的内置占位符与 `{甲|乙}`，但不再套这一类，免得两个互相引用转圈。

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Piece {
    Text(String),
    At,
    /// `{image}` 是 `None`；`{image:来源}` 带着来源。
    Image(Option<String>),
    Avatar,
    Face(String),
}

/// 占位符的值从哪来。
pub trait Values {
    fn value(&self, key: &str) -> Option<String>;

    /// 内容本身也是模板的占位符；只展开一层。
    fn snippet(&self, _key: &str) -> Option<String> {
        None
    }
}

/// 一张键值表就是最简单的 [`Values`]。
impl<S: std::hash::BuildHasher> Values for std::collections::HashMap<&str, String, S> {
    fn value(&self, key: &str) -> Option<String> {
        self.get(key).cloned()
    }
}

pub fn pieces(template: &str, values: &impl Values) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut text = String::new();
    split(template, values, true, &mut text, &mut out);
    if !text.is_empty() {
        out.push(Piece::Text(text));
    }
    out
}

/// 切一段模板，文字攒进 `text`、消息段进 `out`。`snippets`：这一层认不认 [`Values::snippet`]。
fn split(
    template: &str,
    values: &impl Values,
    snippets: bool,
    text: &mut String,
    out: &mut Vec<Piece>,
) {
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        text.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after
            .find(['}', '{'])
            .filter(|&at| after.as_bytes()[at] == b'}')
        else {
            text.push('{');
            rest = after;
            continue;
        };
        let inner = after[..close].trim();
        let piece = match inner {
            "at" => Some(Piece::At),
            "image" => Some(Piece::Image(None)),
            "avatar" => Some(Piece::Avatar),
            _ => match inner.split_once(':') {
                Some(("face", id)) if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) => {
                    Some(Piece::Face(id.to_string()))
                }
                Some(("image", src)) if !src.trim().is_empty() => {
                    Some(Piece::Image(Some(src.trim().to_string())))
                }
                _ => None,
            },
        };
        match piece {
            Some(piece) => {
                if !text.is_empty() {
                    out.push(Piece::Text(std::mem::take(text)));
                }
                out.push(piece);
            }
            None if inner.contains('|') => {
                let options: Vec<&str> = inner.split('|').collect();
                text.push_str(options[rand::random_range(0..options.len())]);
            }
            None => match values.value(inner) {
                Some(value) => text.push_str(&value),
                None => match values.snippet(inner).filter(|_| snippets) {
                    Some(snippet) => split(&snippet, values, false, text, out),
                    None => text.push_str(&rest[open..open + close + 2]),
                },
            },
        }
        rest = &after[close + 1..];
    }
    text.push_str(rest);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn values() -> HashMap<&'static str, String> {
        HashMap::from([
            ("name", "小林".to_string()),
            ("group", "测试群".to_string()),
        ])
    }

    #[test]
    fn splits_segments_and_keeps_unknown_braces() {
        let parts = pieces(
            "欢迎 {at} 来到{group}{face:14}{image}{image: a.png }{nope}{",
            &values(),
        );
        assert_eq!(
            parts,
            vec![
                Piece::Text("欢迎 ".into()),
                Piece::At,
                Piece::Text(" 来到测试群".into()),
                Piece::Face("14".into()),
                Piece::Image(None),
                Piece::Image(Some("a.png".into())),
                Piece::Text("{nope}{".into()),
            ]
        );
    }

    #[test]
    fn random_fragment_picks_one_option() {
        for _ in 0..20 {
            let parts = pieces("{你好|嗨}，{name}", &values());
            let [Piece::Text(text)] = &parts[..] else {
                panic!("{parts:?}")
            };
            assert!(text == "你好，小林" || text == "嗨，小林", "{text}");
        }
    }
}
