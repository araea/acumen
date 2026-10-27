//! 「我的」：一个人的发言报表。
//!
//! 和榜单不是同一件事：榜回答「谁最多」，这里回答「我这段时间怎么样」。所以它不排别人，
//! 只画自己的一条线，再把几个一眼要看的数摆在标题下面——合计、活跃、高峰、名次。
//!
//! 三条口径上的约定：
//!
//! - **今日是 0 点到此刻，昨日是 0 点到 24 点。** 前者按小时分桶只画到当前这一格，
//!   后者是完整 24 格；拿今日和昨日比时比的是「同期」，不是拿半天空场去对一整天。
//! - **近 7 天、本周、本月这些按自然日对齐**，并且没说话的那天补 0，不跳格——
//!   走势图的意义在缺口本身，「三天没说话」不该被一条直接连过去的线藏掉。
//! - **「我的」默认跨群**，前面加「本群」才只看这一个群；名次也只有本群才有，
//!   跨群时那一格换成「参与几个群」。

use super::ChartError;
use super::data_loader::{
    Axis, ChartDataPoint, FALLBACK_THEME, SeriesData, message_type_style, user_metric,
};
use super::renderer::StatCell;
use crate::db::queries::{self, MessageTypeTrend, TrendGranularity, UserMetric};
use chrono::{Datelike, Duration, Local, Months, TimeZone, Timelike};
use sea_orm::DatabaseConnection;
use std::collections::HashMap;

/// 横轴的一格有多长。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Hour,
    Day,
    Month,
}

impl Unit {
    fn granularity(self) -> TrendGranularity {
        match self {
            Self::Hour => TrendGranularity::Hour,
            Self::Day => TrendGranularity::Day,
            Self::Month => TrendGranularity::Month,
        }
    }

    fn span_word(self) -> &'static str {
        match self {
            Self::Hour => "小时",
            Self::Day => "天",
            Self::Month => "个月",
        }
    }
}

/// 横轴上的一格。标签与 SQL 的日期键逐字相同，查询回来的行按它落格。
struct Bucket {
    label: String,
}

/// 这段时间以及它的「上一期」——对比要拿同样长的一段比，不然数字没有意义。
pub struct Period {
    pub start: i64,
    pub end: i64,
    prev: Option<(i64, i64)>,
    prev_label: &'static str,
    unit: Unit,
    buckets: Vec<Bucket>,
}

/// 这张报表要用到的一切。
pub struct MineReport {
    pub meta: String,
    pub cells: Vec<StatCell>,
    pub series: Vec<SeriesData>,
}

/// 生成报表。区间与分桶都在这里定，调用方只需要给「谁、看哪段、看什么」。
pub async fn build(
    db: &DatabaseConnection,
    group_id: Option<&str>,
    user_id: &str,
    data_type: &str,
    time_str: &str,
) -> Result<MineReport, ChartError> {
    // 「总」的横轴从我的第一条消息算起；本群只看这个群，别把别的群的历史
    // 铺成一长串零。
    let first_seen = if time_str == "总" {
        queries::get_user_first_message_time(db, group_id, user_id)
            .await
            .map_err(|e| e.to_string())?
    } else {
        None
    };
    let period = period(time_str, first_seen);

    // 消息类型是多条线，其余数据类型是一条线。
    //
    // 三样东西一起产出：要画的线、单位、以及**我自己**每格的数——标题下那几个
    // 统计只看最后一样。消息类型的六条线是互有重叠的属性（一条「带图的话」
    // 同时进「文本」和「图片」），把它们摞起来既不等于发言条数，也不该拿来
    // 算合计与高峰；所以那种报表另查一条「发言条数」的走势当口径。
    let (series, unit_word, mine_points) = if data_type == "消息类型" {
        let series = message_type_series(db, group_id, user_id, &period).await?;
        let rows = queries::get_user_metric_trend(
            db,
            group_id,
            user_id,
            period.start,
            period.end,
            period.unit.granularity(),
            UserMetric::Messages,
        )
        .await
        .map_err(|e| e.to_string())?;
        let points = fill(&period.buckets, rows.into_iter().map(|r| (r.date, r.count)));
        (series, "条", points)
    } else {
        let metric = user_metric(data_type);
        let rows = queries::get_user_metric_trend(
            db,
            group_id,
            user_id,
            period.start,
            period.end,
            period.unit.granularity(),
            metric,
        )
        .await
        .map_err(|e| e.to_string())?;

        let points = fill(&period.buckets, rows.into_iter().map(|r| (r.date, r.count)));
        let mut series = vec![SeriesData {
            name: metric_name(metric).to_string(),
            color: FALLBACK_THEME,
            points: points.clone(),
            axis: Axis::Left,
        }];
        // 本群这一条当对比：同一段时间里全群说了多少。量级差得远，所以读右轴，
        // 不然个人的线会被压成贴着地面的一条直线。
        if let Some(gid) = group_id {
            let rows = queries::get_group_metric_trend(
                db,
                Some(gid),
                period.start,
                period.end,
                period.unit.granularity(),
                metric,
            )
            .await
            .map_err(|e| e.to_string())?;
            let points = fill(&period.buckets, rows.into_iter().map(|r| (r.date, r.count)));
            if points.iter().any(|p| p.value > 0) {
                series[0].name = "我".to_string();
                series.push(SeriesData {
                    name: "本群".to_string(),
                    color: crate::render::tokens::OUTLINE,
                    points,
                    axis: Axis::Right,
                });
            }
        }
        (series, metric_word(metric), points)
    };

    // 统计只从「我」那一份取：本群对比线读的是全群，混进来会把合计算成
    // 我加全群、把我没说话的格子也算成活跃、高峰还会落在全群的尖峰上。
    let per_bucket: Vec<i64> = mine_points.iter().map(|p| p.value).collect();
    let total: i64 = per_bucket.iter().sum();
    if total <= 0 {
        return Err(ChartError::NoData);
    }

    // 上一期：同样的长度、同样的口径。跨群时是「我在所有群」的合计，同样不含私聊。
    let prev_total = match period.prev {
        Some((s, e)) => {
            let metric = rank_metric(data_type);
            let value = match group_id {
                Some(gid) => {
                    queries::get_user_metric_total(db, Some(gid), user_id, s, e, metric).await
                }
                None => queries::get_user_metric_total_in_groups(db, user_id, s, e, metric).await,
            }
            .map_err(|e| e.to_string())?;
            Some(value)
        }
        None => None,
    };

    let buckets = &period.buckets;
    let active = per_bucket.iter().filter(|v| **v > 0).count();
    let peak = buckets
        .iter()
        .zip(per_bucket.iter())
        .max_by_key(|(_, v)| **v)
        .map(|(b, v)| (b, *v))
        .unwrap_or((&buckets[0], 0));

    // 跨群时这一格与副标题都要知道「跨了几个群、最常说话的是哪个」，查一次就够。
    let groups = if group_id.is_none() {
        let rows = queries::get_user_guild_participation_ranking(
            db,
            user_id,
            period.start,
            period.end,
            200,
            rank_metric(data_type),
        )
        .await
        .map_err(|e| e.to_string())?;
        Some(rows.into_iter().filter(|r| r.count > 0).collect::<Vec<_>>())
    } else {
        None
    };

    let scope_cell = if let Some(guild) = group_id {
        let (rank, people, sum) = queries::get_user_rank_in_guild(
            db,
            guild,
            user_id,
            period.start,
            period.end,
            rank_metric(data_type),
        )
        .await
        .map_err(|e| e.to_string())?;
        let note = if sum > 0 {
            format!("占全群 {:.0}%", total as f64 / sum as f64 * 100.0)
        } else {
            String::new()
        };
        StatCell {
            label: "本群排名".into(),
            value: format!("第 {rank} / {people}"),
            note,
        }
    } else {
        let groups = groups.as_deref().unwrap_or_default();
        let top = groups
            .iter()
            .max_by_key(|r| r.count)
            .map(|r| {
                if r.guild_name.is_empty() {
                    r.guild_id.clone()
                } else {
                    r.guild_name.clone()
                }
            })
            .unwrap_or_default();
        StatCell {
            label: "群覆盖".into(),
            value: format!("{} 个群", groups.len()),
            note: if top.is_empty() {
                String::new()
            } else {
                format!("最多：{top}")
            },
        }
    };

    let scope_text = if group_id.is_some() {
        "本群".to_string()
    } else {
        match groups.as_ref().map_or(0, Vec::len) {
            0 => "跨群".to_string(),
            n => format!("跨群 · {n} 个群"),
        }
    };

    let cells = vec![
        StatCell {
            label: "合计".into(),
            value: format!("{} {}", format_count(total), unit_word),
            note: delta_note(prev_total, total, period.prev_label),
        },
        StatCell {
            label: "活跃".into(),
            value: format!("{} / {} {}", active, buckets.len(), period.unit.span_word()),
            note: String::new(),
        },
        StatCell {
            label: "高峰".into(),
            value: short_label(&peak.0.label),
            note: format!("{} {}", format_count(peak.1), unit_word),
        },
        scope_cell,
    ];

    // 对比线（右轴）的合计：写进副标题，读者不必在两个刻度之间猜。
    let group_total: i64 = series
        .iter()
        .filter(|sr| sr.axis == Axis::Right)
        .flat_map(|sr| sr.points.iter().map(|p| p.value))
        .sum();
    let compare = if group_total > 0 {
        format!(" · 本群 {} {}", format_count(group_total), unit_word)
    } else {
        String::new()
    };

    let now_str = Local::now().format("%Y-%m-%d %H:%M").to_string();
    let meta = format!(
        "{} · 共 {} {} · 合计 {} {}{} · {}",
        scope_text,
        buckets.len(),
        period.unit.span_word(),
        format_count(total),
        unit_word,
        compare,
        now_str
    );

    Ok(MineReport {
        meta,
        cells,
        series,
    })
}

/// 指令里的「我的」用的是哪一段。与 chart::generate 共用，榜单和走势才不会一段看图一段看榜。
pub async fn range_for(
    db: &DatabaseConnection,
    user_id: &str,
    time_str: &str,
) -> Result<(i64, i64), ChartError> {
    let first_seen = if time_str == "总" {
        queries::get_user_first_message_time(db, None, user_id)
            .await
            .map_err(|e| e.to_string())?
    } else {
        None
    };
    let period = period(time_str, first_seen);
    Ok((period.start, period.end))
}

// ================= 区间与分桶 =================

fn start_of_day(dt: chrono::DateTime<Local>) -> chrono::DateTime<Local> {
    dt.date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(Local)
        .unwrap()
}

fn start_of_month(dt: chrono::DateTime<Local>) -> chrono::DateTime<Local> {
    let d = dt.date_naive();
    Local
        .with_ymd_and_hms(d.year(), d.month(), 1, 0, 0, 0)
        .unwrap()
}

fn at(y: i32, m: u32, d: u32) -> chrono::DateTime<Local> {
    Local.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
}

/// 自然语言时间词 → 区间。与 db::utils::get_time_range 的区别是这里按自然日对齐、
/// 并且自带「上一期」；「我的」这条线需要前者来补 0，需要后者来比较。
fn period(time_str: &str, first_seen: Option<i64>) -> Period {
    let now = Local::now();
    let today = start_of_day(now);
    let week_start = today - Duration::days(now.weekday().num_days_from_monday() as i64);
    let month_start = start_of_month(now);
    let prev_month = month_start.checked_sub_months(Months::new(1)).unwrap();
    let year_start = at(now.year(), 1, 1);
    let last_year = year_start.checked_sub_months(Months::new(12)).unwrap();
    let day = 86_400;

    let (start, end, prev, prev_label, unit) = match time_str {
        "今日" => (
            today.timestamp(),
            now.timestamp(),
            Some((today.timestamp() - day, now.timestamp() - day)),
            "较昨日同期",
            Unit::Hour,
        ),
        "昨日" => (
            today.timestamp() - day,
            today.timestamp(),
            Some((today.timestamp() - 2 * day, today.timestamp() - day)),
            "较前日",
            Unit::Hour,
        ),
        "本周" => (
            week_start.timestamp(),
            now.timestamp(),
            Some((week_start.timestamp() - 7 * day, now.timestamp() - 7 * day)),
            "较上周同期",
            Unit::Day,
        ),
        "上周" => {
            let from = week_start.timestamp() - 7 * day;
            (
                from,
                week_start.timestamp(),
                Some((from - 7 * day, from)),
                "较前一周",
                Unit::Day,
            )
        }
        "近7天" => {
            let from = today.timestamp() - 6 * day;
            (
                from,
                now.timestamp(),
                Some((from - 7 * day, now.timestamp() - 7 * day)),
                "较前 7 天",
                Unit::Day,
            )
        }
        "近30天" => {
            let from = today.timestamp() - 29 * day;
            (
                from,
                now.timestamp(),
                Some((from - 30 * day, now.timestamp() - 30 * day)),
                "较前 30 天",
                Unit::Day,
            )
        }
        "本月" => {
            // 上月比本月短时（3 月 31 日对 2 月），「同期」会溢出到本月，
            // 把已经算进合计的那几天再数一遍；截到月末为止。
            let elapsed = now.timestamp() - month_start.timestamp();
            let prev_end = (prev_month.timestamp() + elapsed).min(month_start.timestamp());
            (
                month_start.timestamp(),
                now.timestamp(),
                Some((prev_month.timestamp(), prev_end)),
                "较上月同期",
                Unit::Day,
            )
        }
        "上月" => {
            let prev_prev = prev_month.checked_sub_months(Months::new(1)).unwrap();
            (
                prev_month.timestamp(),
                month_start.timestamp(),
                Some((prev_prev.timestamp(), prev_month.timestamp())),
                "较前一月",
                Unit::Day,
            )
        }
        "今年" => {
            // 闰年多一天，溢出会落到今年 1 月；同「本月」，截到去年年末。
            let elapsed = now.timestamp() - year_start.timestamp();
            let prev_end = (last_year.timestamp() + elapsed).min(year_start.timestamp());
            (
                year_start.timestamp(),
                now.timestamp(),
                Some((last_year.timestamp(), prev_end)),
                "较去年同期",
                Unit::Month,
            )
        }
        "去年" => {
            let before = last_year.checked_sub_months(Months::new(12)).unwrap();
            (
                last_year.timestamp(),
                year_start.timestamp(),
                Some((before.timestamp(), last_year.timestamp())),
                "较前一年",
                Unit::Month,
            )
        }
        "总" => {
            // 起点是我的第一条消息（本群时是这个群里的第一条），不是库里第一行——
            // 别人的历史不该进我的横轴。按天画时就从那天开始，不必先空出大半个月；
            // 跨度太长才折成按月，横轴塞不下几百格。
            let first = first_seen.and_then(|t| Local.timestamp_opt(t, 0).single());
            let first_day = first.map(start_of_day).unwrap_or(today);
            let days = (now.timestamp() - first_day.timestamp()) as f64 / day as f64;
            let (from, unit) = if days > 120.0 {
                (first.map(start_of_month).unwrap_or(month_start), Unit::Month)
            } else {
                (first_day, Unit::Day)
            };
            (from.timestamp(), now.timestamp(), None, "", unit)
        }
        _ => (
            today.timestamp(),
            now.timestamp(),
            Some((today.timestamp() - day, now.timestamp() - day)),
            "较昨日同期",
            Unit::Hour,
        ),
    };

    let buckets = buckets(unit, start, end);
    Period {
        start,
        end,
        prev,
        prev_label,
        unit,
        buckets,
    }
}

fn buckets(unit: Unit, start: i64, end: i64) -> Vec<Bucket> {
    let mut out = Vec::new();
    if start >= end {
        return out;
    }
    let from = Local.timestamp_opt(start, 0).unwrap();
    let mut cursor = match unit {
        Unit::Hour => from
            .date_naive()
            .and_hms_opt(from.hour(), 0, 0)
            .unwrap()
            .and_local_timezone(Local)
            .unwrap(),
        Unit::Day => start_of_day(from),
        Unit::Month => start_of_month(from),
    };
    let fmt = match unit {
        Unit::Hour => "%H:%M",
        Unit::Day => "%Y-%m-%d",
        Unit::Month => "%Y-%m",
    };
    // 上限只是护栏：按天的跨度已经在 period 里换成了按月，正常走不到这里。
    while cursor.timestamp() < end && out.len() < 400 {
        let next = match unit {
            Unit::Hour => cursor + Duration::hours(1),
            Unit::Day => cursor + Duration::days(1),
            Unit::Month => cursor.checked_add_months(Months::new(1)).unwrap(),
        };
        out.push(Bucket {
            label: cursor.format(fmt).to_string(),
        });
        cursor = next;
    }
    out
}

// ================= 取值与文案 =================

/// 把查询回来的（日期键 → 数）铺到每一格上。没说话的格补 0。
///
/// 用累加而不是直接落值：按月的报表拿的是按天的行，同一个月的几行要摞在一起；
/// 按小时/按天的报表里键本来就唯一，累加与落值结果一样。
fn fill(buckets: &[Bucket], rows: impl Iterator<Item = (String, i64)>) -> Vec<ChartDataPoint> {
    let mut map: HashMap<String, i64> = HashMap::new();
    for (key, value) in rows {
        *map.entry(key).or_insert(0) += value;
    }
    buckets
        .iter()
        .map(|b| ChartDataPoint {
            label: b.label.clone(),
            value: map.remove(&b.label).unwrap_or(0),
        })
        .collect()
}

/// 消息类型每天都查回来一行；按月的报表把这几行折成一个月。
fn month_key(date: &str) -> String {
    date.get(..7).unwrap_or(date).to_string()
}

async fn message_type_series(
    db: &DatabaseConnection,
    group_id: Option<&str>,
    user_id: &str,
    period: &Period,
) -> Result<Vec<SeriesData>, ChartError> {
    let rows = queries::get_message_type_trend(
        db,
        group_id,
        Some(user_id),
        period.start,
        period.end,
        period.unit == Unit::Hour,
    )
    .await
    .map_err(|e| e.to_string())?;

    type Get = fn(&MessageTypeTrend) -> i64;
    let types: [(&str, Get); 6] = [
        ("文本", |t| t.text),
        ("图片", |t| t.image),
        ("语音", |t| t.voice),
        ("视频", |t| t.video),
        ("动画表情", |t| t.anim_emoji),
        ("表情", |t| t.face),
    ];

    let mut list = Vec::new();
    for (name, get) in types {
        let points = fill(
            &period.buckets,
            rows.iter().map(|r| {
                let key = if period.unit == Unit::Month {
                    month_key(&r.date)
                } else {
                    r.date.clone()
                };
                (key, get(r))
            }),
        );
        if points.iter().any(|p| p.value > 0) {
            let (color, _) = message_type_style(name);
            list.push(SeriesData {
                name: name.to_string(),
                color,
                points,
                axis: Axis::Left,
            });
        }
    }
    if list.is_empty() {
        return Err(ChartError::NoData);
    }
    Ok(list)
}

fn metric_name(metric: UserMetric) -> &'static str {
    match metric {
        UserMetric::Messages => "发言",
        UserMetric::AnimEmoji => "表情包",
        UserMetric::TextMessages => "文本消息",
        UserMetric::TextChars => "发言字数",
    }
}

fn metric_word(metric: UserMetric) -> &'static str {
    match metric {
        UserMetric::TextChars => "字",
        _ => "条",
    }
}

/// 名次那一格按什么排。消息类型报表没有单一的数，退回按发言条数排。
fn rank_metric(data_type: &str) -> UserMetric {
    if data_type == "消息类型" {
        UserMetric::Messages
    } else {
        user_metric(data_type)
    }
}

fn format_count(value: i64) -> String {
    super::utils::format_thousands(value)
}

/// 对比上一期：同期持平就说持平，翻了几倍就写倍数——百分数到了四位数没人读得下去。
fn delta_note(prev: Option<i64>, total: i64, label: &str) -> String {
    let Some(prev) = prev else {
        return String::new();
    };
    if label.is_empty() {
        return String::new();
    }
    if prev == 0 {
        return if total == 0 {
            format!("{label} 持平")
        } else {
            format!("{label} 新增")
        };
    }
    let ratio = total as f64 / prev as f64;
    if ratio >= 10.0 {
        format!("{label} ×{ratio:.0}")
    } else if (ratio - 1.0).abs() < 0.005 {
        format!("{label} 持平")
    } else {
        format!("{label} {:+.0}%", (ratio - 1.0) * 100.0)
    }
}

/// 高峰那一格的横轴标签：按天的去掉年份，格子窄，年份在副标题里已经有了。
fn short_label(label: &str) -> String {
    if label.len() == 10 && label.as_bytes().get(4) == Some(&b'-') {
        label[5..].to_string()
    } else {
        label.to_string()
    }
}
