use crate::config::build_config;
use serde::{Deserialize, Serialize};
use toml::Value;

/// 词云配置：容器级 `#[serde(default)]` 让缺省字段全部回落到 `Default`，单一事实来源。
#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct WordCloudConfig {
    pub enabled: bool,
    /// 画面里最多排几个词，按出现次数从多到少取。
    pub limit: usize,
    /// 成图宽度（像素）。
    pub width: u32,
    /// 成图高度（像素）。
    pub height: u32,
    /// 字体文件绝对路径。给了且存在就优先于 `font_family`。
    pub font_path: Option<String>,
    /// 字体族名，交给系统去找。
    pub font_family: Option<String>,
    /// 一次最多读多少条原始记录。放大它更全面，代价是出图前那一段等待。
    pub max_msg: usize,
}

impl Default for WordCloudConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            limit: 50,
            width: 800,
            height: 600,
            font_path: None,
            font_family: None,
            max_msg: 50000,
        }
    }
}

pub fn default_config() -> Value {
    build_config(WordCloudConfig::default())
}
