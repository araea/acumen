# 视频解析

消息中出现受支持的视频链接时，插件会下载原片并发送。当前仅支持哔哩哔哩稿件页与分享短链。正文链接、QQ 小程序卡和分享卡均可识别。

插件按 `max_size_mb` 选择不超限的画质，将视频下载到 `data/video_parse/`，上传给 Satori 实现端后发送，并删除临时文件。默认发送视频气泡，不发送预览或引用。解析、下载或发送失败时不在聊天中报错，只记录日志。

## 支持的链接

- 稿件页：`/video/BV…`、`/video/av…`，包括 `m.` 与 `/s/` 前缀
- 分享短链：`b23.tv`、`bili2233.cn`
- QQ 小程序卡与分享卡中的落地链接

番剧、直播、空间、专栏和动态页不由此插件处理。`video_parse` 与 `webshot` 共用准入判据，已交给视频解析的链接不会重复截图。

## 配置

配置位于 `config.toml` 的 `[video_parse]` 段，完整示例见 [`config.example.toml`](../config.example.toml)。

| 字段 | 默认值 | 说明 |
| --- | --- | --- |
| `max_size_mb` | `80` | 下载大小上限，单位 MB |
| `prefer_quality` | `64` | 最高优先画质：80（1080P）、64（720P）、32（480P）、16（360P） |
| `send` | `bubble` | 发送方式：`bubble`、`file` 或 `both` |
| `timeout_seconds` | `300` | 单次解析、下载和上传的总时限；排队不计入 |
| `cookie` | 空 | 可选的哔哩哔哩登录态，用于访问更高画质 |
| `channel.white` | 空 | 非空时只处理白名单中的群 |
| `channel.black` | 空 | 忽略黑名单中的群；黑名单优先 |

未登录时通常只能获取 360P–720P。1080P 及以上需要登录态，且实际画质仍由稿件和哔哩哔哩账号权限决定。`both` 会先发视频气泡，再尝试发送群文件；一条失败不会撤回另一条。

同时最多处理两条视频。相同用户在同一会话中 10 分钟内重复发送同一稿件不会重复下载；状态保留一天，并跨进程重启。

## Cookie 安全

Cookie 是哔哩哔哩账号凭据。仅在必要时配置，并将它视为密码；不要提交到 Git，也不要在群聊中通过 `/ctl` 设置。Agent 控制通道会记录执行命令，可能把 Cookie 写入日志。建议停止机器人后在本机编辑 `config.toml`，再启动；如已泄露，请退出该账号或更新登录态。

桌面浏览器登录 `bilibili.com` 后，可在开发者工具的 Network 面板中从 `api.bilibili.com` 请求头复制完整 `Cookie` 值。普通手机浏览器的 `document.cookie` 不会包含 HttpOnly 的 `SESSDATA`。

## B 站请求

稿件信息使用 `x/web-interface/view`，视频流使用 `x/player/playurl`，请求桌面浏览器 UA 和 `https://www.bilibili.com` Referer。插件获取 MP4 分片并按顺序写入同一个临时文件。接口错误 `-352`、HTTP 412 或 429 通常表示风控或限流。

## 测试

```sh
cargo test --bin acumen
```

以下测试会访问真实服务并向指定群发送视频。请只使用测试群；测试后自行撤回消息。

```sh
cargo test --bin acumen live_reads_the_metadata -- --ignored --nocapture
ACUMEN_VIDEO_PARSE_LIVE_GROUP=<群号> \
  cargo test --bin acumen live_takes_ -- --ignored --nocapture
```
