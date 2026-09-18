# 设计规范总纲

一个进程里跑着 23 个插件，共用一套指令前缀、一份注册表、一个出图入口。用户在群里看到的是一个机器人。插件各自为政，用户看到的就是二十三个机器人。

这份文档是入口：规范在哪，为什么这么定，边界到哪里。

- [一、三句话](#一三句话)
- [二、与 koishi 工作区的分野](#二与-koishi-工作区的分野)
- [三、五条底线](#三五条底线)
- [四、插件一致性的十条硬条目](#四插件一致性的十条硬条目)
- [五、文档地图](#五文档地图)
- [六、落地检查清单](#六落地检查清单)
- [七、这份规范怎么改](#七这份规范怎么改)

---

## 一、三句话

> Built on Material. Behave Native. Think Human.

三句有先后，是[设计系统](DESIGN_SYSTEM.md)那四句主张在本仓库的短写法。

Built on Material 管令牌。M3 Expressive 提供一套可计算的取值：色彩角色、字阶、形状、高度，落在 `res/cards/m3e.css`。文件头写着这套系统与 M3 的三处刻意偏离。令牌齐备之后，一张卡片上该说什么、什么时候说，还没有定义。

Behave Native 管行为。这些插件不是 App，是群聊里的一个参与者。群聊里没有返回键、没有模态框、没有悬停菜单，也没有私有画布，机器人的每一次输出全体群成员都看得见。照搬 App 的交互，做出来的东西在群里不成立。规范在 [`docs/INTERACTION.md`](INTERACTION.md)。

Think Human 管取舍。依据是 Apple HIG，分两层：体验要达到什么质量（九条），以及拿不准时偏向谁（Apple 2026 年 6 月起 HIG 开头的八条原则）。两层都列在[设计系统的体验原则一节](DESIGN_SYSTEM.md#四体验原则--apple-hig)，八条的逐条译法见 INTERACTION.md 第一节。

M3 与 HIG 管的不是同一件事。M3 给的是可以直接出图的令牌，HIG 不规定长相，规定的是质量与偏向。冲突时按[决策优先级](DESIGN_SYSTEM.md#十四决策优先级)排。

---

## 二、与 koishi 工作区的分野

同一套主张，两种落地。koishi 工作区（[koishi-plugin-guidelines](https://github.com/araea/koishi-plugin-guidelines)）是十六个各自独立发布的插件，ayjx 是一个仓库里的二十三个插件。

| 项 | koishi 工作区 | ayjx |
| --- | --- | --- |
| 形态 | 16 个仓库，各自发版 | 1 个仓库，23 个插件同进程 |
| 设计系统 | 四份文档，规范仓库一处 | 同骨架的一份[设计系统](DESIGN_SYSTEM.md)加本仓库的总纲与硬条目 |
| 保证同源的手段 | 11 份逐字节相同的 `m3.ts`，`md5sum` 校验 | 一份注册表、一个出图入口、一条配置写路径 |
| 指令形态 | `插件名.动作` 两级为主，按 authority 分级 | `/` 前缀加中文指令，权限收在 `[ctl].admins` |
| 视觉例外 | 复刻类画面不套规范（棋盘、牌面） | 无例外，五张卡片全在系统内 |
| 文案例外 | 拟人题材台词属于内容 | 搭话人格整套在外，见下节第 3 条 |

主张、哲学、体验原则、十类令牌、决策优先级两边是同一份，形态按各自的媒介落地——**Consistency ≠ identical implementation**。差异见[设计系统的平台适配一节](DESIGN_SYSTEM.md#十三平台适配)。

ayjx 采用那两份文档的判断方式，不采用那十六个插件的具体写法。那边的 `mcdle.猜`、`bull.来一局`、`session.prompt()` 在这里没有对应物，这边的符号指令、引用驱动、卡片出图，那边也没有。

---

## 三、五条底线

这五条是架构上已经成立的约束。改插件时绕开它们，会多出一套私有实现。

**1. 清单只读注册表。** 插件的展示名、分区、一句话说明与指令表都写在 `src/plugins/registry.rs` 一处，`/help`、`/help <插件>`、`/ctl list` 与用法卡都从那里读。新增插件只改这一处。

**2. 出图只有两个入口。** HTML 卡片一律走 `render::web::shoot`，宽度、选择器、格式由调用方声明，量高、等字体、尺寸护栏、并发闸门与超时在里面；CPU 图像工作（统计绘图、词云、GIF、切分）一律走 `render::worker.rs::run`，由它统一排队再 `spawn_blocking`。不要各自 `spawn_blocking`，不要各自写量高与固定睡眠，不要用 `requestAnimationFrame` 等布局。

**3. 文案规范只约束系统面。** `docs/CONTENT.md` 管指令回执、卡片、帮助、报错、空态与推送。搭话人格不在其内：`res/ambient/persona.md` 与 `res/ambient/voice.md` 是口语化的「群里一个普通成员」，用 CONTENT.md 规范它不成立。

**4. 写配置只有一条路。** `ctl::change` 取 `config_save_lock`，按插件真实的 serde 类型校验，先写盘再改内存。聊天的 `/ctl`（按 `[ctl].admins` 判权）与 agent 房间的 `ayjx --ctl`（一次性凭据）都汇到这里。本机控制台（`src/plugins/console/`）是第三个触发器，也汇到这里。凭据换成一道回环口令，校验与保存的步骤一样。

**5. 界面是核心自己发的网页。** 图形界面是 `[console]` 那个插件在回环地址上发的一张网页（`res/console/`）——`./bot ui` 打开它，手机上的浏览器打开的也是它。没有单独的客户端，也没有第二份状态：页面上显示的每一格都是进程里此刻的真实值，改的每一处都汇到 `ctl::change`。要改界面，改的是那个插件与那份网页。终端模式与它无关：`--no-ui` 或者 `[console] enabled = false` 之后，指令、排期与推送一切照旧。

---

## 四、插件一致性的十条硬条目

这十条管一致性。审计现状用的是同一张表，见 [`docs/UNIFORMITY.md`](UNIFORMITY.md)。

### 1. 注册表元数据

`display_name` 用中文，`section` 取自 `help::SECTIONS` 的五个代号（`message`、`play`、`insight`、`system`、`misc`），`summary` 一句话说明它做什么，`commands` 用 `cmds![("指令", "说明"), …]` 列全。三条单元测试钉着：`every_plugin_has_a_summary`、`every_plugin_claims_a_known_section`、`grouping_loses_no_plugin`。`display_name` 与 `summary` 回答「这是什么」，不回答「这能做什么」。

### 2. 指令命名

前缀 `/` 由配置拼上，注册表里只写指令本体；符号指令（`/#`、`##`、`~名`、`-#`、`~#`、`~=`、`-*`、`画·<预设>`）自带写法，不再拼前缀（`help::needs_prefix`）。

开关类的词只有两组：oai 的房间后缀用 `房间?开` / `房间?关`，ai_news 的推送目标用 `ai推送开启` / `ai推送关闭`。写进句子里的状态词也按这两组分开：插件说「已启用 / 已停用」，推送目标说「已开启推送 / 未开启推送」。后缀指令只能给一个字，所以是 `?开`；独立指令是动词，`ai推送开` 念着不自然，所以写 `开启`。

别名用 ` / ` 分隔写在同一条记录里，首个抬为主指令，其余降级为别名。不为同一个动作造第二套叫法：留着 `x` 又加 `x.了`，帮助里会同时出现两个名字。

代码吃得下的写法，注册表里要写全。`commands` 是面向用户的指令表，用户在 `/help` 里看不到的写法等于不存在。注册表不要求与代码里的匹配表逐字对应（有的插件按组匹配，有的走正则），但不许少写。

### 3. 指令描述

动词开头，句末无标点，不复述指令名本身。参数占位统一 `<必填>` / `[可选]`。

```text
✅ ("转链接 / 看链接", "将图片或视频转为直链（可引用消息）")
✅ ("裁剪 <行>x<列> / 切图", "如：裁剪 3x3")
❌ ("撤回", "撤回")                  ← 复述指令名
❌ ("help", "查看帮助。")             ← 句末标点
```

### 4. 配置类型与默认值

只写一份 `Default` 实现，容器级加 `#[serde(default)]`，缺字段落默认值。

```rust
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Config { enabled: bool, /* … */ }

impl Default for Config { fn default() -> Self { Self { enabled: true, /* … */ } } }
```

不要写字段级 `#[serde(default = "fn")]`，那是第二份默认值来源，与 `Default` 实现迟早对不上。`validate_config` 用真实类型反序列化一次即可。

这条只管用户配置，也就是 `config.toml` 里那棵树的类型。模型工具调用的参数 schema（`oai/chat/actions.rs` 的 `Action`、`FileAction` 这类内部枚举）不是配置，没有对应的 `Default` 实现，字段级默认值在那里是唯一的一份。

### 5. 配置字段说明

每个用户可改的字段上方写 `///` 注释，说清这一项做什么与取值含义，陈述句，句末带句号，不以「是否」开头。

字段上的 `///` 是配置说明的权威来源。读者还有两处：`config.example.toml`，新用户唯一能看到的一份完整配置；以及 `/ctl show` 的输出，它只打当前值的快照，不带注释。

加一个配置项要同时动三处：字段与它的 `///`、`Default`、`config.example.toml`。

两个例外。`enabled` 的含义在 23 个插件里完全一样，写 23 遍不增加信息量。不在 `config.toml` 里的持久化结构（`oai/types.rs` 的 `Config` 是 `data/oai/config.json` 的形状，由 ctl 与房间指令维护）该有说明，理由是可维护性。

### 6. 日志 target

统一 `Plugin/<名字>`，名字按单词边界大写，一个插件一个 target，子模块可以在后面加一级（`Plugin/OAI/Search`）。

```text
✅ Plugin/WordCloud   Plugin/VideoParse   Plugin/AiNews
✅ Plugin/WebShot     Plugin/GroupTitle   Plugin/ImageSplit
❌ "Plugin"           ← 无插件名，日志里看不出是谁
❌ Plugin/Wordcloud   ← 单词边界没切开
❌ Plugin/ImageSplitter ← 比注册名多了一个词
```

框架自身的 target（`Chat`、`System`、`Bot`、`Database`）不属此列。

### 7. 出图开关

会出卡片的插件，配置项命名统一：

| 键 | 含义 |
| --- | --- |
| `image_enabled` | 是否出图；关掉或渲染失败退回纯文本 |
| `image_scale` | 渲染倍率，限制在 1 到 4 倍 |
| `theme` / `card_theme` | 阅读主题：`auto` / `light` / `dark` |

成段的报告类卡片用 `theme`，长清单类卡片用 `card_theme`，两者都按北京时间在日读与夜读之间切。关图之后那一份纯文本要信息等价，不能只给一句话。

键名统一，默认值可以不同，因为版心宽度不一样：720 CSS px 的卡片默认 3 倍，`oai` 的 520 px 回复卡默认 2 倍。默认值写在该插件的 `Default` 里。

### 8. 短反馈不出图

一句话的纠错、开关确认与报错走纯文本，`ctl` 的 `Output::card` 就是这条分界。只有需要被读、被翻回去看的输出才出图：手册、状态清单、配置与差异、画像、长报告。判据是读者会不会想把它留着。

### 9. 限流与冷却

统一三个后缀：`*_seconds` 记秒数（冷却、超时、间隔），`*_budget` 记每轮可用次数（0 即关闭），`*_max_per_hour` 与 `*_max_per_day` 记每小时或每日上限。上限不写成 `*_limit`，那个后缀看不出量纲。一条消息切几条这类每轮的上限也归 `*_budget`（`messages_budget`、`actions_budget`）。

`*_budget` 管一轮对话里能花几次，`*_max_per_hour` 管一段时间里能触发几次，两个都要有上限时两边都写。额度用完时说清什么时候恢复，并给一条此刻仍然能做的事。

### 10. 权限门槛

| 操作 | 门槛 |
| --- | --- |
| 改插件开关与配置 | `[ctl].admins` |
| 手动 `/restart` | 仅管理员，且 `allow_manual_restart = true` |
| 群管理类动作（禁言、踢人、改名） | `[ambient].management_groups` 显式列出的群；内置 agent 房间在群里动手走 `[oai.chat].management_groups`，是另一份名单 |
| 设置群头衔 | 机器人须是群主 |

破坏性操作要有门槛，门槛要能说明自己是谁：被拒时回的是「仅限 ctl.admins 中的全局管理员」这种能照做的句子，不是「无权限」。

---

## 五、文档地图

| 文档 | 管什么 | 什么时候读 |
| --- | --- | --- |
| 本文 | 总纲：三句话、五条底线、十条硬条目 | 动手改任何插件之前 |
| [`docs/DESIGN_SYSTEM.md`](DESIGN_SYSTEM.md) | 设计系统本身：主张、哲学、视觉语言、体验原则、十类令牌、决策优先级。与 [koishi 工作区](https://github.com/araea/koishi-plugin-guidelines)同骨架 | 拿不准一处该怎么做时 |
| [`docs/INTERACTION.md`](INTERACTION.md) | 交互：八条原则的译法、群聊原生模式、节奏、自适应、信息架构 | 改指令、改流程、加等待或追问时 |
| [`docs/CONTENT.md`](CONTENT.md) | 文案：声音、语气、标点、状态词表、术语表、图标 | 写任何一句用户可见的话时 |
| `res/cards/m3e.css` 文件头 | 视觉：字阶、形状、高度、配色角色、组件基元（五张卡片图 + 控制台那一套方案） | 改卡片版式或图上文字时 |
| `res/console/app.css` 文件头 | 视觉：控制台的版式层与交互基元（按钮、开关、输入、导航、日志面板） | 改控制台界面时 |
| [`docs/ARCHITECTURE.md`](ARCHITECTURE.md) | 架构：事件流、插件系统、出图与渲染、新增插件的步骤 | 查一个功能是怎么接进来的 |
| [`docs/UNIFORMITY.md`](UNIFORMITY.md) | 统一度审计：偏离清单与位置 | 逐插件打磨时当待办清单看 |
| [`docs/CONTROL.md`](CONTROL.md) | 控制通道与部署 | 改配置、排期、上线 |
| [`docs/ambient.md`](ambient.md)、[`portrait.md`](portrait.md)、[`video_parse.md`](video_parse.md)、[`agent.md`](agent.md) | 单个复杂插件的用法与实现 | 动那一个插件时 |

两份文档分工不重。DESIGN_SYSTEM.md 是与 koishi 工作区共用的那一层，说明这套系统是什么、按什么排序；本文是 ayjx 特有的落地约束，说明这个仓库里哪些东西已经被架构钉住。

两层不能混。系统层 `m3e.css` 说「是什么」，版式层（`res/cards/reading.css` 与各插件里那份 `const CSS`）只说「摆在哪儿」，版式层里不出现色值、字号、圆角、阴影的字面量，一律 `var(--md-*)` 取令牌。

控制台按同一条分工多分出一类，理由写在 `res/console/app.css` 的文件头：令牌仍在 `m3e.css`（它多了一套 `scheme-console` 方案，把浅深两版写全），`app.css` 是它的版式层，另外**多担一件卡片不需要的事：交互基元**。卡片是静态位图，没有悬停与按压，所以 M3 的状态层在 `m3e.css` 里用不上。界面要响应，按钮、开关、输入、日志面板得有真反馈，这些写在 `app.css`。`app.css` 里允许出现结构尺寸（外壳多高、版心多宽）与动效时长曲线，但统一收在开头那组 `--zy-*` 里。色值、字号、圆角、阴影仍然一律取令牌，`src/plugins/console/assets.rs` 有一条测试盯着。

界面这一层还有两条自己的规矩，理由是上一版踩过：

- **换页一律用链接。** 导航与「进详情」都写成 `<a href="#/…">`，换页只靠 `hashchange`；脚本里的事件委派挂在 `document` 上。上一版把点击委派在 `#view` 上，而底部导航是它的兄弟节点，于是整条导航点不动。
- **行里不套按钮。** 行身是铺满整行的一层链接，开关压在上面；链接里套按钮是无效标记，浏览器会连开两件事（换页 + 开关）。

界面还是那份可安装的应用（`res/console/manifest.webmanifest`）：三档宽度对应底栏、导航轨与抽屉，装到桌面之后没有地址栏。改版式时三种宽度都要过一遍 `bash scripts/review-console.sh`；写操作与压力回归运行 `node tests/console.cjs`，使用隔离数据。性能边界与本轮审计见 [WEBUI_AUDIT.md](WEBUI_AUDIT.md)。

---

## 六、落地检查清单

改完一个插件逐条过。

- [ ] 注册表那一条写全了：`display_name` / `section` / `summary` / `commands`
- [ ] `cargo test` 通过，注册表元数据、示例配置一致性与 `satori_compat_tests` 都是绿的
- [ ] 加了配置项，`config.example.toml` 同步了
- [ ] 出图走的是 `render::web::shoot` 或 `render::worker::run`
- [ ] 配置只改 `ctl` 那一条路径
- [ ] 指令名查过第四节第 2 条；指令描述动词开头、无句末标点
- [ ] 配置只有一份 `Default`，每个字段上方有 `///` 说明
- [ ] 日志 target 是 `Plugin/<单词边界大写的注册名>`
- [ ] 用户可见的每一句话过了一遍 `CONTENT.md` 的检查清单，空态用 📭 不是 ❌
- [ ] 文案里的指令带的是当前前缀
- [ ] 没有为兼容保留的旧名字、旧配置键、旧分支或注释掉的死代码
- [ ] 拿不准的地方过了一遍[设计系统的检查清单](DESIGN_SYSTEM.md#十六检查清单)
- [ ] 改过版式，跑过 `bash scripts/review-cards.sh`，并且看过图

---

## 七、这份规范怎么改

改之前先确定一件事：用户会不会因此对不上号。

这个仓库不留兼容包袱。没有外部用户，没有发版约定，不存在「别人还在用旧写法」的情况。改名就删旧名，改配置键就直改，统一术语就全仓一次改完，已经不用的机制整块删掉。留下旧名字会让后来的人无法判断这两个名字哪个是对的。

加一条硬条目之前，先确定它能不能被测试或架构钉住。钉不住的写在 INTERACTION.md 里，不进第四节的硬条目。加一个图标或状态词，先看 `CONTENT.md` 那张表里有没有现成能用的。改动落地时同步三处：文档、`res/cards/m3e.css` 里对应的组件、各插件的代码。

规范的依据来自 Material 3 Expressive（令牌）、Apple HIG 的两层原则（体验九条与取舍八条，见[设计系统](DESIGN_SYSTEM.md#四体验原则--apple-hig)）、Nielsen Norman Group 的启发式（错误预防、认得出优于记得住）与 WCAG 2.2（对比度、颜色不作唯一通道）。四份都为桌面与网页而写，这里是逐条译成群聊里的动作。
