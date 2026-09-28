# 知微（Acumen）

Rust 编写的群聊机器人，通过 Satori v1 连接实现端，以插件处理群聊、定时任务与 AI 对话

## 安装

需要 Rust 1.94 或更高版本。

```sh
cp config.example.toml config.toml
cargo build --release --locked
./bot start
```

默认连接 `http://127.0.0.1:3001`。启动后日志显示控制台地址，默认监听 `127.0.0.1:7801` 并使用口令保护。控制台通过 `[console]` 配置；`--no-ui` 关闭本次运行的 Web 界面。

网页截图与部分图片插件需要本机安装 Chrome 或 Chromium，可在配置中指定 `browser_path`。浏览器不可用时回退到文本输出。中文图片需要系统中日韩字体。

## 配置

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

## 文档

- [Satori 接入与兼容范围](docs/SATORI.md)
- [群聊搭话](docs/ambient.md)
- [内置 Agent 房间](docs/agent.md)
- [视频解析](docs/video_parse.md)
- [架构与插件开发](docs/ARCHITECTURE.md)

## 测试

```sh
cargo test --locked
cargo build --release --locked
```
