#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use simd_json::owned::{Object, Value};

/// 消息段 (Segment)
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Segment {
    #[serde(rename = "type")]
    pub type_: String,
    pub data: Object,
}

impl Segment {
    pub fn new(type_: &str, data: Object) -> Self {
        Self {
            type_: type_.to_string(),
            data,
        }
    }
}

/// 消息链 (Message Chain)
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct Message(pub Vec<Segment>);

impl Message {
    pub fn new() -> Self {
        Self::default()
    }

    /// 通用添加方法：手动构建 Segment
    pub fn add(mut self, type_: &str, data: Object) -> Self {
        self.0.push(Segment::new(type_, data));
        self
    }

    // ================== 基础文本类 ==================

    /// 纯文本
    pub fn text(self, text: impl Into<String>) -> Self {
        let mut data = Object::new();
        data.insert("text".into(), Value::from(text.into()));
        self.add("text", data)
    }

    /// QQ 表情 (ID)
    pub fn face(self, id: impl ToString) -> Self {
        let mut data = Object::new();
        data.insert("id".into(), Value::from(id.to_string()));
        self.add("face", data)
    }

    /// Markdown (仅限双层合并转发内使用)
    pub fn markdown(self, content: impl Into<String>) -> Self {
        let mut data = Object::new();
        data.insert("content".into(), Value::from(content.into()));
        self.add("markdown", data)
    }

    /// Json 消息
    pub fn json(self, json_string: impl Into<String>) -> Self {
        let mut data = Object::new();
        data.insert("data".into(), Value::from(json_string.into()));
        self.add("json", data)
    }

    // ================== 媒体资源类 ==================

    /// 图片
    /// - `file`: 图片文件名、URL、Base64 或文件路径
    pub fn image(self, file: impl Into<String>) -> Self {
        let mut data = Object::new();
        data.insert("file".into(), Value::from(file.into()));
        self.add("image", data)
    }

    /// 信息图片带一句描述，供记录与读屏识别；出图失败时才改发等价文本。
    pub fn image_described(self, file: impl Into<String>, description: impl Into<String>) -> Self {
        let mut data = Object::new();
        data.insert("file".into(), Value::from(file.into()));
        data.insert("summary".into(), Value::from(description.into()));
        self.add("image", data)
    }

    /// 语音
    /// - `file`: 文件名、URL、Base64 或文件路径
    pub fn record(self, file: impl Into<String>) -> Self {
        let mut data = Object::new();
        data.insert("file".into(), Value::from(file.into()));
        self.add("record", data)
    }

    /// 视频
    /// - `file`: 文件名、URL、Base64 或文件路径
    pub fn video(self, file: impl Into<String>) -> Self {
        let mut data = Object::new();
        data.insert("file".into(), Value::from(file.into()));
        self.add("video", data)
    }

    /// 文件
    /// - `file`: 文件路径/URL
    /// - `name`: (可选) 显示的文件名
    pub fn file(self, file: impl Into<String>, name: Option<impl Into<String>>) -> Self {
        let mut data = Object::new();
        data.insert("file".into(), Value::from(file.into()));
        if let Some(n) = name {
            data.insert("name".into(), Value::from(n.into()));
        }
        self.add("file", data)
    }

    /// 商城表情包 (Market Face)
    pub fn mface(
        self,
        emoji_id: impl ToString,
        emoji_package_id: impl ToString,
        key: impl Into<String>,
    ) -> Self {
        let mut data = Object::new();
        data.insert("emoji_id".into(), Value::from(emoji_id.to_string()));
        data.insert(
            "emoji_package_id".into(),
            Value::from(emoji_package_id.to_string()),
        );
        data.insert("key".into(), Value::from(key.into()));
        self.add("mface", data)
    }

    // ================== 互动/艾特类 ==================

    /// @某人
    pub fn at(self, user_id: impl ToString) -> Self {
        let mut data = Object::new();
        data.insert("qq".into(), Value::from(user_id.to_string()));
        self.add("at", data)
    }

    /// @全体成员。实现端把它发成 Satori 的 `<at type="all"/>`；没有权限或今天的次数用完，
    /// 平台会把它当成普通文字或干脆拒发——先问 [`at_all_quota`](crate::adapters::satori::api::at_all_quota)，
    /// 或者直接用 [`send_at_all`](crate::adapters::satori::api::send_at_all)。
    pub fn at_all(self) -> Self {
        self.at(AT_ALL)
    }

    /// 这条消息里 @ 了谁。
    pub fn mentions(&self) -> Mentions {
        let mut mentions = Mentions::default();
        for segment in self.0.iter().filter(|segment| segment.type_ == "at") {
            if let Some(id) = segment.data.get("qq").and_then(id_text) {
                mentions.note(id);
            }
        }
        mentions
    }

    /// 回复消息
    pub fn reply(self, message_id: impl ToString) -> Self {
        let mut data = Object::new();
        data.insert("id".into(), Value::from(message_id.to_string()));
        self.add("reply", data)
    }

    /// 群聊戳一戳
    pub fn poke(self, user_id: impl ToString) -> Self {
        let mut data = Object::new();
        data.insert("qq".into(), Value::from(user_id.to_string()));
        self.add("poke", data)
    }

    /// 猜拳魔法表情 (发送时通常不需要参数，或指定结果)
    pub fn rps(self) -> Self {
        self.add("rps", Object::new())
    }

    /// 骰子魔法表情
    pub fn dice(self) -> Self {
        self.add("dice", Object::new())
    }

    // ================== 分享/推荐类 ==================

    /// 推荐好友
    pub fn contact_user(self, user_id: impl ToString) -> Self {
        let mut data = Object::new();
        data.insert("type".into(), Value::from("qq"));
        data.insert("id".into(), Value::from(user_id.to_string()));
        self.add("contact", data)
    }

    /// 推荐群聊
    pub fn contact_group(self, group_id: impl ToString) -> Self {
        let mut data = Object::new();
        data.insert("type".into(), Value::from("group"));
        data.insert("id".into(), Value::from(group_id.to_string()));
        self.add("contact", data)
    }

    /// 小程序卡片 (调用 get_mini_app_ark 接口)
    pub fn lightapp(self, json_content: impl Into<String>) -> Self {
        let mut data = Object::new();
        data.insert("content".into(), Value::from(json_content.into()));
        self.add("lightapp", data)
    }

    // ================== 音乐分享类 ==================

    /// 现有音源分享 (QQ/网易云等)
    /// - `platform`: "qq", "163", "kugou", "migu", "kuwo"
    /// - `id`: 歌曲 ID
    pub fn music(self, platform: impl Into<String>, id: impl ToString) -> Self {
        let mut data = Object::new();
        data.insert("type".into(), Value::from(platform.into()));
        data.insert("id".into(), Value::from(id.to_string()));
        self.add("music", data)
    }

    /// 自定义音乐分享
    pub fn music_custom(
        self,
        url: impl Into<String>,
        audio: impl Into<String>,
        title: impl Into<String>,
        image: Option<impl Into<String>>,
        singer: Option<impl Into<String>>,
    ) -> Self {
        let mut data = Object::new();
        data.insert("type".into(), Value::from("custom"));
        data.insert("url".into(), Value::from(url.into()));
        data.insert("audio".into(), Value::from(audio.into()));
        data.insert("title".into(), Value::from(title.into()));

        if let Some(img) = image {
            data.insert("image".into(), Value::from(img.into()));
        }
        if let Some(s) = singer {
            data.insert("singer".into(), Value::from(s.into()));
        }
        self.add("music", data)
    }

    // ================== 转发节点类 ==================

    /// 转发消息节点 - 引用现有消息
    /// - `id`: 消息 ID
    pub fn node(self, id: impl ToString) -> Self {
        let mut data = Object::new();
        data.insert("id".into(), Value::from(id.to_string()));
        self.add("node", data)
    }

    /// 转发消息节点 - 自定义内容 (伪造消息)
    /// - `user_id`: 发送者 QQ
    /// - `nickname`: 发送者昵称
    /// - `content`: 消息内容 (Message 链)
    pub fn node_custom(
        self,
        user_id: impl ToString,
        nickname: impl Into<String>,
        content: Message,
    ) -> Self {
        let mut data = Object::new();
        data.insert("user_id".into(), Value::from(user_id.to_string()));
        data.insert("nickname".into(), Value::from(nickname.into()));

        // 将 Message 转换为 simd_json 的 Value 数组
        let content_array: Vec<Value> = content
            .0
            .into_iter()
            .map(|seg| {
                let mut seg_obj = Object::new();
                seg_obj.insert("type".into(), Value::from(seg.type_));
                seg_obj.insert("data".into(), Value::from(seg.data));
                Value::from(seg_obj)
            })
            .collect();

        data.insert("content".into(), Value::from(content_array));

        self.add("node", data)
    }
}

/// 消息段里「@全体成员」的写法：`{"type":"at","data":{"qq":"all"}}`，收发同一种。
/// 与 Satori 的 `<at type="all"/>` 互换在适配器里做（两个实现端都这样认）。
pub const AT_ALL: &str = "all";

/// 一条消息提到了谁：被 @ 的账号与有没有 @全体成员。插件别各自去翻 `at` 段，
/// 也别把 [`AT_ALL`] 当成一个账号 ID 去查资料、拼头像。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mentions {
    /// 被 @ 的账号，按出现顺序去重；不含 [`AT_ALL`]。
    pub users: Vec<String>,
    /// 有没有 @全体成员。
    pub all: bool,
}

impl Mentions {
    /// 从入站消息段（事件里的 `message` 数组）读出提及。
    pub fn of(segments: &simd_json::OwnedValue) -> Self {
        use simd_json::base::ValueAsArray;
        use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsScalar};
        let mut mentions = Self::default();
        for segment in segments.as_array().into_iter().flatten() {
            if segment.get_str("type") != Some("at") {
                continue;
            }
            if let Some(id) = segment
                .get("data")
                .and_then(|data| data.get("qq"))
                .and_then(id_text)
            {
                mentions.note(id);
            }
        }
        mentions
    }

    /// 某个账号是否被点名 @ 了（@全体成员 不算点名）。
    pub fn names(&self, user_id: &str) -> bool {
        self.users.iter().any(|user| user == user_id)
    }

    pub fn is_empty(&self) -> bool {
        self.users.is_empty() && !self.all
    }

    fn note(&mut self, id: String) {
        if id.eq_ignore_ascii_case(AT_ALL) {
            self.all = true;
        } else if !id.is_empty() && !self.names(&id) {
            self.users.push(id);
        }
    }
}

/// `qq` 字段可能是字符串，也可能是数字。
fn id_text(value: &Value) -> Option<String> {
    use simd_json::base::ValueAsScalar;
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_u64().map(|id| id.to_string()))
        .or_else(|| value.as_i64().map(|id| id.to_string()))
}

// 允许直接从字符串字面量转换为纯文本消息
impl From<&str> for Message {
    fn from(s: &str) -> Self {
        Message::new().text(s)
    }
}

impl From<String> for Message {
    fn from(s: String) -> Self {
        Message::new().text(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_split_all_from_users() {
        let segments = simd_json::json!([
            {"type": "at", "data": {"qq": "all"}},
            {"type": "at", "data": {"qq": 10001}},
            {"type": "text", "data": {"text": " 开会"}},
            {"type": "at", "data": {"qq": "10001"}},
        ]);
        let mentions = Mentions::of(&segments);
        assert!(mentions.all);
        assert_eq!(mentions.users, ["10001"]);
        assert_eq!(Message::new().at_all().at(7).mentions().users, ["7"]);
    }
}
