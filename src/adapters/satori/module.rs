//! 跟随本机的 Satori 模块：知弦（`satori_qq`）与知言（`satori_wx`）。
//!
//! 两个模块把端口与令牌存在 root 才读得到的模块目录里，用户在模块管理页改了令牌，
//! 这边不该跟着手抄一遍。所以每次连接前经 `su` 现读一次；读不到（没 root、没装模块）
//! 时退回默认端口、不带令牌，和以前手写 `url` 的效果一样。

use crate::config::BotConfig;
use std::time::Duration;

/// 一次读到的模块设置。
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Module {
    /// 模块目录在不在（没装）。
    pub installed: bool,
    /// 管理器里把模块关了。
    pub disabled: bool,
    pub port: Option<u16>,
    pub token: Option<String>,
}

struct Known {
    id: &'static str,
    label: &'static str,
    file: &'static str,
    default_port: u16,
}

const KNOWN: &[Known] = &[
    Known {
        id: "satori_qq",
        label: "知弦（satori-qq）",
        file: "config.json",
        default_port: 3001,
    },
    Known {
        id: "satori_wx",
        label: "知言（satori-wx）",
        file: "satori-wx.conf",
        default_port: 5601,
    },
];

fn known(id: &str) -> Option<&'static Known> {
    KNOWN.iter().find(|k| k.id == id)
}

/// 连接这一轮要用的地址与令牌。`Err` 是「这条连接现在不用连」的原因（模块没装）。
pub(super) async fn resolve(config: &BotConfig) -> Result<BotConfig, String> {
    let Some(id) = config.module.as_deref() else {
        return Ok(config.clone());
    };
    let Some(spec) = known(id) else {
        return Err(format!(
            "不认识的模块「{id}」；现在只有 satori_qq 与 satori_wx"
        ));
    };
    let module = read(spec).await;
    if !module.installed {
        return Err(format!("没装{}，这条连接先不连", spec.label));
    }
    if module.disabled {
        crate::warn!(target: super::LOG_TARGET, "{}在 root 管理器里被关掉了，重新打开并重启手机后才连得上。", spec.label);
    }
    let mut resolved = config.clone();
    if resolved.url.as_deref().is_none_or(str::is_empty) {
        let port = module.port.unwrap_or(spec.default_port);
        resolved.url = Some(format!("http://127.0.0.1:{port}"));
    }
    if resolved.access_token.as_deref().is_none_or(str::is_empty) {
        resolved.access_token = module.token;
    }
    Ok(resolved)
}

/// 模块目录是 root 0700，普通应用连遍历都不行，只能经 su 读。
async fn read(spec: &Known) -> Module {
    let dir = format!("/data/adb/modules/{}", spec.id);
    let script = format!(
        "[ -d {dir} ] || exit 3; [ -f {dir}/disable ] && echo '#disabled'; cat {dir}/{file} 2>/dev/null",
        file = spec.file
    );
    let run = tokio::process::Command::new("su")
        .args(["-c", &script])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    // 没有 su 或没给授权：知弦按默认端口照连（交给连接本身去报错），知言就不去碰了。
    let unknown = Module {
        installed: spec.id == "satori_qq",
        ..Module::default()
    };
    let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(8), run).await else {
        return unknown;
    };
    if output.status.code() == Some(3) {
        return Module::default();
    }
    if !output.status.success() && output.stdout.is_empty() {
        return unknown;
    }
    parse(spec.id, &String::from_utf8_lossy(&output.stdout))
}

fn parse(id: &str, text: &str) -> Module {
    let mut module = Module {
        installed: true,
        ..Module::default()
    };
    let mut body = String::new();
    for line in text.lines() {
        if line == "#disabled" {
            module.disabled = true;
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    if id == "satori_qq" {
        // config.json：{"port": 3001, "token": "…", …}
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
            module.port = value
                .get("port")
                .and_then(serde_json::Value::as_u64)
                .and_then(|p| u16::try_from(p).ok());
            module.token = value
                .get("token")
                .and_then(serde_json::Value::as_str)
                .filter(|t| !t.is_empty())
                .map(str::to_owned);
        }
    } else {
        // satori-wx.conf：一行一个 key=value。
        for line in body.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "port" => module.port = value.parse().ok(),
                "token" if !value.is_empty() => module.token = Some(value.to_string()),
                _ => {}
            }
        }
    }
    module
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_port_and_token_from_both_module_formats() {
        let qq = parse(
            "satori_qq",
            "{\"port\": 3005, \"token\": \"abc\", \"host\": \"127.0.0.1\"}\n",
        );
        assert_eq!((qq.port, qq.token.as_deref()), (Some(3005), Some("abc")));
        let wx = parse("satori_wx", "#disabled\nport=5601\ntoken=xyz\nsend=on\n");
        assert!(wx.disabled);
        assert_eq!((wx.port, wx.token.as_deref()), (Some(5601), Some("xyz")));
    }

    #[test]
    fn empty_token_means_no_auth() {
        let qq = parse("satori_qq", "{\"port\": 3001, \"token\": \"\"}");
        assert_eq!(qq.token, None);
    }
}
