use crate::adapters::satori::{LockedWriter, send_msg};
use crate::command::strip_prefix;
use crate::db::queries;
use crate::db::utils::get_time_range;
use crate::event::Context;
use crate::message::Message;
use crate::plugins::{ChannelConfig, PluginConfig, PluginError, get_config_or_default};
use crate::scheduler::{Pace, PushFrequency};
use chrono::Weekday;
use futures_util::future::BoxFuture;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

mod chart;
mod pusher;

use self::chart::data_loader::user_metric;

const LOG_TARGET: &str = "Plugin/Stats";

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

    /// 排行榜默认列出多少名。指令后面跟一个数字可以临时改，如「本群今日发言排行榜 30」。
    pub ranking_limit: u32,
    /// 指令里那个数字的上限，超过按上限出图。一行约 120 像素高，100 名就是一万两千
    /// 像素的长图，再长手机上翻不动，出图也要多等一截。
    pub ranking_max_limit: u32,

    /// 群名单：配了黑名单就对名单外的所有群生效并推送，配了白名单则只对名单内的群
    /// 生效并推送。查询指令与主动推送共用这份名单，不会出现「能查不能推」的错位。
    pub channel: ChannelConfig,

    // —— 主动推送总开关与阈值 ——
    /// 群在统计区间内消息数低于此值则跳过推送（避免打扰冷群）
    pub push_min_messages: u64,
    /// 主动推送时是否先发一条文字提示（小结、数字、标题等），再发图。
    /// 默认 false：保持静默，只把结果图片发进群，不输出任何文字。
    pub push_text_enabled: bool,

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
            ranking_limit: 20,
            ranking_max_limit: 100,
            channel: ChannelConfig::default(),
            push_min_messages: 20,
            push_text_enabled: false,
            push_group_gap_min_seconds: 20,
            push_group_gap_max_seconds: 75,
            daily_push_enabled: false,
            daily_push_time: "23:30:00".to_string(),
            morning_recap_enabled: false,
            morning_recap_time: "09:00:00".to_string(),
            noon_brief_enabled: false,
            noon_brief_time: "12:30:00".to_string(),
            weekly_recap_enabled: false,
            weekly_recap_time: "10:00:00".to_string(),
            monthly_recap_enabled: false,
            monthly_recap_time: "10:20:00".to_string(),
        }
    }
}

impl PluginConfig for StatsConfig {
    const NAME: &'static str = "stats";
}

// ================= 正则匹配 =================

static REGEX_GLOBAL: OnceLock<Regex> = OnceLock::new();
static REGEX_NORMAL: OnceLock<Regex> = OnceLock::new();

/// 时间词统一成图里写的那几个说法：只认别名不改口径，后面的区间计算与标题都只用规范词。
fn canonical_time(raw: &str) -> Option<&'static str> {
    Some(match raw {
        "今日" | "今天" => "今日",
        "昨日" | "昨天" => "昨日",
        "本周" => "本周",
        "上周" => "上周",
        "近7天" | "近七日" | "近七天" | "近7日" => "近7天",
        "近30天" | "近三十日" | "近三十天" | "近30日" => "近30天",
        "本月" => "本月",
        "上月" => "上月",
        "今年" => "今年",
        "去年" => "去年",
        "总" => "总",
        _ => return None,
    })
}

fn get_regex_global() -> &'static Regex {
    REGEX_GLOBAL.get_or_init(|| {
        Regex::new(
            r"^所有群(?P<time>今日|昨天|昨日|今天|本周|上周|近7天|近七日|近七天|近7日|近30天|近三十日|近三十天|近30日|本月|上月|今年|去年|总)发言(?P<chart>排行榜|走势)(?:\s*(?P<count>[0-9０-９]+))?$",
        )
        .unwrap()
    })
}

/// 一般指令：可选的「本群我的 / 跨群我的 / 我的 / 本群 / 跨群」，然后是时间、数据类型、图型。
///
/// 「我的」写在最前，前面再挂本群/跨群表示范围：不写就是跨群（一个人的发言本来就散在
/// 好几个群里），写「本群」才只看当前这个群。
fn get_regex_normal() -> &'static Regex {
    REGEX_NORMAL.get_or_init(|| {
        Regex::new(r"^(?:(?<mine>本群我的|跨群我的|我的)|(?<plain>本群|跨群))?(?<time>今日|今天|昨日|昨天|本周|上周|近7天|近七日|近七天|近7日|近30天|近三十日|近三十天|近30日|本月|上月|今年|去年|总)(?<data>被艾特|艾特别人|艾特|发言字数|文本字数|文本消息|纯文本|文本|字数|发言|表情包|消息类型|管理员灰条禁言时长|管理员标称禁言时长|管理员单次最长禁言|管理员最长禁言|管理员累计禁言时长|管理员禁言总时长|管理员禁言时长|管理员禁言|灰条禁言时长|标称禁言时长|单次最长禁言|最长禁言|累计禁言时长|禁言总时长|禁言时长|禁言)(?<chart>排行榜|走势)(?:\s*(?P<count>[0-9０-９]+))?$")
            .unwrap()
    })
}

// ================= 范围 =================

/// 指令的四种范围。前两种是「大家的榜」，后两种是「我的」——后者默认跨群：
/// 一个人的发言本来就散在好几个群里，只看当前群才需要在前面加「本群」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Group,
    Cross,
    MineGroup,
    MineCross,
}

impl Scope {
    fn is_mine(self) -> bool {
        matches!(self, Self::MineGroup | Self::MineCross)
    }

    /// 需要当前群号的范围（「我的」也可以限定在本群）。
    fn in_group(self) -> bool {
        matches!(self, Self::Group | Self::MineGroup)
    }

    /// 写进标题的那一段。
    fn label(self) -> &'static str {
        match self {
            Self::Group => "本群",
            Self::Cross => "跨群",
            Self::MineGroup => "本群我的",
            Self::MineCross => "我的",
        }
    }
}

/// 拆好的一条指令。
struct Request<'a> {
    scope: Scope,
    /// 规范化的时间词（近七日 → 近7天），后面所有区间计算与标题都只用它。
    time: &'static str,
    data_type: &'a str,
    chart_type: &'a str,
    /// 「所有群」那条窄规则：只认发言，范围是全部群。
    all_groups: bool,
    count: Option<&'a str>,
}

/// 一条指令拆成六个部分；认不出返回 None。
///
/// 范围写在最前：「我的」默认跨群，前面加「本群」才只看当前群；不带「我的」时只有
/// 本群 / 跨群两种。所有群走另一条更窄的规则（只认发言）。
fn parse_command(content: &str) -> Option<Request<'_>> {
    if let Some(caps) = get_regex_global().captures(content) {
        return Some(Request {
            scope: Scope::Cross,
            time: canonical_time(caps.name("time").map_or("", |m| m.as_str()))?,
            data_type: "发言",
            chart_type: caps.name("chart").map_or("", |m| m.as_str()),
            all_groups: true,
            count: caps.name("count").map(|m| m.as_str()),
        });
    }

    let caps = get_regex_normal().captures(content)?;
    let scope = match caps.name("mine").map_or("", |m| m.as_str()) {
        "本群我的" => Scope::MineGroup,
        "跨群我的" | "我的" => Scope::MineCross,
        _ => match caps.name("plain").map_or("", |m| m.as_str()) {
            "跨群" => Scope::Cross,
            _ => Scope::Group,
        },
    };
    Some(Request {
        scope,
        time: canonical_time(caps.name("time").map_or("", |m| m.as_str()))?,
        data_type: caps.name("data").map_or("", |m| m.as_str()),
        chart_type: caps.name("chart").map_or("", |m| m.as_str()),
        all_groups: false,
        count: caps.name("count").map(|m| m.as_str()),
    })
}

// ================= 插件入口 =================

pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let Some(msg) = ctx.as_message() else {
            return Ok(Some(ctx));
        };
        let Some(content) = strip_prefix(&ctx, msg.text()) else {
            return Ok(Some(ctx));
        };

        // 群名单外的群不响应查询，与主动推送保持同一套生效范围
        let config: StatsConfig = get_config_or_default(&ctx);
        if !config.channel.allows(msg.group_id()) {
            return Ok(Some(ctx));
        }

        let Some(req) = parse_command(content) else {
            return Ok(Some(ctx));
        };
        let Request {
            scope,
            time: time_str,
            data_type,
            chart_type,
            all_groups: is_all_groups,
            count,
        } = req;

        // 名次数只对排行榜有意义；「走势 5」「排行榜 0」不是这个插件的指令，静默放过
        let limit = match count {
            None => ranking_limit(&config, None),
            Some(_) if chart_type != "排行榜" => return Ok(Some(ctx)),
            Some(raw) => match parse_count(raw) {
                Some(n) => ranking_limit(&config, Some(n)),
                None => return Ok(Some(ctx)),
            },
        };

        let group_id = msg.group_id();
        let user_id = msg.user_id();

        // 艾特只有排行榜：一天一天的 @ 次数折线还没人问过，真有人问再画。
        if chart::is_mention_ranking(data_type) && chart_type != "排行榜" {
            send_msg(
                &ctx,
                writer,
                group_id,
                Some(user_id),
                "❌ 被艾特、艾特别人只有排行榜
想看谁被 @ 得多，用「本群本月被艾特排行榜」
想看谁最爱 @ 人，用「本群本月艾特别人排行榜」
想看我自己在各群的情况，用「我的本月被艾特排行榜」",
            )
            .await?;
            return Ok(None);
        }

        // 本群的艾特榜本来就是按人排的，再加「我的」只会让人以为是同一个人一行。
        // 「我的」在艾特榜上的意义是「按群合计」，那只在跨群时成立。
        if chart::is_mention_ranking(data_type) && scope == Scope::MineGroup {
            send_msg(
                &ctx,
                writer,
                group_id,
                Some(user_id),
                "❌ 本群的艾特榜直接按人排
想看本群谁被 @ 得多，用「本群本月被艾特排行榜」
想看我在各个群被 @ 的情况，用「我的本月被艾特排行榜」",
            )
            .await?;
            return Ok(None);
        }

        // 禁言只有「本群」的排行榜：禁言是某个群里的事，跨群合计或个人名下都没有意思；
        // 走势没有（一个群一个月也没几次）。说清楚能用什么，别让人对着沉默猜。
        if chart::is_ban_ranking(data_type) && (scope != Scope::Group || chart_type != "排行榜")
        {
            send_msg(
                &ctx,
                writer,
                group_id,
                Some(user_id),
                "❌ 禁言只有本群的排行榜
试试：
「本群本月禁言排行榜」谁被禁言次数最多
「本群本月禁言时长排行榜」谁被禁言累计最久（实际禁言了多久）
「本群本月单次最长禁言排行榜」谁被禁得最久的一次
「本群本月灰条禁言时长排行榜」灰条上写的时长相加
想看动手禁言的人，把「禁言」换成「管理员禁言」，如「本群本月管理员禁言排行榜」",
            )
            .await?;
            return Ok(None);
        }

        // 文字类榜只有排行榜；「我的」是例外——一个人的字数/条数按天画出来是有意义的，
        // 全群按天画多个人的字数则没有现成的口径，所以只放开「我的」。
        if chart::is_text_ranking(data_type) && chart_type != "排行榜" && !scope.is_mine() {
            send_msg(
                &ctx,
                writer,
                group_id,
                Some(user_id),
                "❌ 全群的发言字数、文本消息只有排行榜
想看自己字数的变化，用「我的近7天发言字数走势」
「本群本月发言字数排行榜」谁写的字最多
「本群本月文本消息排行榜」谁发的纯文字消息最多（图片、表情包、语音、视频都不算）",
            )
            .await?;
            return Ok(None);
        }

        // 消息类型没有「我的排行榜」：一个人一条消息的类型构成是占比，不是名次。
        if data_type == "消息类型" && scope == Scope::MineGroup && chart_type == "排行榜" {
            send_msg(
                &ctx,
                writer,
                group_id,
                Some(user_id),
                "❌ 消息类型没有「我的排行榜」
想看自己的构成，用「我的今日消息类型走势」
想看本群全体的构成，用「本群今日消息类型排行榜」",
            )
            .await?;
            return Ok(None);
        }

        if scope.in_group() && group_id.is_none() {
            send_msg(
                &ctx,
                writer,
                None,
                Some(user_id),
                "❌ 这个范围只在群里有效
用「本群」查群里的统计，或用「我的」查个人的",
            )
            .await?;
            return Ok(None);
        }

        info!(
            target: LOG_TARGET,
            "Req: Scope={}, Time={}, Data={}, Chart={}, Global={}, Limit={}",
            scope.label(), time_str, data_type, chart_type, is_all_groups, limit
        );

        // 「我的」用自己的区间：近 7 天这类按自然日对齐，并且要能算出「上一期」来比较。
        let (start_time, end_time) = if scope.is_mine() {
            match chart::mine_range(&ctx.db, user_id, time_str).await {
                Ok(range) => range,
                Err(e) => {
                    send_msg(
                        &ctx,
                        writer,
                        group_id,
                        Some(user_id),
                        format!("❌ 统计失败：{e}"),
                    )
                    .await?;
                    return Ok(None);
                }
            }
        } else {
            get_time_range(time_str)
        };

        // 中文标题不靠空格断词：连写成一句「本群今日发言排行榜」像一行标题，
        // 用空格隔开则像四个并排的关键词。范围、总量、时间都在图里的元信息行上。
        let name = chart::ranking_name(data_type);
        let subject = if is_all_groups {
            format!("所有群{time_str}{name}")
        } else {
            format!("{}{time_str}{name}", scope.label())
        };
        let title = format!("{subject}{chart_type}");

        // 「本群我的排行榜」问的不是一张榜，是一个名次：一句话答得清，就不必出长图。
        if scope == Scope::MineGroup && chart_type == "排行榜" {
            let metric = user_metric(data_type);
            let total = queries::get_user_metric_total(
                &ctx.db, group_id, user_id, start_time, end_time, metric,
            )
            .await
            .unwrap_or(0);

            let reply = if total <= 0 {
                format!(
                    "📭 {subject}：这段时间你还没在本群说过话
换个时间范围试试"
                )
            } else {
                let (rank, people, sum) = queries::get_user_rank_in_guild(
                    &ctx.db,
                    group_id.unwrap_or_default(),
                    user_id,
                    start_time,
                    end_time,
                    metric,
                )
                .await
                .unwrap_or((0, 0, 0));
                let word = if metric == queries::UserMetric::TextChars {
                    "字"
                } else {
                    "条"
                };
                let share = if sum > 0 {
                    format!("，占全群 {:.0}%", total as f64 / sum as f64 * 100.0)
                } else {
                    String::new()
                };
                format!(
                    "📊 {subject}
你排第 {rank} / {people}，{} {word}{share}",
                    chart::utils::format_thousands(total)
                )
            };
            send_msg(&ctx, writer, group_id, Some(user_id), reply).await?;
            return Ok(None);
        }

        let (query_group, query_user) = match scope {
            Scope::Group => (group_id, None),
            Scope::Cross => (None, None),
            Scope::MineGroup => (group_id, Some(user_id)),
            Scope::MineCross => (None, Some(user_id)),
        };

        let result_img = if scope.is_mine() && chart_type == "走势" {
            chart::generate_mine(&ctx, query_group, user_id, data_type, time_str, &title).await
        } else {
            chart::generate(
                &ctx,
                is_all_groups,
                data_type,
                chart_type,
                query_group,
                query_user,
                user_id,
                start_time,
                end_time,
                limit,
                &title,
            )
            .await
        };

        match result_img {
            Ok(b64) => {
                // 主动查询只回一张图，不再额外刷一条文本榜。
                let reply = Message::new().image_described(b64, &title);
                send_msg(&ctx, writer, group_id, Some(user_id), reply).await?;
            }
            Err(chart::ChartError::NoData) => {
                // 空态不是错误：说清为什么空，再给一条能立刻做的事。
                let hint = if scope.is_mine() {
                    "这段时间你还没在这个范围里说过话
换个时间范围试试，或先在群里聊几句"
                } else if chart::is_ban_ranking(data_type) {
                    // 禁言只记机器人在线之后发生的；按时长排时，没读出时长的那些也上不了榜。
                    "这段时间没有可统计的禁言（只记录机器人在线时发生的）
换一个时间范围试试"
                } else if chart::is_mention_ranking(data_type) {
                    // 艾特记录从本版才开始记，早些的日子确实是一片空白。
                    "这段时间没有艾特记录（本版起才开始记，早些的日子没有）
换一个时间范围试试"
                } else if chart::is_text_ranking(data_type) {
                    // 只有图片、表情包的那段日子，在这里确实是一片空白。
                    "这段时间没有文字发言（图片、表情包、语音、视频都不算）
换一个时间范围试试"
                } else {
                    "换一个时间范围，或先让群里聊几句"
                };
                send_msg(
                    &ctx,
                    writer,
                    group_id,
                    Some(user_id),
                    format!(
                        "📭 {title}没有数据
{hint}"
                    ),
                )
                .await?;
            }
            Err(chart::ChartError::Failed(e)) => {
                send_msg(
                    &ctx,
                    writer,
                    group_id,
                    Some(user_id),
                    format!("❌ 生成失败：{e}"),
                )
                .await?;
            }
        }

        Ok(None)
    })
}

/// 指令里的名次数：全角数字（输入法常给）当半角读；0 不算数。
/// 位数长到溢出的，当作「要很多」，交给上限去收。
fn parse_count(raw: &str) -> Option<u64> {
    let digits: String = raw
        .chars()
        .map(|c| match c {
            '０'..='９' => char::from(b'0' + (c as u32 - '０' as u32) as u8),
            _ => c,
        })
        .collect();
    let n = digits.parse::<u64>().unwrap_or(u64::MAX);
    (n > 0).then_some(n)
}

/// 这一张榜列多少名：指令给了就用指令的，没给用配置的默认值；都收进 1—上限。
fn ranking_limit(config: &StatsConfig, requested: Option<u64>) -> usize {
    let max = config.ranking_max_limit.max(1) as u64;
    requested
        .unwrap_or(config.ranking_limit as u64)
        .clamp(1, max) as usize
}

pub fn on_connected(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let config: StatsConfig = get_config_or_default(&ctx);

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
                |c, w, gid, m, t| Box::pin(pusher::push_morning_recap(c, w, gid, m, t)),
            ),
            (
                config.noon_brief_enabled,
                "NoonBrief",
                config.noon_brief_time.clone(),
                PushFrequency::Daily,
                |c, w, gid, m, t| Box::pin(pusher::push_noon_brief(c, w, gid, m, t)),
            ),
            (
                config.daily_push_enabled,
                "DailySummary",
                config.daily_push_time.clone(),
                PushFrequency::Daily,
                |c, w, gid, m, t| Box::pin(pusher::push_daily_summary(c, w, gid, m, t)),
            ),
            (
                config.weekly_recap_enabled,
                "WeeklyRecap",
                config.weekly_recap_time.clone(),
                PushFrequency::Weekly(Weekday::Mon),
                |c, w, gid, m, t| Box::pin(pusher::push_weekly_recap(c, w, gid, m, t)),
            ),
            (
                config.monthly_recap_enabled,
                "MonthlyRecap",
                config.monthly_recap_time.clone(),
                PushFrequency::Monthly(1),
                |c, w, gid, m, t| Box::pin(pusher::push_monthly_recap(c, w, gid, m, t)),
            ),
        ];

        // 关闭的项不登记：日志里不会出现「已计划」，也不会留下一个到点才自我否决的定时器。
        // 运行中关掉仍有下面那道二次判断兜底；运行中新开要重启一次才会排期。
        for (index, (enabled, label, time_str, freq, runner)) in
            registrations.into_iter().enumerate()
        {
            if !enabled {
                continue;
            }
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
                        get_config_or_default::<StatsConfig>(&c);
                    let switches = [
                        current.morning_recap_enabled,
                        current.noon_brief_enabled,
                        current.daily_push_enabled,
                        current.weekly_recap_enabled,
                        current.monthly_recap_enabled,
                    ];
                    if current.enabled && switches[index] && current.channel.allows_group(&gid) {
                        runner(
                            c,
                            w,
                            gid,
                            current.push_min_messages,
                            current.push_text_enabled,
                        )
                        .await;
                    }
                },
            );
        }

        Ok(Some(ctx))
    })
}

type PushFn = fn(
    Context,
    LockedWriter,
    String,
    u64,
    bool,
) -> BoxFuture<'static, ()>;

#[cfg(test)]
mod tests {
    use super::*;

    fn check(cmd: &str, scope: Scope, time: &str, data: &str, chart: &str, all: bool) {
        let got = parse_command(cmd).unwrap_or_else(|| panic!("没认出「{cmd}」"));
        assert_eq!(
            (got.scope, got.time, got.data_type, got.chart_type, got.all_groups),
            (scope, time, data, chart, all),
            "「{cmd}」"
        );
    }

    #[test]
    fn parses_scopes_and_periods() {
        check("本群今日发言排行榜", Scope::Group, "今日", "发言", "排行榜", false);
        check("今日发言排行榜", Scope::Group, "今日", "发言", "排行榜", false);
        check("跨群本月发言走势", Scope::Cross, "本月", "发言", "走势", false);
        check("所有群近7天发言走势", Scope::Cross, "近7天", "发言", "走势", true);
        check("我的昨日发言走势", Scope::MineCross, "昨日", "发言", "走势", false);
        check("跨群我的近30天发言字数走势", Scope::MineCross, "近30天", "发言字数", "走势", false);
        check("本群我的今日消息类型走势", Scope::MineGroup, "今日", "消息类型", "走势", false);
        check("本群我的上周被艾特排行榜", Scope::MineGroup, "上周", "被艾特", "排行榜", false);
        // 口语里的近七日、昨天都归到同一段
        check("我的近七日发言走势", Scope::MineCross, "近7天", "发言", "走势", false);
        check("本群我的昨天消息类型走势", Scope::MineGroup, "昨日", "消息类型", "走势", false);
    }

    #[test]
    fn keeps_names_and_rejects_others() {
        assert_eq!(
            parse_command("本群今日发言走势 5").unwrap().count,
            Some("5")
        );
        assert!(parse_command("本群今日发言").is_none());
        assert!(parse_command("我们今日发言排行榜").is_none());
        assert!(parse_command("我的发言排行榜").is_none());
    }
}

