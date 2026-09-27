//! 字符串 ID 的数字替身。
//!
//! acumen 从适配器到每个插件都拿 i64 记群号和用户号（群过滤、数据库、插件的状态表与配置），
//! 这是照着 QQ 写下的假设。satori-wx 的 ID 却是字符串：用户是 `wxid_…`，群是 `<数字>@chatroom`。
//! 与其把所有插件改成字符串 ID，不如在适配器这一层给字符串换一个稳定的数字替身：
//! 入站事件里换成替身，出站 API 调用时（见 `super::route`）再换回原样。
//!
//! 替身是「平台 + 原始 ID」的 FNV-1a 哈希，落在 `[10^15, 9·10^15)`：
//! - 16 位，比任何 QQ 号、群号都长，两边永不撞号，日志里一眼也认得出来；
//! - 正数，插件里「大于 0 才是有效号码」的检查照常通过；
//! - 小于 2^53，网页控制台按 JS 数字显示不丢精度；
//! - 只由内容决定：反查表丢了也还是同一个替身，插件按群记的账不会断。
//!
//! 反查表（替身 → 平台与原始 ID）落在 `data/satori/ids.json`。回复消息时原始 ID 就在事件里，
//! 用不上它；定时推送这类没有入站事件垫底的发送，要靠它找回原始 ID。
//! 纯数字的 ID（QQ）原样当数字用，不进表。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

const BASE: u64 = 1_000_000_000_000_000;
const SPAN: u64 = 8_000_000_000_000_000;
const PATH: &str = "data/satori/ids.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alias {
    pub platform: String,
    pub raw: String,
}

fn table() -> MutexGuard<'static, HashMap<i64, Alias>> {
    static TABLE: OnceLock<Mutex<HashMap<i64, Alias>>> = OnceLock::new();
    TABLE
        .get_or_init(|| Mutex::new(load()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn load() -> HashMap<i64, Alias> {
    if cfg!(test) {
        return HashMap::new();
    }
    let Ok(text) = std::fs::read_to_string(PATH) else {
        return HashMap::new();
    };
    match serde_json::from_str::<HashMap<String, Alias>>(&text) {
        Ok(entries) => entries
            .into_iter()
            .filter_map(|(id, alias)| Some((id.parse().ok()?, alias)))
            .collect(),
        Err(error) => {
            crate::warn!(target: "Bot", "ID 反查表 {PATH} 读不了，从空表开始：{error}");
            HashMap::new()
        }
    }
}

fn save(table: &HashMap<i64, Alias>) {
    if cfg!(test) {
        return;
    }
    let entries: HashMap<String, &Alias> = table
        .iter()
        .map(|(id, alias)| (id.to_string(), alias))
        .collect();
    let result = (|| -> std::io::Result<()> {
        std::fs::create_dir_all("data/satori")?;
        let temporary = format!("{PATH}.tmp-{}", std::process::id());
        std::fs::write(&temporary, serde_json::to_vec_pretty(&entries)?)?;
        std::fs::rename(&temporary, PATH)
    })();
    if let Err(error) = result {
        crate::warn!(target: "Bot", "ID 反查表写不进 {PATH}：{error}");
    }
}

fn hash(platform: &str, raw: &str) -> i64 {
    let mut value: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in platform.bytes().chain([0]).chain(raw.bytes()) {
        value ^= u64::from(byte);
        value = value.wrapping_mul(0x0100_0000_01b3);
    }
    (BASE + value % SPAN) as i64
}

/// 把实现端给的 ID 换成 acumen 用的数字：纯数字原样用，其余换成替身并记进反查表。
pub fn intern(platform: &str, raw: &str) -> i64 {
    if raw.is_empty() {
        return 0;
    }
    if let Ok(id) = raw.parse::<i64>() {
        return id;
    }
    let id = hash(platform, raw);
    let mut table = table();
    if table
        .get(&id)
        .is_none_or(|known| known.raw != raw || known.platform != platform)
    {
        let alias = Alias {
            platform: platform.to_string(),
            raw: raw.to_string(),
        };
        if let Some(previous) = table.insert(id, alias) {
            crate::warn!(target: "Bot", "ID 替身 {id} 撞号：{} 被 {platform}/{raw} 顶掉", previous.raw);
        }
        save(&table);
    }
    id
}

/// 替身对应的平台与原始 ID；不是替身（QQ 号、没见过的数）返回 None。
pub fn lookup(id: i64) -> Option<Alias> {
    if !(BASE as i64..(BASE + SPAN) as i64).contains(&id) {
        return None;
    }
    table().get(&id).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_stay_numbers_and_strings_get_a_stable_long_alias() {
        assert_eq!(intern("red", "3373167460"), 3373167460);
        assert_eq!(intern("wechat", ""), 0);
        let group = intern("wechat", "45123456789@chatroom");
        assert_eq!(group, intern("wechat", "45123456789@chatroom"));
        assert_eq!(group.to_string().len(), 16);
        assert!(group < (1_i64 << 53));
        // 同一串在别的平台上是另一个人。
        assert_ne!(group, intern("discord", "45123456789@chatroom"));
        assert_eq!(
            lookup(group),
            Some(Alias {
                platform: "wechat".into(),
                raw: "45123456789@chatroom".into()
            })
        );
        assert_eq!(lookup(3373167460), None);
    }
}
