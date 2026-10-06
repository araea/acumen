# Satori 接入

Acumen 通过 [Satori v1](https://satori.js.org/zh-CN/) 连接实现端；实现端同时提供事件 WebSocket 与 HTTP API，Acumen 不绑定 QQ 的专有传输协议

## 连接与鉴权

在 `config.toml` 的 `[[bots]]` 中配置实现端：

```toml
[[bots]]
enabled = true
protocol = "satori"
url = "http://127.0.0.1:3001"
access_token = ""
```

- `ACUMEN_SATORI_TOKEN` 环境变量优先于 `access_token`；两者都为空时不发送令牌。
- `url` 可使用 `http(s)://`、`ws(s)://` 或完整的 `/v1/events` 地址，启动时规范化为 API 根地址与 WebSocket 事件地址。
- 连接 `/v1/events`，10 秒内发送 `IDENTIFY`；收到 `READY` 后建立登录状态并通知插件。
- 每 10 秒发送一次 `PING`；收到反向 `PING` 也回复 `PONG`。
- 断线后从 3 秒起指数退避，最长 60 秒；重连时携带最后收到的事件序号 `sn`，请求实现端补发断线期间的事件。`READY` 里任何扩展对象（`satori_qq`、`satori_wx`）的 `session_id` 变了，说明实现端进程重启过，旧序号作废；带旧序号恢复被拒（IDENTIFY 之后直接关连接）也丢弃它，下一次从头开始。序号写成整数值的浮点（`1.79e+15`）也认。
- HTTP 请求使用 `Authorization: Bearer …`，并带上 `Satori-Platform` 与 `Satori-User-ID`。身份来自 `READY`；账号资料更新时刷新，账号变化时重连。

`message.create` 超时为 100 秒，覆盖默认出站排队与媒体确认 / 重试；其他请求为 65 秒。实现端不会把没有回执的发送当作成功，超时可能返回 `send outcome unknown`。`ai_news` 图片确认成功后只发图片，明确失败才回退文本；结果不明或发送后 HTTP 响应丢失时，本条不自动重投或补发文本并记录告警。

锁屏时 `ambient` 没搭话，先对照日志：没有及时入站事件要查 QQ 冻结 / 后台调度及 Satori 断连；有 `保持沉默` 则判定已执行；`搭话失败` 中的模型超时要查网络与模型服务。QQ 的 Android 唤醒锁与 QQ 内核前后台状态是两层，`kernel_foreground` 用于后者。

## 协商与错误

实现端的错误体和能力声明用同一套口径，Acumen 按声明办事：

- **错误体**：每个非 2xx 响应都是 `{"code": "<机器可读的短名>", "message": "<给人看的>"}`，Acumen 解成 `SatoriApiError { status, code, message }`，上游按 `code` 判断（`removed_action`、`unsupported_method`、`session_stabilizing` 等）。
- **暂时不可用**：实现端在请求交给平台之前就挡下时回 503（内核离线、上线后的稳定期、队列满、熔断中）或 429（超出发送额度），能给出恢复时间的带 `Retry-After`。请求没有被执行，原样再发是安全的：Acumen 等够 `Retry-After`（累计不超过 45 秒）再发一次；没带承诺的、要等更久的，原样报错。发出之后才出的错（`send outcome unknown`）不在此列，不会自动重发。
- **标准方法**：`READY` 与 `login-updated` 里 `login.features` 记在连接上；没有声明的标准方法（比如微信上的 `reaction.create`）直接报 `unsupported_method`。没拿到 `features` 时不拦任何调用。扩展方法（`internal/…`）不在 `features` 里，归扩展自己的能力声明管。
- **限额**：连上后读一次 `internal/capabilities`，两个实现端都用同一份 `limits`；目前用到 `upload_bytes`，视频取片的体积上限取 `[video_parse].max_size_mb` 与它的较小值，明知会被拒的上传在发出前就报错。取不到就当没有限额。
- **登录状态**：`login-updated` 里 `status` 的变化记进日志（在线、离线、重连中等）；状态本身不拦调用，离线时实现端自己会回 503。
- **发送者名字**：协议里 `user.name` 是用户名、`user.nick` 是昵称，两个实现各填各的（satori-qq 的 `name` 是 QQ 昵称，satori-wx 的昵称在 `nick`，`name` 只在设了备注或微信号时才有）。Acumen 认 `name`，缺了认 `nick`；群名片取 `member.nick`。

## 资源与消息

实现端返回的资源地址不一定可直接下载。`internal:` 地址与 `READY` / `META` 提供的代理域名通过 `/v1/proxy/{url}` 获取；其他 HTTP(S) 地址直连。`data:`、`file:` 与本地路径不能作为远程下载地址。原始 `src` 保留在消息元素中（段的 `file`），供插件回传；可下载的地址放在段的 `url`。satori-qq 与 satori-wx 的入站媒体都是 `internal:` 链接（不带实现端的监听地址），回传时实现端直接在本机解析，不会再请求自己。

消息里内联的 `base64://` 与 `data:` 媒体，发送前先经 `upload.create` 换成 `internal:` 链接再写进元素。资源指南不推荐内联编码（消息体积大幅增加、实现端得在处理消息时解码），而且实现端的内联上限远低于上传上限（satori-wx 内联图片 8 MiB、上传 1 GiB）。上传跟着消息走路由后的那条连接和登录；上传失败退回内联，消息照发。

事件里带 `referrer`（被动请求的来源上下文，实现端自己定义内容）时，回复的 `message.create` 原样带回它，只交还给发出它的登录。satori-qq 与 satori-wx 都不下发 `referrer`，这一步对它们是空操作。

文本内容按 Satori 规则转义 `<`、`>` 和 `&`，属性值还转义双引号。为保留段首或段尾换行，Acumen 将其转换为 `<br/>`，段内换行保持原样。

内部消息视图覆盖文本、@、引用、表情、图片、音频、视频、文件、JSON 与合并转发。按 Satori 协议，消息 ID 与频道、群、用户等各类标识符一律是字符串，跨连接时不改写、不做数字替身；元素中的引用 ID 同样保持字符串。

## API 与事件

| 功能 | Satori 方法 |
| --- | --- |
| 发送、查询与撤回消息 | `message.create`、`message.get`、`message.delete` |
| 查询群与成员 | `guild.list`、`guild.member.get` |
| 上传媒体 | `upload.create`，再通过 `message.create` 发送 |
| 添加或移除回应 | `reaction.create`、`reaction.delete` |
| QQ 扩展 | `internal/special_title`、`internal/poke`、`internal/reaction_summary`、`internal/reaction_clear`、`internal/get_forward` |

列表接口跟随 `next` 分页令牌读取；重复令牌或超过安全页数时返回错误，不把不完整结果交给定时任务。`message.create` 返回消息 ID，插件不需要通过 WebSocket 回声匹配发送结果。

事件转换为内部上下文后进入插件流水线。频道、群、用户、角色、时间戳与消息 ID 使用统一字段，原始 Satori 事件保存在 `_satori`，非消息事件保留 `satori_type`。全局群名单在事件进入插件前执行。

## 边界

生产目标是本机 [satori-qq](https://github.com/araea/satori-qq)，默认地址 `http://127.0.0.1:3001`，平台名 `red`；支持范围以该仓库的 [Satori v1 接口表](https://github.com/araea/satori-qq/blob/master/docs/SATORI_SUPPORT.md) 为准。同一份配置里可以再接一段 [satori-wx](https://github.com/araea/satori-wx)（平台名 `wechat`，地址 `http://127.0.0.1:5601`），两个实现用同一套协议约定：字符串 ID、`created_at`、资源提升、`upload.create` 的 `internal:` 链接、`session_id`。两端都带同一份黑盒探针 `conformance.py`，改动任何一端之后先跑它。

- `reaction.clear` 在标准协议中语义是清除所有人的回应，QQ 实现端不提供；清除自己的回应用 `internal/reaction_clear`。
- `typing` 与 `mark_read` 是实现端扩展，不是标准 Satori 方法，调用前应查询能力并检查回执。
- 复读与其他有时效的即时消息可附带 `satori_qq.if_latest_message_id` 与 `satori_qq.expires_at`。实现端在排队、媒体转换后再次检查消息是否过期；其他实现端可能忽略它，Acumen 仍会在发送前检查一次。
- 已交给实现端或 QQ 内核的消息无法撤回。

完整方法、事件、元素及扩展参数见 satori-qq 的 Satori 支持表。群聊搭话用到的能力边界见[群聊搭话](ambient.md)。
