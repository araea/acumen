# 插件控制与部署

`/help` 查看功能与状态，`/ctl` 管理插件开关与配置；中文别名为 `/控制`、`/插件`

## 权限与初始配置

在停止 Acumen 后编辑 `config.toml`：

```toml
[ctl]
enabled = true
admins = [123456789]
```

**号主不用写进来**：登录账号本人在 QQ 客户端里亲手发的消息（satori-qq 标着 `manual_self`）自动有全部管理权限；机器人自己发出去又回声进来的那一份不算，即使名单里写着登录号。`admins` 只填号主之外、也要能管的人的 QQ 号。名单为空时只有号主与本机 Web 控制台可以管理；群管理员身份不会自动获得全局权限。帮助、`/ctl` 用法与插件状态对所有人开放；关闭 `ctl` 不会绕过其他指令的权限检查。

直接编辑文件前先运行 `acumen stop`。运行中的实例退出时会把内存配置写回文件，在线编辑可能被覆盖；解析失败时不会覆盖原文件。

## 指令

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

操作名支持列表、状态、开启、启用、关闭、禁用、查看、默认、设置、重置与差异。插件名英文不区分大小写，中文名需完全匹配。数组与表使用 TOML 语法，字符串中的空格无需转义；路径以点分隔，数组索引从 0 开始。

`set` 按真实配置类型校验。保存使用临时文件与原子替换；校验或写入失败不会留下部分修改。批量开关先验证全部名称再统一写入。敏感字段（含 Cookie）在回复中隐藏；凭据请在本机配置，避免在群聊或 Agent 房间发送原始值。

配置写入只补缺失字段，不覆盖已有值，也不删除多余字段。首次启用自动初始化；连接钩子在下次连接时执行，定时排期修改也在下次连接后生效。已开始的请求与任务不会因关闭开关而强制取消。

## 名单与模型列表

`repeater`、`webshot`、`stats` 等插件使用相同的群黑白名单规则：

- 两个列表都空：所有群生效
- 只有 `black` 有值：除黑名单外的群生效
- `white` 非空：只对白名单中的群生效
- 同时在两个列表中：黑名单优先
- 私聊不受白名单约束；`webshot` 的黑名单可以点名私聊会话（对方的账号或频道 ID），例如把微信支付 `gh_3dfda90e39d6` 排除掉

`[global_filter]` 是所有插件之前的独立过滤层，语义与插件 `channel` 不同：开启白名单时只允许名单内的群，空白名单禁止所有群，并忽略黑名单；只有关闭白名单时才检查启用的黑名单。两个开关都关闭则允许所有群。主动推送必须同时通过全局规则与插件名单。

`/%` 模型列表由 `[oai].model_filter` 控制。`keep` 保留匹配任一关键字的模型，留空表示不过滤；`drop` 命中即剔除，优先级高于 `keep`。关键字不区分大小写，按子串匹配。

```text
/ctl set oai model_filter.keep ["gpt-5.6", "claude-opus-5"]
/ctl set oai model_filter.drop ["-lite", "-2026-"]
```

## Agent 管控（`acumen_plugins` 工具）

内置 Agent（如「管家大人」）有一件原生工具 `acumen_plugins`，等价于手打 `/ctl`：看插件清单（带一句话说明）、看配置（密钥自动隐藏）、开关插件、改配置、恢复默认、看版本与待重启的插件，以及让机器人过几秒重启。改动走同一条 ctl 事务（校验、先落盘再生效、`ctl` 保命规则），操作记在日志「管家改机器人设置」里。

谁能动手：

| 动作 | 谁 |
| --- | --- |
| `list` `show` `defaults` `diff` `status` | 任何人 |
| `enable` `disable` `set` `reset` | 号主；号主以外的人取决于 `[ctl].guest_control`（默认开）。`[ctl].agent_control` 关掉则所有人都只能看 |
| `restart` | 只有号主 |

「号主」指 `[ctl].admins` 里的人，或账号自己亲手打的字（号主就在这个账号上打字；机器人自己发出去、作为回声回来的话不算，否则任何能让机器人开口的人都成了号主）；身份由平台上报的发送者核出，Agent 每轮都会收到一条系统注记，用户消息里自称号主不算数。

无论谁问都不开放的位置：任何名字带 `token` / `api_key` / `secret` / `password` / `cookie` 的配置键，整个 `ctl` 与 `console` 配置，`oai` 里的 `providers` 与 `search.backends`；也不能停用 `oai` / `ctl` / `console`（停掉它们等于让管家或管理入口自己失联）。这些要改，请到本机控制台或用 `/ctl`。

群聊搭话（ambient）里同一件工具只在号主叫它办事的那一轮开放写入，其余时候只读。

Agent 的本机 `bash` 工具不是沙箱；这件工具不是能力的边界，而是让常见操作有一条带权限判断、可审计的正路。可用 `/ctl set ctl agent_control false` 一键收回；要限制整间房的工具，改 `[oai.chat].tools` 白名单。

## 本机 Web 控制台

控制台与命令行共用配置写入路径，提供运行状态、插件配置、搭话内容、日志与全局接入设置。默认只监听 `127.0.0.1:7801`，API 需要口令；静态资源不需要口令。

```toml
[console]
enabled = true
bind = "127.0.0.1"
port = 7801
panel_port = 7800
token = ""
log_lines = 400
```

`token` 留空时首次启动自动生成，保存到 `data/console/token`（权限 `0600`）；启动日志会输出含口令的 URL。不要把 `bind` 改为 `0.0.0.0`，除非已确认网络隔离与口令保护足够。`--no-ui` 只关闭本次运行的 Web 界面，不影响机器人指令、定时任务与推送。

```sh
acumen ui       # 打开管理页（面板在就开面板，否则直接开控制台）
acumen ui url   # 只打印地址
```

名单（设置页的全局黑白名单、插件配置里的 `channel.white / black`、各类「开启的群」，以及资讯推送的私聊目标、管理员 `ctl.admins` 这类私聊用户名单）在控制台里不用手打引号：从已加入的群 / 好友里点选（带名字与搜索，好友还能按昵称、备注找），或整段粘贴 ID；移除可撤销，群名单底下说明它现在的实际效果。名册来自实现端的 `guild.list` 与 `friend.list`，只有连着的账号读得到，读不到时仍可直接输入群号或账号。

控制台可安装到手机或桌面浏览器。日志仅在日志页可见时接收，页面离开或进入后台会暂停订阅，返回时读取最近记录。控制台操作不会直接发送群消息。

有新版本时顶栏右侧出现「更新」（只在有新版本时出现），点开看更新说明，确认后下载、校验、换上并原地重启，等价于 `acumen update`；从源码构建的实例不显示。

### 管理面板

控制台长在机器人进程里，机器人停了页面也跟着没了。管理面板是同一个可执行文件的另一种跑法（`acumen --panel`），由 runit 单独托管（`$PREFIX/var/service/acumen-panel`），默认监听 `127.0.0.1:7800`、口令与控制台相同：

- 知微在跑时，面板把 `/api/*` 原样转给控制台（日志流边收边转），看到的就是控制台，运行卡上多一颗「停止」。
- 知微停着时，同一张页面换成「已停止」：运行卡给出原因（主动停止 / 进程不在 / 进程在但不应答），一颗「启动」（或「重启」），下面是 runit 收的终端日志；有新版本时顶栏照样出现「更新」。
- 起停与更新调的是管理命令 `acumen start / stop / restart / update`，在自己的进程组里跑，页面显示进度与最后几行输出；跑完、知微回来后自动换回控制台。
- 可执行文件被更新换掉后，面板在空闲时自行退出，由 runit 用新版本拉起。
- `acumen stop` 只停知微、不停面板；root 守护在知微停着而面板开着时也会拉起 runit 监督树，所以重启手机后管理页照样打得开。`panel_port = 0` 关掉面板。

总览页运行卡底部有一颗「重启」按钮，等价于 `acumen restart`：先确认，进程随后原地重启，页面轮询到新进程的启动时间后自动刷新。`[restart]` 关闭时按钮会直接说明原因。

## 运行与更新

安装后的布局：

| 位置 | 内容 |
| --- | --- |
| `~/acumen/acumen` | 可执行文件；`ACUMEN_HOME` 可换目录 |
| `~/acumen/config.toml`、`~/acumen/data/` | 配置与数据 |
| `$PREFIX/bin/acumen` | 管理命令，随可执行文件一起更新 |
| `$PREFIX/var/service/acumen/` | runit 服务；日志在 `$PREFIX/var/log/sv/acumen/`，每份 1 MB、留 5 份 |
| `$PREFIX/var/service/acumen-panel/` | 管理面板（`acumen --panel`）；日志在 `$PREFIX/var/log/sv/acumen-panel/` |
| `/data/adb/service.d/acumen.sh` | root 守护，开机由 KernelSU / Magisk 执行；日志 `/data/local/tmp/acumen-keeper.log` |

| 命令 | 用途 |
| --- | --- |
| `acumen` / `acumen start` | 启动并打开管理页；`-q` 不打开 |
| `acumen stop` | 停止，并留下停止标记：开机与守护都不会再拉起 |
| `acumen restart` | 重启 |
| `acumen status` | 运行状态、连接、保活与新版本 |
| `acumen log [行数]` | 实时日志 |
| `acumen update` | 立即检查并更新，运行中的会随即重启 |
| `acumen setup` | 重新检查依赖、字体、省电白名单与 root 守护，可重复运行 |
| `acumen run` | 前台运行并启用本地终端适配器，可直接输入指令测试 |
| `acumen uninstall [--purge]` | 卸载 |

保活分三层：

1. runit 托管机器人进程，崩溃立即拉起；一起来就退的（如配置写坏）每次歇 20 秒，不空转。
2. 启动时持有 Termux 唤醒锁；setup 把 Termux 加入省电白名单、放开后台限制。
3. root 守护每分钟看一次 runit 监督树：Termux 被系统清理后经 `RUN_COMMAND` 在 Termux 里重新拉起，必要时打开一次 Termux 界面；唤醒锁或省电白名单被系统收回时补上。

`acumen stop` 与系统设置里的「强行停止」都算主动停止，守护不干预。设有锁屏密码时，重启后要先解锁一次：解锁前 Termux 的目录处于加密状态，谁也起不来。

例行维护由 `[restart]` 控制：在 `times` 的每个时刻先查新版本（`auto_update`），有就下载、校验、替换可执行文件；再等到 `quiet_seconds` 秒内没有新消息、手头的消息都处理完（最多等 `max_wait_minutes` 分钟）后重启。没有新版本时，`daily_restart` 决定是否照常重启。每次启动（含重启）半分钟后也查一次新版本，有才换上并同样平滑重启；十分钟内查过的就不再查，例行维护后的那次重启、崩溃后被反复拉起都不会重复去查。内存超过 `memory_threshold_mb` 时同样平滑重启。重启在进程内原地完成，PID 不变。

从源码构建的（可执行文件在 `target/` 下）不参与自动更新。开发时把构建产物换进去：

```sh
cargo build --release --locked && cp target/release/acumen ~/acumen/acumen && acumen restart
```

发布新版本用 `scripts/release.sh`：按 `Cargo.toml` 的版本构建、打包，并把 `CHANGELOG.md` 对应一节作为说明发到 GitHub Releases。

卡片与截图用安装时装好的 Termux Chromium 与思源黑体 / 宋体；`browser_path` 可指定别的浏览器。出图失败时帮助与控制回退到文本。

信息卡出图成功后只发卡片图，出图失败时改发可复制、可读屏的等价文本。OAI 删除或清空会先显示影响范围，60 秒内重发同一条指令才执行；不重发即取消。
