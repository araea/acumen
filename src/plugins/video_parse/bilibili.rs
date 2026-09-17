//! B 站取片：链接 → 稿件信息 → 可直下的分片地址。
//!
//! 只用站点自己的两个 web 接口：`x/web-interface/view` 拿稿件与分 P，
//! `x/player/playurl` 拿 mp4 分片。没走 yt-dlp 那类外部工具，理由是 `durl`
//! 里带着每一段的精确字节数——按上限挑画质之前就能算准体积，不必先下几十兆
//! 才发现超了；也不需要为了合流再依赖 ffmpeg。
//!
//! 未登录时 B 站只放 360P/480P，多数稿件给到 720P；要 1080P 得填登录 Cookie，
//! 所以插件配置里留了 `cookie` 一栏。

use anyhow::{Result, anyhow};
use serde::Deserialize;
use std::time::Duration;
use url::Url;

/// 请求头。这三样都有讲究：
///
/// - UA 必须是**桌面**浏览器。`playurl` 回的分片地址带 `platform=pc`，CDN 会按 UA
///   复核：手机 UA 一律 403（实测同一个地址，`Mozilla/5.0` 拿 200、Chrome/Android
///   拿 403），而桌面 UA 与接口、分片两头都对得上。
/// - Referer 缺了分片也是 403，退回到站内页面。
pub(crate) const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
pub(crate) const REFERER: &str = "https://www.bilibili.com";

/// 分享短链的域名，两个都见过——手机端分享出来的是这些。
const SHORT_HOSTS: &[&str] = &["b23.tv", "bili2233.cn"];

const VIEW_API: &str = "https://api.bilibili.com/x/web-interface/view";
const PLAYURL_API: &str = "https://api.bilibili.com/x/player/playurl";

/// 主机是不是分享短链。短链要真正跟随一次跳转才知道落到哪个页面。
pub(crate) fn is_short_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    SHORT_HOSTS
        .iter()
        .any(|rule| host == *rule || host.ends_with(&format!(".{rule}")))
}

/// 链接指向哪一个稿件、哪一分 P。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VideoRef {
    /// 稿件号。`BV…` 与 `av…` 两种写法各存各的，接口两样都收。
    pub(crate) bvid: Option<String>,
    pub(crate) aid: Option<u64>,
    pub(crate) page: u32,
}

impl VideoRef {
    fn query(&self) -> String {
        match (&self.bvid, self.aid) {
            (Some(bvid), _) => format!("bvid={bvid}"),
            (None, Some(aid)) => format!("aid={aid}"),
            _ => String::new(),
        }
    }
}

/// 从这个地址里认出稿件页；不是稿件页就返回 `None`。
///
/// 只认稿件：番剧（`/bangumi/play/ep…`）、直播、空间、专栏、动态都不算——
/// 那些页面截个图比取片有用，留给 `webshot`。
pub(crate) fn reference(url: &Url) -> Option<VideoRef> {
    let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    if host != "bilibili.com" && !host.ends_with(".bilibili.com") {
        return None;
    }
    let page = page_of(url);
    // 分享出来的地址形态不止一种：`/video/BV…`、`/s/video/BV…`、`/video/av123`。
    // 统一按段找 `video`，它后面那一段就是稿件号。
    let mut segments = url.path_segments()?;
    while let Some(segment) = segments.next() {
        if !segment.eq_ignore_ascii_case("video") {
            continue;
        }
        let id = segments.next()?;
        return video_id(id).map(|(bvid, aid)| VideoRef { bvid, aid, page });
    }
    None
}

/// 稿件号的两种写法。`BV` 后面的字符**区分大小写**（那是编码过的数字），
/// 只把前缀本身折成大写，其余原样留着。
///
/// `BV` 号定长 12 位，长度也要卡：`/video/BV1GJ/other` 那种路径片段不该被当成稿件。
fn video_id(id: &str) -> Option<(Option<String>, Option<u64>)> {
    if !id.is_ascii() || id.len() < 3 {
        return None;
    }
    let (prefix, rest) = id.split_at(2);
    if prefix.eq_ignore_ascii_case("bv") {
        let valid = rest.len() == 10 && rest.chars().all(|c| c.is_ascii_alphanumeric());
        return valid.then(|| (Some(format!("BV{rest}")), None));
    }
    if prefix.eq_ignore_ascii_case("av") {
        return rest.parse::<u64>().ok().map(|aid| (None, Some(aid)));
    }
    None
}

/// 分 P 号，地址里写作 `?p=3`；缺省是第一 P。
fn page_of(url: &Url) -> u32 {
    url.query_pairs()
        .find(|(key, _)| key == "p")
        .and_then(|(_, value)| value.parse::<u32>().ok())
        .filter(|page| *page > 0)
        .unwrap_or(1)
}

/// 取片要用的那几样。成品本身就是要发出去的东西，不另外给它写一段说明，
/// 所以这里只留「取哪一 P 的哪条流」与一个能当文件名用的标题。
#[derive(Debug, Clone)]
pub(crate) struct Video {
    pub(crate) bvid: String,
    /// 选中那一 P 的 `cid`，取流时要用
    pub(crate) cid: i64,
    /// 选中那一 P 的序号
    pub(crate) page: u32,
    /// 总共几 P
    pub(crate) pages: u32,
    pub(crate) title: String,
}

/// 拉一次稿件信息。
pub(crate) async fn info(reference: &VideoRef, cookie: &str, timeout: Duration) -> Result<Video> {
    let query = reference.query();
    if query.is_empty() {
        return Err(anyhow!("认不出稿件号"));
    }
    let data: ViewData = get_json(&format!("{VIEW_API}?{query}"), cookie, timeout).await?;

    // 分 P 的 cid 在 `pages` 里；`data.cid` 只是第一 P 的。
    let page = reference.page.max(1);
    let chosen = data.pages.get(page as usize - 1);
    let cid = chosen.map(|part| part.cid).unwrap_or(data.cid);
    if cid == 0 {
        return Err(anyhow!("这页没有可播放的分 P"));
    }

    Ok(Video {
        bvid: data.bvid,
        cid,
        page,
        pages: data.videos.max(1),
        title: data.title.trim().to_string(),
    })
}

#[derive(Deserialize)]
struct Envelope<T> {
    code: i64,
    #[serde(default)]
    message: Option<String>,
    data: Option<T>,
}

#[derive(Deserialize)]
struct ViewData {
    bvid: String,
    #[serde(default)]
    cid: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    videos: u32,
    #[serde(default)]
    pages: Vec<Page>,
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    cid: i64,
}

/// 一档画质的可下载地址。
#[derive(Debug, Clone)]
pub(crate) struct Streams {
    /// 实际拿到的画质编号（见 [`quality_label`]）
    pub(crate) quality: u32,
    /// 分片地址，按顺序拼接就是完整的 mp4
    pub(crate) urls: Vec<String>,
    /// 分片字节数之和。挑画质时就按它算，不必先下一遍。
    pub(crate) size: u64,
}

#[derive(Deserialize)]
struct PlayData {
    #[serde(default)]
    quality: u32,
    #[serde(default)]
    accept_quality: Vec<u32>,
    #[serde(default)]
    durl: Vec<Durl>,
}

#[derive(Deserialize)]
struct Durl {
    #[serde(default)]
    size: u64,
    #[serde(default)]
    url: String,
}

/// 一次取流的结果，外加站点这一稿允许的画质列表（挑下一档时要用）。
struct Play {
    streams: Streams,
    accept: Vec<u32>,
}

async fn playurl(
    bvid: &str,
    cid: i64,
    quality: u32,
    cookie: &str,
    timeout: Duration,
) -> Result<Play> {
    // `fnval=1` 要的是 mp4（`durl`）而不是 DASH：一个文件、不用再合流，
    // 每一段的字节数还写在回包里。
    let url = format!("{PLAYURL_API}?bvid={bvid}&cid={cid}&qn={quality}&fnval=1&fourk=1");
    let data: PlayData = get_json(&url, cookie, timeout).await?;
    let urls: Vec<String> = data
        .durl
        .iter()
        .map(|part| part.url.trim().to_string())
        .filter(|url| !url.is_empty())
        .collect();
    if urls.is_empty() {
        return Err(anyhow!("这条稿件没有可直下的 mp4 分片"));
    }
    let size = data.durl.iter().map(|part| part.size).sum();
    Ok(Play {
        streams: Streams {
            quality: if data.quality == 0 {
                quality
            } else {
                data.quality
            },
            urls,
            size,
        },
        accept: data.accept_quality,
    })
}

/// 按体积上限挑一档画质，并给出可直接下载的分片地址。
///
/// 先用 `prefer` 要一次；回包里带着站点这一稿实际允许的画质列表，放不下就
/// 逐个往下试，挑到第一个放得下的为止。都用不下时报最小那一档的体积，
/// 让群里知道是「太大」而不是「坏了」。
pub(crate) async fn plan(
    bvid: &str,
    cid: i64,
    prefer: u32,
    cap: u64,
    cookie: &str,
    timeout: Duration,
) -> Result<Streams> {
    let prefer = prefer.max(1);
    let first = playurl(bvid, cid, prefer, cookie, timeout).await?;
    if first.streams.size <= cap {
        return Ok(first.streams);
    }

    let mut smallest = first.streams;
    for quality in lower_qualities(&first.accept, prefer) {
        let play = playurl(bvid, cid, quality, cookie, timeout).await?;
        if play.streams.size <= cap {
            return Ok(play.streams);
        }
        if play.streams.size < smallest.size {
            smallest = play.streams;
        }
    }

    Err(anyhow!(
        "最小的 {} 也有 {:.0} MB，超过了大小上限",
        quality_label(smallest.quality),
        smallest.size as f64 / 1_048_576.0
    ))
}

/// 比 `prefer` 低的几档，从高到低。匿名状态下画质列表只有两三档，
/// 这里再收一道，免得某个稿件的列表异常长时一路试下去。
fn lower_qualities(accept: &[u32], prefer: u32) -> Vec<u32> {
    let mut ladder: Vec<u32> = accept
        .iter()
        .copied()
        .filter(|quality| *quality > 0 && *quality < prefer)
        .collect();
    ladder.sort_unstable_by(|a, b| b.cmp(a));
    ladder.dedup();
    ladder.truncate(4);
    ladder
}

/// 画质编号的中文档位名。
pub(crate) fn quality_label(quality: u32) -> String {
    match quality {
        6 => "240P",
        16 => "360P",
        32 => "480P",
        64 => "720P",
        74 => "720P60",
        80 => "1080P",
        112 => "1080P+",
        116 => "1080P60",
        120 => "4K",
        125 => "HDR",
        126 => "杜比视界",
        127 => "8K",
        other => return other.to_string(),
    }
    .to_string()
}

/// `code != 0` 时翻成一句能直接发进群里的话。
fn fail(code: i64, message: Option<&str>) -> anyhow::Error {
    match code {
        -404 | -400 => anyhow!("这条稿件不存在或已被删除"),
        -403 => anyhow!("这条稿件看不了（可能仅限特定地区或需要登录）"),
        -352 => anyhow!("B 站风控拦下了这次请求（配置里填一个登录 Cookie 通常就好了）"),
        -509 => anyhow!("被 B 站限流了，过一会儿再试"),
        other => {
            let detail = message.unwrap_or("").trim();
            anyhow!("B 站回了个错误（{other} {detail}）")
        }
    }
}

async fn get_json<T: serde::de::DeserializeOwned>(
    url: &str,
    cookie: &str,
    timeout: Duration,
) -> Result<T> {
    let envelope: Envelope<T> = request(url, cookie, timeout).await?.json().await?;
    if envelope.code != 0 {
        return Err(fail(envelope.code, envelope.message.as_deref()));
    }
    envelope.data.ok_or_else(|| anyhow!("B 站没有回内容"))
}

async fn request(url: &str, cookie: &str, timeout: Duration) -> Result<reqwest::Response> {
    let mut request = crate::http::client()
        .get(url)
        .header(reqwest::header::USER_AGENT, UA)
        .header(reqwest::header::REFERER, REFERER)
        .timeout(timeout);
    if !cookie.trim().is_empty() {
        request = request.header(reqwest::header::COOKIE, cookie.trim());
    }
    let response = request.send().await?;
    // 412/429 是风控与限流，HTTP 层就该拦下，进了 JSON 只会看到一坨 HTML。
    match response.status().as_u16() {
        412 | 429 => {
            return Err(anyhow!(
                "B 站风控拦下了这次请求（配置里填一个登录 Cookie 通常就好了）"
            ));
        }
        _ => {}
    }
    Ok(response.error_for_status()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    #[test]
    fn video_pages_are_recognised_in_every_shape_we_have_seen() {
        for raw in [
            "https://www.bilibili.com/video/BV1GJ411x7h7",
            "https://www.bilibili.com/video/BV1GJ411x7h7/",
            "https://www.bilibili.com/video/BV1GJ411x7h7?spm_id_from=333.999",
            "https://m.bilibili.com/video/BV1GJ411x7h7",
            "https://www.bilibili.com/s/video/BV1GJ411x7h7",
            "https://www.bilibili.com/video/av80433022",
        ] {
            let reference = reference(&url(raw)).unwrap_or_else(|| panic!("{raw} 应该认得出"));
            assert_eq!(reference.page, 1);
        }
        assert_eq!(
            reference(&url("https://www.bilibili.com/video/BV1GJ411x7h7?p=3"))
                .unwrap()
                .page,
            3
        );
        assert_eq!(
            reference(&url("https://www.bilibili.com/video/av80433022"))
                .unwrap()
                .aid,
            Some(80433022)
        );
    }

    /// `BV` 后面的字符区分大小写，折叠大小写会把稿件号改坏。
    #[test]
    fn bvid_keeps_its_case() {
        let reference = reference(&url("https://www.bilibili.com/video/bv1gj411x7h7")).unwrap();
        assert_eq!(reference.bvid.as_deref(), Some("BV1gj411x7h7"));
    }

    #[test]
    fn other_pages_of_the_same_site_are_left_alone() {
        for raw in [
            "https://www.bilibili.com/bangumi/play/ep307580",
            "https://www.bilibili.com/read/cv123456",
            "https://space.bilibili.com/486906719",
            "https://live.bilibili.com/12345",
            "https://www.bilibili.com/video/",
            "https://www.bilibili.com/video/BV1GJ/other",
            "https://www.bilibili.com",
            "https://example.com/video/BV1GJ411x7h7",
        ] {
            assert!(reference(&url(raw)).is_none(), "{raw} 不该被认成稿件页");
        }
    }

    #[test]
    fn short_link_hosts_are_known() {
        assert!(is_short_host("b23.tv"));
        assert!(is_short_host("B23.TV."));
        assert!(is_short_host("www.b23.tv"));
        assert!(is_short_host("bili2233.cn"));
        assert!(!is_short_host("bilibili.com"));
        assert!(!is_short_host("notb23.tv"));
    }

    #[test]
    fn the_ladder_only_walks_down_from_the_preference() {
        assert_eq!(lower_qualities(&[80, 64, 32, 16], 64), vec![32, 16]);
        assert_eq!(lower_qualities(&[80, 64, 32, 16], 80), vec![64, 32, 16]);
        assert_eq!(lower_qualities(&[], 64), Vec::<u32>::new());
        assert_eq!(lower_qualities(&[16], 16), Vec::<u32>::new());
        let long: Vec<u32> = (1..=12).map(|step| step * 8).collect();
        assert_eq!(lower_qualities(&long, 96).len(), 4);
    }

    #[test]
    fn quality_labels_cover_the_usual_ladder() {
        assert_eq!(quality_label(64), "720P");
        assert_eq!(quality_label(120), "4K");
        assert_eq!(quality_label(999), "999");
    }
}
