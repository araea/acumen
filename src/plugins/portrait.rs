//! 角色画像：读一个群成员的历史发言，给他出一份档案，出一张图文报告。
//!
//! 指令只有一条，`画像`。不带参数是查自己，@ 一个人或直接写 QQ 号是查别人。
//!
//! 这份东西是发在群里给大家看着玩的，所以它敢下判断、敢夸张；同时每一句都留了出处。
//! 报告分三层：
//!
//! - **观测**：[`collect`] 从库里数出来的四块——语言指纹、活跃节律、口头禅候选、群内往来。
//!   都不经模型，可核验，模型接不接都一样在。
//! - **档案**：[`persona`] 把观测与样本交给模型，换回十个维度各一句判定
//!   （性格 / 兴趣 / 好恶 / 生计 / 家庭 / 年岁 / 学历 / 经历 / 志向 / 人际），每条配一条
//!   依据与一档把握（明说 / 可推 / 待考）。
//! - **戏说**：同一份输出里的标签墙与小传。这一节明写着是玩笑，允许夸张。
//!
//! [`card`] 把三层排成一张 HTML 报告图；[`avatar`] 取对象与往来对象的 QQ 头像配在开头。
//!
//! 三层之外还有一处笔法，落在综述与判词上：那是**亮刀**的地方。白描留给档案那一节，
//! 这一节允许隐喻、典故、反问、反语——但放开的只是写法，不是出处：每句判断底下仍然要
//! 压着事实，比喻是把事实照亮，不是拿来替事实的。准星只有一条，**靶子是他的做法与处境，
//! 不是他这个人的价值**（这一份会发在群里，他自己也会看到）。整份画像收在末尾那句判词上，
//! 它要刺一下，也要留一点暖。
//!
//! 四条线，两处刻意为之：
//!
//! - 不编数字（观测四块全由 [`collect`] 算出来）、不编原话（引语、口头禅、以及「明说」
//!   的依据都逐字比对样本）、不冒充把握（标了「明说」却拿不出原话的，收口时降成「可推」）、
//!   不拿别人的脸开玩笑（外貌与健康不写）。
//! - **不再给任何人定 MBTI 与九型**。凭一个人打过的字给他一个四字母的型，是把一次粗糙的
//!   归类说得像一次测量。要判断一个人是什么样，看他亲口说过什么，比看他在四个轴上的位置
//!   诚实得多。
//! - 那句「正在生成」在成品发出去之后会被撤回：它是一句进度，留在群里是噪声。过了 QQ 的
//!   两分钟就不去试（实现端的出站闸门会把连续的撤回失败算成故障，见 [`retract`]）。
//! - 同一个目标同时在跑只允许一次，`cooldown_seconds` 之内也不重复，免得群里连着刷。
//!   这两道闸只影响发指令的人，不影响其它功能。

pub mod avatar;
pub mod card;
pub mod collect;
pub mod persona;

use crate::adapters::satori::{LockedWriter, send_msg, send_msg_id};
use crate::config::build_config;
use crate::event::{Context, EventType};
use crate::message::Message;
use crate::plugins::{ChannelConfig, PluginError, get_config_or_default};
use futures_util::future::BoxFuture;
use rig_core::completion::Message as LlmMessage;
use rig_core::completion::message::{Text, UserContent};
use serde::{Deserialize, Serialize};
use simd_json::derived::{ValueObjectAccess, ValueObjectAccessAsArray, ValueObjectAccessAsScalar};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use toml::Value;

const LOG_TARGET: &str = "Plugin/Portrait";

/// 一次补全的上限。画像是一发一收的整段生成，给足时间但不无限等。
const COMPLETION_TIMEOUT: Duration = Duration::from_secs(90);
/// 归档目录里最多留多少份 HTML，超出的按文件名（含时间戳）从旧到新删。
const ARCHIVE_KEEP: usize = 40;

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct PortraitConfig {
    enabled: bool,
    /// 分析用的模型，写 `供应商/模型`；供应商取自 `[oai.providers]`，
    /// 不带前缀则沿用 oai 默认接口。
    model: String,
    /// 思考强度：`off` / `minimal` / `low` / `medium` / `high` / `xhigh`，留空交给接口默认。
    /// 侧写要读一百多条样本再落笔，默认给到 `high`；调低会明显变浅。
    thinking: String,
    /// 只统计最近多少天，0 表示全部留存记录。
    days: i64,
    /// 交给模型的发言样本条数上限。
    max_samples: usize,
    /// 一次最多从库里读多少条原始记录。
    max_scan: u64,
    /// 报告里最多摆几个往来对象（他和谁聊得来那一节）。
    partners: usize,
    /// 报告主题：`auto` 按北京时间在日读与夜读之间切换，也可固定 `light` / `dark`。
    theme: String,
    /// 是否把画像排版成卡片图；关掉或渲染失败时退回一份等价的文字版。
    image_enabled: bool,
    /// 出图倍率（1—4）。倍率越高越清晰，图也越大。
    image_scale: f64,
    /// 同一个目标两次生成之间的最短间隔秒数。
    cooldown_seconds: u64,
    /// 群名单，语义同其它插件。
    channel: ChannelConfig,
}

impl Default for PortraitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: "deepseek/deepseek-flash".to_string(),
            thinking: "high".to_string(),
            days: 0,
            max_samples: 120,
            max_scan: 8_000,
            partners: 6,
            theme: "auto".to_string(),
            image_enabled: true,
            image_scale: 3.0,
            cooldown_seconds: 180,
            channel: ChannelConfig::default(),
        }
    }
}

pub fn default_config() -> Value {
    build_config(PortraitConfig::default())
}

pub fn validate_config(value: &Value) -> Result<(), String> {
    <PortraitConfig as serde::Deserialize>::deserialize(value.clone())
        .map(|_| ())
        .map_err(|_| "配置类型不匹配（请检查天数、条数与倍率）".to_string())
}

// ================= 指令解析 =================

/// 画像的别名。长的写在前面，前缀匹配才不会被短的抢走。
const KEYWORDS: [&str; 7] = [
    "用户画像报告",
    "用户画像",
    "角色画像",
    "人物画像",
    "我的画像",
    "画像报告",
    "画像",
];

#[derive(Debug, Clone, PartialEq)]
pub struct Mention {
    pub user_id: i64,
    /// @ 段里带的群名片，能让「正在生成」那句话像人话。
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// 查自己。
    Mine,
    /// 查别人。
    Other(i64, Option<String>),
}

/// 从指令文本里认出画像请求。
///
/// 判定很严：关键词之后只允许空白、`@`、一串数字，或者「报告」两个字。宁可漏认，
/// 也不要在日常闲聊里把「画像」这个普通词吃掉——群里说一句「这游戏的画像有点丑」
/// 不该触发一次模型调用。
///
/// 唯一的例外是消息里带了明确的 `@`：平台会把「@某人的名字」也写进文本段，
/// 于是关键词后面跟的常常是「黑猫警长」加他随口的半句话。目标是谁已经写在
/// `mentions` 里，这时候不再挑剔后面那点尾巴。
pub fn parse_command(content: &str, mentions: &[Mention]) -> Option<Request> {
    let compact: String = crate::plugins::oai::utils::normalize(content)
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    let rest = KEYWORDS
        .iter()
        .find_map(|word| compact.strip_prefix(word))?;
    let rest = rest.trim_start_matches('@');
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let tail = rest[digits.len()..].trim();
    if !tail.is_empty() && tail != "报告" && mentions.is_empty() {
        return None;
    }
    if let Some(mention) = mentions.first() {
        return Some(Request::Other(mention.user_id, mention.name.clone()));
    }
    if let Ok(user_id) = digits.parse::<i64>()
        && user_id > 0
    {
        return Some(Request::Other(user_id, None));
    }
    Some(Request::Mine)
}

/// 取出指令正文。
///
/// 平台把「@某人」的名字也写进文本段，于是群里看到的正文常常长这样：
/// 「/画像 @小黑 你说呢」或者「@小黑 /画像」。前一种靠前缀剥离就够，
/// 后一种的前缀躲在这个名字后面，得跳过名字再剥一次。
fn command_body<'a>(ctx: &Context, text: &'a str) -> Option<&'a str> {
    body_after_prefixes(&crate::command::get_prefixes(ctx), text)
}

/// [`command_body`] 的本体，抽出来是为了能单测。语义与 `command::strip_prefix` 一致，
/// 只在剥不动的时候多试一次「跳过开头那个 @名字」。
fn body_after_prefixes<'a>(prefixes: &[String], text: &'a str) -> Option<&'a str> {
    let strip = |value: &'a str| -> Option<&'a str> {
        let value = value.trim();
        if prefixes.is_empty() {
            return Some(value);
        }
        prefixes
            .iter()
            .find_map(|p| value.strip_prefix(p.as_str()).map(|rest| rest.trim_start()))
    };
    if let Some(rest) = strip(text) {
        return Some(rest);
    }
    let rest = text.trim_start().strip_prefix('@')?;
    let boundary = rest.find(char::is_whitespace)?;
    strip(&rest[boundary..])
}

/// 从事件的消息段里取出纯文本与 @ 目标。
///
/// 直接用 `raw_message` 不行：它把 @ 渲染成一个名字，关键词就不在开头了，
/// 前缀匹配会失效。这里跳过 @ 与引用段自己拼文本，顺带把 @ 的目标捞出来。
/// **注意平台不填 `name`**，@ 的显示名字会混在文本段里，由 [`parse_command`] 容忍。
///
/// `me` 是机器人自己的 QQ 号：群里喊指令习惯先 @ 一下机器人，那个 @ 不是画像的
/// 对象，要排除掉，否则「@机器人 画像」会去分析机器人自己。
fn read_message(event: &crate::event::Event, me: Option<i64>) -> (String, Vec<Mention>) {
    let mut text = String::new();
    let mut mentions = Vec::new();
    let Some(segments) = event.get_array("message") else {
        return (text, mentions);
    };
    for segment in segments {
        let kind = segment.get_str("type").unwrap_or_default();
        let data = segment.get("data");
        match kind {
            "text" => text.push_str(data.and_then(|d| d.get_str("text")).unwrap_or_default()),
            "at" => {
                // qq 可能是字符串，也可能是数字，与 command.rs 里的取法保持一致。
                let qq = data
                    .and_then(|d| d.get_str("qq").map(str::to_string))
                    .or_else(|| data.and_then(|d| d.get_i64("qq")).map(|v| v.to_string()))
                    .or_else(|| data.and_then(|d| d.get_u64("qq")).map(|v| v.to_string()))
                    .unwrap_or_default();
                if qq.eq_ignore_ascii_case("all") {
                    continue;
                }
                if let Ok(user_id) = qq.parse::<i64>()
                    && user_id > 0
                    && Some(user_id) != me
                {
                    let name = data
                        .and_then(|d| d.get_str("name"))
                        .map(str::to_string)
                        .filter(|name| !name.trim().is_empty());
                    mentions.push(Mention { user_id, name });
                }
            }
            _ => {}
        }
    }
    (text, mentions)
}

// ================= 频次闸门 =================

/// 上一次真正发出去的那份成品。冷却期内再问，就把这份原样再发一次——同一批素材
/// 起的还是同一卦，为它再花一次模型与出图的钱没有意义。
#[derive(Clone)]
enum Cached {
    /// 卡片图，与消息里发出去的是同一份 base64。
    Card(String),
    /// 出图失败时退回的那段文字报告。
    Report(String),
}

#[derive(Default)]
struct Gate {
    busy: HashSet<i64>,
    last: HashMap<i64, Instant>,
    /// 每个人最近一次的成品，按发出去的先后从旧到新排。卡片是几 MB 的 base64，
    /// 所以只留最近这几个。
    served: Vec<(i64, Cached)>,
    /// 最近几份画像用过的称号、一句话与判词，从旧到新。它只干一件事：下一份画像的提示词
    /// 里带上它，让模型换个说法——十来个人拿到十来个「夜猫子」、十来句同一个句式的判词，
    /// 这份东西就不好玩了。
    recent: Vec<persona::Recent>,
}

/// 成品缓存留几个。冷却默认三分钟，够覆盖「同一批人反复问」的场面。
const SERVED_KEEP: usize = 6;
/// 排除表留几条。给多了提示词变长，给少了挡不住撞车。
const RECENT_KEEP: usize = 8;

/// 开始生成时发的一张「票」，走到哪一步都不会忘了还——哪怕是提前 return
/// 或者 panic，`Drop` 都会把忙碌标记摘掉。
struct Ticket {
    user_id: i64,
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if let Some(gate) = GATE.get() {
            gate.lock().unwrap().busy.remove(&self.user_id);
        }
    }
}

enum Entry {
    Go(Ticket),
    Busy,
    Cooling(u64),
}

static GATE: OnceLock<Mutex<Gate>> = OnceLock::new();

fn gate() -> &'static Mutex<Gate> {
    GATE.get_or_init(|| Mutex::new(Gate::default()))
}

fn enter(user_id: i64, cooldown: Duration) -> Entry {
    let mut gate = gate().lock().unwrap();
    if gate.busy.contains(&user_id) {
        return Entry::Busy;
    }
    let now = Instant::now();
    if let Some(last) = gate.last.get(&user_id)
        && let Some(left) = cooldown.checked_sub(now.saturating_duration_since(*last))
        && !left.is_zero()
    {
        return Entry::Cooling(left.as_secs().max(1));
    }
    gate.busy.insert(user_id);
    gate.last.insert(user_id, now);
    Entry::Go(Ticket { user_id })
}

/// 记下这一次真正发出去的东西，冷却期内再问就直接重发它。
fn remember(user_id: i64, result: Cached) {
    let mut gate = gate().lock().unwrap();
    gate.served.retain(|(id, _)| *id != user_id);
    gate.served.push((user_id, result));
    if gate.served.len() > SERVED_KEEP {
        let drop = gate.served.len() - SERVED_KEEP;
        gate.served.drain(..drop);
    }
}

/// 上一次发出去的那份成品，没有就是没发过。
fn served(user_id: i64) -> Option<Cached> {
    gate()
        .lock()
        .unwrap()
        .served
        .iter()
        .find(|(id, _)| *id == user_id)
        .map(|(_, result)| result.clone())
}

/// 记下这一次用过的称号、一句话与判词，给下一份画像当排除表。
///
/// 只在真发出去之后调：群里没人看见的那份，不该占掉别人的说法。
fn note_words(entry: &persona::Recent) {
    if entry.title.trim().is_empty()
        && entry.note.trim().is_empty()
        && entry.closing.trim().is_empty()
    {
        return;
    }
    let mut gate = gate().lock().unwrap();
    gate.recent.retain(|seen| seen.title != entry.title);
    gate.recent.push(entry.clone());
    if gate.recent.len() > RECENT_KEEP {
        let drop = gate.recent.len() - RECENT_KEEP;
        gate.recent.drain(..drop);
    }
}

/// 最近用过的称号、一句话与判词，从旧到新。
fn recent_words() -> Vec<persona::Recent> {
    gate().lock().unwrap().recent.clone()
}

// ================= 插件入口 =================

pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let config: PortraitConfig = get_config_or_default(&ctx, "portrait");
        if !config.enabled {
            return Ok(Some(ctx));
        }
        let Some(msg) = ctx.as_message() else {
            return Ok(Some(ctx));
        };
        let Some(event) = (match &ctx.event {
            EventType::Satori(event) => Some(event),
            _ => None,
        }) else {
            return Ok(Some(ctx));
        };

        let (text, mentions) = read_message(event, ctx.bot.login_user.get().id.parse::<i64>().ok());
        let Some(content) = command_body(&ctx, &text) else {
            return Ok(Some(ctx));
        };
        let Some(request) = parse_command(content, &mentions) else {
            return Ok(Some(ctx));
        };
        let group_id = msg.group_id();
        if !config.channel.allows(group_id) {
            return Ok(Some(ctx));
        }

        let requester = msg.user_id();
        let message_id = msg.message_id();
        let (target, label) = match request {
            Request::Mine => (requester, None),
            Request::Other(user_id, name) => (user_id, name),
        };
        if target <= 0 {
            return Ok(Some(ctx));
        }

        let cooldown = Duration::from_secs(config.cooldown_seconds.min(86_400));
        // 票绑在一个活到函数结束的名字上：忙碌标记靠它的 `Drop` 摘掉，
        // 写进 `Ok(_)` 之类的分支里会当场被丢掉，闸门就白设了。
        let _ticket = match enter(target, cooldown) {
            Entry::Busy => {
                let _ = say(
                    &ctx,
                    writer,
                    group_id,
                    requester,
                    message_id,
                    "⏳ 这张画像正在生成，画完会自动发出".to_string(),
                )
                .await;
                return Ok(None);
            }
            Entry::Cooling(left) => {
                // 冷却期内再问，把上一次的成品原样再发一次：标签、综述、版式都在里面，
                // 同一批素材再跑一次还是这一份。没发过东西（比如上次翻不到记录）
                // 才退回原来那句。
                match served(target) {
                    Some(cached) => {
                        let mut reply = Message::new();
                        if message_id > 0 {
                            reply = reply.reply(message_id);
                        }
                        reply = reply.text(format!("⏳ 还是刚才那份画像，{left} 秒后可重新生成"));
                        reply = match cached {
                            Cached::Card(base64) => reply.image(base64),
                            Cached::Report(report) => reply.text(report),
                        };
                        if let Err(error) =
                            send_msg(&ctx, writer, group_id, Some(requester), reply).await
                        {
                            warn!(target: LOG_TARGET, "冷却期内重发上次的画像失败：{error}");
                        }
                    }
                    None => {
                        let _ = say(
                            &ctx,
                            writer,
                            group_id,
                            requester,
                            message_id,
                            format!("⏳ 刚画过，{left} 秒后可重新生成"),
                        )
                        .await;
                    }
                }
                return Ok(None);
            }
            Entry::Go(ticket) => ticket,
        };

        let who = match &label {
            Some(name) => name.clone(),
            None if target == requester => "你".to_string(),
            None => format!("QQ {target}"),
        };
        let notice = say(
            &ctx,
            writer.clone(),
            group_id,
            requester,
            message_id,
            format!("⏳ 正在读 {who} 的发言记录，整理画像…"),
        )
        .await;

        info!(target: LOG_TARGET, "开始生成画像：目标 {}（请求者 {}）", target, requester);

        let now = card::now(beijing());
        let request = collect::Request {
            user_id: target,
            start: window_start(config.days, now.timestamp()),
            end: now.timestamp() + 1,
            max_scan: config.max_scan.clamp(200, 50_000),
            max_samples: config.max_samples.clamp(20, 400),
            partners: config.partners.min(12),
        };
        let material = match collect::collect(&ctx.db, &request).await {
            Ok(Some(material)) => material,
            Ok(None) => {
                let _ = say(
                    &ctx,
                    writer.clone(),
                    group_id,
                    requester,
                    message_id,
                    // 空态不是错误：说清为什么空，再给一条能立刻做的事。
                    format!("📭 没有找到 {who} 在群里的发言记录\n他在这段时间里没在群里说过话，或换个时间范围再试"),
                )
                .await;
                // 这一趟到此为止，那句进度也该收走——群里只留一条说得清的。
                retract(&ctx, writer, notice).await;
                return Ok(None);
            }
            Err(error) => {
                error!(target: LOG_TARGET, "查询发言记录失败：{error:#}");
                let _ = say(
                    &ctx,
                    writer.clone(),
                    group_id,
                    requester,
                    message_id,
                    format!("❌ 查记录时出错了：{error}\n过一会儿再试"),
                )
                .await;
                retract(&ctx, writer, notice).await;
                return Ok(None);
            }
        };

        // 观测四块与样本交给模型，换回十格档案、戏说与综述；收口在 `sanitize` 里做，
        // 认不出维度、没依据、冒认「明说」的都在那儿落地。
        let (base, key, model) = match endpoint(&ctx, &config.model).await {
            Ok(triple) => triple,
            Err(error) => {
                warn!(target: LOG_TARGET, "模型接口不可用：{error:#}");
                let _ = say(
                    &ctx,
                    writer.clone(),
                    group_id,
                    requester,
                    message_id,
                    format!("❌ 模型接口没配好：{error}\n检查 [oai.providers] 里的接口地址与密钥"),
                )
                .await;
                retract(&ctx, writer, notice).await;
                return Ok(None);
            }
        };

        let thinking = config.thinking.trim();
        let history = vec![
            LlmMessage::System {
                content: persona::system_prompt().to_string(),
            },
            LlmMessage::User {
                content: vec![UserContent::Text(Text::new(persona::user_prompt(
                    &material,
                    &material.style,
                    &recent_words(),
                )))],
            },
        ];
        let completion = tokio::time::timeout(
            COMPLETION_TIMEOUT,
            crate::plugins::oai::llm::complete(
                &base,
                &key,
                &model,
                history,
                (!thinking.is_empty()).then_some(thinking),
            ),
        )
        .await;

        let profile = match completion {
            Ok(Ok(raw)) => match persona::parse(&raw) {
                Ok(parsed) => parsed.sanitize(&material),
                Err(error) => {
                    warn!(target: LOG_TARGET, "画像 JSON 解析失败，退回统计直出：{error:#}");
                    persona::Persona::from_stats(&material)
                }
            },
            Ok(Err(error)) => {
                warn!(target: LOG_TARGET, "模型调用失败，退回统计直出：{error:#}");
                persona::Persona::from_stats(&material)
            }
            Err(_) => {
                warn!(target: LOG_TARGET, "模型调用超过 {} 秒，退回统计直出", COMPLETION_TIMEOUT.as_secs());
                persona::Persona::from_stats(&material)
            }
        };

        // 头像一共两拨：对象一张大图，往来对象每人一张小图。并起来取，
        // 六个人挨个取最坏要等一分钟。
        let partners: Vec<i64> = material.ties.iter().map(|tie| tie.user_id).collect();
        let (avatar, faces) = tokio::join!(
            avatar::data_url(material.user_id),
            avatar::data_urls(&partners, avatar::PARTNER_SPEC),
        );
        // 页脚那条下一步带上当前环境的前缀，读者能整条抄走。
        let command = format!(
            "{}画像 @某人",
            crate::command::get_prefixes(&ctx)
                .first()
                .cloned()
                .unwrap_or_default()
        );
        let view = card::View {
            material: &material,
            persona: &profile,
            avatar: avatar.as_deref(),
            faces: &faces,
            model: &model,
            theme: &config.theme,
            command: &command,
            offset: beijing(),
            now,
        };
        let html = card::html(&view);
        archive(&html, target).await;

        // 关掉出图时直接走文字版，与出图失败走同一条路。
        let captured = if config.image_enabled {
            card::capture(&html, config.image_scale).await
        } else {
            Err(anyhow::anyhow!("image_enabled = false"))
        };
        match captured {
            Ok(base64) => {
                let reply = Message::new().image(base64.clone());
                // 真发出去了才记：没发出去的那份，群里没人看见过。
                if send_msg(&ctx, writer.clone(), group_id, Some(requester), reply)
                    .await
                    .is_ok()
                {
                    remember(target, Cached::Card(base64));
                    note_words(&profile.stamp());
                }
            }
            Err(error) => {
                if config.image_enabled {
                    error!(target: LOG_TARGET, "画像出图失败：{error:#}");
                }
                // 出图失败或主动关图都不该等于没有结果：退回成文字版。
                let summary = text_report(&material, &profile, &model, config.image_enabled);
                remember(target, Cached::Report(summary.clone()));
                note_words(&profile.stamp());
                let _ = say(
                    &ctx,
                    writer.clone(),
                    group_id,
                    requester,
                    message_id,
                    summary,
                )
                .await;
            }
        }

        // 成品已经在群里了，那句「正在生成」就该收走：它是一句进度，留在那儿是噪声。
        retract(&ctx, writer, notice).await;

        info!(target: LOG_TARGET, "画像完成：目标 {}（{} 条样本）", target, material.samples.len());
        Ok(None)
    })
}

/// 统计窗口的起点。`days` 为 0 表示从最早的那条记录算起。
fn window_start(days: i64, now: i64) -> i64 {
    if days <= 0 {
        0
    } else {
        now.saturating_sub(days.min(3_650) * 86_400)
    }
}

/// 北京时间。实现在 [`crate::render::beijing`]——六张卡片共用同一个口径。
fn beijing() -> chrono::FixedOffset {
    crate::render::beijing()
}

/// 解析模型接口：`供应商/模型` 走 `[oai.providers]`，不带前缀沿用 oai 默认接口。
async fn endpoint(ctx: &Context, model: &str) -> anyhow::Result<(String, String, String)> {
    let (provider, model) = crate::plugins::oai::utils::split_provider(model);
    if model.trim().is_empty() {
        anyhow::bail!("portrait.model 没配");
    }
    let providers = get_config_or_default::<crate::plugins::oai::OaiConfig>(ctx, "oai").providers;
    let (default_base, default_key) = match crate::plugins::oai::data::MANAGER.get() {
        Some(manager) => {
            let config = manager.config.read().await;
            (config.api_base.clone(), config.api_key.clone())
        }
        None => (String::new(), String::new()),
    };
    let Some((base, key)) = crate::plugins::oai::resolve_endpoint(
        &providers,
        &default_base,
        &default_key,
        provider.as_deref(),
    ) else {
        anyhow::bail!(
            "未知供应商 {}（在 [oai.providers] 里配置）",
            provider.as_deref().unwrap_or_default()
        );
    };
    if base.is_empty() || key.is_empty() {
        anyhow::bail!("需要 oai 的接口地址与密钥");
    }
    Ok((base, key, model))
}

/// 卡片图之外的文字版：一句话定位、人物档案、戏说、口头禅、怎么说话、什么时候来、
/// 群内往来、画像综述与判词，以及那句边界说明——一张图里有的信息，这一层一条不落，
/// 包括 24 小时与一周的完整分布。
/// 够用户在群里看懂这份画像，不至于因为一张图没出成就什么都拿不到。
///
/// `image_enabled` 只影响最后那句交代：是「出图失败」还是「本来就关了图」。
fn text_report(
    material: &collect::Material,
    profile: &persona::Persona,
    model: &str,
    image_enabled: bool,
) -> String {
    let mut out = format!("▍一句话定位\n{}\n{}\n", profile.title, profile.note);

    out.push_str("\n▍人物档案（把握三档：明说＝他本人讲过，可推＝几条线索，待考＝只一处线索）\n");
    if profile.facets.is_empty() {
        out.push_str(
            "　这一层这次空着：模型没接上，一格都没写。\
             下面几节照旧——它们全部由记录数出，不经模型。\n",
        );
    } else {
        for (name, _) in persona::FACETS {
            let Some(facet) = profile.facet(name) else {
                continue;
            };
            out.push_str(&format!("　{}｜{}\n", name, facet.tier()));
            out.push_str(&format!("　　{}\n", facet.verdict));
            out.push_str(&format!("　　{}\n", evidence_line(facet)));
        }
    }

    // 戏说：标签墙与小传。文字版里把口径也带上，不然发出来容易被人当真。
    if !profile.labels.is_empty() || !profile.sketch.trim().is_empty() {
        out.push_str("\n▍戏说（拿来玩的：允许夸张，也允许说偏）\n");
        for item in &profile.labels {
            if item.why.trim().is_empty() {
                out.push_str(&format!("　· {}\n", item.label));
            } else {
                out.push_str(&format!("　· {}｜{}\n", item.label, item.why));
            }
        }
        if !profile.sketch.trim().is_empty() {
            out.push_str(&format!("{}\n", profile.sketch));
        }
    }

    if !profile.catchphrases.is_empty() {
        out.push_str("\n▍口头禅（他自己反复说的，逐字照抄，一个字没改）\n");
        for phrase in &profile.catchphrases {
            out.push_str(&format!("　「{phrase}」\n"));
        }
    }

    // 语言：全部由事实算出。
    let s = &material.style;
    out.push_str("\n▍怎么说话（全部由记录数出）\n");
    out.push_str(&format!(
        "　提问 {}、感叹 {}、笑声 {}、语气词 {}、省略 {}；每百字自称 {:.1} 次、对称呼 {:.1} 次\n",
        persona::percent(s.question_rate),
        persona::percent(s.exclaim_rate),
        persona::percent(s.laugh_rate),
        persona::percent(s.modal_rate),
        persona::percent(s.ellipsis_rate),
        s.self_per100,
        s.you_per100,
    ));
    out.push_str(&format!(
        "　均长 {:.1} 字；长消息 {}、短消息 {}；长度起伏 {:.2}；爆发指数 {:.2}；相邻重复 {}\n",
        material.avg_len(),
        persona::percent(s.long_rate),
        persona::percent(s.short_rate),
        s.len_cv,
        s.burstiness,
        persona::percent(s.repeat_rate),
    ));
    if !profile.style.trim().is_empty() {
        out.push_str(&format!("　{}\n", profile.style));
    }

    out.push_str("\n▍什么时候来\n");
    out.push_str(&format!(
        "　{}前后最密；夜间（0—6 点）占 {}；周末占 {}；最活跃的一天是{}\n",
        persona::hour_label(material.peak_hour()),
        persona::percent(material.night_ratio()),
        persona::percent(material.weekend_ratio()),
        persona::weekday_label(material.peak_weekday()),
    ));
    out.push_str(&format!("　0—23 时依次：{}\n", counts(&material.hour)));
    out.push_str(&format!(
        "　日 一 二 三 四 五 六依次：{}\n",
        counts(&material.weekday)
    ));

    if !material.ties.is_empty() {
        out.push_str("\n▍群内往来（点名是 @，接话是紧跟在对方之后的下一条；两者都不等于回复）\n");
        for tie in &material.ties {
            out.push_str(&format!("　{}（QQ {}）\n", tie.name, tie.user_id));
            out.push_str(&format!(
                "　　点名 我叫他 {} 次 · 他叫我 {} 次（{}）\n",
                tie.at_out,
                tie.at_in,
                tie.initiative(),
            ));
            out.push_str(&format!(
                "　　接话 我接他 {} 次 · 他接我 {} 次\n",
                tie.turn_out, tie.turn_in,
            ));
            if let Some(line) = profile
                .ties
                .iter()
                .find(|reading| reading.id == tie.user_id)
            {
                out.push_str(&format!("　　{}\n", line.line));
            }
        }
    }

    let passages: Vec<&persona::Passage> = profile.live_passages().collect();
    let closing = profile.closing.trim();
    if !passages.is_empty() || !closing.is_empty() {
        out.push_str("\n▍画像综述\n");
        for passage in passages {
            if passage.is_quote() {
                out.push_str(&format!("　「{}」\n", passage.text));
                if !passage.note.trim().is_empty() {
                    out.push_str(&format!("　——{}\n", passage.note));
                }
            } else {
                out.push_str(&format!("{}\n", passage.body));
            }
        }
        // 判词换一行立在末尾，前面加一条记号：文字版没有细线可用，
        // 这一块的分量就落在「判词」这两个字和一整行的留白上。
        if !closing.is_empty() {
            out.push_str(&format!("\n▍判词\n　{}\n", closing));
        }
    }

    let footer = if image_enabled {
        "出图失败，先给你一份文字版"
    } else {
        "出图已关闭，这一份是文字版"
    };
    out.push_str(&format!(
        "\n画像是对行为的抽象，有损：只含他在群里说过的部分，不等于本人。\
         档案每一格都标了把握，标「明说」的那几格，依据是他本人的原话；\
         口头禅那几句是原样照抄的；戏说那一节是玩笑，允许夸张。\
         {footer}（{model}）。"
    ));
    out
}

/// 一条依据。标了「明说」的那几格，依据就是他本人的原话，用书名号式的引号括起来，
/// 与版面上的处理一致。
fn evidence_line(facet: &persona::Facet) -> String {
    if facet.quoted {
        format!("「{}」", facet.evidence)
    } else {
        facet.evidence.clone()
    }
}

/// 一列计数，一行印全。文字版里没有柱状图，图里有的数一个不落。
fn counts(values: &[u64]) -> String {
    values
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// 一条刚发出去、还来得及撤回的消息。
///
/// 频道跟着消息一起记下来，不靠调用方再算一遍：群聊与私聊的 `channel_id` 写法不一样
/// （群是群号本身，私聊是 `private:<QQ>`），撤回要按发出去的那条频道去撤。
struct Notice {
    id: String,
    channel: String,
    sent: Instant,
}

/// 预告消息的撤回窗口，取 QQ 的两分钟再留二十秒余量。
///
/// **过了窗口就不去试**。QQ 只给两分钟，超了必然失败，而实现端的出站闸门会把连续的
/// 撤回失败算成故障（连撤三条就把整台机器人的发消息能力关上两分钟，见 satori-qq 的
/// OutboundGuard）。一条留在群里的过期预告，比机器人失声两分钟轻得多。
const RETRACT_WINDOW: Duration = Duration::from_secs(100);

/// 把「正在生成」那条收回去。
///
/// 发一句进度是在群里占一行，成品出来之后它就成了噪声；收走它，会话干净。
/// 收不掉不算失败：日志记一行，什么都不说。
async fn retract(ctx: &Context, writer: LockedWriter, notice: Option<Notice>) {
    let Some(notice) = notice else {
        return;
    };
    let waited = notice.sent.elapsed();
    if waited >= RETRACT_WINDOW {
        debug!(
            target: LOG_TARGET,
            "预告消息发出已 {} 秒，过了撤回窗口，留着它",
            waited.as_secs()
        );
        return;
    }
    let result = writer
        .call::<_, serde_json::Value>(
            ctx,
            "message.delete",
            serde_json::json!({
                "channel_id": notice.channel,
                "message_id": notice.id,
            }),
        )
        .await;
    match result {
        Ok(_) => debug!(target: LOG_TARGET, "已收回预告消息 {}", notice.id),
        // 撤回失败不是这次生成的问题：日志留一行就够了，群里一个字都不说。
        Err(error) => {
            debug!(target: LOG_TARGET, "预告消息没收回（不影响这份画像）：{error}")
        }
    }
}

/// 发一句提示，并把句柄带回来——能不能撤、要不要撤由调用方定。
///
/// 指令回执、空态与报错都该留在群里，用不到那个句柄；只有「正在生成」那一条会在成品
/// 发出去之后交给 [`retract`]。
async fn say(
    ctx: &Context,
    writer: LockedWriter,
    group_id: Option<i64>,
    user_id: i64,
    message_id: i64,
    text: String,
) -> Option<Notice> {
    let mut message = Message::new();
    if message_id > 0 {
        message = message.reply(message_id);
    }
    let channel = match group_id.filter(|id| *id != 0) {
        Some(id) => id.to_string(),
        None => format!("private:{user_id}"),
    };
    match send_msg_id(ctx, writer, group_id, Some(user_id), message.text(text)).await {
        Ok(id) => id.map(|id| Notice {
            id,
            channel,
            sent: Instant::now(),
        }),
        Err(error) => {
            warn!(target: LOG_TARGET, "回复失败：{error}");
            None
        }
    }
}

/// 把这一版的 HTML 落到插件数据目录。出图失败时它就是唯一的成品，
/// 平时也能拿它核对版式。
async fn archive(html: &str, user_id: i64) {
    let Ok(dir) = crate::plugins::get_data_dir("portrait").await else {
        return;
    };
    let name = format!(
        "{user_id}-{}.html",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    if let Err(error) = tokio::fs::write(dir.join(name), html).await {
        warn!(target: LOG_TARGET, "画像 HTML 归档失败：{error}");
        return;
    }
    let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        return;
    };
    let mut files: Vec<String> = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Some(name) = entry.file_name().to_str()
            && name.ends_with(".html")
        {
            files.push(name.to_string());
        }
    }
    if files.len() <= ARCHIVE_KEEP {
        return;
    }
    files.sort();
    for name in files.iter().take(files.len() - ARCHIVE_KEEP) {
        let _ = tokio::fs::remove_file(dir.join(name)).await;
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use sea_orm::{FromQueryResult, Statement};

    /// 端到端跑一遍：真库取素材、真模型出画像、真截图。
    ///
    /// 给运维用：换模型或改提示词之后，确认素材读得出来、模型给的是合法 JSON、
    /// 标签落在四个维度里、引语确实出自样本、出图不是一张空白。目标默认取记录最多的人，
    /// 也可以 `PORTRAIT_LIVE_USER=<QQ号>` 指定。
    /// `cargo test --release portrait_live -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "读取 config.toml 与 data/bot.db，访问真实模型接口，需要 Chromium"]
    async fn portrait_live_from_the_real_database() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let Some(user_id) = live_target(manifest).await else {
            println!("data/bot.db 里没有可用记录，跳过");
            return;
        };
        let (base, key, model) = live_endpoint(manifest);
        println!("目标 {user_id}，模型 {model}");

        let db = sea_orm::Database::connect(format!("sqlite://{manifest}/data/bot.db?mode=ro"))
            .await
            .expect("打不开 data/bot.db");
        let now = card::now(beijing());
        let request = collect::Request {
            user_id,
            start: 0,
            end: now.timestamp() + 1,
            max_scan: 8_000,
            max_samples: 120,
            partners: 6,
        };
        let material = collect::collect(&db, &request)
            .await
            .expect("查询失败")
            .expect("这个人没有群聊记录");
        println!(
            "===== 素材 =====\n{} 条发言，覆盖 {} 天，样本充分性 {}，往来对象 {} 个",
            material.total,
            material.span_days(),
            material.sufficiency().label(),
            material.ties.len()
        );

        let history = vec![
            LlmMessage::System {
                content: persona::system_prompt().to_string(),
            },
            LlmMessage::User {
                content: vec![UserContent::Text(Text::new(persona::user_prompt(
                    &material,
                    &material.style,
                    &recent_words(),
                )))],
            },
        ];
        let raw = crate::plugins::oai::llm::complete(&base, &key, &model, history, Some("high"))
            .await
            .expect("模型调用失败");
        println!("===== 模型原始输出 =====\n{raw}\n");

        let profile = persona::parse(&raw)
            .expect("模型没有返回可用 JSON")
            .sanitize(&material);
        println!(
            "===== 收口后的画像 =====\n称号：{}\n一句话：{}\n怎么说话：{}\n档案（{}/{} 格）：\n{}\n戏说：\n{}\n口头禅：\n{}\n往来读法：\n{}\n综述：\n{}\n判词：{}",
            profile.title,
            profile.note,
            profile.style,
            profile.covered(),
            persona::FACETS.len(),
            persona::FACETS
                .iter()
                .filter_map(|(name, _)| {
                    let facet = profile.facet(name)?;
                    Some(format!(
                        "　{}｜{}｜{}｜{}",
                        name,
                        facet.tier(),
                        facet.verdict,
                        facet.evidence
                    ))
                })
                .collect::<Vec<_>>()
                .join("\n"),
            profile
                .labels
                .iter()
                .map(|item| format!("　· {}｜{}", item.label, item.why))
                .chain(std::iter::once(format!("　{}", profile.sketch)))
                .collect::<Vec<_>>()
                .join("\n"),
            profile
                .catchphrases
                .iter()
                .map(|phrase| format!("　「{phrase}」"))
                .collect::<Vec<_>>()
                .join("\n"),
            profile
                .ties
                .iter()
                .map(|reading| format!("　{}｜{}", reading.id, reading.line))
                .collect::<Vec<_>>()
                .join("\n"),
            profile
                .live_passages()
                .map(|passage| if passage.is_quote() {
                    format!("　「{}」——{}", passage.text, passage.note)
                } else {
                    format!("　{}", passage.body)
                })
                .collect::<Vec<_>>()
                .join("\n"),
            profile.closing,
        );
        println!(
            "===== 群内往来（由记录数出）=====\n{}",
            material
                .ties
                .iter()
                .map(|tie| format!(
                    "　{}：我叫他 {}、他叫我 {}；我接他 {}、他接我 {}（{}）",
                    tie.name,
                    tie.at_out,
                    tie.at_in,
                    tie.turn_out,
                    tie.turn_in,
                    tie.initiative()
                ))
                .collect::<Vec<_>>()
                .join("\n")
        );
        assert!(!profile.title.is_empty(), "一句话定位里的戏称不该是空的");
        assert!(!profile.note.is_empty(), "一句话概括不该是空的");
        // 十格里至少写出四格，否则这份档案没成形；模型接上了就不该只剩一两格。
        assert!(
            profile.covered() >= 4,
            "档案只写出了 {} 格",
            profile.covered()
        );
        assert!(
            profile.live_passages().count() >= 4,
            "综述至少要有四段，这次只有 {} 段",
            profile.live_passages().count()
        );
        // 每一格都有一条依据；标了「明说」的，依据必须是他的原话。
        for facet in &profile.facets {
            assert!(
                !facet.evidence.trim().is_empty(),
                "{} 没有依据",
                facet.dimension
            );
            if facet.tier() == "明说" {
                assert!(
                    persona::quote_is_from_samples(&facet.evidence, &material.samples),
                    "{} 标了明说，依据却不在样本里：{}",
                    facet.dimension,
                    facet.evidence
                );
            }
        }
        // 引语是被比对过的：要么没有，要么每一句都归一化后出自样本（与收口同一把尺）。
        for passage in profile.live_passages().filter(|passage| passage.is_quote()) {
            assert!(
                persona::quote_is_from_samples(&passage.text, &material.samples),
                "引语不在样本里：{}",
                passage.text
            );
        }

        let partners: Vec<i64> = material.ties.iter().map(|tie| tie.user_id).collect();
        let (avatar, faces) = tokio::join!(
            avatar::data_url(material.user_id),
            avatar::data_urls(&partners, avatar::PARTNER_SPEC),
        );
        let html = card::html(&card::View {
            material: &material,
            persona: &profile,
            avatar: avatar.as_deref(),
            faces: &faces,
            model: &model,
            theme: "auto",
            command: "/画像 @某人",
            offset: beijing(),
            now,
        });
        // 归档一份：出图预算探针要拿真卡片量，版式也能直接打开看。
        archive(&html, material.user_id).await;
        let base64 = card::capture(&html, 2.0).await.expect("出图失败");
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&base64)
            .unwrap();
        let image = image::load_from_memory(&bytes).unwrap();
        println!("出图 {}×{}", image.width(), image.height());
        let path = std::env::temp_dir().join("acumen-portrait-live.jpg");
        std::fs::write(&path, &bytes).ok();
        println!("出图已写入 {}", path.display());
    }

    /// 出图预算探针：把一份真卡片按各档倍率各截一次，打印像素数与成败。
    ///
    /// 画像卡每加一节就多几百像素，而 `render::web` 有一道「宽度 × 高度 × 倍率² ≤ 6400 万」
    /// 的像素护栏；线上 `image_scale` 是 3，所以卡片长到一定程度会悄没声地退回文字版。
    /// 改完版式拿这个量一次，比等它失败强。
    ///
    /// 卡片默认取 `target/release/data/portrait/` 里最近归档的那一份，也可以用
    /// `PORTRAIT_CARD_HTML=<文件>` 指定。
    /// `cargo test --release portrait_card_budget -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "需要 Chromium 与一份归档的画像 HTML"]
    async fn portrait_card_budget_at_every_scale() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let html = match std::env::var("PORTRAIT_CARD_HTML") {
            Ok(path) => std::fs::read_to_string(path).expect("读不到指定的 HTML"),
            Err(_) => {
                // 归档目录跟着 `current_exe()` 走：生产是 `target/release/data/portrait`，
                // 而这个探针自己是 `target/release/deps/` 下的测试二进制，归档会落到
                // `deps/data/portrait`。两处都翻一遍，按改动时间取最近的那一份。
                let root = std::path::Path::new(manifest).join("target/release");
                let newest = [root.join("data/portrait"), root.join("deps/data/portrait")]
                    .iter()
                    .filter_map(|dir| std::fs::read_dir(dir).ok())
                    .flatten()
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.extension().is_some_and(|ext| ext == "html"))
                    .filter_map(|path| {
                        let stamp = path.metadata().ok()?.modified().ok()?;
                        Some((stamp, path))
                    })
                    .max_by_key(|(stamp, _)| *stamp)
                    .map(|(_, path)| path);
                match newest {
                    Some(path) => {
                        println!("用量的是 {}", path.display());
                        std::fs::read_to_string(&path).expect("读不到归档的 HTML")
                    }
                    None => {
                        println!("target/release 下没有归档，跳过");
                        return;
                    }
                }
            }
        };
        println!("HTML {} KB", html.len() / 1024);
        for scale in [1.0, 2.0, 3.0, 4.0] {
            match card::capture(&html, scale).await {
                Ok(base64) => {
                    use base64::Engine as _;
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(&base64)
                        .unwrap();
                    let image = image::load_from_memory(&bytes).unwrap();
                    let pixels = image.width() as f64 * image.height() as f64;
                    println!(
                        "倍率 {scale}：{}×{} = {:.1} 万像素（护栏 6400 万 · {}）",
                        image.width(),
                        image.height(),
                        pixels / 10_000.0,
                        if pixels > 64_000_000.0 {
                            "超了，会退回文字版"
                        } else {
                            "在护栏内"
                        }
                    );
                }
                Err(error) => println!("倍率 {scale}：出图失败（线上会退回文字版）：{error}"),
            }
        }
        cdp_html_shot::Browser::shutdown_global().await;
    }

    /// 记录最多的那个用户，或 `PORTRAIT_LIVE_USER` 指定的那个。
    async fn live_target(manifest: &str) -> Option<i64> {
        if let Ok(value) = std::env::var("PORTRAIT_LIVE_USER")
            && let Ok(user_id) = value.trim().parse::<i64>()
        {
            return Some(user_id);
        }
        #[derive(sea_orm::FromQueryResult)]
        struct UidRow {
            uid: i64,
        }
        let path = format!("{manifest}/data/bot.db");
        if !std::path::Path::new(&path).exists() {
            return None;
        }
        let db = sea_orm::Database::connect(format!("sqlite://{path}?mode=ro"))
            .await
            .ok()?;
        let row = UidRow::find_by_statement(Statement::from_string(
            db.get_database_backend(),
            "SELECT user_id AS uid FROM message_records \
             WHERE role != 'self' AND group_id != 0 \
             GROUP BY user_id ORDER BY COUNT(*) DESC LIMIT 1"
                .to_string(),
        ))
        .one(&db)
        .await
        .ok()??;
        (row.uid > 0).then_some(row.uid)
    }

    /// 线上接口：`[oai.providers.deepseek]` 与 `[portrait].model`。
    fn live_endpoint(manifest: &str) -> (String, String, String) {
        let raw =
            std::fs::read_to_string(format!("{manifest}/config.toml")).expect("读不到 config.toml");
        let value: toml::Value = toml::from_str(&raw).expect("config.toml 解析失败");
        let lookup = |path: &[&str]| {
            path.iter()
                .try_fold(&value, |node, key| node.get(*key))
                .and_then(|node| node.as_str())
        };
        let base = lookup(&["oai", "providers", "deepseek", "api_base"]).unwrap_or_default();
        let key = lookup(&["oai", "providers", "deepseek", "api_key"]).unwrap_or_default();
        assert!(
            !base.is_empty() && !key.is_empty(),
            "config.toml 里没配 [oai.providers.deepseek]"
        );
        // 还没重启过的新插件没有这一节，用默认模型即可。
        let model = lookup(&["portrait", "model"]).unwrap_or("deepseek/deepseek-flash");
        let (_, model) = crate::plugins::oai::utils::split_provider(model);
        (base.to_string(), key.to_string(), model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mention(user_id: i64, name: &str) -> Mention {
        Mention {
            user_id,
            name: Some(name.to_string()),
        }
    }

    #[test]
    fn a_bare_command_asks_for_your_own_report() {
        for input in [
            "画像",
            " 画像 ",
            "用户画像",
            "角色画像",
            "我的画像",
            "人物画像",
            "画像报告",
            "用户画像报告",
        ] {
            assert_eq!(parse_command(input, &[]), Some(Request::Mine), "{input}");
        }
    }

    #[test]
    fn an_at_mention_points_the_report_at_somebody_else() {
        let mentions = [mention(10001, "某人")];
        assert_eq!(
            parse_command("画像", &mentions),
            Some(Request::Other(10001, Some("某人".to_string())))
        );
        assert_eq!(
            parse_command("用户画像", &mentions),
            Some(Request::Other(10001, Some("某人".to_string())))
        );
    }

    #[test]
    fn a_qq_number_works_without_an_at() {
        assert_eq!(
            parse_command("画像 123456", &[]),
            Some(Request::Other(123456, None))
        );
        assert_eq!(
            parse_command("画像@123456", &[]),
            Some(Request::Other(123456, None))
        );
        // 全角 @ 也认。
        assert_eq!(
            parse_command("画像＠123456", &[]),
            Some(Request::Other(123456, None))
        );
    }

    /// 闲聊里出现「画像」这个词不该触发模型调用。
    #[test]
    fn ordinary_chatter_is_not_a_command() {
        for input in [
            "这个游戏的画像有点丑",
            "画像怎么做的",
            "帮我画像素画",
            "画像 这个 那个",
            "画像12abc",
        ] {
            assert_eq!(parse_command(input, &[]), None, "{input}");
        }
    }

    /// 画像只做一件事：筮法那一套别名已经拿掉了，说到也不算指令。
    #[test]
    fn the_divination_aliases_are_gone() {
        for input in ["算卦", "起卦", "卜卦", "易经画像"] {
            assert_eq!(parse_command(input, &[]), None, "{input}");
        }
    }

    /// 消息里带图时 @ 与文本会被拆成多段，拼出来的文本仍要能认出指令。
    #[test]
    fn segments_are_joined_without_the_mentions() {
        let event: crate::event::Event = simd_json::serde::to_owned_value(serde_json::json!({
            "post_type": "message",
            "message": [
                {"type": "at", "data": {"qq": "3373167460", "name": "机器人"}},
                {"type": "text", "data": {"text": " 画像 "}},
                {"type": "at", "data": {"qq": "10001", "name": "某人"}},
            ]
        }))
        .unwrap();
        let (text, mentions) = read_message(&event, Some(3373167460));
        assert_eq!(text, " 画像 ");
        assert_eq!(mentions, vec![mention(10001, "某人")]);
        assert_eq!(
            parse_command(text.trim(), &mentions),
            Some(Request::Other(10001, Some("某人".to_string())))
        );
    }

    /// 群里喊指令习惯先 @ 机器人，那个 @ 不是画像对象。
    #[test]
    fn mentioning_the_bot_itself_falls_back_to_myself() {
        let event: crate::event::Event = simd_json::serde::to_owned_value(serde_json::json!({
            "post_type": "message",
            "message": [
                {"type": "at", "data": {"qq": "3373167460", "name": "机器人"}},
                {"type": "text", "data": {"text": "画像"}},
            ]
        }))
        .unwrap();
        let (text, mentions) = read_message(&event, Some(3373167460));
        assert!(mentions.is_empty());
        assert_eq!(parse_command(&text, &mentions), Some(Request::Mine));
    }

    #[test]
    fn at_all_is_not_a_target() {
        let event: crate::event::Event = simd_json::serde::to_owned_value(serde_json::json!({
            "post_type": "message",
            "message": [
                {"type": "at", "data": {"qq": "all"}},
                {"type": "text", "data": {"text": "画像"}},
            ]
        }))
        .unwrap();
        let (_, mentions) = read_message(&event, None);
        assert!(mentions.is_empty());
    }

    /// 这是线上真实踩到的那一条：平台把「@某人的名字」也写进了文本段，
    /// 于是正文成了「/画像  @黑猫警长 职业目标是抓捕萨摩耶」，尾巴把那道严格的
    /// 校验顶掉了，指令整条不生效（2026-09-14 群 818965288 的日志）。
    #[test]
    fn a_mention_whose_name_leaks_into_the_body_still_fires() {
        let event: crate::event::Event = simd_json::serde::to_owned_value(serde_json::json!({
            "post_type": "message",
            "message": [
                {"type": "text", "data": {"text": "/画像  "}},
                {"type": "at", "data": {"qq": "3201735089"}},
                {"type": "text", "data": {"text": "@爱捡漏的黑猫警长 职业目标是抓捕萨摩耶"}},
            ]
        }))
        .unwrap();
        let (text, mentions) = read_message(&event, Some(3373167460));
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].user_id, 3201735089);

        let prefixes = vec!["/".to_string()];
        let body = body_after_prefixes(&prefixes, &text).expect("前缀应当剥得掉");
        assert_eq!(
            parse_command(body, &mentions),
            Some(Request::Other(3201735089, None))
        );
        // 同一个正文，没有 @ 时照旧不算指令。
        assert_eq!(parse_command(body, &[]), None);
    }

    /// 前缀躲在「@名字」后面时也要认。
    #[test]
    fn the_prefix_may_hide_behind_a_leading_mention() {
        let prefixes = vec!["/".to_string()];
        assert_eq!(body_after_prefixes(&prefixes, "@小黑 /画像"), Some("画像"));
        assert_eq!(
            body_after_prefixes(&prefixes, "/画像 @小黑"),
            Some("画像 @小黑")
        );
        assert_eq!(body_after_prefixes(&prefixes, "  /画像  "), Some("画像"));
        // 剥不出前缀就是普通聊天。
        assert_eq!(body_after_prefixes(&prefixes, "@小黑 你好"), None);
        assert_eq!(body_after_prefixes(&prefixes, "你好"), None);
        // 名字后面没有空白，认不出边界，就不硬猜。
        assert_eq!(body_after_prefixes(&prefixes, "@小黑/画像"), None);
        // 没配前缀时整条消息就是正文，与 command::strip_prefix 的语义一致。
        assert_eq!(body_after_prefixes(&[], "画像 @小黑"), Some("画像 @小黑"));
    }

    /// @ 段里的 qq 可能是字符串也可能是数字，两种都要认得。
    #[test]
    fn a_numeric_qq_is_read_as_well_as_a_string_one() {
        let event: crate::event::Event = simd_json::serde::to_owned_value(serde_json::json!({
            "post_type": "message",
            "message": [
                {"type": "text", "data": {"text": "画像"}},
                {"type": "at", "data": {"qq": 3201735089_u64}},
            ]
        }))
        .unwrap();
        let (text, mentions) = read_message(&event, None);
        assert_eq!(
            mentions,
            vec![Mention {
                user_id: 3201735089,
                name: None
            }]
        );
        assert_eq!(
            parse_command(&text, &mentions),
            Some(Request::Other(3201735089, None))
        );
    }

    #[test]
    fn the_window_starts_at_the_first_record_by_default() {
        assert_eq!(window_start(0, 1_700_000_000), 0);
        assert_eq!(window_start(-5, 1_700_000_000), 0);
        assert_eq!(window_start(30, 1_700_000_000), 1_700_000_000 - 30 * 86_400);
        // 天数被夹在上限内，写错一个巨大的值也不会把窗口算成负数。
        assert!(window_start(999_999, 1_700_000_000) >= 0);
    }

    #[test]
    fn the_gate_serialises_and_cools_down() {
        let cooldown = Duration::from_secs(60);
        // 用一个不常见的号，避免与其它测试共用静态闸门。
        let user_id = 987_654_321;
        let Entry::Go(ticket) = enter(user_id, cooldown) else {
            panic!("第一次应当放行");
        };
        // 同一个人还没跑完，再来一次是「忙」，不是「冷却」。
        assert!(matches!(enter(user_id, cooldown), Entry::Busy));
        drop(ticket);
        // 票一还就可以再进，只是仍在冷却里。
        assert!(matches!(enter(user_id, cooldown), Entry::Cooling(_)));
        // 关掉冷却（0 秒）之后立刻可以重来。
        assert!(matches!(enter(user_id, Duration::ZERO), Entry::Go(_)));
    }

    /// 冷却期内再问，拿回的是上一次那份成品，而不是一句「稍后再来」。
    #[test]
    fn a_cooling_request_gets_the_previous_result_back() {
        let cooldown = Duration::from_secs(60);
        // 另用一个不常见的号，别与其它用例共用静态闸门。
        let user_id = 987_654_322;
        assert!(served(user_id).is_none(), "还没发过东西，无从重发");
        assert!(matches!(enter(user_id, cooldown), Entry::Go(_)));
        // 正在画的那一轮还没落盘，缓存里也还没有。
        assert!(served(user_id).is_none());

        remember(user_id, Cached::Card("卡片".into()));
        match enter(user_id, cooldown) {
            Entry::Cooling(left) => assert!(left > 0, "冷却剩余应报出来"),
            _ => panic!("冷却期内应当是 Cooling"),
        }
        match served(user_id) {
            Some(Cached::Card(base64)) => assert_eq!(base64, "卡片"),
            other => panic!("应当是上次那张卡片：{}", other.is_some()),
        }
        // 冷却过去之后照常重新生成，缓存留着不碍事。
        assert!(matches!(enter(user_id, Duration::ZERO), Entry::Go(_)));
    }

    /// 用过的称号、一句话与判词会留下来，旧的挤掉，同名的抬到最新——
    /// 这份东西不千篇一律就靠这一手。
    #[test]
    fn the_words_already_used_are_kept_to_avoid_repeats() {
        let said = |index: usize| persona::Recent {
            title: format!("第 {index} 个称号"),
            note: format!("第 {index} 句"),
            closing: format!("第 {index} 句判词"),
        };
        for index in 0..=RECENT_KEEP {
            note_words(&said(index));
        }
        let recent = recent_words();
        assert_eq!(recent.len(), RECENT_KEEP);
        assert!(
            !recent.iter().any(|entry| entry.title == "第 0 个称号"),
            "最早的那条该被挤掉"
        );
        assert_eq!(
            recent.last().map(|entry| entry.title.as_str()),
            Some(format!("第 {RECENT_KEEP} 个称号").as_str())
        );

        // 同一个称号再出现时抬到最后，不占两个位置（同一批素材重发时会发生）。
        note_words(&persona::Recent {
            title: "第 1 个称号".to_string(),
            note: "换了一句话".to_string(),
            closing: "第 1 句判词".to_string(),
        });
        let recent = recent_words();
        assert_eq!(recent.len(), RECENT_KEEP);
        assert_eq!(
            recent
                .last()
                .map(|entry| (entry.title.as_str(), entry.note.as_str())),
            Some(("第 1 个称号", "换了一句话"))
        );

        // 三样都空的不记：那是一次没生成出东西的失败，不该占掉别人的说法。
        let before = recent_words().len();
        note_words(&persona::Recent::default());
        assert_eq!(recent_words().len(), before);
    }

    /// 判词也要进排除表：只有称号与一句话防撞车，十来份判词会套成同一个句式。
    #[test]
    fn the_closing_line_joins_the_exclusion_list() {
        let profile = persona::Persona {
            title: "夜班报错客服".to_string(),
            note: "他把白天让给了别的事".to_string(),
            closing: "舍不得关灯的人，灯也舍不得他".to_string(),
            ..Default::default()
        };
        // 记的是这份画像里那三样，投影不许漏掉判词。
        assert_eq!(profile.stamp().closing, "舍不得关灯的人，灯也舍不得他");
        note_words(&profile.stamp());
        let recent = recent_words();
        let last = recent.last().expect("刚记下的那份应该在");
        assert_eq!(last.closing, "舍不得关灯的人，灯也舍不得他");
        // 判词进不进得了提示词，由 persona 那边的用例盯着（那边有素材工厂）。
    }

    /// 缓存只留最近这几个：问过的人多了，最早的那份被挤掉。
    #[test]
    fn the_result_cache_keeps_only_the_most_recent_askers() {
        let base = 987_656_000;
        for offset in 0..=SERVED_KEEP as i64 {
            remember(base + offset, Cached::Report(format!("第 {offset} 份")));
        }
        assert!(served(base).is_none(), "最早的那份该被挤掉");
        assert!(matches!(
            served(base + SERVED_KEEP as i64),
            Some(Cached::Report(_))
        ));
    }

    #[test]
    fn the_text_report_carries_the_conclusion() {
        let material = crate::plugins::portrait::collect::Material {
            user_id: 1,
            name: "甲".into(),
            total: 100,
            first_time: 0,
            last_time: 86_400 * 9,
            active_days: 6,
            hour: {
                let mut hour = [0u64; 24];
                hour[23] = 40;
                hour
            },
            weekday: {
                let mut weekday = [0u64; 7];
                weekday[5] = 30;
                weekday
            },
            groups: Vec::new(),
            kinds: Default::default(),
            longest: 50,
            avg_len: 10.0,
            words: Vec::new(),
            phrases: Vec::new(),
            samples: Vec::new(),
            style: Default::default(),
            ties: vec![collect::Tie {
                user_id: 10001,
                name: "老张".into(),
                at_out: 12,
                at_in: 4,
                turn_out: 30,
                turn_in: 9,
                last_time: 0,
                samples: Vec::new(),
            }],
        };
        let profile = persona::Persona {
            title: "夜行改稿人".into(),
            note: "白天潜水夜里冒泡".into(),
            style: "话短，句尾常带问号。".into(),
            facets: vec![
                persona::Facet {
                    dimension: "性格".into(),
                    certainty: "可推".into(),
                    verdict: "说事先给结论".into(),
                    evidence: "三条长发言都是先下判断再补理由".into(),
                    quoted: false,
                },
                persona::Facet {
                    dimension: "生计".into(),
                    certainty: "明说".into(),
                    verdict: "在上班，要早起".into(),
                    evidence: "三点还在改".into(),
                    quoted: true,
                },
            ],
            ties: vec![persona::TieReading {
                id: 10001,
                line: "跟老张主要聊装机".into(),
            }],
            profile: vec![
                persona::Passage {
                    kind: "text".into(),
                    body: "他把手艺当退路。".into(),
                    ..Default::default()
                },
                persona::Passage {
                    kind: "quote".into(),
                    text: "三点还在改".into(),
                    note: "很有他".into(),
                    ..Default::default()
                },
            ],
            closing: "他把白天让给了别的事，深夜才回来认领自己。".into(),
            ..Default::default()
        };
        let report = text_report(&material, &profile, "deepseek/deepseek-flash", true);
        assert!(report.contains("夜行改稿人"));
        assert!(report.contains("白天潜水夜里冒泡"));
        assert!(report.contains("三点还在改"));
        // 档案：只有写出来的那两格才印，把握与依据都跟着落下来。
        assert!(report.contains("▍人物档案"));
        assert!(report.contains("　性格｜可推"));
        assert!(report.contains("　生计｜明说"));
        assert!(report.contains("　　说事先给结论"));
        assert!(report.contains("　　三条长发言都是先下判断再补理由"));
        assert!(
            report.contains("　　「三点还在改」"),
            "明说的依据按原话括起来"
        );
        assert!(!report.contains("　家庭｜"), "没有的格子不印");
        // 观测三节都在。
        assert!(report.contains("▍怎么说话"));
        assert!(report.contains("▍什么时候来"));
        assert!(report.contains("▍群内往来"));
        assert!(report.contains("老张（QQ 10001）"));
        assert!(report.contains("　　点名 我叫他 12 次 · 他叫我 4 次（我这边主动）"));
        assert!(report.contains("　　接话 我接他 30 次 · 他接我 9 次"));
        assert!(report.contains("跟老张主要聊装机"));
        // 图里有的信息，文字版一条不落：24 小时与一周的完整分布。
        assert!(report.contains("　0—23 时依次：0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 40"));
        assert!(report.contains("　日 一 二 三 四 五 六依次：0 0 0 0 0 30 0"));
        assert!(report.contains("▍画像综述"));
        assert!(report.contains("他把手艺当退路。"));
        // 判词在文字版里也要有自己的一行——它是一张图里分量最重的一块。
        assert!(report.contains("▍判词"));
        assert!(report.contains("他把白天让给了别的事，深夜才回来认领自己。"));
        assert!(report.contains("不等于本人"));
    }

    /// 模型没接上时，文字版要照实说档案空着，而观测三节照旧在。
    #[test]
    fn the_text_report_says_when_the_dossier_is_empty() {
        let material = crate::plugins::portrait::collect::Material {
            user_id: 1,
            name: "甲".into(),
            total: 4,
            first_time: 0,
            last_time: 86_400,
            active_days: 2,
            hour: [0; 24],
            weekday: [0; 7],
            groups: Vec::new(),
            kinds: Default::default(),
            longest: 8,
            avg_len: 4.0,
            words: Vec::new(),
            phrases: vec![("这就去".into(), 3)],
            samples: vec!["今天这个雨下得没完没了".into()],
            style: Default::default(),
            ties: Vec::new(),
        };
        let profile = persona::Persona::from_stats(&material);
        let report = text_report(&material, &profile, "deepseek/deepseek-flash", false);
        assert!(report.contains("这一层这次空着"));
        assert!(report.contains("▍怎么说话"));
        assert!(report.contains("▍什么时候来"));
        assert!(!report.contains("▍群内往来"), "没有往来对象就不占版面");
        assert!(report.contains("出图已关闭"));
    }
}
