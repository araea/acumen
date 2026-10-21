//! 出网前的地址准入：内网、本机与保留段一律不碰。
//!
//! 网页截图与智能体的联网抓取读同一套判据。搜索结果是不可信输入，模型完全可能被
//! 网页里的一句话指使去读 `127.0.0.1`；把判据放在框架层，两处就不会各自演化。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use url::Host;

/// 主机是不是内网/本机。域名先按后缀快速判，再解析一遍看落点。
pub async fn host_is_internal(host: &Host<&str>) -> bool {
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

pub fn ip_is_internal(ip: IpAddr) -> bool {
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
        || first >= 240 // 240.0.0.0/4 保留段
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
