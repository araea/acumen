# 架构说明

面向维护者的参考手册，只描述当前实现和约定，不记录历史。

- [目录结构](#目录结构)
- [事件流](#事件流)
- [插件系统](#插件系统)
- [插件编写约定](#插件编写约定)
- [出图与渲染](#出图与渲染)
- [配置与数据](#配置与数据)
- [新增一个插件](#新增一个插件)
- [构建与测试](#构建与测试)

## 目录结构

```text
src/
  main.rs          启动：加载配置、初始化数据库、连接适配器、优雅退出
  adapters/        适配器；satori.rs 为 Satori WS/HTTP 实现，console.rs 供本地测试
  command.rs       指令解析与消息内容提取的公共工具
  config.rs        AppConfig 与插件配置读写，build_config 辅助函数
  event.rs         Context / EventType / MessageEvent 定义
  http.rs          全局 reqwest 客户端（Android 上从系统 CA 取根证书），download_bytes
  log.rs           控制台与文件日志的统一输出
  matcher.rs       交互消息等待与分发（取消时自动清理）
  message.rs       Message 消息构建器（text/image/node_custom 等）
  plugins.rs       插件框架核心：Plugin 定义、注册宏、流水线、配置读写
  plugins/         各插件；registry.rs 为注册表（唯一的插件清单）
  render/          卡片渲染层：web.rs 是全部 HTML 卡片的唯一出图入口，另含原生字体与画布
  scheduler.rs     定时任务（daily / interval / 周期推送，带 Pace 错峰）
  db/              sea-orm 实体与查询（SQLite，data/bot.db）
```

`res/` 存放插件的静态资源（词库、人格提示词、技能说明、卡片与控制台的样式表），`docs/` 是这份手册，`tests/` 是几个用 Node 运行的端到端脚本，覆盖前台指令、重启和卡片落盘。

## 事件流

```text
适配器收到事件 → 构造 Context → plugins::run()
  按顺序执行已启用的插件 handler：
    Ok(Some(ctx)) → 传给下一个插件（插件拥有 Context 所有权，可以改写事件）
    Ok(None)      → 事件已被消费，流水线结束
    Err           → 记录 error 日志，按已消费处理，不会让适配器崩溃
  流水线结束后事件仍未消费 → 派发 EventType::BeforeSend
```

Context 通过移动传递，不深拷贝事件。`plugins::send_fake_event` 可以把伪造事件放回流水线。

插件的执行顺序就是 `registry.rs` 里的书写顺序。过滤类插件写在最前面（`meta_filter` 拦住心跳和元事件），`ctl` 紧随其后，保证管理入口不会被其他插件拦下。记录类插件（`logger`、`recorder`）在业务插件之前取得原始消息。

链接类插件的先后同样按书写顺序：`video_parse` 写在 `webshot` 前面，视频站链接先被它接走（就地取原片），截图那边跳过这类链接。准入判据是 `video_parse::is_video_link` 一处，两边不会各截一次又取一次。同类共用判据还有 `webshot::host_is_internal`（`web_fetch` 也用它拦内网）。

## 插件系统

一个插件是 `src/plugins/` 下的一个模块，提供三个必需项和两个可选钩子：

| 项 | 签名 | 说明 |
| --- | --- | --- |
| `handle` | `fn(Context, LockedWriter) -> BoxFuture<Result<Option<Context>, PluginError>>` | 必需，事件处理 |
| `default_config` | `fn() -> toml::Value` | 必需，通常为 `build_config(Config::default())` |
| `validate_config` | `fn(&toml::Value) -> Result<(), String>` | 必需，用真实配置类型反序列化 |
| `init` | `fn(Context) -> BoxFuture<Result<(), PluginError>>` | 可选，启动时建表或载入数据 |
| `on_connected` | 同 `handle` | 可选，Bot 连接就绪后注册推送任务 |

`Plugin` 结构还带一组面向用户的元数据，全部在注册表里声明：

| 字段 | 缺省 | 用途 |
| --- | --- | --- |
| `display_name` | 模块名 | 中文展示名，`/help` 与 `/ctl` 都认它 |
| `section` | `"misc"` | 帮助总览的分区代号，取值见 `help::SECTIONS` |
| `summary` | `""` | 一句话说明 |
| `commands` | `&[]` | 指令清单，`cmds![("指令", "说明"), …]` |

帮助中心不保存插件清单，`/help` 和 `/ctl` 都从注册表读取这些字段。新增插件只改 `registry.rs` 一处，帮助总览、插件详情和控制面板会同时更新。`section` 写错会落到「其他」而不是消失，`summary` 漏填会被 `help::tests` 拦下。

插件配置使用顶层 `[<name>] enabled`（例如 `[help]`），运行时每次事件都从配置快照读取。带生命周期钩子的插件如果启动时没有开启，之后开启会等待重启初始化，以免调用尚未就绪的 handler，`/ctl list` 里会标为「待重启」。控制与部署说明见 [CONTROL.md](CONTROL.md)。

## 插件编写约定

配置：只保留一份 Default 实现，并在容器级加 `serde(default)`，缺少的字段自动使用默认值，不要再写字段级 `default = "fn"`。

```rust
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Config { enabled: bool, /* ... */ }

impl Default for Config {
    fn default() -> Self { Self { enabled: true, /* ... */ } }
}

pub fn default_config() -> Value { build_config(Config::default()) }
```

读取用 `get_config_or_default(&ctx, "name")`（需要 `T: Default`），反序列化失败会告警并使用默认值。

指令匹配统一走 `crate::command`：

前缀类指令用 `match_command(ctx, cmd)` 或 `first_command_match(ctx, &[cmd])`。要求指令名后为空白或消息末尾的（ctl）用 `match_word_command`。自带正则匹配的（词云、stats 式）用 `strip_prefix`。参数用 `extract_text_arg(&matched.args)` 拼成纯文本，取图用 `get_image_url(ctx, writer, &args, reply_id)`，从文本里提第一个 URL 用 `find_url(text)`，要连卡片段一起看用 `message_links(ctx)`（卡片里的落地地址排在正文前面，`card_target_url` 单独用可按载荷取地址）。

匹配到就处理并返回 `Ok(None)`，不属于本插件就返回 `Ok(Some(ctx))` 放行。

错误处理：插件公开接口统一使用 `PluginError`（`Box<dyn Error + Send + Sync>`），可以用 `PluginResult<T>` 别名。内部子模块可以用 anyhow，但不要让它出现在边界之外。发送消息失败直接 `?` 传播，流水线会记录日志，不要用 `let _ =` 忽略错误。

发送消息统一走 `crate::adapters::satori::send_msg(&ctx, writer, group_id, user_id, msg)`，msg 支持 `Message`、`&str`、`String`。下载资源用 `crate::http::download_bytes(url)`。

日志 target 统一 `Plugin/<名字>`，名字按**单词边界**大写（`ai_news` → `Plugin/AiNews`，
`webshot` → `Plugin/WebShot`，`wordcloud` → `Plugin/WordCloud`），
子模块可以在后面加一级（`Plugin/OAI/Search`）。

## 出图与渲染

根据内容的阅读体验选择出图方式：

| 路线 | 依赖 | 使用方 | 适用 |
| --- | --- | --- | --- |
| HTML 卡片 `render/web.rs` | Chrome/Chromium、系统 CJK 字体 | help、ctl、ai_news、oai | 插件手册、状态清单、配置与差异、资讯长图、Markdown 回复 |
| 图表 plotters | 无 | stats、wordcloud | 坐标轴、折线、柱状、词云 |
| 真实网页截图 | Chrome/Chromium | webshot | 把链接本身截下来 |

需要出图的插件只声明「宽度 / 出图范围 / 格式」，剩下的都交给 `render::web::shoot`：

```rust
render::web::shoot(
    render::web::Shot::new(&html, 720).scale(scale).jpeg(92),
).await
```

`Shot` 的三个关键取值是宽度（视口与成图宽度，决定文字怎么换行）、选择器（命中的元素
的外接矩形就是成图边界，默认 `.shot`，oai 的回复卡片用 `.card`）和格式（help/ctl 用 PNG，
篇幅长的资讯用 JPEG）。量高度、等字体、尺寸护栏、并发闸门都在 `shoot` 里，
调用方不重复实现。

`shoot` 只对页面做一趟 `evaluate`：字体和内嵌图片用 `document.fonts.ready` / `img.decode()` 与一个 900 ms 定时器
赛跑，再让出一轮宏任务提交布局，然后一次量出盒模型，最后用带 clip 的整页截图取下来。
等布局靠观测，不靠固定睡眠，因此不出现「睡少了量到偏小的高度、卡片底部被切」这类
偶发问题。**不要退回 `requestAnimationFrame`**：headless 下它不保证触发，用它等布局
会死锁。

排队在超时之外：`CARD_GATE` 在 45 秒预算之前获取，排在后头的请求不会因为前面那张慢
而被判超时。超时、量不到盒子、超过高度上限（16000 CSS px）或像素预算（6400 万）
都返回错误，由调用方回退成完整文本，不产出一张截掉一半的图。`image_scale` 限制在
1—4 倍，非有限值回退到 3 倍。

闸门按「一类工作」划分，不按调用点划分：`render/web.rs` 的 `CARD_GATE`（3）管自家
生成的卡片，`webshot` 自己的闸门（2）管真实网页。两者若共用一道，一条慢网页会占住
卡片的位置。

`webshot` 打开网页走共享的浏览器实例，只有微信文章例外：微信按 UA 判「环境异常」，
桌面 UA 打开 `mp.weixin.qq.com` 的文章会被 302 到 `/mp/wappoc_appmsgcaptcha`，截出来
只有一句「当前环境异常」。这类链接另起一个带 MicroMessenger UA 的浏览器，宽度也用
480 的手机版式，用完即关。少数文章换了 UA 仍被送进验证页，这时不发图，只记一条日志。

一条消息里的正文链接取全量（`command::find_urls`，从前只取第一个）：逐条过准入、并发
截图，成功的几张合成一条消息发出去，一轮分享只打扰群聊一次。单条消息最多截
`MAX_LINKS_PER_MESSAGE`（4）条，超出按出现顺序丢弃。

### 卡片设计系统

四种卡片图（手册、控制、回复、资讯，资讯有日读与夜读两档）共用一套样式，
分两层，顺序不能换：

```rust
format!("{}{}", render::web::DESIGN_SYSTEM, 本卡版式)   // 拼成一个 <style> 的 body
```

- **系统层** `res/cards/m3e.css`（`render::web::DESIGN_SYSTEM`）：按 Material 3
  Expressive 的口径定义字阶、形状、高度、间距、配色角色与组件基元（`.md-card`、
  `.md-badge`、`.md-chip`、`.md-callout`、`.md-command`…）。四套配色方案
  （`scheme-manual` / `scheme-control` / `scheme-reply` / `scheme-news`，
  最后一套有深色档）也在这一个文件里，放在一起便于横向比。
- **版式层**：`res/cards/reading.css`（help / ctl 的 `Doc` 模型）与各插件里那份
  `const CSS`。**只写「摆在哪儿」，不许出现色值、字号、圆角、阴影的字面量**，
  一律 `var(--md-*)` 取令牌。写了就是绕过令牌直接写字面量。

控制台不用这一套。它的令牌只在 `res/console/tokens.css`（配色由
`scripts/make-tokens.py` 从种子色经 HCT 生成），组件在 `res/console/app.css`，
两层按这个顺序拼（`src/plugins/console/assets.rs` 的 `stylesheet()`）。规范见
[WebUI 设计系统](DESIGN_SYSTEM.md)。

三条与 M3 的刻意偏离（字阶按中文字面放大、字重只用 500/600/700/800 四档、
阴影不透明度收回到纸面量级）与「为什么不引外部字体」都写在 `m3e.css` 的开头。

字体只列系统里真有的：`Noto Sans CJK SC` 一族到底，不做拉丁与中日韩混排。装真字重见
`scripts/install-cjk-weights.sh`。

图表（plotters，位图，取不到 CSS）与词云的配色对照同一张色表：`stats` 的
`ColorScheme` / `HUES` 与 `wordcloud` 的 `WORD_COLORS` 都从 `m3e.css` 里抄了字面量，
由 `a_chart_is_painted_in_the_card_scheme`、`the_word_hues_come_from_the_design_system`
两条单测从样式表里读回来比对。改了 CSS 没改代码，测试会红。

文案与这层配套：声音、语气、标点、状态词表、术语表、状态图标。

样式表塞在 `style` 元素里，HTML 的 raw text 解析遇到闭合标签
就结束。**任何注释里都不许出现 HTML 的成对标签字面量**，否则整张样式表被截断，
页面不报错，只是退回无样式。`render::web::assert_embeddable` 钉着这一条。

另外：所有动态内容都做 HTML 转义，页面不执行脚本，也不加载外部资源，截图前等待字体
和布局完成。help / ctl 的 `Doc` 模型由浏览器完成字体塑形、标点和长文本换行，默认输出
3 倍 PNG。总览用 920 px 两列网格，条目的分隔线用「每条加顶线、首行两条不画」，
任何条数都左右对称。`:last-child` 在网格里只命中整个网格的最后一条，会让右列末条有线、
左列末条没线。

字重：Android 自带的 Noto Serif/Sans CJK 只有 Regular 一档，向系统请求 Bold 得到的仍是 400 字重。两条出图路径都会自行合成粗体（浏览器原生支持，原生绘制使用 `Typeface.embolden` 做形态学膨胀），但外扩轮廓无法补出笔画的粗细对比。运行 `sh scripts/install-cjk-weights.sh` 把真实的 Bold(700) 和 Black(900) 安装到 `~/.fonts` 后，fontconfig 和 fontdb 会自动使用它们，合成量为零，代码不需要改动。不安装也能运行，只是标题会细一档。网页卡片标题按用途使用 700—800 字重。字体是设备本地状态，仓库里无法恢复，换机器需要重新运行脚本。

CPU 图像工作统一通过 `render/worker.rs::run`：统计绘图、词云、GIF（包括信息和拆帧）、
图片切分共享两条阻塞执行槽。先异步等待许可，再提交 `spawn_blocking`。许可归计算闭包
持有，调用方被取消后也不会提前放开并发。网络请求与发送消息不占执行槽。

词云横向排词、暖白底，画布只决定词排得开不开。词是绕着画布中心一条螺旋线摆开的，
内容天然是中间一团：词少的时候 800×600 上能空出近一半暖白，群里刷过去就是一张白图。
裁边交给 araea-wordcloud（0.1.13 起）：布局时记下每个词占到的包围盒，`trim` 把成图的
viewBox 与尺寸收到「内容 + `trim_margin`」，SVG 与 PNG 走同一条路，不扫像素、不依赖
背景色，也省掉了解码重编码那一趟；内容顶到画布边的那一侧夹在画布内，成图不会比设定
画布还大。GIF 的单次处理上限为 256 帧和合计 3200 万像素。缩放、拼图也在分配输出画布
前检查尺寸。图片切分按相邻网格边界分配余数，完整保留原图边缘。

智能回复表格使用固定表布局与单元格换行，来源标题完整折行。资讯标题取消 CSS 行数
裁切（摘要仍遵循插件配置的字符预算）。资讯固定 720 CSS px，滚动条不影响版心。

原生工具 `render/font.rs`、`canvas.rs`、`kit.rs` 保留供原生绘图使用。是否迁移渲染方式以实际阅读质量为准，ai_news 保持网页日夜主题。

出图失败时回退到纯文本：浏览器缺失、初始化失败、截图超时或尺寸超限都不应让帮助和控制失去响应。图文数据来自同一份注册表，以及经过权限校验、敏感字段脱敏的配置。

短反馈不出图。一句话的纠错、开关确认和报错都走纯文本。出图慢，在群里还多一条图片，也不方便复制文字。ctl 的 `Output::card` 按这条界线划分。

## 配置与数据

`config.toml` 不入库：首次启动写入默认值，启动时补字段、清残留，解析失败则退出，不覆盖原文件。插件配置改动经过 `plugins::update_config` 或 ctl 插件，持久化由 `config_save_lock` 串行化。数据库是 `data/bot.db`，插件数据目录是 `data/<plugin>/`（`get_data_dir`）。

写配置只有一条路径：`ctl::change`。它获取 `config_save_lock`，按插件真实的 serde 类型校验，先写盘再改内存，任何一步失败都不会留下部分修改。两个入口都汇到这里：

| 入口 | 身份 | 实现 |
| --- | --- | --- |
| 聊天或前台控制台 `/ctl` | 消息发起人，按 `ctl.admins` 判权 | `plugins/ctl.rs` |
| agent 房间 `acumen --ctl` | 一次性凭据换维护者身份 | `plugins/ctl/bridge.rs` |
| 本机控制台网页 | 回环地址 + 口令 | `plugins/console/` |

控制台（`src/plugins/console/`）是唯一一处「读」也不走指令的地方，它的分工是：

| 文件 | 管什么 |
| --- | --- |
| `mod.rs` | 插件的注册面：配置、`init`、`on_connected`、`--no-ui` 与 `--ui` 两个覆盖项 |
| `state.rs` | 进程内的那一份状态：启动时刻、口令、日志环形缓冲与订阅、各适配器的连接 |
| `server.rs` | HTTP 面：静态资源不设防，`/api/*` 一律要口令；起停与优雅关闭 |
| `api.rs` | 各接口的数据组装。写的一律转给 `ctl` |
| `assets.rs` | 内嵌的前端（`res/console/`），以及盯着令牌分层、图标与转义的测试 |

日志落到面板上只挂了一处钩子：`log::hook` 在 `print` 里接一个闭包（`src/log.rs`），控制台启动时装上它。`log.rs` 因此仍然谁都不依赖。

## 新增一个插件

1. 写 `src/plugins/<name>.rs`（或 `<name>/mod.rs` 式的目录模块），提供 `handle`、`default_config`、`validate_config`，按需加 `init` / `on_connected`
2. 在 `src/plugins/registry.rs` 增加一条记录，位置决定它在流水线中的顺序：

   ```rust
   my_plugin {
       display_name: "我的插件",
       section: "play",
       summary: "一句话说明它做什么",
       commands: cmds![("我的指令 <参数>", "这条指令干什么")],
       on_init: Some(my_plugin::init)
   },
   ```

3. 运行 `cargo test`。注册表与帮助的一致性检查会指出还差什么：`every_plugin_has_a_summary`、`every_plugin_claims_a_known_section`、`grouping_loses_no_plugin`，以及跑遍全部插件的 `satori_compat_tests`

不需要修改 `help.rs`、`ctl.rs` 或任何渲染代码，`/help`、`/help <插件名>`、`/ctl list` 都会自动包含新插件。只有需要新分区时，才去 `help::SECTIONS` 加一行。

## 构建与测试

```sh
cargo check        # 快速验证
cargo test         # 出图与浏览器类为 ignored
cargo fmt          # 提交前
```

改动插件后至少运行 `cargo test`：`plugins::satori_compat_tests` 会用规范化消息跑全部插件，`help::tests` 校验注册表元数据完整、分区不丢插件。

卡片版式改动需要人工看图，各插件有 `ignored` 的落盘测试：

```sh
HELP_CARD_DUMP=/tmp/cards    cargo test help::card     -- --ignored
CTL_CARD_DUMP=/tmp/cards     cargo test ctl::card      -- --ignored
AI_NEWS_CARD_DUMP=/tmp/cards cargo test live_page_is_parseable -- --ignored  # 落盘 HTML
```

它们用真实注册表生成样张（包含启用/停用、长昵称、超长指令表等边界情况），出图落盘后逐张核对。断言只保证能画完并且是合法 PNG，是否好看需要自己看。
