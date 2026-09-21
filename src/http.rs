//! 统一的 HTTP 客户端。
//!
//! reqwest 的 `rustls` 特性走 `rustls-platform-verifier`，它在 Android 上要靠
//! JVM 侧的 `init_hosted` 注入证书存储。Termux 里跑的是纯 CLI 进程，没有 JVM，
//! 首次 TLS 握手会直接 panic（`Expect rustls-platform-verifier to be
//! initialized`），且 panic 发生在 tokio 工作线程里——插件表现为「没反应」。
//!
//! 所以 Android 上改从系统 CA 装载根证书，让 rustls 走纯 webpki 校验；
//! 其他平台保持 reqwest 默认行为。
//!
//! 系统 CA 有两种摆法，两种都要认：
//! - **一份 bundle**：Termux 与常见发行版那样，一个 `cert.pem` 装下全部；
//! - **一目录一证书**：Android 自己那样，`/system/etc/security/cacerts/` 下
//!   149 个 `<hash>.0`，每个文件一张 PEM。应用形态下 Termux 那几条路径都不存在
//!   （2026-09-16 实测：`ca_bundle()` 返回 None，于是回落到默认校验器，
//!   定时任务每 60 秒 panic 一次、所有 HTTPS 全废），所以要自己拼一份。

use reqwest::{Certificate, Client, ClientBuilder};
use std::sync::OnceLock;
use std::time::Duration;

/// Termux 与常见发行版的 CA 包位置；`SSL_CERT_FILE` 优先。
const CA_BUNDLES: &[&str] = &[
    "/data/data/com.termux/files/usr/etc/tls/cert.pem",
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/ssl/cert.pem",
    "/etc/pki/tls/certs/ca-bundle.crt",
];

/// Android 的系统根证书目录（一目录一证书）。Android 14 起挪到了 apex 下，
/// `/system/etc/security/cacerts` 在多数机型上仍是指向它的软链，两条都试一遍。
#[cfg(target_os = "android")]
const CA_DIRS: &[&str] = &[
    "/apex/com.android.conscrypt/cacerts",
    "/system/etc/security/cacerts",
];

#[cfg(target_os = "android")]
fn ca_bundle() -> Option<&'static [u8]> {
    static BUNDLE: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    BUNDLE
        .get_or_init(|| {
            let single = std::env::var("SSL_CERT_FILE")
                .ok()
                .into_iter()
                .chain(CA_BUNDLES.iter().map(|path| (*path).to_string()))
                .find_map(|path| std::fs::read(&path).ok().filter(|bytes| !bytes.is_empty()));
            if single.is_some() {
                return single;
            }
            // 拼一份：每张证书后面补一个换行，免得上一个文件没有行尾时两张粘在一起。
            let mut joined = Vec::new();
            for dir in CA_DIRS {
                if join_certificate_dir(std::path::Path::new(dir), &mut joined) {
                    break;
                }
            }
            (!joined.is_empty()).then_some(joined)
        })
        .as_deref()
}

/// 把「一目录一证书」的那个目录拼进 `into`，返回是否拼到了东西。
///
/// 单独拎出来是为了能在本机（非 Android）上测——真正读系统目录那一步没法测，
/// 但「跳过读不出来的、空文件不加空行、缺行尾的补一个换行」这些是纯逻辑。
/// 读不出来的条目直接跳过：目录里混着子目录或权限不对的文件时，
/// 整份根证书不该因为一条坏的而四散掉。
fn join_certificate_dir(dir: &std::path::Path, into: &mut Vec<u8>) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let before = into.len();
    for entry in entries.flatten() {
        if let Ok(bytes) = std::fs::read(entry.path())
            && !bytes.is_empty()
        {
            into.extend_from_slice(&bytes);
            // 系统里那 149 个文件有的带行尾有的不带；不带才补，补多了会多出空行。
            if !bytes.ends_with(b"\n") {
                into.push(b'\n');
            }
        }
    }
    into.len() > before
}

/// 兜底超时。
///
/// 从前全局客户端一个超时都不设，全靠每个调用点自己写。绝大多数点确实写了，
/// 漏掉的（例如后台刷模型列表）一旦撞上对端不回包就会永久挂住一个任务与一条连接，
/// 而且完全无声——表现是「这个东西一直不变」。这两个数字只做兜底，不打算替代
/// 调用点的取值：单次请求上写的 `.timeout()` 会覆盖它们（视频、音乐生成这些
/// 分钟级的接口各自写了自己的）。
///
/// 用总时限而不是只限连接：不设总时限的话，「连上了、但服务端不吭声」这种更常见
/// 的挂法依然会一直等下去。仓库里没有任何流式响应（长连接只有 Satori 的 WebSocket，
/// 不走 reqwest），所以总时限不会误伤正在传输的大文件。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// 带正确根证书配置的客户端构建器，供需要自定超时/UA 的调用方使用。
pub fn builder() -> ClientBuilder {
    let builder = Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT);
    #[cfg(target_os = "android")]
    {
        if let Some(certs) = ca_bundle().and_then(|pem| Certificate::from_pem_bundle(pem).ok())
            && !certs.is_empty()
        {
            return builder.tls_certs_only(certs);
        }
        warn!(
            target: "System",
            "未找到系统 CA 包（试过 {} 与 {:?}），HTTPS 请求可能失败",
            CA_BUNDLES.join("、"),
            CA_DIRS
        );
    }
    builder
}

/// 全局共享客户端（内部为 Arc，clone 只是增加引用计数）。
pub fn client() -> Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| builder().build().unwrap_or_default())
        .clone()
}

/// `reqwest::get` 的替代：共享连接池，且带正确的根证书。
pub async fn get<U: reqwest::IntoUrl>(url: U) -> reqwest::Result<reqwest::Response> {
    client().get(url).send().await
}

/// 下载资源到内存（图片等小体积文件用）。
pub async fn download_bytes(url: &str) -> reqwest::Result<Vec<u8>> {
    let resp = get(url).await?;
    let bytes = resp.error_for_status()?.bytes().await?;
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一目录一证书的拼法：空的跳过、每张后面补一个换行、坏条目不拖垮整份。
    #[test]
    fn certificates_from_a_directory_are_joined_one_per_line() {
        let dir = std::env::temp_dir().join(format!("acumen-ca-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(
            dir.join("a.0"),
            b"-----BEGIN CERTIFICATE-----\nA\n-----END CERTIFICATE-----",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.0"),
            b"-----BEGIN CERTIFICATE-----\nB\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        // 空文件不该换来一个空行
        std::fs::write(dir.join("c.0"), b"").unwrap();

        let mut joined = Vec::new();
        assert!(join_certificate_dir(&dir, &mut joined));
        let text = String::from_utf8(joined).unwrap();
        assert!(text.contains("A\n-----END CERTIFICATE-----\n"));
        assert!(text.contains("B\n-----END CERTIFICATE-----"));
        assert!(!text.contains("\n\n"), "空文件不该留下空行：{text:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 目录不存在时不该 panic，只回「没拼到」——调用方靠这个往下试下一个位置。
    #[test]
    fn a_missing_directory_is_not_an_error() {
        let mut joined = Vec::new();
        assert!(!join_certificate_dir(
            std::path::Path::new("/nonexistent/acumen-ca"),
            &mut joined
        ));
        assert!(joined.is_empty());
    }
}
