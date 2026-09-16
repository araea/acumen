//! 「我在这个群里是谁」：群友看到的那个名字、那个头衔、那张头像。
//!
//! 在此之前，人格对自己的全部认知是一串 QQ 号。可群里没人用号码叫人——他们叫的是
//! 名片上的字，看的是那张头像。于是「A宝你在吗」读起来像在叫别人，「你头像好可爱」
//! 只能含糊地接一句，「你啥时候进的群」得现编一个。这些都不是措辞问题，是它确实
//! 不知道。
//!
//! 这里把平台上那份现成的答案取回来，摆进每一轮的上下文：
//!
//! - **账号昵称**（`login.get`）——改名片之前，群友看到的就是它；
//! - **本群名片与头衔**（`guild.member.get`）——同一个号在每个群里叫法都不一样，
//!   ②群里它叫「A宝好腻害！」，白虎群里它顶着「不再遗憾啦」的头衔；
//! - **身份与进群时刻**——是群主、管理还是普通成员，来了多久，都是会被问到的事；
//! - **群名**（`guild.get`）——「这是个什么群」本身就是语境；
//! - **头像长什么样**——下下来交给判定模型看一眼，换成一句话存着。
//!
//! 取一次要花四个来回，所以按群缓存 [`TTL`]；头像更慢也更不常变，按图的内容摘要
//! 存在磁盘上（`data/oai/chat/identity.json`），换了头像才重新看一眼。
//!
//! 这些全是设备本地状态：仓库里恢复不出来，换台机器从头取一遍即可。

use super::Avatar as AvatarAuth;
use crate::adapters::satori::{LockedWriter, api};
use crate::event::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// 一份身份资料活多久。名片和头衔是人改的，改完隔几个小时认出来就够；
/// 而每轮都去问一遍平台，等于给每条群消息加四个网络来回。
const TTL: Duration = Duration::from_secs(6 * 3_600);
/// 取资料的整体预算。拿不到就这一轮不带身份，不能让它拖住发言。
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
/// 看一眼头像的预算。
const AVATAR_TIMEOUT: Duration = Duration::from_secs(45);
/// 头像描述的字数上限：它每轮都在提示词里，写成一段就不划算了。
const AVATAR_CHARS: usize = 60;

/// 让判定模型描述头像的那句话。
///
/// 要的是「群友一眼看见什么」，不是一份图像分析报告——所以限定一句话，
/// 并且明说没有人物就别硬说有。
const AVATAR_RUBRIC: &str = "\
这是某个 QQ 用户的头像。用一句不超过 30 字的话描述它长什么样，让没见过的人能想象出来：
画的是什么、什么风格、主色调。只描述看得见的东西，看不清就说看不清，不要猜它象征什么，
不要加任何前缀或标点之外的修饰。";

/// 在一个群里的身份。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Identity {
    /// 自己的 QQ 号。
    pub user_id: i64,
    /// 账号昵称：没设名片的群里，群友看到的就是它。
    pub name: String,
    /// 本群名片；与昵称相同或没设时为空。
    pub card: String,
    /// 群头衔。
    pub title: String,
    /// `owner` / `admin` / 其余按普通成员算。
    pub role: String,
    /// 进群时刻（Unix 秒）；0 表示没问到。
    pub joined_at: i64,
    /// 群名。
    pub group_name: String,
    /// 头像长什么样，一句话；没看成时为空。
    pub avatar: String,
}

impl Identity {
    /// 群友在这个群里看到的那个名字。
    pub(crate) fn display(&self) -> &str {
        if self.card.is_empty() {
            &self.name
        } else {
            &self.card
        }
    }

    /// 身份说法。头衔和管理身份是两回事，各说各的。
    fn standing(&self) -> &'static str {
        match self.role.as_str() {
            "owner" => "群主",
            "admin" | "administrator" => "管理员",
            _ => "普通成员",
        }
    }

    /// 进群多久的口语说法；问不到就空着，不编。
    fn tenure(&self, now: i64) -> String {
        if self.joined_at <= 0 || now < self.joined_at {
            return String::new();
        }
        let days = (now - self.joined_at) / 86_400;
        match days {
            0 => "今天刚进群".to_string(),
            1..=30 => format!("进群 {days} 天"),
            31..=364 => format!("进群 {} 个月", days / 30),
            _ => {
                let years = days / 365;
                let months = (days % 365) / 30;
                if months == 0 {
                    format!("进群 {years} 年")
                } else {
                    format!("进群 {years} 年 {months} 个月")
                }
            }
        }
    }

    /// 注入提示词的那一段：判定与发言都看得见。
    ///
    /// 写成「群里看到的是什么」而不是一张字段表——人格要用的是「他们叫我 A宝」
    /// 这件事，不是 `card=A宝`。
    pub(crate) fn brief(&self, group: i64, now: i64) -> String {
        if self.name.is_empty() && self.card.is_empty() && self.group_name.is_empty() {
            return String::new();
        }
        let mut out = String::from("你自己：");
        let display = self.display();
        if !display.is_empty() {
            out.push_str(&format!("群里看到的你叫「{display}」"));
            if !self.card.is_empty() && !self.name.is_empty() && self.card != self.name {
                out.push_str(&format!("（这是你在本群的名片，账号昵称是「{}」）", self.name));
            }
            out.push('，');
        }
        out.push_str(&format!("QQ {}", self.user_id));
        if !self.title.is_empty() {
            out.push_str(&format!("，顶着「{}」的头衔", self.title));
        }
        out.push_str("，在这个群里是");
        out.push_str(self.standing());
        let tenure = self.tenure(now);
        if !tenure.is_empty() {
            out.push('，');
            out.push_str(&tenure);
        }
        out.push_str("。\n");
        if !self.group_name.is_empty() {
            out.push_str(&format!("这个群叫「{}」（{group}）。\n", self.group_name));
        }
        if !self.avatar.is_empty() {
            out.push_str(&format!(
                "你的头像：{}。有人夸头像或问你头像是什么，说的就是这张。\n",
                self.avatar.trim_end_matches(['。', '.'])
            ));
        }
        out
    }
}

/// 缓存里的一条。
struct Entry {
    identity: Identity,
    at: Instant,
}

#[derive(Default)]
struct Store {
    /// 头像描述的落盘位置。
    path: Option<PathBuf>,
    groups: HashMap<i64, Entry>,
    /// 群消息自己带着的群名（见 [`note_group_name`]）。
    seen_names: HashMap<i64, String>,
    /// 已读过磁盘上那份头像描述。
    loaded: bool,
    avatar: Avatar,
}

/// 磁盘上记着的那句头像描述。
///
/// 按图的内容摘要存：头像直链是固定的（`headimg_dl?dst_uin=…`），换了头像 URL 也
/// 不变，所以只能看内容。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Avatar {
    #[serde(default)]
    digest: String,
    #[serde(default)]
    note: String,
}

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Store::default()))
}

fn lock() -> MutexGuard<'static, Store> {
    store().lock().unwrap_or_else(|error| error.into_inner())
}

/// 指定落盘位置；启动时调用一次。
pub(crate) fn attach(base: &Path) {
    let mut store = lock();
    store.path = Some(base.join("identity.json"));
    store.groups.clear();
    store.seen_names.clear();
    store.avatar = Avatar::default();
    store.loaded = false;
}

fn ensure_loaded(store: &mut Store) {
    if store.loaded {
        return;
    }
    store.loaded = true;
    if let Some(avatar) = store
        .path
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str::<Avatar>(&raw).ok())
    {
        store.avatar = avatar;
    }
}

/// 记下群消息自己带着的群名。
///
/// `guild.get` 并不总给得出 `name`——沙箱群 280183116 就只回 id 和头像，而同一个群的
/// 每条消息都带着群名。平台那边问不到的时候，用群友刚发的那条消息上写的就是了。
pub(crate) fn note_group_name(group: i64, name: &str) {
    if name.is_empty() {
        return;
    }
    let mut store = lock();
    if store.seen_names.get(&group).is_some_and(|had| had == name) {
        return;
    }
    store.seen_names.insert(group, name.to_string());
    // 已经缓存着一份没有群名的身份时顺手补上，不必等它过期。
    if let Some(entry) = store.groups.get_mut(&group)
        && entry.identity.group_name.is_empty()
    {
        entry.identity.group_name = name.to_string();
    }
}

/// 直接摆一份身份进缓存。测试用：认名字与提示词拼装都不该为了一次断言去连平台。
#[cfg(test)]
pub(crate) fn seed(group: i64, identity: Identity) {
    lock().groups.insert(
        group,
        Entry {
            identity,
            at: Instant::now(),
        },
    );
}

/// 手上这份身份资料；没取到过就是 `None`。
pub(crate) fn of(group: i64) -> Option<Identity> {
    lock().groups.get(&group).map(|e| e.identity.clone())
}

/// 注入提示词的那一段；还没取到时为空串。
pub(crate) fn brief(group: i64) -> String {
    of(group)
        .map(|identity| identity.brief(group, chrono::Local::now().timestamp()))
        .unwrap_or_default()
}

/// 这条消息里有没有人直接喊它的名字。
///
/// @ 和引用在协议里是明码，喊名字不是——群友多数时候就是打两个字。认出来只加一个
/// 记号（见 [`super::window::transcript`]），不像 @ 那样直接把人格叫醒：名字是会
/// 撞车的，「秘」既是它的名片也是别人的半句话，把这种猜测当成点名会让它到处接话。
pub(crate) fn called_by_name(group: i64, aliases: &[String], text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let lower = text.to_lowercase();
    let identity = of(group);
    let names = identity
        .iter()
        .flat_map(|identity| [identity.card.as_str(), identity.name.as_str()])
        .chain(aliases.iter().map(String::as_str));
    names
        .map(str::trim)
        // 一个字的名字在中文里到处都是，认它等于每句话都算点名。
        .filter(|name| name.chars().count() >= 2)
        .any(|name| lower.contains(&name.to_lowercase()))
}

/// 需要的话去平台问一遍「我在这个群里是谁」。
///
/// 缓存还新鲜就立刻返回，所以可以每一批都调。任何一步失败都只是这一项空着——
/// 问不到名片不该让整轮搭话失败。
pub(crate) async fn refresh(
    ctx: &Context,
    writer: &LockedWriter,
    avatar: Option<&AvatarAuth>,
    group: i64,
) {
    let fresh = {
        let store = lock();
        store
            .groups
            .get(&group)
            .is_some_and(|entry| entry.at.elapsed() < TTL)
    };
    if fresh {
        return;
    }
    let login = ctx.bot.login_user.get();
    let Ok(me) = login.id.parse::<i64>() else {
        return;
    };
    let mut identity = probe(
        ctx,
        writer,
        group,
        me,
        login
            .name
            .clone()
            .or_else(|| login.nick.clone())
            .unwrap_or_default(),
    )
    .await;
    identity.avatar = avatar_note(avatar, login.avatar.as_deref()).await;
    let mut store = lock();
    store.groups.insert(
        group,
        Entry {
            identity,
            at: Instant::now(),
        },
    );
}

/// 问平台「我在这个群里是谁」。
///
/// 三样里任何一样问不到都只是那一项空着——名片问不到不该让整轮搭话失败。整体也有
/// 一个预算：平台卡住的时候，这一轮宁可不带身份，也不能让群聊等着。
pub(super) async fn probe(
    ctx: &Context,
    writer: &LockedWriter,
    group: i64,
    me: i64,
    account_name: String,
) -> Identity {
    let mut identity = Identity {
        user_id: me,
        name: account_name,
        ..Identity::default()
    };
    let ask = async {
        if let Ok(member) = api::get_group_member_info(ctx, writer.clone(), group, me, false).await
        {
            if !member.nickname.is_empty() {
                identity.name = member.nickname;
            }
            identity.card = member.card;
            identity.title = member.title;
            identity.role = member.role;
            identity.joined_at = member.join_time;
        }
        if let Ok(guild) = api::get_guild_info(ctx, writer.clone(), group).await {
            identity.group_name = guild.group_name;
        }
    };
    if tokio::time::timeout(FETCH_TIMEOUT, ask).await.is_err() {
        debug!(target: super::LOG_TARGET, "群 {group} 取自己的群身份超时，这一轮先不带");
    }
    // 名片和昵称一样时它不是「另一个名字」，留着只会在提示词里重复一遍。
    if identity.card == identity.name {
        identity.card.clear();
    }
    if identity.group_name.is_empty()
        && let Some(seen) = lock().seen_names.get(&group)
    {
        identity.group_name = seen.clone();
    }
    identity
}

/// 头像那一句：磁盘上有且还是同一张图就直接用，换了才重新看一眼。
async fn avatar_note(avatar: Option<&AvatarAuth>, url: Option<&str>) -> String {
    let Some(url) = url.filter(|url| url.starts_with("http")) else {
        return String::new();
    };
    let Some(image) = super::vision::usable_image(url).await else {
        return remembered_note();
    };
    let digest = format!("{:x}", md5::compute(image.as_bytes()));
    {
        let mut store = lock();
        ensure_loaded(&mut store);
        if store.avatar.digest == digest && !store.avatar.note.is_empty() {
            return store.avatar.note.clone();
        }
    }
    let note = match avatar {
        None => {
            debug!(target: super::LOG_TARGET, "没有可用的接口，先不看头像");
            return remembered_note();
        }
        Some(avatar) => {
            match describe(&avatar.api_base, &avatar.api_key, &avatar.model, &image).await {
                Ok(note) => note,
                Err(error) => {
                    debug!(target: super::LOG_TARGET, "看不成自己的头像：{error:#}");
                    return remembered_note();
                }
            }
        }
    };
    if note.is_empty() {
        return remembered_note();
    }
    let remembered = Avatar {
        digest,
        note: note.clone(),
    };
    let path = {
        let mut store = lock();
        store.avatar = remembered.clone();
        store.path.clone()
    };
    if let Some(path) = path
        && let Ok(json) = serde_json::to_string(&remembered)
    {
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        if let Err(error) = tokio::fs::write(&path, json).await {
            warn!(target: super::LOG_TARGET, "写入头像描述失败：{error}");
        }
    }
    info!(target: super::LOG_TARGET, "看了一眼自己的头像：{note}");
    note
}

/// 磁盘上那句（可能是上一张头像的）。看不成新的时候，旧的也好过没有。
fn remembered_note() -> String {
    let mut store = lock();
    ensure_loaded(&mut store);
    store.avatar.note.clone()
}

/// 把头像交给判定模型，换回一句话。
pub(super) async fn describe(
    api_base: &str,
    api_key: &str,
    model: &str,
    image: &str,
) -> anyhow::Result<String> {
    use rig_core::completion::Message;
    use rig_core::completion::message::{DocumentSourceKind, Image, Text, UserContent};

    let messages = vec![
        Message::System {
            content: AVATAR_RUBRIC.to_string(),
        },
        Message::User {
            content: vec![
                UserContent::Image(Image {
                    data: DocumentSourceKind::Url(image.to_string()),
                    media_type: None,
                    detail: None,
                    additional_params: None,
                }),
                UserContent::Text(Text::new("这张头像长什么样？")),
            ],
        },
    ];
    let raw = tokio::time::timeout(
        AVATAR_TIMEOUT,
        crate::plugins::oai::llm::complete(api_base, api_key, model, messages, None),
    )
    .await
    .map_err(|_| anyhow::anyhow!("看头像超时"))??;
    Ok(trim_note(&raw))
}

/// 模型的回答 → 能摆进提示词的一句话。
fn trim_note(raw: &str) -> String {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .trim_start_matches(['-', '*', '「', '"'])
        .trim_end_matches(['」', '"'])
        .trim();
    line.chars().take(AVATAR_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Identity {
        Identity {
            user_id: 3373167460,
            name: "nawyjx".into(),
            card: "A宝好腻害！".into(),
            title: "不再遗憾啦".into(),
            role: "member".into(),
            joined_at: 0,
            group_name: "②群心情管家•助手".into(),
            avatar: "一只歪着头的白猫，背景淡粉".into(),
        }
    }

    /// 群里叫的是名片，不是 QQ 号——这一段的全部意义就在这里。
    #[test]
    fn the_brief_leads_with_the_name_people_actually_see() {
        let brief = sample().brief(818965288, 0);
        assert!(brief.contains("群里看到的你叫「A宝好腻害！」"), "{brief}");
        assert!(brief.contains("账号昵称是「nawyjx」"), "{brief}");
        assert!(brief.contains("不再遗憾啦"), "{brief}");
        assert!(brief.contains("普通成员"), "{brief}");
        assert!(brief.contains("②群心情管家•助手"), "{brief}");
        assert!(brief.contains("白猫"), "{brief}");
    }

    /// 没设名片的群里，群友看到的就是账号昵称，不该凭空多出一个「名片」。
    #[test]
    fn a_group_without_a_card_shows_the_account_name_only_once() {
        let identity = Identity {
            card: String::new(),
            ..sample()
        };
        assert_eq!(identity.display(), "nawyjx");
        let brief = identity.brief(2, 0);
        assert!(brief.contains("群里看到的你叫「nawyjx」"), "{brief}");
        assert!(!brief.contains("名片"), "{brief}");
    }

    /// 什么都没取到时给空串：宁可不带这一段，也不摆一份空表让模型去填。
    #[test]
    fn nothing_known_means_nothing_injected() {
        assert!(Identity::default().brief(2, 0).is_empty());
    }

    #[test]
    fn tenure_reads_like_a_person_would_say_it() {
        let day = 86_400;
        // 没问到入群时刻就空着：这一句宁可不说，也不能说成「今天刚进群」。
        assert_eq!(
            Identity {
                joined_at: 0,
                ..sample()
            }
            .tenure(3 * day),
            ""
        );
        let since = |days: i64| Identity {
            joined_at: 1_700_000_000,
            ..sample()
        }
        .tenure(1_700_000_000 + days * day);
        assert_eq!(since(0), "今天刚进群");
        assert_eq!(since(9), "进群 9 天");
        assert_eq!(since(90), "进群 3 个月");
        assert_eq!(since(400), "进群 1 年 1 个月");
        assert_eq!(since(366), "进群 1 年");
    }

    /// 群里喊的是名片上的字。认出来的是名片、账号昵称和写在配置里的小名。
    #[test]
    fn a_plain_name_in_the_text_counts_as_being_called() {
        let group = -9_200_001;
        seed(group, sample());
        let aliases = vec!["A宝".to_string()];
        assert!(called_by_name(group, &aliases, "A宝 在吗"));
        assert!(called_by_name(group, &aliases, "A宝好腻害！这也能修好"));
        assert!(called_by_name(group, &aliases, "@nawyjx 帮我看看"));
        assert!(!called_by_name(group, &aliases, "这破依赖装了半天"));
        assert!(!called_by_name(group, &aliases, ""));
    }

    /// 一个字的名字在中文里到处都是；认它等于每句话都算点名。
    #[test]
    fn single_character_names_are_never_matched() {
        let group = -9_200_002;
        seed(
            group,
            Identity {
                card: "秘".into(),
                name: "秘".into(),
                ..sample()
            },
        );
        let aliases = vec!["宝".to_string()];
        assert!(!called_by_name(group, &aliases, "这事挺神秘的"));
        assert!(!called_by_name(group, &aliases, "宝子们看这个"));
    }

    #[test]
    fn the_avatar_note_is_one_line_without_decoration() {
        assert_eq!(trim_note("「一只白猫」\n\n另外还有……"), "一只白猫");
        assert_eq!(trim_note("\n- 蓝色像素风的猫  "), "蓝色像素风的猫");
        assert_eq!(trim_note("").len(), 0);
        assert!(trim_note(&"猫".repeat(200)).chars().count() <= AVATAR_CHARS);
    }
}
