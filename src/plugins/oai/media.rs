//! 图片、音乐、视频这三条独立模型调用落在哪家：`[oai.media]` 一处说清。
//!
//! 房间里的这几样不看这张表——房间模型写成 `供应商/模型`，接口由那个前缀决定。
//! 这里管的是模型手里的工具：群聊能力层的 `satori_draw` / `satori_music` /
//! `satori_video` 没有房间可依，得有一处说清打到哪家。
//!
//! 一类一项，值有三种写法：
//! - 留空：跟随默认接口（`oai 地址 密钥` 那一份，从前只有这一条路）；
//! - `名字`：`[oai.providers.<名字>]` 里的那家，模型按 `[oai] *_models` 的关键字在那家的
//!   列表里挑，挑不到用内置兜底 id；
//! - `名字/模型`：连模型一起钉死，不再挑（中转站的模型名与关键字对不上时用这个）。
//!
//! 解析不出「有地址、有密钥」的一条时，对应的工具压根不挂给模型（见 `Slots`）：
//! 只接了 DeepSeek 官方、手上没有图像渠道的实例，模型不会对着三个按不动的按钮绕圈。

use super::catalog;
use super::data::Manager;
use super::types::Config;
use super::OaiConfig;

/// 一类媒体生成。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Image,
    Music,
    Video,
}

impl Kind {
    /// 三类的固定顺序：状态卡、日志、报错都按它排。
    pub(crate) const ALL: [Self; 3] = [Self::Image, Self::Music, Self::Video];

    /// 报错与状态卡上的叫法。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Image => "图像",
            Self::Music => "音乐",
            Self::Video => "视频",
        }
    }

    /// `[oai.media]` 里对应的那一项。
    fn field(self, media: &super::MediaConfig) -> &str {
        match self {
            Self::Image => &media.image,
            Self::Music => &media.music,
            Self::Video => &media.video,
        }
    }

    /// 配置键名，报错时指路用。
    fn key(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Music => "music",
            Self::Video => "video",
        }
    }

    /// 判定「这个 id 是不是这类模型」的关键字表。
    fn keywords(self, oai: &OaiConfig) -> &[String] {
        match self {
            Self::Image => &oai.image_models,
            Self::Music => &oai.music_models,
            Self::Video => &oai.video_models,
        }
    }

    fn matches(self, model: &str, keywords: &[String]) -> bool {
        match self {
            Self::Image => super::images::is_images_model(model, keywords),
            Self::Music => super::music::is_music_model(model, keywords),
            Self::Video => super::video::is_video_model(model, keywords),
        }
    }

    /// 站点列表里挑不到时用的真实模型 id。
    fn fallback(self) -> &'static str {
        match self {
            Self::Image => super::images::FALLBACK_MODEL,
            Self::Music => super::music::FALLBACK_MODEL,
            Self::Video => super::video::FALLBACK_MODEL,
        }
    }
}

/// 一次媒体调用要用的接口、密钥与模型 id。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub base: String,
    pub key: String,
    /// 供应商名（`[oai.providers]` 里的写法）；走默认接口时为 `None`。状态卡用它。
    pub provider: Option<String>,
    pub model: String,
}

/// 三类媒体眼下能不能用：能解析出接口与密钥才算数。
///
/// `tool_names` 拿它决定挂不挂那三件工具，于是「模型看得到的按钮」与「按得动的
/// 按钮」永远对得上——与额度开关是同一种做法。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Slots {
    pub image: bool,
    pub music: bool,
    pub video: bool,
}

impl Slots {
    fn of(config: &Config, oai: &OaiConfig) -> Self {
        Self {
            image: resolve_in(config, oai, Kind::Image).is_ok(),
            music: resolve_in(config, oai, Kind::Music).is_ok(),
            video: resolve_in(config, oai, Kind::Video).is_ok(),
        }
    }
}

/// 眼下三类媒体各自可用不可用。
pub(crate) async fn slots(oai: &OaiConfig, mgr: &Manager) -> Slots {
    let config = mgr.config.read().await;
    Slots::of(&config, oai)
}

/// 解析一类媒体这次要打到哪儿。失败时是一句能直接回给模型（再转给群友）的话。
pub(crate) async fn resolve(oai: &OaiConfig, mgr: &Manager, kind: Kind) -> Result<Target, String> {
    let config = mgr.config.read().await;
    resolve_in(&config, oai, kind)
}

/// `名字/模型`、`名字`、空串三种写法 → （供应商、钉死的模型）。
fn spec_of(value: &str) -> (Option<&str>, Option<&str>) {
    let value = value.trim();
    if value.is_empty() {
        return (None, None);
    }
    match value.split_once('/') {
        Some((provider, model)) if !model.trim().is_empty() => (
            (!provider.trim().is_empty()).then(|| provider.trim()),
            Some(model.trim()),
        ),
        _ => (Some(value), None),
    }
}

fn resolve_in(config: &Config, oai: &OaiConfig, kind: Kind) -> Result<Target, String> {
    let keywords = kind.keywords(oai);
    if !keywords.iter().any(|keyword| !keyword.trim().is_empty()) {
        return Err(format!(
            "未配置{}模型，请在 [oai] 里指定（{}_models）",
            kind.label(),
            kind.key()
        ));
    }
    let field = kind.field(&oai.media);
    let (provider, pinned) = spec_of(field);

    let (base, key) = match provider {
        None => {
            if config.api_base.trim().is_empty() || config.api_key.trim().is_empty() {
                return Err(format!(
                    "{}还没接上：[oai.media] {} 留空表示走默认接口，而默认接口没配。\n\
                     · 接中转站：号主发 `oai <名字> <地址> <密钥>`，再把 [oai.media] {} 写成那个名字\n\
                     · 或把中转站填成默认接口：`oai <地址> <密钥>`",
                    kind.label(),
                    kind.key(),
                    kind.key()
                ));
            }
            (config.api_base.trim().to_string(), config.api_key.trim().to_string())
        }
        Some(name) => {
            let Some(found) = catalog::resolve(&oai.providers, name) else {
                return Err(format!(
                    "[oai.media] {} 写的是「{}」，但不认得供应商「{name}」。自己的中转站先发 `oai {name} <地址> <密钥>`。",
                    kind.key(),
                    field.trim()
                ));
            };
            if found.key.is_empty() {
                return Err(catalog::missing_key_hint(name));
            }
            (found.base, found.key)
        }
    };

    let model = match pinned {
        Some(model) => model.to_string(),
        None => pick(config, provider, kind, keywords),
    };
    Ok(Target { base, key, provider: provider.map(str::to_string), model })
}

/// 在这家的模型列表里按关键字挑一个；挑不到用内置兜底 id。
///
/// 列表可能还没拉回来（刚接上、网络不通），所以挑不到不算错——兜底 id 是真实存在的
/// 那一档，站点没有它时接口会自己报错，比「没有模型可用」更好查。
fn pick(config: &Config, provider: Option<&str>, kind: Kind, keywords: &[String]) -> String {
    let listed: &[String] = match provider {
        Some(name) => config.provider_models.get(name).map_or(&[], Vec::as_slice),
        None => &config.models,
    };
    listed
        .iter()
        .find(|model| kind.matches(model, keywords))
        .cloned()
        .unwrap_or_else(|| kind.fallback().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oai() -> OaiConfig {
        OaiConfig::default()
    }

    fn config(base: &str, key: &str) -> Config {
        Config {
            api_base: base.to_string(),
            api_key: key.to_string(),
            models: vec!["gpt-image-2.5-flare".into(), "suno_music".into(), "veo3.1-pro".into()],
            ..Default::default()
        }
    }

    fn provider(base: &str, key: &str) -> super::super::ProviderConfig {
        super::super::ProviderConfig { api_base: base.into(), api_key: key.into(), models: Vec::new() }
    }

    #[test]
    fn blank_follows_the_default_endpoint() {
        let target = resolve_in(&config("https://relay.example/v1", "sk-x"), &oai(), Kind::Image)
            .expect("默认接口配好了");
        assert_eq!(target.base, "https://relay.example/v1");
        assert_eq!(target.model, "gpt-image-2.5-flare");
    }

    #[test]
    fn blank_without_a_default_endpoint_is_not_available() {
        let slots = Slots::of(&config("", ""), &oai());
        assert!(!slots.image && !slots.music && !slots.video);
        let error = resolve_in(&config("", ""), &oai(), Kind::Video).expect_err("没接口");
        assert!(error.contains("oai <名字> <地址> <密钥>"), "{error}");
    }

    #[test]
    fn a_named_provider_wins_over_the_default_endpoint() {
        let mut oai = oai();
        oai.media.image = "relay".into();
        oai.providers.insert("relay".into(), provider("https://relay.example/v1", "sk-relay"));
        let mut config = config("https://api.deepseek.com/v1", "sk-deepseek");
        config
            .provider_models
            .insert("relay".into(), vec!["gpt-image-2.5-flare".into()]);
        let target = resolve_in(&config, &oai, Kind::Image).expect("这家中转站接上了");
        assert_eq!(target.base, "https://relay.example/v1");
        assert_eq!(target.key, "sk-relay");
        assert_eq!(target.model, "gpt-image-2.5-flare");
    }

    #[test]
    fn a_pinned_model_is_used_as_written() {
        let mut oai = oai();
        oai.media.video = "relay/veo3.1-pro".into();
        oai.providers.insert("relay".into(), provider("https://relay.example/v1", "sk-relay"));
        let target = resolve_in(&config("", ""), &oai, Kind::Video).expect("钉死了模型");
        assert_eq!(target.model, "veo3.1-pro");
    }

    #[test]
    fn an_unknown_name_is_refused_instead_of_falling_back() {
        let mut oai = oai();
        oai.media.music = "nowhere".into();
        let error = resolve_in(&config("https://relay.example/v1", "sk-x"), &oai, Kind::Music)
            .expect_err("不认得就不该悄悄打到默认接口");
        assert!(error.contains("nowhere"), "{error}");
    }
}
