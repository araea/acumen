//! 群里管规则：「自动回复 …」。群主、群管理员（`managers = "staff"` 时）与机器人管理员能用。
//!
//! 在群里加的规则只管这个群；所有群共用的规则只有机器人管理员能加、改、删，群管理员
//! 可以在自己群里把它停用。号主私聊机器人用同一套指令，管的就是所有群共用的那些。
//! 普通群友发「问答」看本群能问什么（[`menu`]）。

use super::book::{self, Asked, BOOK, Mode, Reply, Rule};
use super::reply::{self, Bit};
use super::{Config, LOG_TARGET};
use crate::adapters::satori::{LockedWriter, api, send_msg};
use crate::command::{self, CommandMatch};
use crate::event::Context;
use crate::message::Message;
use crate::plugins::PluginError;

const PAGE: usize = 15;

fn usage(manager: bool) -> String {
    let mut lines = vec![
        "自动回复的用法（编号看「自动回复 列表」）：",
        "· 添加 <触发词> <回复>：本群有人说到触发词就回这句；几个触发词用 | 隔开，回复里可以带图。也可以引用一条消息（含合并转发）再发「自动回复 添加 触发词」",
        "· 加说法 <编号> <回复>：再加一种回复，每次随机一种；改 <编号> <回复>：换掉全部回复",
        "· 加词 / 删词 <编号> <触发词…>：增删触发词",
        "· 模式 <编号> 精确|包含|开头|正则：怎么比（精确最不会误触）",
        "· 转发 <编号> [开|关]：折成合并转发发；艾特 <编号> [开|关]：只在 @ 我时回；冷却 <编号> <秒|默认>",
        "· 停用 / 启用 <编号>；删除 <编号…>；恢复：找回刚删的",
        "· 看 <编号>：详情并发一遍回复；试 <一句话>：看这句会不会触发、由哪条回",
    ];
    if manager {
        lines.push("· 全局添加 <触发词> <回复>：所有群共用（仅机器人管理员）");
    }
    lines.push("群友发「问答」能看到本群可以问的触发词。");
    lines.join("\n")
}

/// 一条群指令。返回 false 表示这条消息不归这里管（不是管理员、或只是在聊天），照常往下传。
pub async fn run(ctx: &Context, writer: &LockedWriter, config: &Config, group: Option<&str>, matched: CommandMatch) -> Result<bool, PluginError> {
    let Some(message) = ctx.as_message() else {
        return Ok(false);
    };
    let manager = crate::access::is_manager(ctx);
    let staff = manager || (config.managers == "staff" && group.is_some() && message.sender_role().is_some_and(api::is_staff));
    if !staff {
        return Ok(false);
    }
    let here = Here {
        ctx,
        writer,
        config,
        group,
        manager,
        user: message.user_id().to_string(),
        message_id: message.message_id().to_string(),
    };
    let arg = command::extract_text_arg(&matched.args);
    let words: Vec<&str> = arg.split_whitespace().collect();
    let word = words.first().copied().unwrap_or("");
    let rest: Vec<&str> = words.iter().skip(1).copied().collect();
    let outcome = match word {
        "" | "列表" | "list" => Ok(here.list(&rest).await),
        "看" | "详情" | "查看" => here.detail(&rest).await,
        "添加" | "加" | "新增" => here.add(&matched, false).await,
        "全局添加" => {
            if manager {
                here.add(&matched, true).await
            } else {
                Err("所有群共用的规则只有机器人管理员能加；在本群加用「自动回复 添加」".into())
            }
        }
        "加说法" | "加回复" => here.variant(&matched, &rest, false).await,
        "改" | "修改" => here.variant(&matched, &rest, true).await,
        "加词" => here.keywords(&rest, true).await,
        "删词" => here.keywords(&rest, false).await,
        "模式" | "比法" => here.mode(&rest).await,
        "转发" => here.flag(&rest, "折成合并转发", |rule| &mut rule.forward).await,
        "艾特" | "@" => here.flag(&rest, "只在 @ 我时回", |rule| &mut rule.at_only).await,
        "冷却" => here.cooldown(&rest).await,
        "停用" | "关" | "关闭" => here.toggle(&rest, false).await,
        "启用" | "开" | "开启" => here.toggle(&rest, true).await,
        "删除" | "删" => here.delete(&rest).await,
        "恢复" | "撤销" => here.restore(&rest).await,
        "试" | "测试" => Ok(here.test(&arg[word.len()..]).await),
        "帮助" | "-h" | "help" => Ok(usage(manager)),
        // 没有指令前缀时「自动回复 好用吗」只是在聊天，不接话。
        _ if command::get_prefixes(ctx).iter().all(String::is_empty) => return Ok(false),
        _ => Err(format!("不认识「{word}」\n{}", usage(manager))),
    };
    let text = outcome.unwrap_or_else(|problem| problem);
    if !text.is_empty() {
        here.say(text).await?;
    }
    Ok(true)
}

/// 普通群友发「问答」：本群能问的触发词。
pub async fn menu(ctx: &Context, writer: &LockedWriter, group: Option<&str>) -> Result<(), PluginError> {
    let Some(message) = ctx.as_message() else {
        return Ok(());
    };
    let lines: Vec<String> = BOOK
        .read(|b| {
            b.visible(group)
                .into_iter()
                .filter(|rule| rule.enabled && rule.covers(group) && rule.mode != Mode::Regex)
                .map(|rule| {
                    let words = rule.keywords.join("、");
                    if rule.at_only { format!("· {words}（要 @ 我）") } else { format!("· {words}") }
                })
                .collect()
        })
        .await;
    let text = if lines.is_empty() {
        "这里还没有设常见问题。".to_string()
    } else {
        let total = lines.len();
        let mut shown: Vec<String> = lines.into_iter().take(30).collect();
        if total > shown.len() {
            shown.push(format!("……共 {total} 条，只列前 30 条"));
        }
        format!("常见问题：直接发下面的词，我来回答\n{}", shown.join("\n"))
    };
    let quoted = Message::new().reply(message.message_id()).text(text);
    let user = group.is_none().then(|| message.user_id());
    send_msg(ctx, writer.clone(), group, user, quoted).await?;
    Ok(())
}

/// 这条指令在哪儿、谁发的。
struct Here<'a> {
    ctx: &'a Context,
    writer: &'a LockedWriter,
    config: &'a Config,
    group: Option<&'a str>,
    manager: bool,
    user: String,
    message_id: String,
}

type Said = Result<String, String>;

/// 「#3」「3」「３」都认。
fn number(word: &str) -> Option<u32> {
    let folded: String = word
        .trim_start_matches(['#', '＃'])
        .chars()
        .map(|c| if ('０'..='９').contains(&c) { char::from_u32(c as u32 - 0xFEE0).unwrap_or(c) } else { c })
        .collect();
    folded.parse().ok().filter(|n| *n > 0)
}

/// 「下载|安装包」→ 两个触发词。
fn keywords_of(word: &str) -> Vec<String> {
    word.split(['|', '｜']).map(str::trim).filter(|k| !k.is_empty()).map(str::to_owned).collect()
}

/// 指令参数里跳过前 `tokens` 个词，剩下的（文字、图、表情原样排着）就是回复内容。
fn content_after(matched: &CommandMatch, tokens: usize) -> Vec<Bit> {
    let mut need = tokens;
    let mut out = Vec::new();
    for bit in reply::bits_of_args(&matched.args) {
        if need > 0 {
            if let Bit::Text(text) = &bit {
                let mut rest = text.as_str();
                while need > 0 {
                    rest = rest.trim_start();
                    if rest.is_empty() {
                        break;
                    }
                    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                    rest = &rest[end..];
                    need -= 1;
                }
                let rest = rest.trim_start();
                if !rest.is_empty() {
                    out.push(Bit::Text(rest.to_string()));
                }
                continue;
            }
            // 词还没数够就遇到了图：后面都是内容。
            need = 0;
        }
        out.push(bit);
    }
    out
}

fn ago(at: i64) -> String {
    if at <= 0 {
        return "还没回过".into();
    }
    let delta = (book::now() - at).max(0);
    match delta {
        0..60 => "刚刚".into(),
        60..3600 => format!("{} 分钟前", delta / 60),
        3600..86400 => format!("{} 小时前", delta / 3600),
        _ => format!("{} 天前", delta / 86400),
    }
}

impl Here<'_> {
    async fn say(&self, text: String) -> Result<(), PluginError> {
        let message = Message::new().reply(&self.message_id).text(text);
        let user = self.group.is_none().then_some(self.user.as_str());
        send_msg(self.ctx, self.writer.clone(), self.group, user, message).await?;
        Ok(())
    }

    fn scope(&self) -> &'static str {
        if self.group.is_some() { "本群" } else { "所有群" }
    }

    fn where_text(&self, rule: &Rule) -> String {
        if rule.groups.is_empty() {
            let off = self.group.is_some_and(|g| rule.exclude.iter().any(|x| x == g));
            if off { "所有群共用（本群已停用）".into() } else { "所有群共用".into() }
        } else if rule.groups.len() == 1 && self.group == Some(rule.groups[0].as_str()) {
            "只在本群".into()
        } else {
            format!("只在 {} 个群", rule.groups.len())
        }
    }

    /// 能不能改这一条：只在本群用的群管理员能改；所有群共用的、跨好几个群的只有机器人管理员能改。
    fn may_edit(&self, rule: &Rule) -> Result<(), String> {
        if self.manager {
            return Ok(());
        }
        match self.group {
            Some(group) if rule.groups.len() == 1 && rule.groups[0] == group => Ok(()),
            _ if rule.groups.is_empty() => Err(format!(
                "#{} 是所有群共用的规则，只有机器人管理员能改；只想本群不回，发「自动回复 停用 {}」",
                rule.id, rule.id
            )),
            _ => Err(format!("#{} 不只在本群用，只有机器人管理员能改", rule.id)),
        }
    }

    /// 找一条这里看得到的规则。
    async fn find(&self, word: Option<&&str>) -> Result<Rule, String> {
        let id = word.and_then(|w| number(w)).ok_or("要哪一条？写上编号，比如「自动回复 看 3」；编号看「自动回复 列表」")?;
        let group = self.group;
        BOOK.read(|b| b.get(id).filter(|r| group.is_none_or(|g| r.groups.is_empty() || r.groups.iter().any(|x| x == g))).cloned())
            .await
            .ok_or_else(|| format!("{}没有 #{id} 这条规则", self.scope()))
    }

    /// 改一条规则：先找、再查权限、再改、再存。
    async fn edit(&self, word: Option<&&str>, change: impl FnOnce(&mut Rule) -> Result<String, String>) -> Said {
        let mut rule = self.find(word).await?;
        self.may_edit(&rule)?;
        let said = change(&mut rule)?;
        let saved = BOOK.with(|b| b.replace(rule)).await?;
        info!(target: LOG_TARGET, "{} 改了 #{}：{said}", self.user, saved.id);
        Ok(said)
    }

    async fn list(&self, rest: &[&str]) -> String {
        let page = rest.first().and_then(|w| number(w)).unwrap_or(1) as usize;
        let group = self.group;
        let rows: Vec<String> = BOOK
            .read(|b| {
                b.visible(group)
                    .into_iter()
                    .map(|rule| {
                        let mut bits = vec![rule.mode.label().to_string(), self.where_text(rule)];
                        if rule.replies.len() > 1 {
                            bits.push(format!("{} 种回复", rule.replies.len()));
                        }
                        if rule.replies.iter().any(|r| matches!(r, Reply::Forward { .. })) || rule.forward {
                            bits.push("合并转发".into());
                        }
                        if rule.at_only {
                            bits.push("要 @".into());
                        }
                        bits.push(if rule.hits > 0 { format!("回过 {} 次", rule.hits) } else { "还没回过".into() });
                        let off = !rule.enabled || group.is_some_and(|g| rule.exclude.iter().any(|x| x == g));
                        format!("#{} {}{} · {}", rule.id, rule.title(), if off { "（停用）" } else { "" }, bits.join(" · "))
                    })
                    .collect()
            })
            .await;
        if rows.is_empty() {
            return format!(
                "{}还没有自动回复。\n加一条：自动回复 添加 下载 安装包在群文件里～\n更多用法：自动回复 帮助",
                self.scope()
            );
        }
        let pages = rows.len().div_ceil(PAGE);
        let page = page.min(pages);
        let mut lines = vec![format!("{}的自动回复（共 {} 条{}）", self.scope(), rows.len(), if pages > 1 { format!("，第 {page}/{pages} 页") } else { String::new() })];
        lines.extend(rows.into_iter().skip((page - 1) * PAGE).take(PAGE));
        lines.push(if pages > page { format!("下一页：自动回复 列表 {}", page + 1) } else { "看详情：自动回复 看 <编号>；全部用法：自动回复 帮助".into() });
        lines.join("\n")
    }

    async fn detail(&self, rest: &[&str]) -> Said {
        let rule = self.find(rest.first()).await?;
        let max = self.config.contains_max_chars;
        let mut lines = vec![format!("#{} {}{}", rule.id, rule.title(), if rule.enabled { "" } else { "（停用）" })];
        if rule.keywords.len() > 3 {
            lines.push(format!("· 触发词：{}", rule.keywords.join("、")));
        }
        lines.push(format!("· 怎么比：{}（{}）", rule.mode.label(), rule.mode.explain(max)));
        lines.push(format!("· 范围：{}", self.where_text(&rule)));
        for (index, reply) in rule.replies.iter().enumerate() {
            lines.push(format!("· 回复 {}：{}", index + 1, reply.brief(40)));
        }
        let cooldown = match rule.cooldown_seconds {
            Some(s) => format!("{s} 秒"),
            None => format!("{} 秒（全局）", self.config.cooldown_seconds),
        };
        let onoff = |on: bool| if on { "开" } else { "关" };
        lines.push(format!("· 冷却 {cooldown} · 折成合并转发：{} · 只在 @ 我时回：{}", onoff(rule.forward), onoff(rule.at_only)));
        lines.push(format!("· 回过 {} 次，最近{}", rule.hits, ago(rule.last_hit)));
        self.say(lines.join("\n")).await.map_err(|e| e.to_string())?;
        // 再把回复真发一遍：图、表情、合并转发长什么样，看一眼最准。
        if let Some(chosen) = rule.replies.first() {
            let name = self.ctx.as_message().map(|m| m.sender_name().to_string()).unwrap_or_default();
            let keyword = rule.keywords.first().cloned().unwrap_or_default();
            let asker = reply::Asker { user_id: &self.user, name: &name, group: self.group, keyword: &keyword };
            reply::send(self.ctx, self.writer, &rule, chosen, &asker, None).await.map_err(|e| format!("回复没发出去：{e}"))?;
        }
        // 两条都自己发过了，不必再回一句。
        Ok(String::new())
    }

    /// 回复的内容：指令后面跟着的，或者引用的那条消息。
    async fn content(&self, matched: &CommandMatch, tokens: usize) -> Result<(Reply, Vec<String>), String> {
        let bits = content_after(matched, tokens);
        let has_content = bits.iter().any(|b| !matches!(b, Bit::Text(t) if t.trim().is_empty()));
        let mut notes = Vec::new();
        let captured = if has_content {
            Some(reply::capture(&bits).await)
        } else if let Some(quoted) = matched.reply_id.as_deref().filter(|id| !id.is_empty()) {
            let original = api::get_msg(self.ctx, self.writer.clone(), quoted)
                .await
                .map_err(|e| format!("引用的那条消息取不回来：{e}"))?;
            let bits = reply::bits_of_message(&original.message);
            if let Some(Bit::Forward(res)) = bits.iter().find(|b| matches!(b, Bit::Forward(_))) {
                let (nodes, total) = reply::capture_forward(self.ctx, self.writer, res, quoted, self.group.unwrap_or_default())
                    .await
                    .ok_or("这条合并转发展开不了（可能太久了），换一条新的再试")?;
                notes.extend(describe(&total));
                return Ok((Reply::Forward { forward: nodes }, notes));
            }
            Some(reply::capture(&bits).await)
        } else {
            None
        };
        let Some(captured) = captured else {
            return Err("回复什么？把内容写在后面，或者引用一条消息（文字、图片、合并转发都行）再发这条指令".into());
        };
        notes.extend(describe(&captured));
        if captured.text.trim().is_empty() {
            return Err(format!("没有能存下来的内容{}", notes.first().map(|n| format!("：{n}")).unwrap_or_default()));
        }
        Ok((Reply::text(captured.text), notes))
    }

    async fn add(&self, matched: &CommandMatch, everywhere: bool) -> Said {
        let words: Vec<String> = command::extract_text_arg(&matched.args).split_whitespace().map(str::to_owned).collect();
        let keywords = words.get(1).map(|w| keywords_of(w)).unwrap_or_default();
        if keywords.is_empty() {
            return Err("触发词呢？比如「自动回复 添加 下载 安装包在群文件里～」；几个触发词用 | 隔开".into());
        }
        let (reply, notes) = self.content(matched, 2).await?;
        let groups: Vec<String> = match (everywhere, self.group) {
            (false, Some(group)) => vec![group.to_string()],
            _ => Vec::new(),
        };
        let mode = self.config.default_mode();
        // 同一处已经有一模一样的触发词：这句并进去当另一种说法。
        let twin = BOOK.read(|b| b.twin(&keywords, mode, &groups)).await;
        if let Some(id) = twin {
            let count = BOOK
                .with(|b| {
                    let mut rule = b.get(id).cloned().ok_or("刚才那条规则不见了")?;
                    rule.replies.push(reply);
                    b.replace(rule).map(|r| r.replies.len())
                })
                .await?;
            return Ok(with_notes(format!("「{}」已经有 #{id} 了，这句加成它的第 {count} 种说法（每次随机一种）。", keywords.join("」「")), &notes));
        }
        let rule = Rule {
            keywords,
            mode,
            replies: vec![reply],
            groups,
            author: self.user.clone(),
            ..Default::default()
        };
        let saved = BOOK.with(|b| b.insert(rule)).await?;
        info!(target: LOG_TARGET, "{} 加了 #{}{}", self.user, saved.id, saved.title());
        let max = self.config.contains_max_chars;
        let mut lines = vec![format!(
            "已添加 #{}：{}有人{}，就回这句。",
            saved.id,
            if saved.groups.is_empty() { "所有群里" } else { "本群" },
            match saved.mode {
                Mode::Exact => format!("说{}", saved.title()),
                Mode::Prefix => format!("以{}开头说话", saved.title()),
                _ if max > 0 => format!("在 {max} 字内说到{}", saved.title()),
                _ => format!("说到{}", saved.title()),
            }
        )];
        if saved.mode != Mode::Exact {
            lines.push(format!("怕误触就改成整句一样才回：自动回复 模式 {} 精确", saved.id));
        }
        lines.push(format!("再加一种说法：自动回复 加说法 {} <回复>", saved.id));
        Ok(with_notes(lines.join("\n"), &notes))
    }

    async fn variant(&self, matched: &CommandMatch, rest: &[&str], replace: bool) -> Said {
        let rule = self.find(rest.first()).await?;
        self.may_edit(&rule)?;
        let (reply, notes) = self.content(matched, 2).await?;
        let id = rule.id;
        let said = self
            .edit(rest.first(), |rule| {
                if replace {
                    rule.replies = vec![reply];
                    Ok(format!("#{id} 的回复换成了这一句。"))
                } else {
                    rule.replies.push(reply);
                    Ok(format!("#{id} 现在有 {} 种回复，每次随机一种。", rule.replies.len()))
                }
            })
            .await?;
        Ok(with_notes(said, &notes))
    }

    async fn keywords(&self, rest: &[&str], add: bool) -> Said {
        let words: Vec<String> = rest.iter().skip(1).flat_map(|w| keywords_of(w)).collect();
        if words.is_empty() {
            return Err(format!("{}哪些词？比如「自动回复 {} 3 安装包|下载地址」", if add { "加" } else { "删" }, if add { "加词" } else { "删词" }));
        }
        self.edit(rest.first(), |rule| {
            if add {
                for word in &words {
                    if !rule.keywords.iter().any(|k| book::fold(k) == book::fold(word)) {
                        rule.keywords.push(word.clone());
                    }
                }
            } else {
                let before = rule.keywords.len();
                rule.keywords.retain(|k| !words.iter().any(|w| book::fold(w) == book::fold(k)));
                if rule.keywords.len() == before {
                    return Err(format!("#{} 里没有这些触发词", rule.id));
                }
                if rule.keywords.is_empty() {
                    return Err(format!("#{} 只剩这些触发词，全删了就没法触发了；不要这条就「自动回复 删除 {}」", rule.id, rule.id));
                }
            }
            Ok(format!("#{} 现在的触发词：{}", rule.id, rule.keywords.join("、")))
        })
        .await
    }

    async fn mode(&self, rest: &[&str]) -> Said {
        let mode = rest.get(1).and_then(|w| Mode::parse(w)).ok_or("怎么比？精确、包含、开头或正则，比如「自动回复 模式 3 精确」")?;
        let max = self.config.contains_max_chars;
        self.edit(rest.first(), |rule| {
            rule.mode = mode;
            Ok(format!("#{} 改成{}：{}。", rule.id, mode.label(), mode.explain(max)))
        })
        .await
    }

    async fn flag(&self, rest: &[&str], what: &str, field: impl FnOnce(&mut Rule) -> &mut bool) -> Said {
        let want = rest.get(1).map(|w| matches!(*w, "开" | "on" | "是" | "1"));
        self.edit(rest.first(), |rule| {
            let id = rule.id;
            let slot = field(rule);
            *slot = want.unwrap_or(!*slot);
            Ok(format!("#{id}「{what}」{}。", if *slot { "开了" } else { "关了" }))
        })
        .await
    }

    async fn cooldown(&self, rest: &[&str]) -> Said {
        let value = match rest.get(1).copied() {
            Some("默认" | "全局" | "default") => None,
            Some(word) => Some(word.trim_end_matches('秒').parse::<u64>().map_err(|_| "冷却写秒数，或写「默认」照全局")?),
            None => return Err("冷却多少秒？比如「自动回复 冷却 3 60」，写「默认」就照全局".into()),
        };
        let global = self.config.cooldown_seconds;
        self.edit(rest.first(), |rule| {
            rule.cooldown_seconds = value;
            Ok(match value {
                Some(0) => format!("#{} 不冷却了：每次问到都回（同一个人与每分钟的上限照旧）。", rule.id),
                Some(s) => format!("#{} 在同一个群里至少隔 {s} 秒才回第二次。", rule.id),
                None => format!("#{} 的冷却照全局：{global} 秒。", rule.id),
            })
        })
        .await
    }

    async fn toggle(&self, rest: &[&str], on: bool) -> Said {
        let rule = self.find(rest.first()).await?;
        // 群管理员停用共用规则：只在本群停用，记进 exclude。
        if rule.groups.is_empty()
            && let Some(group) = self.group
            && !self.manager
        {
            let id = rule.id;
            let group = group.to_string();
            BOOK.with(|b| {
                let mut rule = b.get(id).cloned().ok_or("刚才那条规则不见了")?;
                rule.exclude.retain(|g| g != &group);
                if !on {
                    rule.exclude.push(group);
                }
                b.replace(rule)
            })
            .await?;
            return Ok(format!("#{id} 在本群{}（别的群照旧）。", if on { "恢复了" } else { "停用了" }));
        }
        self.edit(rest.first(), |rule| {
            rule.enabled = on;
            if on && let Some(group) = self.group {
                rule.exclude.retain(|g| g != group);
            }
            Ok(format!("#{} {}。", rule.id, if on { "启用了" } else { "停用了，规则还留着，「自动回复 启用」就回来" }))
        })
        .await
    }

    async fn delete(&self, rest: &[&str]) -> Said {
        let ids: Vec<u32> = rest.iter().filter_map(|w| number(w)).collect();
        if ids.is_empty() {
            return Err("删哪条？比如「自动回复 删除 3」，可以一次写好几个编号".into());
        }
        let mut gone = Vec::new();
        let mut problems = Vec::new();
        for id in ids {
            let word = id.to_string();
            match self.find(Some(&word.as_str())).await.and_then(|rule| self.may_edit(&rule).map(|()| rule)) {
                Ok(rule) => {
                    let group = self.group.unwrap_or_default();
                    if BOOK.with(|b| b.remove(rule.id, group)).await.is_some() {
                        info!(target: LOG_TARGET, "{} 删了 #{}{}", self.user, rule.id, rule.title());
                        gone.push(format!("#{}{}", rule.id, rule.title()));
                    }
                }
                Err(problem) => problems.push(problem),
            }
        }
        let mut lines = Vec::new();
        if !gone.is_empty() {
            lines.push(format!("删了 {}。删错了发「自动回复 恢复」找回来。", gone.join("、")));
        }
        lines.extend(problems);
        Ok(lines.join("\n"))
    }

    async fn restore(&self, rest: &[&str]) -> Said {
        let id = rest.first().and_then(|w| number(w));
        // 群管理员只能找回本群删的；机器人管理员在哪删的都能找回来。
        let group = if self.manager { None } else { self.group };
        let rule = BOOK
            .with(|b| b.restore(id, group))
            .await
            .ok_or_else(|| if id.is_some() { "回收站里没有这一条".to_string() } else { "没有刚删掉的规则可以恢复".to_string() })?;
        Ok(format!("#{}{} 回来了。", rule.id, rule.title()))
    }

    async fn test(&self, text: &str) -> String {
        let text = text.trim();
        if text.is_empty() {
            return "试哪句话？比如「自动回复 试 请问怎么下载」".into();
        }
        let max = self.config.contains_max_chars;
        // 私聊里管的是所有群共用的规则：按「一个没有单独规则的群」来试。
        let group = self.group.or(Some("*"));
        let found = BOOK.read(|b| book::best(&b.rules, &Asked::new(text, false, group), max).map(|(r, k)| (r.clone(), k))).await;
        let at = BOOK.read(|b| book::best(&b.rules, &Asked::new(text, true, group), max).map(|(r, k)| (r.clone(), k))).await;
        match (found, at) {
            (Some((rule, keyword)), _) => format!("会由 #{}{} 回复（{}，对上了「{keyword}」）。", rule.id, rule.title(), rule.mode.label()),
            (None, Some((rule, _))) => format!("直接说不会触发；@ 我再说，会由 #{}{} 回复。", rule.id, rule.title()),
            (None, None) => "没有规则会回这句。".into(),
        }
    }
}

fn describe(captured: &reply::Captured) -> Vec<String> {
    let mut notes = Vec::new();
    if !captured.skipped.is_empty() {
        notes.push(format!("{}存不了，只存了文字和图片", captured.skipped.join("、")));
    }
    if captured.lost_images > 0 {
        notes.push(format!("有 {} 张图没下载下来，没存进去", captured.lost_images));
    }
    notes
}

fn with_notes(text: String, notes: &[String]) -> String {
    if notes.is_empty() { text } else { format!("{text}\n（{}）", notes.join("；")) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use simd_json::json;

    fn matched(args: Vec<simd_json::OwnedValue>) -> CommandMatch {
        CommandMatch { args, reply_id: None, at_ids: Vec::new() }
    }

    #[test]
    fn numbers_and_keyword_lists() {
        assert_eq!(number("#3"), Some(3));
        assert_eq!(number("１２"), Some(12));
        assert_eq!(number("0"), None);
        assert_eq!(keywords_of("下载|安装包｜ 地址 "), vec!["下载", "安装包", "地址"]);
    }

    #[test]
    fn content_keeps_layout_and_images_after_the_words() {
        let args = vec![
            json!({"type": "text", "data": {"text": " 添加 下载 第一行\n第二行"}}),
            json!({"type": "image", "data": {"url": "https://example.com/a.png"}}),
            json!({"type": "text", "data": {"text": "结尾"}}),
        ];
        assert_eq!(
            content_after(&matched(args), 2),
            vec![Bit::Text("第一行\n第二行".into()), Bit::Image("https://example.com/a.png".into()), Bit::Text("结尾".into())]
        );
        let only = vec![json!({"type": "text", "data": {"text": "添加 下载"}})];
        assert!(content_after(&matched(only), 2).is_empty());
    }
}
