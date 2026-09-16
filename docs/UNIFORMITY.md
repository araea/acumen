# 统一度审计（2026-09-16）

按 [`docs/GUIDELINES.md`](GUIDELINES.md) 的十条硬条目与
[`docs/INTERACTION.md`](INTERACTION.md) 核对全仓 22 个插件的现状。
这份是**基线**，给后续逐插件打磨当待办清单用；改完之后回来更新结论。

- [口径](#口径)
- [结论](#结论)
- [一、已经同源的部分](#一已经同源的部分)
- [二、口径本身要先定的](#二口径本身要先定的)
- [三、硬条目上的偏离](#三硬条目上的偏离)
- [四、文案层的偏离](#四文案层的偏离)
- [五、交互层的偏离](#五交互层的偏离)
- [六、复核方式](#六复核方式)

---

## 口径

- 范围：`src/plugins/` 下注册进 `registry.rs` 的 22 个插件，加 `src/plugins.rs`（框架）。
- 视觉层（`m3e.css`）与文案层（`CONTENT.md`）本次只核**代码里有没有照着做**，
  不重新评估设计本身。
- 结论分两级：**硬条目**（GUIDELINES 第四节，有测试或架构兜着）与**文案/交互**
  （要人看，测试兜不住）。
- 不记已经修好的历史问题，只看当前工作区。

## 结论

**架构层是齐的，规范层是缺的，文案层是散的。**

- 注册表、出图入口、配置写路径、图标集合这四样已经真正统一——这是最难统一的部分，
  而且都被测试或类型钉住了；22 个插件里没有哪个绕开了它们。
- 缺的是**一套写下来的行为规范**，所以同一件事在不同插件里有不同的做法：
  把「超时」叫三个名字、把 `⚠️` 用来表达六种意思、把当前前缀硬编码进提示。
  这些不是哪个插件写错了，是从来没有一份东西说该写哪个。
- 本次新增的 GUIDELINES.md 与 INTERACTION.md 补的就是这一层。

按硬条目的偏离数量看，`ai_news`（最大，72 个配置字段）与 `oai`（符号指令与
卡片正文最多）是两个主要来源；`help` / `ctl` / `stats` / `portrait` 基本干净。

---

## 一、已经同源的部分

这六项不用动，后续改动也别退回去。

**1. 注册表是唯一清单。** 22 个插件的 `display_name` / `section` / `summary` 全部写全，
`/help`、`/help <插件>`、`/ctl list` 与用法卡全部从这一处读。
四条测试钉着：`every_plugin_has_a_summary`、`every_plugin_claims_a_known_section`、
`sections_are_all_in_use_or_are_the_fallback`、`grouping_loses_no_plugin`。
五个分区（`message` / `play` / `insight` / `system` / `misc`）各有插件认领，没有空摆设。

**2. 出图只有两个入口，没有例外。** 五处 HTML 卡片调用点
（`help/card.rs`、`ctl/card.rs`、`oai/render.rs`、`portrait/card.rs`、`ai_news/card.rs`）
全部走 `render::web::shoot`；CPU 图像工作全部走 `render/worker.rs::run`。
没有哪个插件自己 `spawn_blocking`，也没有谁自己写量高与固定睡眠——
`AUDIT.md` 里记的「四套卡片各写各的量高」已经收干净了。

**3. 写配置只有一条路。** `/ctl`（按 `[ctl].admins` 判权）与 agent 房间的
`ayjx --ctl`（一次性凭据）都汇到 `ctl::change`，先写盘再改内存。

**4. 图标收敛到六个，没有装饰表情。** 全仓只用到 `CONTENT.md` 那六个
（✅ 17 次、❌ 75 次、📭 12 次、⚠️ 11 次、⏳ 9 次、💡 7 次）加三个行内记号
（`▍` / `🔗` / `📄`）。没有出现 🎉 ✨ 🔥 这类装饰性表情。（用法本身有问题，见第四节。）

**5. 视觉两层分工写着也测着。** `m3e.css` 文件头定「是什么」，各卡版式只说
「摆在哪儿」；`a_chart_is_painted_in_the_card_scheme` 与
`the_word_hues_come_from_the_design_system` 两条单测**从样式表读回令牌再比对**，
改了 CSS 没改代码会红。`assert_embeddable` 钉住了「注释里不许出现成对 HTML 标签」
这个会把整张样式表静默截断的坑。

**6. 文案规范已经划清了边界。** `CONTENT.md` 明确写了不管搭话人格
（`res/ambient/persona.md` + `voice.md`），这条边界是清楚的，后续别把它抹掉。

---

## 二、口径本身要先定的

这三条不是代码写错了，是**规则没写清或自相矛盾**。先定下来，再改代码。

### 2.1 日志 target 的口径与代码对不上

`ARCHITECTURE.md` 写的是「命名与注册名一致（例如 `Plugin/WordCloud`）」，
但注册名是 `wordcloud`——`Plugin/WordCloud` 并不是注册名，规则按字面读自己就不成立。
全仓 15 个插件级 target，14 个走的都是**按单词边界大写**：
`Plugin/AiNews`、`Plugin/Ambient`、`Plugin/Ctl`、`Plugin/GroupTitle`、`Plugin/Help`、
`Plugin/OAI`（另加 `Plugin/OAI/{Images,MJ,Music,Video}` 四个子级）、`Plugin/Portrait`、
`Plugin/Recorder`、`Plugin/Repeater`、`Plugin/Restart`、`Plugin/Stats`、
`Plugin/VideoParse`、`Plugin/WebShot`、`Plugin/WordCloud`。

不符合的只有两处：

| 位置 | 现状 | 按口径应为 |
| --- | --- | --- |
| `image_split.rs:140/178/200` | `Plugin/ImageSplitter` | `Plugin/ImageSplit`（比注册名多了一个词） |
| `src/plugins.rs` 的 8 处 | 裸 `"Plugin"` | 框架层，可另立固定 target（如 `Plugin/Lifecycle`） |

第二处有 `[{}]` 带了插件名，日志还能看出是谁；第一处是真的多了一个词。

**建议**：口径改成「注册名按单词边界大写、子模块加一级」，
`ImageSplitter` → `ImageSplit`；框架那 8 处改成固定 target 或在规范里写明框架不适用。

### 2.2 指令本体里的数字不留空格

全仓到处在用：`近7天`、`近30天`、`裁剪 3x3` 的 `3x3`、
`ai静默 23:30-07:30`。`CONTENT.md` 3.2 只说「数字与中文之间留一个空格」，
照字面读这几个全违规，但它们**必须紧贴才能照打**——指令本体是给用户抄的。

**建议**：写进 `CONTENT.md` 3.2：「指令本体里的数字紧贴中文，因为要能整条抄走；
散文里（包括参数占位符之外的行文）数字与中文之间留空格。」
规则写下来，下一个人就不会去「统一」掉其中一边。

### 2.3 状态图标与 koishi 工作区是两套

| | koishi 工作区 | ayjx |
| --- | --- | --- |
| 集合 | ✅ ⏳ 💡 ⚠️ ❌ 📋 🏆 | ✅ ⚠️ ❌ ⏳ 📭 💡 |
| 「空」 | 📋（罗列） | 📭（空） |
| 「庆祝」 | 🏆 | 没有 |

同一个作者的两套东西，不能靠「哪天顺手对齐一下」解决。

**判断：保留 ayjx 这套。** 理由有两个——ayjx 的系统消息里没有「庆祝」这个场景
（猜中通关那类内容都在搭话人格里，不受 `CONTENT.md` 管），加 🏆 只会变成一个
没人用的符号；`📭` 比 `📋` 更直白地表达「没有内容，但不是错误」，而 `📋`
在 ayjx 里没有对应物（帮助与清单直接出卡片图）。

**建议**：在 `CONTENT.md` 里加一句说明「与 koishi 工作区的差异是有意的，
不要拿那边的表来对齐」，并说清为什么。差异本身没问题，没写下来才是问题。

---

## 三、硬条目上的偏离

### 3.1 字段级 `#[serde(default = "fn")]` 还在，共 41 处

违反 GUIDELINES 四.4「只保留一份 `Default`，不要写字段级默认值」。

| 文件 | 处数 |
| --- | --- |
| `src/plugins/ai_news.rs` | 35 |
| `src/config.rs` | 4（框架层） |
| `src/plugins/help.rs` | 2（`image_enabled` 用 `default_true`） |

其余 20 个插件的配置都是容器级 `#[serde(default)]` + 一份 `Default`，
说明这个约定本身是能落地的，`ai_news` 只是没跟上——它也是唯一有 72 个字段、
39 个名字像 `default_realtime_max_per_hour` 这种包装函数的插件。

**建议**：`ai_news` 整块换成 `#[serde(default)]` + `Default`，删掉那 35 个
`default_*` 函数。`help` 那两处一并收。`config.rs` 是框架层，可以单独一轮。

### 3.2 两个卡片插件没有关图开关

`image_enabled` 只有 `help` / `ctl` / `ai_news` 三个有。
`portrait` 与 `oai` 也出卡片（见 ARCHITECTURE 的出图路线表），
但部署者想把图关掉时，在这两个插件上做不到。

| 插件 | 出图 | `image_enabled` | `image_scale` | 主题 |
| --- | --- | --- | --- | --- |
| help | 卡片 | 有 | 有 | — |
| ctl | 卡片 | 有 | 有 | `card_theme` |
| ai_news | 卡片 | 有 | 有 | `card_theme` + `theme` |
| portrait | 卡片 | **缺** | 有 | `theme` |
| oai | 卡片 | **缺** | **缺** | — |
| webshot | 真实网页 | — | 有（`device_scale_factor`） | — |

**建议**：`portrait` 与 `oai` 补 `image_enabled`；`oai` 一并补 `image_scale`。
`webshot` 截的是真实网页，不是自家卡片，可以不走这套——但要写进规范说明为什么例外。

### 3.3 `config.example.toml` 只覆盖 9 / 22 个插件

371 行的手写文件，只有 `oai`、`ambient`、`ctl`、`repeater`、`stats`、`portrait`、
`webshot`、`video_parse`、`help` 九段。缺的 13 个里包括 **`ai_news`——它有 72 个
配置字段，是最大的一个**，以及 `gif`、`restart`、`recorder`、`wordcloud`、
`sticker`、`media`、`recall`、`echo`、`group_title`、`image_split`、`logger`、
`meta_filter`。

而且没有测试把它和 `Default` 对齐，也没有测试保证每个插件都有一段。

**建议**：补全 13 段；加一条测试——每个注册插件在 `config.example.toml` 里都有
自己的段，且键集合与 `default_config()` 一致。这条测试比人工核对可靠得多，
也正是 GUIDELINES 四.5「加一个配置项 = 同时动三处」唯一能被机器兜住的部分。

---

## 四、文案层的偏离

`CONTENT.md` 已经在仓库里，下面这些是代码没跟上。

### 4.1 `⚠️` 一个符号表达六种意思

全仓 11 处 `⚠️`，按 `CONTENT.md` 第二节那张表（`⚠️` = 只做成一半，或做成了
但结果不完整）**只有一处是合规的**：

| 现在的用法 | 位置 | 实际含义 | 按表应为 |
| --- | --- | --- | --- |
| `⚠️ 切片数量过多，为防止风控，仅发送前 99 张` | `image_split` | 做成了，但结果不完整 | ⚠️ ✔ |
| `⚠️ 刷新失败，将展示缓存列表：{}` | `oai` | 降级，但结果完整 | ⏳ 或忽略 |
| `⚠️ 实时快报已切换为全部资讯；…` | `ai_news` | 成功，带提醒 | ✅ |
| `⚠️ 已清空 {} 个智能体的所有历史` | `oai` | 成功 | ✅ |
| `⚠️ 未检测到媒体文件` | `media` | 这次输入不被接受 | ❌ |
| `⚠️ 未检测到有效链接` | `media` | 这次输入不被接受 | ❌ |
| `⚠️ 检测不到图片或表情，可能是商城表情等特殊格式` | `sticker` | 这次输入不被接受 | ❌ |
| `⚠️ 请在发送指令时附带图片，或引用一张图片` | `image_split` | 这次输入不被接受 | ❌ |
| `⚠️ 获取模型失败：{}` | `oai` | 外部条件问题 | ❌ |
| `⚠️ 重启指令未开放，可用 /ctl set …` | `restart` | 这次输入不被接受 | ❌ |
| `⚠️ 实时推送的总开关已停用。…` | `ai_news` | 这次输入不被接受 | ❌ |

这正是 koishi 工作区文案规范开头点名的那个问题——「同一个符号有四种含义」。
区别是我们这边是六种。

**建议**：在 `CONTENT.md` 里把 `⚠️` 收窄成**一种**（做成了，但有代价或只做成一部分），
然后按上表全仓改一遍。这个改动会让 `❌` 的数量从 75 涨到八十多，
这是对的——`❌` 本来就是「这次输入不被接受」。

### 4.2 `【】` 强调残留 4 处

`CONTENT.md` 3.5 明确「`【】` 作为强调一律移除」，但还在用：

- `media.rs:120` `请【引用】一条包含图片或视频的消息`
- `media.rs:200` `请在指令后附带 URL，或【引用】一条包含 URL 的消息`
- `sticker.rs:61` `请【引用】你想要保存的表情包，然后发送此指令`
- `portrait.rs:616` 画像标签用 `【{}】{}` 拼标题

这三处 `【引用】` 都是同一个错误来源：想强调「这一步得用引用」。
按 INTERACTION.md 第三节，引用是 ayjx 的招牌模式，**该由帮助与卡片说明，
而不是在每条报错里加粗**。`portrait.rs:616` 是版式层面的标签，改用容器色或字重表达。

### 4.3 硬编码指令前缀 5 处

`restart.rs:191`、`ai_news.rs:1247 / 1251 / 1272 / 1694` 里写死了 `/ctl` 与
`/ai实时模式`。前端前缀由 `command_prefix` 配置，部署者改掉之后这些提示就是死路
（`CONTENT.md` 3.4 要求带**当前**前缀）。

**建议**：从 `get_prefixes(ctx)` 取，与 `help.rs:118` 的 `prefix_of()` 同一处口径。

### 4.4 一行以内的提示带句末句号 3 处

`CONTENT.md` 3.2「一行以内的提示句末不加标点」：

- `oai/logic.rs:1108` `❌ 无效模型。\`/%\` 查看中转站模型，或用 \`智能体%pi 模型\` 交给内置智能体。`
- `oai/logic.rs:1161` `❌ 不认识这个写法。\`房间?\` 换一边…`
- `ai_news.rs:1356` `⚠️ 实时推送的总开关已停用。…`

### 4.5 同一个实体四种叫法

```text
❌ {} 不存在            （oai）
❌ 智能体 {} 不存在      （oai）
❌ {} 已存在            （oai）
❌ 目标名称 {} 已存在    （oai）
```

`CONTENT.md` 3.7 的术语表已经定了「智能体」这个词，但只有一处用了。
另外「图片下载失败」与「图片下载失败：{}」两种写法并存——按第三节的
「失败要说清卡在哪」，应该统一成带原因的那种。

---

## 五、交互层的偏离

`INTERACTION.md` 是这次新写的，下面几条是新规范与现状的差距，
**改不改、怎么改放到逐插件打磨那一轮去定**，这里只列出来。

### 5.1 「超时」有三个名字，看名字分不出区别

`reply_timeout_seconds`（等模型）、`gate_timeout_seconds`（等搭话判定）、
`max_wait_seconds`（等合并窗口）、`pi_stall_seconds`（单次请求的上限）。
四个确实语义不同，但名字上看不出各自等的是什么，
部署者改配置时很容易改错那一个。

**建议**：既然不留兼容包袱，就改成读得懂的名字——
`model_timeout_seconds` / `gate_timeout_seconds` / `coalesce_window_seconds` /
`request_stall_seconds`。至少也要在每个 `///` 里写清「等的是什么」。

### 5.2 限流的命名几乎统一，有两处同义不同名

`*_seconds` / `*_budget` 两个后缀基本落实了。
两个例外：搭话用 `hourly_limit`，资讯用 `realtime_max_per_hour`，是同一件事；
另外 `*_limit` 这个后缀看不出量纲是每小时还是总共。

**建议**：按 GUIDELINES 四.9 收成 `*_max_per_hour`——`hourly_limit` →
`max_per_hour`，其余照旧。

### 5.3 引用驱动的有效期没有统一口径

`ai_news` 的卡片提取明说「近 30 天发送的」（写在找不到时的报错里），
`video_parse`、`media`、`sticker`、`recall` 各自实现，没有说法。
按 INTERACTION.md 第三节，引用指向历史消息，**必须说清它还能用多久**。

**建议**：给这四个各定一个有效期（或明确「不限时」），并写进各自插件的
`docs/*.md` 与帮助卡。用户看到「引用之后回一句话」时，应该知道这句话明天还作不作数。

### 5.4 短反馈不出图只有 ctl 有显式分界

`ctl` 有 `Output::card` 这个类型，把「要出图的四种输出」与「一句话反馈」分开
（`ctl.rs:397`）。`help` 的 `not_found` 也是不出图的（`help.rs:286` 有注释说明），
但那是零散判断，不是一处口径。`portrait` / `oai` / `ai_news` 没有对应的分界，
靠各处自己决定。

**建议**：不必强造一个共用类型（三个插件的数据形状不同），
但把判据写进 GUIDELINES 四.8 后就按它核对一遍——判据是
**「读者会不会想把它留着」**，不是「这段文字长不长」。

---

## 六、复核方式

改完之后用这几条核对，不要凭印象。

```sh
# 硬条目
cargo test --locked                                     # 注册表元数据、分区、兼容性
rg 'serde\(default = "' src/                            # 期望：只剩容器级 #[serde(default)]
rg 'target: "Plugin"' src/                              # 期望：无输出（框架层除外）

# 日志 target 与注册名对照
for p in $(rg -oP '^\s{4}\K[a-z_]+(?= \{)' src/plugins/registry.rs); do
  printf '%s -> %s\n' "$p" "$(rg -oP 'target: "(Plugin/[^"]*)"' src/plugins/$p.rs src/plugins/$p/*.rs 2>/dev/null | sort -u | tr '\n' ' ')"
done

# 文案层
rg '【' src/plugins/                                   # 期望：只剩测试数据
rg '"⚠️ ' src/plugins/                                 # 逐条按 CONTENT.md 第二节判
rg -n '/ctl|/help|/ai' src/plugins/*.rs | rg -v '^[^:]*:[0-9]*:\s*//'   # 硬编码前缀

# 配置说明覆盖
diff <(rg -oP '^\s{4}\K[a-z_]+(?= \{)' src/plugins/registry.rs | sort) \
     <(rg -oP '^\[\[?([a-z_]+)' config.example.toml | sort)              # 期望：无差异

# 视觉
bash scripts/review-cards.sh                            # 样张落盘 + 布局报告
cargo test --locked a_chart_is_painted_in_the_card_scheme the_word_hues_come_from_the_design_system
```

**图像的审美仍然要看图**，`review-cards.sh` 的自动断言只保证结构与内容边界。
