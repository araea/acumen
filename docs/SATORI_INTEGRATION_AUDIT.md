# Satori 协作审计（实现端 0.28.0 / 客户端 0.28.0）

对象是 [satori-qq](https://github.com/araea/satori-qq)（Satori v1 实现端，Zygisk 注入的纯 JNI 层）
与 acumen 的 Satori 适配层。日期 2026-09-23，基线是实现端 0.27.1 / 客户端 `b716c03`。

## 结论

两侧的协作面是清楚的：实现端负责 QQ NT 内核调用与事件投递，客户端只在 Satori 语义内取用，
QQ 专有能力走 `internal/*`。按这个分工可以做到互不猜疑——但 0.27.1 之前有几处实现端与客户端
各自成立、合在一起却不成立的假设，下面逐条列出并给出改动。

## 方法

- 对照 Satori 的[事件](https://satori.chat/zh-CN/protocol/events.html)、
  [表态](https://satori.chat/zh-CN/resources/reaction.html)与
  [扩展](https://satori.chat/zh-CN/advanced/internal.html)规范，逐条核对两侧信令与回执。
- 通读 WS 信令面（`IDENTIFY` / `READY` / `EVENT` / `PING` / `PONG` / `META`）、三条 HTTP 通道
  与出站闸门。
- 两侧测试：实现端 JVM 用例 27 组 + `qqguard` 状态机；客户端 `cargo test --locked --bin acumen`。
- 真机：Android QQ 9.3.65，Zygisk Next 1.5.0，测试群 `1126269891`。

## 逐项发现与修复

### F1 READY 与事件投递的竞态

`READY`、历史回放与实时广播原来各自在 `eventEmitLock` 外或锁内不同的位置分配 `sn`，客户端可能
先收到较大的 `sn` 再收到较小的。现在 READY 与回放整体进入投递锁，`sn` 一律在
`emitSatoriEvent` 投递的那一刻分配，投递顺序即 `sn` 顺序；重复 `IDENTIFY` 不再回放第二遍。
`READY` 另带 `satori_qq.session_id`（进程标识）与 `satori_qq.sn`（当前游标），
客户端据此区分「同一个实现端进程」与「进程重启过」。`sn` 从当前毫秒时间起算。

### F2 重连重复投递待审批申请

重连请求回放时，之前除了历史事件还会再投一遍待审批的好友、群申请。现在回放与
「补投待审批请求」二选一，只有不带 `sn` 的新会话才补投。

### F3 换号后旧任务沿用新账号

两侧都有这个问题，各自修了一半：

- 实现端：换号时清掉历史缓冲、自己的表态缓存、消息与资源缓存、群名与去重表，并在投递锁内
  通告 `login-updated`。排队中的写操作与媒体转换后真正落盘的发送会复核登录账号，
  不一致直接回 404，不再用新账号把上一个账号的请求发出去。
- 客户端：账号身份在一条连接内固定。`login-updated` 只在账号相同时更新资料；账号变化时
  satori-qq 客户端主动断开重连，新连接从 `READY` 重新取身份。`login-removed` 一律重连。
  其余适配器保持原行为。

### F4 `reaction.clear` 的语义与标准不符

实现端原来把标准 `reaction.clear` 实现成「清除自己加的回应」，与规范里「清除该消息上所有
用户的表态」不是一回事，客户端调用方无从分辨。QQ JNI 没有清别人表态的能力，因此：

- `reaction.clear` 从 `features` 移除并返回 404（`removed_action`），移进 `removed` 表，
  客户端读到即记入「不可用」名单，不再反复试。
- 「清自己的表态」迁到 `POST /v1/internal/reaction_clear`，参数
  `channel_id, message_id, emoji_id?`，返回 `{cleared, scope:"self"}`；没有自己的表态返回 0
  而不是报错；部分失败明确报错，不假装全清成功。
- 单个表情的撤销继续用标准 `reaction.delete`。
- `reaction.list` 按规范要求必须带 `emoji_id`（缺参数回 400）。QQ 内核一次只认一个表情，
  原来不带 `emoji_id` 时返回的是「自己加过的那些」，那是自造语义，已删除。

### F5 新增 `internal/reaction_summary`

客户端此前只能知道「自己有没有回应过」，拿不到一条消息上有哪些回应、各有多少。
新增 `POST /v1/internal/reaction_summary`，参数 `channel_id, message_id`，返回：

```json
{"message_id":"…","data":[{"emoji_id":"76","count":2,"self":true}],
 "source":"kernel_cache","observed_at":1730000000000}
```

`count` 来自 QQ 本地消息快照，`self` 优先取实现端已确认的动作。两者更新时间可能不同，
所以带 `observed_at`。QQ 没有「列出全部回应者」的内核调用，这条接口也不试图推断是谁点的。

### F6 `internal/poke` 的目标解析

原来只认 `guild_id` + `user_id`，私聊没有走法。现在 `channel_id` 可给群号或 `private:<QQ号>`，
并拒绝自相矛盾的目标（`channel_id` 与 `guild_id`/`user_id` 指向不同对象时回 400）。

### F7 QQ 专有消息元素缺命名空间

`json`、`mface`、`poke` 三个 QQ 专有元素原来以裸标签收发，与 Satori 标准元素表撞名。现在输出
`satori-qq:json`、`satori-qq:mface`、`satori-qq:poke`，输入两种都收。两侧的标签正则都补了
命名空间前缀——此前客户端与实现端的解析正则都不认 `:`，专有元素会被静默丢掉。骰子与猜拳继续用
标准 `<emoji>`。

### F8 WS 失联不会超时

客户端原来只发 `PING`、不看 `PONG`，实现端进程僵住时连接会一直挂着。现在 30 秒收不到 `PONG`
即断开重连；`READY` 失败、连接钩子报错与正常断开都会回收写任务与心跳任务；连接持续一分钟后
把重连退避重置回 3 秒。

### F9 能力拒绝缓存全局共享

客户端把「实现端说这个动作不可用」记在进程级缓存里，键只有动作名。一个账号受限会影响其他连接
与其他账号。现在键是「连接 + 平台 + 账号」。

### F10 客户端的表态动作

- `react_clear` 只撤销自己的回应：单个表情用 `reaction.delete`，全部用 `internal/reaction_clear`；
  遇到没有该扩展的旧实现端，只撤销本轮已确认添加的那些，并报告范围受限。
- `satori_read({message_id, reactions:true})` 可查回应概况，与 `forward:true` 互斥。
- agent 工具说明与 `satori-reply` 技能文档同步说明「计数可能滞后、不推断回应者」。

### F11 错误类型

客户端的 HTTP 错误原来是格式化好的字符串，插件只能匹配文案。现在 `SatoriApiError` 保留
`method/status/code/message`，插件可 `downcast` 判断 404 与 `removed_action`；`Display` 文案
保持原样，现有匹配代码不受影响。QQ 扩展的 `ok:false` 仍按失败处理，动作超时是结果未知，
不自动重试。

### F12 客户端可用的扩展入口

新增 `adapters::satori::qq::{capabilities, poke, reactions, clear_reactions, call}`：
ID 一律用字符串，能力清单与回应概况有类型化返回值，`call` 可调用能力表里的其他扩展；
非 satori-qq 适配器调用返回明确错误。示例见 [Satori 接入](SATORI.md)。

## 验证

| 项目 | 命令 | 结果 |
| --- | --- | --- |
| 实现端 JVM + qqguard | `./test.sh` | 27 JVM + qqguard 通过 |
| 实现端构建 | `bash build.sh` | `SatoriQQ.apk` 与 `SatoriQQ-module.zip`，native 无第三方依赖 |
| 客户端测试 | `cargo test --locked --bin acumen` | 557 项通过 |
| 客户端构建 | `cargo build --release --locked` | 通过 |

## 限制与未做的事

- **资料卡点赞**：实现端 0.23.0 起已移除，客户端读到 `removed` 表后记入不可用名单，不出网、
  不占额度。不要再把它当「还没试过的按钮」。
- **`reaction.clear` 无法实现**：清所有用户的表态在 QQ JNI 没有对应调用，保持 404。
- **回应计数可能滞后**：`reaction_summary` 的 `count` 是 QQ 本机缓存，刚完成的动作不保证立刻
  反映；`self` 以实现端已确认的动作为准，因此「自己回应过」比 `count` 更新。
- **不推断回应者**：表态事件只给数量变化，没有可靠操作者时不填 `user`；消息作者不等于回应者。
- **事件缓冲只在进程内**：只保留最近 4096 条，`sn` 不跨进程重启恢复；重启后 `session_id` 变化，
  客户端按新会话处理。
- 本次改动没有新增 ART hook、Java hook 框架或第三方 native 依赖；真实权限、频率限制与服务端
  接受度仍以回执为准，超时不代表失败。
