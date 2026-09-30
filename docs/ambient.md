# 群聊搭话

让内置 Agent 以固定人格参与指定群聊。默认关闭，只在 `[ambient].groups` 列出的群中生效。

搭话复用 `[oai]` 的模型接口、供应商、联网后端与图像模型，独立保存 `[ambient]` 配置及人格资料。群聊能力层与 Agent 房间共用；长期记忆、表情包库与群身份存于 `data/oai/chat/`，两种模式互相可见。

## 参与方式

每群保留最近 80 条消息。进程首次处理某群时，先用 `message.list` 翻回三小时内的记录垫到窗口前面；翻不到（非 satori-qq、接口失败）就从空窗口开始，不重试。

- 静默 6 秒（`debounce_seconds`）后算一阵；持续有新消息时最多等 40 秒（`max_pending_seconds`）。
- 之后按 `gate_interval_seconds`（默认 90 秒，实际在 0.6–1.5 倍间随机）看一次；正聊着（在关注、或三分钟内开过口）时缩到三分之一。
- 判定看「上次看群之后」的全部新消息（分隔线下，最多 36 条）加前情，按最能开口的一件估分，达到 `score_threshold`（默认 60）才调用发言模型。判定期间出现新消息时旧分数作废并重新评估。
- @、引用、戳一戳、喊名字（名片或 `aliases`）与 `/搭话` 不等下一眼。@ 或引用可跳过判定直接交发言模型，但人格仍可用 `[silent]` 不回复；名字只作提示，仍要过判定。发图不再立刻触发判定。
- `/搭话`（可通过 `summon_command` 配置）跳过判定，指令本身从上下文移除，只保留后续文字。`peak.mode = "pause"` 阻止主动判定与发言。

## 门槛

门槛由基础值与配置项计算，最终限制在 1–100：

| 来源 | 方向 | 默认 |
| --- | --- | --- |
| `score_threshold` | 基础门槛 | 60 |
| 最近十分钟已说几轮 | 每轮 +8，最多 +24 | `speech_penalty_per_turn` / `speech_penalty_cap` |
| 刚开过口（`cooldown_seconds` 内） | 按剩余时间比例加，满额 +18 | `cooldown_penalty` |
| 本小时说超目标轮数 | 每超一轮 +12，不封顶 | `max_per_hour` / `budget_penalty` |
| 当前精神头与兴致 | ±10 以内 | 见[记忆与状态](#记忆与状态) |
| 关注的话题被接住（`continuation`） | −5 | `focus_relief` |
| 可选的沉默补偿 | 负 | 默认关闭 |

发言频率、冷却时间与小时目标会提高判定门槛，但不会直接拦截回应。判定标 `help`（有人认真求助、群里还没人答好、而它答得上）时只按 `score_threshold` 再让 10 分，不吃加价。

**紧急突破。** 判定认定 `urgent`（有人求救或出事、有人正等它马上答、它自己刚说错的话正在误导人）且分数到 75 时，所有加价不算，直接交人格，日志记 `紧急突破`。每群每小时最多 `breakthrough_per_hour` 次（默认 3）。「救命」「在线等」「被骗」「报警」等本地关键词会让这一阵不等下一眼立即判定，玩笑中的「救命笑死」会被识别；高峰 `sleep` 档也会被这类字眼叫醒一次。`peak.mode = "pause"` 不突破。

**答疑。** 判定标了 `help` 或 `urgent`，或有人 @ 它问事时，这一轮按答疑来：思考强度换成 `help_thinking`（默认 `high`），需要时换 `help_model`，并在正文追加一段交代。

**群背景资料。** `data/ambient/groups/<群号>.md` 由管理员维护，一行一条，`#` 开头是注释，每轮现读，判定与发言都带着。写群里人默认都知道的事实。调高 `score_threshold` 或 `speech_penalty_per_turn` 减少主动发言；调低它们或提高 `focus_relief` 增加发言机会。

人设可输出内部状态行：

```text
[focus:{"users":[114514],"topic":"刚聊的游戏机制","seconds":180}]
```

最多关注三位当前记录里的群友，也可 `users:[]` 只跟话题。默认一次最多 180 秒，可随互动续期，`[focus:{"seconds":0}]` 主动结束。关注期间看群间隔缩到三分之一。关注不落库，重启清空。

## 计价时段

DeepSeek 官方价格按北京时间分高峰与空闲：周一至周五 9:00–12:00、14:00–18:00 为高峰，其余半价。价格规则可能变化，使用前核对服务商定价。

`[ambient.peak]` 只在判定模型或发言模型使用 `[oai.providers.deepseek]` 时生效；两者都用其他供应商时全天照常。替代模型须在 `[oai.providers]` 配置。

| `mode` | 高峰时段行为 |
| --- | --- |
| `swap` | 照常跑，只把判定与发言模型换成 `model`；`model` 留空等同 `normal` |
| `sleep`（默认） | 睡着，默认不主动判定或插话；`doze_gate_seconds` 非零时隔一段看一眼；被 @ / 引用 / 戳一戳时立刻醒 |
| `pause` | 一句话不说，零调用 |
| `normal` | 不理会时段，照常（高峰也用主模型） |

睡着时的自主接话由两条闸门控成本，两者只读本地时间戳，不产生费用：

| 字段 | 默认值 | 说明 |
| --- | --- | --- |
| `doze_gate_seconds` | `0` | 仅 `sleep`：0 表示高峰不主动判定；非零值夹到 30–3600 秒 |
| `doze_reply_limit` | `2` | 仅 `sleep`：每小时最多自主开口几次（不含被点名与搭话指令），0 表示不额外限制 |
| `model` | `""` | 高峰顶替主模型的便宜模型（`供应商/模型`），留空表示不换 |

`windows` 是 `HH:MM-HH:MM` 列表，按北京时间（UTC+8），可跨零点，不受本机时区影响。`weekdays` 是算作高峰的星期几（1=周一 … 7=周日，留空等于每天）。写坏的时段当作不存在。

```text
/ctl show ambient peak
/ctl set ambient peak.mode swap
/ctl set ambient peak.model provider/model
/ctl set ambient peak.mode pause
/ctl set ambient peak.windows ["09:00-12:00", "14:00-18:00"]
/ctl set ambient peak.doze_reply_limit 1
```

无论哪种模式，群消息照常进入滚动窗口，也照常更新熟人记忆，这两件事不产生费用。

## 身份

`identity.rs` 将平台资料加入判定与回复上下文：账号昵称与登录资料（`login.get`）、当前群名片 / 头衔 / 角色 / 入群时间（`guild.member.get`）、群名（`guild.get`）、头像描述（头像变化后才重新分析）。身份资料按群缓存 6 小时；查询失败只省略对应字段，旧头像描述在新分析失败时继续使用。头像描述按图片内容摘要保存到 `data/oai/chat/identity.json`，当前资料也出现在 `satori_context.identity` 中。

`preferred_name` 指定平时介绍自己用的称呼；`called_by_name` 用账号昵称、群名片与 `aliases` 匹配称呼，命中后仅在上下文加 `〔叫了你的名字〕`，不直接绕过判定；少于两个字的名字不参与匹配。缓存保存在本机，换设备后重新获取。

## 记忆与状态

三样都由本机算出，不额外调用模型。

- **熟人记忆**（`memory_enabled`，默认开）：每条群消息更新一份落盘档案，记录是谁、最近群名片、见过多少条、聊过几次、上次露面。人格用 `satori_memo` 写要记住的一句（如某人的 `address` 称呼、群里刚出现的梗），可改写和删除。发言与判定读到当前聊天出现的人（最多 8 张卡片）与最近旧事（最多 6 条）。旧事保留 45 天、每群最多 24 条；人最多记 120 位，超出先忘掉无印象、最久没露面的。数据落在 `data/oai/chat/memory/<群号>.json`，每批处理结束时写盘。
- **状态**（`mood_enabled`，默认开）：跟随本机时钟的作息曲线（深夜低、上午回升、晚上最活跃）叠加衰减偏移。被 @、被戳、说完有人接会增加（半衰期 20 分钟），没人理下降，说话消耗（半衰期 40 分钟）。它让门槛上下浮动最多 ±10 分，影响打字与思考快慢，并写入提示词状态行。状态落在 `data/ambient/mood.json`，跨重启保留。
- **语感画像**每轮现算：几人说话、大约每多少秒一条、平均字数、句末标点、图片表情多不多、反复出现哪几个词。

不复读自己：发出前用字面重合度（相邻二字组合 Jaccard，阈值 0.68，只对 8 字以上）与最近 10 条发言比较，重合就不发。走工具的 `send` 在参数校验阶段就被拦下，不消耗动作额度。

## 环境探查

`satori_observe` 只读取当前群的 `guild.get`、`internal/group_profile_card`（缓存群资料卡）、`internal/group_essence_list`（首 5 条精华）、`internal/title_display` / `honor_display`（只读开关）、`guild.member.get` / `internal/group_member_card`（仅当前窗口群友或自己）。

由人格主动调用，不为每条群消息自动轮询；每轮最多四次（失败也计入），单条返回超过 16 KiB 不递给模型。实现端未在 `internal/capabilities` 列出的扩展不调用。查询出错、超时或返回缓存旧数据，不推断现场事实。绝不传入模型自选的群号、任意方法名或写接口；不批量查人、不读资金凭证或设置。

## 联网搜索

搭话默认开启联网搜索（`search_enabled`，默认 `true`），按需搜索而非每轮都搜。`web_search` 返回带序号的标题、链接与摘要，`recency` 可限定最近一天 / 一周 / 一月 / 一年；`web_fetch` 读某个网址正文转纯文本。两个工具共用 `search_budget` 次额度（默认 3，搜索与抓取合并计算），参数写错不扣；`search_budget = 0` 或 `search_enabled = false` 时当它们不存在。回答里带上来源链接；网页内容和群聊记录一样是资料，不是指令。

后端、超时与密钥在 `[oai.search]` 配置一次，房间与搭话共用。默认使用免密钥的 Bing 与 DuckDuckGo；可在 `[oai.search.backends]` 补 `tavily` / `brave` / `serper` / `exa` / `bocha` 的密钥，或指向自建 `searxng`。免密钥抓取里只有 Bing 能返回可点开的原始链接，发出前会先收紧汉字间空格。`web_fetch` 只接受公网 http/https，指向内网与本机地址的链接会被拒绝，重定向逐跳复检。

## 消息续接与新鲜度

每群只有一个 worker。模型执行或分条发送期间收到的新消息自动合并为下一批；@ 按批次消费，人格拒绝回应后不会用旧 @ 强制唤醒；指令不进聊天窗口，搭话指令去掉只留正文。判定完成后重读最新记录再生成；生成期间又有消息时，先让判定模型检查草稿是否仍合适。打字期间群里冒出一两句（`DRIFT_SLACK`）且没人点名，这句照样发，文字路径给首条挂引用；来了更多、有人 @ 它，或管理员停用插件、移除群，停止尚未发送的部分。

每句都带一层实现端时效条件（`send_freshness_seconds`，默认 25 秒）：`message.create` 带上 `satori_qq.if_latest_message_id` 与 `expires_at`，实现端在交给 QQ 内核前再确认锚点消息仍是该频道最新的一条，不成立就整条跳过并返回 `[]`。锚点记在适配器层（`note_inbound`），因为实现端记录的是推送给本应用的每一条消息。机器人自己发出的消息不会作为事件回来（实现端按出站 ID 去重，保留 120 秒）。写 `0` 关闭这层条件。

`screenshot_on_suspicion = true`（默认关闭）时，仅在已开启搭话的群里，被 @ / 引用 / 叫昵称质疑「你是不是机器人」且消息仍在 90 秒内，会取本群最新至多三条消息经 satori-qq 0.29.5 的 `internal/chat_screenshot` 离屏渲染，返回的内部 PNG 资源连玩笑文案送回**原群**；`screenshot_cooldown_seconds = 21600` 控制每群间隔，最低一小时，进程重启后重新计时。聊天图是文字气泡、媒体占位，不是真实像素截图；此功能会把本群近期他人发言重发本群，群友不接受时不要开启。

## 平台动作与表达

QQ 官方机器人及 AI 助手的 Markdown 卡片正文能进入窗口；内联按钮只以 `按钮: [标签]` 显示，不代表可点击。QQ 分享卡、小程序卡显示可识别落地地址。QQ 内核钱包元素只读（`[红包]` 或 `[QQ钱包消息]`），不暴露凭证、账单号或支付链接，不能推断红包已领或已付款，也无自动收发。

人格通过本轮专属聊天界面工具参与群聊（schema 在 `src/plugins/oai/agent/tools.rs`，实现在 `src/plugins/oai/chat/`，进程内运行；内置 Agent 房间在群里用同一份）：

- `satori_context`：读取最新窗口、精确消息 ID、原始资源、当前平台能力与剩余额度，并带回 `register`（语感）、`state`（精神头）与 `remember`（记得的人与旧事）
- `satori_read`：查询窗口原消息或展开合并转发；语音消息带回 `voice_text`
- `satori_action`：执行结构化动作并返回回执
- `satori_draw`：调 `[oai]` 图像模型生成图片存 `ambient/media`，返回本地路径、改写标题与剩余额度，再用 `satori_action` 的 send + image 发出；不占平台写动作额度，受 `draw_budget` 限流
- `satori_music`：调 `[oai]` 的 Suno 接口写歌，一次出两个版本，音频与封面落 `ambient/media`，返回本地路径、歌词、时长与花费，再 send + audio（想带封面再加 image）；不占平台写动作额度，受 `music_budget` 限流
- `satori_video`：调 `[oai]` 视频接口生成视频存 `ambient/media`，返回本地路径与费用，再 send；受 `video_budget` 限制（默认 1），可能产生费用，设为 `0` 可关闭
- `satori_memo`：写长期记忆（`people` / `notes` / `forget_people` / `forget_notes`）；不占发送额度，受 `memo_budget` 限流，关闭 `memory_enabled` 时不注册

`capabilities.actions` 列客户端接受的动作，`extension_actions` 是实现端声明的扩展，`environment_lookups` 列当前可用探查类型，`unavailable` 标记平台已拒绝的动作。

支持精确引用、@群成员、QQ 小表情、图片与 GIF、复用入站图片与商城表情、文件、语音、视频、骰子、猜拳、戳一戳、资料卡点赞、消息表态与取消、撤回自己的消息、合并转发。转发已有消息保留真实作者，新整理内容署机器人自己。骰子与猜拳通过 QQ 魔法表情发送，无独立消息元素；资料卡点赞可能受账号或应用限制，拒绝后暂列 `unavailable`；网络超时结果未知，不自动重试。

## 表情包库

用 `satori_action` 的 `sticker` 参数（`message_id` 与 `index`）收藏当前消息中的项目，`note` 可加标签；取用时指定库内 `id`。

- 普通图片按内容摘要去重，存于 `data/oai/chat/stickers/`；超过 4 MiB 不保存，发送经 `upload.create` 上传。
- 按表情包样子发（`sub-type="1"`）在 satori-qq 0.27.1–0.29.6 必然失败（0.29.7 修复）；适配器去掉子类型当普通图片重发一次。
- 商城表情保存表情 ID、表情包 ID 与 `key`，无需下载图片；QQ 自定义表情保留表情子类型。
- 发言提示词含库内编号与最近三条入站图片 / 表情；判定模型不读整个库。
- 表情包库由 Agent 房间与搭话共用，不按群隔离；`sticker_max` 默认 120，超出按使用情况清理，设为 `0` 关闭收藏。
- 收藏在接收时写入，不依赖后续消息是否成功发送；删除文件后相应库记录清理。

每群消息窗口最多 80 条且重启清空；库内文件单独落盘，窗口清空后仍可继续用。

## 合并转发

合并转发在记录里只是占位符，正文单独取回：

- `native:<父消息 ID>` 走 QQ 内核，图片、表情包、逐条消息 ID 与时间戳都在；satori-qq 0.8.9.28 起接受 `channel_id`，父消息不在模块缓存也能定位。
- resId 走 `SsoRecvLongMsg` 伪造节点协议，NT 客户端发的图片整段消失，节点只剩空正文。

`satori_read({message_id, forward:true})` 先走内核，失败退 resId，并把「已退回旧协议」写进 `notes`。嵌套转发逐层展开，受 60 个节点、3 层深度约束，触顶置 `truncated`。返回 `transcript`（逐条：编号、发送者、时间、正文，嵌套缩进）、`nodes`（depth、message_id、user_id、原始 elements）与 `images`（转发内图片直链，最多 4 张）。入站记录保留媒体与引用参数，消息编号以字符串传给模型避免大整数精度丢失；戳一戳作为点名进入判定；撤回清掉原正文与媒体；表态事件无可靠操作者只记平台事件。

每轮动作即时执行，成功消息 ID 可引用或撤回，错误回工具与窗口。同一工具请求 ID 不重复执行；一旦调用动作工具，最终自然语言不再另发；最终 `[silent]` 只停后续输出，不能取消已执行动作。已进入 QQ 内核的操作不能靠取消本轮收回；网络超时结果未确认，不自动重发。每轮最多 `actions_budget` 个写操作（含失败尝试），发送仍受 `messages_budget` 限制。

本轮工作目录中的本地文件与 `data/ambient/media` 素材经 `upload.create` 上传，不能直接交 Termux 私有路径；当前实例通常为 `target/release/data/ambient/media`，context 每轮最多列 40 项；本地单文件上限 20 MiB，网络媒体必须是已核实的 http(s) 直链。不提供任意路径读取发送、跨群发言与 @全体；群管理动作默认一个群都不放行，要写 `management_groups`。`bash` / `read` 仍按原配置运行，不是操作系统沙箱；内置 Agent 房间授权在 `[oai.chat].management_groups`。

## 分段发送

优先保留模型设置的消息边界；单条长文本才由 `breath` 按标点、空格与长度切分：

- 切点：句末标点（`。！？…`，标点跟上一句）、汉字间空格、句中逗号（切开时逗号丢弃）
- 一次 `send` 多文本段按独立消息发送；@ 与表情随所属段；超剩余额度时多余段合并上一条并以换行分隔
- 带图片 / 文件 / 转发段切不动，整条发；带换行不切（模型自己排过的版）
- `[at:]` / `[img:]` 标记护住下标不被截断；at 段只带 QQ 号，后面紧跟着文字时补一个空格
- 切几段由长度决定（约 `split_chars` 一条，超过一条半才切），由剩余消息额度限定上限；片段不短于四字

工具发送与逐行文本共用分段规则；`split_chars = 0` 关闭自动切分，默认 60。未使用动作工具时，最终正文按逐行输出，支持 `[at:]`、`[face:]`、`[img:]`、`[reply]`、`[poke]`、`[dice]`、`[rps]`、`[wait:]`、`[silent]` 内联标记；模型耗时计入首条等待，后续按打字速度错开；收到新消息停止旧草稿继续发送。

## 提及与协议清洗

消息记录统一用 `[at:QQ号]` 标记提及；模型正文中写 `@QQ号`，发送前转平台提及。邮箱与带数字普通文本不转换；平台重复附加的 `@` 移除，昵称保留。

输出中的工具协议标记（包括 `[satori_action:…]` 与 `<parameter name="request">…</parameter>`）不作普通消息发送。解析器断句前只保留合法 `send` 的 `text`、`at` 与 `face`；无效 JSON、其他动作与不完整标记丢弃。已发错可用回执 `message_id` 调 `satori_action` 的 `recall` 撤回自己消息；分段发送的 `message_ids` 可逐条撤回。

完整操作协议见 `res/ambient/skills/satori-reply/SKILL.md`。

## 人设与样本

- `res/ambient/persona.md`：仓库默认人设，首次启动复制到 `data/ambient/persona.md`，之后升级不覆盖实例定制；判定与发言每轮读运行时文件。恢复默认需删运行时副本后重启。人设聚焦身份、兴趣、语气与接话方式。
- `res/ambient/voice.md`：从 `data/bot.db` 挑出的号主手打短句，按场景分组；发言随机挑 3–5 条贴提示词，跟眼前话题沾边的不挑，判定轮不贴。样本随二进制编译进，改样本需重新构建。`scripts/mine-voice.py` 按场景捞候选，人工过一遍后原样粘进 `voice.md`，再用 `--verify` 核一遍；每条必须是记录原文。
- `res/ambient/self.md`：号主档案（设备、作息、在追什么等），首次启动写到 `data/ambient/self.md` 之后不覆盖；仓库那份整篇注释，删掉或只留注释等于关掉。两份都在 `Scene::own` 里，只有发言轮取用。

## 切换模型

由 `ctl.admins` 中的全局管理员发送，私聊、已接入群聊与本机控制台均可；发言模型影响所有已启用搭话的群：

```text
/ctl set ambient reply_model deepseek/deepseek-flash
/ctl show ambient reply_model
/ctl set ambient gate_model deepseek/deepseek-flash
/ctl show ambient gate_model
```

供应商需在 `[oai.providers]` 配置。默认判定与发言模型为 `deepseek/deepseek-flash`，具体能力与价格以供应商为准。`swap` 只替换两处模型名。带供应商前缀按 `[oai.providers]` 选接口，不带前缀走 `[oai]` 默认接口。发言端思考强度默认 `low`。新消息到旧判定分作废，配置变更下一批生效，无需重启。只有模型名带 `deepseek/` 前缀才按峰谷作息，换成别家全天一个价。

## 配置

搭话设置位于 `config.toml` 的 `[ambient]`；Agent 房间设置位于 `[oai.chat]`，见[内置 Agent 房间](agent.md)。两者共用记忆与表情包库，调用预算独立。

判定模型处理新消息，发言模型仅在判定通过后调用。`gate_persona` 默认使用简短画像，减少高频判定提示词开销；更换为低价模型也可降成本。

| 字段 | 默认值 | 说明 |
| --- | --- | --- |
| `enabled` | `false` | 总开关 |
| `groups` | `[]` | 允许搭话的群号 |
| `management_groups` | `[]` | 开放人格群管理的群号，还须具有实际 QQ 权限；运行时移除立即阻止后续管理动作 |
| `gate_model` | `deepseek/deepseek-flash` | 判定模型；`供应商/模型` 按 `[oai.providers]` 取接口，不带前缀走 oai 默认接口 |
| `gate_persona` | 浓缩画像 | 判定读的兴趣画像，留空则回退完整人设 |
| `reply_model` | `deepseek/deepseek-flash` | 发言模型 |
| `help_thinking` | `high` | 答疑那一轮（`help` / `urgent`，或被 @ 问事）的思考强度；留空同 `thinking` |
| `help_model` | `""` | 答疑那一轮换用的发言模型；留空沿用 `reply_model` |
| `breakthrough_per_hour` | `3` | 每群每小时最多紧急突破几次；0 关闭 |
| `thinking` | `low` | 发言模型思考强度 |
| `temperature` | `1.0` | 发言模型采样温度；不写这一项交给接口默认值 |
| `tools` | `read,write,bash` | 本地工具白名单（bash / read / write / edit / glob / grep）；本轮按开关自动附加 satori 系列工具。写错的名字会被静默忽略 |
| `score_threshold` | `60` | 普通开口意愿门槛，调高更沉默。判定分档：好笑的图与能接的梗 40–60，有人求助且没人答好 65–80，冲着它来 85+ |
| `speech_penalty_per_turn` | `8` | 最近十分钟每说过一轮，门槛上调的分数；0 关闭 |
| `speech_penalty_cap` | `24` | 上面那笔加价的上限 |
| `focus_relief` | `5` | 关注中的话题被接住时门槛下调的分数 |
| `focus_max_seconds` | `180` | 每次关注期限上限，最多 600 秒；0 关闭 |
| `silence_relief_per_10min` | `0` | 可选：每沉默 10 分钟降低的门槛分数 |
| `silence_relief_cap` | `0` | 可选：门槛降低上限 |
| `context_turns` | `20` | 最近消息数，限制在 1–80 |
| `context_images` | `2` | 最新图片数；0 关闭 |
| `debounce_seconds` | `6` | 消息合并等待 |
| `max_pending_seconds` | `40` | 持续有消息时最多等待时间 |
| `gate_interval_seconds` | `90` | 没人叫它时隔多久扫一眼群（0.6–1.5 倍随机，正聊着时三分之一）；点名、喊名字与搭话指令不等；设 0 每阵都看 |
| `cooldown_seconds` | `150` | 两次主动开口之间的时间下限，窗口内按 `cooldown_penalty` 抬价；0 关闭这笔 |
| `cooldown_penalty` | `18` | 冷却窗口内门槛上调的满额，随时间线性退到窗口结束的 0 |
| `max_per_hour` | `5` | 每群每小时的目标发言轮数；超出后每轮再加 `budget_penalty`，0 关闭这笔 |
| `budget_penalty` | `12` | 超过每小时目标后每多一轮加的门槛分；一路抬到 100 为止 |
| `reply_on_mention` | `true` | 新 @ 或引用跳过筛选，但人格仍可沉默 |
| `aliases` | `[]` | 群友还会怎么叫它：名片之外的小名、简称；认出来只在记录上加一个〔叫了你的名字〕，不跳过判定 |
| `summon_command` | `/搭话` | 搭话指令：带它的群消息跳过判定直接交给人格，指令本身不进上下文；留空关闭 |
| `memory_enabled` | `true` | 熟人记忆：落盘记住群里的人与旧事，并注册 `satori_memo` |
| `mood_enabled` | `true` | 作息与互动驱动的内部状态：影响门槛、打字快慢与提示词里的状态行 |
| `memo_budget` | `3` | 每轮最多写几条记忆；0 关闭 `satori_memo` |
| `search_enabled` | `true` | 发言时是否联网；后端与房间共用 `[oai.search]` |
| `search_budget` | `3` | 每轮最多联网几次（搜索与抓取合并）；0 关闭 `web_search` / `web_fetch` |
| `peak.mode` | `sleep` | 计价高峰时段的行为：`swap` 照常跑只换模型，`sleep` 默认只在被叫时醒，`pause` 完全不出声，`normal` 不理会时段。两个模型都不走 DeepSeek 时整段让路 |
| `peak.windows` | `["09:00-12:00", "14:00-18:00"]` | 高峰时段，北京时间，可跨零点 |
| `peak.weekdays` | `[1,2,3,4,5]` | 算作高峰的星期几，1=周一；留空等于每天 |
| `peak.doze_gate_seconds` | `0` | 仅 `sleep`：两次主动判定之间的最短间隔（秒）；写 0 只有被点名才醒 |
| `peak.doze_reply_limit` | `2` | 仅 `sleep`：每小时最多自主开口几次；0 表示不额外限制 |
| `peak.model` | `""` | 高峰时段顶替主模型的便宜模型（`供应商/模型`）；留空不换，仍用 `gate_model` / `reply_model` |
| `messages_budget` | `2` | 一轮最多发送消息数，限制在 1–5 |
| `split_chars` | `60` | 一条消息大约多少字就该分段；超过约一条半时按断句切分，总数仍受 `messages_budget` 约束；0 关闭 |
| `actions_budget` | `6` | 一轮平台写动作总数，限制在 1–12，失败尝试也计入 |
| `draw_budget` | `2` | 每轮最多生成图片的张数；0 关闭绘图，绘图走 `[oai]` 配置的图像模型 |
| `music_budget` | `1` | 每轮最多生成几首歌；0 关闭 `satori_music`。一次请求生成两个版本，可能产生费用 |
| `video_budget` | `1` | 每轮最多生成几段视频；0 关闭 `satori_video`。视频服务可能收费，使用前核对上游定价 |
| `sticker_max` | `120` | 偷来的表情包最多留几张（`data/oai/chat/stickers/`，与房间共用一份）；满了先丢最没人用的，0 表示不攒 |
| `send_freshness_seconds` | `25` | 消息时效窗口：交给 QQ 之前群里又有人说话就整条不发；0 关闭 |
| `typing_cpm` | `150` | 打字速度，字/分钟 |
| `qq_typing` | `false` | 实验性 QQ JNI 输入指示；仅即将发送时触发，失败不影响消息 |
| `qq_mark_read` | `false` | 回复前每轮最多一次 QQ JNI 已读标记；失败不影响消息 |
| `voice_cpm` | `420` | 长句等效语音输入速度 |
| `think_seconds` | `3.0` | 思考等待，模型耗时计入其中 |
| `gate_timeout_seconds` | `45` | 单次判定超时 |
| `reply_timeout_seconds` | `240` | 人格回复含工具调用超时 |

判定遇网络抖动重试一次；其他错误本轮保持沉默。图片缩放转码，GIF 仅取首帧，结果缓存 30 分钟；处理时间计入模拟打字等待。超时会终止工具及其子进程。`bash` 可执行本机命令，无需使用时从白名单移除。

## 排障与边界

排查「始终不主动说话」：先确认目标群在 `groups`、入站群消息仍到达（手机后台冻结或 Satori 断连会让插件没有机会判断），再查 `Plugin/Ambient` 日志：

- `判定模型` / `换替补` 验证路由
- `翻了翻聊天记录` 是重启后补回的上下文
- `保持沉默（分数/门槛）` 是模型判分与节奏共同决定
- `交给人格决定` 之后的 `想了想，还是没说话` 是发言模型自行放弃
- `搭话失败` 看接口错误 / 超时
- `说（…）` 才是确认真正发出

高峰 `swap` 不降级能力，但模型拒收某张图时坏图会先丢弃；内容安全拒绝、网络抖动或群聊不断推进仍可能整批不发。`/搭话` 可验证发言链路，但会在真实群回话，排障别反复刷群。

```sh
cargo test ambient
```

带网络的 ignored 测试需显式设置 `ACUMEN_AMBIENT_LIVE_GATE_BASE` 与 `ACUMEN_AMBIENT_LIVE_GATE_KEY`，并提供可用的 OpenAI 兼容端点。

改人设、提示词或判定前，用真实群聊回放对比前后：

```sh
scripts/ambient-replay.py 818965288 '2026-09-26 10:00' '2026-09-26 10:55' > /tmp/r.json
ACUMEN_REPLAY=/tmp/r.json ACUMEN_AMBIENT_LIVE_DATA=$PWD/target/release/data/oai \
ACUMEN_REPLAY_SELF_DIR=$PWD/target/release/data/ambient \
ACUMEN_AMBIENT_LIVE_GATE_BASE=https://api.deepseek.com/v1 ACUMEN_AMBIENT_LIVE_GATE_KEY=… \
cargo test --release live_replay -- --ignored --nocapture
```

回放点默认取机器人当时开口的那几刻，只把那一刻之前的消息交给模型，时间平移到「刚刚」；`ACUMEN_REPLAY_PERSONA` 换人设、`ACUMEN_REPLAY_TEMPERATURE` 换温度。只打印，不发群消息。

QQ 服务端可能拒绝受限操作，以真实回执为准。支持发送现成语音与视频资源，并可读取 QQ 语音转写；不提供语音合成或视频理解。

滚动窗口、关注状态与动作回执在内存中，重启后清空；窗口第一次处理该群时从平台翻回三小时内的记录。跨重启保留的只有每群记忆（`memory/<群号>.json`）、状态（`mood.json`）与管理员维护的本体档案（`self.md`）；它们不是聊天记录，也不跨群关联。样本位于 `res/ambient/voice.md` 并编译进二进制，修改后需重新构建。
