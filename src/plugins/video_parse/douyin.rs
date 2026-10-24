//! 抖音：分享短链 → 落地页 HTML 里的 `_ROUTER_DATA` → 原片直链。
//!
//! 抖音的网页版接口要签名（a_bogus / X-Bogus），自签容易翻车；但**落地页本身**
//! 就带着结构化的 `window._ROUTER_DATA`（分享页与稿件页都有），里面的
//! `videoInfoRes.item_list` 含标题与 play_addr 直链——开源生态里
//! （watermark-remover、short_videos 这类解析器的通用做法）都走这条路，
//! 免 Cookie、免签名，2026-10 实测分享短链（v.douyin.com）可达。
//!
//! 口径与 B 站那边一致：只取视频本体；图集（note）与直播不接，静默让路。

use anyhow::{Result, anyhow};
use serde_json::Value;

/// 桌面 Chrome 的 UA。落地页对移动 UA 与桌面 UA 都出数据，统一用桌面页。
pub(crate) const UA: &str = crate::plugins::video_parse::bilibili::UA;

/// 抖音的两种域名形态：落地页 douyin.com（含 www）与分享落点 iesdouyin.com。
/// `v.douyin.com` 是分享短链，被后缀匹配顺带覆盖。
pub(crate) fn is_douyin_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "douyin.com"
        || host == "iesdouyin.com"
        || host.ends_with(".douyin.com")
        || host.ends_with(".iesdouyin.com")
}

/// 解析出来的一条成品：直链与文件名用的标题。
pub(crate) struct Item {
    /// 去重键（aweme id）。
    pub key: String,
    /// 文件名与日志用的标题（desc，可能为空）。
    pub title: String,
    /// 原片直链。通常一条；分段的按顺序拼。
    pub urls: Vec<String>,
}

/// 从一条链接里取出成品。短链在这里跟着跳一次；不是稿件页就报错，让上层静默收场。
pub(crate) async fn parse(raw: &str, timeout: std::time::Duration) -> Result<Item> {
    let client = crate::http::client();
    let response = client
        .get(raw)
        .header(reqwest::header::USER_AGENT, UA)
        .timeout(timeout)
        .send()
        .await?;
    let landed = response.url().clone();
    let html = response.text().await?;

    let data = router_data(&html).ok_or_else(|| anyhow!("落地页没有 _ROUTER_DATA（{landed}）"))?;
    let item = item_list(&data).ok_or_else(|| anyhow!("落地页里没有视频数据"))?;

    let key = item
        .get("aweme_id")
        .and_then(Value::as_str)
        .unwrap_or("douyin")
        .to_string();
    let title = item
        .get("desc")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();

    // 图集与直播没有 play_addr，直接说清，别在下载那一步莫明失败。
    let play = item.pointer("/video/play_addr").ok_or_else(|| anyhow!("这条不是视频（图集或直播暂不支持）"))?;
    let mut urls: Vec<String> = play
        .get("url_list")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .filter(|u| u.starts_with("http"))
                .map(|u| u.replace("/playwm/", "/play/"))
                .collect()
        })
        .unwrap_or_default();
    if urls.is_empty() {
        // 有些条目只在 uri 上带取片参数：拼上官方的取片端点。
        let uri = play.get("uri").and_then(Value::as_str).unwrap_or_default();
        if !uri.is_empty() {
            urls.push(format!("https://www.douyin.com/aweme/v1/play/?uri={uri}"));
        }
    }
    if urls.is_empty() {
        return Err(anyhow!("没取到直链"));
    }
    Ok(Item { key, title, urls })
}

/// 从 HTML 里抠出 `window._ROUTER_DATA = {…}` 那一段 JSON。
///
/// 大括号配对着数扫到与首个 `{` 相抵为止——JSON 里不会有超出字面量的大括号，
/// 字符串里的照单收下，够了。
fn router_data(html: &str) -> Option<Value> {
    let marker = html.find("_ROUTER_DATA")?;
    let rest = &html[marker..];
    let eq = rest.find('=')?;
    let start = rest[eq..].find('{')? + eq;
    let bytes = rest.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in bytes.iter().enumerate().skip(start) {
        let byte = *byte;
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return serde_json::from_str(&rest[start..=offset]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

/// loaderData 里带 item_list 的那一份。键名随落地形态变（video_(id)/page、note_…），
/// 只认值不认键。
fn item_list(data: &Value) -> Option<&Value> {
    let loader = data.get("loaderData")?;
    loader.as_object()?.values().find_map(|page| {
        page.pointer("/videoInfoRes/item_list")
            .and_then(Value::as_array)
            .and_then(|list| list.first())
            .filter(|item| item.get("video").is_some())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 抽取器认得出标准落地页的 _ROUTER_DATA，字符串里的大括号不干扰配对。
    #[test]
    fn router_data_survives_braces_in_strings() {
        let html = r#"<html><script>window._ROUTER_DATA = {"loaderData":{"video_(abc)/page":{"videoInfoRes":{"item_list":[{"aweme_id":"741","desc":"花括号 } 与 { 混排","video":{"play_addr":{"uri":"v0d00fg10000","url_list":["https://www.douyin.com/aweme/v1/playwm/?video_id=v0d00fg10000"]}}}]}}}};</script></html>"#;
        let data = router_data(html).expect("应能抽出 JSON");
        let item = item_list(&data).expect("应能找到视频条目");
        assert_eq!(item.get("aweme_id").and_then(Value::as_str), Some("741"));
    }

    /// 没有 _ROUTER_DATA 的页面返回 None，不 panic。
    #[test]
    fn plain_page_yields_none() {
        assert!(router_data("<html><body>普通页面</body></html>").is_none());
    }
}
