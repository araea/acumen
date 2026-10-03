# 知微（acumen）

群聊机器人：经 Satori 协议接入多个聊天平台，以可插拔插件提供 AI 对话、群管与媒体处理能力

[![GitHub](https://img.shields.io/badge/GitHub-araea%2Facumen-181717?logo=github&logoColor=white)](https://github.com/araea/acumen)

## 安装

需要 Rust 1.94 或更高版本。

```sh
cp config.example.toml config.toml
cargo build --release --locked
./bot start
```

首次启动会补齐缺失的默认配置；配置解析失败时不会覆盖原文件。不要将 `config.toml` 或 `data/` 提交到 Git。

网页截图与部分图片插件需要本机安装 Chrome 或 Chromium，可在配置中指定 `browser_path`；浏览器不可用时回退到文本输出。中文图片需要系统中日韩字体。

## 快速使用

默认连接 `http://127.0.0.1:3001`。在 `[ctl]` 中设置 `admins` 后，才能从群聊管理插件。

```sh
./bot status
./bot stop
./bot restart
./bot ui       # 打开控制台
./bot ui url   # 只打印地址
```

发送 `/help` 查看指令，发送 `/ctl` 查看插件状态、开关和配置。控制台地址、`--no-ui` 与 runit 托管见[插件控制与部署](docs/CONTROL.md)。

## 配置

连接实现端在 `config.toml` 的 `[[bots]]` 中配置，每段一个 `protocol`、`url` 与 `access_token`。`ACUMEN_SATORI_TOKEN` 环境变量优先于 `access_token`。默认地址 `http://127.0.0.1:3001`。

完整配置项见 [`config.example.toml`](config.example.toml)。

## 限制 / 风险

部分能力（视频解析的 1080P、AI 生图 / 生歌 / 生视频）依赖外部服务并可能产生费用，启用前确认账号与价格。

Agent 房间的 `bash` 等本机工具不是安全沙箱，具有当前系统账户权限；共享房间不应开放 Agent 控制通道。

## 必要链接

- [Satori 接入](docs/SATORI.md)
- [群聊搭话](docs/ambient.md)
- [内置 Agent 房间](docs/agent.md)
- [视频解析](docs/video_parse.md)
- [点歌](docs/song.md)
- [Markdown 转图](docs/markdown.md)
- [二维码识别](docs/qr_scan.md)
- [插件控制与部署](docs/CONTROL.md)
- [架构与插件开发](docs/ARCHITECTURE.md)
- [WebUI 设计系统](docs/DESIGN_SYSTEM.md)
