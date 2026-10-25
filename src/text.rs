//! 进程内共用的文本处理：中文分词，以及比对之前把一句话收拾成统一的样子。
//!
//! Jieba 首次使用会载入词典，构造一次就够了。分词结果（空格分隔的词串）写进
//! `message_records.tokens`，词云、统计与 AI 资讯的关键词都从那里读。
//!
//! 放在框架层而不是某个插件里：AI 资讯也在切词，自动回复与违禁词也在用同一套
//! 归一化，插件之间不必为了一个只读函数互相 import。

use jieba_rs::Jieba;
use std::sync::OnceLock;

/// 全局分词器。
pub fn jieba() -> &'static Jieba {
    static JIEBA: OnceLock<Jieba> = OnceLock::new();
    JIEBA.get_or_init(Jieba::new)
}

/// 切成以空格分隔的词串；空白文本给空串。
pub fn words(text: &str) -> String {
    jieba()
        .cut(text, false)
        .into_iter()
        .map(|token| token.word)
        .collect::<Vec<_>>()
        .join(" ")
}

/// 比对用的归一化：全角转半角、不分大小写、去掉空白、标点与零宽字符。
///
/// 群里绕着词说话的办法就那么几种——「加 微 信」「加，微，信」「Ｖ信」、夹几个看不见的
/// 零宽空格。把两边都收拾成同一个样子再比，这些写法才对得上；代价是英文单词之间也不再有
/// 空格（`free money` 与 `freemoney` 同形），对中文群聊是划算的。
pub fn fold(text: &str) -> String {
    text.chars()
        .map(width)
        .filter(|c| !c.is_whitespace() && !punctuation(*c) && !invisible(*c))
        .flat_map(char::to_lowercase)
        .collect()
}

/// 正则比对用的归一化：全角转半角、不分大小写、去掉零宽字符，但保留空白与标点——
/// 正则里的 `\s*`、`[，,]` 还得对得上。想连空白一起忽略就写折叠后的文本。
pub fn plain(text: &str) -> String {
    text.chars()
        .map(width)
        .filter(|c| !invisible(*c))
        .flat_map(char::to_lowercase)
        .collect()
}

/// 全角 ASCII 转半角；其余原样。
fn width(c: char) -> char {
    match c {
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        _ => c,
    }
}

/// 句子里的标点（中英文都算）。折叠时丢掉，只留下「说了什么」。
fn punctuation(c: char) -> bool {
    const MARKS: &str = "，。、；：？！“”‘’（）【】《》〈〉「」『』…—～·〜・｡､";
    c.is_ascii_punctuation() || MARKS.contains(c)
}

/// 看不见、也不该参与比对的字符：零宽空格、零宽连接符、软连字符之类。
fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}' | '\u{2060}' | '\u{FEFF}' | '\u{00AD}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_ignores_spaces_punctuation_width_and_invisible_marks() {
        assert_eq!(fold("怎么 下载？？"), "怎么下载");
        assert_eq!(fold("ＱＱ群！"), "qq群");
        assert_eq!(fold("  Hello, World. "), "helloworld");
        assert_eq!(fold("加\u{200B}微\u{200B}信"), "加微信");
        assert_eq!(fold("加，微，信"), "加微信");
    }
}
