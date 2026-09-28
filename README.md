# 知微（Acumen）

群聊机器人：经 Satori 协议接入多个聊天平台，以可插拔插件提供 AI 对话、群管与媒体处理能力

[![GitHub](https://img.shields.io/badge/GitHub-仓库-181717)](https://github.com/araea/acumen)

## 安装

需要 Rust 1.94 或更高版本。

```sh
cp config.example.toml config.toml
cargo build --release --locked
./bot start
```

默认连接 `http://127.0.0.1:3001`。启动后日志显示控制台地址，默认监听 `127.0.0.1:7801` 并使用口令保护。控制台通过 `[console]` 配置；`--no-ui` 关闭本次运行的 Web 界面。

网页截图与部分图片插件需要本机安装 Chrome 或 Chromium，可在配置中指定 `browser_path`。浏览器不可用时回退到文本输出。中文图片需要系统中日韩字体。

## 快速使用

首次启动补全缺失的默认配置；配置解析失败时不会覆盖原文件。不要将 `config.toml` 或 `data/` 提交到 Git。

在 `[ctl]` 中设置 `admins`，才能从群聊管理插件。首次启动前完成设置；名单为空时仅本机控制台可管理。发送 `/help` 查看指令，发送 `/ctl` 查看插件状态、开关和配置。

```sh
./bot status
./bot stop
./bot restart
./bot ui       # 打开控制台
./bot ui url   # 只打印地址
```

Termux 用户可用 `termux-services` 将进程交给 runit 托管。部署、更新和守护配置见[插件控制与部署](docs/CONTROL.md)。

## 配置

连接实现端在 `config.toml` 的 `[[bots]]` 中配置，每段一个 `protocol`、`url` 与 `access_token`。`ACUMEN_SATORI_TOKEN` 环境变量优先于 `access_token`。生产目标为本机 [satori-qq](https://github.com/araea/satori-qq)，默认地址 `http://127.0.0.1:3001`。完整配置项见 [`config.example.toml`](config.example.toml)。

权限、控制台与 Agent 控制通道见[插件控制与部署](docs/CONTROL.md)；Satori 接入与兼容范围见[Satori 接入](docs/SATORI.md)。

## 限制 / 风险

浏览器卡片需要 Chrome/Chromium 与系统中日韩字体，出图失败时回退文本。部分能力（视频解析的 1080P、AI 生图 / 生歌 / 生视频）依赖外部服务并可能产生费用，启用前确认账号与价格。Agent 房间的 `bash` 等本机工具不是安全沙箱，具有当前系统账户权限；共享房间不应开放 Agent 控制通道。

## 必要链接

- [Satori 接入与兼容范围](docs/SATORI.md)
- [群聊搭话](docs/ambient.md)
- [内置 Agent 房间](docs/agent.md)
- [视频解析](docs/video_parse.md)
- [插件控制与部署](docs/CONTROL.md)
- [架构与插件开发](docs/ARCHITECTURE.md)
