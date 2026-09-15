use crate::adapters::satori::{LockedWriter, send_msg};
use crate::command::find_url;
use crate::config::build_config;
use crate::event::Context;
use crate::message::Message;
use crate::plugins::{ChannelConfig, PluginError, get_config_or_default};
use crate::render::web::TabGuard;
use anyhow::{Result, anyhow};
use cdp_html_shot::{Browser, CaptureOptions, ImageFormat, Viewport};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsArray, ValueObjectAccessAsScalar};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::time;
use toml::Value;
use url::{Host, Url};

// ================= Config =================

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub max_height: u32,
    pub timeout_seconds: u64,
    pub quality: u8,
    pub viewport_width: u32,
    pub device_scale_factor: f64,
    pub ignore_domains: Vec<String>,
    /// 是否允许截图访问内网/本机地址。默认关闭——见 `check_url` 的说明。
    pub allow_private_hosts: bool,
    /// 是否跳过截出来没有内容的站点。默认开启——见 `WALLED_DOMAINS`。
    pub block_walled_sites: bool,
    /// 群名单：配了黑名单就对名单外的所有群截图，配了白名单则只对名单内的群截图。
    pub channel: ChannelConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            max_height: 5000,
            timeout_seconds: 30,
            quality: 80,
            viewport_width: 1280,
            device_scale_factor: 1.0,
            ignore_domains: vec![],
            allow_private_hosts: false,
            block_walled_sites: true,
            channel: ChannelConfig::default(),
        }
    }
}

pub fn default_config() -> Value {
    build_config(Config::default())
}

// ================= 链接准入 =================

/// 同时进行的网页截图上限。任意群友都能用一条链接触发渲染，没有闸门时
/// 大量页面会同时吃内存——本机的卡片截图因此同样是串行的。
static CAPTURE_GATE: Semaphore = Semaphore::const_new(2);

/// 单张截图的像素上限（含 `device_scale_factor`），与 `render/web.rs` 保持一致。
const MAX_CAPTURE_PIXELS: f64 = 64_000_000.0;

/// 截出来没有内容的站点。两种成因，结果一样——发给群里的图不是登录页就是验证页，
/// 所以默认跳过（`block_walled_sites` 可关）。
///
/// 只列**稳定**如此、且正文必须登录的站点。实测能正常渲染的（CSDN、虎扑、豆瓣、
/// 今日头条、淘宝、闲鱼、BOSS直聘、AcFun、晋江、起点、番茄、Tumblr、Threads、
/// B 站的番剧与直播页、`m.weibo.cn`）都不在此列；贴吧的「百度安全验证」是偶发的，重测
/// 三次有两次能出内容，也不列。按域名后缀匹配，`douyin.com` 覆盖分享短链的落点
/// `v.douyin.com`，但覆盖不到 `iesdouyin.com`，同名的那条要单独列。
///
/// B 站的**稿件页**（`/video/BV…`）本来也能渲染，但它现在不问这里了：链接准入先经
/// [`crate::plugins::video_parse::is_video_link`]，那类链接直接跳过截图，改由视频解析
/// 插件回一条预览。
const WALLED_DOMAINS: &[&str] = &[
    // 登录墙：没有登录态只能看到登录表单或「打开App」引导
    "douyin.com",
    "iesdouyin.com",
    "kuaishou.com",
    "xiaohongshu.com",
    "xhslink.com",
    "tiktok.com",
    "instagram.com",
    "facebook.com",
    "x.com",
    "twitter.com",
    "linkedin.com",
    "weibo.com",
    "zhihu.com",
    "xueqiu.com",
    "nga.cn",
    "ngabbs.com",
    // 风控墙：从本机（Termux，出口走代理）打开只有验证页或 block 页。
    // 跟登录无关，换出口 IP 后可能又能开，届时把这两条摘掉即可。
    "reddit.com",
    "quora.com",
];

/// 链接是否允许截图，不允许时返回可写进日志的原因。
///
/// 机器人跑在本机，`[webshot]` 又把截图原样发回群里，所以任意群友都能借一条链接
/// 把 `127.0.0.1:6520` 的控制面板、`192.168.x.x` 的路由器后台渲染成图片读走。
/// 这里在交给浏览器之前先把主机拦下来，并用 `url` 做规范化，`http://0x7f000001/`、
/// `http://2130706433/` 这类写法会先被还原成 `127.0.0.1` 再判定。
async fn check_url(raw: &str, config: &Config) -> std::result::Result<Url, String> {
    let url = Url::parse(raw).map_err(|_| "无法解析的链接".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("不支持的协议 {}", url.scheme()));
    }
    let host = url.host().ok_or_else(|| "链接缺少主机名".to_string())?;

    // 视频站的链接交给 video_parse：那边先回一条预览，用户引用预览再开口取片。
    // 这里跳过不截——稿件页要等播放器起画面，截出来又慢又没什么有效信息。
    // 判据与那边共用一份，两处不会各截一次又取一次。
    if crate::plugins::video_parse::is_video_link(raw) {
        return Err("视频站链接由视频解析插件接".to_string());
    }

    if let Host::Domain(name) = host {
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        if let Some(rule) = config
            .ignore_domains
            .iter()
            .find(|rule| domain_matches(&name, rule))
        {
            return Err(format!("域名 {} 命中忽略名单 {}", name, rule.trim()));
        }
        // 放在解析之前：这类站点不必真去 DNS 查一遍，也省一次查询。
        if config.block_walled_sites
            && let Some(rule) = WALLED_DOMAINS.iter().find(|rule| domain_matches(&name, rule))
        {
            return Err(format!("{} 截出来是登录墙或验证页，没有内容", rule));
        }
    }

    if !config.allow_private_hosts && host_is_internal(&host).await {
        return Err(format!("{} 指向内网/本机地址", url));
    }

    Ok(url)
}

/// 忽略名单按域名后缀匹配：`evil.com` 同时覆盖 `a.evil.com`，
/// 但不会像子串匹配那样把 `evil.com.attacker.net` 也算进去。
fn domain_matches(host: &str, rule: &str) -> bool {
    let rule = rule.trim().trim_start_matches('.').to_ascii_lowercase();
    !rule.is_empty() && (host == rule || host.ends_with(&format!(".{rule}")))
}

/// 主机是否落在不该被截图访问的地址上。
///
/// IP 字面量直接判定；域名会真实解析一次，所以 `127.0.0.1.nip.io` 这类
/// 「公网域名解析回本机」的绕过同样会被拦下。
///
/// `web_fetch` 也用这套判定：搜索结果是不可信输入，模型可能被网页里的一句话
/// 指使去读本机面板，两处必须用同一份规则，不能各写一套。
pub(crate) async fn host_is_internal(host: &Host<&str>) -> bool {
    match host {
        Host::Ipv4(ip) => ip_is_internal(IpAddr::V4(*ip)),
        Host::Ipv6(ip) => ip_is_internal(IpAddr::V6(*ip)),
        Host::Domain(name) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            if name == "localhost"
                || name.ends_with(".localhost")
                || name.ends_with(".local")
                || name.ends_with(".internal")
                || name.ends_with(".lan")
                || name.ends_with(".home.arpa")
            {
                return true;
            }
            // `ToSocketAddrs` 会阻塞，挪到阻塞线程；解析失败时放行，
            // 让浏览器自己失败，免得一次 DNS 抖动就误伤正常链接。
            let resolved = tokio::task::spawn_blocking(move || {
                (name.as_str(), 0u16)
                    .to_socket_addrs()
                    .map(|addrs| addrs.map(|addr| addr.ip()).collect::<Vec<_>>())
                    .unwrap_or_default()
            })
            .await
            .unwrap_or_default();
            resolved.iter().any(|ip| ip_is_internal(*ip))
        }
    }
}

fn ip_is_internal(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_is_internal(v4),
        IpAddr::V6(v6) => v6_is_internal(v6),
    }
}

fn v4_is_internal(ip: Ipv4Addr) -> bool {
    let first = ip.octets()[0];
    ip.is_loopback()          // 127.0.0.0/8
        || ip.is_private()    // 10/8、172.16/12、192.168/16
        || ip.is_link_local() // 169.254.0.0/16，含云元数据 169.254.169.254
        || ip.is_unspecified()
        || ip.is_multicast()
        || first == 0         // 0.0.0.0/8
        || first >= 240       // 240.0.0.0/4 保留段
}

fn v6_is_internal(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return v4_is_internal(v4);
    }
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (ip.segments()[0] & 0xfe00) == 0xfc00 // fc00::/7 唯一本地
        || (ip.segments()[0] & 0xffc0) == 0xfe80 // fe80::/10 链路本地
}

/// 缩放因子落在合理区间，非有限值回落到默认的 1.0。
fn scale_factor(scale: f64) -> f64 {
    if scale.is_finite() {
        scale.clamp(0.5, 4.0)
    } else {
        1.0
    }
}

// ================= Core Logic =================

/// 截一张图。闸门、总超时和页面清理都在这里，`capture_page` 只管渲染。
async fn capture_url(url: &str, config: &Config, browser_path: Option<String>) -> Result<String> {
    let _permit = CAPTURE_GATE
        .acquire()
        .await
        .map_err(|_| anyhow!("截图闸门不可用"))?;

    let budget = Duration::from_secs(config.timeout_seconds.clamp(5, 120) + 15);
    // page 放在超时之外：无论正常返回、报错还是超时，都能走到下面的清理。
    let mut page = None;
    let result = time::timeout(budget, async {
        let browser = match browser_path.filter(|p| !p.is_empty()) {
            Some(path) => Browser::instance_with_path(path).await,
            None => Browser::instance().await,
        };
        page = Some(TabGuard::new(browser.new_tab().await?));
        capture_page(page.as_ref().unwrap().tab(), url, config).await
    })
    .await;

    if let Some(guard) = page {
        guard.close().await;
    }

    match result {
        Ok(result) => result,
        Err(_) => Err(anyhow!("截图总耗时超时")),
    }
}

async fn capture_page(tab: &cdp_html_shot::Tab, url: &str, config: &Config) -> Result<String> {
    let width = config.viewport_width.clamp(200, 4096);
    let scale = scale_factor(config.device_scale_factor);
    let load_timeout = Duration::from_secs(config.timeout_seconds.clamp(5, 120));

    tab.set_viewport(&Viewport::new(width, 800).with_device_scale_factor(scale))
        .await?;

    match time::timeout(load_timeout, tab.goto(url)).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(anyhow!("Navigate failed: {}", e)),
        Err(_) => return Err(anyhow!("Page load timeout")),
    };

    // 等待页面渲染
    time::sleep(Duration::from_millis(1000)).await;

    // 计算页面高度
    let height_js = "Math.max(document.body.scrollHeight, document.documentElement.scrollHeight)";
    let page_height = tab.evaluate(height_js).await?.as_f64().unwrap_or(800.0) as u32;

    // 先按配置上限收口，再按像素预算收口，超长页面不会把内存吃干。
    let max_height = config.max_height.clamp(100, 20000);
    let pixel_cap = (MAX_CAPTURE_PIXELS / (f64::from(width) * scale * scale)).floor().max(100.0);
    let final_height = (page_height.max(100).min(max_height) as f64).min(pixel_cap) as u32;

    let capture_viewport = Viewport::new(width, final_height).with_device_scale_factor(scale);

    tab.set_viewport(&capture_viewport).await?;

    if page_height > 800 {
        time::sleep(Duration::from_millis(500)).await;
    }

    let quality = config.quality.clamp(1, 100);
    let format = if quality >= 100 {
        ImageFormat::Png
    } else {
        ImageFormat::Jpeg
    };

    let opts = CaptureOptions::new()
        .with_viewport(capture_viewport)
        .with_format(format)
        .with_quality(quality)
        .with_full_page(true);

    tab.screenshot(opts)
        .await
        .map_err(|e| anyhow!("Screenshot failed: {}", e))
}

// ================= Main Handler =================

pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        // 尝试解析为消息事件
        let msg_event = match ctx.as_message() {
            Some(e) => e,
            None => return Ok(Some(ctx)),
        };

        // 读取配置
        let config: Config = get_config_or_default(&ctx, "webshot");

        // 获取全局浏览器路径配置
        let browser_path = ctx.config.read().unwrap().browser_path.clone();

        // 检查群组黑白名单
        let group_id = msg_event.group_id();
        if !config.channel.allows(group_id) {
            return Ok(Some(ctx));
        }

        let user_id = msg_event.user_id();
        let self_id = ctx.bot.login_user.get().id.parse::<i64>().unwrap_or(0);

        if user_id == self_id {
            return Ok(Some(ctx));
        }

        // 提取 URL
        let url_candidate = if let crate::event::EventType::Satori(event) = &ctx.event {
            if let Some(arr) = event.get_array("message") {
                arr.iter()
                    .filter(|seg| seg.get_str("type") == Some("text"))
                    .find_map(|seg| {
                        seg.get("data")
                            .and_then(|d| d.get_str("text"))
                            .and_then(find_url)
                    })
            } else {
                find_url(msg_event.text())
            }
        } else {
            find_url(msg_event.text())
        };

        if let Some(candidate) = url_candidate {
            let url = match check_url(&candidate, &config).await {
                Ok(url) => url,
                Err(reason) => {
                    info!(target: "Plugin/WebShot", "跳过截图：{}", reason);
                    return Ok(Some(ctx));
                }
            };

            // 执行截图
            info!(target: "Plugin/WebShot", "Capturing: {}", url);

            match capture_url(url.as_str(), &config, browser_path).await {
                Ok(base64_img) => {
                    let msg = Message::new()
                        .reply(msg_event.message_id())
                        .image(format!("base64://{}", base64_img));

                    send_msg(&ctx, writer, group_id, Some(user_id), msg).await?;
                }
                Err(e) => {
                    error!(target: "Plugin/WebShot", "Error capturing {}: {}", url, e);
                }
            }
        }

        Ok(Some(ctx))
    })
}

/// Validate control edits against the plugin's actual configuration type.
pub fn validate_config(value: &toml::Value) -> Result<(), String> {
    <Config as serde::Deserialize>::deserialize(value.clone())
        .map(|_| ())
        .map_err(|_| "配置类型不匹配（请检查数组元素、字段类型及整数范围）".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config::default()
    }

    #[test]
    fn domain_rules_match_on_suffix_not_substring() {
        assert!(domain_matches("evil.com", "evil.com"));
        assert!(domain_matches("a.evil.com", "evil.com"));
        assert!(domain_matches("a.evil.com", ".evil.com"));
        assert!(domain_matches("evil.com", " Evil.COM "));
        // 旧实现用 `url.contains`，这两种都会被误当成命中。
        assert!(!domain_matches("evil.com.attacker.net", "evil.com"));
        assert!(!domain_matches("notevil.com", "evil.com"));
        assert!(!domain_matches("example.com", ""));
    }

    #[test]
    fn internal_ip_ranges_are_recognised() {
        for blocked in [
            "127.0.0.1",
            "127.1.2.3",
            "10.0.0.1",
            "172.16.5.5",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            let ip: IpAddr = blocked.parse().unwrap();
            assert!(ip_is_internal(ip), "{blocked} 应判为内网");
        }
        for allowed in ["8.8.8.8", "1.1.1.1", "172.32.0.1", "2001:4860:4860::8888"] {
            let ip: IpAddr = allowed.parse().unwrap();
            assert!(!ip_is_internal(ip), "{allowed} 不应判为内网");
        }
    }

    #[tokio::test]
    async fn private_and_non_http_links_are_rejected() {
        let config = config();
        for raw in [
            "http://127.0.0.1:6520/panel",
            "http://localhost:3001/v1/proxy/https://x",
            "http://[::1]/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://0x7f000001/",
            "http://2130706433/",
            "http://127.0.0.1./",
            "file:///etc/passwd",
        ] {
            assert!(check_url(raw, &config).await.is_err(), "{raw} 不应放行");
        }
        assert!(check_url("https://example.com/a?b=1", &config).await.is_ok());
    }

    #[tokio::test]
    async fn ignore_list_and_opt_in_are_respected() {
        let mut config = config();
        config.ignore_domains = vec!["evil.com".into()];
        assert!(check_url("https://a.evil.com/x", &config).await.is_err());

        // 显式放开后，内网字面量可以截图；忽略名单仍然生效。
        config.allow_private_hosts = true;
        assert!(check_url("http://192.168.1.1/", &config).await.is_ok());
        assert!(check_url("http://evil.com/", &config).await.is_err());
    }

    #[tokio::test]
    async fn walled_sites_are_skipped() {
        let strict = config();
        // 群友复制出来的抖音分享文案，链接落点是 v.douyin.com。
        for raw in [
            "https://v.douyin.com/c9EJkQ5hNz0/",
            "https://www.douyin.com/video/7682536956487677802",
            "https://www.iesdouyin.com/share/video/7682536956487677802/",
            "https://www.xiaohongshu.com/explore/abc",
            "https://xhslink.com/a/abc",
            "https://www.tiktok.com/@a/video/123",
            "https://x.com/a/status/1",
            "https://www.zhihu.com/question/1",
            "https://xueqiu.com/S/SH600519",
            "https://bbs.nga.cn/thread.php?fid=-7",
            "https://www.reddit.com/r/rust/",
            "https://www.quora.com/What-is-rust",
        ] {
            assert!(check_url(raw, &strict).await.is_err(), "{raw} 不应放行");
        }

        // 未登录也能看正文的站点不受影响。
        for raw in [
            "https://www.bilibili.com/bangumi/play/ep307580",
            "https://live.bilibili.com/12345",
            "https://m.weibo.cn/detail/123",
            "https://mp.weixin.qq.com/s/abc",
            "https://www.youtube.com/watch?v=abc",
            "https://www.pixiv.net/artworks/91475850",
            "https://tieba.baidu.com/f?kw=rust",
            "https://www.hupu.com/",
        ] {
            assert!(check_url(raw, &strict).await.is_ok(), "{raw} 不应被拦");
        }

        // 关掉开关后回到旧行为。
        let mut relaxed = config();
        relaxed.block_walled_sites = false;
        assert!(check_url("https://v.douyin.com/c9EJkQ5hNz0/", &relaxed).await.is_ok());
    }

    /// 视频站的稿件链接改由 video_parse 接：预览 + 引用取片。这里必须跳过，
    /// 否则同一条链接既回一条预览又被截一张图。
    #[tokio::test]
    async fn video_links_are_left_to_the_video_parser() {
        for raw in [
            "https://www.bilibili.com/video/BV1GJ411x7h7",
            "https://b23.tv/BV1GJ411x7h7",
            "https://m.bilibili.com/video/av80433022",
        ] {
            assert!(check_url(raw, &config()).await.is_err(), "{raw} 不该截图");
        }
    }

    #[test]
    fn scale_factor_is_bounded_even_for_non_finite_values() {
        assert_eq!(scale_factor(f64::NAN), 1.0);
        assert_eq!(scale_factor(f64::INFINITY), 1.0);
        assert_eq!(scale_factor(0.1), 0.5);
        assert_eq!(scale_factor(9.0), 4.0);
    }
}
