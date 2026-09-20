//! 画像对象的 QQ 头像。
//!
//! 头像地址不需要问任何接口：QQ 的头像 CDN 直接接受 QQ 号，拼出来的地址与
//! satori-qq 模块给事件里 `user.avatar` 的是同一条。画像分析的是历史记录，
//! 手里只有 QQ 号，所以走这条自己拼的。
//!
//! 卡片不引外部资源，所以这里先把图片下回来，转成 data URL 再嵌进 HTML；
//! 取不到（无网、超时、号不存在）就交给调用方退回名字首字的占位头像。

use std::collections::HashMap;
use std::time::Duration;

/// 头像取回的最长等待。它只是报告里的一张配图，不该拖住整次生成。
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// 主头像的边长。`640` 是 CDN 上能取到的最大尺寸。
const FULL_SPEC: u32 = 640;
/// 往来对象那一排头像的边长。卡片上那一枚只有 34 CSS 像素，出图 3 倍也不到 102，
/// 取 140 够用——六张 640 的图会把内嵌的 HTML 撑大好几倍，而它们只占一个小圆。
pub const PARTNER_SPEC: u32 = 140;

/// QQ 公开头像 CDN 上的地址。
pub fn url_at(user_id: i64, spec: u32) -> String {
    format!("https://q.qlogo.cn/headimg_dl?dst_uin={user_id}&spec={spec}")
}

/// 主头像的地址，`spec=640` 是能取到的最大尺寸。
pub fn url(user_id: i64) -> String {
    url_at(user_id, FULL_SPEC)
}

/// 取某个 QQ 号的头像，返回可直接嵌进 HTML 的 data URL；取不到返回 `None`。
pub async fn data_url(user_id: i64) -> Option<String> {
    data_url_at(user_id, FULL_SPEC).await
}

/// 取指定尺寸的头像。往来对象用 [`PARTNER_SPEC`]。
pub async fn data_url_at(user_id: i64, spec: u32) -> Option<String> {
    if user_id <= 0 {
        return None;
    }
    // 下载失败时 `to_data_url` 会把原地址退回来，用前缀区分这两种结果。
    let fetched = tokio::time::timeout(
        FETCH_TIMEOUT,
        crate::plugins::oai::logic::to_data_url(&url_at(user_id, spec)),
    )
    .await
    .ok()?;
    fetched.starts_with("data:").then_some(fetched)
}

/// 一次取回若干个 QQ 号的头像，并起来跑，取不到的就不放进结果里。
///
/// 并起来是必要的：一个人最多六个往来对象，挨个取最坏要等一分钟。它们各自只有
/// 十几 KB，一起发出去对 CDN 也不算什么。
pub async fn data_urls(user_ids: &[i64], spec: u32) -> HashMap<i64, String> {
    let fetches = user_ids
        .iter()
        .copied()
        .map(|user_id| async move { (user_id, data_url_at(user_id, spec).await) });
    let mut out = HashMap::new();
    for (user_id, data) in futures_util::future::join_all(fetches).await {
        if let Some(data) = data {
            out.insert(user_id, data);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_url_carries_the_qq_number() {
        assert_eq!(
            url(3373167460),
            "https://q.qlogo.cn/headimg_dl?dst_uin=3373167460&spec=640"
        );
        // 往来对象那一排取小一档的图。
        assert_eq!(
            url_at(10001, PARTNER_SPEC),
            "https://q.qlogo.cn/headimg_dl?dst_uin=10001&spec=140"
        );
    }

    /// 号非正时不去请求。
    #[tokio::test]
    async fn an_invalid_number_is_not_fetched() {
        assert!(data_url(0).await.is_none());
        assert!(data_url(-1).await.is_none());
    }

    /// 几个号一起取，取不到的（号非法）不进结果，也不影响别的。
    #[tokio::test]
    async fn invalid_numbers_are_skipped_in_a_batch() {
        let faces = data_urls(&[0, -1], PARTNER_SPEC).await;
        assert!(faces.is_empty());
    }

    /// 真拉一次头像，确认地址与网络在线上都能用。
    /// `cargo test --release portrait::avatar -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "访问 QQ 头像 CDN"]
    async fn a_real_avatar_comes_back_as_a_data_url() {
        let data = data_url(3373167460).await.expect("取不到头像");
        assert!(
            data.starts_with("data:image/"),
            "{}",
            &data[..40.min(data.len())]
        );
        println!("头像 data URL 长度 {}", data.len());
    }
}
