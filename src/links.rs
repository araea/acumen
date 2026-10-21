//! 链接归类：是不是视频站稿件、指向哪一个稿件。
//!
//! 纯解析、不发请求。`video_parse` 用它决定接不接，`webshot` 用它决定要不要让路——
//! 两边读同一个判据，归属只在 `links` 里写一次。

use url::Url;

/// 分享短链的域名，两个都见过——手机端分享出来的是这些。
const SHORT_HOSTS: &[&str] = &["b23.tv", "bili2233.cn"];

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
    pub(crate) fn query(&self) -> String {
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

/// 这条链接归「视频解析」管吗。
pub(crate) fn is_video_link(raw: &str) -> bool {
    let Ok(url) = Url::parse(raw) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    // 短链只有跟过去才知道落到哪个页面；分享出来基本都是稿件，先认下。
    is_short_host(host) || reference(&url).is_some()
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
