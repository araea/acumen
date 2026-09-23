# 插件控制 ctl

`help` 负责说明和状态展示，`ctl`（中文别名 `控制`、`插件`）负责统一管理。

## 初次配置

在 acumen 停止运行时编辑 `config.toml`：

```toml
[ctl]
enabled = true
admins = [123456789] # 维护者 QQ 号，可填多个
```

控制台适配器可以直接管理。QQ 里只有上述全局管理员能查看配置或修改全局状态，群管理员身份不会自动获得全局权限。`admins` 为空表示仅允许本机控制台管理。`/ctl` 用法、`/ctl list` 状态和 `/help` 帮助对所有人开放。权限检查不依赖 `ctl.enabled`，关闭 ctl 也不会让 `/restart` 失去权限检查。

## 常用命令

下列命令使用默认 `/` 前缀。修改 `command_prefix` 后使用相应前缀，空数组表示无前缀。

| 指令 | 功能 |
| --- | --- |
| `/ctl` | 完整用法 |
| `/ctl list` | 所有注册插件及全局开关 |
| `/ctl list on`、`/ctl list off` | 按开关状态筛选 |
| `/ctl list 统计` | 按英文名或中文名筛选 |
| `/ctl on help echo` | 一次开启多个插件 |
| `/插件 关闭 复读机,网页截图` | 中文名与逗号分隔也可用 |
| `/ctl show repeater` | 当前完整配置 |
| `/ctl show oai model_filter` | 查看嵌套字段 |
| `/ctl defaults help` | 查看默认值 |
| `/ctl set help image_enabled 关` | 设置布尔值 |
| `/ctl set oai plain_text_max_chars 120` | 设置数字 |
| `/ctl set repeater channel.white [123456, 789012]` | 替换数组 |
| `/ctl set repeater channel.white.0 456789` | 修改已有数组元素，索引从 0 开始 |
| `/ctl set repeater channel { white = [123456], black = [] }` | 替换整张表 |
| `/ctl set wordcloud font_family Noto Sans CJK SC` | 字符串可含空格 |
| `/ctl diff help` | 与默认值比较 |
| `/ctl set ctl image_enabled 关` | 让 ctl 只回纯文本，不出卡片图 |
| `/ctl reset help image_scale --confirm` | 恢复某项默认值 |
| `/ctl reset help --confirm` | 恢复插件参数，保留其开关 |

中文操作名：列表、状态、开启、启用、关闭、禁用、查看、默认、设置、重置、差异。英文插件名忽略大小写，中文显示名完全匹配。数组与表使用 TOML 语法，空字符串写 `""`，清空数组写 `[]`，带引号的字符串按 TOML 解码。配置路径以点分隔，只能修改已有路径，数组元素必须已经存在，空数组先整体设置。词云的可选 `font_path`、`font_family` 默认省略，可以直接用 `set` 添加，整插件 `reset` 会移除它们。

## 群名单：黑名单与白名单

`repeater`、`webshot`、`stats` 共用同一套 `channel` 子表，语义完全一致：

| 配置 | 效果 |
| --- | --- |
| 两个都留空 | 对所有群生效 |
| 只填 `black` | 除名单内的群以外都生效（`stats` 的定时推送同样跳过名单内的群） |
| 只填 `white` | 只有名单内的群生效并推送 |
| 两个都填 | 黑名单优先，同时出现在两边的群按禁止处理 |

私聊不受群名单约束，`stats` 的「我的」「跨群」查询在私聊里照常可用。

```text
/ctl set stats channel.black [123456789]
/ctl set webshot channel.white [123456789, 987654321]
/ctl set stats channel { white = [], black = [] }
```

这一份是插件自己的名单，与 `config.toml` 顶层的 `[global_filter]` 是两道独立的过滤：主动推送要同时通过全局过滤和插件名单才会发出。

## 模型列表过滤

`/%` 展示的模型来自 `[oai].model_filter`。中转站一次可以返回上千个 id，其中大多是历史快照、小参数量档位和语音视频等与聊天无关的条目，默认规则只保留当前可用的旗舰对话与图像模型。

| 字段 | 作用 |
| --- | --- |
| `keep` | 只保留命中任一关键字的模型；留空表示不筛选 |
| `drop` | 命中即剔除，优先于 `keep` |

两份关键字都不区分大小写，按子串匹配，写到「系列」粒度即可。站点上新或下架时修改配置即可，不必改代码，`/%` 每次都按当前规则重新拉取。

```text
/ctl show oai model_filter
/ctl set oai model_filter.keep ["gpt-5.6", "claude-opus-5", "gemini-3.8-flash"]
/ctl set oai model_filter.drop ["-2026-", "-lite"]
```

模型列表按厂商分区展示，Midjourney 的绘图模型不走过滤，始终附在列表末尾。

## 保存、权限与生效时间

- 批量开关全部验证通过才写入，任何插件名错误或保留入口检查失败都不会产生部分修改
- 通过真实插件配置类型检查数组元素和整数范围，并检查概率、图片倍率、时间等常用约束。拦下的是**这一次编辑自己写出来的**出入：写错键名报 `未知配置键：channel.whiet`，整段替换时丢掉已有的键报 `缺少配置键：channel.white`。配置文件里本来就带着的出入不拦：改过名或已删掉的旧键（`[webshot] block_login_walls` 现在叫 `block_walled_sites`）、手工删掉的新键，都放过。二进制读配置时本来就容忍这两类——缺的键按 serde 默认值补齐，不认识的键忽略——它们不影响运行，却会挡住一次与它们无关的修改，而它们在界面上既删不掉也补不出来
- 数字写进小数位时按落点的类型收一下：网页的输入框分不出 `1` 与 `1.0`（`JSON.stringify(3.0)` 就是 `"3"`），`image_scale`、`probability` 这类字段收到整数会当作整数写回去，改成 3 就会被自己的类型检查判错。这条只收这一种情况，别的类型不匹配照旧拒绝
- 配置先写入同目录临时文件，同步后原子替换，成功后才发布到内存。保存失败保留原内存配置，并发修改通过同一把锁串行提交
- 启动时按插件默认值补全 `config.toml` 里缺失的字段，嵌套表里的也补，只补空缺、从不覆盖已有取值。`/ctl` 的路径解析走不进不存在的键，所以升级带来的新开关如果没有被补出来，运行时靠 serde 默认值照常工作，管理员却改不到它，`[ambient.peak]` 这种嵌套新表就属于这一类。补全会写回配置文件并记日志，补过一次之后不再重写。补全只加不删：改过名或已删掉的旧键会一直留在文件里，`/ctl diff` 把它们列成「额外项」，它们不参与运行也不拦人；要清掉用整插件 `reset`（保留 `enabled` 与 ctl 的 `admins`）或停机手改
- ctl 位于日志与消息记录插件之前，控制指令不会被这些插件记录，也不会被业务插件消费。回复隐藏名称含 `token`、`secret`、`password`、`api_key` 等的字段，成功回执不复述输入值。敏感参数与管理员列表请在私聊或本机控制台设置
- 全局开关影响所有适配器和会话。关闭后下一条事件不再进入该插件。`recorder`、`stats`、`ai_news`、`restart` 的后台任务在后续触发时检查总开关。已经开始执行的请求或任务不会强制取消
- 无生命周期钩子的插件可以直接开关。带 `init` / `on_connected` 的插件如果启动时未开启，运行时开启会标注「待重启」，在重启完成初始化前不会进入消息处理。初始化参数、定时排期、推送间隔等在重启后完整生效，实时读取的参数下一次使用时生效
- ctl 不允许通过聊天关闭自身，也不允许管理员通过聊天移除自己的权限。整插件 `reset` 保留 `enabled`，重置 ctl 还保留 `admins`
- `/restart` 需要全局管理员，且 `restart.allow_manual_restart = true`。定时与手动重启都只向主循环提出请求，由主循环停止任务、关闭数据库与浏览器、保存配置。Unix/Termux 随后 exec 替换当前进程，保留 PID、终端、环境变量、启动参数与单实例锁，重新连接 Satori 前有短暂连接中断。`restart.time` 支持 `HH:MM` 或 `HH:MM:SS`，使用系统本地时区；内存阈值只统计 acumen 自身 RSS（Linux/Android），不含 Chromium 子进程

## 在 agent 房间里用自然语言操作

`[ctl].agent_control`（默认 `true`）让 agent 房间可以直接说「把复读机关掉」「词云的字体调大一点」，由 agent 自己去查、去改、去复核。

一轮 agent 房间对话开始时，acumen 为这一轮签发一次性凭据，随环境变量交给 agent 的工具子进程，并附带说明用法的 `acumen-control` skill。agent 执行 `acumen --ctl "<命令>"`，命令经本机 Unix 套接字回到运行中的实例，由 ctl 以维护者身份执行，回执原样打到 stdout，因此 agent 看得到结果，能据此决定下一条命令。

- 不做身份限制：任何能在 agent 房间里说话的人都能借它操作机器人。这是部署时的明确选择。agent 房间本来就持有全权限 shell，这条通道没有扩大它的能力边界，但确实把「改配置」从管理员专属变成了人人可用
- ctl 自身的保护规则仍然有效：不能通过聊天关闭 ctl，也不能让管理员失去管理入口。这些规则针对误操作，权限不在此列
- 凭据随这一轮对话结束立即作废（最长寿命 30 分钟），只存在于内存与子进程环境变量里，不落盘，也不出现在命令行。套接字是 `data/ctl/control.sock`，权限 `0600`
- 每条经通道执行的命令都按「控制通道执行（QQ号）：命令」记进日志，可以追溯到人
- 群聊搭话（`[ambient]`）里的 agent 不签发 ctl 管理凭据，那是无人触发的自发言，不该带有修改配置的能力
- 收回这份开放有两个层次：`/ctl set ctl agent_control 关` 只关闭这条通道，agent 房间的 shell 仍然存在。真正的边界在 agent 的工具白名单

ctl 操作 `config.toml` 中插件自己的配置。连接凭据、全局过滤规则、数据库中的插件业务数据，以及 oai 独立存储的模型 API 与智能体历史仍由各自的入口管理。例如 oai 的 API、模型与房间操作见 `/oai`，推送目标快捷指令见 `/help ai_news`。

## 本机界面

`[console]` 那个插件在回环地址上发一张网页，是知微唯一的一张界面：运行状况、插件开关与配置、搭话的运行时文本、实时日志，以及接入与全局设置。它同时是一份可以装到桌面的应用——手机与桌面浏览器都能把它加到主屏幕，装上之后没有地址栏，顶栏贴在状态栏下面。

```toml
[console]
enabled = true
bind = "127.0.0.1"   # 只绑回环；改成 0.0.0.0 之前先想清楚口令够不够
port = 7801
token = ""           # 留空即首次启动自动生成，写在 data/console/token（0600）
log_lines = 400     # 上限 2000；环、接口与快照三处都用这个数
```

启动日志里有唯一一条带口令的地址，启动时也会把它写一份在 `data/console/url`（0600）：日志会被群里刷走，这个文件留在本地。

```text
[14:05:03] [INFO] [Plugin/Console] 控制台已就绪 http://127.0.0.1:7801/?t=…
```

```sh
./bot ui          # 打开它（termux-open-url）
./bot ui url      # 只打印地址
```

页面上五处：总览、插件（开关与全部配置项，就地改）、搭话（人格与档案可改、记忆与表情包库可看）、日志（实时流，按级别与关键字筛）、命令（一行 `/ctl`）。顶栏那枚齿轮是接入与全局：连哪几个实现端、指令前缀、全局群黑白名单、浏览器路径——它们不在任何插件的配置里，`/ctl` 够不着，没有这一页就只能在停机时改文件。

总览页右下角那六行「最近日志」每六秒拉一次（拉的是内存里的环形缓冲，不碰数据库也不碰平台），不会停在打开那一刻。

版式按可用宽度分三档，不按设备分：窄于 600px 是底部导航条，600—839px 是左侧导航轨，840px 起是常驻抽屉。宽屏上插件页是「列表 + 详情」并排，点一行只换右边那一格；窄屏上详情另占一页。宽屏还有两条键盘路径：`1`—`5` 换页，`/` 跳到当前页的搜索框。

装到桌面：Android 用 Chrome 打开，菜单里的「安装应用」；iOS 用 Safari 打开，分享里的「添加到主屏幕」；桌面浏览器看地址栏右侧有没有安装图标。设置页里有一张卡片写着这台设备该怎么装。

几条边界：

- **写配置只有 `ctl::change` 一条路径。** 页面上的每一次改动都汇到 `ctl::change` / `ctl::set_value`，校验、串行化、失败不改内存、原子替换一步不少。口令决定谁有权按下它，保存路径不变。
- **页面上的动作不产生群消息。** 页面上的动作等价于在本机敲 `/ctl`，不碰 QQ、不发消息、不触发模型。要与群互动请回群里，或者用 agent 房间。
- **搭话那一页改的是运行目录里那两份文本**（`data/ambient/persona.md` 与 `self.md`），与手工覆盖是同一条路——落盘前先存一份 `backup-<日期>-<时分>`，仓库里那份模板不动。
- **关掉它不影响指令、排期与推送。** `enabled = false` 或启动带 `--no-ui` 之后，这几样一切照旧。运行中改成 `false` 会让接口立刻停下应答，端口要到下次启动才释放。
- **访问控制靠那道口令。** 32 位十六进制，首次启动生成。换一个就 `rm data/console/token` 再重启；填在 `[console] token` 里就用你填的那个。`bind` 换成非回环地址不安全，起作用的是那道口令。

### 界面这一层不跟机器人抢机器

界面与机器人在同一台机器上跑（Termux 上尤其），所以这一层按「自己别碍事」写：

- 页面不加载任何外部资源，图标、样式与脚本都编进二进制，完全离线也打得开；静态资源带 `ETag`，重复打开走 304。
- 能就地更新就不整页重画：开关、配置项、搜索与筛选只改受影响的节点，焦点、滚动和输入法状态都留在原地。
- 日志只在日志页可见时接收：换页、切到后台、离开页面都断开，回来由服务端补一份快照。断线（401／503 这类非事件流响应）按 2→4→8→16 秒退避自己重开。
- 服务端把日志合批，页面每 100ms 至多写一次 DOM、一次至多 40 行，DOM 窗口 100 行，缓冲 2000 行。跟随只看一个判据：贴底就跟随，向上翻就暂停，暂停期间 DOM 不动。
- 动画只碰 `transform`、`opacity` 与圆角，开了「减少动态效果」时位移类动画整条归零。

回归在 `node tests/console.cjs`（隔离夹具，不动线上实例，`ACUMEN_CONSOLE_SHOTS=<目录>` 顺带出图）；线上实例的只读审计是 `python3 scripts/audit-contrast.py`，用真 Chromium 逐元素算 WCAG 2.2 AA 里能算的几条（对比度、控件边界、触控目标、可访问名、占位文字），不过就退 1。

### 这一层的设计语言

规范在 [WebUI 设计系统](DESIGN_SYSTEM.md)，取值在 `res/console/tokens.css`。四个来源的裁决次序：

```text
平台原生规范 > 可用性与无障碍 > 产品一致性 > M3E > Carbon > Miuix
```

界面不再借卡片图的 `res/cards/m3e.css`：卡片是发进群里的静态位图，界面是要跟随系统明暗、对比度与动态偏好的网页。

## 部署顺序

1. 编译并测试：`cargo test`、`cargo build --release`。也可以运行 `node tests/foreground.cjs` 验证隔离配置下的前台指令、进程管理与退出保存，运行 `node tests/restart.cjs` 验证保留 PID 的手动重启与定时重启
2. 向正在运行的 acumen 发送 SIGTERM，等待进程退出和「配置已保存」日志
3. 备份并修改配置，开启所需插件。基础部署可以开启 `ctl`、`help`、`meta_filter`、`logger`、`recorder`，按实际需求启用其他插件
4. 从仓库目录运行 `./bot start`，前台启动并临时开放本机控制台
5. 检查日志中的插件初始化与 Satori READY / 登录状态，再通过 `/ctl list` 查看配置

先停机再改配置，避免退出保存覆盖手工修改。私有配置、凭据、运行日志与数据库不提交到 Git。

## 前台运行与进程管理（Linux / Termux）

在仓库目录执行 `./bot start`。它会切换到正确的工作目录并运行 release 程序，输出实时日志，直接输入 `/ctl` 或 `/ctl list` 即可管理插件。按 `Ctrl+C` 停止，等待日志显示「配置已保存」和「Bye!」后，再手工编辑配置。

`--console` 只作用于本次启动，不改变 `[[bots]]` 的 console 开关，也不改变任何插件开关。例如 help 原来是关闭的，仍会保持关闭，可以用 `/ctl on help` 手动开启。

另一个终端中可执行：

| 指令 | 作用 |
| --- | --- |
| `./bot status` | 查看进程状态与 PID（受 runit 托管时同时打印 `sv status`）；停止时退出码为 3 |
| `./bot start` | 启动：未托管时在当前终端前台启动并开放控制台；托管时等价 `sv up` |
| `./bot stop` | 优雅停止：未托管时发送 SIGTERM 并等待退出，超时报告失败而不强制终止；托管时走 `sv down` |
| `./bot restart` | 托管时 `sv restart`，否则停止后重新前台启动 |
| `./bot console` | 手动前台启动（带控制台）。托管时要先 `./bot stop`，否则撞文件锁 |
| `./bot logs` | 创建（或复用）名为 `acumen-log` 的 tmux 窗口，实时跟运行日志 |
| `./bot attach` | 进入该日志窗口 |
| `./bot` | 无参数时先 `logs` 再 `attach` |
| `./bot enable` / `./bot disable` | 是否随 Termux 监督树自启（仅托管时有意义） |
| `./bot reap` | 收掉父进程已不在的 cdp-shot 浏览器进程（每次启动也会自动做） |
| `./bot power on/off` | Termux 唤醒锁：熄屏保持网络，`off` 需先停止 bot |
| `./bot help` | 启动脚本帮助 |

`status`、`stop`、`attach` 分别可以简写为 `s`、`down`、`a`。`logs` 也可以写 `session` 或 `up`。`enable` / `disable` 可以写 `on` / `off`。把脚本链接到 `$PREFIX/bin/bot`（Termux）或 `~/.local/bin/bot` 后，任意目录都能直接使用。`./bot start` 在 Termux 上会自动取得唤醒锁以避免熄屏断网，`ACUMEN_WAKE_LOCK=0` 可以关闭。唤醒锁由整个 Termux 共享，`./bot power off` 会影响其他 Termux 任务，因此要求先停止 bot。

未托管时按 `Ctrl+C` 停止 bot，`tmux` 会话里跑的就是 bot 本身。托管后 bot 由 runsv 管，`./bot logs` 的窗口只是 `tail -F` 日志，关掉它不影响 bot。无论哪种方式，脚本都通过进程可执行文件路径识别本仓库实例，不要绕过脚本另外启动第二份程序。

手动启动时还会用 `.bot.lock` 加文件锁防止重复启动。托管路径（`serve`）不加锁。runsv 已保证同一时刻只有一个 `run` 实例，而且那把锁的 fd 会被 bot 派生出的 Chromium 继承，浏览器可能比 bot 活得久，锁就被一个已经无关的进程持有，后续启动全部报「锁被占用」（2026-09-14 因此出现过托管服务连续退出码 1 起不来）。

每次启动还会先收掉浏览器僵尸。`cdp-html-shot` 只在正常析构时 kill 浏览器（`BrowserProcess::drop`），所以 bot 被 SIGKILL、panic-abort、或走 `std::process::exit()` 时不会执行，浏览器会变成孤儿一直占内存又没有任何作用。判据是三条同时成立：命令行带 `--user-data-dir=` 与 `cdp-shot_`（即该 crate 拉起的浏览器及其 renderer/gpu 子进程）、不在本进程的祖先链上、往上找不到活着的 acumen 祖先。有祖先说明正被某个 bot 或测试用着（认 acumen 用可执行文件名，临时目录里跑的测试实例也算）。启动时自动做，也可以 `./bot reap` 手动收一次。

## 交给 termux-services（runit）托管

Termux 上可以让 `termux-services` 常驻监督 bot：进程一退出 runsv 就把它拉起来，日志交给 `svlogd` 落盘。服务文件在 `scripts/service/acumen/`，装到 `$PREFIX/var/service/acumen/`，权限都设 755：

```sh
mkdir -p "$PREFIX/var/service/acumen/log"
cp scripts/service/acumen/run scripts/service/acumen/finish "$PREFIX/var/service/acumen/"
cp scripts/service/acumen/log/run "$PREFIX/var/service/acumen/log/"
chmod 755 "$PREFIX/var/service/acumen/run" "$PREFIX/var/service/acumen/finish" "$PREFIX/var/service/acumen/log/run"
# 再把 run 最后一行换成这个 checkout 的绝对路径
```

| 文件 | 作用 |
| --- | --- |
| `run` | runsv 启动 bot（`bot serve`）。`./bot` 靠这个文件里出现本 checkout 的绝对路径判断自己是否被托管，所以那行不能写成 `$HOME/...` |
| `finish` | runsv 在服务终止后、重启前执行。秒退（低于 `ACUMEN_MIN_UPTIME`，默认 20 秒）就退避同样时长；人为停止不退避 |
| `log/run` | `svlogd -tt` 输出到 `$PREFIX/var/log/sv/acumen` |

`runsv` 没有内置退避，`run` 秒退时它会约每 1.25 秒重启一次（实测 20 秒起 16 次），配置解析失败、二进制缺失、启动锁被占这类情况会一直热循环。`finish` 的退避把重试压到每分钟几次。活得久说明是运行中偶发退出，立刻重启，不影响正常崩溃恢复。人为停止不退避：实测 bot 接住 SIGTERM/SIGINT 后是正常退出（退出码 0、信号 0，日志以「Bye!」结尾），所以判据是退出码 0 而不是信号。注意 `runsv` 在 `finish` 里跑 sleep 时 `sv up` 要等 sleep 结束才生效，这也是人为停止必须走豁免的原因。

`tests/` 会把启动脚本复制到临时目录运行，那种情况按未托管处理，`start` / `stop` 不会去动真正在跑的服务。改完 `run` 后 `runsv` 会在下次启动时读取新内容。

托管后：停止用 `./bot stop`（等价 `sv down acumen`，runsv 不会再拉起来），恢复用 `./bot start`。`./bot logs` 跟的是 `$PREFIX/var/log/sv/acumen/current`。想让 Termux 重启后也不自启，用 `./bot disable`。

## 宿主被厂商清理器杀掉时（Android）

ColorOS 之类的清理器会连整个 Termux 应用一起杀掉，Termux 里的 `runsvdir`、bot 和日志窗口随之消失，runit 管不到这一层。`scripts/` 下有一套以 root 运行的看守来补这一层：

| 仓库文件 | 部署位置（都 755） |
| --- | --- |
| `scripts/97-termux-revive.sh` | `/data/adb/service.d/97-termux-revive.sh`（KernelSU / Magisk 开机拉起） |
| `scripts/termux-revive.sh` | `/data/adb/termux-revive/termux-revive.sh`（看守本体） |

看守每 60 秒看一次 `runsvdir` 在不在，不在就拉起 Termux。它区分「你手动停的」和「被动被杀」，前者不复活，判据是 `dumpsys package` 的 `stopped` 状态与 `ApplicationExitInfo` 的 `reason`。连续拉不起来时退避（`RECOVER_WAIT` 翻倍，封顶 `MAX_WAIT`，默认 30 分钟），不会每 65 秒反复拉一次。`--check` 打印全部判据，`hold` / `resume` 暂停与恢复。细节见两个脚本的头部注释。

进程「运行中」不代表 QQ 已经连接，连接成功应看到 Satori READY / 登录就绪日志。`/ctl list` 查看插件开关，`/ctl show <插件>` 查看配置。

ctl 与 help 使用 Chromium 网页卡片。插件总览按目录排，版心 920px、条目按两列网格并排。插件详情与 ctl 的卡片版心 640px，单栏呈现。开关与待重启状态有文字标签，指令、别名、配置与差异自动换行并保留完整内容。`image_scale` 控制 PNG 分辨率（1—4 倍，默认 3），`image_enabled = false` 可以使用纯文本。

安装 Chrome/Chromium 与系统中日韩字体，并在全局 `browser_path` 指定浏览器路径。单张卡片最多渲染 45 秒（排队不计入，见 [ARCHITECTURE.md](ARCHITECTURE.md) 的出图与渲染），结束后清理页面。缺少浏览器、超时或图片超出安全尺寸时自动回复完整文本。`on` / `off` / `set` / `reset` 的确认及错误继续以文本回复。
