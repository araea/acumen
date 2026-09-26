pub mod avatar;
pub mod data_loader;
pub mod renderer;
pub mod utils;

use crate::event::Context;
use crate::plugins::get_config_or_default;
use crate::plugins::stats::StatsConfig;

use self::avatar::prepare_avatars;
use self::data_loader::{BarData, SeriesData, fetch_bar_data, fetch_line_data};
use self::renderer::{draw_bar_chart, draw_line_chart, draw_message_type_ranking};

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
                .map(|s| s.to_string())
                .or_else(|| e.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown rendering error".to_string());
            Err(ChartError::Failed(format!(
                "图表渲染失败（当前字体可能不支持中文渲染。font_path='{}', font_family='{}'。请通过 font_path 指定字体文件，或安装并配置 font_family）：{}",
                config.font_path, config.font_family, msg
            )))
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn generate(
    ctx: &Context,
    is_all_groups: bool,
    data_type: &str,
    chart_type: &str,
    query_group: Option<i64>,
    query_user: Option<i64>,
    sender_id: i64,
    start_time: i64,
    end_time: i64,
    title: &str,
) -> Result<String, ChartError> {
    let db = &ctx.db;
    let config: StatsConfig = get_config_or_default(ctx, "stats");

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
    let mut bar_data: Vec<BarData> = fetch_bar_data(
        db,
        is_all_groups,
        data_type,
        query_group,
        query_user,
        sender_id,
        start_time,
        end_time,
    )
    .await?;

    // 3. 准备头像
    prepare_avatars(&mut bar_data).await;

    // 4. 绘图：消息类型用竖排信息卡，其余沿用头像条形榜
    let message_types = data_type == "消息类型";
    let image = crate::render::worker::run(move || {
        draw_with_font_panic_guard(&config, || {
            if message_types {
                draw_message_type_ranking(&config, &title, bar_data)
            } else {
                draw_bar_chart(&config, &title, bar_data)
            }
        })
    })
    .await
    .map_err(|e| format!("图表任务失败：{e}"))??;
    Ok(image)
}
