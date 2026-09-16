# 统一度审计（2026-09-16）

按 [`docs/GUIDELINES.md`](GUIDELINES.md) 的十条硬条目与 [`docs/INTERACTION.md`](INTERACTION.md) 核对全仓 23 个插件，逐项整改。本文记整改之后的结论、每一项落在哪里，以及尚未处理的事项。

口径：范围是 `src/plugins/` 下注册进 `registry.rs` 的插件加 `src/plugins.rs`（第四轮时是 22 个，第五轮加了控制台变 23 个）。视觉层与文案层只核代码有没有照着做。不保留兼容包袱，旧名字直接删。

## 结论

架构层一直统一。规范层本次补齐。文案层本次收拢。

注册表、出图入口、配置写路径、图标集合这四项一直统一，且都有测试或类型约束。规范层此前是空的，于是同一件事在不同插件里有不同做法：把「超时」叫三个名字，把 `⚠️` 用来表达六种意思，把当前前缀写死进提示，两个出卡片的插件没有关图开关。本次补上 GUIDELINES.md 与 INTERACTION.md，并按它改到位。

按偏离的分布看，`ai_news`（72 个配置字段）与 `oai`（符号指令与卡片正文最多）是两处主要来源，`help`、`ctl`、`stats`、`portrait` 改动最少。

## 已经同源的部分

这六项一直不用动，后续改动也不要退回去。

**注册表是唯一清单。** 每个插件的 `display_name`、`section`、`summary` 全部写全，`/help`、`/help <插件>`、`/ctl list` 与用法卡都从这一处读。四条测试钉着：`every_plugin_has_a_summary`、`every_plugin_claims_a_known_section`、`sections_are_all_in_use_or_are_the_fallback`、`grouping_loses_no_plugin`。

**出图只有两个入口。** 五处 HTML 卡片调用点全部走 `render::web::shoot`，CPU 图像工作全部走 `render::worker.rs::run`，没有插件自己 `spawn_blocking`，也没有谁自己写量高与固定睡眠。

**写配置只有一条路。** `/ctl`、agent 房间的 `ayjx --ctl` 与本机控制台网页都汇到 `ctl::change`，先写盘再改内存。

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

感叹号只出现在测试数据与人格样本里。语气词、波浪线、自夸词没有命中。省略号 `...` 只在日志行里，日志不属于说出口的话。状态词三组各归其位。术语表里 `命令` 的命中全在 agent 的工具说明，那是给模型读的参数描述。

### 第四轮：版式层里的字阶

| 项 | 处理 |
| --- | --- |
| `oai` 的回复卡在系统之外另有一套字阶 | 页脚轨迹、来源序号、智能体小卡这些卡上的小字原本写着 10、10.5、11、11.5、13、13.5、14 七个值，一律落到 `--md-type-label-small-size`（11px）与 `--md-type-label-medium-size`（14px）。行内代码的 `0.86em` 保留 |
| `ai_news` 的条色带有一个 `border-radius:3px` | 换成 `var(--md-shape-xs)` |

五张卡与 `reading.css` 里没有十六进制色值，`box-shadow` 全部走令牌。

### 第五轮：控制台

这一轮加的是界面，不是插件，但它是「用户看得见的第十种载体」，所以按同一张表过了一遍。

| 项 | 处理 |
| --- | --- |
| 界面另起一套视觉语言的风险 | 令牌不新造：`m3e.css` 加一套 `scheme-console` 方案（浅深两版都写全，它是网页，跟随系统明暗），`res/console/app.css` 只写版式与交互基元。`the_layout_layer_borrows_every_value_from_the_system_layer` 剥掉注释后扫十六进制色、`rgb(`/`hsl(`、`font-size`、`border-radius`、`box-shadow`，一条都不许写字面量 |
| 界面与终端各存一份状态的风险 | 控制台不另存状态：读的是注册表与 `config.toml`，写的是 `ctl::change`，日志是 `log::hook` 挂上来的同一行。它是先有的「一条写路径」的第三个触发器，不是第四条 |
| 分区名两处维护的风险 | `help::SECTIONS` 提成 `help::sections()`，插件页的筛选项与 `/help` 的分区读同一份 |
| 空态、状态词、图标另起一套的风险 | 沿用 `CONTENT.md`：空态 `📭`、「已启用／已停用／待重启」三组状态词、`label-*` 两档小字 |
| 页面加载外部资源的风险 | `the_page_loads_nothing_from_the_network` 扫 `src="http`、`href="http`、`@import`、`url(http`——与卡片那条同源，界面要能在完全离线的设备上打开 |
| 界面挂住没法看的问题 | 新增 `scripts/review-console.sh`，八页拍成本地图片。走 chromedriver 而不是 `chromium --screenshot`：日志页的长连接让页面永不空闲，headless 截图会等满超时 |
| 图标改一处别处对不上的风险 | 标记的几何只写在 `scripts/make-icon.py` 一处，产物是 `res/console/icon.svg` |

两处仍靠人工核对：**界面的手感**（间距、折行、滚动）只能看图，`review-console.sh` 的产物是给人看的。**窄屏折行**只有两条硬规矩（键名不断词、日志正文整条落到第二行），其余按 `GUIDELINES` 的分工判断。

### 第六轮：界面长成应用形态（2026-09-16）

这一轮把那张网页做成可安装的应用（Material 3 Expressive：自适应导航、状态层、涟漪、分段按钮、自家对话框），并按同一张表过了一遍。

| 项 | 处理 |
| --- | --- |
| 点不动按钮的风险 | 导航与「进详情」一律是真链接（`<a href="#/…">`），换页只靠 hashchange；事件委派挂在 `document` 上，不再挂在某个容器的兄弟节点。上一版底栏是死的——点击只委派在 `#view` 上，而 `#nav` 是它的兄弟。`review-console.sh` 现在逐格点一遍五格导航 |
| 行里套按钮的风险 | 行身是一层铺满整行的链接（`.row-hit`），开关压在上面（`.row-tail` 的 `z-index`）。链接里套按钮是无效标记，浏览器会连开两件事（换页 + 开关）。断言：点开关不换页、点行身进详情 |
| 破坏性操作弹出浏览器原生框的风险 | 换成本机 `<dialog>`（Esc、焦点陷阱、返回键由浏览器给），填充版用 `error-container` 那一对，不拿 `error` 当底色——深红字压在深绿底上就是看不见的按钮 |
| 手机上打字被三档宽度挤没的风险 | 窄屏（≤560px）配置表不摆两列：键名一行、控件占满下一行。两列时输入框只剩一百多像素，模型名与路径都给截断 |
| 界面拖慢机器人的风险 | 动画只碰 `transform` 与 `opacity`，外壳不设 `transform`/`filter`/`backdrop-filter`（那三样会让固定定位的底栏飞掉，也最费 GPU）；日志只在日志页可见时接收，服务端合批 50ms、一批最多 128 行，页面每 100ms 至多落一次 DOM；静态资源带 `ETag` 走 304 |
| 图标少一张装不上的风险 | `make-icon.py` 一处几何出七份产物（svg、192、512、遮罩 512、单色 svg 与 512、iOS 180），`every_icon_the_manifest_promises_exists` 对着清单逐个认，并按格式校验（矢量看开头、位图看 PNG 签名） |
| 界面挂住没法看的问题 | `review-console.sh` 从八页扩成三档宽度的七页，并跑四条交互断言；四条不过就退非零（当天晚些时候收成三条只读断言，见文末复审） |

### 第七轮：外围、字号、标记与跟随（2026-09-16）

上一轮之后又过了一遍全仓的用户可见面。这一轮改的都是「同一个产品里两处长得不一样」的地方，以及一处真坏了的交互。

| 项 | 处理 |
| --- | --- |
| 卡片图的「相纸」只有一张卡真画了 | `m3e.css` 一直写着 surface-dim 是卡片外的相纸，但只有 `oai` 真铺了它；另外四张（help/ctl/ai_news/portrait）的卡面圆角、描边、阴影落在纯白上，合起来只剩一条悬着的线——资讯卡那个「外围边框不协调」就是这么来的。现在 `.shot` 收进 `m3e.css` 的组件基元（一处给底色与内边距），五张卡的成图外围是同一种处理，`oai` 也从「裁到卡面」改成「裁到相纸」，圆角外那点透明底跟着没有了 |
| 界面另有一支字阶 | `res/console/app.css` 的 `:root` 覆盖了十三级 `--md-type-*` 的字号与行高，整体收一档（正文 16→15、小字 16→13.5、标题 33→26）。卡片图的字阶不动——那一套是按群聊缩略图标定的 |
| 界面卡面与卡片图不像同一张纸 | 界面的 `.card` 补上 `outline-variant` 的 1px 描边与 `elevation-1`，与 `.md-card` 同形；页面底色本来就是 surface-dim，于是「卡面浮在相纸上」这层关系两边一致 |
| 页头那枚小标另抄了一份几何 | `index.html` 里内联的 `<svg class="brand-mark">` 与 `icon.svg` 是两份手抄的同一图形，已经不一致（内联那份修好了白值，文件那份没有）。改成 `<img src="/icon.svg">`，几何只剩一处 |
| 标记不符合自适应图标的安全区 | 旧的「言」字外接框 60×64，右下角离中心 45.7，超出安全圆半径 33——圆形遮罩一刀下去，口的那两个下角就没了。新标记外缘收在 32（`main()` 里有一条断言盯着），并补齐 monochrome 层、清单里的 `purpose: monochrome`、以及 iOS 那份不透明铺满的位图 |
| 域名撞名 | 产品名从「知言」改成「知微」。审计出的两条硬理由：古义里「言」是宾语（别人的话），而它最吃重的一件事是自己开口说话；以及应用商店里已经有两个可下载的 AI 助手叫「知言」。理由与出处写在 `README.md`，不改的是仓库名、可执行文件名与线上的 `x-zhiyan-token` 请求头——那是内部标识，与显示名无关 |
| 日志看不到最新几行 | 屏外行的 `content-visibility: auto` 按 48px 估值，面板的 `scrollHeight` 因此比实际矮，贴底落在一个过时的高度上。DOM 窗口只有 80 行，省下来的重排本来就不值得，整条去掉；贴底改成「判定只看位置」，程序化滚动与换行都不会被误判成暂停意图，滚回底部自己恢复；`EventSource` 进入 CLOSED 之后按 2→4→8→16 秒退避自己重开 |
| 总览那六行日志停在打开那一刻 | 每六秒拉一次 `/api/logs?limit=6`，只在页面可见时拉。拉的是内存里的环形缓冲，不碰数据库 |

## 尚未处理的

**`portrait`、`oai`、`ai_news` 的「短反馈不出图」边界靠人工核对。** `ctl` 有 `Output::card` 这个类型把两类输出分开，其余三个插件的数据形状不同，没有对应的造型。本轮查过一遍：`oai` 的 79 处一句话反馈全走 `reply_text`，`portrait` 走 `say()`，`ai_news` 的指令回执直接返回纯文本。

**`config.example.toml` 里 `[ambient] gate_persona` 只留一行注释。** 它的默认值是一整段中文，抄进示例等于同一段话放在两个地方。测试里的 `TEXT_DEFAULTS_IN_COMMENTS` 例外表记着这件事。

**`send_freshness_seconds` 与 `max_pending_seconds` 名字相邻、语义不同。** 前者是「交给平台后群里又有人说话就不再发」，后者是「攒够多久就判定」。两处 `///` 都写清楚了，暂不改名。

## 复核方式

```sh
cargo test --locked                                     # 含注册表、示例配置、兼容性与控制台
bash scripts/review-cards.sh                            # 七份样张 + 布局报告
bash scripts/review-console.sh                          # 三种宽度 × 七页样张 + 三条只读交互断言（要先有在跑的实例）
node tests/console-backend.cjs                          # 隔离实例：口令、历史上限、SSE、前台响应、优雅退出
node tests/console.cjs                                  # 真实 Chromium：交互、压力、前后台切换与样张

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


## 2026-09-16 · 控制台视觉与性能复审

后续复审发现：宽屏行点击仍会截获开关，指令表单与连接开关存在遗漏，日志离页不断连，暂停仍改 DOM。此次统一修复，并把日志从逐行事件与 500 行 DOM 改为服务端合批、可见页订阅、有界 DOM 窗口。补齐快照连续性、浅深色日志对比、48px 触控与焦点状态。完整结论、取舍和验证入口见 [WebUI 审计](WEBUI_AUDIT.md)。**DOM 窗口在第七轮从 120 行收到 80 行、一次最多插 40 行**，`content-visibility` 整条去掉。

线上样张脚本现为三条只读交互断言；更完整的写操作回归移至隔离的 `tests/console.cjs`，不再改变运行实例的插件开关。

同一天的后一轮复审（六个维度各查一遍、每条发现由另一个代理独立反驳）又确认并修掉十余处：设置页草稿的「删掉这条」会删掉配置里第一条真连接、锁屏与路由错误页整页没有标题、段控焦点环被祖先裁掉、两处对比度不达标、口令需要转义时日志流恒 401、地址栏里的旧 `?t=` 会盖掉刚存下的新口令、`log_lines` 上限三处不一致、后端测试会连上真实现端、两处测试断言实际测不到它声称的东西。清单、改法与两处明确不改的取舍见 [WebUI 审计](WEBUI_AUDIT.md) 的「第二轮复审」。
