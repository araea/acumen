# 插件控制与部署

`/help` 查看功能与状态，`/ctl` 管理插件开关和配置。中文别名为 `/控制`、`/插件`。

## 权限与初始配置

在停止 Acumen 后编辑 `config.toml`：

```toml
[ctl]
enabled = true
admins = [123456789]
```

`admins` 填维护者 QQ 号。名单为空时，只有本机 Web 控制台可以管理；群管理员身份不会自动获得全局权限。帮助、`/ctl` 用法和插件状态对所有人开放。关闭 `ctl` 不会绕过其他指令的权限检查。

修改配置文件前先运行 `./bot stop`。运行中的实例退出时会将内存配置写回文件，在线编辑可能被覆盖。配置解析失败时不会覆盖原文件。

## 常用指令

默认前缀为 `/`。修改 `command_prefix` 后使用新前缀；空数组表示无前缀。

| 指令 | 用途 |
| --- | --- |
| `/ctl list` | 查看插件及全局开关 |
| `/ctl list on`、`/ctl list off` | 按开关状态筛选 |
| `/ctl list <名称>` | 按英文名或中文名筛选 |
| `/ctl on help echo` | 同时启用多个插件 |
| `/ctl show oai model_filter` | 查看配置项 |
| `/ctl defaults help` | 查看默认值 |
| `/ctl set help image_enabled false` | 修改配置项 |
| `/ctl set repeater channel.white [123456, 789012]` | 替换数组 |
| `/ctl set repeater channel { white = [123456], black = [] }` | 替换表 |
| `/ctl diff help` | 查看与默认值的差异 |
| `/ctl reset help --confirm` | 恢复插件默认值，保留开关 |

操作名支持列表、状态、开启、启用、关闭、禁用、查看、默认、设置、重置和差异。插件名英文不区分大小写；中文名需完全匹配。数组和表使用 TOML 语法；字符串中的空格无需转义。路径以点分隔，数组索引从 0 开始。

`set` 会按真实配置类型校验。保存使用临时文件和原子替换；校验或写入失败不会留下部分修改。批量开关会先验证全部名称，再统一写入。敏感字段在回复中隐藏；视频解析的 B 站 `cookie` 不在敏感字段列表中，**不要在群聊或 Agent 房间里设置 Cookie**。请停止机器人后在本机编辑配置文件。

配置升级时只补缺失字段，不覆盖已有值，也不自动删除旧字段。某些插件带启动初始化钩子；运行时启用此类插件会标记「待重启」，重启后才开始工作。已开始的请求和任务不会因关闭开关而强制取消。

## 群名单与模型列表

`repeater`、`webshot`、`stats` 等插件使用相同的群黑白名单规则：

- 两个列表都空：所有群生效。
- 只有 `black` 有值：除黑名单外的群生效。
- `white` 非空：只对白名单中的群生效。
- 同时在两个列表中：黑名单优先。

`[global_filter]` 是所有插件之前的独立过滤层。主动推送必须同时通过全局规则和插件名单。

`/%` 模型列表由 `[oai].model_filter` 控制。`keep` 保留匹配任一关键字的模型，留空表示不过滤；`drop` 命中即剔除，优先级高于 `keep`。关键字不区分大小写，按子串匹配。

```text
/ctl set oai model_filter.keep ["gpt-5.6", "claude-opus-5"]
/ctl set oai model_filter.drop ["-lite", "-2026-"]
```

## Agent 控制通道

`[ctl].agent_control` 默认开启时，Agent 房间可以运行 `acumen --ctl` 修改插件配置。Acumen 为每轮对话提供一次性凭据，最长有效 30 分钟，不写入磁盘或命令行；操作会记录在日志中。

**Agent 房间中的任何成员都可能借此修改机器人配置。** 该功能不提供成员级权限隔离；Agent 的本机 `bash` 工具也不是沙箱。共享房间不应开放这项能力。可用 `/ctl set ctl agent_control false` 关闭控制通道；要限制整个房间的工具权限，需修改 Agent 工具白名单。

此通道只修改 `config.toml` 中的插件配置，不管理连接凭据、群名单、数据库业务数据或 OAI 独立配置。不要通过通道传递 Cookie 或其他未被脱敏的凭据。

## 本机 Web 控制台

控制台与命令行共用配置写入路径，提供运行状态、插件配置、搭话内容、日志和全局接入设置。默认只监听 `127.0.0.1:7801`，API 需要口令；静态资源不需要口令。

```toml
[console]
enabled = true
bind = "127.0.0.1"
port = 7801
token = ""
log_lines = 400
```

`token` 留空时首次启动自动生成，保存到 `data/console/token`（权限 `0600`）；启动日志会输出含口令的 URL。**不要将 `bind` 改为 `0.0.0.0`，除非已确认网络隔离与口令保护足够。** `--no-ui` 只关闭本次运行的 Web 界面，不影响机器人指令、定时任务和推送。

```sh
./bot ui       # 打开控制台
./bot ui url   # 只打印地址
```

控制台可安装到手机或桌面浏览器。日志仅在日志页可见时接收，页面离开或进入后台会暂停订阅，返回时读取最近记录。控制台操作不会直接发送群消息。

## 运行与更新

推荐按以下顺序部署：

1. 运行 `cargo test --locked` 和 `cargo build --release --locked`。
2. 运行 `./bot stop` 并等待进程退出。
3. 备份配置和数据，再修改 `config.toml`。
4. 从仓库目录运行 `./bot start`。
5. 查看连接日志，再用 `/ctl list` 检查插件状态。

Linux / Termux 下，`./bot start` 前台运行并显示日志；按 `Ctrl+C` 优雅停止。常用命令：

| 命令 | 用途 |
| --- | --- |
| `./bot status` | 查看运行状态与 PID |
| `./bot start` | 启动；受 runit 管理时等同 `sv up` |
| `./bot stop` | 优雅停止；受 runit 管理时等同 `sv down` |
| `./bot restart` | 重启 |
| `./bot logs` | 查看实时日志 |
| `./bot attach` | 进入日志窗口 |

Termux 可通过 `termux-services`（runit）托管。服务脚本位于 `scripts/service/acumen/`；将它们复制到 `$PREFIX/var/service/acumen/`，并把 `run` 中的仓库路径改为绝对路径。托管后可用 `./bot enable` / `./bot disable` 控制随 Termux 启动。

Android 厂商可能直接结束 Termux，runit 无法恢复被结束的宿主应用。`scripts/97-termux-revive.sh` 和 `scripts/termux-revive.sh` 提供 root 侧恢复方案，安装步骤与状态检查见脚本注释。

浏览器卡片需要 Chrome/Chromium 与系统中日韩字体。出图失败时帮助和控制回退到文本；运行浏览器类插件时可通过全局 `browser_path` 指定浏览器。
