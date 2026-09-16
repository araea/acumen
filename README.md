# 知言

知言（仓库名 `ayjx`）是一个跑在自己机器上的群聊机器人：基于 Rust，通过 Satori v1 协议连接实现端，23 个插件在配置文件里开关与调整。

它有两种形态，同一份核心：

- **终端**：`cargo build --release --locked` 出来的那个可执行文件，前台或交给 runit 常驻，没有图形界面也照常跑。
- **应用**：`app/` 里那份 Android 壳（显示名「知言」），把同一个核心跑在应用内，界面是核心自己发的本机网页——运行状况、插件、搭话、日志都在上面。

两种形态不互斥：应用可以自带核心，也可以只当一块屏幕去连已经在跑的那一份。差别见[应用形态](docs/APP.md)。

名字取自《孟子·公孙丑上》「我知言，我善养吾浩然之气」——知言是听得懂话里的意思。这台机器人在群里做的两件事正好是它：听懂大家在说什么再开口，以及从一个人的话里读出一个人的样子。仓库名、可执行文件名与 `./bot` 脚本都还叫 `ayjx`，改的只是给人看的那个名字。

它与接入层那两个名字是一家人：QQ 进程里那个实现端模块叫[**知弦**](https://github.com/araea/satori-qq)（弦是把它接上的那根），这里叫**知言**（言是它开口说的那句）。

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

```sh
./bot ui        # 打开控制台（地址取自启动时写下的那一份）
./bot ui url    # 只打印地址，不打开
```

## 配置

`config.toml` 不提交到 Git。首次启动会写入缺少的默认字段，配置解析失败时不覆盖原文件。

- `command_prefix`：指令前缀，默认 `/`
- `global_filter`：全局群黑白名单
- `[[bots]]`：Satori 连接实现端，`console` 用于本地测试
- `access_token`：Satori 令牌，也可以由 `AYJX_SATORI_TOKEN` 提供
- `[ctl]`：插件控制权限，`admins` 填维护者 QQ 号
- `[console]`：本机控制台，默认只绑 `127.0.0.1:7801`
- `[oai]`：可选的 OAI 与内置 Agent 设置
- `[ambient]`：群聊搭话设置，复用 `[oai]` 的模型、接口与联网配置

数据库文件是 `data/bot.db`。数据目录默认在可执行文件旁边，也可以用环境变量 `AYJX_DATA_DIR` 挪到别处（Android 应用里就是这么做的）。

## 插件与运行

插件放在 `src/plugins/`，清单在 `src/plugins/registry.rs`；新增插件只改这一处，`/help`、`/ctl` 与控制台会自动包含它。发送 `/help` 查看指令；`/ctl`（别名 `/控制`、`/插件`）用于查看和修改插件开关与配置。两者默认以卡片图作答，把 `image_enabled` 设为 false 可以改回纯文本。首次使用前需要停机设置 `[ctl].admins`，列表为空时只允许本机控制台管理。

```sh
./bot status
./bot stop
./bot restart
```

Termux 下 `./bot start` 会取得唤醒锁；`./bot logs` 用 tmux 窗口跟运行日志，`./bot attach` 进入。把 bot 交给 `termux-services`（runit）托管后，进程崩溃会自动重启，`start` / `stop` / `restart` 自动改走 `sv`，见[插件控制](docs/CONTROL.md)。

## 打包成应用

```sh
bash app/build.sh            # 产出 app/build/Zhiyan.apk
su -c "cp app/build/Zhiyan.apk /data/local/tmp/ && pm install -r /data/local/tmp/Zhiyan.apk"
```

构建脚本用本机的 `aapt` / `d8` / `zipalign` / `apksigner` 手工打包，没有 Gradle；Rust 核心交叉编译成 `arm64-v8a` 的 `libayjx_core.so` 随包走。前置条件、装机步骤与两种运行模式见[应用形态](docs/APP.md)。

## 文档与测试

- [设计规范总纲](docs/GUIDELINES.md)
- [交互规范](docs/INTERACTION.md)
- [文案规范](docs/CONTENT.md)
- [统一度审计](docs/UNIFORMITY.md)
- [架构说明](docs/ARCHITECTURE.md)
- [架构与渲染审计](docs/AUDIT.md)
- [插件控制](docs/CONTROL.md)
- [应用形态](docs/APP.md)
- [Satori 接入](docs/SATORI.md)
- [内置 Agent 房间](docs/agent.md)
- [群聊搭话](docs/ambient.md)
- [用户画像](docs/portrait.md)
- [视频解析](docs/video_parse.md)

```sh
cargo test --locked
cargo build --release --locked
```
