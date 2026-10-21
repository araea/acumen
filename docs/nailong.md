# 奶龙拦截

认出群里的奶龙、奶蛙图：撤回，按次数加码禁言；认错了，管理员引用机器人的提醒说一句「不是」就纠正、解禁，这类图往后不再拦。

默认关闭。打开：`/ctl on nailong`。第一张图到来时自动下载识图模型（约 45 MB，见[模型](#模型)），之后离线运行。

## 处理

群里有人发图（含动图），机器人在后台看，不拖慢别的插件：

1. **认出来** → 撤回那条，记一次。
2. 近 `window_hours`（默认 24）小时内第几次，按 `mute_seconds` 的阶梯处理：默认第一次只撤回提醒，第二次禁言 10 分钟，之后 1 小时、1 天。
3. 说一句提醒（@ 本人）：

```text
@小明 本群不收奶龙图，这张先撤了。
@小明 本群不收奶蛙图，这是第 2 次，禁言 10 分钟。
```

账号不是群主或管理员时撤不了也禁不了，只引用那张图提醒一句（`remind_without_admin = false` 则不说话）。群主、管理员与机器人管理员发的图不管（`exempt_staff`）。

没认出来什么都不说；不发「正在识别」之类的过程话。

## 指令

| 指令 | 谁能用 | 作用 |
| --- | --- | --- |
| `/奶龙` | 所有人 | 本群的拦截状态、今天拦下几张、模型是否就绪 |
| `/奶龙 测` | 所有人 | 引用一张图（或附图）看奶龙、奶蛙的分数和会不会拦 |
| `/奶龙 是 [奶龙\|奶蛙]` | 群主、管理员 | 引用漏掉的图：往后同类图就拦；不写类别按模型更像的那类 |
| `/奶龙 不是` | 群主、管理员 | 引用误判的图**或机器人的那条提醒**：记住不拦，解除这次禁言、撤回提醒、不计入次数 |
| `/奶龙 忘了` | 群主、管理员 | 引用教过的图：撤销那次纠正，回到模型自己判断 |

`/奶蛙` 与 `/奶龙` 等同。图被撤回后就引用不到了，所以误判时引用机器人的提醒——它记得当时是哪张图、禁了谁。

教过的图对所有群生效。新图和某张教过的图足够像（余弦相似度 ≥ 0.9，同一张图换个压缩、裁掉一圈边都在此之上）时以教的为准，所以模型漏掉的新变体，教一两张就认得了。

## 配置

```toml
[nailong]
enabled = false
targets = ["nailong", "naiwa"]   # 拦哪几类
threshold = 0.6                  # 判定阈值 0.3–0.95：高则少误判、多漏判
recall = true
mute_seconds = [0, 600, 3600, 86400]  # 第 1、2、3…次；0 = 只撤回；超出取最后一档
window_hours = 24
exempt_staff = true
ignore_users = []
remind_without_admin = true
max_images = 4                   # 一条消息最多看几张
max_frames = 4                   # 动图最多抽几帧（均匀抽，第一帧必看）
recall_text = "{at} 本群不收{kind}图，这张先撤了。"
mute_text = "{at} 本群不收{kind}图，这是第 {count} 次，禁言 {duration}。"
remind_text = "本群不收{kind}图，麻烦撤回一下。"

[nailong.channel]                # 群黑白名单；都空 = 全部群
white = []
black = []
```

占位符：`{at}` `{name}` `{kind}`（奶龙 / 奶蛙）`{count}` `{duration}`。文案留空则不说话，照常撤回与禁言。

## 模型

两段：

- **图像编码器**：[MobileCLIP-S0](https://huggingface.co/apple/MobileCLIP-S0) 的视觉部分（[Xenova 转的 ONNX](https://huggingface.co/Xenova/mobileclip_s0)，45 MB），把图压成 512 维向量。从 Hugging Face 下载，失败改走 hf-mirror.com；钉在固定提交，按 SHA-256 校验，存在 `data/nailong/`。权重随 Apple 的研究许可证，只在本机下载使用，不随发布包分发。
- **分类头**：两层小网络（512 → 256 → 3，约 0.5 MB，编进可执行文件），输出「其他 / 奶龙 / 奶蛙」三类概率。

推理用 [tract](https://github.com/sonos/tract)（纯 Rust），不依赖系统里的推理库。手机上一帧约 0.25 秒；同一张图（按内容摘要）再出现直接用上次的结论；同一时间只跑一张，排队超过 12 张时新来的不看，免得刷屏时积压到几分钟后才撤。

### 为什么不直接用现成的模型

2026-10 调研了公开的奶龙识别实现，在同一批图上实测（MobileCLIP 分类头一栏是留出集：这些来源没有参与训练）：

| 模型（阈值） | 奶龙（XiC1 测试集） | 奶蛙表情包（GeorgeNaNN） | 误判：普通表情包 | 误判：黄色易混图 |
| --- | --- | --- | --- | --- |
| [YOLOX v2.6 tiny](https://github.com/nkxingxh/NailongDetection)（0.5） | 197/235 | 136/145 | **93/533** | 63/352 |
| [YOLOv11n 奶龙](https://github.com/Threkork/kovi-plugin-check-alllong)（0.78） | 174/235 | **50/145** | 2/533 | 2/352 |
| [NaiLongKiller helicopter](https://github.com/SnifferCaptain/NaiLongKiller)（0.5） | 185/235 | 107/145 | 1/533 | 23/352 |
| MobileCLIP-S0 + 分类头（0.8，本插件） | 201/235 | 114/145 | 0/533 | 7/352 |

XiC1 的「奶龙」测试集是搜索引擎抓的，混着不少玫瑰、人像、广告，35 张左右本来就不是奶龙，所以那一栏到不了满分。动图按帧抽查，哪帧像就算。易混图是皮卡丘、小火龙、小黄人、海绵宝宝、黄发角色之类。

现有模型都训练于 2024 年，没见过 2026 年流行的**奶蛙**（AI 生成的写实「变异奶龙」），在奶蛙表情包上只认得出一半左右；YOLOX 版对普通表情包误判太多。所以换成通用图像向量 + 自己训的分类头，并留了「教」的口子应对以后的新变体。

### 训练数据

| 类别 | 来源 |
| --- | --- |
| 奶龙 | [XiC1/nailong-dataset](https://huggingface.co/datasets/XiC1/nailong-dataset) 训练集、[Logxk/nailong](https://github.com/Logxk/nailong) 图库、[whxoperator/nailongnaiwa](https://github.com/whxoperator/nailongnaiwa) |
| 奶蛙 | [Nailong-Studio/wallpaper](https://github.com/Nailong-Studio/wallpaper)、whxoperator/nailongnaiwa |
| 其他 | XiC1/nailong-dataset 训练集的通用表情包与黄色易混图（皮卡丘、小火龙、小黄人、海绵宝宝、黄发角色……） |

留出测试：XiC1 测试集（奶龙与其他）、[GeorgeNaNN/nailong-memes](https://github.com/GeorgeNaNN/nailong-memes)（奶蛙为主的表情包）。分类头是两层小网络（ReLU、dropout 0.2、Adam 60 轮、类别权重取样本数反比的平方根）：同样的数据上，逻辑回归在阈值调到普通表情包不误判时只认得出三到六成奶蛙。上表是只用训练来源训出的版本；发布的分类头用全部来源（含留出集）重训，超参不变。

### 重训

```sh
python3 scripts/nailong-head.py vision_model.onnx 数据目录 --eval
```

数据目录按 `nailong/`、`naiwa/`、`other/` 分类放图，`test/` 下同样三类是留出集（`--eval` 时只打分）。向量按图片内容缓存，加几张图再训只算新的。脚本写回 `src/plugins/nailong/head.bin`，重新构建即可。

### 复核

```sh
NAILONG_MODEL=data/nailong/mobileclip_s0_vision.onnx NAILONG_IMAGES=图片目录 \
  cargo test --release nailong_eval -- --ignored --nocapture
```

用插件真实的解码、抽帧、预处理与推理逐张打印两类分数与耗时。
