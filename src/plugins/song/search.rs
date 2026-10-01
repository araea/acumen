//! B 站视频搜索：给点歌挑片源用的那一页结果。
//!
//! 只用站点自己的 web 搜索接口 `x/web-interface/search/type`（匿名可用），但必须
//! 带一枚 `buvid3` Cookie，否则随时会被风控拦下。Cookie 在第一次搜索前向
//! `x/frontend/finger/spi` 领一枚，缓存一天；被拦下就换一枚重试一次。
//! UA 与 Referer 沿用视频解析那边摸清的桌面浏览器口径。

use crate::plugins::video_parse::bilibili::UA;
use anyhow::{Result, anyhow};
use serde::Deserialize;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SEARCH_API: &str = "https://api.bilibili.com/x/web-interface/search/type";
const SPI_API: &str = "https://api.bilibili.com/x/frontend/finger/spi";

/// 搜索与领 Cookie 都是很轻的接口，10 秒足够。
const TIMEOUT: Duration = Duration::from_secs(10);

/// 领来的 Cookie 用一天；过期或被拦下再换新的。
const BUVID_TTL: Duration = Duration::from_secs(24 * 3600);

static BUVID: Mutex<Option<(String, Instant)>> = Mutex::new(None);

/// 一条搜索结果，只留挑片与取片要用的几样。
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub(crate) bvid: String,
    pub(crate) title: String,
    pub(crate) author: String,
    /// 成品时长（秒），从站点给的 `分:秒` 文本折算
    pub(crate) duration: u64,
    /// 播放量，模型挑片时看一眼热度
    pub(crate) play: u64,
}

impl Candidate {
    /// 给模型看与写日志用的时长文本。
    pub(crate) fn duration_label(&self) -> String {
        format_duration(self.duration)
    }
}

/// 按关键词搜一页视频，返回相关度排序的候选。
pub(crate) async fn videos(keyword: &str, page_size: u32) -> Result<Vec<Candidate>> {
    let cookie = buvid().await?;
    match fetch(keyword, page_size, &cookie).await {
        Ok(list) => Ok(list),
        Err(first) => {
            // 拦截大多出在 Cookie 上：换一枚重试一次，还不行才是真的搜不动。
            let fresh = fresh_buvid().await?;
            fetch(keyword, page_size, &fresh)
                .await
                .map_err(|second| anyhow!("B 站搜索两次都没成（{first} / {second}）"))
        }
    }
}

/// 当前可用的 Cookie，缓存没过期就沿用。
async fn buvid() -> Result<String> {
    if let Some((value, at)) = BUVID.lock().unwrap().clone()
        && at.elapsed() < BUVID_TTL
    {
        return Ok(value);
    }
    fresh_buvid().await
}

/// 领一枚新 Cookie 并记进缓存。
async fn fresh_buvid() -> Result<String> {
    #[derive(Deserialize)]
    struct Spi {
        code: i64,
        data: Option<SpiData>,
    }
    #[derive(Deserialize)]
    struct SpiData {
        #[serde(rename = "b_3")]
        buvid3: String,
    }

    let value: Spi = crate::http::client()
        .get(SPI_API)
        .header(reqwest::header::USER_AGENT, UA)
        .timeout(TIMEOUT)
        .send()
        .await?
        .json()
        .await?;
    let buvid3 = value
        .data
        .ok_or_else(|| anyhow!("领 Cookie 失败（{}）", value.code))?
        .buvid3;
    *BUVID.lock().unwrap() = Some((buvid3.clone(), Instant::now()));
    Ok(buvid3)
}

async fn fetch(keyword: &str, page_size: u32, cookie: &str) -> Result<Vec<Candidate>> {
    #[derive(Deserialize)]
    struct Envelope {
        code: i64,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        data: Option<SearchData>,
    }
    #[derive(Deserialize)]
    struct SearchData {
        #[serde(default)]
        result: Vec<Item>,
    }
    #[derive(Deserialize)]
    struct Item {
        #[serde(default)]
        bvid: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        author: String,
        #[serde(default)]
        duration: String,
        /// 直播与课程行里这一栏是字符串，视频行才是数字——宽松收下再折。
        #[serde(default)]
        play: serde_json::Value,
    }

    // 这份 reqwest 编译时没带 query 参数支持，查询串手工拼：关键词里的
    // 汉字与符号都交给 form 编码。
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("search_type", "video")
        .append_pair("keyword", keyword)
        .append_pair("page", "1")
        .append_pair("page_size", &page_size.to_string())
        .finish();
    let response = crate::http::client()
        .get(format!("{SEARCH_API}?{query}"))
        .header(reqwest::header::USER_AGENT, UA)
        // Referer 要落在搜索页：给 `www.bilibili.com` 会被风控回一段 HTML（实测）。
        .header(reqwest::header::REFERER, "https://search.bilibili.com")
        .header(reqwest::header::COOKIE, format!("buvid3={cookie}"))
        .timeout(TIMEOUT)
        .send()
        .await?;
    // 412/429 是风控与限流，HTTP 层就该拦下，进了 JSON 只会看到一坨 HTML。
    if matches!(response.status().as_u16(), 412 | 429) {
        anyhow::bail!("B 站风控拦下了这次搜索");
    }
    let envelope: Envelope = response.error_for_status()?.json().await?;
    if envelope.code != 0 {
        let detail = envelope.message.unwrap_or_default();
        anyhow::bail!("B 站搜索回了个错误（{} {detail}）", envelope.code);
    }

    let list = envelope
        .data
        .map(|data| data.result)
        .unwrap_or_default()
        .into_iter()
        .filter(|item| !item.bvid.is_empty() && !item.title.is_empty())
        .map(|item| Candidate {
            bvid: item.bvid,
            title: clean_title(&item.title),
            author: item.author,
            duration: duration_seconds(&item.duration).unwrap_or(0),
            play: item.play.as_u64().unwrap_or(0),
        })
        .filter(|item| item.duration > 0)
        .collect();
    Ok(list)
}

/// 标题里混着高亮记号与 HTML 实体，都还原成给模型与日志看的素文本。
fn clean_title(raw: &str) -> String {
    const REPLACEMENTS: &[(&str, &str)] = &[
        ("<em class=\"keyword\">", ""),
        ("</em>", ""),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&nbsp;", " "),
        // 放最后：别的实体先还原，`&amp;lt;` 这类才不会被解到一半。
        ("&amp;", "&"),
    ];
    let mut text = raw.to_string();
    for (from, to) in REPLACEMENTS {
        text = text.replace(from, to);
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `分:秒`、`时:分:秒` 都折成秒。空段或非数字返回 `None`，由调用方丢掉。
fn duration_seconds(text: &str) -> Option<u64> {
    let mut total = 0u64;
    for part in text.trim().split(':') {
        total = total.checked_mul(60)?.checked_add(part.trim().parse().ok()?)?;
    }
    Some(total)
}

/// 秒数折回给人看的 `时:分:秒`（不满一小时就 `分:秒`）。
pub(crate) fn format_duration(seconds: u64) -> String {
    let (hours, rest) = (seconds / 3600, seconds % 3600);
    let (minutes, secs) = (rest / 60, rest % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}
