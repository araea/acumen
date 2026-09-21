use crate::adapters::satori::{LockedWriter, send_msg};
use crate::command::strip_prefix;
use crate::config::build_config;
use crate::db::utils::get_time_range;
use crate::event::Context;
use crate::message::Message;
use crate::plugins::{ChannelConfig, PluginError, get_config};
use crate::scheduler::{Pace, PushFrequency};
use chrono::Weekday;
use futures_util::future::BoxFuture;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use toml::Value;

mod chart;
mod pusher;

// ================= 配置定义 =================

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct StatsConfig {
    pub enabled: bool,
    /// 字体文件绝对路径。若提供且存在，优先于 `font_family` 使用。
    pub font_path: String,
    /// 字体族名，交给系统去找。
    pub font_family: String,
    /// 成图宽度（像素）。
    pub width: u32,
    /// 成图高度（像素）。
    pub height: u32,

    /// 排行榜的构图线与发言条谁盖谁。
    /// `true`（默认）：构图线画在最上层，从榜首通到榜尾，格子不被任何一根条打断；
    /// `false`：实色条盖住它，每根条都是完整的一块颜色。
    /// 只影响遮挡关系，线的位置与疏密两种都一样；文字始终在最上面，不会被线压到。
    pub ranking_grid_over_bars: bool,

    /// 排行榜的次数与占比写在哪儿。
    /// `true`（默认）：紧跟在自己那根条的尾巴后面。眼睛被条的颜色牵到条尾，答案就在
    /// 那里，中间不用换一次视线；代价是二十个数字排成一串阶梯。
    /// `false`：右对齐成固定的两列。上下扫一眼就能比大小、画面更齐整；代价是读完条
    /// 还得横着扫到画面最右边，再回头认这是哪一行。
    pub ranking_value_follows_bar: bool,

    /// 群名单：配了黑名单就对名单外的所有群生效并推送，配了白名单则只对名单内的群
    /// 生效并推送。查询指令与主动推送共用这份名单，不会出现「能查不能推」的错位。
    pub channel: ChannelConfig,

    // —— 主动推送总开关与阈值 ——
    /// 群在统计区间内消息数低于此值则跳过推送（避免打扰冷群）
    pub push_min_messages: u64,

    // —— 多群推送节奏 ——
    /// 群与群之间的最小等待秒数
    pub push_group_gap_min_seconds: u64,
    /// 群与群之间的最大等待秒数；实际间隔在 min—max 之间随机取值，
    /// 避免所有群在同一时刻收到推送，也让节奏不那么"机器"
    pub push_group_gap_max_seconds: u64,

    // —— 每日 23:30 当日总结 ——
    /// 是否推送当日总结。
    pub daily_push_enabled: bool,
    /// 推送时间（HH:MM:SS，北京时间）。
    pub daily_push_time: String,

    // —— 每日 09:00 早安回顾（昨日数据） ——
    /// 是否推送早安回顾。
    pub morning_recap_enabled: bool,
    /// 推送时间（HH:MM:SS，北京时间）。
    pub morning_recap_time: String,

    // —— 每日 12:30 午间速览（今日上午） ——
    /// 是否推送午间速览。
    pub noon_brief_enabled: bool,
    /// 推送时间（HH:MM:SS，北京时间）。
    pub noon_brief_time: String,

    // —— 每周一 10:00 上周回顾 ——
    /// 是否推送上周回顾。
    pub weekly_recap_enabled: bool,
    /// 推送时间（HH:MM:SS，北京时间）。
    pub weekly_recap_time: String,

    // —— 每月 1 日 10:20 上月回顾（与周一 10:00 的周报错开，1 号恰逢周一时不会挤在一起）——
    /// 是否推送上月回顾。
    pub monthly_recap_enabled: bool,
    /// 推送时间（HH:MM:SS，北京时间）。
    pub monthly_recap_time: String,
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            font_path: String::new(),
            font_family: "Noto Sans CJK SC".to_string(),
            width: 960,
            height: 800,
            ranking_grid_over_bars: true,
            ranking_value_follows_bar: true,
            channel: ChannelConfig::default(),
            push_min_messages: 20,
            push_group_gap_min_seconds: 20,
            push_group_gap_max_seconds: 75,
            daily_push_enabled: true,
            daily_push_time: "23:30:00".to_string(),
            morning_recap_enabled: true,
            morning_recap_time: "09:00:00".to_string(),
            noon_brief_enabled: true,
            noon_brief_time: "12:30:00".to_string(),
            weekly_recap_enabled: true,
            weekly_recap_time: "10:00:00".to_string(),
            monthly_recap_enabled: true,
            monthly_recap_time: "10:20:00".to_string(),
        }
    }
}

pub fn default_config() -> Value {
    build_config(StatsConfig::default())
}

// ================= 正则匹配 =================

static REGEX_GLOBAL: OnceLock<Regex> = OnceLock::new();
static REGEX_NORMAL: OnceLock<Regex> = OnceLock::new();

fn get_regex_global() -> &'static Regex {
    REGEX_GLOBAL.get_or_init(|| {
        Regex::new(
            r"^所有群(今日|昨日|本周|上周|近7天|近30天|本月|上月|今年|去年|总)发言(排行榜|走势)$",
        )
        .unwrap()
    })
}

fn get_regex_normal() -> &'static Regex {
    REGEX_NORMAL.get_or_init(|| {
        Regex::new(r"^(?:(本群|跨群|我的))?(今日|昨日|本周|上周|近7天|近30天|本月|上月|今年|去年|总)(发言|表情包|消息类型)(排行榜|走势)$")
            .unwrap()
    })
}

// ================= 插件入口 =================

pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let msg = match ctx.as_message() {
            Some(m) => m,
            None => return Ok(Some(ctx)),
        };
        let content = match strip_prefix(&ctx, msg.text()) {
            Some(c) => c,
            None => return Ok(Some(ctx)),
        };

        // 群名单外的群不响应查询，与主动推送保持同一套生效范围
        let config: StatsConfig = get_config(&ctx, "stats").unwrap_or_default();
        if !config.channel.allows(msg.group_id()) {
            return Ok(Some(ctx));
        }

        let (scope, time_str, data_type, chart_type, is_all_groups) =
            if let Some(caps) = get_regex_global().captures(content) {
                let t = caps.get(1).map_or("", |m| m.as_str());
                let c_type = caps.get(2).map_or("", |m| m.as_str());
                ("跨群", t, "发言", c_type, true)
            } else if let Some(caps) = get_regex_normal().captures(content) {
                let s = caps.get(1).map_or("本群", |m| m.as_str());
                let t = caps.get(2).map_or("", |m| m.as_str());
                let d = caps.get(3).map_or("", |m| m.as_str());
                let c = caps.get(4).map_or("", |m| m.as_str());
                let final_scope = if s.is_empty() { "本群" } else { s };
                (final_scope, t, d, c, false)
            } else {
                return Ok(Some(ctx));
            };

        let group_id = msg.group_id();
        let user_id = msg.user_id();

        if scope == "本群" && group_id.is_none() {
            send_msg(
                &ctx,
                writer,
                None,
                Some(user_id),
                "❌ 这个范围只在群里有效\n用「本群」查群里的统计，或用「我的」查个人的",
            )
            .await?;
            return Ok(None);
        }

        info!(
            target: "Plugin/Stats",
            "Req: Scope={}, Time={}, Data={}, Chart={}, Global={}",
            scope, time_str, data_type, chart_type, is_all_groups
        );

        let (start_time, end_time) = get_time_range(time_str);

        let (query_group, query_user) = match scope {
            "本群" => (group_id, None),
            "跨群" => (None, None),
            "我的" => (None, Some(user_id)),
            _ => (None, None),
        };

        // 中文标题不靠空格断词：连写成一句「本群今日发言排行榜」像一行标题，
        // 用空格隔开则像四个并排的关键词。范围、总量、时间都在图里的元信息行上。
        let title = if is_all_groups {
            format!("所有群{}{}{}", time_str, data_type, chart_type)
        } else {
            format!("{}{}{}{}", scope, time_str, data_type, chart_type)
        };

        let result_img = chart::generate(
            &ctx,
            is_all_groups,
            data_type,
            chart_type,
            query_group,
            query_user,
            user_id,
            start_time,
            end_time,
            &title,
        )
        .await;

        match result_img {
            Ok(b64) => {
                let reply = Message::new().image(b64);
                send_msg(&ctx, writer, group_id, Some(user_id), reply).await?;
            }
            Err(chart::ChartError::NoData) => {
                // 空态不是错误：说清为什么空，再给一条能立刻做的事。
                send_msg(
                    &ctx,
                    writer,
                    group_id,
                    Some(user_id),
                    format!("📭 {title}没有数据\n换一个时间范围，或先让群里聊几句"),
                )
                .await?;
            }
            Err(chart::ChartError::Failed(e)) => {
                send_msg(
                    &ctx,
                    writer,
                    group_id,
                    Some(user_id),
                    format!("❌ 生成失败：{}", e),
                )
                .await?;
            }
        }

        Ok(None)
    })
}

pub fn on_connected(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let config: StatsConfig = get_config(&ctx, "stats").unwrap_or_default();

        let scheduler = ctx.scheduler.clone();

        let pace = Pace::new(
            config.push_group_gap_min_seconds,
            config.push_group_gap_max_seconds,
        );

        // 注册一系列分时段的主动推送任务
        // 每项可独立开关；设计原则：错峰、不打扰冷群、单条推送内按"引言→数字→主榜→走势→副榜→词云"展开
        //
        // 排期与 ai_news 的资讯推送整体错开（见 `plugins::ai_news` 模块文档的时间表），
        // 同一时刻不会有两个插件同时往群里刷图。
        let registrations: [(bool, &str, String, PushFrequency, PushFn); 5] = [
            (
                config.morning_recap_enabled,
                "MorningRecap",
                config.morning_recap_time.clone(),
                PushFrequency::Daily,
                |c, w, gid, m| Box::pin(pusher::push_morning_recap(c, w, gid, m)),
            ),
            (
                config.noon_brief_enabled,
                "NoonBrief",
                config.noon_brief_time.clone(),
                PushFrequency::Daily,
                |c, w, gid, m| Box::pin(pusher::push_noon_brief(c, w, gid, m)),
            ),
            (
                config.daily_push_enabled,
                "DailySummary",
                config.daily_push_time.clone(),
                PushFrequency::Daily,
                |c, w, gid, m| Box::pin(pusher::push_daily_summary(c, w, gid, m)),
            ),
            (
                config.weekly_recap_enabled,
                "WeeklyRecap",
                config.weekly_recap_time.clone(),
                PushFrequency::Weekly(Weekday::Mon),
                |c, w, gid, m| Box::pin(pusher::push_weekly_recap(c, w, gid, m)),
            ),
            (
                config.monthly_recap_enabled,
                "MonthlyRecap",
                config.monthly_recap_time.clone(),
                PushFrequency::Monthly(1),
                |c, w, gid, m| Box::pin(pusher::push_monthly_recap(c, w, gid, m)),
            ),
        ];

        for (index, (_enabled, label, time_str, freq, runner)) in
            registrations.into_iter().enumerate()
        {
            scheduler.schedule_periodic_push(
                ctx.clone(),
                writer.clone(),
                "Stats",
                label,
                time_str,
                freq,
                pace,
                move |c, w, gid| async move {
                    let current =
                        crate::plugins::get_config::<StatsConfig>(&c, "stats").unwrap_or_default();
                    let switches = [
                        current.morning_recap_enabled,
                        current.noon_brief_enabled,
                        current.daily_push_enabled,
                        current.weekly_recap_enabled,
                        current.monthly_recap_enabled,
                    ];
                    if current.enabled && switches[index] && current.channel.allows_group(gid) {
                        runner(c, w, gid, current.push_min_messages).await;
                    }
                },
            );
        }

        Ok(Some(ctx))
    })
}

type PushFn = fn(Context, LockedWriter, i64, u64) -> futures_util::future::BoxFuture<'static, ()>;

/// Validate control edits against the plugin's actual configuration type.
pub fn validate_config(value: &toml::Value) -> Result<(), String> {
    <StatsConfig as serde::Deserialize>::deserialize(value.clone())
        .map(|_| ())
        .map_err(|_| "配置类型不匹配（请检查数组元素、字段类型及整数范围）".to_string())
}
