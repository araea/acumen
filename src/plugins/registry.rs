// 插件注册表：框架里唯一需要为「新增一个插件」改动的文件。
//
// 一条记录同时决定三件事：
//   1. 模块声明与流水线顺序 —— 宏按此顺序生成 `pub mod` 与执行序列；
//   2. 生命周期钩子 —— on_init / on_connected；
//   3. 帮助元数据 —— section / summary / commands，`/help` 与 `/ctl` 直接读，
//      不需要再去别处补一份清单。
//
// 字段说明：
//   config        配置类型（必填、放第一位）：实现 `PluginConfig`，默认配置、类型校验与
//                 读写都由它生成，模块里不再自写 default_config / validate_config
//   display_name  中文显示名（面向用户）；配置键与代码引用用标识符（模块名）
//   section       帮助总览的分区代号，取值见 `help::SECTIONS`；省略即「其他」
//   summary       一句话说明；`cargo test` 会检查每个插件都写了
//   commands      指令清单，`cmds![("指令", "说明"), ...]`；后台插件省略
//
// 指令写法：`/` 前缀由框架按配置拼上，这里只写指令本体；
// 符号指令（`/#`、`~名`、`##`、`-#`、`-*`）自带写法，不会被再拼前缀。
// 别名用 ` / ` 分隔，渲染时首个抬为主指令，其余降级为别名。
// 参数占位符统一 `<必填>` / `[可选]`，图上会单独着色。

register_plugins!(
    meta_filter {
        config: meta_filter::FilterConfig,
        display_name: "元事件过滤",
        section: "system",
        summary: "过滤心跳与元事件，避免噪声进入流水线"
    },
    ctl {
        config: ctl::Config,
        display_name: "插件控制",
        section: "system",
        summary: "统一管理全部插件开关与配置；仅号主与 ctl.admins 可改，首次启用自动初始化，排期修改下次连接生效",
        commands: cmds![
            ("ctl / 控制 / 插件", "查看完整用法和示例"),
            ("ctl list [on|off|关键词]", "查看全局状态及待重启提示"),
            ("ctl on <插件…>", "批量启用，支持英文名及中文显示名"),
            ("ctl off <插件…>", "批量停用；保留 ctl 管理入口"),
            ("ctl show <插件> [路径]", "查看配置，密钥隐藏"),
            ("ctl defaults <插件> [路径]", "查看默认配置"),
            (
                "ctl set <插件> <路径> <值>",
                "校验后保存；支持数组、表及点分路径"
            ),
            (
                "ctl reset <插件> [路径] --confirm",
                "恢复默认；整插件重置保留开关与管理员"
            ),
            ("ctl diff <插件>", "比较当前配置与默认值"),
        ]
    },
    console {
        config: console::Config,
        display_name: "本机控制台",
        section: "system",
        summary: "把运行状况、插件配置、搭话与日志摆到本机网页上；只绑回环地址并要口令",
        on_init: Some(console::init),
        on_connected: Some(console::on_connected)
    },
    logger {
        config: logger::LoggerConfig,
        display_name: "日志输出",
        section: "system",
        summary: "将收到的消息打印到控制台日志"
    },
    recorder {
        config: recorder::RecorderConfig,
        on_sent: Some(recorder::on_sent),
        display_name: "消息记录",
        section: "data",
        summary: "把消息记录到数据库，为词云、统计等插件提供数据源",
        on_init: Some(recorder::init)
    },
    lurker {
        config: lurker::Config,
        display_name: "潜水清理",
        section: "group",
        summary: "找出长期不说话的群成员：名单出成长图（带序号、分节、头像），群主管理员按序号挑人、确认后低速分批移出，中途冒泡的留下；可先 @ 提醒；机器人离线过也不会冤枉人；自动清理默认关",
        on_init: Some(lurker::init),
        commands: cmds![
            ("潜水 [天数]", "群管理：查一遍本群谁在潜水，名单带序号；写天数只这一次按它看"),
            ("潜水 踢 <序号…>", "群管理：移出这些人，如「潜水 踢 1-20 25」「潜水 踢 全部 除 3 7」；回「确认」才动手"),
            ("潜水 提醒 <序号…>", "群管理：先 @ 他们打个招呼，说清宽限几天"),
            ("潜水 进度 / 停 / 继续", "群管理：看进展、停下、接着被打断的那批"),
            ("潜水 豁免 @某人 / 取消豁免 @某人", "群管理：这个人永远不进名单；只发「潜水 豁免」看名单"),
            ("潜水 天数 <天数|默认>", "群管理：本群多少天没说话算潜水"),
            ("潜水 记录", "群管理：最近几次移出了谁"),
        ]
    },
    welcome {
        config: welcome::Config,
        display_name: "入群欢迎",
        section: "group",
        summary: "进群 @ 欢迎（多种说法随机、可配图配头像、可自动撤回）、退群送别；可选入群验证（口令、验证码或算术，倒计时提醒、答错提示，超时移出或禁言）与加群申请自动审批",
        on_connected: Some(welcome::on_connected),
        commands: cmds![
            ("欢迎", "群管理：本群的欢迎与验证设置、待验证名单"),
            ("欢迎 预览 [送别|移出|验证|提醒|答错|通过|超时]", "群管理：把这条提示对自己发一遍看效果"),
            ("欢迎 放行 @某人", "群管理：让待验证的新人直接通过"),
        ]
    },
    anti_recall {
        config: anti_recall::Config,
        on_receive: Some(anti_recall::on_receive),
        display_name: "防撤回",
        section: "group",
        summary: "有人撤回消息时把原文转存到指定私聊或群；群黑白名单可收窄范围，连撤多条合并成一条转发"
    },
    nailong {
        config: nailong::Config,
        display_name: "奶龙提醒",
        section: "group",
        summary: "认出群里的奶龙、奶蛙图，把原图与来龙去脉发到私聊或指定群，由管理员确认后处理；可选在账号是管理员时顺手撤回、禁言（默认关）",
        on_init: Some(nailong::init),
        commands: cmds![
            ("奶龙 / 奶蛙", "本群的识别状态与今天发现几张"),
            ("奶龙 测", "引用一张图（或附图）看奶龙、奶蛙的分数"),
            ("奶龙 是 [奶龙|奶蛙]", "管理员：引用漏掉的图，往后同类图也提醒"),
            ("奶龙 不是", "管理员：引用误报的图或私聊里的提醒，记住不提醒；禁过言的一并解除"),
            ("奶龙 忘了", "管理员：引用教过的图，撤销那次纠正"),
            ("奶龙 记录", "近几天发现的，带编号"),
            ("奶龙 看 <编号>", "私聊里再看一遍那张图"),
        ]
    },
    keyword_guard {
        config: keyword_guard::Config,
        display_name: "违禁词",
        section: "group",
        summary: "群管理：消息里出现设定的词（也能写正则）就按规则提醒、撤回、禁言或撤回并禁言；提示可静默、可定时自动撤回，管理员与豁免名单不动手，刷屏只处理一次不刷提示",
        on_init: Some(keyword_guard::init),
        commands: cmds![
            ("违禁词", "群管理：本群规则、动作与提示设置"),
            ("违禁词 添加 <词> [动作] [时长]", "群管理：多个词用「、」隔开，写成 /正则/ 按正则比"),
            ("违禁词 改 <编号> <动作> [时长]", "群管理：仅提醒 / 撤回 / 禁言 / 撤回禁言"),
            ("违禁词 模式 <编号> <包含|正则>", "群管理：切换怎么比"),
            ("违禁词 全含 <编号> [开|关]", "群管理：多个词要全部出现才算对上"),
            ("违禁词 停用 <编号>", "群管理：暂时不生效"),
            ("违禁词 启用 <编号>", "群管理：恢复生效"),
            ("违禁词 删除 <编号…>", "群管理：删 1 条或多条；删错可恢复"),
            ("违禁词 恢复", "群管理：找回最近删掉的一条"),
            ("违禁词 试 <一句话>", "群管理：不发出去，看会不会触发、由哪条处理"),
            ("违禁词 全局 …", "号主：维护所有群共用的规则，用法同上"),
        ]
    },
    media {
        config: media::Config,
        display_name: "媒体转换",
        section: "media",
        summary: "媒体与链接互转：图片或视频 ↔ 直链",
        commands: cmds![
            (
                "转链接 / 看链接 / 提取地址 / url",
                "将图片或视频转为直链（可引用消息）"
            ),
            ("转图片 / 预览", "将链接转为图片发送"),
            ("转视频", "将链接转为视频发送"),
        ]
    },
    sticker {
        config: sticker::Config,
        display_name: "表情收藏",
        section: "media",
        summary: "收藏对方发送的表情或图片（需引用原消息）",
        commands: cmds![
            ("收 / 偷 / 存表情", "引用表情或图片后收藏"),
            ("表情转图片", "引用动画表情后转为静态图片"),
        ]
    },
    group_title {
        config: group_title::Config,
        display_name: "群头衔",
        section: "chat",
        summary: "机器人为群主时，给申请者设置群专属头衔",
        commands: cmds![("我要头衔 <文字>", "给自己申请一个群专属头衔")]
    },
    essence {
        config: essence::Config,
        display_name: "群精华",
        section: "group",
        summary: "群主或管理员引用一条消息发「加精」收进群精华，「取消加精」移出；机器人需要是群主或管理员",
        commands: cmds![
            ("加精 / 设精华", "群管理：引用一条消息，把它设为群精华"),
            ("取消加精 / 撤精", "群管理：引用一条精华消息，把它移出群精华"),
        ]
    },
    recall {
        config: recall::Config,
        on_sent: Some(recall::on_sent),
        display_name: "消息撤回",
        section: "chat",
        summary: "引用后发送撤回指令；用户撤回触发消息时自动撤回对应回复（可关闭）",
        commands: cmds![("撤回", "引用要撤回的消息后发送")]
    },
    echo {
        config: echo::Config,
        display_name: "消息回显",
        section: "chat",
        summary: "回显参数内容（支持图片等富文本）",
        commands: cmds![("echo <内容>", "原样回显参数")]
    },
    markdown {
        config: markdown::Config,
        display_name: "Markdown 转图",
        section: "media",
        summary: "把 Markdown 渲染成排版精致的图片：指令后直接写，或引用一条消息再发指令；长文自动分页",
        commands: cmds![
            (
                "md / markdown / 渲染 <Markdown>",
                "指令后直接写内容，可多行；也可引用一条消息只发指令"
            ),
        ]
    },
    repeater {
        config: repeater::RepeaterConfig,
        on_receive: Some(repeater::on_receive),
        on_consumed: Some(repeater::interrupt),
        on_sent: Some(repeater::on_sent),
        display_name: "复读机",
        section: "chat",
        summary: "同一句话接力到阈值就跟读一次；可设冷却与跟读概率，也可以按概率主动打断这次接力"
    },
    wordcloud {
        config: wordcloud::WordCloudConfig,
        display_name: "词云",
        section: "data",
        summary: "根据消息记录生成词云图",
        commands: cmds![
            (
                "<范围><时间>词云",
                "范围：本群、跨群、我的，省略即本群；时间：今日、昨日、本周、上周、近7天、近30天、本月、上月、今年、去年、总"
            ),
            ("今日词云", "示例：本群今日"),
            ("我的总词云", "示例：个人全部"),
        ]
    },
    stats {
        config: stats::StatsConfig,
        display_name: "统计图表",
        section: "data",
        summary: "群统计图表：发言、发言字数、文本消息、表情、消息类型排行榜与走势，本群禁言榜（被禁言的人、动手的管理员，按次数、累计时长、单次最长或灰条所写时长），被艾特与艾特别人榜，支持早中晚与周月的错峰定时推送",
        commands: cmds![
            (
                "<范围><时间><类型><图表>",
                "范围：本群、跨群、我的、所有群；时间：今日…总；类型：发言、发言字数、文本消息、表情包、消息类型；图表：排行榜、走势"
            ),
            ("本群今日发言排行榜", "示例"),
            (
                "本群今日发言排行榜 30",
                "示例：排行榜后跟数字，列出前 30 名"
            ),
            ("本群本周发言走势", "示例"),
            (
                "本群本周发言字数排行榜",
                "谁写的字最多：每条消息里的文字字符数相加；图片、表情包本身不算字，榜上另写条数"
            ),
            (
                "本群本周文本消息排行榜",
                "谁发的纯文字消息最多：图片、表情包、语音、视频、音乐、合并转发都不算；榜上另写字数"
            ),
            (
                "本群本月禁言排行榜 / 本群总禁言时长排行榜",
                "谁被禁言次数最多 / 谁被禁言累计最久（每次实际禁言的时长加总，提前解除的只算到解除，还没到期的只算到此刻，所以一天最多 24 小时；也可写「禁言总时长」）；只有本群的排行榜，只记录机器人在线时发生的禁言"
            ),
            (
                "本群本月单次最长禁言排行榜",
                "谁被禁得最久的一次（每人取最长的那一次，也是按实际禁言的时长，还没到期的只算到此刻；也可写「最长禁言」）"
            ),
            (
                "本群本月灰条禁言时长排行榜",
                "把灰条上写的禁言时长相加（禁言 1 天、几秒后解除也记 1 天，与「禁言时长」按实际时长算不同；也可写「标称禁言时长」）"
            ),
            (
                "本群本月管理员禁言排行榜 / 本群总管理员禁言时长排行榜",
                "谁禁言禁得最多 / 累计禁得最久（含群主）；灰条里没写操作者的禁言不计入，榜上会注明；「管理员单次最长禁言」「管理员灰条禁言时长」同理"
            ),
            ("所有群近7天发言排行榜", "示例：跨全部群"),
            (
                "本群本月被艾特排行榜 / 本群本月艾特别人排行榜",
                "谁被 @ 得最多 / 谁最爱 @ 人；@全体成员不算，机器人自己两侧都不进榜；艾特记录从本版起才开始记，只有排行榜没有走势，「我的」范围排的是各个群"
            ),
        ],
        on_connected: Some(stats::on_connected)
    },
    gif {
        config: gif::Config,
        display_name: "GIF 工具箱",
        section: "media",
        summary: "GIF 合成、变速、倒放、缩放、拆分与拼图",
        commands: cmds![
            ("gif帮助 / gifhelp", "GIF 工具使用帮助"),
            ("合成gif", "多张图片合成 GIF"),
            ("gif变速", "调整播放速度"),
            ("gif倒放", "反向播放"),
            ("gif信息", "查看 GIF 帧数、尺寸等"),
            ("gif缩放", "调整 GIF 尺寸"),
            ("gif旋转", "旋转角度"),
            ("gif翻转", "水平、垂直翻转"),
            ("gif拆分", "拆分为单帧图片"),
            ("gif拼图", "多张图片拼接"),
        ]
    },
    image_split {
        config: image_split::Config,
        display_name: "图片切分",
        section: "media",
        summary: "将一张图按行列切片",
        commands: cmds![("裁剪 <行>x<列> / 切图 / 分割", "如：裁剪 3x3")]
    },
    qr_scan {
        config: qr_scan::Config,
        display_name: "二维码",
        section: "media",
        summary: "二维码识别与生成：图里的码转成文字与链接（多个码在原图上标序号），文字转成好扫的二维码图，引用一条消息即可",
        commands: cmds![
            (
                "扫码 / 识别二维码",
                "图片附在指令前后，或引用一张含二维码的图片再发指令"
            ),
            (
                "生成二维码 <内容> / 转二维码 / qrgen",
                "把文字或链接转成二维码图；不写内容就转所引用那条的文字，裸域名自动补 https://"
            ),
            (
                "二维码 / qr",
                "有图就识别，没图而后面有文字就生成"
            ),
        ]
    },
    video_parse {
        config: video_parse::Config,
        display_name: "视频解析",
        section: "media",
        summary: "视频站链接（正文、小程序卡、分享卡）就地取原片进群；同一条消息默认全解析，可限条数并随机间隔；平台可配（默认仅 B 站，抖音可选开）"
    },
    song {
        config: song::Config,
        display_name: "点歌",
        section: "media",
        summary: "点歌 <关键词>：B 站搜歌并由模型挑选，只回成品视频一条消息，不报错不吭声",
        commands: cmds![
            ("点歌 <歌名或关键词>", "找一首歌发进群；没写歌手名优先原唱与官方 MV"),
        ]
    },
    webshot {
        config: webshot::Config,
        display_name: "网页截图",
        section: "media",
        summary: "给消息里的网页链接自动截图（视频站链接交给「视频解析」）；manual_channels 名单内的群不自动截，可用指令手动触发",
        commands: cmds![
            ("截图 <链接> / webshot", "手动截图：链接写在指令后面，或引用一条含链接的消息"),
        ]
    },
    autoreply {
        config: autoreply::Config,
        display_name: "自动回复",
        section: "chat",
        summary: "群里问到设好的关键词就自动回（可带图、表情，或一整条合并转发）；每群单独设置，防误触（精确 / 包含 / 开头 / 正则、字数上限、冷却与限流），群管理员用指令或在控制台增删",
        on_init: Some(autoreply::init),
        commands: cmds![
            ("问答", "列出本群能问的触发词（所有人可用）"),
            ("自动回复 [列表]", "群管理：本群的自动回复，带编号"),
            ("自动回复 添加 <触发词> <回复>", "群管理：本群说到触发词就回；触发词写成 /正则/ 按正则比；可带图，也可引用一条消息或合并转发当回复"),
            ("自动回复 加说法 / 改 <编号> <回复>", "群管理：再加一种回复（随机一种）/ 换掉全部回复"),
            ("自动回复 模式 <编号> <精确|包含|开头|正则>", "群管理：怎么比；精确最不会误触"),
            ("自动回复 停用 / 启用 / 删除 <编号>", "群管理：共用规则停用只管本群；删错了发「自动回复 恢复」"),
            ("自动回复 试 <一句话>", "群管理：这句会不会触发、由哪条回"),
            ("自动回复 帮助", "全部用法：加词、删词、转发、艾特、冷却、全局添加"),
        ]
    },
    oai {
        config: oai::OaiConfig,
        display_name: "智能对话",
        section: "ai",
        summary: "多房间智能体对话：每间房按需开工具（无工具 / 联网 / 全量），模型、历史各自管理",
        commands: cmds![
            ("oai", "接入状态（默认模型、各家供应商、联网走哪儿）与完整指令；oai 开头的指令带不带前缀都认"),
            ("oai <供应商> <密钥>", "接上一家模型供应商，如 oai deepseek sk-…；地址自动补，先验密钥再存"),
            ("oai 用 <模型>", "换默认模型（所有没单独写模型的房间都跟着换），可写供应商名、供应商/模型或 /% 里的序号"),
            ("oai <名字> <地址> <密钥>", "接自己的中转站；oai <地址> <密钥> 配默认接口"),
            ("##<名称>(<描述>) <模型> <提示词>", "建房间；模型可省（跟随默认模型），新房间不挂工具"),
            ("<名称> <内容>", "与智能体对话；名称前加 ~ 为临时对话"),
            ("<名称>?<档位>", "切换工具：无工具 / 联网 / 全量（全量只有号主与管理员能开）；只写 ? 看当前档位"),
            ("<名称>%<模型>", "换模型，不动工具档位；%默认 跟随默认模型"),
            ("<名称>!", "停止智能体回复"),
            ("-#<名称>", "删除智能体"),
            ("<名称>~#<新名>", "复制智能体"),
            ("<名称>~=<新名>", "重命名智能体"),
            ("##:<描述…>", "自动填充智能体描述"),
            ("/#", "智能体列表"),
            ("/%", "模型列表"),
            ("-*", "清空所有智能体的公开历史（保留智能体）"),
            ("-*!", "清空所有智能体的公开与私有历史（保留智能体）"),
        ],
        on_init: Some(oai::init),
        on_connected: Some(oai::on_connected)
    },
    ambient {
        config: ambient::AmbientConfig,
        display_name: "群聊搭话",
        section: "ai",
        summary: "内置智能体以固定人格参与群聊，只在 [ambient] groups 的群生效",
        on_init: Some(ambient::init)
    },
    ai_news {
        config: ai_news::AiNewsConfig,
        display_name: "AI 资讯推送",
        section: "ai",
        summary: "AI 资讯、热点、日报周报月报与模型榜（数据源 AIHOT），引用卡片后回复序号取链接",
        commands: cmds![
            (
                "ai资讯 / ai新闻 [分类] [24小时|7天]",
                "最近的 AI 精选资讯；可只看某一类（模型、产品、行业、论文、教程、观点），也可改看 24 小时或 7 天"
            ),
            (
                "ai热点 [名次]",
                "当前 AI 热点榜 Top 10；带名次展开那个事件的完整综述与报道时间线"
            ),
            ("ai日报 [日期]", "最新一期 AI 日报；写明日期（2026-10-17）看那一天的"),
            ("ai周报 [期号]", "最新一期 AI 周报；写明期号（2026-W41）看那一周的"),
            ("ai月报 [月份]", "最新一期 AI 月报；写明月份（2026-09）看那个月的"),
            (
                "ai模型排行榜 / ai模型榜 / 模型排行榜 / 模型榜 [编程|科研|专业办公|知识问答]",
                "AIHOT 大模型排行榜：AIHOT 评分、评测覆盖与官网参考价，可按能力分类看"
            ),
            (
                "ai搜索 <关键词> [-- 分类 24小时|7天]",
                "按关键词检索 AI 资讯；-- 后可加论文、24小时等筛选"
            ),
            (
                "ai推送添加 <群|私聊> <ID>",
                "从任意群或私聊添加指定推送目标"
            ),
            (
                "ai推送删除 <群|私聊> <ID>",
                "从任意群或私聊删除指定推送目标"
            ),
            (
                "ai推送开启 / ai推送关闭",
                "不带参数时开启或关闭当前群或私聊的推送，也可指定目标"
            ),
            ("ai推送列表", "查看全部群与私聊推送目标"),
            ("ai实时开启 / ai实时关闭", "当前或指定目标是否接收实时快报"),
            ("ai实时模式 <精选|全部>", "默认仅推精选；可切换实时资讯来源"),
            (
                "ai分类 <分类>",
                "当前目标独立选择模型、产品、行业、论文、教程观点或全部"
            ),
            (
                "ai静默 <时间段>",
                "当前目标独立设置实时静默时段，如 23:30-07:30"
            ),
            (
                "ai推送状态 [目标]",
                "查看当前或指定目标的开关、实时参数与排期"
            ),
            ("ai推送重置 [目标]", "清空当前或指定目标的去重记录"),
            (
                "ctl set ai_news card_theme auto",
                "阅读主题自动切换；也可使用 light、dark"
            ),
        ],
        on_init: Some(ai_news::init),
        on_connected: Some(ai_news::on_connected)
    },
    help {
        config: help::Config,
        display_name: "帮助中心",
        section: "system",
        summary: "按当前注册表列出全部插件与开关状态，配置管理用 ctl",
        commands: cmds![
            ("help / 帮助 / 插件列表", "插件总览"),
            ("help <插件名>", "查看插件详情与全部指令"),
        ]
    },
    restart {
        config: restart::RestartConfig,
        display_name: "更新与重启",
        section: "system",
        summary: "群里直接查版本、查新版本并一键换上（自动下载、校验、平滑重启）；另有启动、周期与每日定时查新，以及内存阈值监控",
        commands: cmds![
            ("版本 / version", "当前版本、是不是发布版、有没有新版"),
            ("查更新 / 检查更新 / check", "仅限 ctl.admins：立即查一次有没有新版，不安装"),
            ("更新 / 升级 / update", "仅限 ctl.admins：下载校验并换上，随后平滑重启"),
            ("restart", "仅限 ctl.admins；需开启 allow_manual_restart"),
        ],
        on_init: Some(restart::init)
    },
);
