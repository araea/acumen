//! 进程内共用的中文分词。
//!
//! Jieba 首次使用会载入词典，构造一次就够了。分词结果（空格分隔的词串）写进
//! `message_records.tokens`，词云、统计与 AI 资讯的关键词都从那里读。
//!
//! 放在框架层而不是 recorder 插件里：AI 资讯也在切词，插件之间不必为了一个只读
//! 词典互相 import。

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
