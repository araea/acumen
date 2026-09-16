# 统一度审计（2026-09-16）

按 [`docs/GUIDELINES.md`](GUIDELINES.md) 的十条硬条目与 [`docs/INTERACTION.md`](INTERACTION.md) 核对全仓 22 个插件，逐项整改。本文记整改之后的结论、每一项落在哪里，以及尚未处理的事项。

口径：范围是 `src/plugins/` 下注册进 `registry.rs` 的 22 个插件加 `src/plugins.rs`；视觉层与文案层只核代码有没有照着做；不保留兼容包袱，旧名字直接删。

## 结论

架构层一直是统一的，规范层本次补齐，文案层本次收拢。

注册表、出图入口、配置写路径、图标集合这四项一直统一，且都有测试或类型约束。规范层此前是空的，于是同一件事在不同插件里有不同做法：把「超时」叫三个名字，把 `⚠️` 用来表达六种意思，把当前前缀写死进提示，两个出卡片的插件没有关图开关。本次补上 GUIDELINES.md 与 INTERACTION.md，并按它改到位。

按偏离的分布看，`ai_news`（72 个配置字段）与 `oai`（符号指令与卡片正文最多）是两处主要来源，`help`、`ctl`、`stats`、`portrait` 改动最少。

## 已经同源的部分

这六项一直不用动，后续改动也不要退回去。

**注册表是唯一清单。** 22 个插件的 `display_name`、`section`、`summary` 全部写全，`/help`、`/help <插件>`、`/ctl list` 与用法卡都从这一处读。四条测试钉着：`every_plugin_has_a_summary`、`every_plugin_claims_a_known_section`、`sections_are_all_in_use_or_are_the_fallback`、`grouping_loses_no_plugin`。

**出图只有两个入口。** 五处 HTML 卡片调用点全部走 `render::web::shoot`，CPU 图像工作全部走 `render::worker.rs::run`，没有插件自己 `spawn_blocking`，也没有谁自己写量高与固定睡眠。

**写配置只有一条路。** `/ctl` 与 agent 房间的 `ayjx --ctl` 都汇到 `ctl::change`，先写盘再改内存。

**图标收敛。** 全仓只用到 `CONTENT.md` 那六个加三个行内记号（`▍`、`🔗`、`📄`），没有装饰性表情。

**视觉两层分工写着也测着。** `m3e.css` 文件头定「是什么」，各卡版式只说「摆在哪儿」。`a_chart_is_painted_in_the_card_scheme` 与 `the_word_hues_come_from_the_design_system` 从样式表读回令牌再比对；`assert_embeddable` 钉住「注释里不出现成对 HTML 标签」这一条，那个问题会把整张样式表截断而不报错。

**文案规范的边界清楚。** `CONTENT.md` 写明不管搭话人格。

## 整改清单

### 第一轮：口径与硬条目

先定规则再改代码。日志 target 的旧规则与代码矛盾（举例 `Plugin/WordCloud`，但注册名是 `wordcloud`），口径改成「注册名按单词边界大写」，写进 GUIDELINES 四.6，`ARCHITECTURE.md` 同步。指令本体里的数字不留空格（`近7天`）此前没写进规范，补进 `CONTENT.md` 3.2。状态图标与 koishi 工作区的两套不同，保留 ayjx 这套（`📭` 代替 `📋`，没有 `🏆`），在 `CONTENT.md` 3.5 写明差异是有意的。

| 项 | 处理 |
| --- | --- |
| 字段级 `serde(default = "fn")` 共 50 处 | `ai_news`（35）、`restart`（4）、`config.rs`（4）、`help`（2）全部换成容器级 `#[serde(default)]` 加一份 `Default`。`ambient/actions.rs` 的 5 处在工具调用参数 schema 里，不是用户配置，按四.4 保留 |
| `portrait` 与 `oai` 出卡片却没有关图开关 | `portrait` 补 `image_enabled`，`oai` 补 `image_enabled` 与 `image_scale`，关图与出图失败走同一条纯文本回退，`oai` 的开关收在 `reply_card` 一处 |
| `config.example.toml` 只覆盖 9 / 22 个插件，且没有测试 | 补全到 22 个，键与 `default_config()` 一一对应，新增三条测试 |
| `⚠️` 11 处表达 6 种意思 | `CONTENT.md` 3.5 收窄成「做成了，但要打折着看」，代码改到只剩 3 处（切片截断、改用缓存列表、切全量池），其余改成 ✅ 或 ❌。`⚠️` 11 降到 3，`❌` 75 升到 91 |
| `【】` 强调残留 4 处 | `media.rs` 与 `sticker.rs` 改成直接说明怎么发，`portrait.rs` 的文字版报告改用行内记号 `▍` 分节，同处的 `〖〗` 一并去掉 |
| 硬编码指令前缀 9 处 | `restart.rs`、`ai_news.rs` 改用 `get_prefixes(ctx)`，`parse_push_target`、`query_search`、`render_target_list` 加上 `prefix` 参数 |
| 一行以内的提示带句末句号 3 处 | `oai/logic.rs` 2 处、`ai_news.rs` 1 处改掉 |
| 同一个实体四种叫法 | `oai` 里 17 处 `❌ {} 不存在` 与 2 处 `已存在` 一律带上「智能体」，`❌ 图片下载失败` 补上原因 |
| 「超时」四个名字看不出各自等什么 | `pi_stall_seconds` 改 `request_stall_seconds`（`pi` 是已拆掉的旧引擎名），`max_wait_seconds` 改 `max_pending_seconds`（它管的是攒多久就必须判一次）。`gate_timeout_seconds` 与 `reply_timeout_seconds` 本来成对，保留 |
| 搭话 `hourly_limit` 与资讯 `realtime_max_per_hour` 同义不同名 | 统一成 `max_per_hour` |
| 引用驱动的有效期没有统一口径 | 定成「由插件自己存对应关系的两种统一留 30 天」，两处常量互相指认并指向 INTERACTION.md 第三节，`video_parse` 的注册表说明写明有效期 |
| 短反馈不出图只有 `ctl` 有显式分界 | 判据写进四.8，不抽共用类型 |

改了键名要同步线上 `config.toml`，否则运行中那份的值会被默认值替换。

```sh
./bot stop
# pi_stall_seconds → request_stall_seconds（180）
# max_wait_seconds  → max_pending_seconds（12）
# hourly_limit      → max_per_hour（线上 6）
./bot start   # 新增的 image_enabled / image_scale 由 fill_missing 补上
```

### 第二轮：逐插件

| 项 | 处理 |
| --- | --- |
| 配置字段缺 `///` 说明 | 补 40 多处：`stats` 14、`webshot` 6、`wordcloud` 7、`ai_news` 6、`image_split` 2、`recorder` 2、`logger` 1 |
| 注册表的指令表少写了代码吃得下的写法 | `portrait` 从 3 个补到 6 个，`video_parse` 从 3 个补到 10 个，`ai_news` 的模型榜从 2 个补到 4 个（漏了 `ai模型排行榜` 本身） |
| 空态被当成故障报 | `wordcloud` 与 `stats` 把「这段区间没数据」包成 `❌ 生成失败：…`，改成底层分开返回（`GenError::Empty`、`ChartError::NoData`），上层用 📭 说清为什么空。推送侧同理：冷群没数据记 `info!`，真失败才 `warn!` |
| 半角与弯引号混进中文文案 | `wordcloud` 与 `stats` 里的 `“本群”`、`"能查不能推"` 改成 `「」` |
| `wordcloud` 的日志 target 是字面量 | 提到 `LOG_TARGET` 常量 |

`CONTENT.md` 新增 3.6.1「空不是故障」，把这条判据与两处实现写在一起。

### 第三轮：文案终扫

按 `CONTENT.md` 3.1、3.2、3.7 扫全仓的用户可见文案。

| 项 | 处理 |
| --- | --- |
| 分隔线还在群消息里 | `help` 与 `ai_news` 各有一个 `———————————————` 常量，用在纯文本回退那条路上。两处删掉，改用 `▍` 分组与空行 |
| 三处空态没带 📭 | `portrait` 的「没有找到某人的发言记录」、`ai_news` 的「还没有推送目标」改成 📭 并各补一句能立刻做的事；`ai_news` 的「引用了一张认不出的卡片」按表改成 ❌ |
| `help` 的「没有找到插件」没有图标 | 补 ❌，去掉句末句号 |
| 失败被包了一层铺垫 | `ctl` 的 12 处错误一律渲染成「操作未完成：…」，改成 `❌ {原因}`；「请指定插件」改成「没写插件名」 |
| 引导被当成失败 | `ctl` 的 `reset` 确认、`oai` 音乐与视频房间的「没说想写什么歌」原本走 ❌，改成 💡 引导，两处都补了断言 |
| `ai_news` 提取序号的三种报错没有图标，且用了弯引号 | 补 ❌，`无法识别序号“2x”` 改成「认不出「2x」这个序号」 |

感叹号只出现在测试数据与人格样本里；语气词、波浪线、自夸词没有命中；省略号 `...` 只在日志行里，日志不属于说出口的话；状态词三组各归其位；术语表里 `命令` 的命中全在 agent 的工具说明，那是给模型读的参数描述。

### 第四轮：版式层里的字阶

| 项 | 处理 |
| --- | --- |
| `oai` 的回复卡在系统之外另有一套字阶 | 页脚轨迹、来源序号、智能体小卡这些卡上的小字原本写着 10、10.5、11、11.5、13、13.5、14 七个值，一律落到 `--md-type-label-small-size`（11px）与 `--md-type-label-medium-size`（14px）。行内代码的 `0.86em` 保留 |
| `ai_news` 的条色带有一个 `border-radius:3px` | 换成 `var(--md-shape-xs)` |

五张卡与 `reading.css` 里没有十六进制色值，`box-shadow` 全部走令牌。

## 尚未处理的

**`portrait`、`oai`、`ai_news` 的「短反馈不出图」边界靠人工核对。** `ctl` 有 `Output::card` 这个类型把两类输出分开，其余三个插件的数据形状不同，没有对应的造型。本轮查过一遍：`oai` 的 79 处一句话反馈全走 `reply_text`，`portrait` 走 `say()`，`ai_news` 的指令回执直接返回纯文本。

**`config.example.toml` 里 `[ambient] gate_persona` 只留一行注释。** 它的默认值是一整段中文，抄进示例等于同一段话住在两个地方。测试里的 `TEXT_DEFAULTS_IN_COMMENTS` 例外表记着这件事。

**`send_freshness_seconds` 与 `max_pending_seconds` 名字相邻、语义不同。** 前者是「交给平台后群里又有人说话就不再发」，后者是「攒够多久就判定」。两处 `///` 都写清楚了，暂不改名。

## 复核方式

```sh
cargo test --locked                                     # 521 项；含注册表、示例配置与兼容性
bash scripts/review-cards.sh                            # 七类卡片样张 + 布局报告

# 硬条目
rg 'serde\(default = "' src/                            # 期望：只剩 ambient/actions.rs 的工具参数
rg 'target: "Plugin"' src/                              # 期望：无输出
rg -c '"⚠️ ' src/                                        # 期望：3

# 文案层
rg -nP '"[^"]*/(ctl|help|ai|搭话|撤回|画像)[^"]*"' src/plugins  # 期望：只剩测试夹具与 include_str
rg -nP '"[^"]*[❌⚠️✅📭⏳💡][^"]*。"' src/plugins                # 期望：只剩 help 的三行页脚

# 配置
cargo test --locked example_config    # 每个插件都有一段、键集合与默认值一致、每段都能过校验
```

卡片的审美要看图，`review-cards.sh` 的自动断言只保证结构与内容边界。
