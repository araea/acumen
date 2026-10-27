//! 群里人人看得到的几样东西：群文件、群精华、群公告、群荣誉榜。只读，供 `satori_group` 用。
//!
//! 从前它们是 `satori_action` 里的四个动作（files_list / file_download / bulletins / essences），
//! 三处不对：读一下要占一次动作额度；群聊往前走一句就被「先读 satori_context」挡回去；
//! 搭话里只要碰过 `satori_action`，最后那句回答就按「话已经在工具里说过了」吞掉——
//! 群友问「精华里有什么」，它翻完不吭声。回执也是实现端的原始大 JSON，文件大小是字节数、
//! 时间是 Unix 秒，模型还得自己换算。
//!
//! 现在单独一件工具：不占额度、不看时效、不算开过口；回执在这里规整成「谁、什么时候、
//! 什么」，字段名与单位都是给人读的。

use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};

/// 一轮最多读几次。翻文件夹、翻页都算一次。
pub(crate) const READS: usize = 8;

/// 群文件一页列几条；按名搜最多给几条。
const FILE_PAGE: i64 = 60;
const SEARCH_ROWS: i64 = 40;
/// 群精华一页几条。
const ESSENCE_PAGE: i64 = 20;
/// 群公告一次读几条。
const BULLETINS: i64 = 10;
/// 群荣誉每一类列前几名。
const HONOR_ROWS: i64 = 5;
/// 单条精华 / 公告正文截到多少字。
const TEXT_CHARS: usize = 800;

/// 这一次要读什么。
#[derive(Debug, PartialEq)]
pub(crate) enum Query {
    /// 列一个文件夹（`folder` 空串是根目录），从第 `start` 条起。
    Files { folder: String, start: i64 },
    /// 按文件名搜整个群的文件。
    Search { keyword: String },
    /// 群精华第 `page` 页（从 0 起）。
    Essences { page: i64 },
    Bulletins,
    /// 群荣誉榜：龙王、群聊之火、群聊炽焰、冒尖小春笋、快乐源泉（知弦 0.52.0 起）。
    Honors,
}

impl Query {
    pub(crate) fn parse(request: &Value) -> Result<Self> {
        let text = |key: &str| {
            request[key]
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        let next = request["next"].as_i64().unwrap_or(0).max(0);
        Ok(match request["what"].as_str().unwrap_or("").trim() {
            "files" | "file" => match text("keyword") {
                Some(keyword) => Self::Search { keyword },
                None => Self::Files {
                    folder: text("folder_id").unwrap_or_default(),
                    start: next,
                },
            },
            "essences" | "essence" => Self::Essences { page: next },
            "bulletins" | "bulletin" => Self::Bulletins,
            "honors" | "honor" => Self::Honors,
            other => bail!("what 只认 files / essences / bulletins / honors，收到「{other}」"),
        })
    }

    /// 实现端的方法名与参数。
    pub(crate) fn rpc(&self, guild: &str) -> (&'static str, Value) {
        match self {
            Self::Files { folder, start } => {
                let folder = if folder.is_empty() { "/" } else { folder.as_str() };
                (
                    "internal/group_file_list",
                    json!({"guild_id":guild,"folder_id":folder,"start_index":start,"limit":FILE_PAGE}),
                )
            }
            Self::Search { keyword } => (
                "internal/group_file_search",
                json!({"guild_id":guild,"word":keyword,"count":SEARCH_ROWS}),
            ),
            Self::Essences { page } => (
                "internal/group_essence_list",
                json!({"guild_id":guild,"page":page,"page_limit":ESSENCE_PAGE}),
            ),
            Self::Bulletins => (
                "internal/group_bulletin_get",
                json!({"guild_id":guild,"count":BULLETINS}),
            ),
            Self::Honors => (
                "internal/group_honor",
                json!({"guild_id":guild,"kind":"all","limit":HONOR_ROWS}),
            ),
        }
    }

    /// 实现端的回包 → 给模型读的回执。
    pub(crate) fn render(&self, data: &Value) -> Result<Value> {
        match self {
            Self::Files { folder, start } => files(data, folder, *start),
            Self::Search { keyword } => search(data, keyword),
            Self::Essences { page } => essences(data, *page),
            Self::Bulletins => bulletins(data),
            Self::Honors => honors(data),
        }
    }
}

/// 实现端报错时换一句模型能转告的话。知弦 0.51.0 以前精华与公告一律取不到网页凭据。
pub(crate) fn explain(error: &anyhow::Error) -> anyhow::Error {
    let text = format!("{error:#}");
    if text.contains("guest ticket") || text.contains("pskey") {
        anyhow::anyhow!(
            "QQ 端没取到读群精华 / 群公告要的网页凭据（{text}）。知弦 0.51.0 起修好了，需要号主更新知弦并重启手机；这一轮别重试，如实告诉对方现在读不到"
        )
    } else {
        anyhow::anyhow!("{text}")
    }
}

fn files(data: &Value, folder: &str, start: i64) -> Result<Value> {
    let rows = data["items"].as_array().cloned().unwrap_or_default();
    // 知弦 0.51.0 以前列表恒为空、也不说走的哪条路：别让模型把「没回包」当成「没有文件」。
    if rows.is_empty() && data.get("source").is_none() && data["all_file_count"].as_i64() == Some(-1) {
        bail!(
            "QQ 端没回群文件列表（知弦 0.51.0 以前的已知问题，号主更新知弦并重启手机后就好）。现在可以先用 keyword 按文件名搜，搜索不受影响"
        );
    }
    let items: Vec<Value> = rows.iter().map(entry).collect();
    let mut out = json!({
        "what": "files",
        "folder": if folder.is_empty() { "/" } else { folder },
        "count": items.len(),
        "items": items,
    });
    let next = data["next_index"].as_i64().unwrap_or(0);
    if !data["is_end"].as_bool().unwrap_or(true) && next > start {
        out["next"] = json!(next);
    }
    if let Some(total) = data["all_file_count"].as_i64().filter(|total| *total >= 0) {
        out["total"] = json!(total);
    }
    if out["count"] == 0 {
        out["note"] = json!("这个文件夹是空的");
    }
    Ok(out)
}

fn search(data: &Value, keyword: &str) -> Result<Value> {
    if data["timeout"].as_bool() == Some(true) {
        bail!("QQ 端搜群文件没回包（超时），换个更具体的关键词再试一次，或者直接列文件夹");
    }
    let body = if data.get("data").is_some() { &data["data"] } else { data };
    ensure!(
        data["ok"].as_bool() != Some(false),
        "搜群文件失败：{}",
        data["msg"].as_str().unwrap_or("QQ 端没说原因")
    );
    let items: Vec<Value> = body["item"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| {
            let mut row = row.clone();
            row["type"] = json!("file");
            entry(&row)
        })
        .collect();
    let mut out = json!({
        "what": "files",
        "keyword": keyword,
        "count": items.len(),
        "items": items,
        "note": "按文件名模糊匹配，跨所有文件夹；匹配可能宽，自己按文件名再筛一遍",
    });
    if let Some(total) = body["totalMatchCount"].as_i64() {
        out["matched"] = json!(total);
    }
    Ok(out)
}

/// 一个文件或文件夹（字段名是内核 `GroupFileInfo` / `GroupFolderInfo` 的）。
fn entry(row: &Value) -> Value {
    let int = |key: &str| row[key].as_i64().or_else(|| row[key].as_str().and_then(|text| text.parse().ok()));
    let text = |key: &str| row[key].as_str().unwrap_or("").to_string();
    let person = |name: String, id: Option<i64>| match (name.is_empty(), id.filter(|id| *id > 0)) {
        (false, Some(id)) => format!("{name}（{id}）"),
        (false, None) => name,
        (true, Some(id)) => id.to_string(),
        (true, None) => String::new(),
    };
    let folder = row["type"].as_str() == Some("folder")
        || (row.get("folderId").is_some() && row.get("fileId").is_none());
    if folder {
        let mut out = json!({
            "type": "folder",
            "folder_id": text("folderId"),
            "name": text("folderName"),
            "creator": person(text("creatorName"), int("createUin")),
            "created": when(int("createTime").unwrap_or(0)),
        });
        if let Some(count) = int("totalFileCount") {
            out["files"] = json!(count);
        }
        return out;
    }
    let mut out = json!({
        "type": "file",
        "file_id": text("fileId"),
        "name": text("fileName"),
        "size": size(int("fileSize").unwrap_or(0)),
        "uploader": person(text("uploaderName"), int("uploaderUin")),
        "uploaded": when(int("uploadTime").unwrap_or(0)),
    });
    if let Some(downloads) = int("downloadTimes").filter(|count| *count > 0) {
        out["downloads"] = json!(downloads);
    }
    // 临时文件到期就没了；0 是永久。
    if let Some(dead) = int("deadTime").filter(|time| *time > 0) {
        out["expires"] = json!(when(dead));
    }
    out
}

fn essences(data: &Value, page: i64) -> Result<Value> {
    ensure!(
        data["ok"].as_bool() != Some(false),
        "读群精华失败：{}",
        data["error"].as_str().unwrap_or("QQ 端没说原因")
    );
    let mut budget = 12_000usize;
    let mut items = Vec::new();
    for row in data["essences"].as_array().into_iter().flatten() {
        let text = clip(row["text"].as_str().unwrap_or(""), TEXT_CHARS);
        budget = budget.saturating_sub(text.chars().count());
        if budget == 0 {
            break;
        }
        let mut item = json!({
            "sender": named(row["sender_name"].as_str(), row["sender_id"].as_str()),
            "sent": when(row["sent_at"].as_i64().unwrap_or(0)),
            "text": text,
            "added_by": named(row["operator_name"].as_str(), row["operator_id"].as_str()),
            "added": when(row["added_at"].as_i64().unwrap_or(0)),
        });
        if let Some(images) = row["images"].as_array().filter(|images| !images.is_empty()) {
            item["images"] = json!(images.len());
        }
        items.push(item);
    }
    let mut out = json!({"what":"essences","page":page,"count":items.len(),"items":items});
    if !data["is_end"].as_bool().unwrap_or(true) {
        out["next"] = json!(page + 1);
    }
    if out["count"] == 0 && page == 0 {
        out["note"] = json!("本群还没有精华消息");
    }
    Ok(out)
}

fn bulletins(data: &Value) -> Result<Value> {
    ensure!(
        data["ok"].as_bool() != Some(false),
        "读群公告失败：{}",
        data["error"].as_str().unwrap_or("QQ 端没说原因")
    );
    let items: Vec<Value> = data["bulletins"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| {
            let mut item = json!({
                "author": row["sender_id"].as_str().unwrap_or(""),
                "published": when(row["published_at"].as_i64().unwrap_or(0)),
                "text": clip(row["text"].as_str().unwrap_or(""), TEXT_CHARS * 2),
            });
            if let Some(title) = row["title"].as_str().map(str::trim).filter(|title| !title.is_empty()) {
                item["title"] = json!(title);
            }
            if row["pinned"].as_bool() == Some(true) {
                item["pinned"] = json!(true);
            }
            if let Some(read) = row["read_count"].as_i64().filter(|count| *count > 0) {
                item["read"] = json!(read);
            }
            item
        })
        .collect();
    let mut out = json!({"what":"bulletins","count":items.len(),"items":items});
    if out["count"] == 0 {
        out["note"] = json!("本群没有公告");
    }
    Ok(out)
}

/// 实现端按类别给（龙王、群聊之火……），这里压成「哪一类、谁、几天」。
fn honors(data: &Value) -> Result<Value> {
    if data.get("honors").is_none() {
        // 老知弦没有这个动作：实现端回 404 时走不到这里，这一句兜的是回包形状不对。
        bail!("QQ 端没给群荣誉榜（要知弦 0.52.0 起）");
    }
    ensure!(
        data["ok"].as_bool() != Some(false),
        "读群荣誉失败：{}",
        data["honors"]["talkative"]["error"].as_str().unwrap_or("QQ 端没说原因")
    );
    let mut items = Vec::new();
    for kind in ["talkative", "performer", "legend", "newbie", "emotion"] {
        let honor = &data["honors"][kind];
        if honor.is_null() || honor.get("error").is_some() {
            continue;
        }
        let people: Vec<Value> = honor["list"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|row| {
                let mut person = json!({
                    "who": named(row["name"].as_str(), row["user_id"].as_str()),
                    "days": row["days"].as_i64().unwrap_or(0),
                });
                if let Some(total) = row["total_days"].as_i64() {
                    person["total_days"] = json!(total);
                }
                person
            })
            .collect();
        let mut item = json!({"honor": honor["title"].as_str().unwrap_or(kind), "people": people});
        if let Some(current) = honor.get("current").filter(|current| current.is_object()) {
            item["current"] = json!(named(current["name"].as_str(), current["user_id"].as_str()));
        }
        items.push(item);
    }
    let mut out = json!({"what":"honors","items":items});
    if items_empty(&out) {
        out["note"] = json!("本群眼下没人上榜");
    }
    Ok(out)
}

fn items_empty(out: &Value) -> bool {
    out["items"]
        .as_array()
        .is_none_or(|items| items.iter().all(|item| item["people"].as_array().is_none_or(Vec::is_empty)))
}

fn named(name: Option<&str>, id: Option<&str>) -> String {
    match (name.map(str::trim).unwrap_or(""), id.map(str::trim).unwrap_or("")) {
        ("", id) => id.to_string(),
        (name, "") => name.to_string(),
        (name, id) => format!("{name}（{id}）"),
    }
}

fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push_str("…（后略）");
    out
}

/// Unix 秒 → 北京时间「2026-10-12 08:30」；0 是没有。
fn when(seconds: i64) -> String {
    if seconds <= 0 {
        return String::new();
    }
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|time| {
            time.with_timezone(&crate::clock::beijing())
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

fn size(bytes: i64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes.max(0) as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_render_people_sizes_and_paging() {
        let data = json!({
            "items": [
                {"type":"folder","folderId":"/abc","folderName":"资料","createUin":10001,"creatorName":"甲","createTime":1700000000,"totalFileCount":3},
                {"type":"file","fileId":"/f1","fileName":"手册.pdf","fileSize":3_733_642,"uploaderName":"乙","uploaderUin":10002,"uploadTime":1677207012,"downloadTimes":5,"deadTime":0}
            ],
            "is_end": false, "next_index": 60, "all_file_count": -1, "source": "oidb"
        });
        let out = Query::Files { folder: String::new(), start: 0 }.render(&data).unwrap();
        assert_eq!(out["items"][0]["folder_id"], "/abc");
        assert_eq!(out["items"][0]["files"], 3);
        assert_eq!(out["items"][1]["size"], "3.6 MB");
        assert_eq!(out["items"][1]["uploader"], "乙（10002）");
        assert_eq!(out["items"][1]["uploaded"], "2023-02-24 10:50");
        assert_eq!(out["next"], 60);
        assert!(out.get("total").is_none());
    }

    #[test]
    fn old_empty_list_is_an_error_not_an_empty_group() {
        let data = json!({"group":1,"folder_id":"","all_file_count":-1,"is_end":true,"next_index":0,"items":[]});
        assert!(Query::Files { folder: String::new(), start: 0 }.render(&data).is_err());
    }

    #[test]
    fn search_reads_kernel_payload() {
        let data = json!({"ok":true,"code":0,"data":{"item":[{"fileId":"/f","fileName":"a.zip","fileSize":1000,"uploaderName":"丙","uploaderUin":3,"uploadTime":0}],"totalMatchCount":9}});
        let out = Query::Search { keyword: "a".into() }.render(&data).unwrap();
        assert_eq!(out["items"][0]["type"], "file");
        assert_eq!(out["items"][0]["size"], "1000 B");
        assert_eq!(out["matched"], 9);
    }

    #[test]
    fn essences_and_bulletins() {
        let data = json!({"ok":true,"is_end":false,"essences":[{"sender_id":"1","sender_name":"丁","sent_at":1700000000,"operator_id":"2","operator_name":"戊","added_at":1700000100,"text":"名言","images":["u"]}]});
        let out = Query::Essences { page: 0 }.render(&data).unwrap();
        assert_eq!(out["items"][0]["sender"], "丁（1）");
        assert_eq!(out["items"][0]["images"], 1);
        assert_eq!(out["next"], 1);
        let data = json!({"ok":true,"bulletins":[{"sender_id":"9","published_at":1700000000,"title":"","text":"群规","pinned":true,"read_count":0}]});
        let out = Query::Bulletins.render(&data).unwrap();
        assert_eq!(out["items"][0]["pinned"], true);
        assert!(out["items"][0].get("title").is_none());
        let failed = json!({"ok":false,"error":"http=200 code=4"});
        assert!(Query::Bulletins.render(&failed).is_err());
    }

    #[test]
    fn honors_flatten_by_category() {
        let data = json!({"ok":true,"group":1,"honors":{
            "talkative":{"title":"龙王","current":{"user_id":"7","name":"台风","days":6},
                "list":[{"user_id":"7","name":"台风","days":6,"total_days":193}]},
            "performer":{"title":"群聊之火","list":[{"user_id":"8","name":"橘子","days":27}]},
            "emotion":{"title":"快乐源泉","error":"http=200 code=1001"}}});
        let out = Query::Honors.render(&data).unwrap();
        assert_eq!(out["items"].as_array().unwrap().len(), 2, "出错的类别不列");
        assert_eq!(out["items"][0]["current"], "台风（7）");
        assert_eq!(out["items"][0]["people"][0]["total_days"], 193);
        assert_eq!(out["items"][1]["honor"], "群聊之火");
        assert_eq!(Query::parse(&json!({"what":"honors"})).unwrap(), Query::Honors);
    }

    #[test]
    fn parse_routes_keyword_to_search() {
        let query = Query::parse(&json!({"what":"files","keyword":" 手册 "})).unwrap();
        assert_eq!(query, Query::Search { keyword: "手册".into() });
        let query = Query::parse(&json!({"what":"essences","next":2})).unwrap();
        assert_eq!(query, Query::Essences { page: 2 });
        assert!(Query::parse(&json!({"what":"members"})).is_err());
    }
}
