# 统一度审计（2026-09-16）

按 [`docs/GUIDELINES.md`](GUIDELINES.md) 的十条硬条目与
[`docs/INTERACTION.md`](INTERACTION.md) 核对全仓 22 个插件的现状，然后逐项整改。
下面记的是**改完之后的结论**与**每一项落在哪里**，给后续逐插件打磨当起点。

- [口径](#口径)
- [结论](#结论)
- [一、已经同源的部分](#一已经同源的部分)
- [二、本次整改的清单](#二本次整改的清单)
- [三、留给下一次的](#三留给下一次的)
- [四、复核方式](#四复核方式)

---

## 口径

- 范围：`src/plugins/` 下注册进 `registry.rs` 的 22 个插件，加 `src/plugins.rs`（框架）。
- 视觉层（`m3e.css`）与文案层（`CONTENT.md`）本次只核**代码里有没有照着做**，
  不重新评估设计本身。
- 不留兼容包袱：改名直接改、旧名字直接删、不写迁移分支。
- 不记已经修好的历史问题。

## 结论

**架构层本来就是齐的，规范层这次补齐了，文案层这次收干净了。**

- 注册表、出图入口、配置写路径、图标集合这四样一直真正统一，而且都被测试或类型钉住。
- 规范层原本是空的，所以同一件事在不同插件里有不同做法：把「超时」叫三个名字、
  把 `⚠️` 用来表达六种意思、把当前前缀硬编码进提示、两个出卡片的插件没有关图开关。
  本次新增 GUIDELINES.md 与 INTERACTION.md 补上这一层，并按它把代码改到位。
- 剩下一件事不是缺陷而是选择：`portrait` 与 `oai` 的「短反馈不出图」边界靠各自判断，
  没有像 `ctl` 那样抽成一个显式类型。三个插件的数据形状不同，强造共用类型得不偿失，
  按 GUIDELINES 四.8 那条判据人工核对即可。

---

## 一、已经同源的部分

这六项一直不用动，后续改动也别退回去。

**1. 注册表是唯一清单。** 22 个插件的 `display_name` / `section` / `summary` 全部写全，
`/help`、`/help <插件>`、`/ctl list` 与用法卡全部从这一处读。
四条测试钉着：`every_plugin_has_a_summary`、`every_plugin_claims_a_known_section`、
`sections_are_all_in_use_or_are_the_fallback`、`grouping_loses_no_plugin`。
五个分区各有插件认领，没有空摆设。

**2. 出图只有两个入口，没有例外。** 五处 HTML 卡片调用点全部走 `render::web::shoot`；
CPU 图像工作全部走 `render::worker.rs::run`。没有哪个插件自己 `spawn_blocking`，
也没有谁自己写量高与固定睡眠。

**3. 写配置只有一条路。** `/ctl`（按 `[ctl].admins` 判权）与 agent 房间的
`ayjx --ctl`（一次性凭据）都汇到 `ctl::change`，先写盘再改内存。

**4. 图标收敛，没有装饰表情。** 全仓只用到 `CONTENT.md` 那六个加三个行内记号
（`▍` / `🔗` / `📄`），没有 🎉 ✨ 🔥 这类装饰性表情。

**5. 视觉两层分工写着也测着。** `m3e.css` 文件头定「是什么」，各卡版式只说
「摆在哪儿」；`a_chart_is_painted_in_the_card_scheme` 与
`the_word_hues_come_from_the_design_system` 两条单测**从样式表读回令牌再比对**。
`assert_embeddable` 钉住了「注释里不许出现成对 HTML 标签」这个会把整张样式表
静默截断的坑。

**6. 文案规范划清了边界。** `CONTENT.md` 明确写了不管搭话人格
（`res/ambient/persona.md` + `voice.md`），这条边界是清楚的，别把它抹掉。

---

## 二、本次整改的清单

### 口径（先定规则，再改代码）

| 项 | 处理 |
| --- | --- |
| 日志 target 的旧规则与代码自相矛盾（举例 `Plugin/WordCloud`，但注册名是 `wordcloud`） | 口径改成「注册名按**单词边界**大写」，写进 GUIDELINES 四.6，`ARCHITECTURE.md` 同步 |
| 指令本体里的数字不留空格（`近7天`）没写进文案规范 | 写进 `CONTENT.md` 3.2，并说明它是指令本体独有的例外 |
| 状态图标与 koishi 工作区的两套 | 保留 ayjx 这套（`📭` 代替 `📋`，没有 `🏆`），在 `CONTENT.md` 3.5 写明差异是有意的 |

### 硬条目

| 项 | 处理 |
| --- | --- |
| 字段级 `serde(default = "fn")` 共 50 处 | `ai_news`（35 处）、`restart`（4）、`config.rs`（4）、`help`（2）全部换成容器级 `#[serde(default)]` + 一份 `Default`。`ambient/actions.rs` 的 5 处在**工具调用参数 schema** 里，不是用户配置，按 GUIDELINES 四.4 的规定留着 |
| `portrait` 与 `oai` 出卡片却没有关图开关 | `portrait` 补 `image_enabled`；`oai` 补 `image_enabled` 与 `image_scale`。关图与出图失败走同一条纯文本回退，`oai` 的开关收在 `reply_card` 一处，调用方不必各自判断 |
| `config.example.toml` 只覆盖 9 / 22 个插件，且没有测试兜着 | 补全到 22 个、键与 `default_config()` 一一对应；新增三条测试：`every_plugin_has_a_section_in_the_example_config`、`the_example_config_lists_exactly_the_default_keys`、`every_example_section_validates` |

### 文案

| 项 | 处理 |
| --- | --- |
| `⚠️` 11 处表达 6 种意思 | `CONTENT.md` 3.5 把 `⚠️` 收窄成「做成了，但要打折着看」，并给出「这条消息说的事，成了没有？」的判据与三条反例。代码改到只剩 **3 处**（切片截断、用缓存兜底、切全量池要涨消息量），其余 8 处改成 ✅ / ❌：`⚠️` 11 → 3，`❌` 75 → 91 |
| `【】` 强调残留 4 处 | `media.rs` 2 处、`sticker.rs` 1 处改成直接说「引用一条…」；`portrait.rs` 的文字版报告改用行内记号 `▍` 分节（同时去掉了同一处的 `〖〗`） |
| 硬编码指令前缀 5 处以上 | `restart.rs`、`ai_news.rs` 的 9 处改用 `get_prefixes(ctx)` 取当前前缀；`parse_push_target` / `query_search` / `render_target_list` 相应加上 `prefix` 参数 |
| 一行以内的提示带句末句号 3 处 | `oai/logic.rs` 2 处、`ai_news.rs` 1 处改掉 |
| 同一个实体四种叫法 | `oai` 里 17 处 `❌ {} 不存在` 与 2 处 `已存在` 一律带上「智能体」；`❌ 图片下载失败` 补上原因，与 `❌ 图片下载失败：{}` 统一 |

### 交互

| 项 | 处理 |
| --- | --- |
| 「超时」四个名字看不出各自等什么 | `pi_stall_seconds` → `request_stall_seconds`（`pi` 是已经拆掉的旧引擎名，属于遗留）；`max_wait_seconds` → `max_pending_seconds`（它管的是「攒多久就必须判一次」）。`gate_timeout_seconds` 与 `reply_timeout_seconds` 本来就成对，留 |
| 搭话 `hourly_limit` 与资讯 `realtime_max_per_hour` 同义不同名 | 统一成 `max_per_hour`；GUIDELINES 四.9 的后缀表相应改成 `*_seconds` / `*_budget` / `*_max_per_hour` |
| 引用驱动的有效期没有统一口径 | 定成「由插件自己存对应关系的那两种（资讯卡片、视频预览）统一留 30 天」，两处常量互相指认并指向 INTERACTION.md 第三节；`video_parse` 的注册表说明写明「预览 30 天内有效」。另外三种（撤回、转链接、收藏表情）读的是平台上的消息本体，窗口归 QQ，不归我们，也写进规范 |
| 短反馈不出图只有 `ctl` 有显式分界 | 判据（「读者会不会想把它留着」）写进 GUIDELINES 四.8；不强行抽共用类型 |

### 配置迁移

改了键名就要同步线上 `config.toml`，否则运行中那份的值会被默认值顶掉：

```sh
./bot stop
# pi_stall_seconds → request_stall_seconds（180）
# max_wait_seconds  → max_pending_seconds（12）
# hourly_limit      → max_per_hour（线上 6）
./bot start   # 新增的 image_enabled / image_scale 由 fill_missing 补上
```

### 第二轮：逐插件过一遍规范

第一轮改完之后，又按同一份规范把 22 个插件逐个查了一遍。这一轮的项目都不显眼，
但每一处都是「用户在群里真的会看到」。

| 项 | 处理 |
| --- | --- |
| 配置字段缺 `///` 说明 | 补了 40 多处：`stats` 14、`webshot` 6、`wordcloud` 7、`ai_news` 6、`image_split` 2、`recorder` 2、`logger` 1。`enabled` 与 `oai/types.rs` 的运行时结构按规范豁免（前者 22 个插件里含义完全一样，后者不是 `config.toml`） |
| 注册表的指令表少写了代码吃得下的写法 | `portrait` 从 3 个补到 6 个（少了 `人物画像`/`画像报告`/`用户画像报告`），`video_parse` 从 3 个补到 10 个，`ai_news` 的模型榜从 2 个补到 4 个（少了 `ai模型排行榜` 本身）。**用户看不见的写法等于不存在**，所以宁可把命令格写长 |
| **空态被当成故障报** | `wordcloud` 与 `stats` 把「这段区间没数据」包成 `❌ 生成失败：…`。两处都改成底层分开返回（`GenError::Empty` / `ChartError::NoData`），上层用 📭 说清为什么空、再给一条能立刻做的事。推送侧同一件事：冷群没数据记 `info!`，真失败才 `warn!` |
| 半角/弯引号混进中文文案 | `wordcloud` 的 `“本群”`、`stats` 的 `"本群"` 与 `"能查不能推"` 一律改成 `「」` |
| `wordcloud` 的日志 target 还是字面量 | 提到 `LOG_TARGET` 常量，子模块引用它 |

这一轮之后，`CONTENT.md` 3.6.1 新增了一节「空不是故障」，
把这条判据与两处实现钉在一起。

### 第三轮：扫语气、标点与剩下几处空态

按 `CONTENT.md` 3.1 / 3.2 / 3.7 把全仓的用户可见文案过一遍筛子
（感叹号、语气词、波浪线、自夸词、省略号、分隔线、状态词、术语表）。

| 项 | 处理 |
| --- | --- |
| **分隔线还在群消息里** | `help` 与 `ai_news` 各有一个 `———————————————` 常量，用在**纯文本兜底**那条路上，而 `CONTENT.md` 3.2 明写「分隔线不用」。两处都删掉：`help` 的分组本来就有 `▍`、下一步有 `💡`，横线只是多占一行；`ai_news` 的条目之间与页脚之前各留一个空行 |
| 还差三处空态没带 📭 | `portrait` 的「没有找到某人的发言记录」、`ai_news` 的「还没有推送目标」改成 📭 并各补一句能立刻做的事；`ai_news` 的「引用了一张认不出的卡片」按表改成 ❌（那是这次输入不被接受，不是空） |
| `help` 的「没有找到插件」没有图标 | 补 ❌，并去掉句末句号 |

其余几类查下来是干净的，记在这里免得下次重查：

- **感叹号**只出现在测试数据与人格样本里（`portrait` 的示例发言、`ambient` 的
  群名片样本），系统面一句都没有。
- **语气词、波浪线、自夸词**没有命中；`~` 与 `～` 的命中全是 `oai` 的
  符号指令解析（`~#` / `～＃` 等全半角变体）。
- **省略号 `...`** 只出现在日志行里——日志是给维护者看的，不属于「说出口的话」，
  不适用本条。
- **状态词**分成三组用对了：插件开关是「已启用 / 已停用」，推送目标与实时快报是
  「已开启 / 已关闭」，房间级开关是 `?开` / `?关`。
- **术语表**里 `命令` 的命中全在 agent 的工具说明（那是给模型读的参数描述），
  群消息里一律是「指令」。

---

## 三、留给下一次的

**1. `portrait` / `oai` / `ai_news` 的「短反馈不出图」边界靠人工核对。**
第二轮逐插件查过一遍：`oai` 的 79 处一句话反馈全走 `reply_text`，
`portrait` 走 `say()`，`ai_news` 的指令回执直接返回纯文本；出图的只有
「要被读、要被翻回去看」的那些。
`ctl` 有 `Output::card` 这个类型把两类输出分开（`ctl.rs` 的注释里写着理由），
其余三个插件的数据形状不同，没有对应的造型。做法是改动时按 GUIDELINES 四.8
的判据过一遍，不为此另造一个共用类型。

**2. `config.example.toml` 里 `[ambient] gate_persona` 只留一行注释。**
它的默认值是一整段人写的中文，抄进示例就是同一段话住在两个地方。
测试里有一张 `TEXT_DEFAULTS_IN_COMMENTS` 的例外表记着这件事——
再加例外要同时说明理由，别让它变成「懒得写」的后门。

**3. 搭话的 `send_freshness_seconds` 与 `max_pending_seconds` 名字相邻、语义不同。**
前者是「交给平台后群里又有人说话就不再发」，后者是「攒够多久就判定」。
两个 `///` 里都写清楚了，暂时不改名会更好——改名的收益比这一条小。

---

## 四、复核方式

改完之后用这几条核对，不要凭印象。

```sh
cargo test --locked                                     # 521 项；含注册表、示例配置与兼容性
bash scripts/review-cards.sh                            # 七类卡片样张 + 布局报告

# 硬条目
rg 'serde\(default = "' src/                            # 期望：只剩 ambient/actions.rs 的工具参数
rg 'target: "Plugin"' src/                              # 期望：无输出
rg -c '"⚠️ ' src/                                        # 期望：3（切片截断、缓存兜底、切全量池）

# 日志 target 与注册名对照
for p in $(rg -oP '^\s{4}\K[a-z_]+(?= \{)' src/plugins/registry.rs); do
  printf '%s -> %s\n' "$p" "$(rg -oP 'target: "(Plugin/[^"]*)"' src/plugins/$p.rs src/plugins/$p/*.rs 2>/dev/null | sort -u | tr '\n' ' ')"
done

# 文案层
rg '【' src/plugins/                                   # 期望：只剩测试数据
rg -nP '"[^"]*/(ctl|help|ai|搭话|撤回|画像)[^"]*"' src/plugins  # 期望：只剩测试与注释

# 配置
diff <(rg -oP '^\s{4}\K[a-z_]+(?= \{)' src/plugins/registry.rs | sort) \
     <(rg -oP '^\[\[?([a-z_]+)' config.example.toml | sort)      # 期望：无差异
```

**图像的审美仍然要看图**，`review-cards.sh` 的自动断言只保证结构与内容边界。
