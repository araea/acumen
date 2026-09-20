//! 用户画像的素材采集。
//!
//! 画像需要两类东西：可统计的事实（发了多少、什么时候发、在哪发、发的是什么）
//! 与可阅读的原文（给模型看，也拿来做报告里的引用）。两者都只读
//! `message_records`，不写任何数据。
//!
//! 范围限定在群聊（`group_id != 0`）且排除 `role = 'self'`。前者是因为画像讲的是
//! 「在群里是什么样」，与机器人的私聊不该混进来；后者是这台机器的特殊情况——
//! 机器人与号主共用同一个 QQ 号，记录靠 `role` 区分人和机，不排掉就会把机器人
//! 自己生成的那些话算成号主的。

use crate::plugins::wordcloud::stopwords::get_stop_words;
use sea_orm::{DatabaseConnection, DbErr, FromQueryResult, Statement};
use std::collections::HashSet;

/// 单条样本的字数上限。太长的发言在提示词里性价比很低，截断即可。
const SAMPLE_MAX_CHARS: usize = 90;
/// 送进提示词的样本总字数上限，防止素材把上下文挤爆。
const SAMPLE_TOTAL_CHARS: usize = 9_000;
/// 无论预算怎么紧，至少留下这么多条样本。
const SAMPLE_MIN_KEEP: usize = 8;
/// 最长的这几条一定入选。
const MANDATORY_LONG: usize = 3;

/// 一次采集的窗口参数。
pub struct Request {
    pub user_id: i64,
    pub start: i64,
    pub end: i64,
    /// 从库里最多读多少条原始记录；越大越慢，也越完整。
    pub max_scan: u64,
    pub max_samples: usize,
}

/// 消息类型的构成。
#[derive(Debug, Default, Clone, Copy)]
pub struct Kinds {
    pub text: u64,
    pub image: u64,
    pub anim_emoji: u64,
    pub face: u64,
    pub voice: u64,
    pub video: u64,
    pub reply: u64,
    pub at: u64,
}

/// 某个群里的发言量。
#[derive(Debug, Clone)]
pub struct GroupSlice {
    pub name: String,
    pub count: u64,
}

/// 语言与行为指纹——**全部由事实算出**，不经过模型，因此是可核验的那一层。
///
/// 这一组数是新版画像的地基：它回答旧版漏掉的那个问题——「这个人是怎么说话的」。
/// 每一个字段都是能在 `message_records` 里数出来的比例或形状，不掺一点推断；模型读
/// 的是这组数，不是凭空猜。分母统一用**可读发言**（`length > 0` 且去掉纯占位标记后
/// 仍有字的那些条），空消息、纯图片纯表情不进这一层。
#[derive(Debug, Clone, Copy, Default)]
pub struct Style {
    /// 可读发言条数——这一层的分母。
    pub readable: u64,
    /// 以 ？/? 收尾的消息占比。提问倾向。
    pub question_rate: f64,
    /// 以 ！/! 收尾的消息占比。
    pub exclaim_rate: f64,
    /// 含省略号（… / ... / 。。）的消息占比。
    pub ellipsis_rate: f64,
    /// 平均每条逗号数。长句、铺垫倾向。
    pub comma_per_msg: f64,
    /// 含笑声词（哈哈 / hh / 233 / xswl / lol / 笑死）的消息占比。
    pub laugh_rate: f64,
    /// 含语气词（吧呢哦啊嘛啦诶呀）的消息占比。
    pub modal_rate: f64,
    /// 每百字自称（我/俺/咱）出现次数。
    pub self_per100: f64,
    /// 每百字对称呼（你/您）出现次数。对着人说，还是自言自语。
    pub you_per100: f64,
    /// 消息长度变异系数（标准差 / 均值）。起伏大 = 时短时长，起伏小 = 匀称。
    pub len_cv: f64,
    /// 长消息（≥ 40 字）占比。
    pub long_rate: f64,
    /// 短消息（≤ 5 字）占比。
    pub short_rate: f64,
    /// 发言间隔的爆发指数：同一条时间线上相邻发言间隔的标准差与均值，按
    /// (σ−μ)/(σ+μ) 归一。-1 表示像钟摆一样匀，0 表示随机，接近 +1 表示一阵一阵。
    /// 用这个有界的量而不是方差除均值：后者在秒级的连发里会蹿到几万，既不可读也压不住。
    pub burstiness: f64,
    /// 与上一条内容相同的占比。口头禅、复读机倾向。
    pub repeat_rate: f64,
}

/// 采集结果。字段都是报告直接要用的形状，呈现层不再回头查库。
#[derive(Debug)]
pub struct Material {
    pub user_id: i64,
    /// 群里最常用的名字：有群名片用群名片，否则用昵称。
    pub name: String,
    pub total: u64,
    pub first_time: i64,
    pub last_time: i64,
    pub active_days: u64,
    pub hour: [u64; 24],
    pub weekday: [u64; 7],
    pub groups: Vec<GroupSlice>,
    pub kinds: Kinds,
    pub longest: u64,
    pub avg_len: f64,
    /// 高频词与出现次数。
    pub words: Vec<(String, u64)>,
    /// 交给模型的发言样本，按时间由近及远；模型引用的原话必须出自这里。
    pub samples: Vec<String>,
    /// 语言与行为指纹，全部由事实算出，见 [`Style`]。
    pub style: Style,
}

impl Material {
    /// 发言最集中的那一小时（0—23）。没有记录时返回 0。
    pub fn peak_hour(&self) -> usize {
        peak_index(&self.hour)
    }

    /// 最活跃的一天（0 为周日）。没有记录时返回 0。
    pub fn peak_weekday(&self) -> usize {
        peak_index(&self.weekday)
    }

    /// 夜间（0:00—5:59）发言占比。
    pub fn night_ratio(&self) -> f64 {
        ratio(self.hour[0..6].iter().sum(), self.total)
    }

    /// 带图/表情包/小表情的发言占比。
    pub fn media_ratio(&self) -> f64 {
        let visual =
            self.kinds.image + self.kinds.anim_emoji + self.kinds.face + self.kinds.video;
        ratio(visual, self.total)
    }

    /// 引用别人的次数占比。
    pub fn reply_ratio(&self) -> f64 {
        ratio(self.kinds.reply, self.total)
    }

    /// 平均每条发言的字数。
    pub fn avg_len(&self) -> f64 {
        self.avg_len
    }

    /// 覆盖的天数（首末发言之间的自然天，至少 1）。
    pub fn span_days(&self) -> i64 {
        let seconds = (self.last_time - self.first_time).max(0);
        (seconds / 86_400 + 1).max(1)
    }

    /// 平均每天发几条。
    pub fn per_day(&self) -> f64 {
        self.total as f64 / self.span_days() as f64
    }
}

fn ratio(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

/// 样本充分性：一份画像能信到什么程度，先看证据有多少。
///
/// 阈值是拍出来的，不是算出来的：两百条以上、铺满两周以上，四个维度才都站得住；
/// 五十条、五天以上勉强能读出活跃与表达；再少就只剩几条事实标签。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sufficiency {
    Enough,
    Fair,
    Thin,
}

impl Sufficiency {
    pub fn label(self) -> &'static str {
        match self {
            Sufficiency::Enough => "充分",
            Sufficiency::Fair => "一般",
            Sufficiency::Thin => "有限",
        }
    }
}

impl Material {
    /// 这份素材够不够撑起一份画像。
    pub fn sufficiency(&self) -> Sufficiency {
        if self.total >= 200 && self.active_days >= 14 {
            Sufficiency::Enough
        } else if self.total >= 50 && self.active_days >= 5 {
            Sufficiency::Fair
        } else {
            Sufficiency::Thin
        }
    }
}

fn peak_index(values: &[u64]) -> usize {
    let mut best = 0usize;
    for (index, value) in values.iter().enumerate() {
        if *value > values[best] {
            best = index;
        }
    }
    best
}

#[derive(Debug, FromQueryResult)]
struct Totals {
    total: i64,
    days: i64,
    first_time: Option<i64>,
    last_time: Option<i64>,
    texts: i64,
    images: i64,
    anim: i64,
    faces: i64,
    voices: i64,
    videos: i64,
    replies: i64,
    ats: i64,
    longest: i64,
    avg_len: f64,
}

#[derive(Debug, FromQueryResult)]
struct BucketRow {
    bucket: i32,
    count: i64,
}

#[derive(Debug, FromQueryResult)]
struct GroupRow {
    name: String,
    count: i64,
}

#[derive(Debug, FromQueryResult)]
struct TextRow {
    content: String,
    length: i32,
    tokens: String,
    time: i64,
}

/// 采集某个用户的群聊发言素材。窗口内没有记录时返回 `None`。
pub async fn collect(
    db: &DatabaseConnection,
    request: &Request,
) -> Result<Option<Material>, DbErr> {
    let scope = format!(
        "FROM message_records WHERE user_id = {} AND role != 'self' AND group_id != 0 \
         AND time >= {} AND time < {}",
        request.user_id, request.start, request.end
    );

    let totals_sql = format!(
        "SELECT COUNT(*) AS total, \
         COUNT(DISTINCT strftime('%Y-%m-%d', datetime(time, 'unixepoch', 'localtime'))) AS days, \
         MIN(time) AS first_time, MAX(time) AS last_time, \
         COALESCE(SUM(CASE WHEN length > 0 THEN 1 ELSE 0 END), 0) AS texts, \
         COALESCE(SUM(image_count), 0) AS images, \
         COALESCE(SUM(CASE WHEN is_anim_emoji THEN 1 ELSE 0 END), 0) AS anim, \
         COALESCE(SUM(face_count), 0) AS faces, \
         COALESCE(SUM(CASE WHEN is_voice THEN 1 ELSE 0 END), 0) AS voices, \
         COALESCE(SUM(CASE WHEN is_video THEN 1 ELSE 0 END), 0) AS videos, \
         COALESCE(SUM(CASE WHEN is_reply THEN 1 ELSE 0 END), 0) AS replies, \
         COALESCE(SUM(at_count), 0) AS ats, \
         COALESCE(MAX(length), 0) AS longest, \
         CAST(COALESCE(AVG(CASE WHEN length > 0 THEN length END), 0) AS REAL) AS avg_len {scope}"
    );
    let backend = db.get_database_backend();
    let totals = Totals::find_by_statement(Statement::from_string(backend, totals_sql))
        .one(db)
        .await?
        .unwrap_or(Totals {
            total: 0,
            days: 0,
            first_time: None,
            last_time: None,
            texts: 0,
            images: 0,
            anim: 0,
            faces: 0,
            voices: 0,
            videos: 0,
            replies: 0,
            ats: 0,
            longest: 0,
            avg_len: 0.0,
        });
    if totals.total <= 0 {
        return Ok(None);
    }

    let hours = buckets(
        db,
        &format!("SELECT time_hour AS bucket, COUNT(*) AS count {scope} GROUP BY time_hour"),
    )
    .await?;
    let weekdays = buckets(
        db,
        &format!(
            "SELECT time_weekday AS bucket, COUNT(*) AS count {scope} GROUP BY time_weekday"
        ),
    )
    .await?;

    let mut hour = [0u64; 24];
    for row in hours {
        if let Some(slot) = hour.get_mut(row.bucket.max(0) as usize) {
            *slot = row.count.max(0) as u64;
        }
    }
    let mut weekday = [0u64; 7];
    for row in weekdays {
        if let Some(slot) = weekday.get_mut(row.bucket.max(0) as usize) {
            *slot = row.count.max(0) as u64;
        }
    }

    let groups = GroupRow::find_by_statement(Statement::from_string(
        backend,
        format!(
            "SELECT MAX(group_name) AS name, COUNT(*) AS count {scope} \
             GROUP BY group_id ORDER BY count DESC LIMIT 6"
        ),
    ))
    .all(db)
    .await?
    .into_iter()
    .map(|row| GroupSlice {
        name: if row.name.trim().is_empty() {
            "（未知群）".to_string()
        } else {
            row.name
        },
        count: row.count.max(0) as u64,
    })
    .collect();

    let name = display_name(db, request).await?;
    let rows = TextRow::find_by_statement(Statement::from_string(
        backend,
        format!(
            "SELECT content_rich AS content, length, tokens, time {scope} \
             ORDER BY time DESC LIMIT {}",
            request.max_scan
        ),
    ))
    .all(db)
    .await?;

    let words = top_words(&rows);
    let samples = pick_samples(&rows, request.max_samples);
    let style = style_of(&rows);

    Ok(Some(Material {
        user_id: request.user_id,
        name,
        total: totals.total.max(0) as u64,
        first_time: totals.first_time.unwrap_or(request.end),
        last_time: totals.last_time.unwrap_or(request.end),
        active_days: totals.days.max(0) as u64,
        hour,
        weekday,
        groups,
        kinds: Kinds {
            text: totals.texts.max(0) as u64,
            image: totals.images.max(0) as u64,
            anim_emoji: totals.anim.max(0) as u64,
            face: totals.faces.max(0) as u64,
            voice: totals.voices.max(0) as u64,
            video: totals.videos.max(0) as u64,
            reply: totals.replies.max(0) as u64,
            at: totals.ats.max(0) as u64,
        },
        longest: totals.longest.max(0) as u64,
        avg_len: totals.avg_len.max(0.0),
        words,
        samples,
        style,
    }))
}

async fn buckets(db: &DatabaseConnection, sql: &str) -> Result<Vec<BucketRow>, DbErr> {
    BucketRow::find_by_statement(Statement::from_string(db.get_database_backend(), sql.to_string()))
        .all(db)
        .await
}

/// 取群里最常用的名字。群名片比昵称更能代表「在群里是谁」，但在不同群里可能不一样，
/// 所以按出现次数投票，平手时取更近的那一个。
async fn display_name(db: &DatabaseConnection, request: &Request) -> Result<String, DbErr> {
    let scope = format!(
        "FROM message_records WHERE user_id = {} AND role != 'self' AND group_id != 0 \
         AND time >= {} AND time < {}",
        request.user_id, request.start, request.end
    );
    #[derive(Debug, FromQueryResult)]
    struct NameRow {
        name: String,
        count: i64,
    }
    let row = NameRow::find_by_statement(Statement::from_string(
        db.get_database_backend(),
        format!(
            "SELECT name, COUNT(*) AS count FROM (\
               SELECT CASE WHEN TRIM(sender_nick) != '' THEN sender_nick \
                           WHEN TRIM(user_name) != '' THEN user_name \
                           ELSE '' END AS name, time \
               {scope}\
             ) GROUP BY name ORDER BY count DESC, MAX(time) DESC LIMIT 1"
        ),
    ))
    .one(db)
    .await?;
    let name = row.map(|row| row.name).unwrap_or_default();
    Ok(if name.trim().is_empty() {
        format!("QQ {}", request.user_id)
    } else {
        name.trim().to_string()
    })
}

/// 高频词。分词结果在 `tokens` 列里已由录制插件算好，这里只做计数与过滤，
/// 规则与词云插件保持一致（单字与停用词不要）。
fn top_words(rows: &[TextRow]) -> Vec<(String, u64)> {
    let stop_words = get_stop_words();
    let mut counts: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
    for row in rows {
        for word in row.tokens.split_whitespace() {
            if word.chars().count() > 1
                && !stop_words.contains(word)
                && !word.chars().all(|c| c.is_ascii_digit())
            {
                *counts.entry(word).or_insert(0) += 1;
            }
        }
    }
    let mut words: Vec<(String, u64)> = counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(word, count)| (word.to_string(), count))
        .collect();
    words.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    words.truncate(24);
    words
}

/// 把富文本摘要里那些「只有标记没有话」的记录去掉。
///
/// 录制插件把图片写成 `[图片]`、表情写成 `[表情]`、@ 写成 `[@123]`，纯图片消息的
/// 摘要整条都是标记。这样的样本对理解这个人没有帮助，反而挤掉真正有内容的话。
fn meaningful(content: &str) -> Option<String> {
    let text = content.trim();
    if text.is_empty() {
        return None;
    }
    let mut rest = String::with_capacity(text.len());
    let mut depth = 0usize;
    for ch in text.chars() {
        match ch {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => rest.push(ch),
            _ => {}
        }
    }
    if rest.trim().is_empty() {
        return None;
    }
    Some(text.to_string())
}

/// 按时间分层挑样本。
///
/// 两种挑法各有各的毛病：只取最近的看不到这个人一年来的变化，随机取又常常
/// 全落在同一个晚上的闲聊里。所以先把最长的几条定下来——长发言信息密度最高，
/// 也最能体现一个人认真说话时的样子——再等距撒点把剩下的名额铺满整条时间线。
fn pick_samples(rows: &[TextRow], limit: usize) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    let mut pool: Vec<String> = Vec::new();
    // (字数, 在 pool 里的下标)，行是按时间倒序来的，所以下标越小越近。
    let mut by_length: Vec<(usize, usize)> = Vec::new();
    for row in rows {
        if row.length <= 0 {
            continue;
        }
        let Some(text) = meaningful(&row.content) else {
            continue;
        };
        if !seen.insert(text.clone()) {
            continue;
        }
        by_length.push((text.chars().count(), pool.len()));
        pool.push(text);
    }
    if pool.is_empty() {
        return Vec::new();
    }

    by_length.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut picked: Vec<usize> = by_length
        .iter()
        .take(limit.min(MANDATORY_LONG))
        .map(|(_, index)| *index)
        .collect();
    let fill = limit.saturating_sub(picked.len());
    for index in spread(pool.len(), fill) {
        if !picked.contains(&index) {
            picked.push(index);
        }
    }
    // 时间重排，让模型读到的是一条时间线。
    picked.sort_unstable();
    picked.dedup();

    truncate_samples(picked.into_iter().map(|i| pool[i].clone()).collect())
}

/// 在 `0..len` 上等距取 `count` 个下标，首尾都要取到。
fn spread(len: usize, count: usize) -> Vec<usize> {
    if count == 0 || len == 0 {
        return Vec::new();
    }
    if count == 1 {
        return vec![0];
    }
    if len <= count {
        return (0..len).collect();
    }
    (0..count)
        .map(|k| k * (len - 1) / (count - 1))
        .collect()
}

/// 逐条截断，再按总字数预算收口。实在放不下就丢最早的那几条。
fn truncate_samples(samples: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut budget = SAMPLE_TOTAL_CHARS;
    for text in samples {
        let text: String = text.chars().take(SAMPLE_MAX_CHARS).collect();
        let cost = text.chars().count();
        if cost > budget && out.len() >= SAMPLE_MIN_KEEP {
            break;
        }
        budget = budget.saturating_sub(cost);
        out.push(text);
    }
    out
}

// ==================== 语言与行为指纹 ====================

/// 语气词表。命中任意一个即算这条带语气词。
const MODALS: [char; 8] = ['吧', '呢', '哦', '啊', '嘛', '啦', '诶', '呀'];

/// 笑声词判定。只收几个误伤小的，字符逐个匹配比建一整套正则省事。
fn has_laugh(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("哈")
        || lower.contains("hh")
        || lower.contains("233")
        || lower.contains("xswl")
        || lower.contains("lol")
        || lower.contains("笑死")
        || lower.contains("wwww")
}

/// 数一个字符串里出现了几次 `needles` 里的任意字符（不去重，按出现次数累加）。
fn count_chars(text: &str, needles: &[char]) -> u64 {
    text.chars().filter(|ch| needles.contains(ch)).count() as u64
}

/// 从扫描到的原始行里算出 [`Style`]。
///
/// 分母只用**可读发言**：`length > 0` 且 `meaningful` 通过。句子按 `content_rich` 判定
/// （标点、笑声、语气词都在原文里），长度按 `length`（ recorder 已算好）。时间用来算爆发
/// 指数，按时间升序排，行本身是倒序来的。
fn style_of(rows: &[TextRow]) -> Style {
    let readable: Vec<&TextRow> = rows
        .iter()
        .filter(|row| row.length > 0 && meaningful(&row.content).is_some())
        .collect();
    let n = readable.len();
    if n == 0 {
        return Style::default();
    }
    let n_f = n as f64;

    let mut question = 0u64;
    let mut exclaim = 0u64;
    let mut ellipsis = 0u64;
    let mut comma_sum = 0u64;
    let mut laugh = 0u64;
    let mut modal = 0u64;
    let mut self_ref = 0u64;
    let mut you_ref = 0u64;
    let mut char_cnt = 0u64;
    let mut lengths: Vec<u64> = Vec::with_capacity(n);
    let mut times: Vec<i64> = Vec::with_capacity(n);
    for row in &readable {
        let text = row.content.trim();
        if text.ends_with(['?', '？']) {
            question += 1;
        }
        if text.ends_with(['!', '！']) {
            exclaim += 1;
        }
        if text.contains("…") || text.contains("...") || text.contains("。。") {
            ellipsis += 1;
        }
        comma_sum += count_chars(text, &[',', '，']);
        if has_laugh(text) {
            laugh += 1;
        }
        if text.chars().any(|ch| MODALS.contains(&ch)) {
            modal += 1;
        }
        self_ref += count_chars(text, &['我', '俺', '咱']);
        you_ref += count_chars(text, &['你', '您']);
        char_cnt += text.chars().count() as u64;
        lengths.push(row.length.max(0) as u64);
        times.push(row.time);
    }

    let ratio = |part: u64| part as f64 / n_f;
    let long = lengths.iter().filter(|&&l| l >= 40).count() as u64;
    let short = lengths.iter().filter(|&&l| l <= 5).count() as u64;
    let mean = lengths.iter().sum::<u64>() as f64 / n_f;
    let variance = if mean > 0.0 {
        lengths.iter().map(|&l| (l as f64 - mean).powi(2)).sum::<f64>() / n_f
    } else {
        0.0
    };
    let per100 = |count: u64| {
        if char_cnt == 0 {
            0.0
        } else {
            count as f64 / char_cnt as f64 * 100.0
        }
    };

    // 爆发指数：按时间升序算相邻间隔的 (σ−μ)/(σ+μ)。至少要有两个间隔才谈得上爆发。
    times.sort_unstable();
    let gaps: Vec<f64> = times
        .windows(2)
        .map(|w| (w[1] - w[0]).max(0) as f64)
        .collect();
    let burstiness = if gaps.len() >= 2 {
        let gmean = gaps.iter().sum::<f64>() / gaps.len() as f64;
        let gvar = gaps.iter().map(|g| (g - gmean).powi(2)).sum::<f64>() / gaps.len() as f64;
        let gstd = gvar.sqrt();
        if gstd + gmean > 0.0 {
            ((gstd - gmean) / (gstd + gmean)).clamp(-1.0, 1.0)
        } else {
            0.0
        }
    } else {
        0.0
    };

    // 相邻重复：按时间升序，与上一条内容相同即算一次。
    let chronological: Vec<&TextRow> = {
        let mut idx: Vec<usize> = (0..n).collect();
        idx.sort_by_key(|&i| times[i]);
        idx.into_iter().map(|i| readable[i]).collect()
    };
    let mut repeat = 0u64;
    for pair in chronological.windows(2) {
        if pair[0].content.trim() == pair[1].content.trim() {
            repeat += 1;
        }
    }

    Style {
        readable: n as u64,
        question_rate: ratio(question),
        exclaim_rate: ratio(exclaim),
        ellipsis_rate: ratio(ellipsis),
        comma_per_msg: comma_sum as f64 / n_f,
        laugh_rate: ratio(laugh),
        modal_rate: ratio(modal),
        self_per100: per100(self_ref),
        you_per100: per100(you_ref),
        len_cv: if mean > 0.0 {
            (variance.sqrt() / mean * 100.0).round() / 100.0
        } else {
            0.0
        },
        long_rate: ratio(long),
        short_rate: ratio(short),
        burstiness,
        repeat_rate: if n >= 2 {
            repeat as f64 / (n - 1) as f64
        } else {
            0.0
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::recorder::entity::Entity as RecordEntity;
    use sea_orm::{ConnectionTrait, Database, Schema};

    fn row(content: &str, length: i32) -> TextRow {
        TextRow {
            content: content.to_string(),
            length,
            tokens: String::new(),
            time: 0,
        }
    }

    /// 样本要铺满整条时间线，同时把最长的几条带上。
    #[test]
    fn samples_spread_over_time_and_keep_the_longest() {
        let mut rows: Vec<TextRow> = (0..24)
            .map(|i| row(&format!("第 {i} 条发言"), 8))
            .collect();
        rows[0] = row("最新的一条", 5);
        // 压轴的一条既是最旧的，也是最长的：两个条件都该把它选中。
        rows.push(row("这是一条特别长的发言，用来验证长样本会被挑中", 25));

        let samples = pick_samples(&rows, 6);
        assert!(samples.len() <= 6, "不该超过上限: {}", samples.len());
        assert!(samples.iter().any(|s| s == "最新的一条"), "{samples:?}");
        assert!(
            samples.iter().any(|s| s.starts_with("这是一条特别长")),
            "长样本必须入选: {samples:?}"
        );
    }

    #[test]
    fn spread_covers_both_ends() {
        assert_eq!(spread(200, 0), Vec::<usize>::new());
        assert_eq!(spread(0, 5), Vec::<usize>::new());
        assert_eq!(spread(3, 8), vec![0, 1, 2]);
        let picked = spread(200, 8);
        assert_eq!(picked.first(), Some(&0));
        assert_eq!(picked.last(), Some(&199));
        // 单调递增，不重复。
        assert!(picked.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn only_placeholder_messages_are_dropped() {
        assert_eq!(meaningful("哈哈哈"), Some("哈哈哈".to_string()));
        assert_eq!(meaningful("[图片]"), None);
        assert_eq!(meaningful("[表情]"), None);
        assert_eq!(meaningful("[@10001]"), None);
        assert_eq!(meaningful("  [图片] 这也算话"), Some("[图片] 这也算话".into()));
        assert_eq!(meaningful(""), None);
    }

    #[test]
    fn long_samples_are_clipped_and_the_budget_holds() {
        let samples: Vec<String> = (0..400)
            .map(|i| format!("{i}{}", "字".repeat(200)))
            .collect();
        let out = truncate_samples(samples);
        assert!(out.iter().all(|s| s.chars().count() <= SAMPLE_MAX_CHARS));
        let total: usize = out.iter().map(|s| s.chars().count()).sum();
        assert!(total <= SAMPLE_TOTAL_CHARS, "总预算被突破: {total}");
        assert!(out.len() >= SAMPLE_MIN_KEEP, "至少留 {} 条", SAMPLE_MIN_KEEP);
    }

    #[test]
    fn empty_input_yields_no_samples() {
        assert!(pick_samples(&[], 10).is_empty());
        assert!(pick_samples(&[row("[图片]", 0)], 10).is_empty());
    }

    /// 语言指纹只管可读发言，且每一项都数得准。
    #[test]
    fn the_style_fingerprint_counts_what_was_really_said() {
        let rows = vec![
            // 纯占位标题不进分母。
            row("[图片]", 0),
            row("你觉得呢？", 5),
            row("太强了！！", 5),
            row("哈哈哈哈哈笑死", 7),
            row("就这样吧。", 5),
            row("我说你好啊", 5),
        ];
        let s = style_of(&rows);
        assert_eq!(s.readable, 5, "纯图片那条不进分母");
        // 问号收尾 1/5
        assert!((s.question_rate - 0.2).abs() < 1e-9);
        // 感叹收尾 1/5
        assert!((s.exclaim_rate - 0.2).abs() < 1e-9);
        // 笑声词（哈哈、笑死）命中 1 条 → 0.2
        assert!((s.laugh_rate - 0.2).abs() < 1e-9);
        // 语气词（呢、吧、啊）命中 3 条 → 0.6
        assert!((s.modal_rate - 0.6).abs() < 1e-9);
        // 自称："我说你好啊" 有一个 我 → 每百字按总字数折算，> 0 即可
        assert!(s.self_per100 > 0.0);
    }

    /// 没有可读发言时指纹是全零，不崩。
    #[test]
    fn an_empty_style_is_all_zero() {
        let s = style_of(&[row("[图片]", 0)]);
        assert_eq!(s.readable, 0);
        assert_eq!(s.question_rate, 0.0);
        assert_eq!(s.len_cv, 0.0);
    }

    /// 爆发指数是个有界的量：秒级连发不会蹿到几万；完全匀的间隔趋向 -1。
    #[test]
    fn the_burstiness_is_bounded_and_scale_free() {
        let of = |times: &[i64]| {
            let rows: Vec<TextRow> = times
                .iter()
                .map(|&t| TextRow {
                    content: format!("话{t}"),
                    length: 2,
                    tokens: String::new(),
                    time: t,
                })
                .collect();
            style_of(&rows).burstiness
        };
        // 间隔全相等 → σ=0 → (0−μ)/(0+μ) = −1
        assert!((of(&[0, 60, 120, 180, 240]) - (-1.0)).abs() < 1e-9);
        // 秒级连发夹一个长空档：必然落在 [-1, 1]
        let b = of(&[0, 1, 2, 3, 4, 3600, 3601, 3602]);
        assert!((-1.0..=1.0).contains(&b), "burstiness = {b}");
    }

    #[test]
    fn ratios_and_peaks_come_from_the_histogram() {
        let material = Material {
            user_id: 1,
            name: "甲".into(),
            total: 100,
            first_time: 0,
            last_time: 86_400 * 9,
            active_days: 5,
            hour: {
                let mut hour = [0u64; 24];
                hour[23] = 40;
                hour[1] = 10;
                hour
            },
            weekday: {
                let mut weekday = [0u64; 7];
                weekday[3] = 30;
                weekday
            },
            groups: Vec::new(),
            kinds: Kinds {
                text: 50,
                image: 40,
                anim_emoji: 10,
                ..Default::default()
            },
            longest: 120,
            avg_len: 12.0,
            words: Vec::new(),
            samples: Vec::new(),
            style: Style::default(),
        };
        assert_eq!(material.peak_hour(), 23);
        assert_eq!(material.peak_weekday(), 3);
        assert!((material.night_ratio() - 0.1).abs() < 1e-9);
        assert!((material.media_ratio() - 0.5).abs() < 1e-9);
        // 首末跨 9 个整天 → 跨度为 10 天。
        assert_eq!(material.span_days(), 10);
        assert!((material.per_day() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn sufficiency_follows_how_much_evidence_there_is() {
        let mut material = Material {
            user_id: 1,
            name: "甲".into(),
            total: 0,
            first_time: 0,
            last_time: 0,
            active_days: 0,
            hour: [0; 24],
            weekday: [0; 7],
            groups: Vec::new(),
            kinds: Default::default(),
            longest: 0,
            avg_len: 0.0,
            words: Vec::new(),
            samples: Vec::new(),
            style: Style::default(),
        };
        assert_eq!(material.sufficiency(), Sufficiency::Thin);
        material.total = 60;
        assert_eq!(material.sufficiency(), Sufficiency::Thin, "条数够但天数不够");
        material.active_days = 6;
        assert_eq!(material.sufficiency(), Sufficiency::Fair);
        material.total = 200;
        material.active_days = 14;
        assert_eq!(material.sufficiency(), Sufficiency::Enough);
        assert_eq!(Sufficiency::Enough.label(), "充分");
    }

    async fn seeded_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let builder = db.get_database_backend();
        let schema = Schema::new(builder);
        let mut stmt = schema.create_table_from_entity(RecordEntity);
        stmt.if_not_exists();
        db.execute_raw(builder.build(&stmt)).await.unwrap();
        db
    }

    /// 测试里插一行要写全字段，参数多是故意的。
    #[allow(clippy::too_many_arguments)]
    async fn insert(
        db: &DatabaseConnection,
        user_id: i64,
        group_id: i64,
        group_name: &str,
        nick: &str,
        role: &str,
        time: i64,
        content: &str,
        length: i32,
        tokens: &str,
    ) {
        let sql = format!(
            "INSERT INTO message_records \
             (platform, group_id, group_name, user_id, user_name, sender_nick, message_type, \
              content_rich, tokens, role, is_reply, length, time, time_hour, time_weekday, \
              has_image, image_count, is_anim_emoji, has_at, at_count, face_count, \
              is_voice, is_video, is_music, is_rps, is_dice, is_poke, is_forward) VALUES \
             ('satori', {group_id}, '{group_name}', {user_id}, '{nick}', '{nick}', 'group', \
              '{content}', '{tokens}', '{role}', 0, {length}, {time}, {}, {}, 0, 0, 0, 0, 0, 0, \
              0, 0, 0, 0, 0, 0, 0)",
            (time / 3600) % 24,
            (time / 86_400) % 7,
        );
        db.execute_unprepared(&sql).await.unwrap();
    }

    #[tokio::test]
    async fn collect_reads_only_this_person_and_never_counts_the_bot() {
        let db = seeded_db().await;
        // 甲：两条真实发言，另有一条机器人的（role=self，同一个 QQ 号）。
        insert(&db, 7, 100, "测试群", "甲", "member", 1_700_000_000, "天气不错", 4, "天气 不错").await;
        insert(&db, 7, 100, "测试群", "甲", "member", 1_700_000_060, "天气转凉了", 5, "天气 转凉").await;
        insert(&db, 7, 100, "测试群", "甲", "self", 1_700_000_120, "我是机器人说的话", 8, "机器人 说话").await;
        insert(&db, 7, 0, "", "甲", "member", 1_700_000_180, "私聊不算", 4, "私聊 不算").await;
        insert(&db, 8, 100, "测试群", "乙", "member", 1_700_000_240, "别人的话", 4, "别人 的话").await;

        let request = Request {
            user_id: 7,
            start: 0,
            end: i64::MAX,
            max_scan: 100,
            max_samples: 10,
        };
        let material = collect(&db, &request).await.unwrap().unwrap();

        assert_eq!(material.total, 2, "只算本人在群里的发言");
        assert_eq!(material.name, "甲");
        assert_eq!(material.active_days, 1);
        assert_eq!(material.groups.len(), 1);
        assert_eq!(material.groups[0].name, "测试群");
        assert_eq!(material.samples.len(), 2);
        assert!(!material.samples.iter().any(|s| s.contains("机器人")));
        assert!(material.words.iter().any(|(word, _)| word == "天气"));
    }

    #[tokio::test]
    async fn a_user_without_records_yields_nothing() {
        let db = seeded_db().await;
        insert(&db, 7, 100, "测试群", "甲", "member", 1_700_000_000, "一句话", 3, "").await;
        let request = Request {
            user_id: 999,
            start: 0,
            end: i64::MAX,
            max_scan: 100,
            max_samples: 10,
        };
        assert!(collect(&db, &request).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn the_window_leaves_out_older_records() {
        let db = seeded_db().await;
        insert(&db, 7, 100, "测试群", "甲", "member", 1_700_000_000, "新的", 2, "").await;
        insert(&db, 7, 100, "测试群", "甲", "member", 1_600_000_000, "旧的", 2, "").await;
        let request = Request {
            user_id: 7,
            start: 1_650_000_000,
            end: i64::MAX,
            max_scan: 100,
            max_samples: 10,
        };
        let material = collect(&db, &request).await.unwrap().unwrap();
        assert_eq!(material.total, 1);
        assert_eq!(material.samples, vec!["新的".to_string()]);
    }
}
