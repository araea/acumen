//! 事件折叠：把「同一件事的多家报道」并成一条。
//!
//! ## 为什么要折
//!
//! AIHOT 官网首页是按**事件**排的：同一场发布会，官方号、YouTube、Hacker News、
//! 好几个 X 账号各写了一条，页面上只留一张卡片，底下写「另有 3 家信源报道」。
//! 公开接口 `/api/v1/items` 没有这层归并（页面数据里的 `factId` 不在契约内），
//! 每篇报道各是一条、各有各的 id。插件如果只按 id 去重，一件事就会被推好几遍——
//! 2026-09-28 夜里 Claude Sonnet 5.5 发布，群里 5 分钟内收到三次；Agent Arena 的
//! 同一则战报隔半小时来一遍，英伟达的智能体安全平台前后推了五条。
//!
//! 接口不给归并结果，所以在本地判。
//!
//! ## 怎么判
//!
//! 同一件事的几篇报道，标题是同一个句式换着说：「Anthropic 发布 Claude Sonnet 5.5，
//! 速度比 Sonnet 5 快超 30%」和「Anthropic 发布 Claude Sonnet 5.5：比 Sonnet 5 快超
//! 30% 且成本最多降 30%」。所以只看词元重合：
//!
//!   - **词元**：英文与数字按词（`claude` / `5.5` / `gpt-6` / `30%`），中文按相邻两字，
//!     先把「发布 / 推出 / 宣布 / 的」这类每条新闻都有的虚词剪掉，只留带事件身份的部分；
//!   - **相似度**：标题一份、标题加摘要一份，各算一个 Ochiai 系数
//!     （`|A∩B| / √(|A|·|B|)`），取平均。只看标题会被短标题的巧合重合骗，
//!     只看摘要又被各家写法的细节冲淡，两个一起才稳；
//!   - **不用 IDF**：一件大事的词会在短时间内反复出现，用语料词频加权反而把它们
//!     自己压低，发布会越热越认不出来。不加权就是无状态的，同样两条永远得同样的分。
//!
//! 判到同一事件的条目连成一片（单链，见 [`fold`]）。
//!
//! 阈值 [`SAME_EVENT`] 取 0.45，是拿 2026-09 下旬的 400 多条真实条目标注出来的：
//! 这个点上同一事件的重合率约八成，误并约一成，误并的多是「某模型发布」与
//! 「某模型上线 OpenRouter」这种同一主角的相邻消息。**宁可漏折也不错折**——
//! 漏折只是多看一条，错折是少了一条消息。折进去的报道不会消失：卡片上写着
//! 「另有 N 家信源报道」，引用提取还能取到它们的链接。
//!
//! 夹具在 `fixtures/aihot-2026-09-28.json`，是那天的真实标题与摘要。

use super::api::Item;
use std::collections::HashSet;

/// 判为同一事件的相似度下限
pub const SAME_EVENT: f64 = 0.45;

/// 同一事件的报道最多相隔多久。发布会的报道扎堆在几小时里，隔了一天半还在说的
/// 通常已是后续进展，官网也会另起一张卡片。
pub const WINDOW_SECONDS: i64 = 36 * 3600;

/// 两条至少要有这么多共同词元才可能是同一事件：挡住「Manus 2.0 发布」这类
/// 只剩两三个词的短标题碰巧重合
const MIN_SHARED: usize = 4;

/// 一个事件卡片最多带几条陪衬报道
const MAX_ALSO: usize = 12;

const STOP_ASCII: &[&str] = &[
    "ai", "the", "an", "of", "and", "in", "on", "to", "for", "with", "by", "is", "at", "as", "vs",
    "via",
];

/// 每条新闻都有、不带事件身份的中文词。匹配时取最长的，所以先写长的也无妨
const STOP_CJK: &[&str] = &[
    "发布", "推出", "宣布", "表示", "披露", "上线", "公布", "开放", "正式", "最新", "新增", "支持",
    "进行", "以及", "消息", "报道", "评论", "分享", "介绍", "解析", "详解", "称其", "显示", "据称",
    "再度", "今日", "首次", "全新", "升级", "更新", "实现", "提供", "包括", "通过", "用于", "使用",
    "基于", "相关", "多个", "一个", "两个", "三个", "称", "并", "和", "与", "的", "了", "在", "为",
    "将", "对", "被", "其", "该", "等", "已", "可", "由", "从", "向", "把", "让", "据",
];

fn is_han(ch: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&ch)
}

/// 把一段文字拆成词元集合
fn tokens(text: &str) -> HashSet<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = HashSet::new();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch.is_ascii_alphabetic() {
            // 英文词：字母开头，可含数字与 `-_.+`（`gpt-6` / `mimo-v2.6` / `c++`）
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '-' | '_' | '.' | '+'))
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect::<String>().to_ascii_lowercase();
            let word = word.trim_matches(['.', '-', '_', '+']);
            if word.len() > 1 && !STOP_ASCII.contains(&word) {
                out.insert(word.to_string());
            }
        } else if ch.is_ascii_digit() {
            // 数字：`5.5` / `30%`
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            if i + 1 < chars.len() && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
                i += 1;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if i < chars.len() && chars[i] == '%' {
                i += 1;
            }
            out.insert(chars[start..i].iter().collect());
        } else if is_han(ch) {
            let start = i;
            while i < chars.len() && is_han(chars[i]) {
                i += 1;
            }
            bigrams(&chars[start..i], &mut out);
        } else {
            i += 1;
        }
    }
    out
}

/// 一段连续汉字：剪掉虚词后，在剩下的每一截里取相邻两字
fn bigrams(run: &[char], out: &mut HashSet<String>) {
    let mut segment: Vec<char> = Vec::new();
    let flush = |segment: &mut Vec<char>, out: &mut HashSet<String>| {
        for pair in segment.windows(2) {
            out.insert(pair.iter().collect());
        }
        segment.clear();
    };

    let mut i = 0;
    while i < run.len() {
        // 先试两字虚词，再试一字虚词
        let stop = [2usize, 1].into_iter().find(|&len| {
            run.get(i..i + len)
                .is_some_and(|w| STOP_CJK.contains(&w.iter().collect::<String>().as_str()))
        });
        match stop {
            Some(len) => {
                flush(&mut segment, out);
                i += len;
            }
            None => {
                segment.push(run[i]);
                i += 1;
            }
        }
    }
    flush(&mut segment, out);
}

/// 一条资讯的词元指纹：标题一份、标题加摘要一份
#[derive(Debug, Clone, Default)]
pub struct Fingerprint {
    head: HashSet<String>,
    body: HashSet<String>,
}

impl Fingerprint {
    pub fn new(title: &str, summary: &str) -> Self {
        let head = tokens(title);
        let mut body = tokens(summary);
        body.extend(head.iter().cloned());
        Self { head, body }
    }

    pub fn of(item: &Item) -> Self {
        Self::new(
            item.title.as_deref().unwrap_or_default(),
            item.summary.as_deref().unwrap_or_default(),
        )
    }

    /// 0—1，越大越像同一件事
    pub fn similarity(&self, other: &Self) -> f64 {
        let (head, _) = ochiai(&self.head, &other.head);
        let (body, shared) = ochiai(&self.body, &other.body);
        if shared < MIN_SHARED {
            return 0.0;
        }
        (head + body) / 2.0
    }

    pub fn same_event(&self, other: &Self) -> bool {
        self.similarity(other) >= SAME_EVENT
    }
}

/// Ochiai 系数与共有词元数
fn ochiai(a: &HashSet<String>, b: &HashSet<String>) -> (f64, usize) {
    if a.is_empty() || b.is_empty() {
        return (0.0, 0);
    }
    let shared = a.intersection(b).count();
    (shared as f64 / ((a.len() * b.len()) as f64).sqrt(), shared)
}

/// 两个时间点是否近到可能是同一事件；缺时间时不拦
pub fn within_window(a: Option<i64>, b: Option<i64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() <= WINDOW_SECONDS,
        _ => true,
    }
}

/// 一个事件：代表报道 + 同一件事的其它报道
#[derive(Debug, Clone)]
pub struct Cluster {
    /// 代表报道：最早报出这件事的那条（官网也是这么选的）
    pub lead: Item,
    /// 其它报道，按报出的先后排
    pub also: Vec<Item>,
}

impl Cluster {
    pub fn single(item: Item) -> Self {
        Self {
            lead: item,
            also: Vec::new(),
        }
    }

    pub fn members(&self) -> impl Iterator<Item = &Item> {
        std::iter::once(&self.lead).chain(self.also.iter())
    }

    /// 全部成员的去重键，代表报道在前
    pub fn keys(&self) -> Vec<String> {
        self.members().filter_map(Item::dedupe_key).collect()
    }

    /// 其它报道来自哪几家信源：去掉技术后缀、去重、不含代表报道自己的信源。
    /// 同一家信源连发两条不算「另有一家」。
    pub fn other_reports(&self) -> Vec<(String, &Item)> {
        let mut seen: Vec<String> = self.lead.source_label().into_iter().collect();
        let mut out = Vec::new();
        for item in &self.also {
            let Some(name) = item.source_label() else {
                continue;
            };
            if seen.contains(&name) {
                continue;
            }
            seen.push(name.clone());
            out.push((name, item));
        }
        out
    }

    /// 「另有 N 家信源报道」里的 N 与信源名
    pub fn other_sources(&self) -> Vec<String> {
        self.other_reports().into_iter().map(|(name, _)| name).collect()
    }

    /// 并入一条同事件的报道；代表报道始终是最早的那条
    pub fn absorb(&mut self, item: Item) {
        if first_reported(&item) < first_reported(&self.lead) {
            let earlier = std::mem::replace(&mut self.lead, item);
            self.also.push(earlier);
        } else {
            self.also.push(item);
        }
        self.also.sort_by_key(first_reported);
        self.also.truncate(MAX_ALSO);
    }
}

/// 报出的先后；没有时间的排最后
fn first_reported(item: &Item) -> (i64, i64) {
    // 分数高的排前面，所以取负
    (
        item.first_reported_ts().unwrap_or(i64::MAX),
        -(item.score.unwrap_or(0.0) as i64),
    )
}

/// 不折叠：每条各是一个事件
pub fn singles(items: Vec<Item>) -> Vec<Cluster> {
    items.into_iter().map(Cluster::single).collect()
}

/// `enabled` 为假时不折叠（配置 `fold_same_event`）
pub fn fold_if(enabled: bool, items: Vec<Item>) -> Vec<Cluster> {
    if enabled { fold(items) } else { singles(items) }
}

/// 把一批条目折成事件。
///
/// 输入顺序就是展示顺序：一个事件排在它**第一个出现的成员**的位置上，
/// 官网也是这样（卡片跟着最新一家信源冒上来）。
///
/// 任何一对像就连起来（单链），事件是连通的一片，与输入顺序无关：同一件事各家
/// 写法差得远时，中间那几条能把两头接起来——Sonnet 5.5 发布那晚，Hacker News 的
/// 标题与 YouTube 的只有三成像，但两边都与 Anthropic 官方号那条对得上。
/// 代价是链条可能越接越远，所以两条之间还得在 [`WINDOW_SECONDS`] 之内。
pub fn fold(items: Vec<Item>) -> Vec<Cluster> {
    let n = items.len();
    let prints: Vec<Fingerprint> = items.iter().map(Fingerprint::of).collect();
    let stamps: Vec<Option<i64>> = items.iter().map(Item::timeline_ts).collect();

    let mut parent: Vec<usize> = (0..n).collect();
    fn root(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for i in 0..n {
        for j in 0..i {
            if within_window(stamps[i], stamps[j]) && prints[i].same_event(&prints[j]) {
                let (a, b) = (root(&mut parent, i), root(&mut parent, j));
                parent[a.max(b)] = a.min(b);
            }
        }
    }

    // 按第一个成员出现的先后给事件排位
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut slot_of: Vec<Option<usize>> = vec![None; n];
    for i in 0..n {
        let r = root(&mut parent, i);
        let slot = *slot_of[r].get_or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[slot].push(i);
    }

    let mut slots: Vec<Option<Item>> = items.into_iter().map(Some).collect();
    groups
        .into_iter()
        .filter_map(|members| {
            let mut members = members.into_iter().filter_map(|i| slots[i].take());
            let mut cluster = Cluster::single(members.next()?);
            for item in members {
                cluster.absorb(item);
            }
            Some(cluster)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::ai_news::api::Source;
    use serde::Deserialize;
    use std::collections::HashMap;

    #[derive(Deserialize)]
    struct Fixture {
        items: Vec<FixtureItem>,
        groups: HashMap<String, Vec<String>>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct FixtureItem {
        id: String,
        title: String,
        summary: Option<String>,
        published_at: Option<String>,
        discovered_at: Option<String>,
        category: Option<String>,
        score: Option<f64>,
        source: String,
    }

    /// 2026-09-28 夜里从 AIHOT 取的真实条目，按事件分好了组
    fn fixture() -> (Vec<Item>, HashMap<String, Vec<String>>) {
        let raw: Fixture =
            serde_json::from_str(include_str!("fixtures/aihot-2026-09-28.json")).unwrap();
        let items = raw
            .items
            .into_iter()
            .map(|f| Item {
                id: Some(f.id),
                title: Some(f.title),
                summary: f.summary,
                source: Some(Source { name: Some(f.source) }),
                published_at: f.published_at,
                discovered_at: f.discovered_at,
                category: f.category,
                score: f.score,
                ..Default::default()
            })
            .collect();
        (items, raw.groups)
    }

    fn pick(items: &[Item], ids: &[String]) -> Vec<Item> {
        ids.iter()
            .map(|id| {
                items
                    .iter()
                    .find(|item| item.id.as_deref() == Some(id))
                    .expect("夹具里有这条")
                    .clone()
            })
            .collect()
    }

    fn title_of(c: &Cluster) -> &str {
        c.lead.title.as_deref().unwrap()
    }

    #[test]
    fn tokens_keep_names_and_numbers_and_drop_boilerplate() {
        let t = tokens("Anthropic 发布 Claude Opus 5.5 (High)，速度提升 30%，GPT-6 与 MiMo-V2.6-Pro");
        for expect in ["anthropic", "claude", "opus", "5.5", "high", "30%", "gpt-6", "mimo-v2.6-pro"] {
            assert!(t.contains(expect), "缺 {expect}：{t:?}");
        }
        assert!(t.contains("速度") && t.contains("度提") && t.contains("提升"));
        // 「发布」「与」是虚词，剪掉后不与邻字连成词元
        assert!(!t.contains("发布") && !t.contains("布 ") && !t.contains("与M"));
        // 单个字母与英文虚词不算
        assert!(!t.contains("ai") && !t.contains("a"));
    }

    #[test]
    fn stop_words_cut_the_run_instead_of_bridging_it() {
        // 「推出全新模型」剪掉「推出」「全新」后剩「模型」，不会产生「出全」「新模」
        let t = tokens("推出全新模型");
        assert_eq!(t, HashSet::from(["模型".to_string()]));
    }

    #[test]
    fn every_announcement_folds_into_one_event() {
        let (items, groups) = fixture();
        for name in ["sonnet", "arena", "gpt6", "voice"] {
            let clusters = fold(pick(&items, &groups[name]));
            assert_eq!(
                clusters.len(),
                1,
                "{name} 应折成一个事件，实际：{:?}",
                clusters.iter().map(title_of).collect::<Vec<_>>()
            );
        }
    }

    /// 已知的漏折：四家把「Meta 请 MongoDB 前 CEO 管企业平台」写成了四种句式，
    /// 只有一半连得上。漏折的代价是多看一条，这里把现状钉住，免得阈值一动就悄悄变。
    #[test]
    fn the_meta_hire_is_a_known_partial_miss() {
        let (items, groups) = fixture();
        let clusters = fold(pick(&items, &groups["meta"]));
        assert!(
            (1..=2).contains(&clusters.len()),
            "{:?}",
            clusters.iter().map(title_of).collect::<Vec<_>>()
        );
    }

    #[test]
    fn unrelated_news_stays_apart() {
        let (items, groups) = fixture();
        let others = pick(&items, &groups["others"]);
        let n = others.len();
        assert_eq!(fold(others).len(), n);

        // 每个事件挑一条与不相干的混在一起，事件数不多不少
        let mut mixed = Vec::new();
        for name in ["sonnet", "arena", "gpt6", "voice"] {
            mixed.push(pick(&items, &groups[name][..1])[0].clone());
        }
        mixed.extend(pick(&items, &groups["others"]));
        assert_eq!(fold(mixed).len(), 4 + n);
    }

    #[test]
    fn a_review_of_a_launch_is_not_the_launch() {
        let (items, groups) = fixture();
        let gpt6 = pick(&items, &groups["gpt6"]);
        let review = pick(&items, &groups["others"])
            .into_iter()
            .find(|item| item.title.as_deref().unwrap().contains("Artificial Analysis 评测"))
            .unwrap();
        let mut batch = gpt6;
        batch.push(review);
        assert_eq!(fold(batch).len(), 2);
    }

    #[test]
    fn two_related_stories_from_the_same_saga_are_kept_separate() {
        // 同一场风波里的两件事：TechCrunch 的对齐报告站，与 Ars 的暂停训练
        let (items, groups) = fixture();
        let others = pick(&items, &groups["others"]);
        let pair: Vec<Item> = others
            .into_iter()
            .filter(|item| {
                let title = item.title.as_deref().unwrap();
                title.contains("对齐报告站") || title.contains("暂停前沿模型训练")
            })
            .collect();
        assert_eq!(pair.len(), 2);
        assert_eq!(fold(pair).len(), 2);
    }

    #[test]
    fn the_first_reporter_leads_and_the_rest_are_in_order() {
        let (items, groups) = fixture();
        let mut batch = pick(&items, &groups["sonnet"]);
        // 展示顺序是新的在前
        batch.sort_by_key(|item| std::cmp::Reverse(item.first_reported_ts()));
        let clusters = fold(batch);
        let cluster = &clusters[0];

        let published: Vec<i64> = cluster.members().filter_map(Item::first_reported_ts).collect();
        assert_eq!(cluster.also.len(), 4);
        assert_eq!(published[0], *published.iter().min().unwrap(), "代表是最早的");
        assert!(published[1..].windows(2).all(|w| w[0] <= w[1]), "其余按先后排");
    }

    #[test]
    fn an_event_sits_where_its_first_listed_member_sits() {
        let (items, groups) = fixture();
        let sonnet = pick(&items, &groups["sonnet"]);
        let others = pick(&items, &groups["others"]);
        let mut batch = vec![others[0].clone()];
        batch.extend(sonnet);
        batch.push(others[1].clone());
        let clusters = fold(batch);
        assert_eq!(clusters.len(), 3);
        assert_eq!(clusters[0].also.len(), 0);
        assert_eq!(clusters[1].also.len(), 4);
        assert_eq!(clusters[2].also.len(), 0);
    }

    #[test]
    fn identical_headlines_days_apart_are_different_events() {
        let mut a = Item {
            id: Some("a".into()),
            title: Some("OpenAI 发布 GPT-6 Sol 和 GPT-6 Luna，API 价格较 GPT-5.6 降 50%".into()),
            summary: Some("OpenAI 发布 GPT-6 Sol 与 Luna，两款模型的 API 价格比 GPT-5.6 降低一半".into()),
            published_at: Some("2026-09-22T19:13:00Z".into()),
            discovered_at: Some("2026-09-22T19:13:00Z".into()),
            ..Default::default()
        };
        let mut b = a.clone();
        b.id = Some("b".into());
        b.published_at = Some("2026-09-25T19:13:00Z".into());
        b.discovered_at = b.published_at.clone();
        assert_eq!(fold(vec![a.clone(), b.clone()]).len(), 2);

        b.published_at = Some("2026-09-22T20:13:00Z".into());
        b.discovered_at = b.published_at.clone();
        assert_eq!(fold(vec![a.clone(), b]).len(), 1);
        a.title = None;
        a.summary = None;
        assert_eq!(fold(vec![a]).len(), 1, "空条目不会 panic");
    }

    #[test]
    fn two_word_headlines_do_not_fold_by_coincidence() {
        let mk = |id: &str, title: &str| Item {
            id: Some(id.into()),
            title: Some(title.into()),
            ..Default::default()
        };
        let clusters = fold(vec![mk("1", "Manus 2.0 发布"), mk("2", "Manus 2.0 发布")]);
        assert_eq!(clusters.len(), 2, "共同词元不足 {MIN_SHARED} 个时不判定");
    }

    #[test]
    fn other_sources_are_cleaned_deduplicated_and_exclude_the_lead() {
        let (items, groups) = fixture();
        let clusters = fold(pick(&items, &groups["sonnet"]));
        let cluster = &clusters[0];
        let names = cluster.other_sources();

        let lead = cluster.lead.source_label().unwrap();
        assert!(!names.contains(&lead));
        let unique: HashSet<&String> = names.iter().collect();
        assert_eq!(unique.len(), names.len());
        assert!(names.iter().all(|n| !n.contains("（RSS）") && !n.contains("(@")), "{names:?}");
        assert!(names.len() < cluster.also.len() + 1);
    }

    #[test]
    fn fold_can_be_turned_off() {
        let (items, groups) = fixture();
        let batch = pick(&items, &groups["sonnet"]);
        assert_eq!(fold_if(false, batch.clone()).len(), batch.len());
        assert_eq!(fold_if(true, batch).len(), 1);
    }

    /// 全夹具当一批跑：事件数与夹具里标好的一致，没有把任何两组并到一起
    #[test]
    fn the_whole_fixture_yields_exactly_the_labelled_events() {
        let (items, groups) = fixture();
        let meta = fold(pick(&items, &groups["meta"])).len();
        let expected = 4 + meta + groups["others"].len();
        let clusters = fold(items);
        assert_eq!(
            clusters.len(),
            expected,
            "{:?}",
            clusters.iter().map(title_of).collect::<Vec<_>>()
        );
    }
}
