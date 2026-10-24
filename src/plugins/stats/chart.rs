pub mod avatar;
pub mod data_loader;
pub mod medal;
pub mod renderer;
pub mod utils;

use crate::event::Context;
use crate::plugins::get_config_or_default;
use crate::plugins::stats::StatsConfig;

use self::avatar::prepare_avatars;
use self::data_loader::{
    BanBoard, BarData, Rank, SeriesData, TextBoard, fetch_ban_bars, fetch_bar_data, fetch_line_data,
    fetch_mention_bars,
};
use crate::db::mentions;
use self::renderer::{BarUnit, draw_bar_chart, draw_line_chart, draw_message_type_ranking};

/// 图表生成不出来的原因。分两类是因为对用户来说这是两件事：
/// 「这段区间没数据」是空态（📭，谁也不怪），其余才是故障（❌）。
#[derive(Debug)]
pub enum ChartError {
    /// 区间内没有任何数据可画。
    NoData,
    /// 真的失败了，附带原因。
    Failed(String),
}

impl std::fmt::Display for ChartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoData => write!(f, "区间内没有数据"),
            Self::Failed(reason) => write!(f, "{reason}"),
        }
    }
}

// 图层内部的失败原因都是人写的句子，一路 `?` 上来即可，不必每处包一层。
impl From<String> for ChartError {
    fn from(reason: String) -> Self {
        Self::Failed(reason)
    }
}

impl From<&str> for ChartError {
    fn from(reason: &str) -> Self {
        Self::Failed(reason.to_string())
    }
}

/// Guard against plotters panics when font glyphs are missing (e.g. CJK text with Latin-only font).
fn draw_with_font_panic_guard<F>(config: &StatsConfig, f: F) -> Result<String, ChartError>
where
    F: FnOnce() -> Result<String, ChartError>,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(e) => {
            let msg = e
                .downcast_ref::<&str>()
                .map(ToString::to_string)
                .or_else(|| e.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown rendering error".to_string());
            Err(ChartError::Failed(format!(
                "图表渲染失败（当前字体可能不支持中文渲染。font_path='{}', font_family='{}'。请通过 font_path 指定字体文件，或安装并配置 font_family）：{}",
                config.font_path, config.font_family, msg
            )))
        }
    }
}

/// 禁言榜：排被禁言的人（`禁言` 按次数、`禁言时长` 按累计的实际时长、`单次最长禁言` 按最长的一次、`灰条禁言时长` 按灰条所写时长相加），
/// 或排动手的管理员（`管理员禁言`、`管理员禁言时长`、`管理员单次最长禁言`）。
pub fn is_ban_ranking(data_type: &str) -> bool {
    BanBoard::parse(data_type).is_some()
}

/// 艾特榜：被艾特、艾特别人。
pub fn is_mention_ranking(data_type: &str) -> bool {
    MentionBoard::parse(data_type).is_some()
}

/// 文字类榜：按纯文本消息条数或文本字数排。
pub fn is_text_ranking(data_type: &str) -> bool {
    TextBoard::parse(data_type).is_some()
}

/// 榜名里数据类型那一段：禁言时长榜写明「累计」，别名（总时长）也统一成这个写法；
/// 文字类榜把「字数」「纯文本」这类顺口说法统一成「发言字数」「文本消息」；其余原样。
pub fn ranking_name(data_type: &str) -> &str {
    if let Some(board) = TextBoard::parse(data_type) {
        return board.name();
    }
    if let Some(board) = MentionBoard::parse(data_type) {
        return board.name();
    }
    BanBoard::parse(data_type).map_or(data_type, |board| board.name())
}

#[allow(clippy::too_many_arguments)]
pub async fn generate(
    ctx: &Context,
    is_all_groups: bool,
    data_type: &str,
    chart_type: &str,
    query_group: Option<&str>,
    query_user: Option<&str>,
    sender_id: &str,
    start_time: i64,
    end_time: i64,
    limit: usize,
    title: &str,
) -> Result<String, ChartError> {
    let db = &ctx.db;
    let config: StatsConfig = get_config_or_default(ctx);

    let title = title.to_owned();

    // 1. 走势图
    if chart_type == "走势" {
        let chart_data: Vec<SeriesData> = fetch_line_data(
            db,
            is_all_groups,
            data_type,
            query_group,
            query_user,
            start_time,
            end_time,
        )
        .await?;

        let image = crate::render::worker::run(move || {
            draw_with_font_panic_guard(&config, || draw_line_chart(&config, &title, chart_data))
        })
        .await
        .map_err(|e| ChartError::Failed(format!("图表任务失败：{e}")))??;
        return Ok(image);
    }

    // 2. 柱状图 / 排行榜
    let (mut bar_data, unit, notes): (Vec<BarData>, BarUnit, Vec<String>) =
        if let Some(board) = MentionBoard::parse(data_type) {
            let bars = fetch_mention_bars(
                db,
                query_group,
                query_user,
                sender_id,
                start_time,
                end_time,
                limit,
                board.direction,
            )
            .await?;
            (bars, BarUnit::Count, Vec::new())
        } else if let Some(board) = BanBoard::parse(data_type) {
            let guild = query_group.ok_or("禁言榜只统计群里的")?;
            let (bars, notes) =
                fetch_ban_bars(db, guild, start_time, end_time, limit, sender_id, board).await?;
            let unit = match board.rank {
                Rank::Times => BarUnit::Count,
                Rank::Total | Rank::Declared => BarUnit::Duration,
                Rank::Longest => BarUnit::Record,
            };
            (bars, unit, notes)
        } else {
            let unit = match TextBoard::parse(data_type) {
                Some(TextBoard::Chars) => BarUnit::Chars,
                _ => BarUnit::Count,
            };
            let bars = fetch_bar_data(
                db,
                is_all_groups,
                data_type,
                query_group,
                query_user,
                sender_id,
                start_time,
                end_time,
                limit,
            )
            .await?;
            (bars, unit, Vec::new())
        };

    // 3. 准备头像
    prepare_avatars(&mut bar_data).await;

    // 4. 绘图：消息类型用竖排信息卡，其余沿用头像条形榜
    let message_types = data_type == "消息类型";
    let image = crate::render::worker::run(move || {
        draw_with_font_panic_guard(&config, || {
            if message_types {
                draw_message_type_ranking(&config, &title, bar_data)
            } else {
                draw_bar_chart(&config, &title, bar_data, unit, &notes)
            }
        })
    })
    .await
    .map_err(|e| format!("图表任务失败：{e}"))??;
    Ok(image)
}

/// 艾特榜：排被 @ 的人（被艾特），或动手 @ 的人（艾特别人）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MentionBoard {
    pub direction: mentions::Direction,
}

impl MentionBoard {
    /// 指令里的数据类型 → 榜的种类；不是艾特榜返回 None。
    ///
    /// 「艾特」单说指动手的那一方：跟「禁言」与「管理员禁言」的关系一样，
    /// 被动的一方有「被」字打头，不会混。
    pub fn parse(data_type: &str) -> Option<Self> {
        let direction = match data_type {
            "被艾特" => mentions::Direction::Received,
            "艾特" | "艾特别人" => mentions::Direction::Given,
            _ => return None,
        };
        Some(Self { direction })
    }

    /// 榜名里数据类型那一段。
    pub fn name(self) -> &'static str {
        match self.direction {
            mentions::Direction::Received => "被艾特",
            mentions::Direction::Given => "艾特别人",
        }
    }
}
