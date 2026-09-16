# 视频解析

视频站链接不走截图：抖音那类本来就只有登录墙，B 站这类要等播放器起画面，截图慢且信息少。这些链接由「视频解析」插件接，两步走。

## 两步：预览，再按需取片

1. 群里出现一条能解析的视频链接，插件只回一条消息：封面、标题、UP 主、时长、
   播放量，外加一行「引用本条并回复「视频」可取原片」。这一步只调一次接口，不下载
   任何视频。
2. 用户**引用那条预览**并回复「视频」「原片」「下载」「文件」这类词，才真的去取片：
   按 `max_size_mb` 挑一档放得下的画质、下载到 `data/video_parse/` 下的临时文件、
   上传给实现端、按 `send` 配的发法发出去，最后把本地那份删掉。

「引用 + 回复」这套隐式交互与 AI 资讯的「引用卡片回复序号」是同一个设计（见
`plugins/ai_news/state.rs`）：预览只出一条消息，取片等用户引用后再说。预览消息与稿件的对应
关系落在 `data/video_parse/state.json`，保留 30 天。同一条预览只会取一次片。取失败
会把标记放回去，用户可以再点一次。

## 取片词前面的 @

QQ 的引用回复会自动补一个 @，平台又把这个 @ 的显示名写进正文：`at` 段后面跟着
一段「@名字 正文」，而插件读到的 `raw_message` 只拼文本段（`at` 段与引用段都不
进去），于是用户只打了「视频」，拿到的却是 `@A宝好腻害！ 视频`。候选由
`command::spoken_bodies` 给（正文不带 @ 时只有整段一个），这里挑「去首尾空白与
句末标点后整段相等」的那一截——昵称里可能带空格，名字的边界猜不出来。名字之外的
话（「这个视频不错」）照旧不算取片请求。资讯提取那条路用的是同一个函数，见
`docs/INTERACTION.md` 第三节。

## 链接准入

`is_video_link` 是唯一的判据，`webshot` 也读它：同一条链接不会既回一条预览又被截
一张图。认的是 B 站的**稿件页**（`/video/BV…`、`/video/av…`，含 `m.` 与 `/s/` 前缀）
与分享短链（`b23.tv`、`bili2233.cn`）。番剧、直播、空间、专栏、动态页仍归截图。

链接不一定写在正文里。QQ 的小程序卡与分享卡都是整段 JSON，整段落在消息的 `<json>`
元素里，而且载荷里的斜杠常被转义成 `\/`，在字符串上找不到 `https://`。
`command::card_target_url` 按 JSON 解析这段载荷取「点开这张卡会去哪」的地址：
`qqdocurl`（小程序真正打开的页面）优先于 `jumpUrl`（分享卡的落地地址），
`icon` / `preview` 那些图片地址不取。`command::message_links` 把卡片里的地址排在
正文前面一起交给插件，插件取第一个认得的稿件页——所以一个群里贴链接和发卡片
都会回预览。

## B 站的几个接口事实

- 稿件信息走 `x/web-interface/view`，取流走 `x/player/playurl`。
- 请求参数用 `fnval=1`（要 mp4 的 `durl`，不要 DASH）：一个文件、不用合流，
  每一段的字节数还写在回包里，所以挑画质之前就能算准体积。
- **UA 必须是桌面浏览器**。分片地址里带着 `platform=pc`，CDN 会按 UA 复核，
  手机 UA 一律 403（实测同一个地址，`Mozilla/5.0` 拿 200、Chrome/Android 拿 403）。
- `Referer: https://www.bilibili.com` 同样必需，缺了分片回 403。
- 未登录时只放 360P/480P，多数稿件给到 720P。要 1080P 及以上得在 `cookie` 里填
  登录态。接口回 `-352` 是风控拦下，也是这一栏能救的。
- 老稿件的 `durl` 会切成好几段，顺序拼进同一个文件即可（它们本来就是同一路 mp4
  的连续分片）。
- 封面走 `data.pic`，`transparent.png` 那种占位图会被丢掉。

## 配置

见 `config.example.toml` 的 `[video_parse]`：`max_size_mb`（默认 80）、
`prefer_quality`（默认 64）、`send`（默认 `both`，与 `[oai] video_send` 同一套写法）、
`timeout_seconds`、`ack_after_seconds`（默认 20 秒。取片慢就先回一句「正在取片」，
0 表示什么都不说）、`cookie`、`hint` 与 `[video_parse.channel]` 群名单。

群名单走的是一般的黑白名单：`black` 里的群一律不解析，`white` 非空时只解析名单内
的群。线上 818965288 与 924989840 在 `black` 里——那两个群已经有别的解析机器人。
被名单拦下的链接连截图都不进（`webshot` 本来就把视频站链接让给本插件），所以那两个
群里我们一句话都不说。

## 验证

```sh
cargo test --bin ayjx                                       # 分流、文案、状态机
cargo test --bin ayjx live_reads_the_metadata -- --ignored --nocapture
AYJX_VIDEO_PARSE_LIVE_GROUP=280183116 \
  cargo test --bin ayjx live_takes_the_video -- --ignored --nocapture
AYJX_VIDEO_PARSE_LIVE_GROUP=280183116 \
  cargo test --bin ayjx live_sends_the_preview -- --ignored --nocapture
```

`live_takes_the_video_when_the_platform_adds_an_at` 重放真机上那条失败的引用回复
（记录 id 113798：用户只说了「视频」，正文被平台补成 `@A宝好腻害！ 视频`），跑法
同上。

后几条会真的往沙盒群发消息（预览一条、成品三条），跑完自己撤回。要确认「发出去了
没有」看实现端的 `message.list` / `message.get`，不要看 ayjx 的日志。`[Chat] 发送 ->`
只说明打算发。

## 加站点

目前只做了 B 站。要加一个站点：在 `is_video_link` 里认它的链接（短链可以先承认、
再像 B 站那样跟随一次跳转），再补一份「拉信息 + 取流」的实现。取片那条路只要求
最后拿到一组可直下的分片地址与它们的字节数，别处不用改。
