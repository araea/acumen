//! 分群的打字习惯：号主在每个群里亲手打字时，长短、句尾、引用、连发各是什么样。
//!
//! 同一个人在不同的群里不是同一副口气。2026-10-02 对着记录量过：中位字数 ④群 4、
//! ②群 4、驾校群 10；句尾挂「（」「）」的话占比从 3.7% 到 36.6%；一个回合只发一条的
//! 比例从 29% 到 59%。人设里写死的「四到十个字」只对其中一部分群成立，而样本又是不分群
//! 抽的——机器人在一个群里说的话，长短与句尾习惯是所有群揉在一起的平均。
//!
//! 这里按群把他最近亲手打的话量一遍，每轮发言往提示词里放一句「你在这个群里是这么
//! 打字的」。它和样本一样只借劲儿：说的是形状（多长、挂不挂「（」、引不引用），不是
//! 配额，更不是内容。

/// 量习惯至少要有这么多条文字消息，少了不下结论。
const MIN_LINES: usize = 30;

/// 一个群里的打字习惯。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Habit {
    /// 量了多少条文字消息。
    pub lines: usize,
    /// 字数的下四分位、中位、上四分位。
    pub p25: usize,
    pub p50: usize,
    pub p75: usize,
    /// 句尾挂「（」或「）」的比例。
    pub paren: f32,
    /// 带引用的比例（所有手打消息，含只有图的）。
    pub quoted: f32,
    /// 一个回合（三十秒内连着发的算一个回合）只发一条的比例。
    pub single: f32,
    /// 他在这个群里平时一天说几个回合（按天取中位数，不被放假那种一天爆发的日子带偏）；
    /// 日子不够、量不出来是 `None`。
    pub daily_turns: Option<f32>,
}

/// 他一天里大概有几个钟头会在群里说话：把「一天几个回合」摊成「每小时几个回合」用的。
const ACTIVE_HOURS: f32 = 10.0;
/// 机器人在群里的发言轮数，允许比他本人平时多这么多倍：它顶的是他不在的时候，
/// 但不该比他本人在群里还爱说话。
const HEADROOM: f32 = 1.5;
/// 量日均回合数至少要有几天的跨度。
const MIN_SPAN_DAYS: i64 = 5;

/// 三十秒内接着发的，算同一个回合。
const TURN_GAP: i64 = 30;

impl Habit {
    /// `rows` 是这个群里他亲手打的消息 `(时刻, 清掉占位符的正文, 带不带引用)`，按时间正序。
    /// 空正文（只有图、只有表情）不算文字消息，但算引用与回合。
    pub(super) fn measure(rows: &[(i64, String, bool)], now: i64) -> Option<Habit> {
        let mut lengths: Vec<usize> = rows
            .iter()
            .filter(|(_, text, _)| !text.is_empty())
            .map(|(_, text, _)| text.chars().count())
            .collect();
        if lengths.len() < MIN_LINES {
            return None;
        }
        lengths.sort_unstable();
        let at = |q: f32| lengths[((lengths.len() - 1) as f32 * q).round() as usize];
        let texts = lengths.len() as f32;
        let paren = rows
            .iter()
            .filter(|(_, text, _)| text.ends_with(['（', '）', '(', ')']))
            .count() as f32
            / texts;
        let quoted = rows.iter().filter(|(_, _, quoted)| *quoted).count() as f32
            / rows.len() as f32;
        let mut starts: Vec<i64> = Vec::new();
        let mut singles = 0usize;
        let mut run = 0usize;
        let mut last = i64::MIN;
        for (time, _, _) in rows {
            if run > 0 && time - last <= TURN_GAP {
                run += 1;
            } else {
                if run == 1 {
                    singles += 1;
                }
                run = 1;
                starts.push(*time);
            }
            last = *time;
        }
        if run == 1 {
            singles += 1;
        }
        let turns = starts.len();
        Some(Habit {
            lines: lengths.len(),
            p25: at(0.25),
            p50: at(0.5),
            p75: at(0.75),
            paren,
            quoted,
            single: singles as f32 / turns.max(1) as f32,
            daily_turns: median_daily(&starts, now),
        })
    }

    /// 机器人在这个群里每小时说几轮算「跟他本人差不多」：他平时的日均回合数摊到每小时，
    /// 再放宽一点，至少一轮。量不出来就是 `None`，由配置里的统一值管。
    ///
    /// 同样是搭话群，驾校群里他一天说十来句、占全群的一成多；④群大群里一个月都说不了几句。
    /// 前者每小时一两轮是他的常态，后者一小时说两轮，就已经是他平时一整天的量了。
    pub(super) fn hourly_target(&self) -> Option<usize> {
        self.daily_turns
            .map(|daily| ((daily / ACTIVE_HOURS) * HEADROOM).ceil().max(1.0) as usize)
    }

    /// 放进发言提示词的一句。
    pub(super) fn brief(&self) -> String {
        let mut parts = vec![format!(
            "字数多是 {} 到 {} 个（中位 {}）",
            self.p25.max(1),
            self.p75.max(2),
            self.p50.max(1)
        )];
        if self.paren >= 0.03 {
            parts.push(
                match self.paren {
                    p if p >= 0.25 => "经常（三四句里就有一句）在句尾挂「（」或「）」".to_string(),
                    p if p >= 0.10 => "常在句尾挂「（」或「）」（十句里一两句）".to_string(),
                    _ => "偶尔在句尾挂「（」或「）」".to_string(),
                },
            );
        }
        if self.quoted >= 0.05 {
            parts.push(format!(
                "大约每 {} 句里有 1 句引用别人",
                (1.0 / self.quoted).round().max(2.0)
            ));
        } else {
            parts.push("几乎不用引用".to_string());
        }
        parts.push(if self.single >= 0.7 {
            "基本一回只发一条".to_string()
        } else if self.single >= 0.45 {
            "一半左右的回合只发一条，其余会连着补一两条".to_string()
        } else {
            "常常连着发两三条".to_string()
        });
        format!(
            "你在这个群里亲手打字的习惯（照你自己最近 {} 句量的，借它的劲儿，不是配额）：{}。\n",
            self.lines,
            parts.join("；")
        )
    }
}

/// 回合起点（Unix 秒）→ 每天回合数的中位数。今天还没过完，不算；没说话的日子算 0。
fn median_daily(starts: &[i64], now: i64) -> Option<f32> {
    use chrono::TimeZone as _;
    let day = |at: i64| chrono::Local.timestamp_opt(at, 0).single().map(|t| t.date_naive());
    let today = day(now)?;
    let mut per_day: std::collections::BTreeMap<chrono::NaiveDate, usize> = Default::default();
    for start in starts {
        let date = day(*start)?;
        if date < today {
            *per_day.entry(date).or_default() += 1;
        }
    }
    let first = *per_day.keys().next()?;
    let span = (today - first).num_days();
    if span < MIN_SPAN_DAYS {
        return None;
    }
    let mut counts: Vec<usize> = (0..span)
        .map(|offset| {
            per_day
                .get(&(first + chrono::Duration::days(offset)))
                .copied()
                .unwrap_or(0)
        })
        .collect();
    counts.sort_unstable();
    Some(counts[counts.len() / 2] as f32)
}
