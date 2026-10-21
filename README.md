# 知微（acumen）

群聊机器人：经 Satori 协议接入多个聊天平台，以可插拔插件提供 AI 对话、群管与媒体处理能力

[![GitHub](https://img.shields.io/badge/GitHub-araea%2Facumen-181717?logo=github&logoColor=white)](https://github.com/araea/acumen)

## 安装

需要一台已 root（KernelSU）的安卓手机，装好 [知弦（satori-qq）](https://github.com/araea/satori-qq) 模块并登录 QQ。在 Termux 里运行：

```sh
curl -fsSL https://github.com/araea/acumen/releases/latest/download/install.sh | bash
```

它会下载最新版本到 `~/acumen`，装好浏览器、中文字体与其余依赖，配好保活，然后启动并打开控制台。第一次要下载几百 MB，耗时几分钟。Termux 请从 F-Droid 或 GitHub 安装，并在 KernelSU 管理器的「超级用户」里给 Termux 授权。

连接不用配置：端口与令牌跟随知弦模块里的设置。装了知言（satori-wx）模块的，会同时接入微信。

## 使用

| 命令 | 用途 |
| --- | --- |
| `acumen` | 启动并打开管理页；已在运行就只打开管理页 |
| `acumen stop` | 停止；开机与守护都不会再拉起，直到下次启动 |
| `acumen restart` | 重启 |
| `acumen status` | 运行状态、QQ 连接与保活情况 |
| `acumen log` | 实时日志，`Ctrl+C` 退出 |
| `acumen update` | 立即检查并更新 |
| `acumen help` | 全部命令 |

管理页（`acumen ui`，默认 `http://127.0.0.1:7800`）在知微停着时也打得开：可以直接在网页上启动、停止、重启和更新，不必回到 Termux 敲命令。添加到主屏幕后就像一个应用。

在 QQ 里发送 `/help` 查看指令，`/ctl` 查看插件状态、开关和配置。在群里改配置要先成为管理员：在控制台的插件页打开「插件控制」，把自己的 QQ 号加进 `admins`。

## 保活与更新

- 进程崩溃由 Termux 里的 runit 立即拉起。
- 熄屏时持有 Termux 唤醒锁；Termux 加入系统省电白名单。
- root 守护在开机后拉起 Termux 与机器人，Termux 被系统清理后再拉回来。设有锁屏密码时，重启后要先解锁一次。
- 每天 04:30 例行维护：有新版本就下载、校验、换上，等群里安静两分钟后重启。每次启动或重启后也会查一次，之后每 6 小时再查一次（`check_interval_hours`），有新版本同样换上。时刻与开关在 `[restart]`。
- 群里发 `/版本` 看当前版本与有没有新版，`/查更新` 查一次并看更新说明，`/更新` 直接换上并平滑重启——后两条只有号主与管理员能用。

`acumen stop` 与在系统设置里强行停止 Termux 都算主动停止，守护不会再拉起。

## 配置

配置在 `~/acumen/config.toml`，数据在 `~/acumen/data/`。日常修改用控制台或 `/ctl`；直接编辑文件前先 `acumen stop`，运行中的实例退出时会写回内存里的配置。完整配置项见 [`config.example.toml`](config.example.toml)。

AI 对话、搭话等功能需要自备模型服务的 API 密钥，在控制台的插件页填写。

## 卸载

```sh
acumen uninstall          # 保留配置与数据
acumen uninstall --purge  # 一起删除
```

## 限制 / 风险

只提供安卓 Termux（arm64）的发布版。

部分能力（视频解析的 1080P、AI 生图 / 生歌 / 生视频）依赖外部服务并可能产生费用，启用前确认账号与价格。

Agent 房间的 `bash` 等本机工具不是安全沙箱，具有 Termux 账户权限；共享房间不应开放 Agent 控制通道。

## 必要链接

- [插件控制与部署](docs/CONTROL.md)
- [Satori 接入](docs/SATORI.md)
- [群聊搭话](docs/ambient.md)
- [内置 Agent 房间](docs/agent.md)
- [视频解析](docs/video_parse.md)
- [点歌](docs/song.md)
- [Markdown 转图](docs/markdown.md)
- [二维码识别与生成](docs/qr_scan.md)
- [奶龙拦截](docs/nailong.md)
- [架构与插件开发](docs/ARCHITECTURE.md)
- [WebUI 设计系统](docs/DESIGN_SYSTEM.md)
- [更新记录](CHANGELOG.md)
