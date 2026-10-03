//! 翻旧账守卫：眼前没人提起，却说起「上次」「之前说过」的话。
//!
//! 记忆块（自己说过的话、旧事、几摊事）是给人格前后一致用的，可小模型读到「你以前
//! 说过……」就忍不住拿它起话头：群里在聊众筹，它来一句「上次说不值一千万是我嘴硬了」，
//! 群友回「笨笨的」「自说自话」（线上 2026-10-03）。源头那一头已经按话题筛了记忆，
//! 这里是最后一道：话里带着回头看的字眼，而它说的内容在眼前几条里一个实词都碰不上，
//! 就当它是从记忆里搬来的，不发。
//!
//! 只管这一种最容易看穿的：**明说**了「上次 / 之前说过 / 我记得」的。不带这类字眼的
//! 旧话重提拦不住，也不去猜——误拦一句正常的话，比放过一句怪话代价更大。

use crate::plugins::oai::chat::tone::{content_words, visible};
use crate::plugins::oai::chat::window::Turn;
use regex::Regex;
use std::sync::OnceLock;

/// 回头看的字眼。
fn cue() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"上次|上回|上一回|之前(?:我|你|他|还|有)?(?:说|讲|提|聊|问)|以前(?:我|你)?(?:说|讲|提|聊)|前几天|前阵子|昨天(?:说|聊|讲)|那天(?:说|聊|讲)|还记得|你不是(?:说|讲)过|我(?:说|讲|提)过|我记得|记得你|早先",
        )
        .unwrap()
    })
}

/// 看「眼前」的这么多条。
const LOOKBACK: usize = 10;

/// 这句话是不是一次没有来头的翻旧账。
pub(super) fn is_ungrounded_callback(text: &str, turns: &[Turn]) -> bool {
    if !cue().is_match(text) {
        return false;
    }
    // 眼前有人自己提起过去（「你上次不是说……」），顺着回就是接话。
    let others: String = turns
        .iter()
        .rev()
        .filter(|turn| !turn.from_me)
        .take(LOOKBACK)
        .map(|turn| visible(&turn.text))
        .collect::<Vec<_>>()
        .join("\n");
    if cue().is_match(&others) {
        return false;
    }
    let words = content_words(&cue().replace_all(&visible(text), " "));
    if words.is_empty() {
        return false;
    }
    // 回头看的内容在眼前碰得上（自己刚说过的也算）：那是在接眼前的话。
    let seen: String = turns
        .iter()
        .rev()
        .take(LOOKBACK)
        .map(|turn| visible(&turn.text))
        .collect::<Vec<_>>()
        .join("\n");
    words.is_disjoint(&content_words(&seen))
}
