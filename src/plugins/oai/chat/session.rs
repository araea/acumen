//! 一轮群聊行动的工具出口：写操作串行、回执可见、同一请求只执行一次。
//!
//! `satori_*` 工具由执行层转发进来（见 [`crate::plugins::oai::agent::ChatBridge`]），
//! [`Bridge::call`] 直接进 [`Session`]：额度、去重、平台拒绝记账都在这一层。
//! 人格只在这一层之外——需要问一句的地方走 [`super::Persona`]。
use super::{
    ChatConfig, ChatEnv, Persona,
    actions::{self, Action, Part},
    identity, memory, stickers,
    window::{self, Turn, turn_from_platform},
};
use crate::{
    adapters::satori::{LockedWriter, forward, freshness_for, send_fresh_msg_id},
    event::Context,
    message::{Message, Segment},
};
use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

/// `satori_action` 实际接受的动作；也是 [`capability`] 的取值域。
///
/// 人格对「自己能做什么」的认知不该只靠 skill 里那张手写的表——文档会过期，
/// 这份清单和代码一起走，`satori_context` 每次都照它报告。
pub(crate) const ACTION_KINDS: [&str; 15] = [
    "send",
    "poke",
    "like",
    "react",
    "recall",
    "forward",
    "sign",
    "card",
    "title",
    "essence",
    "mute",
    "kick",
    "mute_all",
    "rename_group",
    "react_clear",
];

/// 这一轮的群聊现场从哪里来。
///
/// 两边的用法不一样：搭话要一直听着群聊（自动环境感知，见 [`super::window`]），
/// 房间只在模型伸手要的时候看一眼。两者都落到同一份 [`Turn`] 上，所以下面的工具
/// 实现不必分两套——差别只在「消息不在眼前时怎么办」：搭话的窗口就是它知道的全部，
/// 动手得落在窗口里；房间没有窗口，消息与群友由平台核对。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scene {
    /// 搭话：自己记着的那段常驻窗口。
    Window,
    /// 房间：不跟踪群聊，要看时向平台要最近一页，动手时按消息号现取。
    Channel,
}

/// 搭话的只读探查白名单。参数全部由本群上下文构造，模型不能传入任意方法名、群号或分页游标。
fn observation(kind: &str, group: &str, user_id: &str) -> Result<(&'static str, Value)> {
    let guild = group;
    Ok(match kind {
        "group" => ("guild.get", json!({"guild_id":guild})),
        "member" => ("guild.member.get", json!({"guild_id":guild,"user_id":user_id})),
        "member_card" => ("internal/group_member_card", json!({"guild_id":guild,"user_id":user_id})),
        "group_card" => ("internal/group_profile_card", json!({"guild_id":guild,"fetch_mode":"KFROMCACHE"})),
        "essence" => ("internal/group_essence_list", json!({"guild_id":guild,"page_start":0,"page_limit":5})),
        "title_display" => ("internal/title_display", json!({"guild_id":guild})),
        "honor_display" => ("internal/honor_display", json!({"guild_id":guild})),
        _ => anyhow::bail!("未知环境探查类型：{kind}"),
    })
}

/// 平台明确拒绝过的能力记多久。
///
/// 从前这份记账只活在一轮之内，于是每一轮都要再花掉一次动作额度，去按同一个
/// 腾讯永远不会放行的按钮（资料卡点赞就是这样）。这类拒绝是账号级的、按天算的，
/// 记上几个小时既能省下那次额度，又不会把「后来又放开了」永久锁死。
const REFUSAL_TTL: std::time::Duration = std::time::Duration::from_secs(6 * 3_600);

/// 跨轮的平台拒绝记录：能力键 → （原因，记下的时刻）。
type RefusalLog = HashMap<(String, String), (String, std::time::Instant)>;

fn known_refusals() -> &'static std::sync::Mutex<RefusalLog> {
    static KNOWN: std::sync::OnceLock<std::sync::Mutex<RefusalLog>> = std::sync::OnceLock::new();
    KNOWN.get_or_init(Default::default)
}

fn remember_platform_refusal(scope: &str, capability: &'static str, reason: &str) {
    known_refusals()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert((scope.to_string(), capability.to_string()), (reason.to_string(), std::time::Instant::now()));
}

/// 测试之间要能互不影响：这份记账是进程级的，跑完一个用例得能抹掉。
fn platform_refusal(scope: &str, capability: &str) -> Option<String> {
    let mut guard = known_refusals()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let key = (scope.to_string(), capability.to_string());
    let (reason, at) = guard.get(&key)?;
    if at.elapsed() > REFUSAL_TTL {
        guard.remove(&key);
        return None;
    }
    Some(reason.clone())
}

/// 动作对应的平台能力键；同一能力被拒绝一次，本轮就不必再撞第二次。
fn capability(action: &Action) -> &'static str {
    match action {
        Action::Send { .. } => "send",
        Action::Poke { .. } => "poke",
        Action::Like { .. } => "like",
        Action::React { .. } => "react",
        Action::Recall { .. } => "recall",
        Action::Forward { .. } => "forward",
        Action::Sign => "sign",
        Action::Card { .. } => "card",
        Action::Title { .. } => "title",
        Action::Essence { .. } => "essence",
        Action::Mute { .. } => "mute",
        Action::Kick { .. } => "kick",
        Action::MuteAll { .. } => "mute_all",
        Action::RenameGroup { .. } => "rename_group",
        Action::ReactClear { .. } => "react_clear",
    }
}

/// 一次行动的工具出口。
///
/// `satori_*` 工具直接调进这里，拿到的是与聊天侧同一份上下文；动作、额度、回执
/// 去重都由 [`Session`] 负责，调用方只给 op 和参数。
pub(crate) struct Bridge {
    session: Arc<tokio::sync::Mutex<Session>>,
    attempted: Arc<AtomicBool>,
    seq: Arc<AtomicU64>,
}
impl Bridge {
    /// 走完整信封：`id` 用于回执去重，`op` 取自请求本身。
    ///
    /// 与从前那条 Unix 套接字路径的唯一区别是少了序列化与 token——信封字段的
    /// 约束（`id` 必填、幂等、额度上限）一个都没变。
    pub(crate) async fn request(&self, value: Value) -> Value {
        self.session.lock().await.request(value).await
    }

    /// 工具调用：`op` + 参数，`call_id` 兼作回执去重键。
    pub(crate) async fn call(&self, call_id: &str, op: &str, params: Value) -> Value {
        let mut envelope = params;
        // 信封字段最后写：参数里万一有同名的键，也不能顶掉 id/op。
        if let Value::Object(map) = &mut envelope {
            map.insert("id".to_string(), json!(call_id));
            map.insert("op".to_string(), json!(op));
        }
        self.request(envelope).await
    }

    pub(crate) fn used(&self) -> bool {
        self.attempted.load(Ordering::SeqCst)
    }
    pub(crate) fn revision(&self) -> u64 {
        self.seq.load(Ordering::SeqCst)
    }
}

/// 接进 agent 执行层的聊天界面出口（见 [`crate::plugins::oai::agent::ChatBridge`]）。
impl crate::plugins::oai::agent::ChatBridge for Bridge {
    fn call<'a>(
        &'a self,
        call_id: &'a str,
        op: &'a str,
        params: Value,
    ) -> futures_util::future::BoxFuture<'a, Value> {
        Box::pin(Bridge::call(self, call_id, op, params))
    }

    fn used(&self) -> bool {
        Bridge::used(self)
    }

    fn revision(&self) -> u64 {
        Bridge::revision(self)
    }
}

struct Session {
    ctx: Context,
    writer: LockedWriter,
    group: String,
    config: ChatConfig,
    persona: Option<Arc<dyn Persona>>,
    scratch: PathBuf,
    media: PathBuf,
    /// 偷来的表情包那份库（全局一份，见 [`super::stickers`]）。
    stickers: PathBuf,
    seq: Arc<AtomicU64>,
    attempted: Arc<AtomicBool>,
    writes: usize,
    messages: usize,
    draws: usize,
    music: usize,
    videos: usize,
    memos: usize,
    observations: usize,
    spoke: bool,
    read_marked: bool,
    started: Instant,
    receipts: HashMap<String, Value>,
    capabilities: Value,
    own_reactions: HashMap<String, Vec<String>>,
    /// 平台明确拒绝过的能力（动作名 → 给模型的解释）。见 [`Session::refused`]。
    refusals: HashMap<&'static str, String>,
    /// 这个群现在开着吗；停用之后这一轮什么都不做。
    enabled: bool,
    /// 动手之前要不要确认群聊没有往前走。
    require_fresh: bool,
    /// 这一轮的群聊现场从哪儿来。
    scene: Scene,
    /// 房间那一侧向平台要回来的那一页（一轮只取一次）。
    page: Option<Vec<Turn>>,
}

/// 开一轮：把这一轮的现场、额度与产物目录打包成一个工具出口。
///
/// 起点在这里，出口交给执行层（[`crate::plugins::oai::agent::ChatBridge`]）。
pub(crate) async fn start(env: ChatEnv<'_>) -> Result<Bridge> {
    let ChatEnv {
        ctx,
        writer,
        group,
        config,
        enabled,
        require_fresh,
        scratch,
        media,
        persona,
        scene,
    } = env;
    // 起点记下群聊现场走到哪一步：模型读完 `satori_context` 之后，这里就是它看过
    // 的那一版；期间群里又有人说话，动手之前就会被拦下。
    let seq = Arc::new(AtomicU64::new(window::with_group(&group, |s| s.seq)));
    let attempted = Arc::new(AtomicBool::new(false));
    let session = Session {
        ctx: ctx.clone(),
        writer: writer.clone(),
        group,
        config: config.clone(),
        persona,
        scratch: scratch.to_path_buf(),
        media: media.to_path_buf(),
        // 表情包库的位置由库自己记着（[`super::stickers::attach`]）。
        stickers: stickers::root().unwrap_or_default(),
        seq: seq.clone(),
        attempted: attempted.clone(),
        writes: 0,
        messages: 0,
        draws: 0,
        music: 0,
        videos: 0,
        memos: 0,
        observations: 0,
        spoke: false,
        read_marked: false,
        started: Instant::now(),
        receipts: HashMap::new(),
        capabilities: Value::Null,
        own_reactions: HashMap::new(),
        refusals: HashMap::new(),
        enabled,
        require_fresh,
        scene,
        page: None,
    };
    Ok(Bridge {
        session: Arc::new(tokio::sync::Mutex::new(session)),
        attempted,
        seq,
    })
}

impl Session {
    /// 这一轮的群聊现场。
    ///
    /// 窗口那一侧直接读常驻窗口；房间那一侧向平台要最近一页，一轮只取一次。
    async fn scene_turns(&mut self, count: usize) -> Vec<Turn> {
        match self.scene {
            Scene::Window => window::with_group(&self.group, |state| state.recent(count)),
            Scene::Channel => {
                if self.page.is_none() {
                    self.page = self.fetch_page(count).await.ok();
                }
                self.page.clone().unwrap_or_default()
            }
        }
    }

    /// 按消息号取一条：现场里有就用现场那份，没有就问平台要。
    ///
    /// 房间那边翻旧账翻出来的消息号往往早于眼前这一页，而引用、转发、偷表情都要
    /// 原消息的元素，所以这条兜底是必需的。搭话那一侧仍然只认自己的窗口。
    async fn turn_of(&mut self, id: &str) -> Result<Turn> {
        let id = actions::id(id)?;
        if let Some(turn) = self
            .scene_turns(80)
            .await
            .iter()
            .find(|turn| turn.message_id == id)
        {
            return Ok(turn.clone());
        }
        ensure!(self.scene == Scene::Channel, "消息不在本群当前窗口内，先读 satori_context");
        let raw = self
            .rpc(
                "message.get",
                json!({"channel_id":self.group.to_string(),"message_id":id.to_string()}),
            )
            .await?;
        turn_from_platform(&self.ctx, &self.writer, &raw)
            .ok_or_else(|| anyhow::anyhow!("QQ 没有返回这条消息"))
    }


    /// 房间那一侧：把动作点到的消息与群友补进眼前这一页。
    ///
    /// 补完之后 [`Action::validate`] 那条「目标得看得见」的规矩原样成立——只不过
    /// 看见它的方式是从平台取回来，而不是在窗口里等着它出现。
    async fn hydrate(&mut self, action: &Action, turns: &mut Vec<Turn>) -> Result<()> {
        if self.scene == Scene::Window {
            return Ok(());
        }
        let (messages, users) = action.referenced();
        for id in messages {
            if turns.iter().any(|turn| turn.message_id == id) {
                continue;
            }
            let raw = self
                .rpc(
                    "message.get",
                    json!({"channel_id":self.group.to_string(),"message_id":id.to_string()}),
                )
                .await?;
            if let Some(turn) = turn_from_platform(&self.ctx, &self.writer, &raw) {
                turns.push(turn);
            }
        }
        for user_id in users {
            // 群友不需要「说过话」才存在；这里补一条出处，成员资格交给平台。
            if !turns.iter().any(|turn| turn.user_id == user_id) {
                turns.push(Turn {
                    user_id,
                    ..Turn::default()
                });
            }
        }
        Ok(())
    }

    /// 平台存的那一页消息。
    async fn fetch_page(&self, count: usize) -> Result<Vec<Turn>> {
        let page = self
            .rpc(
                "message.list",
                json!({"channel_id":self.group.to_string(),"limit":count.clamp(1,100)}),
            )
            .await?;
        let Some(items) = page["data"].as_array() else {
            return Ok(Vec::new());
        };
        Ok(items
            .iter()
            .filter_map(|item| turn_from_platform(&self.ctx, &self.writer, item))
            .collect())
    }
    /// 这一轮还该不该动手。
    ///
    /// 两道：这个群还开着（停用之后一轮都不要再发出去），以及群聊还停在模型看过的
    /// 那一版（搭话的回复只对刚才那批消息负责）。房间不要求时效——它回答的是一句
    /// 直接请求，期间群里聊了什么与这次回答无关。
    fn current(&self) -> bool {
        self.enabled
            && (!self.require_fresh
                || window::with_group(&self.group, |s| s.seq) == self.seq.load(Ordering::SeqCst))
    }
    /// 打完字那一刻还发不发：比 [`Self::current`] 宽一条——打字的工夫里群里冒出
    /// 一句无关的，人照样会发出去；来了好几句、或者有人点了名，就得重看现场。
    fn sendable(&self) -> bool {
        self.enabled
            && (!self.require_fresh
                || window::with_group(&self.group, |s| {
                    s.drift(self.seq.load(Ordering::SeqCst)) <= 1 && !s.has_unread_mention()
                }))
    }
    async fn request(&mut self, request: Value) -> Value {
        let key = request["id"].as_str().unwrap_or("").to_string();
        if key.is_empty() || key.len() > 200 {
            return json!({"ok":false,"error":"request id required"});
        }
        if let Some(receipt) = self.receipts.get(&key) {
            return receipt.clone();
        }
        if self.receipts.len() >= 64 {
            return json!({"ok":false,"error":"request budget exhausted"});
        }
        let result = self.execute(&request).await;
        let response = match result {
            Ok(value) => json!({"ok":true,"result":value}),
            Err(error) => json!({"ok":false,"error":format!("{error:#}")}),
        };
        self.receipts.insert(key, response.clone());
        response
    }
    async fn execute(&mut self, request: &Value) -> Result<Value> {
        match request["op"].as_str().unwrap_or("") {
            "context" => {
                ensure!(self.enabled(), "本群的群聊功能已停用");
                if self.capabilities.is_null() {
                    self.capabilities = self.describe_capabilities().await;
                }
                // 平台级拒绝会跨轮留着（见 [`REFUSAL_TTL`]），每次报告都要重算，
                // 免得人格把一个已知按不动的按钮当成还没试过的。
                let mut capabilities = self.capabilities.clone();
                capabilities["management_enabled"] = json!(self.management_enabled());
                let unavailable: serde_json::Map<String, Value> = ACTION_KINDS
                    .iter()
                    .filter_map(|kind| {
                        platform_refusal(&self.refusal_scope(), kind).map(|why| ((*kind).to_string(), json!(why)))
                    })
                    .collect();
                capabilities["unavailable"] = Value::Object(unavailable);
                let count = self.config.context_turns.clamp(1, 80);
                let (seq, turns, rhythm) = match self.scene {
                    Scene::Window => window::with_group(&self.group, |s| {
                        s.take_mention();
                        (s.seq, s.recent(count), s.rhythm())
                    }),
                    // 房间不跟踪群聊：现场就是刚才问平台要回来的那一页，
                    // 也就没有「群聊走到哪一步」这回事。
                    Scene::Channel => (0, self.scene_turns(count).await, String::new()),
                };
                self.seq.store(seq, Ordering::SeqCst);
                // 人格那一份现场由调用方给（房间没有），缺的键留空串，形状一样。
                let persona = self
                    .persona
                    .as_ref()
                    .map(|persona| persona.scene(&self.group, &turns, &rhythm))
                    .unwrap_or(Value::Null);
                let turns: Vec<Value> = turns.iter().map(|t| json!({
                    "message_id":t.message_id.to_string(),"user_id":t.user_id.to_string(),"name":t.name,
                    "text":t.text,"from_me":t.from_me,"time":t.at,"elements":t.elements,
                })).collect();
                let mut media = Vec::new();
                if let Ok(mut entries) = tokio::fs::read_dir(&self.media).await {
                    while let Ok(Some(entry)) = entries.next_entry().await {
                        if media.len() >= 40 {
                            break;
                        }
                        if entry.file_type().await.is_ok_and(|t| t.is_file()) {
                            media.push(entry.path().to_string_lossy().into_owned());
                        }
                    }
                }
                // 还没认过这个群就先问一遍「我在这个群里是谁」；有缓存时是一个空转。
                if identity::of(&self.group).is_none() {
                    let avatar = self.persona.as_ref().and_then(|persona| persona.avatar());
                    identity::refresh(&self.ctx, &self.writer, avatar.as_ref(), &self.group).await;
                }
                let identity = identity::of(&self.group).map(|identity| json!({
                    "name": identity.name, "card": identity.card, "display": identity.display(),
                    "title": identity.title, "role": identity.role, "joined_at": identity.joined_at,
                    "group_name": identity.group_name, "avatar": identity.avatar,
                }));
                Ok(
                    json!({"revision":seq,"group_id":self.group.to_string(),"self_id":self.ctx.bot.login_user.get().id,
                    "identity":identity,
                    "now":super::now_context(),
                    "register":persona.get("register").and_then(Value::as_str).unwrap_or(""),
                    "state":persona.get("state").and_then(Value::as_str).unwrap_or(""),
                    "remember":persona.get("remember").and_then(Value::as_str).unwrap_or(""),
                    "capabilities":capabilities,"rhythm":rhythm,"messages":turns,"media":media,
                    "observations_remaining":if self.scene == Scene::Window {4usize.saturating_sub(self.observations)} else {0},
                    "environment_lookups":if self.scene == Scene::Window {json!(["group","member","member_card","group_card","essence","title_display","honor_display"])} else {json!([])},
                    "writes_remaining":self.config.actions_budget.clamp(1,12).saturating_sub(self.writes),
                    "messages_remaining":self.config.messages_budget.clamp(1,5).saturating_sub(self.messages),
                    "draws_remaining":self.config.draw_budget.clamp(0,8).saturating_sub(self.draws),
                    "music_remaining":self.config.music_budget.clamp(0,4).saturating_sub(self.music),
                    "videos_remaining":self.config.video_budget.clamp(0,2).saturating_sub(self.videos),
                    }),
                )
            }
            "read" => {
                ensure!(self.enabled(), "本群的群聊功能已停用");
                let id = request["message_id"].as_str().unwrap_or("");
                let turn = self.turn_of(id).await?;
                ensure!(!(request["reactions"].as_bool().unwrap_or(false) && request["forward"].as_bool().unwrap_or(false)), "回应查询与转发展开请分开调用");
                if request["reactions"].as_bool().unwrap_or(false) {
                    self.rpc("internal/reaction_summary", json!({"channel_id":self.group.to_string(),"message_id":id})).await
                } else if request["forward"].as_bool().unwrap_or(false) {
                    let source = forward::source_of(&turn.elements, Some(turn.message_id.clone()))
                        .ok_or_else(|| anyhow::anyhow!("该消息不是合并转发"))?
                        .in_channel(self.group.to_string());
                    let view = forward::expand(&self.ctx, &self.writer, source).await;
                    ensure!(
                        !view.is_empty(),
                        "合并转发没有读到内容：{}",
                        if view.notes.is_empty() {
                            "原文为空".to_string()
                        } else {
                            view.notes.join("；")
                        }
                    );
                    Ok(json!({
                        "message_id": id,
                        "node_count": view.nodes.len(),
                        "truncated": view.truncated,
                        "notes": view.notes,
                        "images": view.images(),
                        "transcript": view.transcript(),
                        "nodes": view.nodes.iter().map(|node| json!({
                            "depth": node.depth,
                            "message_id": node.message_id,
                            "user_id": node.user_id,
                            "name": node.name,
                            "time": node.time,
                            "text": forward::describe(&node.message),
                            "elements": node.message,
                        })).collect::<Vec<_>>(),
                    }))
                } else {
                    let message = self
                        .rpc(
                            "message.get",
                            json!({"channel_id":self.group.to_string(),"message_id":id}),
                        )
                        .await?;
                    Ok(message)
                }
            }
            "observe" => {
                ensure!(self.enabled(), "本群的群聊功能已停用");
                ensure!(self.scene == Scene::Window, "环境探查只对搭话开放；房间使用现有上下文和精确消息读取");
                ensure!(self.ctx.bot.adapter == "satori-qq", "环境探查需要 satori-qq");
                ensure!(self.observations < 4, "本轮环境探查次数已用完");
                let kind = request["kind"].as_str().unwrap_or("");
                let user_id = request["user_id"].as_str().unwrap_or("");
                // 只允许查本群窗口里真实出现过的人，不能把 QQ 号当成任意资料查询入口。
                if matches!(kind, "member" | "member_card") {
                    ensure!(user_id.parse::<i64>().is_ok_and(|id| id > 0), "需要有效的群成员 QQ 号");
                    let known = window::with_group(&self.group, |state| state.recent(80).iter().any(|t| t.user_id.to_string() == user_id));
                    ensure!(known || user_id == self.ctx.bot.login_user.get().id, "只可探查当前群聊中出现的成员或自己");
                }
                let (method, params) = observation(kind, &self.group, user_id)?;
                if method.starts_with("internal/") {
                    if self.capabilities.is_null() {
                        self.capabilities = self.describe_capabilities().await;
                    }
                    let name = method.trim_start_matches("internal/");
                    ensure!(self.capabilities["extension_actions"].as_array().is_some_and(|a| a.iter().any(|v| v == name)), "实现端未声明 {name} 能力");
                }
                self.observations += 1; // 失败也计入，避免失效的内核接口被反复探测。
                let data = self.rpc(method, params).await?;
                let text = serde_json::to_string(&data)?;
                ensure!(text.len() <= 16_384, "平台返回过大，停止展示；不要重复查询");
                Ok(json!({"kind":kind,"data":data,"source":method,"note":"QQ 内核资料可能滞后；返回值不等于实时现场，以回执和群聊记录为准"}))
            }
            "draw" => {
                ensure!(self.enabled(), "本群的群聊功能已停用");
                ensure!(self.current(), "群聊已更新，先读 satori_context 再决定");
                let prompt = request["prompt"].as_str().unwrap_or("").trim().to_string();
                ensure!(!prompt.is_empty(), "绘图提示词先给几个字");
                let images: Vec<String> = request["images"]
                    .as_array()
                    .map(|array| {
                        array
                            .iter()
                            .filter_map(|value| value.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let size = request["size"].as_str().map(str::to_string);
                let quality = request["quality"].as_str().map(str::to_string);
                let budget = self.config.draw_budget.clamp(0, 8);
                ensure!(budget > 0, "本群已关闭绘图（draw_budget = 0）");
                ensure!(self.draws < budget, "本轮绘图额度已用完");
                let oai = crate::plugins::get_config_or_default::<crate::plugins::oai::OaiConfig>(
                    &self.ctx, "oai",
                );
                let (api_base, api_key, model) = media_endpoint(
                    crate::plugins::oai::images::is_images_model,
                    &oai.image_models,
                    crate::plugins::oai::images::FALLBACK_MODEL,
                    "图像",
                )
                .await?;
                // 绘图是模型调用而不是平台写操作，不占用 writes/messages 额度；
                // 单独设每轮张数上限，让语音/表情之外多一种表达不失控。
                self.draws += 1;
                let generated = crate::plugins::oai::images::generate(
                    &api_base,
                    &api_key,
                    &model,
                    &prompt,
                    &images,
                    size.as_deref(),
                    quality.as_deref(),
                )
                .await?;
                let mut saved = Vec::new();
                for (index, url) in generated.urls.iter().enumerate() {
                    match self.save_image_to_media(url, index).await {
                        Ok(path) => saved.push(json!({ "file": path, "url": url })),
                        Err(error) => {
                            warn!(target: super::LOG_TARGET, "保存生成的图片失败 {url}: {error:#}");
                        }
                    }
                }
                ensure!(!saved.is_empty(), "生成了图片，但写入本地失败");
                Ok(json!({
                    "images": saved,
                    "caption": generated.caption,
                    "model": generated.model,
                    "draws_remaining": budget.saturating_sub(self.draws),
                }))
            }
            "music" => {
                ensure!(self.enabled(), "本群的群聊功能已停用");
                ensure!(self.current(), "群聊已更新，先读 satori_context 再决定");
                let prompt = request["prompt"].as_str().unwrap_or("").trim().to_string();
                ensure!(!prompt.is_empty(), "写歌得先说清楚写一首什么样的歌");
                let budget = self.config.music_budget.clamp(0, 4);
                ensure!(budget > 0, "本群已关闭写歌（music_budget = 0）");
                ensure!(self.music < budget, "本轮写歌额度已用完");
                let oai = crate::plugins::get_config_or_default::<crate::plugins::oai::OaiConfig>(
                    &self.ctx, "oai",
                );
                let (api_base, api_key, model) = media_endpoint(
                    crate::plugins::oai::music::is_music_model,
                    &oai.music_models,
                    crate::plugins::oai::music::FALLBACK_MODEL,
                    "音乐",
                )
                .await?;
                let options = crate::plugins::oai::music::Options {
                    prompt,
                    title: request["title"].as_str().unwrap_or("").trim().to_string(),
                    tags: request["tags"].as_str().unwrap_or("").trim().to_string(),
                    version: oai.music_version(),
                    instrumental: request["instrumental"].as_bool().unwrap_or(false),
                    // 发法由人格自己挑（成品存到本地后走 satori_action 的 audio / file），
                    // 这里那一栏是房间路径的提示词开关，搭话用不上。
                    send: None,
                };
                // 写歌和绘图一样是模型调用，不占 writes/messages 额度；一次出两个版本，
                // 两个都留，让模型自己挑一首发、或者两首都发。
                self.music += 1;
                let generated = crate::plugins::oai::music::generate(
                    &api_base,
                    &api_key,
                    &options,
                    self.media_deadline(),
                )
                .await?;
                let stamp = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
                let mut songs = Vec::new();
                for (index, clip) in generated.clips.iter().enumerate() {
                    let mut song = json!({
                        "title": clip.title.trim(),
                        "duration": clip.duration.round() as i64,
                        "lyrics": clip.prompt,
                        "audio_url": clip.audio_url,
                        "cover_url": clip.image_url,
                    });
                    if !clip.audio_url.trim().is_empty()
                        && let Some(path) = self
                            .save_remote(
                                clip.audio_url.trim(),
                                &format!("music-{stamp}-{index}.mp3"),
                            )
                            .await
                    {
                        song["audio"] = json!(path);
                    }
                    if !clip.image_url.trim().is_empty()
                        && let Some(path) = self
                            .save_remote(
                                clip.image_url.trim(),
                                &format!("music-{stamp}-{index}.jpg"),
                            )
                            .await
                    {
                        song["cover"] = json!(path);
                    }
                    songs.push(song);
                }
                ensure!(
                    songs
                        .iter()
                        .any(|song| song["audio"].as_str().is_some_and(|path| !path.is_empty())),
                    "歌出来了，但音频没能存到本地"
                );
                Ok(json!({
                    "songs": songs,
                    "tags": generated.tags,
                    "version": generated.version,
                    "cost": generated.cost,
                    "model": model,
                    "note": "两个版本是同一次生成的两首，各带本地 audio 与 cover 路径；发哪首、还是一起发，由你定。",
                    "music_remaining": budget.saturating_sub(self.music),
                }))
            }
            "video" => {
                ensure!(self.enabled(), "本群的群聊功能已停用");
                ensure!(self.current(), "群聊已更新，先读 satori_context 再决定");
                let prompt = request["prompt"].as_str().unwrap_or("").trim().to_string();
                ensure!(!prompt.is_empty(), "拍片得先说清楚要拍什么");
                let budget = self.config.video_budget.clamp(0, 2);
                ensure!(budget > 0, "本群已关闭拍片（video_budget = 0）");
                ensure!(self.videos < budget, "本轮拍片额度已用完");
                let oai = crate::plugins::get_config_or_default::<crate::plugins::oai::OaiConfig>(
                    &self.ctx, "oai",
                );
                let (api_base, api_key, model) = media_endpoint(
                    crate::plugins::oai::video::is_video_model,
                    &oai.video_models,
                    crate::plugins::oai::video::FALLBACK_MODEL,
                    "视频",
                )
                .await?;
                // 时长与画面比例都从枚举里挑，别让模型把任意字符串塞进接口。
                let seconds = request["seconds"]
                    .as_u64()
                    .filter(|value| (1..=30).contains(value))
                    .unwrap_or(u64::from(oai.video_seconds()))
                    .to_string();
                let size = match request["size"].as_str().unwrap_or("") {
                    "竖屏" | "portrait" | "9:16" => crate::plugins::oai::video::PORTRAIT,
                    _ => crate::plugins::oai::video::LANDSCAPE,
                };
                self.videos += 1;
                let generated = crate::plugins::oai::video::generate(
                    &api_base,
                    &api_key,
                    &model,
                    &prompt,
                    &seconds,
                    size,
                    self.media_deadline(),
                )
                .await?;
                let stamp = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
                let mut video = json!({
                    "video_url": generated.video_url,
                    "model": generated.model,
                    "seconds": generated.seconds,
                    "cost": generated.cost,
                    "size": size,
                });
                if let Some(path) = self
                    .save_remote(&generated.video_url, &format!("video-{stamp}.mp4"))
                    .await
                {
                    video["video"] = json!(path);
                }
                Ok(json!({
                    "video": video,
                    "note": "有本地 video 路径时用它发；没有就只把 video_url 说给群友。",
                    "videos_remaining": budget.saturating_sub(self.videos),
                }))
            }
            "memo" => {
                ensure!(self.enabled(), "本群的群聊功能已停用");
                ensure!(self.config.memory_enabled, "本群已关闭记忆");
                let budget = self.config.memo_budget.clamp(0, 8);
                ensure!(
                    budget > 0,
                    "本群已关闭记忆写入（memo_budget = 0）"
                );
                ensure!(self.memos < budget, "本轮记忆额度已用完");
                self.memos += 1;
                let turns = self.scene_turns(80).await;
                let now = chrono::Local::now().timestamp();
                let mut done = Vec::new();
                if let Some(people) = request["people"].as_array() {
                    for entry in people.iter().take(8) {
                        let raw = entry["user_id"].as_str().unwrap_or("");
                        // 记谁都可以：群友不需要在眼前这段记录里出现过。
                        let id = actions::id(raw)?;
                        let note = entry["note"].as_str();
                        let address = entry["address"].as_str();
                        ensure!(
                            note.is_some() || address.is_some(),
                            "给 {id} 的记忆里 note 与 address 都是空的，没有可写入的内容"
                        );
                        let name = turns
                            .iter()
                            .find(|turn| turn.user_id == id)
                            .map(|turn| turn.name.clone())
                            .unwrap_or_default();
                        memory::edit(&self.group, |memory| {
                            memory.see(&id, &name, now);
                            if let Some(address) = address {
                                memory.address(&id, address)?;
                            }
                            if let Some(note) = note {
                                memory.remember(&id, note)?;
                            }
                            Ok::<_, anyhow::Error>(())
                        })?;
                        done.push(format!("记住 {id}"));
                    }
                }
                if let Some(notes) = request["notes"].as_array() {
                    for note in notes.iter().take(8) {
                        let text = note.as_str().unwrap_or("");
                        memory::edit(&self.group, |memory| memory.jot(text, now))?;
                        done.push("记下一件事".to_string());
                    }
                }
                for entry in request["forget_people"].as_array().into_iter().flatten() {
                    let id = actions::id(entry.as_str().unwrap_or(""))?;
                    if memory::edit(&self.group, |memory| memory.forget(&id)) {
                        done.push(format!("忘掉 {id}"));
                    }
                }
                for entry in request["forget_notes"].as_array().into_iter().flatten() {
                    let text = entry.as_str().unwrap_or("");
                    if memory::edit(&self.group, |memory| memory.drop_note(text)) {
                        done.push("忘掉一件事".to_string());
                    }
                }
                for entry in request["forget_claims"].as_array().into_iter().flatten() {
                    let text = entry.as_str().unwrap_or("");
                    if memory::edit(&self.group, |memory| memory.drop_claim(text)) {
                        done.push("忘掉一条自己说过的话".to_string());
                    }
                }
                ensure!(!done.is_empty(), "没有可写入的记忆内容");
                memory::flush_now(&self.group).await;
                Ok(json!({
                    "applied": done,
                    "summary": memory::with_group(&self.group, |memory| memory.summary()),
                    "memos_remaining": budget.saturating_sub(self.memos),
                }))
            }
            "action" => {
                // 一旦选择工具动作，就不再把最终解释当作第二份消息发送。
                self.attempted.store(true, Ordering::SeqCst);
                let action: Action = serde_json::from_value(request["request"].clone())?;
                // 撤回已发出的自己的消息是补救，不是对旧话题继续发言。
                // 新消息进群也应允许补救；停用的群仍然不许动作。
                ensure!(
                    self.enabled() && (matches!(action, Action::Recall { .. }) || self.current()),
                    "群聊已更新或停用。先读 satori_context 再决定，旧动作照现在聊的重新想一遍更稳"
                );
                let mut turns = self.scene_turns(80).await;
                self.hydrate(&action, &mut turns).await?;
                ensure!(
                    !action.requires_management(&self.ctx.bot.login_user.get().id)
                        || self.management_enabled(),
                    "本群未启用管理动作（management_groups 里没有这个群）"
                );
                // 管理对象可能很久没说话。按 QQ 的当前群名册核实，不往聊天窗口伪造发言。
                if let Some(target) = action.management_target() {
                    let uid = actions::id(target)?;
                    if !turns.iter().any(|t| t.user_id == uid) {
                        // 先做纯参数校验，格式错误不消耗查询额度。
                        turns.push(Turn {
                            user_id: uid,
                            ..Default::default()
                        });
                        action.validate(&turns)?;
                        let member = self
                            .rpc(
                                "guild.member.get",
                                json!({"guild_id":self.group.to_string(),"user_id":target}),
                            )
                            .await?;
                        ensure!(
                            member.pointer("/user/id").and_then(Value::as_str) == Some(target),
                            "QQ 未确认该用户是本群成员"
                        );
                    }
                }
                action.validate(&turns)?;
                if let Action::Send { parts, .. } = &action
                    && let Some(persona) = &self.persona
                {
                    let body = parts
                        .iter()
                        .filter_map(|part| match part {
                            Part::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    if let Some(reason) = persona.vet_text(&self.group, &body, &turns) {
                        anyhow::bail!("{reason}");
                    }
                }
                // 平台已经明确拒绝过的能力不再占额度：那一次尝试没有产生任何副作用，
                // 让它把本轮仅有的几次动作耗在必然失败的按钮上只会换来一次沉默。
                if let Some(reason) = self.refused(&action) {
                    anyhow::bail!("{reason}");
                }
                ensure!(
                    self.writes < self.config.actions_budget.clamp(1, 12),
                    "本轮动作额度已用完"
                );
                ensure!(
                    !action.is_message() || self.messages < self.config.messages_budget.clamp(1, 5),
                    "本轮消息额度已用完"
                );
                self.writes += 1;
                if action.is_message() {
                    self.messages += 1;
                }
                let result = self.perform(&action, &turns).await;
                if let Err(ref error) = result {
                    if self.remember_refusal(&action, error) {
                        // 服务端直接判定不允许，动作没有到达聊天：退回额度，别让它算作用掉一次。
                        self.writes -= 1;
                        if action.is_message() {
                            self.messages -= 1;
                        }
                    } else {
                        // 其余错误进入下一轮上下文；网络超时可能已经成功，不自动重放。
                        self.record(
                            format!(
                                "[动作未确认 {}：{}；结果未知，换个做法更稳]",
                                request["request"]["action"], error
                            ),
                            String::new(),
                            Message::new(),
                            false,
                        );
                    }
                }
                result
            }
            _ => anyhow::bail!("unknown operation"),
        }
    }
    /// 报告一次「这条链路此刻真正能做什么」。
    ///
    /// 三层各说各的：`platform_features` 是实现端自报的 Satori 方法，
    /// `actions` / `lookups` / `profile` 是这座桥实际接受的参数，`qq_extensions` 决定
    /// 戳一戳、点赞这类 QQ 专有动作在不在。查询失败也照样把后两层报出去——它们不依赖
    /// 那次探测，而实际结果无论如何都以回执为准。
    fn refusal_scope(&self) -> String {
        format!("{}|{}|{}", self.writer.connection_key(), self.ctx.bot.platform, self.ctx.bot.login_user.get().id)
    }

    async fn describe_capabilities(&self) -> Value {
        let qq = self.ctx.bot.adapter == "satori-qq";
        let login = self
            .writer
            .call::<_, Value>(&self.ctx, "login.get", json!({}))
            .await
            .ok();
        let features = login
            .as_ref()
            .and_then(|login| login.get("features").cloned());
        let extensions = if qq {
            self.rpc("internal/capabilities", json!({})).await.ok()
        } else {
            None
        };
        // 实现端自己列的「曾经有、现在没了」直接记进不可用名单。它给 `removed` 这张表
        // 就是为了这个：不必等到撞一次 404 才知道按不动（见 `remember_platform_refusal`）。
        if let Some(removed) = extensions
            .as_ref()
            .and_then(|value| value.get("removed"))
            .and_then(Value::as_object)
        {
            for kind in ACTION_KINDS {
                let Some(why) = removed.get(kind).and_then(Value::as_str) else {
                    continue;
                };
                remember_platform_refusal(
                    &self.refusal_scope(), kind,
                    &format!("实现端已移除这个动作（{why}）。这一轮换个法子回应更划算。"),
                );
            }
        }
        let own_member = self.rpc("guild.member.get", json!({"guild_id":self.group.to_string(),"user_id":self.ctx.bot.login_user.get().id})).await;
        let environment = match own_member {
            Ok(member) => {
                json!({"self_member":member,"observed_at":chrono::Utc::now().timestamp_millis()})
            }
            Err(e) => {
                json!({"self_member":null,"error":e.to_string(),"observed_at":chrono::Utc::now().timestamp_millis()})
            }
        };
        let mut out = json!({
            "adapter": self.ctx.bot.adapter,
            "extension_actions":extensions.as_ref().and_then(|v| v.get("actions")),
            "environment":environment,
            "management_enabled":self.management_enabled(),
            "qq_extensions": qq,
            "login_status": login.as_ref().and_then(|login| login.get("status")),
            "platform_features": features,
            "actions": ACTION_KINDS,
            "note": "以回执为准；这里列的是参数层面接受什么，不保证 QQ 服务端每次都放行。",
        });
        if !qq {
            out["note"] =
                json!("当前适配器未声明 QQ 扩展：戳一戳与资料卡点赞不可用，其余以回执为准。");
        }
        out
    }








    /// 已知不可用的能力；有值就直接回绝，不占动作额度。
    /// 先看本轮的记账，再看跨轮那份（平台限制不会因为换了一轮就消失）。
    fn refused(&self, action: &Action) -> Option<String> {
        let key = capability(action);
        self.refusals
            .get(key)
            .cloned()
            .or_else(|| platform_refusal(&self.refusal_scope(), key))
    }
    /// 记录一次「服务端明确拒绝、动作从未到达聊天」的失败，并返回它是否属于这一类。
    ///
    /// 只认平台自己给出的判定语句。网络超时的结果是未知的，绝不能算进来——
    /// 那会让一次可能已经送达的操作被当成没发生。
    fn remember_refusal(&mut self, action: &Action, error: &anyhow::Error) -> bool {
        let text = format!("{error:#}");
        let refusal = match action {
            // 资料卡点赞自 2026 年起被腾讯按 appid 限流，整段 oidb 被服务端驳回。
            Action::Like { .. }
                if text.contains("send_like failed") || text.contains("not match appid") =>
            {
                "QQ 拒绝了这个账号的资料卡点赞（平台限制，不是参数问题）。这一轮换个法子回应更划算。"
            }
            // satori-qq 0.23.0 起只走 JNI 层，资料卡点赞（要 QQ 的 WUP/Handler 通道）不在了。
            // 实现端会给 `code=removed_action`，按它认，别去匹配会变的中文文案。
            Action::Like { .. } if text.contains("removed_action") => {
                "实现端已不再提供资料卡点赞（0.23.0 起只走 JNI 层）。这一轮换个法子回应更划算。"
            }
            _ => return false,
        };
        let reason = format!("{refusal}原始回执：{text}");
        remember_platform_refusal(&self.refusal_scope(), capability(action), &reason);
        self.refusals.insert(capability(action), reason);
        true
    }
    fn enabled(&self) -> bool {
        self.enabled
    }
    /// 这个群允许执行群管理动作吗（见 [`ChatConfig::management_groups`]）。
    fn management_enabled(&self) -> bool {
        self.config.management_groups.contains(&self.group)
    }
    async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        ensure!(
            !method.starts_with("internal/") || self.ctx.bot.adapter == "satori-qq",
            "当前适配器未声明 QQ 扩展"
        );
        let data: Value = self
            .writer
            .call(&self.ctx, method, params)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        ensure!(
            data.get("payload") != Some(&Value::Bool(false)),
            "{method} 没有提供数据；状态未知，不能当作空列表或零值：{data}"
        );
        Ok(data)
    }

    /// 一条 `sticker` 段落 → 真能发出去的段落。
    ///
    /// 两个来路。`id` 是库里的那张：商城表情照原样再发一遍，图那份文件是当初偷的时候
    /// 抄下来的，早就不在窗口里了，所以走上传。没有 `id` 就是从眼前这条记录里偷——
    /// 偷到手的同时抄进库，往后窗口滑过去、进程重启，它还认得这张图；`note` 是人格
    /// 顺手写的那句说明，进库当标签，以后才挑得出来。
    async fn sticker(
        &self,
        turns: &[Turn],
        id: Option<u32>,
        message_id: &str,
        index: usize,
        note: &str,
    ) -> Result<Message> {
        let Some(id) = id else {
            let source = actions::message(turns, message_id)?;
            let segment = actions::sticker(source, index)?;
            // 商城表情没有下载这一说：重发靠参数，字节存下来也没用。
            let bytes = match actions::image_source(&segment) {
                Some(url) => fetch_media(url).await.ok(),
                None => None,
            };
            if let Some(id) = stickers::keep(
                &segment,
                source,
                &self.group,
                note,
                bytes.as_deref(),
                self.config.sticker_max,
            ) {
                info!(target: super::LOG_TARGET, "偷来的表情包，第 {id} 张进库了");
            }
            return Ok(Message(vec![stickers::sticker_style(segment)]));
        };
        let entry = stickers::take(id, note)
            .ok_or_else(|| anyhow::anyhow!("库里没有编号 {id} 那张表情包"))?;
        match &entry.kind {
            stickers::Kind::Shop { data } => Ok(Message(vec![Segment::new("mface", data.clone())])),
            stickers::Kind::Image { file } => {
                let path = stickers::file_of(&entry)
                    .ok_or_else(|| anyhow::anyhow!("第 {id} 张表情包的文件不在了"))?;
                let image = Message::new().image(self.source(&path.to_string_lossy(), file).await?);
                Ok(Message(image.0.into_iter().map(stickers::sticker_style).collect()))
            }
        }
    }
    async fn source(&self, source: &str, name: &str) -> Result<String> {
        if source.starts_with("https://") || source.starts_with("http://") {
            let url = url::Url::parse(source)?;
            ensure!(
                url.host_str().is_some() && url.username().is_empty() && url.password().is_none(),
                "资源 URL 无效"
            );
            return Ok(source.into());
        }
        // Termux 的私有文件不能直接让 QQ 进程读取，必须走 upload.create。
        let input = Path::new(source);
        let path = tokio::fs::canonicalize(if input.is_absolute() {
            input.to_path_buf()
        } else {
            self.scratch.join(input)
        })
        .await?;
        let scratch = tokio::fs::canonicalize(&self.scratch).await?;
        let media = tokio::fs::canonicalize(&self.media).await.ok();
        let stickers = tokio::fs::canonicalize(&self.stickers).await.ok();
        ensure!(
            path.starts_with(scratch)
                || media.is_some_and(|root| path.starts_with(root))
                || stickers.is_some_and(|root| path.starts_with(root)),
            "本地资源只能取自本轮工作目录或表情包库，Termux 私有路径 QQ 读不到"
        );
        let meta = tokio::fs::metadata(&path).await?;
        ensure!(
            meta.is_file() && meta.len() <= 20 * 1024 * 1024,
            "文件须为普通文件且不超过 20 MiB"
        );
        let bytes = tokio::fs::read(path).await?;
        let uploaded = self
            .writer
            .upload(&self.ctx, bytes, name, "application/octet-stream")
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        uploaded["file"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("上传未返回资源"))
    }
    async fn perform(&mut self, action: &Action, turns: &[Turn]) -> Result<Value> {
        let group = self.group.to_string();
        let (method, params, summary) = match action {
            Action::Send { parts, reply_to } => {
                // 模型偶尔把工具调用当正文写出来（`[satori_action:{…}]`）。那是协议，
                // 不是要说的话，而且必须在断句之前摘：JSON 里的逗号看着像换气处，
                // 先切会把标记切成两半，后半截没有名字，照样漏进群。
                // 行首的 `[reply]` 同理：那是文字路径的引用写法，这里摘掉，想引谁
                // 就换成真引用（`reply_to` 已经给了就以它为准）。
                // 接口把拒绝句当回复递回来时，那是报错不是话；发出去群友只会看到一句英文。
                if parts.iter().any(|part| {
                    matches!(part, Part::Text { text } if super::protocol::is_provider_noise(text))
                }) {
                    return Ok(json!({"status":"skipped","note":"那是接口的报错，不是要说的话"}));
                }
                let mut marked = None;
                let parts: Vec<Part> = parts
                    .iter()
                    .map(|part| match part {
                        Part::Text { text } => {
                            let text = super::protocol::strip(text);
                            let (text, quote) = super::protocol::take_reply_markers(&text);
                            marked = marked.take().or(quote);
                            Part::Text {
                                text: text.into_owned(),
                            }
                        }
                        other => other.clone(),
                    })
                    .collect();
                let reply_to = reply_to.clone().or_else(|| quote_for(marked?, turns));
                // 人格那边决定这条引用留不留：两个人对聊时每句都挂引用，是机器人的样子。
                let reply_to = reply_to.filter(|id| {
                    self.persona
                        .as_ref()
                        .is_none_or(|persona| persona.keeps_quote(id, turns))
                });
                let reply_to = &reply_to;
                // 摘干净之后什么都不剩（整条只有一个伪调用）就没有话要发。
                if parts.is_empty()
                    || parts
                        .iter()
                        .all(|part| matches!(part, Part::Text { text } if text.trim().is_empty()))
                {
                    return Ok(json!({"status":"skipped","note":"没有可发送的内容"}));
                }
                // 一整段话按换气处分成几条发出去。模型写得越顺，越容易把两三个意思
                // 塞进一条；群里没人这么说话。切法见 [`super::breath`]，切几条受本轮
                // 剩下的消息额度约束——真正发出去的条数才是额度算的东西。
                let budget = self
                    .config
                    .messages_budget
                    .clamp(1, 5)
                    .saturating_sub(self.messages.saturating_sub(1));
                if let Some(rows) = split_send(&parts, budget, self.config.split_chars) {
                    return self.send_in_pieces(rows, reply_to.as_deref()).await;
                }
                let mut msg = Message::new();
                if let Some(id) = reply_to {
                    msg = msg.reply(id);
                }
                for (index, part) in parts.iter().enumerate() {
                    msg = match part {
                        Part::Text { text } => {
                            // 落在这一条路径上说明这条 send 并不到此为止（后面挂着图片、
                            // 文件、转发那类段落），只能整条发。模型把话分成几段文字时，
                            // 段与段之间是它自己的换气：补一个换行，别让两句话黏成一句。
                            if index > 0 && matches!(parts.get(index - 1), Some(Part::Text { .. }))
                            {
                                msg = msg.text("\n");
                            }
                            msg.0.extend(super::pace::text_segments(text).0);
                            msg
                        }
                        Part::At { user_id } => {
                            let msg = msg.at(user_id);
                            if at_needs_gap(&parts, index) {
                                msg.text(" ")
                            } else {
                                msg
                            }
                        }
                        Part::Face { id } => msg.face(id),
                        Part::Image { source } => {
                            msg.image(self.source(source, "image.png").await?)
                        }
                        Part::File { source, name } => {
                            msg.file(self.source(source, name).await?, Some(name))
                        }
                        Part::Audio { source } => {
                            msg.record(self.source(source, "audio.mp3").await?)
                        }
                        Part::Video { source } => {
                            msg.video(self.source(source, "video.mp4").await?)
                        }
                        Part::Sticker {
                            message_id,
                            index,
                            id,
                            note,
                        } => {
                            msg.0
                                .extend(self.sticker(turns, *id, message_id, *index, note).await?.0);
                            msg
                        }
                        Part::Dice => msg.dice(),
                        Part::Rps => msg.rps(),
                    };
                }
                return self.send(msg).await;
            }
            Action::Forward { message_ids, texts } => {
                let mut msg = Message::new();
                for id in message_ids {
                    let t = actions::message(turns, id)?;
                    msg = msg.node_custom(&t.user_id, &t.name, t.elements.clone());
                }
                let login = self.ctx.bot.login_user.get();
                for text in texts {
                    msg = msg.node_custom(
                        &login.id,
                        login.name.as_deref().unwrap_or("我"),
                        super::pace::text_segments(text),
                    );
                }
                return self.send(msg).await;
            }
            Action::Poke { user_id } => (
                "internal/poke",
                json!({"guild_id":group,"user_id":user_id}),
                format!("[戳一戳 {user_id}]"),
            ),
            Action::Like { user_id, times } => (
                "internal/like",
                json!({"user_id":user_id,"times":times}),
                format!("[给 {user_id} 资料卡点赞 {times} 次]"),
            ),
            Action::React {
                message_id,
                emoji_id,
                remove,
            } => (
                if *remove {
                    "reaction.delete"
                } else {
                    "reaction.create"
                },
                json!({"channel_id":group,"message_id":message_id,"emoji_id":emoji_id}),
                format!(
                    "[{}消息 {message_id} 的表态 {emoji_id}]",
                    if *remove { "取消" } else { "添加" }
                ),
            ),
            Action::Sign => (
                "internal/sign",
                json!({"guild_id":group}),
                "[群签到]".into(),
            ),
            Action::Card { user_id, card } => (
                "internal/card",
                json!({"guild_id":group,"user_id":user_id.as_deref().unwrap_or(&self.ctx.bot.login_user.get().id),"card":card}),
                format!("[设置群名片：{card}]"),
            ),
            Action::Title { user_id, title } => (
                "internal/special_title",
                json!({"guild_id":group,"user_id":user_id,"title":title}),
                format!("[设置 {user_id} 的群头衔：{title}]"),
            ),
            Action::Essence { message_id, remove } => (
                "internal/essence",
                json!({"guild_id":group,"message_id":message_id,"remove":remove}),
                format!(
                    "[{}精华消息 {message_id}]",
                    if *remove { "取消" } else { "设置" }
                ),
            ),
            Action::Mute {
                user_id,
                duration_seconds,
            } => (
                "guild.member.mute",
                json!({"guild_id":group,"user_id":user_id,"duration":u64::from(*duration_seconds)*1000}),
                format!("[禁言 {user_id} {duration_seconds} 秒；0 为解除]"),
            ),
            Action::Kick { user_id, permanent } => (
                "guild.member.kick",
                json!({"guild_id":group,"user_id":user_id,"permanent":permanent}),
                format!("[移出群成员 {user_id}]"),
            ),
            Action::MuteAll { duration_seconds } => (
                "channel.mute",
                json!({"channel_id":group,"duration":u64::from(*duration_seconds)*1000}),
                format!("[全员禁言 {duration_seconds} 秒；0 为解除]"),
            ),
            Action::RenameGroup { name } => (
                "channel.update",
                json!({"channel_id":group,"data":{"name":name}}),
                format!("[群名改为 {name}]"),
            ),
            Action::ReactClear {
                message_id,
                emoji_id,
            } => {
                if emoji_id.is_none()
                    && self
                        .own_reactions
                        .get(message_id)
                        .is_some_and(|ids| !ids.is_empty())
                {
                    return self.clear_known_reactions(message_id).await;
                }
                let mut params = json!({"channel_id":group,"message_id":message_id});
                if let Some(emoji) = emoji_id {
                    params["emoji_id"] = json!(emoji);
                }
                (
                    if emoji_id.is_some() { "reaction.delete" } else { "internal/reaction_clear" },
                    params,
                    format!("[清除自己在消息 {message_id} 上的表态]"),
                )
            }
            Action::Recall { message_id } => (
                "message.delete",
                json!({"channel_id":group,"message_id":message_id}),
                format!("[撤回自己的消息 {message_id}]"),
            ),
        };
        ensure!(
            !method.starts_with("internal/") || self.ctx.bot.adapter == "satori-qq",
            "当前适配器未声明 QQ 扩展"
        );
        ensure!(
            self.enabled() && (matches!(action, Action::Recall { .. }) || self.current()),
            "群聊已更新，动作未执行；请读 satori_context"
        );
        ensure!(
            !action.requires_management(&self.ctx.bot.login_user.get().id)
                || self.management_enabled(),
            "本群管理动作已停用"
        );
        let result = self.rpc(method, params).await?;
        match action {
            Action::React {
                message_id,
                emoji_id,
                remove,
            } => {
                let emojis = self.own_reactions.entry(message_id.clone()).or_default();
                emojis.retain(|id| id != emoji_id);
                if !remove {
                    emojis.push(emoji_id.clone());
                }
            }
            Action::ReactClear {
                message_id,
                emoji_id,
            } => {
                if let Some(emoji) = emoji_id {
                    if let Some(ids) = self.own_reactions.get_mut(message_id) {
                        ids.retain(|id| id != emoji);
                    }
                } else {
                    self.own_reactions.remove(message_id);
                }
            }
            _ => {}
        }
        if let Action::Recall { message_id } = action {
            window::with_group(&self.group, |s| s.recall(message_id.trim()));
        }
        self.record(summary, String::new(), Message::new(), true);
        Ok(json!({"status":"confirmed","data":result}))
    }
    /// QQ 的表态缓存可能晚于添加回执。仅对明确的“无已知表态”补偿，超时不重放。
    async fn clear_known_reactions(&mut self, message_id: &str) -> Result<Value> {
        ensure!(self.current(), "群聊已更新，请先读 satori_context");
        let params = json!({"channel_id":self.group.to_string(),"message_id":message_id});
        match self.rpc("internal/reaction_clear", params.clone()).await {
            Ok(data) => {
                self.own_reactions.remove(message_id);
                self.record(
                    format!("[清除自己在消息 {message_id} 上的表态]"),
                    String::new(),
                    Message::new(),
                    true,
                );
                Ok(json!({"status":"confirmed","data":data}))
            }
            Err(error)
                if error
                    .to_string()
                    .contains("no reaction set by this login on the message")
                    || error.to_string().contains("(404") =>
            {
                let known = self
                    .own_reactions
                    .get(message_id)
                    .cloned()
                    .unwrap_or_default();
                let mut cleared = Vec::new();
                for emoji in known {
                    ensure!(
                        self.current(),
                        "群聊已更新，已取消的表态：{cleared:?}；其余未执行"
                    );
                    let mut item = params.clone();
                    item["emoji_id"] = json!(emoji);
                    self.rpc("reaction.delete", item)
                        .await
                        .with_context(|| format!("已取消的表态：{cleared:?}；其余未确认"))?;
                    if let Some(ids) = self.own_reactions.get_mut(message_id) {
                        ids.retain(|id| id != &emoji);
                    }
                    cleared.push(emoji);
                }
                self.record(
                    format!("[取消消息 {message_id} 的本轮表态 {cleared:?}；其他表态未知]"),
                    String::new(),
                    Message::new(),
                    true,
                );
                Ok(
                    json!({"status":"partial","cleared":cleared,"scope":"this_turn",
                    "note":"QQ 表态缓存尚未提供列表；已取消本轮成功添加的表态，其他表态是否存在仍未知。"}),
                )
            }
            Err(error) => Err(error),
        }
    }

    /// 把切好的几条依次发出去，当作模型的同一次 send。
    ///
    /// 额度按真正发出去的条数扣：模型多写了两个意思，就少一次另开话头的机会。
    /// 中途失败不回滚已经发出去的——那些群友已经看见了，只在回执里说清楚发到哪。
    async fn send_in_pieces(
        &mut self,
        rows: Vec<Vec<Part>>,
        reply_to: Option<&str>,
    ) -> Result<Value> {
        let mut ids: Vec<String> = Vec::new();
        let mut failure = None;
        for (index, row) in rows.iter().enumerate() {
            let mut message = Message::new();
            if index == 0
                && let Some(id) = reply_to
            {
                message = message.reply(id);
            }
            for (position, part) in row.iter().enumerate() {
                // [`split_send`] 只会放行 At/Face/Text，别的段不进这条路径。
                message = match part {
                    Part::At { user_id } => {
                        let message = message.at(user_id);
                        if at_needs_gap(row, position) {
                            message.text(" ")
                        } else {
                            message
                        }
                    }
                    Part::Face { id } => message.face(id),
                    Part::Text { text } => {
                        let mut message = message;
                        message.0.extend(super::pace::text_segments(text).0);
                        message
                    }
                    _ => message,
                };
            }
            if index > 0 {
                self.messages += 1;
            }
            match self.send(message).await {
                Ok(value) => ids.push(value["message_id"].as_str().unwrap_or_default().to_string()),
                Err(error) => {
                    if index > 0 {
                        self.messages -= 1;
                    }
                    failure = Some(error);
                    break;
                }
            }
        }
        // 一条都没发出去时保持原样报错：调用方要按它决定退不退额度。
        let Some(first) = ids.first().cloned() else {
            return Err(failure.unwrap_or_else(|| anyhow::anyhow!("没有可发送的内容")));
        };
        let note = match failure {
            None => format!("这段话在换气处分成 {} 条发出，算你这一次发言", ids.len()),
            Some(error) => format!(
                "前 {} 条已经发出去了，剩下的没发成：{error}。发出去的那几条就留着",
                ids.len()
            ),
        };
        Ok(json!({"status":"confirmed","message_id":first,"message_ids":ids,"note":note}))
    }
    async fn send(&mut self, message: Message) -> Result<Value> {
        let spoken = super::plain_text(&message);
        let pace = self
            .persona
            .as_ref()
            .map(|persona| persona.pace(&self.group))
            .unwrap_or_default();
        let typing = pace.typing_delay(spoken.chars().count());
        // 模型耗时算在「读和想」里，字还得一个个敲：首条不再从打字时间里扣掉它。
        let delay = if self.spoke {
            pace.gap() + typing
        } else {
            pace.think_delay(self.started.elapsed()) + typing
        };
        // 思考/停顿结束、真的开始打字时才通知 QQ。实验性 JNI 能力是
        // best-effort：群里有新话题、实现端不支持或回调超时，都不应阻塞这句回复。
        let input_time = typing.min(delay);
        tokio::time::sleep(delay.saturating_sub(input_time)).await;
        ensure!(
            self.sendable(),
            "准备发送期间群聊已更新，尚未发送；请读 satori_context"
        );
        if self.config.qq_mark_read && !self.read_marked
            && self.ctx.bot.adapter == "satori-qq"
        {
            self.read_marked = true; // one attempt per session, even on timeout
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                crate::adapters::satori::qq::mark_read(
                    &self.ctx, &self.writer, &self.group.to_string(),
                ),
            ).await;
        }
        if self.config.qq_typing && self.ctx.bot.adapter == "satori-qq"
            && input_time >= std::time::Duration::from_secs(1)
        {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                crate::adapters::satori::qq::typing(
                    &self.ctx, &self.writer, &self.group.to_string(),
                ),
            ).await;
        }
        tokio::time::sleep(input_time).await;
        ensure!(
            self.sendable(),
            "准备发送期间群聊已更新，尚未发送；请读 satori_context"
        );
        let receipt = send_fresh_msg_id(
            &self.ctx,
            self.writer.clone(),
            Some(&self.group),
            None,
            &message,
            freshness_for(&self.group, std::time::Duration::from_secs(self.config.freshness_seconds)),
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        let id = receipt.ok_or_else(|| {
            anyhow::anyhow!(
                "这一句没有发出去：交给 QQ 之前群里又有人说话（或被插件拦截）。\
                 先读 satori_context 看看现在在聊什么，再决定要不要说"
            )
        })?;
        self.record(spoken, id.clone(), message, true);
        Ok(json!({"status":"confirmed","message_id":id}))
    }
    fn record(&mut self, text: String, message_id: String, elements: Message, success: bool) {
        let me = self.ctx.bot.self_id();
        info!(target: super::LOG_TARGET, "群 {} 动作：{}", &self.group, text);
        if success && !self.spoke {
            // 锁不可重入：记忆与状态都在 window 的锁外面更新。
            let target = window::with_group(&self.group, |s| {
                s.recent(20)
                    .iter()
                    .rev()
                    .find(|turn| !turn.from_me)
                    .map(|turn| turn.user_id.clone())
            });
            if let Some(persona) = &self.persona {
                persona.spoke(&self.group);
            }
            if self.config.memory_enabled
                && let Some(id) = target
            {
                let now = chrono::Local::now().timestamp();
                memory::edit(&self.group, |memory| memory.exchange(&id, now));
            }
        }
        window::with_group(&self.group, |s| {
            if success && !self.spoke {
                s.mark_spoke();
            }
            s.receive(Turn {
                user_id: me,
                name: "我".into(),
                text: text.chars().take(1000).collect(),
                elements,
                message_id,
                from_me: true,
                at: chrono::Local::now().timestamp(),
                ..Turn::default()
            });
        });
        self.spoke |= success;
    }

    /// 把生成的图片（远程直链或内联 base64）落盘到本轮的工作目录，供随后用工具发送。
    /// 直接发远程直链会受签名过期与防盗链影响，先下载下来再由 satori_action 上传更稳。
    /// 一次媒体生成的等待上限。
    ///
    /// 取本轮发言的总预算：生成得再久也不该超过这一轮自己能活的时间。外层还有一道
    /// 同样的超时兜着，这里先到点就能给模型一句「等太久了」，而不是整轮被掐掉。
    fn media_deadline(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.media_deadline_seconds.clamp(30, 1_800))
    }

    /// 把成品写进 本轮的工作目录，返回本地路径。
    ///
    /// 远端的直链多半带签名、过一会儿就失效，QQ 那边也未必拉得到；先落本地，
    /// 模型随后用 `satori_action` 发出去时走的是 `upload.create`，稳。
    async fn save_media(&self, bytes: &[u8], name: &str) -> Result<String> {
        ensure!(!bytes.is_empty(), "成品是空的");
        tokio::fs::create_dir_all(&self.media)
            .await
            .context("创建素材目录失败")?;
        let path = self.media.join(name);
        tokio::fs::write(&path, bytes)
            .await
            .context("写入成品失败")?;
        Ok(path.to_string_lossy().into_owned())
    }

    /// 下载一份远端成品并存进本地素材目录。失败只让这一项缺席，不拖垮整次生成。
    async fn save_remote(&self, url: &str, name: &str) -> Option<String> {
        match fetch_media(url).await {
            Ok(bytes) => match self.save_media(&bytes, name).await {
                Ok(path) => Some(path),
                Err(error) => {
                    warn!(target: super::LOG_TARGET, "写入素材失败 {name}: {error:#}");
                    None
                }
            },
            Err(error) => {
                warn!(target: super::LOG_TARGET, "下载素材失败 {url}: {error:#}");
                None
            }
        }
    }

    async fn save_image_to_media(&self, url: &str, index: usize) -> Result<String> {
        let bytes = fetch_media(url).await.context("下载生成图片失败")?;
        let name = format!(
            "draw-{}-{index}.{}",
            chrono::Local::now().format("%Y%m%d%H%M%S"),
            super::image_extension(&bytes)
        );
        self.save_media(&bytes, &name)
            .await
            .context("写入生成图片失败")
    }
}

/// 下载一份远端成品；`data:` 内联的也认。
///
/// 超时给得比图片宽：同一个函数也用来取几分钟的歌与几 MB 的视频。
async fn fetch_media(url: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    if let Some((meta, payload)) = url.split_once(',')
        && meta.starts_with("data:")
    {
        return base64::engine::general_purpose::STANDARD
            .decode(payload)
            .context("解码内联媒体失败");
    }
    let response = crate::http::client()
        .get(url)
        .header(
            reqwest::header::USER_AGENT,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36",
        )
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .context("下载成品失败")?;
    if !response.status().is_success() {
        anyhow::bail!("下载成品失败：HTTP {}", response.status().as_u16());
    }
    Ok(response.bytes().await.context("读完成品失败")?.to_vec())
}

/// 挑一个能用某个专用接口的模型，连同接口地址与密钥一起给出。
///
/// 先在站点实际在售的列表（`config.models` 是过滤后的那份）里按关键字挑，挑不到就用
/// 兜底 id——音乐与视频的模型不在 `[oai] model_filter.keep` 里，兜底是常态而不是异常，
/// 所以兜底必须是真实在售的 id，不能拿关键字顶上。
async fn media_endpoint(
    pick: fn(&str, &[String]) -> bool,
    keywords: &[String],
    fallback: &str,
    label: &str,
) -> Result<(String, String, String)> {
    ensure!(
        keywords.iter().any(|keyword| !keyword.trim().is_empty()),
        "未配置{label}模型，请在 [oai] 里指定"
    );
    let mgr = crate::plugins::oai::data::MANAGER
        .get()
        .ok_or_else(|| anyhow::anyhow!("OAI 还没就绪，{label}这会儿用不了"))?;
    let config = mgr.config.read().await;
    ensure!(
        !config.api_base.trim().is_empty() && !config.api_key.trim().is_empty(),
        "OAI 接口地址或密钥未配置"
    );
    let model = config
        .models
        .iter()
        .find(|model| pick(model, keywords))
        .cloned()
        .unwrap_or_else(|| fallback.to_string());
    Ok((config.api_base.clone(), config.api_key.clone(), model))
}

/// 正文行首的 `[reply]` 想引的那条 → `reply_to`。点名的消息号得真在眼前的记录里，
/// 否则一个随口写的号会让整条消息引到不存在的地方；`[reply]` 引最新那条群友消息。
fn quote_for(quote: super::protocol::Quote, turns: &[Turn]) -> Option<String> {
    match quote {
        super::protocol::Quote::Message(id) => {
            turns.iter().any(|turn| turn.message_id == id).then_some(id)
        }
        super::protocol::Quote::Latest => turns
            .iter()
            .rev()
            .find(|turn| !turn.from_me && !turn.message_id.is_empty())
            .map(|turn| turn.message_id.clone()),
    }
}

/// `@` 后面紧跟文字时要不要垫一个空格。
///
/// `at` 段身上只有 QQ 号，那个空格谁都不带：模型按 skill 写成
/// `[{at:…},{"text":"那你说 是谁"}]`，发出去在群里就是「@某人那你说 是谁」，黏成一块。
/// 只认「紧挨着的下一个元素是文字、而文字自己没留白」这一种；`@` 收尾、`@` 后面
/// 接表情或图片都不动——那些本来就该贴着。
fn at_needs_gap(parts: &[Part], index: usize) -> bool {
    matches!(parts.get(index), Some(Part::At { .. }))
        && matches!(
            parts.get(index + 1),
            Some(Part::Text { text })
                if !text.is_empty() && !text.starts_with(char::is_whitespace)
        )
}

/// 一条 `send` 要不要按换气切成几条；要切就给出每一条的元素表。
///
/// 换气有两处，都算数：模型自己把话分成了几段文字（`parts` 里不止一个 `Text`），
/// 以及一段文字内部的句末标点、汉字之间的空格与句中逗号（见 [`super::breath`]）。
/// 前者是它自己分好的版，先满足，这轮剩下的消息额度留给后者。`@` 与表情跟在它们
/// 后面那段文字上，跟着那一条走。带图片/文件/转发那类段的一律不切——那些段的归属
/// 没法靠断句猜，宁可整条发。
///
/// 从前只认「恰好一段文字」，模型分好的几段会被合并回一条消息、中间什么都不剩，
/// 于是「扫码连热点就搬」和「不过跨品牌搬不全」连成了一句（线上记录 id 95426）。
fn split_send(parts: &[Part], budget: usize, target: usize) -> Option<Vec<Vec<Part>>> {
    if !parts.iter().all(|part| {
        matches!(
            part,
            Part::At { .. } | Part::Face { .. } | Part::Text { .. }
        )
    }) {
        return None;
    }
    // 模型自己的分段：每一段文字起一条，走到它前面的 `@` 与表情跟着它。
    // 空文字段（偶尔写成空串或一段光换行）不是一条消息，丢掉，别发一个空泡。
    let mut rows: Vec<Vec<Part>> = Vec::new();
    for part in parts {
        if matches!(part, Part::Text { text } if text.trim().is_empty()) {
            continue;
        }
        let starts_a_row = matches!(part, Part::Text { .. })
            && rows
                .last()
                .is_none_or(|row| row.iter().any(|part| matches!(part, Part::Text { .. })));
        if starts_a_row || rows.is_empty() {
            rows.push(Vec::new());
        }
        rows.last_mut().expect("上面刚保证非空").push(part.clone());
    }
    // 分出来的段比剩下的额度还多时，多出来的并进上一条：中间留一个换行，
    // 免得并起来又黏成一句。额度是硬的，多一条都不发。
    while rows.len() > budget.max(1) {
        let tail = rows.pop().expect("上面刚判过非空");
        let head = rows.last_mut().expect("额度至少为一");
        match head.last_mut() {
            Some(Part::Text { text }) => text.push('\n'),
            _ => head.push(Part::Text { text: "\n".into() }),
        }
        head.extend(tail);
    }
    // 还有剩的额度就替写长了的那几段换气。
    let mut spare = budget.saturating_sub(rows.len());
    let mut out: Vec<Vec<Part>> = Vec::new();
    for row in rows {
        let text = match row.last() {
            // 只有「文字收尾」的行切得动：后面还挂着 at/face 的行，切开之后
            // 那几段归谁说不清。
            Some(Part::Text { text }) => text,
            _ => {
                out.push(row);
                continue;
            }
        };
        let prefix = &row[..row.len() - 1];
        let pieces = super::breath::split(text, spare + 1, target);
        spare = spare.saturating_sub(pieces.len().saturating_sub(1));
        for (index, piece) in pieces.into_iter().enumerate() {
            let mut line: Vec<Part> = if index == 0 {
                prefix.to_vec()
            } else {
                Vec::new()
            };
            line.push(Part::Text { text: piece });
            out.push(line);
        }
    }
    // 只切出一条就交回调用方按普通发送走，两条路径的结果一模一样。
    (out.len() > 1).then_some(out)
}
