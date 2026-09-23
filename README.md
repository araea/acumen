# 知微

知微（Acumen，仓库名 `acumen`）是一个跑在自己机器上的群聊机器人：基于 Rust，通过 Satori v1 协议连接实现端，22 个插件在配置文件里开关与调整。

一份核心，两种用法：

- **终端**：`cargo build --release --locked` 出来的那个可执行文件，前台或交给 runit 常驻。没有图形界面也照常跑——`--no-ui` 之后连网页那一层也不开。
- **网页**：核心自己在回环地址上发的一张控制台（运行状况、插件开关与配置、搭话、实时日志、连接与维护命令），`./bot ui` 打开它。它同时是一份可安装的应用：手机与桌面浏览器都能把它加到主屏幕，装上之后没有地址栏。界面只有这一份，没有单独的客户端。

「知微」取理解细微信息之意，对应搭话中的上下文理解与统计里的日常观察；英文名 **Acumen** 是同一层意思——对细微之处的敏锐洞察。它与接入层[知弦](https://github.com/araea/satori-qq)共同组成群聊机器人的两部分。仓库、可执行文件、runit 服务、环境变量前缀与线上请求头统一为 `acumen`，中文显示名保留「知微」；完整依据见[名称与界面复审](docs/NAME_DESIGN_AUDIT.md)。

## 安装

需要 Rust 1.94 或更高版本。先准备配置，再构建：

```sh
cp config.example.toml config.toml
cargo build --release --locked
./bot start
```

Satori 默认地址是 `http://127.0.0.1:3001`。网页截图和资讯长图需要本机安装 Chrome 或 Chromium，可以用 `browser_path` 指定路径。浏览器不可用时，帮助和插件控制改用纯文本。中文出图需要系统中日韩字体。如果字体只有 Regular 一档，标题会使用合成的粗体，运行 `sh scripts/install-cjk-weights.sh` 可以安装真实的粗体字重。

## 控制台

启动后日志里会有一行带口令的地址，形如：

```text
[14:05:03] [INFO] [Plugin/Console] 控制台已就绪 http://127.0.0.1:7801/?t=…
```

在本机浏览器打开它，或者 `./bot ui` 让 `termux-open-url` 代劳。它只绑回环地址、要那道口令，默认端口 7801；`[console]` 里可以改地址、端口与口令，`--no-ui` 让某一次启动完全不开放它。关掉之后命令行、群里的指令、排期与推送都不受影响。

界面默认使用紧凑字号，可在「接入与全局 → 阅读密度」切换到舒适档。日志工作区支持级别与关键词筛选、缓冲计数和筛选记录导出。

日志只在页面可见时接收，切回 Termux 或离开日志页就暂停连接；回来后补齐最近记录。暂停跟随后可停留阅读旧日志，点「回到最新」恢复。

页面上五处：总览、插件、搭话、日志、命令，右上角的设置按钮是接入与全局设置。版式按宽度分三档（窄屏底部导航条、中等导航轨、宽屏常驻抽屉），宽屏上插件页是列表与详情并排。装到桌面：Android 用 Chrome 的「安装应用」，iOS 用 Safari 的「添加到主屏幕」，设置页里写着当前这台该怎么装。

```sh
./bot ui        # 打开控制台（地址取自启动时写下的那一份）
./bot ui url    # 只打印地址，不打开
```

## 配置

`config.toml` 不提交到 Git。首次启动会写入缺少的默认字段，配置解析失败时不覆盖原文件。

- `command_prefix`：指令前缀，默认 `/`
- `global_filter`：全局群黑白名单
- `[[bots]]`：Satori 连接实现端，`console` 用于本地测试
- `access_token`：Satori 令牌，也可以由 `ACUMEN_SATORI_TOKEN` 提供
- `[ctl]`：插件控制权限，`admins` 填维护者 QQ 号
- `[console]`：本机控制台，默认只绑 `127.0.0.1:7801`
- `[oai]`：可选的 OAI 与内置 Agent 设置
- `[ambient]`：群聊搭话设置，复用 `[oai]` 的模型、接口与联网配置

数据库文件是 `data/bot.db`，插件数据目录在可执行文件旁边的 `data/<插件>/`。

## 插件与运行

插件放在 `src/plugins/`，清单在 `src/plugins/registry.rs`；新增插件只改这一处，`/help`、`/ctl` 与控制台会自动包含它。发送 `/help` 查看指令；`/ctl`（别名 `/控制`、`/插件`）用于查看和修改插件开关与配置。两者默认以卡片图作答，把 `image_enabled` 设为 false 可以改回纯文本。首次使用前需要停机设置 `[ctl].admins`，列表为空时只允许本机控制台管理。

```sh
./bot status
./bot stop
./bot restart
```

Termux 下 `./bot start` 会取得唤醒锁；`./bot logs` 用 tmux 窗口跟运行日志，`./bot attach` 进入。把 bot 交给 `termux-services`（runit）托管后，进程崩溃会自动重启，`start` / `stop` / `restart` 自动改走 `sv`，见[插件控制](docs/CONTROL.md)。

## 文档与测试

- [WebUI 设计系统](docs/DESIGN_SYSTEM.md)
- [名称与界面复审](docs/NAME_DESIGN_AUDIT.md)
- [统一度审计](docs/UNIFORMITY.md)
- [架构说明](docs/ARCHITECTURE.md)
- [架构与渲染审计](docs/AUDIT.md)
- [插件控制](docs/CONTROL.md)
- [Satori 接入](docs/SATORI.md)
- [Satori 协作审计](docs/SATORI_INTEGRATION_AUDIT.md)
- [内置 Agent 房间](docs/agent.md)
- [群聊搭话](docs/ambient.md)
- [视频解析](docs/video_parse.md)

```sh
cargo test --locked
cargo build --release --locked
```
