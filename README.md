# 知微（acumen）

群聊机器人。经 Satori 接入 QQ，用插件提供 AI 对话、群管与媒体处理。

## 安装

一台已 root（KernelSU）的安卓手机，装好[知弦（satori-qq）](https://github.com/araea/satori-qq)模块并登录 QQ。在 Termux 里运行：

```sh
curl -fsSL https://github.com/araea/acumen/releases/latest/download/install.sh | bash
```

连不上 GitHub 时加一个加速前缀：

```sh
curl -fsSL https://gh-proxy.com/https://github.com/araea/acumen/releases/latest/download/install.sh | bash
```

脚本把最新版本装到 `~/acumen`，装好依赖、浏览器与中文字体，然后启动并打开控制台。第一次要下载几百 MB。Termux 从 F-Droid 或 GitHub 安装，并在 KernelSU 里给 Termux 授予 root。

端口与令牌不用填，跟随知弦模块的设置。装了[知言（satori-wx）](https://github.com/araea/satori-wx)的，同时接入微信。

## 使用

| 命令 | 用途 |
| --- | --- |
| `acumen` | 启动并打开控制台；已在运行就只打开控制台 |
| `acumen stop` | 停止 |
| `acumen restart` | 重启 |
| `acumen status` | 运行状态与连接情况 |
| `acumen log` | 实时日志，`Ctrl+C` 退出 |
| `acumen update` | 更新到最新版本 |
| `acumen help` | 全部命令 |

控制台默认在 `http://127.0.0.1:7800`（`acumen ui` 打开）。知微停着时也能打开，可以在页面上启动、停止、重启、更新。添加到主屏幕后像一个应用。

QQ 里发 `/help` 看指令，`/ctl` 看插件状态、开关与配置。要让群管理员也能改配置，在控制台的插件页把 QQ 号加进 `admins`。

## 功能

- AI 对话与群聊搭话：内置房间「管家大人」，会用终端与联网；自建房间默认只聊天，按需开工具。
- 群管：自动回复、潜水清理、入群欢迎、群精华、群头衔与假头衔（把「群主 / 管理员」戴到机器人头上，可自动续报）。
- 媒体：B 站视频解析、点歌、二维码识别与生成、Markdown 转图。
- 其它：奶龙提醒、群统计、词云。

AI 功能要自备模型服务的密钥。在控制台的插件页填，或在任意对话里发 `oai deepseek sk-…`。

## 卸载

```sh
acumen uninstall          # 保留配置与数据
acumen uninstall --purge  # 一起删除
```

## 限制

只提供安卓 Termux（arm64）的发布版。

视频解析的高画质、AI 生图生歌生视频依赖外部服务，可能产生费用。

全量档房间的终端工具不是沙箱，有 Termux 账户的权限。

更新记录见 [CHANGELOG.md](CHANGELOG.md)。
