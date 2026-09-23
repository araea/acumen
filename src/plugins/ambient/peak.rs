//! 按 DeepSeek 的计价时段决定这会儿值不值得开口。
//!
//! DeepSeek 官方接口把「北京时间周一至周五 9:00–12:00、14:00–18:00」定为高峰，
//! 其余时间——午休、傍晚、整夜、整个周末——都是空闲时段，价格是高峰的一半。
//!
//! 搭话是这个 bot 里唯一一个无人触发、跟着群消息频率自动跑的付费功能，所以它
//! 也是最值得挑时段的那个：把它压回空闲时段，账单直接对折，而群里最热闹的晚上
//! 本来就落在空闲时段里，几乎不损失什么。高峰时段有两种省法——彻底不出声，
//! 或者睡着：不跟着消息频率一直判定，只隔一段时间看一眼；被点名或搭话指令
//! 则立刻醒一次。两种都换上最省的一份上下文。
//!
//! 睡着时的自主接话由两条本地闸门控住成本：`doze_gate_seconds` 限制主动判定的
//! 频率（判定是最频繁的那次调用），`doze_reply_limit` 给真正花钱的开口一个每小时
//! 硬上限。两条都只读内存里的时间戳，不产生费用。
//!
//! 时段写在配置里而不是写死：定价规则会变，改一行配置比改一次编译便宜。
//!
//! 峰谷价是 DeepSeek 一家的事，所以这一层只对走 DeepSeek 接口的模型生效
//! （见 [`billed_by_peak`]）：换成全天一个价的供应商，时段管理直接让路，
//! 配置留着不改，换回 DeepSeek 那天立刻又是原来那套作息。

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// DeepSeek 在 `[oai.providers]` 里的供应商名。
const DEEPSEEK: &str = "deepseek";

/// 这个模型走的是不是 DeepSeek 官方那条分峰谷价的接口。
///
/// 按供应商前缀认，不认别名：配置里写成 `另一名字/…` 指的是另一个接口，
/// 那家怎么计价代码不知道，宁可当它全天一个价。
pub(crate) fn billed_by_peak(model: &str) -> bool {
    crate::plugins::oai::utils::split_provider(model)
        .0
        .is_some_and(|provider| provider.eq_ignore_ascii_case(DEEPSEEK))
}

/// 高峰时段的行为。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Mode {
    /// 不理会时段，照常。
    Normal,
    /// 睡着：不跟着消息频率一直判定，只按 `doze_gate_seconds` 偶尔看一眼；
    /// 被 @ / 引用 / 戳一戳时立刻醒一次，且用最省的上下文。
    Sleep,
    /// 停用：高峰时段一句话都不说。
    Pause,
}

/// 这会儿该以什么姿态待着。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stance {
    /// 空闲时段（或关掉了时段管理）：照常。
    Awake,
    /// 高峰时段，睡着：偶尔主动接一句，被点名时立刻回应。
    Dozing,
    /// 高峰时段，停用。
    Asleep,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PeakConfig {
    /// 高峰时段怎么办。
    pub mode: Mode,
    /// 高峰时段，`HH:MM-HH:MM`，按北京时间，可跨零点；空列表等于没有高峰。
    pub windows: Vec<String>,
    /// 算作高峰的星期几，1=周一 … 7=周日；空列表等于每天都算。
    pub weekdays: Vec<u8>,
    /// 睡着时两次主动判定之间的最短间隔（秒）。高峰价格翻倍，但群里该接的话
    /// 隔一会儿看一眼仍接得住；这个间隔把「跟着消息频率一直在判定」压成
    /// 「隔一段时间看一眼」。写 0 表示完全睡着，只有被点名才醒（旧行为）；
    /// 其余夹到 30 秒起步，免得写成 1 秒又把账单拉回高峰水平。
    pub doze_gate_seconds: u64,
    /// 睡着时每小时最多主动开口几次，不含被点名与搭话指令。判定便宜、开口贵，
    /// 这条给高峰时段的自主发言一个硬上限。写 0 表示不额外限制。
    pub doze_reply_limit: usize,
}

impl Default for PeakConfig {
    fn default() -> Self {
        Self {
            mode: Mode::Sleep,
            windows: vec!["09:00-12:00".to_string(), "14:00-18:00".to_string()],
            weekdays: vec![1, 2, 3, 4, 5],
            doze_gate_seconds: 0,
            doze_reply_limit: 2,
        }
    }
}

/// `HH:MM-HH:MM` → 一天中的起止分钟数。写坏的时段当作不存在，不去猜它想表达什么。
fn parse_window(window: &str) -> Option<(u32, u32)> {
    let (start, end) = window.split_once('-')?;
    Some((parse_clock(start)?, parse_clock(end)?))
}

fn parse_clock(clock: &str) -> Option<u32> {
    let (hour, minute) = clock.trim().split_once(':')?;
    let hour: u32 = hour.trim().parse().ok()?;
    let minute: u32 = minute.trim().parse().ok()?;
    (hour < 24 && minute < 60).then_some(hour * 60 + minute)
}

impl PeakConfig {
    /// 这个时刻算不算高峰。跨零点的时段按当时那一刻的星期几判断。
    pub(crate) fn is_peak_at<Tz: chrono::TimeZone>(&self, at: chrono::DateTime<Tz>) -> bool {
        use chrono::{Datelike as _, Timelike as _};
        let at = at.with_timezone(&chrono::FixedOffset::east_opt(8 * 3_600).expect("北京时间"));
        let weekday = at.weekday().number_from_monday() as u8;
        if !self.weekdays.is_empty() && !self.weekdays.contains(&weekday) {
            return false;
        }
        let minutes = at.hour() * 60 + at.minute();
        self.windows
            .iter()
            .filter_map(|window| parse_window(window))
            .any(|(start, end)| {
                if start <= end {
                    minutes >= start && minutes < end
                } else {
                    minutes >= start || minutes < end
                }
            })
    }

    pub(crate) fn stance_at<Tz: chrono::TimeZone>(&self, at: chrono::DateTime<Tz>) -> Stance {
        if self.mode == Mode::Normal || !self.is_peak_at(at) {
            return Stance::Awake;
        }
        match self.mode {
            Mode::Pause => Stance::Asleep,
            _ => Stance::Dozing,
        }
    }

    pub(crate) fn stance(&self) -> Stance {
        self.stance_at(chrono::Local::now())
    }

    /// 睡着时两次主动判定之间要隔多久。写 0 关闭自主判定；其余夹到 30 秒起步，
    /// 免得配置写成 1 秒又把判定频率拉回空闲时段的样子。
    pub(crate) fn doze_gate(&self) -> Duration {
        if self.doze_gate_seconds == 0 {
            Duration::ZERO
        } else {
            Duration::from_secs(self.doze_gate_seconds.clamp(30, 3_600))
        }
    }

    /// 这一小时还能不能再自主开一次口。
    pub(crate) fn doze_allows_reply(&self, spoken_last_hour: usize) -> bool {
        self.doze_reply_limit == 0 || spoken_last_hour < self.doze_reply_limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    /// 2026-09-10 是周四；给一个北京时间。
    fn thursday(hour: u32, minute: u32) -> chrono::DateTime<chrono::FixedOffset> {
        chrono::FixedOffset::east_opt(8 * 3_600)
            .unwrap()
            .with_ymd_and_hms(2026, 9, 10, hour, minute, 0)
            .single()
            .expect("北京时间里这个时刻存在")
    }

    fn sunday(hour: u32) -> chrono::DateTime<chrono::FixedOffset> {
        chrono::FixedOffset::east_opt(8 * 3_600)
            .unwrap()
            .with_ymd_and_hms(2026, 9, 13, hour, 0, 0)
            .single()
            .expect("北京时间里这个时刻存在")
    }

    #[test]
    fn deepseek_peak_hours_cover_the_working_day_and_nothing_else() {
        let config = PeakConfig::default();
        // 高峰：工作日上午与下午的两段。
        assert!(config.is_peak_at(thursday(9, 0)));
        assert!(config.is_peak_at(thursday(11, 59)));
        assert!(config.is_peak_at(thursday(17, 59)));
        // 空闲：午休、傍晚、深夜、整个周末。
        assert!(!config.is_peak_at(thursday(8, 59)));
        assert!(!config.is_peak_at(thursday(12, 0)));
        assert!(!config.is_peak_at(thursday(18, 0)));
        assert!(!config.is_peak_at(thursday(2, 0)));
        assert!(!config.is_peak_at(sunday(10)));
        assert!(config.is_peak_at(thursday(9, 0).with_timezone(&chrono::Utc)));
        assert!(!config.is_peak_at(thursday(18, 0).with_timezone(&chrono::Utc)));
    }

    #[test]
    fn the_mode_decides_what_peak_hours_mean() {
        let mut config = PeakConfig::default();
        assert_eq!(config.stance_at(thursday(10, 0)), Stance::Dozing);
        assert_eq!(config.stance_at(thursday(20, 0)), Stance::Awake);
        config.mode = Mode::Pause;
        assert_eq!(config.stance_at(thursday(10, 0)), Stance::Asleep);
        assert_eq!(config.stance_at(sunday(10)), Stance::Awake);
        // 关掉时段管理之后，高峰时段也照常。
        config.mode = Mode::Normal;
        assert_eq!(config.stance_at(thursday(10, 0)), Stance::Awake);
    }

    #[test]
    fn windows_are_configurable_and_a_broken_one_is_ignored_rather_than_guessed() {
        let config = PeakConfig {
            mode: Mode::Pause,
            // 跨零点的时段；另一条写坏了。
            windows: vec!["23:00-02:00".to_string(), "上午".to_string()],
            weekdays: vec![],
            ..PeakConfig::default()
        };
        assert!(config.is_peak_at(thursday(23, 30)));
        assert!(config.is_peak_at(thursday(1, 0)));
        assert!(!config.is_peak_at(thursday(3, 0)));
        // weekdays 留空等于每天都算。
        assert!(config.is_peak_at(sunday(23)));
        // 没有时段就没有高峰。
        let none = PeakConfig {
            windows: vec![],
            ..PeakConfig::default()
        };
        assert!(!none.is_peak_at(thursday(10, 0)));
    }

    #[test]
    fn config_round_trips_through_toml_and_old_files_get_the_defaults() {
        let config: PeakConfig = toml::from_str("").unwrap();
        assert_eq!(config.mode, Mode::Sleep);
        let text = toml::to_string(&PeakConfig {
            mode: Mode::Pause,
            ..PeakConfig::default()
        })
        .unwrap();
        assert!(text.contains("\"pause\""), "{text}");
        let back: PeakConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.mode, Mode::Pause);
        assert_eq!(back.windows, PeakConfig::default().windows);
    }

    #[test]
    fn only_deepseeks_own_models_carry_peak_pricing() {
        assert!(billed_by_peak("deepseek/deepseek-flash"));
        assert!(billed_by_peak("DeepSeek/deepseek-flash"));
        // 别家全天一个价，挑时段没意义。
        assert!(!billed_by_peak("mimo/mimo-v2.6-flash"));
        assert!(!billed_by_peak("apilio/gemini-3.8-flash"));
        // 不带前缀的走 oai 默认接口，那不是 DeepSeek 官方那条按峰谷计价的线。
        assert!(!billed_by_peak("deepseek-flash"));
        assert!(!billed_by_peak("deepseek/"));
    }

    #[test]
    fn dozing_keeps_a_floor_between_voluntary_judgements() {
        let config = PeakConfig::default();
        assert_eq!(config.doze_gate(), Duration::ZERO);
        assert!(config.doze_allows_reply(0));
        assert!(config.doze_allows_reply(config.doze_reply_limit - 1));
        assert!(!config.doze_allows_reply(config.doze_reply_limit));
        let occasional = PeakConfig {
            doze_gate_seconds: 300,
            ..PeakConfig::default()
        };
        assert_eq!(occasional.doze_gate(), Duration::from_secs(300));
        // 写成 1 秒会被夹回下限，不会把账单拉回高峰水平。
        let eager = PeakConfig {
            doze_gate_seconds: 1,
            ..PeakConfig::default()
        };
        assert_eq!(eager.doze_gate(), Duration::from_secs(30));
        let huge = PeakConfig {
            doze_gate_seconds: u64::MAX,
            ..PeakConfig::default()
        };
        assert_eq!(huge.doze_gate(), Duration::from_secs(3_600));
        // 每小时上限写 0 等于不额外限制。
        let unlimited = PeakConfig {
            doze_reply_limit: 0,
            ..PeakConfig::default()
        };
        assert!(unlimited.doze_allows_reply(usize::MAX));
        // 旧配置里没有这两个键也读得出来，取默认值。
        let legacy: PeakConfig = toml::from_str("mode = 'sleep'").unwrap();
        assert_eq!(legacy.doze_gate(), config.doze_gate());
        assert_eq!(legacy.doze_reply_limit, config.doze_reply_limit);
    }
}
