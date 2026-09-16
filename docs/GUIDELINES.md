# 设计规范总纲

一个进程里跑着 22 个插件，共用一套指令前缀、一份注册表、一个出图入口。
用户在群里看到的是**一个机器人**，不是 22 个各写各的插件。

这份文档是入口：规范在哪、为什么这么定、边界到哪里。

- [一、三句话](#一三句话)
- [二、与 koishi 工作区的分野](#二与-koishi-工作区的分野)
- [三、四条底线](#三四条底线)
- [四、插件一致性的十条硬条目](#四插件一致性的十条硬条目)
- [五、文档地图](#五文档地图)
- [六、落地检查清单](#六落地检查清单)
- [七、这份规范怎么改](#七这份规范怎么改)

---

## 一、三句话

> **Built on Material. Behave Native. Think Human.**

三句有顺序，不是三个并列的口号。

**Built on Material —— 拿什么搭。** 地基是 Material 3 Expressive。
它给的不是一张皮，是一套可计算的令牌：色彩角色、字阶、形状、高度。
落在 `res/cards/m3e.css`（文件头写着它与 M3 的三处刻意偏离，别当笔误改回去）。

地基不是房子。令牌齐了，一张卡片上该说什么、什么时候说，一个字都还没定。

**Behave Native —— 怎么行事。** 这些插件不是 App，是群聊里的一位参与者。
群聊里没有返回键、没有模态框、没有悬停菜单，也**没有私有画布**——
机器人的每一次输出，全体群成员都被迫看见。照搬 App 的交互会做出一个在群里
格格不入的东西，长得再像 M3 也没用。落在 [`docs/INTERACTION.md`](INTERACTION.md)。

**Think Human —— 为谁着想。** 依据是 Apple HIG 的八条设计原则，放在最后，
因为它是前两句加起来的结果，不能直接去做。Apple 自己对 Delight 的解释是
“the sum of the consideration that you put into your product”——它从别的每一条里
长出来。译法见 INTERACTION.md 第一节，并贯穿三份文档。

**为什么地基抄 Google、判断抄 Apple？** 两者管的不是同一件事：M3 给的是
拿来就能出图的令牌；HIG 的八条原则不规定任何长相，只规定**拿不准的时候偏向谁**。

---

## 二、与 koishi 工作区的分野

同一套主张，两种落地。koishi 工作区（[koishi-plugin-guidelines](https://github.com/araea/koishi-plugin-guidelines)）
是十六个各自独立发布的插件；ayjx 是一个仓库里的二十二个插件。同源靠的东西不一样：

| | koishi 工作区 | ayjx |
| --- | --- | --- |
| 形态 | 16 个仓库，各自发版 | 1 个仓库，22 个插件同进程 |
| 「同源」靠什么 | 11 份逐字节相同的 `m3.ts`，`md5sum` 校验 | 一份注册表、一个出图入口、一条配置写路径 |
| 指令形态 | `插件名.动作` 两级为主，按 authority 分级 | `/` 前缀 + 中文指令，权限收在 `[ctl].admins` |
| 视觉例外 | 复刻类画面不套规范（棋盘、牌面） | 无例外，六张卡片全在系统内 |
| 文案例外 | 拟人题材台词属内容 | **搭话人格整套在外**（见下节第 3 条） |

**ayjx 抄的是那两句话的判断方式，不是那十六个插件的具体写法。**
那边的 `mcdle.猜`、`bull.来一局`、`session.prompt()` 在这里没有对应物；
这边的符号指令（`##`、`~`、`-#`）、引用驱动、卡片出图，那边也没有。
往下读时注意：凡是举到具体写法的例子，都是 ayjx 自己的代码。

---

## 三、四条底线

这四条不是建议，是架构上已经成立的约束。改插件时不许绕开，绕开就是又长出一套私有实现。

**1. 清单只读注册表。** 插件的展示名、分区、一句话说明、指令表都写在
`src/plugins/registry.rs` 一处。`/help`、`/help <插件>`、`/ctl list` 与用法卡
全部从那里读，不另存第二份。新增插件只改这一处。

**2. 出图只有两个入口。**
- HTML 卡片一律走 `render::web::shoot`：宽度、选择器、格式由调用方声明，
  量高、等字体、尺寸护栏、并发闸门、超时都在里面。
- CPU 图像工作（统计绘图、词云、GIF、切分）一律走 `render::worker.rs::run`，
  由它统一排队再 `spawn_blocking`。

不要各自 `spawn_blocking`，不要各自写量高与固定睡眠，不要退回
`requestAnimationFrame` 等布局。

**3. 文案规范只约束系统面。** `docs/CONTENT.md` 管指令回执、卡片、帮助、报错、
空态、推送。**搭话人格不在其内**——`res/ambient/persona.md` 与 `res/ambient/voice.md`
是刻意口语化的「群里一个普通成员」，拿 CONTENT.md 去规范它是错的。

**4. 写配置只有一条路。** `ctl::change`：取 `config_save_lock`、按插件真实的
serde 类型校验、先写盘再改内存。两个入口都汇到这里：聊天的 `/ctl`（按
`[ctl].admins` 判权）与 agent 房间的 `ayjx --ctl`（一次性凭据）。

---

## 四、插件一致性的十条硬条目

「统一规范」的骨架就是这十条。每条给的是**必须怎样**与**怎么查**——
审计现状用的也是这张表（见 [`docs/UNIFORMITY.md`](UNIFORMITY.md)）。

### 1. 注册表元数据

`display_name` 用中文，`section` 取自 `help::SECTIONS` 的五个代号
（`message` / `play` / `insight` / `system` / `misc`），`summary` 一句话说明它做什么，
`commands` 用 `cmds![("指令", "说明"), …]` 列全。

- 单元测试钉着：`every_plugin_has_a_summary`、`every_plugin_claims_a_known_section`、
  `grouping_loses_no_plugin`。
- `display_name` 与 `summary` 是**门面**，回答「这是什么」，不是「这能做什么」。

### 2. 指令命名

- 前缀 `/` 由配置拼上，注册表里只写指令本体。
- 符号指令（`/#`、`##`、`~名`、`-#`、`~#`、`~=`、`-*`、`画·<预设>`）自带写法，
  不再拼前缀（`help::needs_prefix`）。
- 同一个动作在全部插件里同名同义。已经统一的那几个：

  | 用 | 不用 |
  | --- | --- |
  | `开` / `关` | `启用` / `停用` / `打开` / `关闭`（作房间开关时） |
  | `排行榜` / `走势` | `榜` / `排名` / `榜单` / `趋势` |
  | `列表` | `清单` / `全部` / `ls` |

- 别名用 ` / ` 分隔写在同一条记录里；首个抬为主指令，其余降级为别名。
  别名是给打过一次旧写法的人用的，不再新增第二套叫法。

### 3. 指令描述

动词开头，句末无标点，不复述指令名本身。参数占位统一 `<必填>` / `[可选]`。

```text
✅ ("转链接 / 看链接", "将图片或视频转为直链（可引用消息）")
✅ ("裁剪 <行>x<列> / 切图", "如：裁剪 3x3")
❌ ("撤回", "撤回")                  ← 复述指令名
❌ ("help", "查看帮助。")             ← 句末标点
```

### 4. 配置类型与默认值

只写**一份** `Default` 实现，容器级加 `#[serde(default)]`，缺字段自动落默认值。

```rust
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Config { enabled: bool, /* … */ }

impl Default for Config { fn default() -> Self { Self { enabled: true, /* … */ } } }
```

**不要再写字段级 `#[serde(default = "fn")]`**——那是第二份默认值来源，
和 `Default` 实现迟早对不上。`validate_config` 用真实类型反序列化一次即可。

这条只约束**用户配置**（`config.toml` 里那棵树的类型）。模型工具调用的参数
schema（`ambient/actions.rs` 的 `Action` / `FileAction` 这类内部枚举）不适用：
它们不是配置、没有对应的 `Default` 实现，字段级默认值是唯一的一份。

### 5. 配置字段说明

每个用户可改的字段上方写 `///` 注释，说清**这一项做什么**与**取值含义**，
陈述句、句末带句号，不以「是否」开头。

字段上的 `///` 是配置说明的**权威来源**，另有两处读者必须与它对上：

- `config.example.toml`（新用户唯一能看到的一份完整配置）
- `/ctl show` 的输出——它只打**当前值**的快照，不带注释，所以取值含义
  只能在别处解释清楚

**加一个配置项 = 同时动三处**：字段与它的 `///`、`Default`、`config.example.toml`。
漏了哪一处，用户就会在某一处看到一个没人解释过的数字。

### 6. 日志 target

统一 `Plugin/<名字>`，名字按**单词边界**大写，一个插件一个 target。

```text
✅ Plugin/WordCloud   Plugin/VideoParse   Plugin/AiNews
✅ Plugin/WebShot     Plugin/GroupTitle   Plugin/ImageSplit
❌ "Plugin"           ← 无插件名，日志里看不出是谁
❌ Plugin/Wordcloud   ← 单词边界没切开
❌ Plugin/ImageSplitter ← 比注册名多了一个词
```

子模块可以在后面加一级（`Plugin/OAI/Search`）。框架自身的 target
（`Chat`、`System`、`Bot`、`Database`）不属此列。

### 7. 出图开关

会出卡片的插件，配置项命名统一：

| 键 | 含义 |
| --- | --- |
| `image_enabled` | 是否出图；关掉或渲染失败退回纯文本 |
| `image_scale` | 渲染倍率，限制在 1—4 倍 |
| `theme` / `card_theme` | 阅读主题：`auto` / `light` / `dark` |

「成段的报告类卡片」用 `theme`，「长清单类卡片」用 `card_theme`——
两者都按北京时间在日读与夜读之间切。**关图之后那一份纯文本必须信息等价**，
不是「图挂了给你一句话」。

**键名统一，默认值可以不同**，因为版心宽度不一样：720 CSS px 的卡片默认 3 倍，
`oai` 的 520 px 回复卡默认 2 倍（1040px 位图，最省体积又不糊）。
默认值写在该插件的 `Default` 里，不要为了「看起来一致」把倍率也抄过去。

### 8. 短反馈不出图

一句话的纠错、开关确认、报错都走纯文本（`ctl` 的 `Output::card` 就是这条分界）。
出图慢，在群里还多一条图片，也不方便复制文字。

**只有「要被读、要被翻回去看」的输出才出图**：手册、状态清单、配置与差异、
画像、长报告。判据是「读者会不会想把它留着」。

### 9. 限流与冷却

统一两个后缀，不要自造第三个：

| 后缀 | 含义 | 例 |
| --- | --- | --- |
| `*_seconds` | 秒数（冷却、超时、间隔） | `cooldown_seconds`、`reply_timeout_seconds`、`max_pending_seconds` |
| `*_budget` | **每轮**可用次数，0 即关闭 | `lookup_budget`、`search_budget`、`draw_budget`、`music_budget` |
| `*_max_per_hour` / `*_max_per_day` | 每小时／每日上限 | `realtime_max_per_hour` |

「上限」不写成 `*_limit`——它的量纲看不出来是每小时还是总共。
`*_budget` 与 `*_max_per_hour` 的分工是：前者管**一轮对话里能花几次**，
后者管**一段时间里能触发几次**，两个都要有上限时两边都写。

**额度用完时说清什么时候回来**，并给一条此刻仍然能做的事。
死路一条的限流是最伤人的那种。

### 10. 权限门槛

| 操作 | 门槛 |
| --- | --- |
| 改插件开关与配置 | `[ctl].admins` |
| 手动 `/restart` | 仅管理员，且 `allow_manual_restart = true` |
| 群管理类动作（禁言、踢人、改名） | `[ambient].management_groups` 显式列出的群 |
| 设置群头衔 | 机器人须是群主 |

**破坏性操作必须有门槛**，而门槛要能说明自己是谁：被拒时回的是
「仅限 ctl.admins 中的全局管理员」这种能照做的句子，不是「无权限」。

---

## 五、文档地图

| 文档 | 管什么 | 什么时候读 |
| --- | --- | --- |
| **本文** | 总纲：三句话、四条底线、十条硬条目 | 动手改任何插件之前 |
| [`docs/INTERACTION.md`](INTERACTION.md) | 交互：八条原则的译法、群聊原生模式、节奏、自适应、信息架构 | 改指令、改流程、加等待或追问时 |
| [`docs/CONTENT.md`](CONTENT.md) | 文案：声音、语气、标点、状态词表、术语表、图标 | 写任何一句用户可见的话时 |
| `res/cards/m3e.css` 文件头 | 视觉：字阶、形状、高度、配色角色、组件基元 | 改卡片版式或图上文字时 |
| [`docs/ARCHITECTURE.md`](ARCHITECTURE.md) | 架构：事件流、插件系统、出图与渲染、新增插件的步骤 | 想知道「这东西怎么接进来的」 |
| [`docs/UNIFORMITY.md`](UNIFORMITY.md) | 统一度审计：2026-09-16 的偏离清单与位置 | 逐插件打磨时当待办清单看 |
| [`docs/CONTROL.md`](CONTROL.md) | 控制通道与部署 | 改配置、排期、上线 |
| [`docs/ambient.md`](ambient.md) / [`portrait.md`](portrait.md) / [`video_parse.md`](video_parse.md) / [`agent.md`](agent.md) | 单个复杂插件的用法与实现 | 动那一个插件时 |

**两层不能混**：系统层（`m3e.css`）说「是什么」，版式层（`res/cards/reading.css`
与各插件里那份 `const CSS`）只说「摆在哪儿」。**版式层里不许出现色值、字号、
圆角、阴影的字面量**，一律 `var(--md-*)` 取令牌——写了就是又长出一套私有的视觉语言。

---

## 六、落地检查清单

改完一个插件，逐条过。前四条是硬的（有测试或架构兜着），后六条要自己看。

- [ ] 注册表里那一条写全了：`display_name` / `section` / `summary` / `commands`
- [ ] 走 `cargo test`：注册表元数据、示例配置一致性与 `satori_compat_tests` 都是绿的
- [ ] 加了配置项 → `config.example.toml` 同步了（`the_example_config_lists_exactly_the_default_keys` 会拦）
- [ ] 出图走的是 `render::web::shoot` 或 `render::worker::run`，没有自己排并发
- [ ] 配置只改 `ctl` 那一条路径，没有绕过 `config_save_lock`
- [ ] 指令名查过第四节第 2 条那张表，没有为同一个动作造第二个词
- [ ] 指令描述动词开头、无句末标点、参数占位写成 `<必填>` / `[可选]`
- [ ] 配置只有一份 `Default`，没有字段级 `serde(default = "fn")`
- [ ] 每个配置字段上方有 `///` 说明，陈述句、带句号
- [ ] 日志 target 是 `Plugin/<单词边界大写的注册名>`
- [ ] 用户可见的每一句话过了一遍 `CONTENT.md` 的检查清单
- [ ] 文案里的指令带的是**当前前缀**，不是写死的 `/`
- [ ] 没有为兼容保留的旧名字、旧配置键、旧分支或注释掉的死代码
- [ ] 改过版式 → `bash scripts/review-cards.sh`，并且**自己看了图**

---

## 七、这份规范怎么改

规范不是刻在石头上的，但改之前先想清楚：**用户会不会因此对不上号。**

**先说不留什么：这个仓库不留兼容包袱。** 没有外部用户，没有发版约定，
没有「别人还在用旧写法」这回事。所以：

- 改名不留别名，旧名字直接删（`help` 的 `TRIGGERS` 就是新名单）。
- 改配置键名直接改，不写「读旧名自动迁移」的兼容分支。
- 统一术语时全仓一次改完（`rg` 一遍旧词），**不允许新旧并存**。
- 已经不用的机制整块删掉，不要留成注释或 dead code。

留一个旧名字的代价不是多几行代码，是让后来的人无法判断「这两个名字哪个是对的」。

其余三条：

- 加一条硬条目 → 先问它能不能被测试或架构钉住。钉不住的写在 INTERACTION.md 里，
  不要放进第四节冒充硬条目。
- 加一个图标或状态词 → 先看 `CONTENT.md` 那张表里有没有现成能用的；
  真要加，同时改表、改已有文案。
- 改动落地时同步三处：文档、`res/cards/m3e.css` 里对应的组件、各插件的代码。

规范的依据来自 Material 3 Expressive（令牌）、Apple HIG 八条设计原则（判断依据）、
Nielsen Norman Group 的启发式（错误预防、认得出优于记得住）与 WCAG 2.2（对比度、
颜色不作唯一通道）。四份都不是照搬——它们为桌面与网页而写，这里是逐条译成群聊里的动作。
