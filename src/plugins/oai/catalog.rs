//! 认得的模型供应商：名字 → 官方接口、密钥从哪来、去哪申请、管家大人用哪个模型。
//!
//! 学的是 pi-ai 的做法：供应商是一张内置的表，用户只需要给一把密钥，接口地址、
//! 环境变量名、推荐模型都由程序补上。于是 `[oai.providers.deepseek]` 只写一行
//! `api_key` 就能用，号主发一句 `oai deepseek sk-…` 也能用；表里没有的中转站照旧
//! 自己写 `api_base`。
//!
//! 表里只放 **OpenAI 兼容** 的接口（`/chat/completions` + `/models`），因为整个
//! 模型层只会说这一种话；每家的地址都在 2026-10 实际请求核对过（未带密钥时回 401）。
//! 推荐模型（[`Known::model`]）只给拿真密钥试过的那几家，其余留空，让用户在 `/%`
//! 里挑——写一个猜的模型名，比不写更糟。

use std::collections::HashMap;

use super::ProviderConfig;

/// 一家认得的供应商。
pub(crate) struct Known {
    /// 配置与模型写法里用的名字：`[oai.providers.<id>]`、`<id>/模型`。
    pub id: &'static str,
    /// 给人看的名字。
    pub name: &'static str,
    /// 官方 OpenAI 兼容接口。
    pub base: &'static str,
    /// 没在配置里写密钥时，读这个环境变量。
    pub env: &'static str,
    /// 去哪申请密钥；不确定的留空。
    pub keys_url: &'static str,
    /// 管家大人在这家上用哪个模型；留空表示没有把握，由用户自己挑。
    pub model: &'static str,
    /// 也认这些叫法（`kimi` = `moonshot`）。
    pub aliases: &'static [&'static str],
    /// 本机服务，不要密钥。
    pub keyless: bool,
}

const fn known(
    id: &'static str,
    name: &'static str,
    base: &'static str,
    env: &'static str,
    keys_url: &'static str,
    model: &'static str,
    aliases: &'static [&'static str],
) -> Known {
    Known { id, name, base, env, keys_url, model, aliases, keyless: false }
}

/// 排列顺序就是 `oai` 状态卡与「换一家」提示里的顺序：国内直连的在前。
pub(crate) const KNOWN: &[Known] = &[
    known(
        "deepseek",
        "DeepSeek",
        "https://api.deepseek.com/v1",
        "DEEPSEEK_API_KEY",
        "https://platform.deepseek.com/api_keys",
        "deepseek-flash",
        &[],
    ),
    known(
        "mimo",
        "小米 MiMo",
        "https://api.xiaomimimo.com/v1",
        "MIMO_API_KEY",
        "https://platform.xiaomimimo.com",
        "mimo-v2.6-flash",
        &["xiaomi"],
    ),
    known(
        "siliconflow",
        "硅基流动",
        "https://api.siliconflow.cn/v1",
        "SILICONFLOW_API_KEY",
        "https://cloud.siliconflow.cn/account/ak",
        "",
        &["silicon"],
    ),
    known(
        "moonshot",
        "月之暗面 Kimi",
        "https://api.moonshot.cn/v1",
        "MOONSHOT_API_KEY",
        "https://platform.moonshot.cn/console/api-keys",
        "",
        &["kimi"],
    ),
    known(
        "zhipu",
        "智谱 GLM",
        "https://open.bigmodel.cn/api/paas/v4",
        "ZHIPU_API_KEY",
        "https://open.bigmodel.cn/usercenter/apikeys",
        "",
        &["glm", "bigmodel"],
    ),
    known(
        "dashscope",
        "阿里云百炼",
        "https://dashscope.aliyuncs.com/compatible-mode/v1",
        "DASHSCOPE_API_KEY",
        "https://bailian.console.aliyun.com",
        "",
        &["qwen", "bailian", "aliyun"],
    ),
    known(
        "volcengine",
        "火山方舟",
        "https://ark.cn-beijing.volces.com/api/v3",
        "ARK_API_KEY",
        "https://console.volcengine.com/ark",
        "",
        &["ark", "doubao"],
    ),
    known(
        "minimax",
        "MiniMax",
        "https://api.minimaxi.com/v1",
        "MINIMAX_API_KEY",
        "https://platform.minimaxi.com",
        "",
        &[],
    ),
    known(
        "openrouter",
        "OpenRouter",
        "https://openrouter.ai/api/v1",
        "OPENROUTER_API_KEY",
        "https://openrouter.ai/keys",
        "",
        &[],
    ),
    known(
        "openai",
        "OpenAI",
        "https://api.openai.com/v1",
        "OPENAI_API_KEY",
        "https://platform.openai.com/api-keys",
        "",
        &["gpt"],
    ),
    known(
        "anthropic",
        "Anthropic",
        "https://api.anthropic.com/v1",
        "ANTHROPIC_API_KEY",
        "https://console.anthropic.com/settings/keys",
        "",
        &["claude"],
    ),
    known(
        "gemini",
        "Google Gemini",
        "https://generativelanguage.googleapis.com/v1beta/openai",
        "GEMINI_API_KEY",
        "https://aistudio.google.com/apikey",
        "",
        &["google"],
    ),
    known("xai", "xAI", "https://api.x.ai/v1", "XAI_API_KEY", "https://console.x.ai", "", &["grok"]),
    known(
        "groq",
        "Groq",
        "https://api.groq.com/openai/v1",
        "GROQ_API_KEY",
        "https://console.groq.com/keys",
        "",
        &[],
    ),
    Known {
        id: "ollama",
        name: "Ollama（本机）",
        base: "http://127.0.0.1:11434/v1",
        env: "",
        keys_url: "",
        model: "",
        aliases: &[],
        keyless: true,
    },
];

/// 按名字或别名找一家，大小写不敏感。
pub(crate) fn find(name: &str) -> Option<&'static Known> {
    let name = name.trim();
    KNOWN.iter().find(|known| {
        known.id.eq_ignore_ascii_case(name) || known.aliases.iter().any(|alias| alias.eq_ignore_ascii_case(name))
    })
}

/// 配置表里的那一项：先按原名找（大小写不敏感），再找写成同一家别名的那一项（`kimi` 与 `moonshot`）。
pub(crate) fn configured<'a>(
    providers: &'a HashMap<String, ProviderConfig>,
    name: &str,
) -> Option<(&'a str, &'a ProviderConfig)> {
    let exact = providers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name.trim()));
    let same = || {
        let known = find(name)?;
        providers.iter().find(|(key, _)| find(key).is_some_and(|other| std::ptr::eq(other, known)))
    };
    exact.or_else(same).map(|(key, value)| (key.as_str(), value))
}

/// 一家供应商最终用的接口与密钥。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub base: String,
    pub key: String,
    /// 密钥是从哪儿来的（状态卡上显示，不显示密钥本身）。
    pub key_from: KeyFrom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyFrom {
    Config,
    Env(&'static str),
    Keyless,
    Missing,
}

/// 名字 → 接口与密钥：配置里写了的优先，没写的地址取官方、密钥读环境变量。
///
/// 配置里没有、表里也不认得的名字返回 `None`——宁可报错也不悄悄打到别的站点。
pub(crate) fn resolve(providers: &HashMap<String, ProviderConfig>, name: &str) -> Option<Resolved> {
    let entry = configured(providers, name).map(|(_, entry)| entry);
    let known = find(name);
    if entry.is_none() && known.is_none() {
        return None;
    }
    let written = |value: &str| (!value.trim().is_empty()).then(|| value.trim().to_string());
    let base = entry
        .and_then(|entry| written(&entry.api_base))
        .or_else(|| known.map(|known| known.base.to_string()))?;
    let (key, key_from) = if let Some(key) = entry.and_then(|entry| written(&entry.api_key)) {
        (key, KeyFrom::Config)
    } else if let Some((var, key)) = known
        .filter(|known| !known.env.is_empty())
        .and_then(|known| std::env::var(known.env).ok().map(|key| (known.env, key)))
        .filter(|(_, key)| !key.trim().is_empty())
    {
        (key.trim().to_string(), KeyFrom::Env(var))
    } else if known.is_some_and(|known| known.keyless) {
        // 本机服务不看密钥，但请求总得带一个，免得被当成「没配」。
        ("local".to_string(), KeyFrom::Keyless)
    } else {
        (String::new(), KeyFrom::Missing)
    };
    Some(Resolved { base, key, key_from })
}

/// 配好了（有地址也有密钥）的供应商，按 [`KNOWN`] 的顺序，自定义的排在后面。
/// 返回 `(名字, 认得的那一家)`；名字是配置里的写法。
pub(crate) fn ready(providers: &HashMap<String, ProviderConfig>) -> Vec<(String, Option<&'static Known>)> {
    let mut out: Vec<(String, Option<&'static Known>)> = Vec::new();
    for known in KNOWN {
        let name = configured(providers, known.id).map_or(known.id, |(name, _)| name);
        // Ollama 不要密钥，但没写进配置就不算「配好了」：不能假定每台机器都跑着它。
        if known.keyless && configured(providers, known.id).is_none() {
            continue;
        }
        if resolve(providers, name).is_some_and(|r| r.key_from != KeyFrom::Missing) {
            out.push((name.to_string(), Some(known)));
        }
    }
    let mut custom: Vec<&String> = providers
        .keys()
        .filter(|name| find(name).is_none() && !name.eq_ignore_ascii_case("apilio"))
        .collect();
    custom.sort();
    for name in custom {
        if resolve(providers, name).is_some_and(|r| r.key_from != KeyFrom::Missing) {
            out.push((name.clone(), None));
        }
    }
    out
}

/// 某家供应商没配好时，回给用户的那段话：缺什么、去哪儿拿、怎么填。
pub(crate) fn missing_key_hint(name: &str) -> String {
    let id = find(name).map_or(name, |known| known.id);
    let mut text = match find(name) {
        Some(known) => format!("{} 还没填密钥。", known.name),
        None => format!("供应商 {name} 还没填密钥。"),
    };
    text.push_str(&format!(
        "\n· 号主发：oai {id} 密钥（验过再存，存好后撤回那条消息）\n· 或在控制台「智能对话 → providers → {id}」填 api_key"
    ));
    if let Some(known) = find(name).filter(|known| !known.keys_url.is_empty()) {
        text.push_str(&format!("\n密钥在 {} 申请。", known.keys_url));
    }
    text
}
