use crate::adapters::satori::{LockedWriter, send_msg};
use crate::command::find_url;
use crate::config::build_config;
use crate::event::Context;
use crate::message::Message;
use crate::plugins::{ChannelConfig, PluginError, get_config_or_default};
use crate::render::web::TabGuard;
use anyhow::{Result, anyhow};
use cdp_html_shot::{Browser, CaptureOptions, ImageFormat, LaunchOptions, Viewport};
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
    /// 成图的高度上限（像素）。网页多长就截多长，超过这个数就截断。
    pub max_height: u32,
    /// 单次截图的超时（秒）。
    pub timeout_seconds: u64,
    /// JPEG 画质（0—100）。
    pub quality: u8,
    /// 截图视口宽度（CSS 像素），也是成图宽度。
    pub viewport_width: u32,
    /// 出图倍率。1.0 即与视口同宽，调大更清晰、体积更大。
    pub device_scale_factor: f64,
    /// 不截图的域名，按后缀匹配（`example.com` 同时覆盖 `a.example.com`）。
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
/// 插件就地取原片。
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
    // 1688 的两条风控墙只点名主机，不能写成 `1688.com`：桌面站能正常出内容。
    // `qr.1688.com` 是群友复制分享文案里的短链，`m.1688.com` 是它跳转的落点，
    // 两者实测都停在阿里的滑块验证页（`_____tmd_____/punish`），
    // 而 `detail.1688.com/offer/…` 与 `www.1688.com` 未登录也能看到标题、价格、
    // 店铺和主图。2026-09-16 连测两轮都是同一张验证页。
    "qr.1688.com",
    "m.1688.com",
];

/// 静态截图做不了的站点。
///
/// 与 [`WALLED_DOMAINS`] 的区别是：这里既不是登录墙也不是验证页，纯粹是**渲染不友好**。
/// `dontboardme.com` 整页由脚本动画拼出来，没有稳定的定格帧——截图往往落在过渡
/// 状态上（元素半透明、位置没归位），而且页面极长，按 `max_height` 截断后依旧是
/// 一张又长又发不出去的大图。多次重截都不稳定，只能整站跳过。
///
/// 这类站点跟登录无关，也不该受 `block_walled_sites` 开关影响，所以单独一份名单、
/// 无条件生效；要临时放开只能从这里摘掉。按域名后缀匹配，`dontboardme.com` 覆盖
/// `www.dontboardme.com` 等子域。
const UNCAPTURABLE_DOMAINS: &[&str] = &["dontboardme.com"];

/// 这条消息是不是机器人自己发出去的那份回声。
///
/// 号主与机器人共用同一个 QQ 号：他在客户端手打的消息同样带着这个号进来，
/// satori-qq 会给这类事件打上 `manual_self`。只跳过机器人自己的回声，
/// 号主贴的链接要照常截图——从前只看 `user_id == self_id`，把他一起漏掉了。
fn is_own_echo(msg: &crate::event::MessageEvent<'_>, self_id: i64) -> bool {
    msg.user_id() == self_id && !msg.is_manual_self()
}

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

    // 视频站的链接交给 video_parse：那边就地取原片发进群。
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
        if let Some(rule) = UNCAPTURABLE_DOMAINS
            .iter()
            .find(|rule| domain_matches(&name, rule))
        {
            return Err(format!("{rule} 动画重、整页极长，截出来不能用"));
        }
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

/// 微信文章的专用 UA。
///
/// 微信按 UA 判「环境异常」：桌面 UA 打开 `mp.weixin.qq.com` 的文章会被 302 到
/// `/mp/wappoc_appmsgcaptcha`，截出来只有「当前环境异常，完成验证后即可继续访问」。
/// 换成微信自己的 UA 后正文、封面图都正常。这里不是伪装成登录态——文章本身公开可读，
/// 拦的只是「不是微信客户端」这件事。
const WECHAT_UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 16_6 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148 MicroMessenger/8.0.42(0x18002a2f) NetType/WIFI Language/zh_CN";

/// 微信文章的截图宽度。
///
/// 上面那条 UA 让微信发移动版式，按桌面宽度（默认 1280）截会得到中间一条窄柱、
/// 两边大片空白，所以这类链接改用手机宽度。
const WECHAT_VIEWPORT_WIDTH: u32 = 480;

/// 微信被 302 到的验证端点。落在这里说明没拿到正文。
const WECHAT_CAPTCHA_PATH: &str = "/mp/wappoc_appmsgcaptcha";

/// 是不是微信公众平台的文章页。
fn is_wechat_article(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.trim_end_matches('.')
            .eq_ignore_ascii_case("mp.weixin.qq.com")
    })
}

/// 这张图按多宽截。
fn capture_width(url: &Url, config: &Config) -> u32 {
    if is_wechat_article(url) {
        WECHAT_VIEWPORT_WIDTH
    } else {
        config.viewport_width
    }
}

/// 微信文章用的启动参数。
///
/// Chromium 的 UA 是进程级开关，本库也没有按标签页改 UA 的入口，所以这类链接
/// 单独起一个浏览器，用完即关。不给全局实例换 UA：那样所有网页都会按移动版式渲染。
fn wechat_launch_options(browser_path: Option<&str>) -> LaunchOptions {
    let options = LaunchOptions::new().user_agent(WECHAT_UA);
    match browser_path.filter(|path| !path.is_empty()) {
        Some(path) => options.path(path),
        None => options,
    }
}

/// 落点是不是需要人过验证的中转页。
///
/// 换 UA 之后绝大部分微信文章能出正文，但仍有少数（实测三条里有一条）照样被送去
/// 验证页，成因在微信那一侧。与其把一张只有「去验证」的图发进群里，不如什么都不发。
fn is_verification_page(landed: &str) -> bool {
    landed.contains(WECHAT_CAPTCHA_PATH)
}

/// 微信文章的图靠它自己的脚本补，这里替它补一遍，并等图解码出来。
///
/// 微信会把图的地址挪进属性、把画在页面上的那份换成占位图，等自己的脚本滚到跟前再补
/// 回来。那套脚本在普通 Chromium 里跑不起来——`window.__lazyload_detected` 停在 false，
/// 页面还会自己挂出一句「因网络连接问题，剩余内容暂无法加载」——于是整篇文章截出来
/// 只有一片比例正确的空白块。等多久都没用：实测静置 9 秒、再把视口拉满整页，
/// 18 处占位图一处都没动。两种写法在这里都补：
///
/// - `<img>` 上只写了 `data-src`（正文里的长图）
/// - 微信特有的 `data-lazy-bgimg`（[E2.COOL] 那类「SVG 交互」长图整篇都是它，
///   `background-image` 被换成了 1×1 的占位 gif）
///
/// 只补当前是空白的，页面自己已经填好的不动。表达式自己等图解码，最多 5 秒——
/// 等不到就照当前状态截，不发图也不是这里的选项。
///
/// [E2.COOL]: https://e2.cool
const WECHAT_REHYDRATE_JS: &str = r#"(() => {
  const blankSrc = (v) => !v || v.startsWith('data:');
  const blankBg = (v) => {
    if (!v) return true;
    const m = v.match(/url\(["']?(.*?)["']?\)/);
    return !m || m[1].startsWith('data:');
  };
  const pending = [];
  for (const img of document.images) {
    const dataSrc = img.getAttribute('data-src');
    if (dataSrc && blankSrc(img.getAttribute('src'))) {
      img.src = dataSrc;
      pending.push(img);
    }
  }
  for (const el of document.querySelectorAll('[data-lazy-bgimg]')) {
    const url = el.getAttribute('data-lazy-bgimg');
    if (url && blankBg(el.style.backgroundImage)) {
      el.style.backgroundImage = 'url("' + url + '")';
      const probe = new Image();
      probe.src = url;
      pending.push(probe);
    }
  }
  if (pending.length === 0) return 0;
  const decoded = Promise.all(pending.map(n => n.decode().catch(() => {})));
  const cap = new Promise(r => setTimeout(r, 5000));
  return Promise.race([decoded, cap]).then(() => pending.length);
})()"#;

/// 补回微信页面的占位图，返回补了几处。
///
/// 补不上不该瘫掉整次截图：出错就照原样出图，只是图还是空的。
async fn rehydrate_wechat_images(tab: &cdp_html_shot::Tab) -> u64 {
    match tab.evaluate(WECHAT_REHYDRATE_JS).await {
        Ok(value) => value.as_f64().unwrap_or(0.0) as u64,
        Err(e) => {
            warn!(target: "Plugin/WebShot", "补微信页面的图失败：{}", e);
            0
        }
    }
}

/// 截一张图。闸门、总超时和页面清理都在这里，`capture_page` 只管渲染。
async fn capture_url(
    url: &Url,
    config: &Config,
    browser_path: Option<String>,
) -> Result<Option<String>> {
    let _permit = CAPTURE_GATE
        .acquire()
        .await
        .map_err(|_| anyhow!("截图闸门不可用"))?;

    let budget = Duration::from_secs(config.timeout_seconds.clamp(5, 120) + 15);
    let width = capture_width(url, config);
    let wechat = is_wechat_article(url);
    // page 放在超时之外：无论正常返回、报错还是超时，都能走到下面的清理。
    let mut page = None;
    let mut dedicated = None;
    let result = time::timeout(budget, async {
        let browser = if wechat {
            let browser =
                Browser::launch_with(wechat_launch_options(browser_path.as_deref())).await?;
            dedicated = Some(browser.clone());
            browser
        } else {
            match browser_path.filter(|p| !p.is_empty()) {
                Some(path) => Browser::instance_with_path(path).await,
                None => Browser::instance().await,
            }
        };
        page = Some(TabGuard::new(browser.new_tab().await?));
        capture_page(page.as_ref().unwrap().tab(), url.as_str(), config, width, wechat).await
    })
    .await;

    if let Some(guard) = page {
        guard.close().await;
    }
    if let Some(browser) = dedicated {
        let _ = browser.close_async().await;
    }

    match result {
        Ok(result) => result,
        Err(_) => Err(anyhow!("截图总耗时超时")),
    }
}

/// 渲染一页并截下来。页面只是验证中转页时返回 `Ok(None)`——没有可发的内容。
async fn capture_page(
    tab: &cdp_html_shot::Tab,
    url: &str,
    config: &Config,
    width: u32,
    wechat: bool,
) -> Result<Option<String>> {
    let width = width.clamp(200, 4096);
    let scale = scale_factor(config.device_scale_factor);
    let load_timeout = Duration::from_secs(config.timeout_seconds.clamp(5, 120));

    tab.set_viewport(&Viewport::new(width, 800).with_device_scale_factor(scale))
        .await?;

    match time::timeout(load_timeout, tab.goto(url)).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(anyhow!("Navigate failed: {}", e)),
        Err(_) => return Err(anyhow!("Page load timeout")),
    };

    // 导航后站点还会再 302 一次（验证页就是这么来的），落点以浏览器为准。
    if let Ok(landed) = tab.url().await
        && is_verification_page(&landed)
    {
        info!(target: "Plugin/WebShot", "跳过截图：{} 被送到了验证页", url);
        return Ok(None);
    }

    // 等待页面渲染
    time::sleep(Duration::from_millis(1000)).await;

    // 微信页面的图得自己补，见 `WECHAT_REHYDRATE_JS`。补完再量高度，
    // 免得图上来了布局却还是占位时的尺寸。
    if wechat {
        let restored = rehydrate_wechat_images(tab).await;
        debug!(target: "Plugin/WebShot", "{} 补回 {restored} 处微信占位图", url);
    }

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

    let image = tab
        .screenshot(opts)
        .await
        .map_err(|e| anyhow!("Screenshot failed: {}", e))?;
    Ok(Some(image))
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
        if is_own_echo(&msg_event, ctx.bot.login_user.get().id.parse::<i64>().unwrap_or(0))
        {
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

            match capture_url(&url, &config, browser_path).await {
                Ok(Some(base64_img)) => {
                    let msg = Message::new()
                        .reply(msg_event.message_id())
                        .image(format!("base64://{}", base64_img));

                    send_msg(&ctx, writer, group_id, Some(user_id), msg).await?;
                }
                // 页面没有可发的内容（验证页），原因上面已经记过日志。
                Ok(None) => {}
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
            // 1688 分享短链与它跳转的移动站落点，都停在滑块验证页。
            "https://qr.1688.com/s/7HOhG7uS",
            "https://m.1688.com/offer/1046051827096.html",
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
            // 1688 的桌面站不在名单里：商品页未登录也有内容。
            "https://detail.1688.com/offer/1046051827096.html",
            "https://www.1688.com/",
        ] {
            assert!(check_url(raw, &strict).await.is_ok(), "{raw} 不应被拦");
        }

        // 关掉开关后回到旧行为。
        let mut relaxed = config();
        relaxed.block_walled_sites = false;
        assert!(check_url("https://v.douyin.com/c9EJkQ5hNz0/", &relaxed).await.is_ok());
    }

    /// 动画重、整页极长的站点直接跳过，跟登录墙那个开关无关。
    #[tokio::test]
    async fn uncapturable_sites_are_skipped() {
        let mut strict = config();
        for raw in [
            "https://dontboardme.com/",
            "https://www.dontboardme.com/board/1",
        ] {
            assert!(check_url(raw, &strict).await.is_err(), "{raw} 不该截图");
        }

        // 关掉登录墙开关不该把这类站点放回来。
        strict.block_walled_sites = false;
        assert!(check_url("https://dontboardme.com/", &strict).await.is_err());

        // 后缀匹配不能误伤同前缀的别的域名。
        assert!(check_url("https://dontboardme.com.attacker.net/", &strict).await.is_ok());
        assert!(check_url("https://example.com/", &strict).await.is_ok());
    }

    /// 视频站的稿件链接改由 video_parse 接：那边就地取原片。这里必须跳过，
    /// 否则同一条链接既取一遍片又被截一张图。
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

    /// 号主与机器人共用同一个 QQ 号，判定要看 `manual_self`。
    #[test]
    fn the_owners_own_messages_are_not_treated_as_echoes() {
        let event = |user_id: i64, manual_self: bool| {
            simd_json::serde::to_owned_value(serde_json::json!({
                "post_type": "message",
                "message_type": "group",
                "group_id": 1,
                "user_id": user_id,
                "manual_self": manual_self,
                "message_id": 1,
                "message": [{"type": "text", "data": {"text": "https://example.com"}}]
            }))
            .unwrap()
        };
        let typed = event(7, true);
        assert!(
            !is_own_echo(&crate::event::MessageEvent(&typed), 7),
            "号主手打的消息不该被当成回声"
        );
        let echoed = event(7, false);
        assert!(
            is_own_echo(&crate::event::MessageEvent(&echoed), 7),
            "机器人自己的回声要跳过"
        );
        let member = event(42, false);
        assert!(!is_own_echo(&crate::event::MessageEvent(&member), 7));
    }

    /// 微信文章走专用 UA 与手机宽度，别的站点一概不动。
    #[test]
    fn wechat_articles_get_the_mobile_ua_and_width() {
        let config = config();
        let url = |raw: &str| Url::parse(raw).unwrap();

        assert!(is_wechat_article(&url(
            "https://mp.weixin.qq.com/s/qp_Hqw5RsoKtaXe-l3XUrA"
        )));
        assert!(is_wechat_article(&url("https://MP.Weixin.QQ.com/s/abc")));
        assert!(is_wechat_article(&url("https://mp.weixin.qq.com./s/abc")));
        assert_eq!(
            capture_width(&url("https://mp.weixin.qq.com/s/abc"), &config),
            WECHAT_VIEWPORT_WIDTH
        );

        for raw in [
            "https://weixin.qq.com/r/abc",
            "https://mp.weixin.qq.com.attacker.net/s/abc",
            "https://www.example.com/s/abc",
        ] {
            assert!(!is_wechat_article(&url(raw)), "{raw} 不该走微信那条路");
            assert_eq!(capture_width(&url(raw), &config), config.viewport_width);
        }

        // UA 里得有 MicroMessenger，否则微信照样判「环境异常」。
        assert!(WECHAT_UA.contains("MicroMessenger"));
    }

    /// 被送去验证页时返回「没有内容」，而不是发一张只有「去验证」的图。
    #[test]
    fn verification_pages_are_not_screenshotted() {
        assert!(is_verification_page(
            "https://mp.weixin.qq.com/mp/wappoc_appmsgcaptcha?poc_token=abc&target_url=x"
        ));
        assert!(!is_verification_page(
            "https://mp.weixin.qq.com/s/qp_Hqw5RsoKtaXe-l3XUrA?nwr_flag=1#wechat_redirect"
        ));
        assert!(!is_verification_page("https://example.com/"));
    }

    /// 微信把真地址藏在两种属性里，补图脚本两种都得认。
    #[test]
    fn the_wechat_fix_covers_both_placeholder_shapes() {
        // 正文长图：`<img>` 上只有 `data-src`。
        assert!(WECHAT_REHYDRATE_JS.contains("data-src"));
        // 「SVG 交互」长图：真地址在 `data-lazy-bgimg` 上，画出来的是占位 gif。
        assert!(WECHAT_REHYDRATE_JS.contains("data-lazy-bgimg"));
    }

    #[test]
    fn scale_factor_is_bounded_even_for_non_finite_values() {
        assert_eq!(scale_factor(f64::NAN), 1.0);
        assert_eq!(scale_factor(f64::INFINITY), 1.0);
        assert_eq!(scale_factor(0.1), 0.5);
        assert_eq!(scale_factor(9.0), 4.0);
    }
}
