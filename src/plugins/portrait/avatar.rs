//! 画像对象的 QQ 头像。
//!
//! 头像地址不需要问任何接口：QQ 的头像 CDN 直接接受 QQ 号，拼出来的地址与
//! satori-qq 模块给事件里 `user.avatar` 的是同一条。画像分析的是历史记录，
//! 手里只有 QQ 号，所以走这条自己拼的。
//!
//! 卡片不引外部资源，所以这里先把图片下回来，转成 data URL 再嵌进 HTML；
//! 取不到（无网、超时、号不存在）就交给调用方退回名字首字的占位头像。

use std::time::Duration;

/// 头像取回的最长等待。它只是报告里的一张配图，不该拖住整次生成。
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// QQ 公开头像 CDN 上的地址，`spec=640` 是能取到的最大尺寸。
pub fn url(user_id: i64) -> String {
    format!("https://q.qlogo.cn/headimg_dl?dst_uin={user_id}&spec=640")
}

/// 取某个 QQ 号的头像，返回可直接嵌进 HTML 的 data URL；取不到返回 `None`。
pub async fn data_url(user_id: i64) -> Option<String> {
    if user_id <= 0 {
        return None;
    }
    // 下载失败时 `to_data_url` 会把原地址退回来，用前缀区分这两种结果。
    let fetched = tokio::time::timeout(
        FETCH_TIMEOUT,
        crate::plugins::oai::logic::to_data_url(&url(user_id)),
    )
    .await
    .ok()?;
    fetched.starts_with("data:").then_some(fetched)
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
    }

    /// 号非正时不去请求。
    #[tokio::test]
    async fn an_invalid_number_is_not_fetched() {
        assert!(data_url(0).await.is_none());
        assert!(data_url(-1).await.is_none());
    }

    /// 真拉一次头像，确认地址与网络在线上都能用。
    /// `cargo test --release portrait::avatar -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "访问 QQ 头像 CDN"]
    async fn a_real_avatar_comes_back_as_a_data_url() {
        let data = data_url(3373167460).await.expect("取不到头像");
        assert!(data.starts_with("data:image/"), "{}", &data[..40.min(data.len())]);
        println!("头像 data URL 长度 {}", data.len());
    }
}
