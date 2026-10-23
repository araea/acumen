//! 时间口径：这个项目里的「几点」一律是北京时间。

use chrono::{DateTime, FixedOffset, NaiveTime, Utc};

/// 北京时间（UTC+8）。
///
/// 卡片上印的时刻、推送与计价高峰的作息都按北京时间，不按机器时区：这台机器跟着群友在
/// 同一个时区，但时区配置本身可能被改（旅行、误设、容器里没有 /etc/localtime），而
/// 『几点』要和群聊里的对话对得上——对不上就是错的，没有第二种解释。
///
/// 从前几张卡与几个插件各写了一份同样的实现，于是各算各的。现在收在一处：要改时间口径
/// 就改这里。
pub fn beijing() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).expect("UTC+8 是合法时区偏移")
}

/// 此刻的北京时间。
pub fn beijing_now() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&beijing())
}

/// 提示里常用的几样「此刻」：问候、时刻、日期、星期。入群欢迎与自动回复的占位符共用这一份。
pub struct Moment {
    /// 早上好、上午好、中午好、下午好、晚上好、夜深了
    pub greeting: &'static str,
    /// 21:05
    pub time: String,
    /// 10 月 22 日
    pub date: String,
    /// 星期三
    pub weekday: String,
}

pub fn moment() -> Moment {
    use chrono::{Datelike, Timelike};
    let now = beijing_now();
    let greeting = match now.hour() {
        5..=8 => "早上好",
        9..=10 => "上午好",
        11..=12 => "中午好",
        13..=17 => "下午好",
        18..=22 => "晚上好",
        _ => "夜深了",
    };
    let weekday =
        ["一", "二", "三", "四", "五", "六", "日"][now.weekday().num_days_from_monday() as usize];
    Moment {
        greeting,
        time: format!("{:02}:{:02}", now.hour(), now.minute()),
        date: format!("{} 月 {} 日", now.month(), now.day()),
        weekday: format!("星期{weekday}"),
    }
}

/// 时长说成人话：整天、整小时、整分钟各用自己的单位，零碎的才写「分 秒」。
pub fn duration_text(seconds: u64) -> String {
    match seconds {
        s if s >= 86400 && s % 86400 == 0 => format!("{} 天", s / 86400),
        s if s >= 3600 && s % 3600 == 0 => format!("{} 小时", s / 3600),
        s if s >= 60 && s % 60 == 0 => format!("{} 分钟", s / 60),
        s if s >= 60 => format!("{} 分 {} 秒", s / 60, s % 60),
        s => format!("{s} 秒"),
    }
}

/// 配置里的钟点：`HH:MM` 或 `HH:MM:SS`（时、分、秒各一到两位）。读不懂（含越界）返回 `None`，
/// 由调用方决定是退回默认值、还是当作「没设」。
pub fn parse_clock(raw: &str) -> Option<NaiveTime> {
    let parts: Vec<&str> = raw.trim().split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let number = |part: &str| part.trim().parse::<u32>().ok();
    let seconds = parts.get(2).map_or(Some(0), |part| number(part))?;
    NaiveTime::from_hms_opt(number(parts[0])?, number(parts[1])?, seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn clock_accepts_hours_minutes_and_optional_seconds() {
        let at = |raw| parse_clock(raw).map(|time| (time.hour(), time.minute(), time.second()));
        assert_eq!(at("09:30"), Some((9, 30, 0)));
        assert_eq!(at(" 9:05:07 "), Some((9, 5, 7)));
        assert_eq!(at("23:59:59"), Some((23, 59, 59)));
    }

    #[test]
    fn clock_rejects_anything_else() {
        for bad in [
            "", "9", "24:00", "12:60", "12:00:60", "ab:cd", "1:2:3:4", "12:",
        ] {
            assert_eq!(parse_clock(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn beijing_is_utc_plus_eight() {
        assert_eq!(beijing().local_minus_utc(), 8 * 3600);
    }
}
