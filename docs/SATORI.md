# Satori 接入说明

适配层实现 [Satori v1 协议](https://satori.js.org/zh-CN/)，不绑定特定实现端。

## 连接

acumen 连接实现端的 `/v1/events` WebSocket，连接建立后 10 秒内发送 `IDENTIFY`。收到 `READY` 后，从首个 login 建立 `BotStatus`，再触发插件的 connected 生命周期。账号身份在连接内固定；同账号资料更新可共享，换号必须重连，旧任务仍携带旧选择器。

- `EVENT`：记录 `sn`，转为框架内部的规范化事件后进入插件流水线。`login-updated` 不进流水线，同账号时更新资料，satori-qq 换号时重连
- `PING`：本端每 10 秒发出，实现端回 `PONG`；收到反向 `PING` 也回 `PONG`
- `META`：刷新代理路由列表
- 连接关闭或失败：从 3 秒开始指数退避，最长 60 秒

重连时的 `IDENTIFY` 带上最后收到的事件 `sn`，实现端据此补推断线期间的事件。首次连接不带 `sn`，表示新会话。

实现端地址同时作为 HTTP API 根地址。配置接受 `http(s)://`，也兼容 `ws(s)://` 和带 `/v1/events` 的完整事件地址，启动时统一规范化。

## 鉴权

token 的取值顺序是环境变量 `ACUMEN_SATORI_TOKEN`、`config.toml` 中 Satori bot 的 `access_token`，两者都为空时不鉴权。同一个 token 用于 WebSocket 的 `IDENTIFY.body.token` 和 HTTP 的 `Authorization: Bearer ...`。HTTP 请求另外带 `Satori-Platform` 和 `Satori-User-ID`，取值由 `READY` 返回的 login 决定，同账号的 `login-updated` 更新资料；账号变化建立新连接，避免旧任务改用新账号。

## 资源链接

消息元素里的 `src` 不一定能直接下载。适配层按协议的[资源链接](https://satori.js.org/zh-CN/advanced/resource.html)规范，为插件解析出可以直接 GET 的 `url`：

- `internal:` 链接（平台内部资源、`upload.create` 的产物）走实现端的 `/v1/proxy/{url}`，该路由不需要鉴权头
- 命中 `READY` / `META` 下发的 `proxy_urls` 前缀的平台链接同样走代理路由，这类链接通常有防盗链或时效限制
- 其余 http(s) 链接直连
- `data:`、`file:` 与本地路径没有下载地址，不写入 `url`

原始 `src` 保留在消息段的 `file` 字段，便于原样回传给实现端。

## API 映射

| 框架能力 | Satori / satori-qq 方法 |
| --- | --- |
| 发送群聊或私聊消息 | `message.create` |
| 获取引用消息 | `message.get` |
| 撤回消息 | `message.delete` |
| 群列表 | `guild.list` |
| 群成员资料 | `guild.member.get` |
| 添加或取消表态 | `reaction.create` / `reaction.delete` |
| 发送文件 | `upload.create` multipart + `message.create` `<file>` |
| 群头衔 | `internal/special_title` |
| 资料卡点赞 | 纯 JNI 实现端不可用，不向模型承诺可用 |
| 戳一戳 / 回应概况 / 清自己的回应 | `internal/poke` / `internal/reaction_summary` / `internal/reaction_clear` |
| 合并转发读取 | `internal/get_forward` |
| 资源代理 | `GET /v1/proxy/{url}` |

HTTP RPC 自带同步响应：需要把后续引用与已发送消息关联时，可以直接读取 `message.create` 返回的消息 ID，不需要 WebSocket echo 匹配器。`guild.list` 等标准分页列表跟随响应里的 `next` 令牌翻页。

## 事件规范化

插件通过 `Context::as_message()` 使用稳定的内部消息视图。适配器从原始 Satori event 提取频道、群、用户、成员角色、时间戳与消息 ID，并把 `message.content` 元素串解析为内部消息链；完整的原始 event 存放在规范化事件的 `_satori` 字段。

非消息事件保留 `satori_type`，并对撤回、成员变化、申请、禁言与戳一戳提供统一字段。全局群黑白名单在事件进入插件流水线前执行。

## 消息元素

文本内容按协议只转义 `<`、`>`、`&`，属性额外转义 `"`。撇号不转义，协议的转义表里没有 `&apos;`。

协议规定「文本内容前后包含换行符的连续空白会被忽略」，所以文本段首尾的换行用 `<br/>` 送出，段内换行保持原样，否则 `@某人\n[图片]` 这类排版会被丢掉。

双向转换覆盖：

- 文本与 XML 实体转义
- `<at>`、`<quote>`、`<emoji>`
- `<img>`、`<audio>`、`<video>`、`<file>`
- `<json>`、`<mface>`、`<poke>`、骰子与猜拳
- `<message forward>` 合并转发与自定义节点

Satori 与 QQ NT 的消息 ID 可能超出 32 位范围，适配层与插件 API 一律使用 `i64`，引用 ID 在元素串中保持十进制字符串。

## 实现端

当前适配目标是本机 `satori-qq`：默认地址 `http://127.0.0.1:3001`，平台名 `red`，适配器名 `satori-qq`，支持范围以该仓库的 `docs/SATORI_SUPPORT.md` 为准。适配器只注册 `satori` 和本地测试用的 `console`，不含 OneBot 接入。

## 复读时效

复读状态在收到事件时同步更新，发送和其他插件仍然异步执行，这样入库、媒体处理和任务调度不会改变事件的先后顺序。指令、被交互等待器消费的输入以及不可复读的消息也会打断复读。

`[repeater].max_delay_ms` 默认 `3000`：优先按 `message.created_at` 计算，缺失时按事件 `timestamp` 计算，没有时间戳时按本地接收时间计算。过期消息不计入复读，也不参与历史补发后的复读计数。`0` 按 `1ms` 处理，不表示无限等待。

准备发送与全部 `BeforeSend` 钩子执行后都会检查这条消息是否仍然有效。对 satori-qq，复读的 `message.create` 还附带非标准扩展：

```json
{"satori_qq": {"if_latest_message_id": "触发消息的 ID", "expires_at": 1788870003000}}
```

更新后的实现端会在出站队列等待结束后、媒体转换后以及每次交给 QQ 内核前检查：同一频道出现任何更新的消息，或超过 Unix 毫秒截止时间，就返回空数组并放弃发送。普通插件发送不携带该条件，仍按原有队列与限速执行。

完整保护需要同时更新 acumen 与 satori-qq。其他实现端或旧版 satori-qq 只有框架发送前的保护。HTTP 请求发出后，框架无法撤销其内部排队。即使双方都更新，已经交给 QQ 内核的消息仍可能受 QQ 网络或媒体上传耗时影响，这时无法保证显示顺序。

## satori-qq 0.9.0 回执与事件兼容

标准无返回值 RPC 支持 204/空响应体。QQ 扩展即使 HTTP 返回 200，JSON 的 `ok:false` 仍作为错误传给插件；原始错误信息保留。`payload:false` 留在通用 API 返回值里，由 ambient 查询层明确报告“状态未知”。

规范化事件保留原始 `channel_id`，API 优先使用它定位频道；成员事件的用户可从 `member.user` 取得，登录身份兼容顶层 `self_id`。只有 `_type=satori-qq/mute` 的成员更新归类为禁言，普通群名片/角色变化保留成员资料更新语义。读取 `native:` 合并转发附带当前 `channel_id`，模块重启后仍可定位父消息所属会话。

更多群内操作与实际限制见 [ambient 的实现端适配](ambient.md#satori-qq-090-的环境与操作适配)。

## satori-qq 0.23.x 兼容说明

0.23.0 起实现端进程内不再有任何 hook 引擎（没有 LSPlant/Dobby、不改写 ArtMethod），引导、内核会话
与回包通道都走 JNI 层。协议面本身不变，下面是这几次改版对适配层的实际影响。

**动作移除要能认**。0.23.0 收掉了 `internal/like`（资料卡点赞走 QQ 的 WUP/Handler 通道，纯 JNI 层
做不到）。曾经有过、后来移除的动作现在报 404 且响应体带 `code=removed_action`，与「方法名写错」的
`API not found` 分开。适配层把 `code` 一并带进错误文案（`[code=removed_action]`），聊天层按它把这
个能力记成不可用——否则每轮都会白试一次，还占掉一次动作额度。

`internal/capabilities` 的 `actions` 现在只列真实可用的动作，另外给一段 `removed` 写明「哪个版本、
为什么」。`payload:false` 与 `ok:false` 的处理没有变化。

**群名是补出来的**。内核的 `GroupSimpleInfo.groupName` 对个别群是空的（本机测试群就是），所以
`guild.get` / `channel.get` 会依次退到 `remarkName` 与一次群详情查询，单个群的显式查询最多等 3 秒；
`guild.list` 仍走异步，不为一个群把整页拖住。返回空 `name` 的窗口比过去短很多，但不是零。

**换号会踢连接**。实现端盯住当前账号，换号时先发 `login-updated` 再断开所有已 READY 的连接——
客户端手里那份 READY 的 `user.id` 是旧账号，继续用会被实现端判成「别的登录」，整条连接哑掉。
适配器的重连（3 秒起指数退避）能接住，重连后按新 READY 重建身份。

**排障看 `internal/status.inbound`**：`recv` / `add` / `update` / `gray_tip` / `emitted` /
`notices` / `manual_self`。事件收不到时先用它分开三种情况——记录压根没进来、进来了但没转成事件、
转了但客户端没消费。

手打消息的身份仍是 `qq-client:{selfUin}`，真实身份在 `satori_qq` 扩展里；这部分 0.23.x 没有变化。


## 0.28.0 协作修复

- 收到重复或较旧 sn 时不重复进入插件流水线；登录事件不改恢复游标。READY 的进程会话标识
  变化会重置游标。只接收当前 READY 所选登录的事件，其他登录不能冒用它调用 HTTP。
- 30 秒没有 Satori PONG 就断线重连；READY 失败、连接钩子异常及正常断开都会回收写任务和心跳。
  连接持续一分钟后重置重连退避。
- `SatoriApiError` 保留 `method/status/code/message`，插件可 downcast 判断 404/removed_action，
  现有错误文案保持兼容。QQ 扩展的 `ok:false` 仍被当作失败，动作超时不自动重试。
- 插件可使用 `adapters::satori::qq::{capabilities,poke,reactions,clear_reactions,call}`。
  ID 使用字符串，能力清单与回应概况有类型化返回值；`call` 可调用能力表中其他扩展。
- ambient 的 `react_clear` 永远只撤销自己的回应：单个用 `reaction.delete`，全部用 QQ 扩展；
  老实现端没有扩展时只对本轮已确认添加的回应逐个撤销，并报告范围受限。
- `satori_read({message_id, reactions:true})` 可查看回应概况。与 `forward:true` 互斥。
  能力拒绝缓存按实现端/平台/账号隔离。QQ 消息元素输出带 `satori-qq:` 前缀，接收兼容旧名称。

示例（插件持有 ctx 与 writer）：

```rust
let caps = satori::qq::capabilities(&ctx, &writer).await?;
if caps.supports("poke") {
    satori::qq::poke(&ctx, &writer, "群号", "目标QQ号").await?;
}
let summary = satori::qq::reactions(&ctx, &writer, "群号", "消息ID").await?;
```

完整检查范围及限制见 [协作审计](SATORI_INTEGRATION_AUDIT.md)。
