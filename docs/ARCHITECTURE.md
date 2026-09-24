# 架构说明

本文记录 Acumen 当前的模块边界和维护约定。界面设计规范见 [WebUI 设计系统](DESIGN_SYSTEM.md)，运行和配置管理见[插件控制](CONTROL.md)。

## 源码结构

```text
src/
  main.rs          配置加载、数据库、适配器连接与退出
  adapters/        Satori 与本地测试用 console 适配器
  command.rs       指令解析、消息内容和链接提取
  config.rs        全局与插件配置
  event.rs         Context、事件类型与消息事件
  http.rs          HTTP 客户端与资源下载
  log.rs           日志输出
  matcher.rs       交互消息等待与分发
  message.rs       消息构造
  plugins.rs       插件接口、注册表、事件流水线与配置读写
  plugins/         插件实现；registry.rs 是唯一插件清单
  render/          卡片渲染、原生字体与画布
  scheduler.rs     定时任务与错峰推送
  db/              SeaORM 实体与 SQLite 查询
res/               词库、提示词、技能、卡片和控制台资源
docs/              项目文档
tests/             前台运行、重启和渲染测试
```

## 事件流水线

适配器收到事件后构造 `Context`，并调用 `plugins::run()`。插件按 `registry.rs` 中的顺序执行：

- `Ok(Some(ctx))`：交给下一个插件。
- `Ok(None)`：事件已处理，结束流水线。
- `Err`：记录错误，并按已处理事件结束；不会使适配器崩溃。

未被消费的事件之后会派发 `BeforeSend`。Context 通过所有权移动，不复制整份事件。插件顺序有语义：过滤器和 `ctl` 靠前，记录插件在业务插件前读取原始消息；`video_parse` 在 `webshot` 前接收视频链接。共享的链接与内网准入判据必须复用现有函数，避免同一链接重复处理或访问本机地址。

## 插件接口

插件实现位于 `src/plugins/`，提供以下接口：

| 项 | 要求 | 用途 |
| --- | --- | --- |
| `handle` | 必需 | 处理事件并返回是否继续传递 |
| `default_config` | 必需 | 返回默认 TOML 配置 |
| `validate_config` | 必需 | 按真实配置类型校验配置 |
| `init` | 可选 | 启动时初始化数据或资源 |
| `on_connected` | 可选 | 适配器连接就绪后启动任务 |

用户可见的名称、分区、摘要和指令在 `registry.rs` 声明。`/help`、`/ctl` 与 Web 控制台均读取该注册表，不维护重复清单。

插件配置使用顶层 `[插件名]` 表和 `enabled` 开关。配置结构体使用一份 `Default` 实现，并在容器级设置 `serde(default)`，使新字段可使用默认值。读取配置用 `get_config_or_default`；反序列化失败时记录告警并使用默认值。

消息匹配和发送复用 `crate::command`、`crate::message` 与 `crate::adapters::satori` 中的接口。插件公开错误使用 `PluginError` / `PluginResult`；发送失败向上返回，不要用 `let _ =` 忽略。

## 新增插件

1. 在 `src/plugins/` 新建模块，实现 `handle`、`default_config` 和 `validate_config`；需要启动钩子时再实现 `init` 或 `on_connected`。
2. 在 `src/plugins/registry.rs` 添加元数据和钩子。注册表顺序决定事件处理顺序。
3. 运行 `cargo test --locked`。测试会检查摘要、分区、帮助清单与 Satori 兼容性。

无需修改帮助、控制或渲染模块。新增帮助分区时才需要同时修改 `help::SECTIONS`。

## 图片渲染与资源限制

HTML 卡片统一经 `render::web::shoot` 渲染。调用方提供内容和宽度；共享层处理字体、图片加载、布局测量、截图格式和并发限制。字体及内嵌图片解码完成后再测量；headless Chromium 不保证触发 `requestAnimationFrame`，不要用它等待布局。

卡片渲染最多等待 45 秒（不含排队），高度限制为 16,000 CSS px，像素预算为 6,400 万。超时、无法测量或超过限制时返回错误，由调用方回退为完整文本，不能发送截断图片。CSS 设计令牌见 [DESIGN_SYSTEM.md](DESIGN_SYSTEM.md)。动态内容须 HTML 转义；卡片不加载外部资源或执行脚本。

生成卡片与真实网页截图使用不同并发闸门：卡片最多 3 项，网页截图最多 2 项。网页请求不应占用卡片的渲染额度。统计图、词云、GIF 和图片切分经 `render/worker.rs` 的两槽阻塞工作池执行；执行槽由实际计算闭包持有，调用方取消不能提前释放。GIF 最多处理 256 帧、合计 3,200 万像素；分配输出前校验尺寸。

浏览器缺失、截图失败或尺寸超限时，帮助与控制仍须保持可用。短反馈使用文本，不为一次开关确认或错误信息渲染图片。

## 配置与数据

`config.toml` 不入库。启动时补齐缺失默认值，不覆盖已有值；解析失败时退出，不覆盖原文件。运行数据位于 `data/`：SQLite 数据库为 `data/bot.db`，插件数据位于 `data/<plugin>/`。

配置写入统一经过 `ctl::change`：按插件真实类型校验，写入同目录临时文件并原子替换，成功后再更新内存。Web 控制台的修改也走这条路径；接口默认绑定 `127.0.0.1` 并使用访问口令。敏感字段、权限、备份和部署限制见[插件控制](CONTROL.md)。

## 构建与测试

```sh
cargo check
cargo test --locked
cargo fmt
cargo build --release --locked
```

卡片版式改动还需用真实注册表生成图片并人工检查。现有落盘测试：

```sh
HELP_CARD_DUMP=/tmp/cards cargo test help::card -- --ignored
CTL_CARD_DUMP=/tmp/cards cargo test ctl::card -- --ignored
```

自动测试能检查图片格式、布局边界和文字溢出，不能代替视觉检查。
