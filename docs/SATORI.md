# Satori 接入

Acumen 通过 [Satori v1](https://satori.js.org/zh-CN/) 连接实现端。实现端同时提供事件 WebSocket 与 HTTP API；Acumen 不绑定 QQ 的专有传输协议。

## 连接与鉴权

在 `config.toml` 的 `[[bots]]` 中配置实现端：

```toml
[[bots]]
enabled = true
protocol = "satori"
url = "http://127.0.0.1:3001"
access_token = ""
```

`ACUMEN_SATORI_TOKEN` 环境变量优先于 `access_token`。两者都为空时不发送令牌。`url` 可使用 `http(s)://`、`ws(s)://` 或完整的 `/v1/events` 地址；启动时会规范化为 API 根地址和 WebSocket 事件地址。

Acumen 连接 `/v1/events`，10 秒内发送 `IDENTIFY`。收到 `READY` 后建立登录状态并通知插件。每 10 秒发送一次 `PING`；收到反向 `PING` 也会回复 `PONG`。断线后从 3 秒起指数退避，最长 60 秒；重连时携带最后收到的事件序号 `sn`，请求实现端补发断线期间的事件。

HTTP 请求使用 `Authorization: Bearer …`，并带上 `Satori-Platform` 和 `Satori-User-ID`。身份来自 `READY`；同一账号更新资料时刷新登录信息，账号变化时重连，避免旧任务以新账号执行。

`message.create` 超时为 100 秒，覆盖默认出站排队与媒体确认/重试；其他请求仍为 65 秒。
satori-qq 0.29.6 起不会把没有回执的发送当作成功，超时可能返回 `send outcome unknown`。
ai_news 图片确认成功后只发图片；明确失败才回退文本。结果不明或发送后 HTTP 响应丢失时，
本条不自动重投或补发文本，并记录告警；这是避免重复的选择，不代表已确认送达，最终失败的条目可能漏发。

锁屏时 ambient 没搭话，先对照日志：没有及时入站事件要查 QQ 冻结/后台调度及 Satori 断连；
有 `保持沉默` 则判定已执行；`搭话失败` 中的模型超时要查网络与模型服务。
QQ 的 Android 唤醒锁与 QQ 内核的前后台状态是两层；satori-qq 的 `kernel_foreground` 用于后者。

## 资源与消息

实现端返回的资源地址不一定可直接下载。`internal:` 地址和 `READY` / `META` 提供的代理域名通过 `/v1/proxy/{url}` 获取；其他 HTTP(S) 地址直连。`data:`、`file:` 和本地路径不能作为远程下载地址。原始 `src` 保留在消息元素中，供插件回传。

文本内容按 Satori 规则转义 `<`、`>` 和 `&`，属性值还转义双引号。为保留段首或段尾换行，Acumen 将其转换为 `<br/>`；段内换行保持原样。

内部消息视图覆盖文本、@、引用、表情、图片、音频、视频、文件、JSON 和合并转发。消息 ID 使用 64 位整数；元素中的引用 ID 保持十进制字符串。

## API 与事件

| 功能 | Satori 方法 |
| --- | --- |
| 发送、查询和撤回消息 | `message.create`、`message.get`、`message.delete` |
| 查询群与成员 | `guild.list`、`guild.member.get` |
| 上传媒体 | `upload.create`，再通过 `message.create` 发送 |
| 添加或移除回应 | `reaction.create`、`reaction.delete` |
| QQ 扩展 | `internal/special_title`、`internal/poke`、`internal/reaction_summary`、`internal/reaction_clear`、`internal/get_forward` |

列表接口跟随 `next` 分页令牌读取；重复令牌或超过安全页数时返回错误，不把不完整结果交给定时任务。`message.create` 返回消息 ID，插件不需要通过 WebSocket 回声匹配发送结果。

事件转换为 Acumen 内部上下文后进入插件流水线。频道、群、用户、角色、时间戳和消息 ID 使用统一字段；原始 Satori 事件保存在 `_satori`。非消息事件保留 `satori_type`。全局群名单在事件进入插件前执行。

## 兼容性边界

生产目标是本机 [satori-qq](https://github.com/araea/satori-qq)，默认地址 `http://127.0.0.1:3001`。它使用平台名 `red`；支持范围以该仓库的 [Satori v1 接口表](https://github.com/araea/satori-qq/blob/master/docs/SATORI_SUPPORT.md) 为准。

`reaction.clear` 在标准协议中的语义是清除所有人的回应，QQ 实现端不提供该能力。清除自己的回应使用 `internal/reaction_clear`。`typing` 与 `mark_read` 是实现端扩展，不是标准 Satori 方法，调用前应查询能力并检查回执。

复读与其他有时效的即时消息可附带 `satori_qq.if_latest_message_id` 和 `satori_qq.expires_at`。该扩展在实现端排队、媒体转换后再次检查消息是否过期；其他实现端可能忽略它，Acumen 仍会在发送前做一次检查。已交给实现端或 QQ 内核的消息无法撤回。

完整方法、事件、元素及扩展参数见 [satori-qq 的 Satori 支持表](https://github.com/araea/satori-qq/blob/master/docs/SATORI_SUPPORT.md)。群聊搭话使用到的能力边界见[群聊搭话](ambient.md)。
