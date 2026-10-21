use super::utils::format_thousands;
use crate::db::bans;
use crate::db::queries::{self, UserMetric};
use crate::db::entity::{self, Entity as MessageLogs};
use plotters::style::RGBColor;
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect, QueryTrait,
};
use std::collections::HashMap;

// 单点数据结构
#[derive(Clone)]
pub struct ChartDataPoint {
    pub label: String,
    pub value: i64,
}

// 多线数据结构
#[derive(Clone)]
pub struct SeriesData {
    pub name: String,
    pub color: RGBColor,
    pub points: Vec<ChartDataPoint>,
}

// 柱状图数据结构 (包含头像和主题色)
pub struct BarData {
    pub label: String,
    pub value: i64,
    pub avatar_url: Option<String>,
    pub avatar_img: Option<image::RgbaImage>,
    pub theme_color: RGBColor, // 从头像提取的主题色
    /// 非用户条目（如消息类型）的图标字符：不绘制头像，改为绘制彩色图标徽章
    pub icon_char: Option<String>,
    /// 数值后面跟的一小段补充（比如累计禁言时长后面的「3 次」），与数值写在同一处。
    pub detail: Option<String>,
}

/// 消息类型的视觉样式（颜色 + 图标字符）。
/// 排行榜与走势图共用同一份样式，保证两种图表中同一消息类型的颜色/标识一致。
pub struct MessageTypeStyle {
    pub label: &'static str,
    pub color: RGBColor,
    pub icon: &'static str,
}

/// 统计图与词云共用生成的 M3 语义色。系列编号提供独立于颜色的辨认通道。
pub const HUES: [RGBColor; 5] = crate::render::tokens::HUES;

/// 取不到头像色时的兜底色：就用主色。
///
/// 从前是一支与全站无关的钢蓝 —— 于是「没头像的那几行」在一张绿卡片后面
/// 格外扎眼，看着像是另一套系统画错了地方。
pub const FALLBACK_THEME: RGBColor = HUES[0];

pub const MESSAGE_TYPE_STYLES: [MessageTypeStyle; 6] = [
    // 文本取 on-surface-variant：它就是「没有别的东西」的那一类，不该抢眼
    MessageTypeStyle {
        label: "文本",
        color: crate::render::tokens::ON_SURFACE_VARIANT,
        icon: "文",
    },
    MessageTypeStyle {
        label: "图片",
        color: HUES[0],
        icon: "图",
    },
    MessageTypeStyle {
        label: "语音",
        color: HUES[1],
        icon: "语",
    },
    MessageTypeStyle {
        label: "视频",
        color: HUES[2],
        icon: "视",
    },
    MessageTypeStyle {
        label: "动画表情",
        color: HUES[3],
        icon: "动",
    },
    MessageTypeStyle {
        label: "表情",
        color: HUES[4],
        icon: "表",
    },
];

/// 按类型名查找视觉样式，未知类型回退到主色
pub fn message_type_style(label: &str) -> (RGBColor, &'static str) {
    for st in &MESSAGE_TYPE_STYLES {
        if st.label == label {
            return (st.color, st.icon);
        }
    }
    (FALLBACK_THEME, "?")
}

/// 走势图的多条线按序取色：从同一张色表里循环，线条再多也不会冒出表外的颜色。
fn get_palette_color(idx: usize) -> RGBColor {
    HUES[idx % HUES.len()]
}

/// 获取走势图数据
/// 支持：普通消息量走势 (单线)、消息类型走势 (多线)、所有群组走势 (多线)
pub async fn fetch_line_data(
    db: &DatabaseConnection,
    is_all_groups: bool,
    data_type: &str,
    query_group: Option<&str>,
    query_user: Option<&str>,
    start_time: i64,
    end_time: i64,
) -> Result<Vec<SeriesData>, String> {
    let mut series_list: Vec<SeriesData> = Vec::new();

    // 24小时内按小时聚合
    let is_hourly = end_time - start_time <= 86400;

    // 1. 所有群组今日发言走势 (多线)
    if is_all_groups && data_type == "发言" {
        let trends = queries::get_daily_trend_by_guild(db, start_time, end_time, is_hourly)
            .await
            .map_err(|e| e.to_string())?;

        // 按 GroupName 分组
        let mut group_map: HashMap<String, Vec<ChartDataPoint>> = HashMap::new();
        for row in trends {
            group_map
                .entry(row.guild_name)
                .or_default()
                .push(ChartDataPoint {
                    label: row.date,
                    value: row.count,
                });
        }

        // 转换为 SeriesData
        let mut raw_series: Vec<(String, i64, Vec<ChartDataPoint>)> = Vec::new();
        for (name, points) in group_map {
            let total: i64 = points.iter().map(|p| p.value).sum();
            raw_series.push((name, total, points));
        }

        // 按总消息量降序排序，并取前10名
        raw_series.sort_by_key(|b| std::cmp::Reverse(b.1));
        if raw_series.len() > 10 {
            raw_series.truncate(10);
        }

        for (i, (name, _, points)) in raw_series.into_iter().enumerate() {
            series_list.push(SeriesData {
                name,
                color: get_palette_color(i),
                points,
            });
        }

        return Ok(series_list);
    }

    // 2. 消息类型多维度走势 (多线)
    if data_type == "消息类型" {
        let trend = queries::get_message_type_trend(
            db,
            query_group,
            query_user,
            start_time,
            end_time,
            is_hourly,
        )
        .await
        .map_err(|e| e.to_string())?;

        type Extractor = fn(&queries::MessageTypeTrend) -> i64;

        // 预定义值提取函数；颜色统一取自 MESSAGE_TYPE_STYLES，与排行榜保持一致
        let types: Vec<(&str, Extractor)> = vec![
            ("文本", |t| t.text),
            ("图片", |t| t.image),
            ("语音", |t| t.voice),
            ("视频", |t| t.video),
            ("动画表情", |t| t.anim_emoji),
            ("表情", |t| t.face),
        ];

        for (name, extractor) in types {
            let (color, _) = message_type_style(name);
            let points: Vec<ChartDataPoint> = trend
                .iter()
                .map(|t| ChartDataPoint {
                    label: t.date.clone(),
                    value: extractor(t),
                })
                .collect();

            // 只有当该类型有数据时才添加
            if points.iter().any(|p| p.value > 0) {
                series_list.push(SeriesData {
                    name: name.to_string(),
                    color,
                    points,
                });
            }
        }

        return Ok(series_list);
    }

    // 3. 普通消息量走势 (单线)
    let mut chart_data: Vec<ChartDataPoint> = Vec::new();

    if is_hourly {
        // SQL 端按 time_hour 聚合，避免把时间范围内所有消息时间戳一次性载入内存。
        // 查询入口均为整点开始（今日/昨日等），桶 i 对应 hour=(start_hour+i)%24，
        // 与 time_hour 语义一致，可精确对齐到 start_time 起点。
        let hourly =
            queries::get_hourly_activity(db, query_group, query_user, start_time, end_time)
                .await
                .map_err(|e| e.to_string())?;

        let duration_hours = ((end_time - start_time) as f64 / 3600.0).ceil() as i64;
        let total_buckets = duration_hours.clamp(1, 24) as usize;
        let mut counts = vec![0i64; total_buckets];

        for h in hourly {
            let hour = h.hour.rem_euclid(24) as usize;
            if hour < total_buckets {
                counts[hour] += h.count;
            }
        }

        use chrono::{Local, TimeZone};
        for (i, count) in counts.iter().enumerate() {
            let label_time = Local
                .timestamp_opt(start_time + (i as i64 * 3600), 0)
                .unwrap();
            chart_data.push(ChartDataPoint {
                label: label_time.format("%H:%M").to_string(),
                value: *count,
            });
        }
    } else {
        // 超过24小时按天
        let trend = queries::get_daily_trend(db, query_group, query_user, start_time, end_time)
            .await
            .map_err(|e| e.to_string())?;

        chart_data = trend
            .into_iter()
            .map(|t| ChartDataPoint {
                label: t.date,
                value: t.count,
            })
            .collect();
    }

    series_list.push(SeriesData {
        name: "消息量".to_string(),
        color: FALLBACK_THEME, // Primary Blue
        points: chart_data,
    });

    Ok(series_list)
}

/// 文字类榜：按纯文本消息条数，还是按文本字数。
///
/// 这两张榜是一对：发言条数会被「分几段发」和「甩表情包」带偏，要么一条长文被拆成十行
/// 记十次，要么一张图顶一条发言。看字数知道谁写得长，看纯文本条数知道谁真的在说话；
/// 各自再顺带写另一面的数，读者才看得出差别在哪。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextBoard {
    /// 纯文本消息条数（图片、表情包、语音、视频、音乐、合并转发都不算）。
    Messages,
    /// 文本字数（每条消息里文字段的字符数相加）。
    Chars,
}

impl TextBoard {
    /// 指令里的数据类型 → 榜的种类；不是文字榜返回 `None`。
    pub fn parse(data_type: &str) -> Option<Self> {
        match data_type {
            "文本消息" | "文本" | "纯文本" => Some(Self::Messages),
            "发言字数" | "文本字数" | "字数" => Some(Self::Chars),
            _ => None,
        }
    }

    /// 榜名里数据类型那一段，别名统一成这两个写法。
    pub fn name(self) -> &'static str {
        match self {
            Self::Messages => "文本消息",
            Self::Chars => "发言字数",
        }
    }

    fn metric(self) -> UserMetric {
        match self {
            Self::Messages => UserMetric::TextMessages,
            Self::Chars => UserMetric::TextChars,
        }
    }
}

/// 榜上数值后面跟的另一面：字数榜写条数，条数榜写字数。其余榜不带。
fn side_detail(metric: UserMetric, side: i64) -> Option<String> {
    match metric {
        UserMetric::TextChars => Some(format!("{} 条", format_thousands(side))),
        UserMetric::TextMessages => Some(format!("{} 字", format_thousands(side))),
        _ => None,
    }
}

/// 某个数据类型按什么数排。
fn user_metric(data_type: &str) -> UserMetric {
    match TextBoard::parse(data_type) {
        Some(board) => board.metric(),
        None if data_type == "表情包" => UserMetric::AnimEmoji,
        None => UserMetric::Messages,
    }
}

/// 禁言榜按什么排。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rank {
    /// 次数。
    Times,
    /// 累计：每次实际禁言的时长加总。
    Total,
    /// 单次最长：每人名下实际禁言得最久的那一次。
    Longest,
    /// 灰条所写：每次禁言按灰条上写的时长相加，后来有没有提前解除都照记。
    Declared,
}

/// 禁言榜的种类：排谁（被禁言的人，还是动手的管理员），按什么排（见 [`Rank`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BanBoard {
    pub by: bans::By,
    pub rank: Rank,
}

impl BanBoard {
    /// 指令里的数据类型 → 榜的种类；不是禁言榜返回 `None`。
    ///
    /// 「禁言时长」是**累计**：一个人的每次禁言各算实际禁言了多久，加起来排；群友说的「累计」
    /// 「总时长」是同一张榜。「单次最长禁言」排的是每人最长的那一次，是另一张榜。
    /// 「灰条禁言时长」按灰条上写的时长相加，不看后来有没有提前解除，同样是另一张榜。
    pub fn parse(data_type: &str) -> Option<Self> {
        let (by, rank) = match data_type {
            "禁言" => (bans::By::Member, Rank::Times),
            "禁言时长" | "累计禁言时长" | "禁言总时长" => (bans::By::Member, Rank::Total),
            "单次最长禁言" | "最长禁言" => (bans::By::Member, Rank::Longest),
            "灰条禁言时长" | "标称禁言时长" => (bans::By::Member, Rank::Declared),
            "管理员禁言" => (bans::By::Operator, Rank::Times),
            "管理员禁言时长" | "管理员累计禁言时长" | "管理员禁言总时长" => {
                (bans::By::Operator, Rank::Total)
            }
            "管理员单次最长禁言" | "管理员最长禁言" => (bans::By::Operator, Rank::Longest),
            "管理员灰条禁言时长" | "管理员标称禁言时长" => (bans::By::Operator, Rank::Declared),
            _ => return None,
        };
        Some(Self { by, rank })
    }

    /// 榜名里「禁言」那一段。时长榜点明是累计、纪录榜点明是单次——光写「禁言时长」，
    /// 读的人会当成单次最长。
    pub fn name(&self) -> &'static str {
        match (self.by, self.rank) {
            (bans::By::Member, Rank::Times) => "禁言",
            (bans::By::Member, Rank::Total) => "累计禁言时长",
            (bans::By::Member, Rank::Longest) => "单次最长禁言",
            (bans::By::Member, Rank::Declared) => "灰条禁言时长",
            (bans::By::Operator, Rank::Times) => "管理员禁言",
            (bans::By::Operator, Rank::Total) => "管理员累计禁言时长",
            (bans::By::Operator, Rank::Longest) => "管理员单次最长禁言",
            (bans::By::Operator, Rank::Declared) => "管理员灰条禁言时长",
        }
    }

    /// 榜上的数值是不是时长（除次数榜外都是）。
    fn is_duration(&self) -> bool {
        self.rank != Rank::Times
    }
}

/// 禁言榜的数据：返回条目，以及要写在元信息行上的榜外说明（没记下时长的次数、没认出操作者的次数，
/// 都是全群合计，不只榜内）。
///
/// 按时长排时，一次时长都没记下的人不上榜。榜上标 ★ 的是提问的人本人，但**不补位**——
/// 上不了这张榜不是缺憾。
pub async fn fetch_ban_bars(
    db: &DatabaseConnection,
    guild_id: &str,
    start_time: i64,
    end_time: i64,
    limit: usize,
    sender_id: &str,
    board: BanBoard,
) -> Result<(Vec<BarData>, Vec<String>), String> {
    let events = bans::events_since(db, guild_id, start_time)
        .await
        .map_err(|e| e.to_string())?;
    let bans::Board {
        rows: mut tallies,
        unattributed,
        cut_short,
    } = bans::tally(&events, start_time, end_time, board.by);
    // 灰条所写榜看的是灰条上有没有写时长；其余按时长排的榜看实际时长算没算出来
    let unknown: i64 = match board.rank {
        Rank::Declared => tallies.iter().map(|t| t.undeclared).sum(),
        _ => tallies.iter().map(|t| t.unknown).sum(),
    };

    let mut notes = Vec::new();
    if board.is_duration() && unknown > 0 {
        notes.push(format!("另有 {unknown} 次时长未记录"));
    }
    match board.rank {
        // 实际时长的榜：禁言 1 天、几秒后就被解除的，只算那几秒；不说一声，
        // 榜上的数字对不上群里看到的灰条。
        Rank::Total | Rank::Longest if cut_short > 0 => {
            notes.push(format!("{cut_short} 次提前解除，按实际时长算"));
        }
        // 灰条所写榜：数字就是灰条上的字，先把口径写明，再交代其中有几次后来提前解除了——
        // 这些次数照记，读者才不会拿它去对实际禁言的时长。
        Rank::Declared => {
            notes.push("按灰条所写时长相加".to_owned());
            if cut_short > 0 {
                notes.push(format!("其中 {cut_short} 次后来提前解除"));
            }
        }
        _ => {}
    }
    if unattributed > 0 {
        notes.push(format!("另有 {unattributed} 次未识别操作者"));
    }

    // 按时长排的榜，一次时长都没记下的人上不了榜
    match board.rank {
        Rank::Times => tallies
            .sort_by(|a, b| (b.times, b.secs, &a.person_id).cmp(&(a.times, a.secs, &b.person_id))),
        Rank::Total => {
            tallies.retain(|t| t.secs > 0);
            tallies.sort_by(|a, b| {
                (b.secs, b.times, &a.person_id).cmp(&(a.secs, a.times, &b.person_id))
            });
        }
        Rank::Declared => {
            tallies.retain(|t| t.declared > 0);
            tallies.sort_by(|a, b| {
                (b.declared, b.times, &a.person_id).cmp(&(a.declared, a.times, &b.person_id))
            });
        }
        Rank::Longest => {
            tallies.retain(|t| t.longest > 0);
            tallies.sort_by(|a, b| {
                (b.longest, b.secs, &a.person_id).cmp(&(a.longest, a.secs, &b.person_id))
            });
        }
    }
    tallies.truncate(limit);

    let mut bars = Vec::with_capacity(tallies.len());
    for tally in tallies {
        // 昵称与头像取这个人在本群最近一条发言里的；从没发过言（或早已过保留期）就退回号码。
        let latest = MessageLogs::find()
            .select_only()
            .column(entity::Column::MemberNick)
            .column(entity::Column::UserAvatar)
            .filter(entity::Column::GuildId.eq(guild_id))
            .filter(entity::Column::UserId.eq(tally.person_id.as_str()))
            .order_by_desc(entity::Column::Time)
            .into_tuple::<(String, String)>()
            .one(db)
            .await
            .map_err(|e| e.to_string())?;
        let (nick, avatar) = latest.unwrap_or_default();
        let nick = if nick.is_empty() {
            tally.person_id.clone()
        } else {
            nick
        };
        let label = if tally.person_id == sender_id {
            format!("★ {nick}")
        } else {
            nick
        };
        let avatar = if avatar.is_empty() {
            format!(
                "https://q.qlogo.cn/headimg_dl?dst_uin={}&spec=640",
                tally.person_id
            )
        } else {
            avatar
        };
        bars.push(BarData {
            label,
            value: match board.rank {
                Rank::Times => tally.times,
                Rank::Total => tally.secs,
                Rank::Longest => tally.longest,
                Rank::Declared => tally.declared,
            },
            // 时长后面带上次数：累计榜里一眼看出是几次加起来的；纪录榜里看出这是几次里的最长
            detail: match board.rank {
                Rank::Times => None,
                Rank::Total | Rank::Declared => Some(format!("{} 次", tally.times)),
                Rank::Longest => Some(format!("共 {} 次", tally.times)),
            },
            avatar_url: Some(avatar),
            avatar_img: None,
            theme_color: FALLBACK_THEME,
            icon_char: None,
        });
    }
    Ok((bars, notes))
}

/// 获取柱状图数据
#[allow(clippy::too_many_arguments)]
pub async fn fetch_bar_data(
    db: &DatabaseConnection,
    is_all_groups: bool,
    data_type: &str,
    query_group: Option<&str>,
    query_user: Option<&str>,
    sender_id: &str,
    start_time: i64,
    end_time: i64,
    limit: usize,
) -> Result<Vec<BarData>, String> {
    let mut bar_data: Vec<BarData> = Vec::new();
    let metric = user_metric(data_type);

    // 1. 消息类型统计
    if data_type == "消息类型" {
        let stats =
            queries::get_message_type_stats(db, query_group, query_user, start_time, end_time)
                .await
                .map_err(|e| e.to_string())?;

        let raw_data = vec![
            ("文本".to_string(), stats.text),
            ("图片".to_string(), stats.image),
            ("语音".to_string(), stats.voice),
            ("视频".to_string(), stats.video),
            ("动画表情".to_string(), stats.anim_emoji),
            ("表情".to_string(), stats.face),
        ];

        bar_data = raw_data
            .into_iter()
            .filter(|(_, v)| *v > 0)
            .map(|(k, v)| {
                // 每种消息类型使用与走势图一致的差异化颜色和图标
                let (color, icon) = message_type_style(&k);
                BarData {
                    label: k,
                    value: v,
                    avatar_url: None,
                    avatar_img: None,
                    theme_color: color,
                    detail: None,
                    icon_char: Some(icon.to_string()),
                }
            })
            .collect();

        bar_data.sort_by_key(|b| std::cmp::Reverse(b.value));

        return Ok(bar_data);
    }

    // 2. 我的群组排行
    if let Some(uid) = query_user
        && query_group.is_none()
        && !is_all_groups
    {
        let ranking = queries::get_user_guild_participation_ranking(
            db,
            uid,
            start_time,
            end_time,
            limit as u64,
            metric,
        )
        .await
        .map_err(|e| e.to_string())?;

        for r in ranking {
            bar_data.push(BarData {
                label: r.guild_name,
                value: r.count,
                avatar_url: Some(r.avatar).filter(|url| !url.is_empty()),
                avatar_img: None,
                theme_color: FALLBACK_THEME,
                detail: side_detail(metric, r.side),
                icon_char: None,
            });
        }
        // 文字榜只排「写了字的」：只发表情包的日子不该占满一整页 0 字。
        if TextBoard::parse(data_type).is_some() {
            bar_data.retain(|b| b.value > 0);
        }
        return Ok(bar_data);
    }

    // 3. 所有群活跃排行
    if is_all_groups {
        let ranking = queries::get_guild_ranking(db, start_time, end_time, limit as u64)
            .await
            .map_err(|e| e.to_string())?;

        for r in ranking {
            bar_data.push(BarData {
                label: r.guild_name,
                value: r.count,
                avatar_url: Some(r.avatar).filter(|url| !url.is_empty()),
                avatar_img: None,
                theme_color: FALLBACK_THEME,
                detail: None,
                icon_char: None,
            });
        }
        return Ok(bar_data);
    }

    // 4. 用户排行 (发言 / 表情包 / 文本消息 / 字数)
    let ranking = queries::get_user_ranking(
        db,
        query_group,
        start_time,
        end_time,
        limit as u64,
        metric,
    )
    .await
    .map_err(|e| e.to_string())?;

    let mut sender_found = false;

    for r in ranking {
        let mut label = r.nickname;
        let is_sender = r.user_id == sender_id;

        if is_sender && !sender_id.is_empty() {
            label = format!("★ {label}");
            sender_found = true;
        }

        bar_data.push(BarData {
            label,
            value: r.count,
            avatar_url: Some(r.avatar).filter(|url| !url.is_empty()),
            avatar_img: None,
            theme_color: FALLBACK_THEME,
            detail: side_detail(metric, r.side),
            icon_char: None,
        });
    }

    // 补位逻辑
    if !sender_found && !sender_id.is_empty() {
        let count = queries::get_user_metric_total(
            db,
            query_group,
            sender_id,
            start_time,
            end_time,
            metric,
        )
        .await
        .map_err(|e| e.to_string())?;

        if count > 0 {
            let latest = MessageLogs::find()
                .select_only()
                .column(entity::Column::MemberNick)
                .column(entity::Column::UserAvatar)
                .filter(entity::Column::UserId.eq(sender_id))
                .apply_if(query_group, |q, g| q.filter(entity::Column::GuildId.eq(g)))
                .order_by_desc(entity::Column::Time)
                .into_tuple::<(String, String)>()
                .one(db)
                .await
                .unwrap_or(None);

            // 榜外补位也要带上另一面，不然「★」那一行会比榜上的少一小截
            let side = match metric.companion() {
                Some(other) => queries::get_user_metric_total(
                    db,
                    query_group,
                    sender_id,
                    start_time,
                    end_time,
                    other,
                )
                .await
                .unwrap_or(0),
                None => 0,
            };
            let (nick, avatar) = latest.unwrap_or_else(|| (sender_id.to_string(), String::new()));
            bar_data.push(BarData {
                label: format!("★ {nick}"),
                value: count,
                avatar_url: Some(avatar).filter(|url| !url.is_empty()),
                avatar_img: None,
                theme_color: FALLBACK_THEME,
                detail: side_detail(metric, side),
                icon_char: None,
            });
        }
    }

    // 同上：文字榜里一个数都没有的人不占行（榜外补位的人也只补正数）。
    if TextBoard::parse(data_type).is_some() {
        bar_data.retain(|b| b.value > 0);
    }

    Ok(bar_data)
}
