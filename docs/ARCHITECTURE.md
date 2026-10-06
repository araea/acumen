# 架构

模块边界与维护约定。界面规范见 [WebUI 设计系统](DESIGN_SYSTEM.md)，运行与配置管理见[插件控制](CONTROL.md)

## 源码结构

```text
src/
  main.rs          配置加载、数据库、适配器连接与退出
  adapters/        Satori 与本地测试用 console 适配器
  command.rs       指令解析、消息内容与链接提取
  config.rs        全局与插件配置
  event.rs         Context、事件类型与消息事件
  http.rs          HTTP 客户端与资源下载
  log.rs           日志输出
  matcher.rs       交互消息等待与分发
  message.rs       消息构造
  plugins.rs       插件接口、注册表、事件流水线与配置读写
  plugins/         插件实现；registry.rs 是唯一插件清单
  render/          卡片渲染、原生字体与画布；render/markdown 是 markdown 插件的排版
  scheduler.rs     定时任务与错峰推送
  storage.rs       数据目录、原子写文件与常驻 JSON 状态
  db/              SeaORM 实体与 SQLite 查询
res/               词库、提示词、技能、卡片与控制台资源
docs/              项目文档
tests/             前台运行、重启、渲染与端到端冒烟（e2e.py 自起假 Satori 实现端）
```

## 事件流水线

适配器收到事件后构造 `Context`，调用 `plugins::run()`。插件按 `registry.rs` 的顺序执行：

- `Ok(Some(ctx))`：交给下一个插件
- `Ok(None)`：事件已处理，结束流水线
- `Err`：记录错误并按已处理结束，不会让适配器崩溃

未被消费的事件随后派发 `BeforeSend`。适配器只认下面「插件接口」里的钩子与 `SendGuard`（条件发送的闸），不认具体插件：复读、消息记录、跟随撤回都是经注册表挂进来的。Context 通过所有权移动，不复制整份事件。顺序有语义：过滤器与 `ctl` 靠前，记录插件在业务插件前读取原始消息，`video_parse` 在 `webshot` 前接收视频链接。共享的链接与内网准入判据必须复用现有函数。

## 插件接口

实现位于 `src/plugins/`：

| 接口 | 要求 | 用途 |
| --- | --- | --- |
| `handle` | 必需 | 处理事件并返回是否继续传递 |
| `PluginConfig` | 必需 | 配置类型：键名、默认值、取值检查；注册表据此生成默认配置与校验 |
| `init` | 可选 | 启动时初始化数据或资源 |
| `on_connected` | 可选 | 适配器连接就绪后启动任务 |
| `on_receive` | 可选 | 事件进入流水线之前，按收到顺序同步调用（可改写事件；返回的任务先于流水线执行） |
| `on_consumed` | 可选 | 消息被交互等待消费、不再进入流水线时调用 |
| `on_sent` | 可选 | 一次 `message.create` 发出并拿到回执之后调用（`Receipt`：发送包、频道、消息 ID） |

用户可见的名称、分区、摘要与指令在 `registry.rs` 声明；`/help`、`/ctl` 与 Web 控制台均读取该注册表。

插件配置使用顶层 `[插件名]` 表与 `enabled` 开关。配置结构体带容器级 `serde(default)` 与一份 `Default` 实现，再实现 `PluginConfig`：`NAME` 是配置表的键（与模块名一致，编译期核对），范围、可选值之类的检查写在 `check`，类型对不上时的提示可用 `MISMATCH` 写得更具体。读取用 `get_config_or_default::<Config>(ctx)`，写回用 `update_config::<Config, _>(ctx, …)`：调用点不再重复插件名；反序列化失败时记录告警并使用默认值。

消息匹配与发送复用 `crate::command`、`crate::message` 与 `crate::adapters::satori`。插件错误使用 `PluginError` / `PluginResult`；发送失败向上返回，不要用 `let _ =` 忽略。

新增插件的步骤：

1. 在 `src/plugins/` 新建模块，实现 `handle` 与配置类型的 `PluginConfig`；需要启动钩子时再实现 `init` 或 `on_connected`。
2. 在 `src/plugins/registry.rs` 添加元数据与钩子（第一项写 `config:`）；注册表顺序决定事件处理顺序。
3. 运行 `cargo test --locked`，检查摘要、分区、帮助清单与 Satori 兼容性。

新增帮助分区时才需同时修改 `help::SECTIONS`。

## 图片渲染与资源限制

`markdown` 插件的解析、排版、分页与代码着色在 `render::markdown`；`oai` 回复卡有独立渲染，两边不共用代码。

HTML 卡片统一经 `render::web::shoot` 渲染。调用方提供内容与宽度，共享层处理字体、图片加载、布局测量、截图格式与并发限制。字体与内嵌图片解码完成后再测量；headless Chromium 不保证触发 `requestAnimationFrame`，不要用它等待布局。

- 卡片最多等待 45 秒（不含排队），高度上限 16,000 CSS px，像素预算 6,400 万。
- 超时、无法测量或超限时返回错误，由调用方回退完整文本，不发送截断图片。
- 动态内容须 HTML 转义；卡片不加载外部资源、不执行脚本。
- 并发闸门：卡片最多 3 项，网页截图最多 2 项，互不占用额度。
- 统计图、词云、GIF 与图片切分经 `render/worker.rs` 的两槽阻塞工作池执行；执行槽由实际计算闭包持有，调用方取消不能提前释放。
- GIF 最多 256 帧、合计 3,200 万像素；分配输出前校验尺寸。
- 浏览器缺失、截图失败或尺寸超限时，帮助与控制仍须可用。短反馈使用文本，不为一次开关确认或错误信息渲染图片。

## 配置与数据

`config.toml` 不入库。启动时补齐缺失默认值，不覆盖已有值；解析失败时退出，不覆盖原文件。运行数据位于 `data/`：SQLite 数据库为 `data/bot.db`，插件数据位于 `data/<plugin>/`。插件写自己的文件一律经 `storage`：`write_atomic*` 先写临时文件、落盘再改名，进程在写的中途被杀也只会留下旧版本；一份常驻内存的 JSON 状态用 `JsonState`（读不出的文件会改名留证据，不直接覆盖）。目录路径只由 `storage::data_path` 算一份。

配置写入统一经过 `ctl::change`：按插件真实类型校验，写入同目录临时文件并原子替换，成功后再更新内存。Web 控制台的修改也走这条路径，接口默认绑定 `127.0.0.1` 并使用访问口令。敏感字段、权限、备份与部署限制见[插件控制](CONTROL.md)。

## 构建与测试

```sh
cargo clippy --all-targets --locked   # 保持零告警
cargo fmt
cargo test --locked
cargo build --release --locked
```

`Cargo.toml` 的 `[lints]` 在默认 clippy 之外强制了一组现代写法：`let … else`、格式串内联变量、`Duration::from_mins` 一类更大的时间单位、去掉多余的限定路径与原始字符串井号。它们都能由 `cargo clippy --fix` 机械改写。

插件层不留死代码：`plugins.rs` 不再带 `allow(dead_code)`，没人用的函数、常量、字段由编译器报出来、直接删。框架层的工具箱模块（`message`、`event`、`command`、`db`、`scheduler`、`adapters/satori/api`、`render`）成套提供接口、不按调用数裁剪，各自在文件头声明了 `allow(dead_code)`。

改动框架、流水线或插件钩子之后，再跑一遍端到端冒烟（需要 `pip install aiohttp`，在隔离目录里拉起一个连假端口的实例，不碰线上数据）：

```sh
python3 tests/e2e.py target/release/acumen
E2E_STARTUP=1 python3 tests/e2e.py target/release/acumen   # 全部插件启动自检
```

卡片版式改动还需用真实注册表生成图片并人工检查：

```sh
HELP_CARD_DUMP=/tmp/cards cargo test help::card -- --ignored
CTL_CARD_DUMP=/tmp/cards cargo test ctl::card -- --ignored
```

自动测试能检查图片格式、布局边界与文字溢出，不能代替视觉检查。
