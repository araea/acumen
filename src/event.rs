#![allow(dead_code)]

use crate::config::AppConfig;
use crate::matcher::Matcher;
use crate::scheduler::Scheduler;
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use simd_json::OwnedValue;
use simd_json::base::ValueAsScalar;
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsScalar};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::Mutex as AsyncMutex;

pub type Event = OwnedValue;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LoginUser {
    pub id: String,
    pub name: Option<String>,
    pub nick: Option<String>,
    pub avatar: Option<String>,
}

/// 登录账号的共享单元。
///
/// 实现端在连接期间可以用 `login-updated` 重新认定账号，而插件各处都按它判断「这条是不是
/// 自己发的」，所以账号不能是 `READY` 时定死的快照。`Clone` 共享同一把锁，读到的一直是当前值。
#[derive(Debug, Clone)]
pub struct SharedLogin(Arc<RwLock<Arc<LoginUser>>>);

impl SharedLogin {
    pub fn new(user: LoginUser) -> Self {
        Self(Arc::new(RwLock::new(Arc::new(user))))
    }

    /// 取当前账号。返回 `Arc` 是为了让调用方在解锁之后继续用，避免读锁跨过 await。
    pub fn get(&self) -> Arc<LoginUser> {
        self.0.read().unwrap().clone()
    }

    pub fn set(&self, user: LoginUser) {
        *self.0.write().unwrap() = Arc::new(user);
    }
}

impl Default for SharedLogin {
    fn default() -> Self {
        Self::new(LoginUser::default())
    }
}

impl From<LoginUser> for SharedLogin {
    fn from(user: LoginUser) -> Self {
        Self::new(user)
    }
}

#[derive(Debug, Clone, Default)]
pub struct BotStatus {
    pub adapter: String,
    pub platform: String,
    pub login_user: SharedLogin,
}

impl BotStatus {
    /// 自己的账号 ID（Satori `login.user.id`），和事件里的 `user_id` 可以直接比较。
    pub fn self_id(&self) -> String {
        self.login_user.get().id.clone()
    }
}

/// 统一的上下文，包含事件数据、可变配置和任务调度器
/// 注意：event 字段直接持有 EventType，支持在插件链中移交所有权从而实现修改。
/// Context 实现了 Clone（因为 EventType 包含的 simd_json::OwnedValue 实现了 Clone），
/// 但在插件流水线中通常通过 Move 传递，避免了 Deep Copy。
///
/// 优化：`config_path` 与 `bot` 改为 Arc 共享，避免每事件克隆字符串与 BotStatus 内部多个 String。
#[derive(Clone)]
pub struct Context {
    pub event: EventType, // 直接持有，不再使用 Arc
    pub config: Arc<RwLock<AppConfig>>,
    pub config_save_lock: Arc<AsyncMutex<()>>,
    pub db: DatabaseConnection,
    pub scheduler: Arc<Scheduler>,
    pub matcher: Arc<Matcher>,
    pub config_path: Arc<str>,
    pub bot: Arc<BotStatus>,
}

impl Context {
    /// 尝试将当前事件视为规范化后的 Satori 消息事件
    pub fn as_message(&self) -> Option<MessageEvent<'_>> {
        if let EventType::Satori(event) = &self.event {
            let view = GeneralEventView(event);
            if view.post_type() == Some("message") {
                return Some(MessageEvent(event));
            }
        }
        None
    }

    /// 全局配置里的浏览器路径；`None` 表示自动查找。
    pub fn browser_path(&self) -> Option<String> {
        self.config.read().unwrap().browser_path.clone()
    }

    /// 这个群是否通过全局黑白名单。收到的事件、定时推送、资讯推送共用这一个口径
    /// （见 [`GlobalFilterConfig::allows`](crate::config::GlobalFilterConfig::allows)）。
    pub fn group_allowed(&self, group_id: &str) -> bool {
        self.config.read().unwrap().global_filter.allows(group_id)
    }

    /// 获取规范化事件的 post_type
    pub fn post_type(&self) -> Option<&str> {
        if let EventType::Satori(event) = &self.event {
            GeneralEventView(event).post_type()
        } else {
            None
        }
    }

    /// 等待特定条件的用户输入 (交互式操作)
    pub async fn wait_input(
        &self,
        group_id: Option<&str>,
        user_id: Option<&str>,
        timeout: Duration,
    ) -> Option<Event> {
        self.matcher.wait(group_id, user_id, timeout).await
    }
}

// ================== 事件封装工具 ==================

/// 通用事件视图，用于快速访问基础字段
pub struct GeneralEventView<'a>(&'a Event);

impl<'a> GeneralEventView<'a> {
    /// 获取 post_type，返回的引用生命周期绑定到原始 Event ('a)
    pub fn post_type(&self) -> Option<&'a str> {
        self.0.get_str("post_type")
    }
}

/// 消息事件封装，提供便捷的强类型访问
pub struct MessageEvent<'a>(pub &'a Event);

impl<'a> MessageEvent<'a> {
    /// 群号（群消息才有）。与 Satori 一致，ID 一律是字符串。
    pub fn group_id(&self) -> Option<&'a str> {
        self.0.get_str("group_id").filter(|id| !id.is_empty())
    }

    /// 发送者 ID。
    pub fn user_id(&self) -> &'a str {
        self.0.get_str("user_id").unwrap_or("")
    }

    /// 消息 ID。
    pub fn message_id(&self) -> &'a str {
        self.0.get_str("message_id").unwrap_or("")
    }

    /// 获取纯文本内容 (raw_message)
    pub fn text(&self) -> &'a str {
        self.0.get_str("raw_message").unwrap_or("")
    }

    /// 是否为群消息
    pub fn is_group(&self) -> bool {
        self.0.get_str("message_type") == Some("group")
    }

    /// 获取发送者昵称
    pub fn sender_nickname(&self) -> Option<&'a str> {
        self.0.get("sender").and_then(|s| s.get_str("nickname"))
    }

    /// 获取发送者群名片 (如果为空则返回 None)
    pub fn sender_card(&self) -> Option<&'a str> {
        self.0
            .get("sender")
            .and_then(|s| s.get_str("card"))
            .filter(|s| !s.is_empty())
    }

    /// 获取发送者显示名称 (优先名片，其次昵称)
    pub fn sender_name(&self) -> &'a str {
        self.sender_card()
            .or_else(|| self.sender_nickname())
            .unwrap_or("Unknown")
    }

    /// 获取发送者角色 (owner, admin, member)
    pub fn sender_role(&self) -> Option<&'a str> {
        self.0.get("sender").and_then(|s| s.get_str("role"))
    }

    /// 频道 ID（Satori `channel.id`）。私聊里它是对方的会话，不是自己的号。
    pub fn channel_id(&self) -> &'a str {
        self.0.get_str("channel_id").unwrap_or("")
    }

    /// 是否为号主自己手发的消息（与机器人自己发出去的回声相对）。
    ///
    /// satori-qq 为 QQ 客户端手发的消息补发自发事件，satori-wx 给微信里手发的消息打
    /// 同样的标记。这类事件仍然允许进入插件流水线（便于机器人账号本人调用指令），但不能对其
    /// 执行 reaction API；satori-qq 使用的虚拟 `qq-client:*` 作者会让表情落错目标。
    pub fn is_manual_self(&self) -> bool {
        self.0.get_bool("manual_self").unwrap_or(false)
            || ["satori_qq", "satori_wx"].into_iter().any(|extension| {
                self.0
                    .get("_satori")
                    .and_then(|value| value.get(extension))
                    .and_then(|value| value.get("manual_self"))
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false)
            })
    }
}

// ================== 基础结构定义 ==================

/// 事件类型
#[derive(Debug, Clone)]
pub enum EventType {
    /// 来自 Satori 的规范化事件
    Satori(Event),
    /// 插件准备发送消息前的拦截事件
    BeforeSend(SendPacket),
    /// 系统初始化事件 (用于插件 on_init 生命周期)
    Init,
}

/// 发送前的最后一道闸：发送包过完 `BeforeSend` 之后、真正交给实现端之前再问一次。
///
/// 「条件发送」的插件（复读：接力断了就不再跟读）给发送包挂一个，适配器只认这个接口，
/// 不认具体是哪个插件。发出之后要补的记账由该插件的 `on_sent` 钩子做，用 [`as_any`]
/// 取回自己的具体类型。
///
/// [`as_any`]: SendGuard::as_any
pub trait SendGuard: std::fmt::Debug + Send + Sync + 'static {
    /// 此刻这条消息还该不该发
    fn is_current(&self) -> bool;
    /// 交给实现端一并判断的时效条件：排队期间群里又说了话，同样不落地
    fn freshness(&self) -> Option<crate::adapters::satori::Freshness>;
    /// 取回具体类型
    fn as_any(&self) -> &dyn std::any::Any;
}

/// 发送包结构，用于在 BeforeSend 中传递
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SendPacket {
    pub action: String,
    /// 条件发送的闸，见 [`SendGuard`]
    #[serde(skip)]
    pub guard: Option<Arc<dyn SendGuard>>,
    /// 可选的消息时效条件：群聊已经往前走了就干脆不发。见
    /// [`crate::adapters::satori::Freshness`]。
    #[serde(skip)]
    pub freshness: Option<crate::adapters::satori::Freshness>,
    pub params: OwnedValue,
    /// 原始触发事件（不参与序列化发送给 Bot）
    #[serde(skip)]
    pub original_event: Option<Event>,
    /// `message.create` 成功后由适配器写入实际消息 ID，供需要建立引用关系的
    /// 调用方读取。使用共享容器是因为发送包会经过多个插件并被克隆。
    #[serde(skip)]
    pub receipt_message_ids: Arc<Mutex<Vec<String>>>,
}

impl SendPacket {
    /// 目标群号
    pub fn group_id(&self) -> Option<&str> {
        self.params.get_str("group_id").filter(|id| !id.is_empty())
    }

    /// 目标用户（私聊）
    pub fn user_id(&self) -> Option<&str> {
        self.params.get_str("user_id").filter(|id| !id.is_empty())
    }

    /// 获取 message 字段的 Value
    pub fn message(&self) -> Option<&OwnedValue> {
        self.params.get("message")
    }

    /// 获取消息类型字符串，返回 Option，若不存在则返回 None
    pub fn message_type(&self) -> Option<&str> {
        self.params.get_str("message_type")
    }
}
