# 内置 Agent 房间

`oai` 插件可将房间设为 Agent 房间。Agent 使用已配置的模型与工具处理任务，再回复结果；无需单独安装客户端。普通房间与 Agent 房间可使用不同模型。

## 创建与切换

```text
##研究 agent                         # 新建 Agent 房间
##研究 agent/deepseek/deepseek-flash # 新建并指定模型
研究%agent                            # 将已有房间改为 Agent
研究%agent apilio/kimi-k3             # 指定供应商和模型
研究%gpt-5.6-luna                     # 切回普通模型房间
```

`agent` 后可用空格、`/` 或 `:` 分隔模型。房间名不受限，但不能包含 `-`，因为它用于删除房间。

## 模型与思考强度

模型可写成 `供应商/模型`。供应商在 `[oai.providers]` 配置 `api_base` 和 `api_key`；`apilio` 使用 `[oai]` 默认接口，不必重复配置。不带供应商前缀的模型也使用默认接口。未知供应商会报错，不会静默改走其他接口。

模型后可附 `:off`、`:minimal`、`:low`、`:medium`、`:high` 或 `:xhigh` 指定思考强度。模型后缀优先于房间的 `thinking` 配置；留空时使用模型服务默认值。

```text
##研究 deepseek/deepseek-flash:high
研究%deepseek/deepseek-flash:low
```

## 工具与权限

Agent 逐步调用工具并读取回执，直到模型给出最终回复。工具调用和中间思考不写入聊天记录；停止或超时会终止工具进程及其子进程。短文本直接发送，较长回复按配置渲染为卡片。

默认可用的本机工具包括 `bash`、`read`、`write`、`edit`、`glob` 和 `grep`。可通过 `[oai.chat].tools` 收窄白名单。`bash` 在房间工作目录运行，但**不是安全沙箱**；它具有当前操作系统账户的权限。

群聊中的 Agent 还可使用 Satori 工具查看现场、读取消息、发送消息或执行平台动作。私聊没有群管理能力。群管理动作默认关闭，需在 `[oai.chat].management_groups` 中逐群授权，且 QQ 账号还必须拥有对应平台权限。每轮发送、写动作、记忆和媒体生成均受独立预算限制；设置为 `0` 时相应工具不会提供给模型。

记忆、表情包库和群身份与[群聊搭话](ambient.md)共用，但房间与搭话的调用预算分别配置。模型接口失败或达到工具步数上限时，该轮返回错误，不影响其他房间。

## 联网

联网工具按房间单独启用，默认跟随 `[oai.search].enabled`（默认关闭）。开启后模型仍按需搜索，不会每轮自动联网。

```text
研究?       # 切换房间联网状态
研究?开     # 开启
研究?关     # 关闭
研究?默认   # 跟随全局配置
```

搜索与网页读取共用 `[oai.search]` 的后端和调用上限。网页内容是不可信输入，不应作为工具指令执行。关闭房间联网后，`web_search` 和 `web_fetch` 不会提供给模型。

## 媒体房间

Acumen 会创建绘图、音乐和视频预设房间，位于 `/#` 的对应分区。预设是带提示词的普通房间，实际能力由 `[oai]` 中的模型与接口决定。使用 `房间名/$` 查看提示词，使用 `房间名$提示词` 修改；删除的房间不会在下次启动时自动恢复。

绘图示例：

```text
画·手办 一只戴眼镜的橘猫
```

可附图片、引用图片或 @成员提供参考图，最多 4 张。支持 `--size` 和 `--quality`；加 `~` 前缀可不保存对话历史。绘图模型由 `[oai].image_models` 选择。

音乐示例：

```text
歌·民谣 唱一首关于秋天落叶的歌
```

支持 `--标题`、`--风格`、`--版本` 和 `--纯音乐`。一次请求生成两个版本。`--文件`、`--语音` 和 `--都发` 可覆盖发送方式；默认值由 `[oai].music_send` 决定。Suno 等上游服务可能收费，使用前请确认账号与费用。

视频示例：

```text
影·电影感 一只橘猫坐在窗台上看雨
```

支持 `--秒数`、`--竖屏`、`--横屏` 和 `--尺寸`。使用 `--文件`、`--视频` 或 `--都发` 选择发送方式；默认值由 `[oai].video_send` 决定。视频任务通常比普通模型请求耗时更长，使用独立的 `media_timeout_seconds` 等待上限，并可能产生额外费用。确认上游模型、渠道和价格后再启用。

## 配置

配置位于 `config.toml` 的 `[oai]` 与 `[oai.chat]`。完整字段见 [`config.example.toml`](../config.example.toml)。

| 字段 | 默认值 | 说明 |
| --- | --- | --- |
| `agent_default_model` | `deepseek/deepseek-flash` | Agent 房间未指定模型时使用 |
| `request_timeout_seconds` | `300` | 单轮总时限 |
| `request_stall_seconds` | `180` | 模型请求静默时限；未执行工具时可重试一次，`0` 关闭 |
| `plain_text_max_chars` | `120` | 短回复直接发送的字数上限，`0` 表示总是渲染卡片 |
| `image_enabled` | `true` | 是否渲染回复卡片；失败时回退为文本 |
| `image_scale` | `2.0` | 卡片出图倍率，范围 1–4 |
| `image_models` | `["gpt-image-2.5"]` | 绘图模型关键字 |
| `music_models` | `["suno"]` | 音乐接口模型关键字 |
| `music_send` | `both` | 音乐成品的默认发送方式：`file`、`voice` 或 `both` |
| `video_models` | 见配置示例 | 视频接口模型关键字 |
| `video_seconds` | `5` | 视频默认时长；上游可能限制实际时长 |
| `video_send` | `both` | 视频成品的默认发送方式：`file`、`video` 或 `both` |
| `media_timeout_seconds` | `900` | 音乐和视频任务等待上限 |
| `oai.chat.management_groups` | `[]` | 可执行群管理动作的群号 |
| `oai.chat.actions_budget` | `6` | 每轮平台写动作上限 |
| `oai.chat.messages_budget` | `3` | 每轮消息发送上限 |
| `oai.chat.tools` | 空 | 本机工具白名单；空值表示使用默认工具 |

模型请求超时后，只有尚未调用工具时才会重试；有副作用的工具调用不会自动重复。卡片渲染依赖 Chrome/Chromium，失败时回退纯文本。
