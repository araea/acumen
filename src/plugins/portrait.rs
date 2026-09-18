//! 用户画像：读一个群成员的历史发言，给他打一份标签化的画像，出一张图文报告。
//!
//! 指令只有一条，`画像`。不带参数是查自己，@ 一个人或直接写 QQ 号是查别人。
//! 报告由三段拼成——[`collect`] 从库里取出可统计的事实与发言样本，[`persona`] 把素材
//! 交给模型换回一份画像（综合标签、四个维度的标签、一段综述），[`card`] 排成一张 HTML
//! 报告图；[`avatar`] 取对象的 QQ 头像配在报告开头。
//!
//! 用户画像是从行为数据里抽象出来的**标签化模型**，这一层不做事的是三件：不编数字（统计量
//! 由 [`collect`] 从库里算出来），不编原话（引语逐字比对样本），不编维度（四个维度与三层
//! 抽象写死在 [`persona`] 里）。模型不接时画像仍在——标签退回统计直出，缺的只是推断层与
//! 那段综述。
//!
//! 两处刻意的保护：同一个目标同时在跑只允许一次，`cooldown_seconds` 之内也不重复，
//! 免得群里连着刷。这两道闸只影响发指令的人，不影响其它功能。

pub mod avatar;
pub mod card;
pub mod collect;
pub mod persona;

use crate::adapters::satori::{LockedWriter, send_msg};
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
const KEYWORDS: [&str; 6] = [
    "用户画像报告",
    "用户画像",
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
}

/// 成品缓存留几个。冷却默认三分钟，够覆盖「同一批人反复问」的场面。
const SERVED_KEEP: usize = 6;

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
                say(
                    &ctx,
                    writer,
                    group_id,
                    requester,
                    message_id,
                    "这张画像正在生成，画完会自动发出".to_string(),
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
                        reply = reply.text(format!("还是刚才那份画像，{left} 秒后可重新生成"));
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
                        say(
                            &ctx,
                            writer,
                            group_id,
                            requester,
                            message_id,
                            format!("刚画过，{left} 秒后可重新生成"),
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
        say(
            &ctx,
            writer.clone(),
            group_id,
            requester,
            message_id,
            format!("正在读 {who} 的发言记录，整理画像…"),
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
        };
        let material = match collect::collect(&ctx.db, &request).await {
            Ok(Some(material)) => material,
            Ok(None) => {
                say(
                    &ctx,
                    writer,
                    group_id,
                    requester,
                    message_id,
                    // 空态不是错误：说清为什么空，再给一条能立刻做的事。
                    format!("📭 没有找到 {who} 在群里的发言记录\n他在这段时间里没在群里说过话，或换个时间范围再试"),
                )
                .await;
                return Ok(None);
            }
            Err(error) => {
                error!(target: LOG_TARGET, "查询发言记录失败：{error:#}");
                say(
                    &ctx,
                    writer,
                    group_id,
                    requester,
                    message_id,
                    format!("❌ 查记录时出错了：{error}"),
                )
                .await;
                return Ok(None);
            }
        };

        // 标签里的「事实」与「统计」两层全从统计量里来，模型只负责推断层与那段综述。
        let (base, key, model) = match endpoint(&ctx, &config.model).await {
            Ok(triple) => triple,
            Err(error) => {
                warn!(target: LOG_TARGET, "模型接口不可用：{error:#}");
                say(
                    &ctx,
                    writer,
                    group_id,
                    requester,
                    message_id,
                    format!("模型接口没配好：{error}"),
                )
                .await;
                return Ok(None);
            }
        };

        let thinking = config.thinking.trim();
        let history = vec![
            LlmMessage::System {
                content: persona::system_prompt().to_string(),
            },
            LlmMessage::User {
                content: vec![UserContent::Text(Text::new(persona::user_prompt(&material)))],
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

        let avatar = avatar::data_url(material.user_id).await;
        let view = card::View {
            material: &material,
            persona: &profile,
            avatar: avatar.as_deref(),
            model: &model,
            theme: &config.theme,
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
                if send_msg(&ctx, writer, group_id, Some(requester), reply)
                    .await
                    .is_ok()
                {
                    remember(target, Cached::Card(base64));
                }
            }
            Err(error) => {
                if config.image_enabled {
                    error!(target: LOG_TARGET, "画像出图失败：{error:#}");
                }
                // 出图失败或主动关图都不该等于没有结果：退回成文字版。
                let summary = text_report(&material, &profile, &model, config.image_enabled);
                remember(target, Cached::Report(summary.clone()));
                say(&ctx, writer, group_id, requester, message_id, summary).await;
            }
        }

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
    let providers =
        get_config_or_default::<crate::plugins::oai::OaiConfig>(ctx, "oai").providers;
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

/// 卡片图之外的文字版：综合标签、四个维度下的标签、综述与那句边界说明，
/// 够用户在群里看懂这份画像，不至于因为一张图没出成就什么都拿不到。
///
/// `image_enabled` 只影响最后那句交代：是「出图失败」还是「本来就关了图」。
fn text_report(
    material: &collect::Material,
    profile: &persona::Persona,
    model: &str,
    image_enabled: bool,
) -> String {
    let mut out = format!("▍{}\n{}\n", profile.title, profile.note);
    out.push_str(&format!(
        "\n▍读数\n共 {} 条群聊发言，覆盖 {} 天（活跃 {} 天），单条平均 {:.1} 字，\
         {}前后最密。样本充分性：{}。\n",
        material.total,
        material.span_days(),
        material.active_days,
        material.avg_len(),
        persona::hour_label(material.peak_hour()),
        material.sufficiency().label(),
    ));

    for (dimension, _) in persona::DIMENSIONS {
        let tags = profile.tags_of(dimension);
        if tags.is_empty() {
            continue;
        }
        out.push_str(&format!("\n▍{dimension}\n"));
        for tag in tags {
            out.push_str(&format!("　{}｜{}\n", tag.tier(), tag.label));
            if !tag.evidence.trim().is_empty() {
                out.push_str(&format!("　　{}\n", tag.evidence));
            }
        }
    }

    let passages: Vec<&persona::Passage> = profile.live_passages().collect();
    if !passages.is_empty() {
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
    }

    let footer = if image_enabled {
        "出图失败，先给你一份文字版"
    } else {
        "出图已关闭，这一份是文字版"
    };
    out.push_str(&format!(
        "\n画像是对行为的抽象，有损：只含他在群里说过的部分，不等于本人。\
         {footer}（{model}）。"
    ));
    out
}

async fn say(
    ctx: &Context,
    writer: LockedWriter,
    group_id: Option<i64>,
    user_id: i64,
    message_id: i64,
    text: String,
) {
    let mut message = Message::new();
    if message_id > 0 {
        message = message.reply(message_id);
    }
    if let Err(error) = send_msg(ctx, writer, group_id, Some(user_id), message.text(text)).await {
        warn!(target: LOG_TARGET, "回复失败：{error}");
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
        };
        let material = collect::collect(&db, &request)
            .await
            .expect("查询失败")
            .expect("这个人没有群聊记录");
        println!(
            "===== 素材 =====\n{} 条发言，覆盖 {} 天，样本充分性 {}",
            material.total,
            material.span_days(),
            material.sufficiency().label()
        );

        let history = vec![
            LlmMessage::System {
                content: persona::system_prompt().to_string(),
            },
            LlmMessage::User {
                content: vec![UserContent::Text(Text::new(persona::user_prompt(&material)))],
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
            "===== 收口后的画像 =====\n综合标签：{}\n一句话：{}\n标签：\n{}\n综述：\n{}",
            profile.title,
            profile.note,
            profile
                .tags
                .iter()
                .map(|tag| format!("　{}｜{}｜{}｜{}", tag.dim().unwrap_or("?"), tag.tier(), tag.label, tag.evidence))
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
        );
        assert!(!profile.title.is_empty(), "综合标签不该是空的");
        assert!(!profile.note.is_empty(), "一句话概括不该是空的");
        // 四个维度里至少三个有标签，否则这份画像没成形。
        let covered = persona::DIMENSIONS
            .iter()
            .filter(|(name, _)| !profile.tags_of(name).is_empty())
            .count();
        assert!(covered >= 3, "只有 {covered} 个维度有标签");
        assert!(
            profile.live_passages().count() >= 4,
            "综述至少要有四段，这次只有 {} 段",
            profile.live_passages().count()
        );
        // 引语是被比对过的：要么没有，要么每一句都是原话。
        for passage in profile.live_passages().filter(|passage| passage.is_quote()) {
            assert!(
                material
                    .samples
                    .iter()
                    .any(|sample| sample.contains(&passage.text)),
                "引语不在样本里：{}",
                passage.text
            );
        }

        let avatar = avatar::data_url(material.user_id).await;
        let html = card::html(&card::View {
            material: &material,
            persona: &profile,
            avatar: avatar.as_deref(),
            model: &model,
            theme: "auto",
            offset: beijing(),
            now,
        });
        let base64 = card::capture(&html, 2.0).await.expect("出图失败");
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&base64)
            .unwrap();
        let image = image::load_from_memory(&bytes).unwrap();
        println!("出图 {}×{}", image.width(), image.height());
        let path = std::env::temp_dir().join("ayjx-portrait-live.jpg");
        std::fs::write(&path, &bytes).ok();
        println!("出图已写入 {}", path.display());
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
        let raw = std::fs::read_to_string(format!("{manifest}/config.toml"))
            .expect("读不到 config.toml");
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
        for input in ["画像", " 画像 ", "用户画像", "我的画像", "画像报告", "用户画像报告"] {
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
            hour: [0; 24],
            weekday: [0; 7],
            groups: Vec::new(),
            kinds: Default::default(),
            longest: 50,
            avg_len: 10.0,
            words: Vec::new(),
            samples: Vec::new(),
        };
        let profile = persona::Persona {
            title: "夜行改稿人".into(),
            note: "白天潜水夜里冒泡".into(),
            tags: vec![
                persona::Tag {
                    dimension: "活跃".into(),
                    layer: "事实".into(),
                    label: "夜里出现".into(),
                    evidence: "夜间发言占 41%".into(),
                },
                persona::Tag {
                    dimension: "表达".into(),
                    layer: "推断".into(),
                    label: "句子短".into(),
                    evidence: String::new(),
                },
            ],
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
            ..Default::default()
        };
        let report = text_report(&material, &profile, "deepseek/deepseek-flash", true);
        assert!(report.contains("夜行改稿人"));
        assert!(report.contains("白天潜水夜里冒泡"));
        assert!(report.contains("三点还在改"));
        assert!(report.contains("共 100 条群聊发言"));
        // 四个维度的小标题、三条层级、标签与证据都要跟着落到文字版里。
        assert!(report.contains("▍活跃"));
        assert!(report.contains("▍表达"));
        assert!(!report.contains("▍内容"), "没有内容的维度不印");
        assert!(report.contains("事实｜夜里出现"));
        assert!(report.contains("夜间发言占 41%"));
        assert!(report.contains("推断｜句子短"));
        assert!(report.contains("▍画像综述"));
        assert!(report.contains("他把手艺当退路。"));
        assert!(report.contains("不等于本人"));
    }
}
