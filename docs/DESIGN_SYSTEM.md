# 知微统一设计系统 · Material 3 Expressive

适用于主框架、控制台以及全部注册插件的视觉、交互、信息内容与文案。旧版混合视觉体系（包括 squircle 与另一套字阶、间距来源）已废弃；不借其它设计系统的颜色、组件或装饰。

实现入口：`res/console/tokens.css`（共享基础令牌）、`app.css`（组件与响应式布局）、`app.js`（状态与交互）、`index.html`（语义外壳）。

## 原则与裁决

视觉只用 Material 3 Expressive（M3E）：HCT 动态角色色、表面层级、15 级字阶、形状级、状态层、空间 / 效果动效、底部导航 / 导航轨、分段列表与控制组件。Apple HIG 仅用于平台交互体验：系统字体、安全区、系统深浅 / 增强对比 / 减少动态偏好、iOS 输入防缩放、系统选择器与原生对话框。不使用 iOS 风格控件或其它视觉体系。

发生冲突时，**平台惯例、可用性与无障碍优先于视觉一致性**；始终以 WCAG 2.2 AA 为下限。

## Tokens

- `scripts/make-tokens.py` 根据靛青种子 `#3f51b5`、Material Color Utilities 的 2025 Tonal Spot 生成浅色、深色及各自增强对比的 `--md-sys-color-*`。不要手改生成块。`success`、`warning` 是 harmonize 后按相同方案生成的自定义角色，信息还必须带文字。
- `--md-sys-typescale-*`、`--md-sys-shape-*`、`--md-sys-motion-*`、`--md-sys-state-*`、`--md-sys-elevation-*` 是视觉令牌；`--zw-*` 只放产品几何映射（4dp 间距基线、尺寸、安全区、焦点环）。组件层不另造全局令牌，不硬编码颜色 / 形状 / 字号。
- 同一脚本生成 `res/cards/tokens.css` 与 `src/render/tokens.rs`，卡片、原生统计图与词云不得手抄配色；卡片的语义角色与控制台一致。卡片根字号 20px，用同一 rem 字阶适配群聊缩略图，空间间距按 4px 基线映射。
- 颜色配对成套使用：`primary/on-primary` 主操作；`primary-container/on-primary-container` 运行卡；`secondary-container/on-secondary-container` 选中；`tertiary-container/on-tertiary-container` 插件总数强调；`surface-container-*` 其余表面。`outline` 用于需要 3:1 的控件边界，`outline-variant` 仅用于装饰线；文字不使用 `outline`。
- 字体使用系统无衬线，日志 / 代码使用系统等宽，数据用 `tabular-nums`。字号用 rem，输入框不得小于 16px。圆角使用 M3E shape scale，不启用 squircle 或其它形状体系。
- **聊天卡片的字体与控制台不同**：卡片在服务端出图，读者的平台字体用不上，而 `system-ui` 在出图机上会落到 DejaVu Sans（拉丁字母又宽又散，汉字另走只有 Regular 一档的思源黑体，500 / 600 字重出不来）。所以 `scripts/make-tokens.py` 在卡片令牌里把 MiSans 排在最前——汉字拉丁一体、字重 Thin–Heavy 齐全；没装它的机器照旧走控制台那一串。装在 `$PREFIX/share/fonts/TTF/MiSans`，属设备本地状态。
- 颜色反馈使用状态层（hover 8%，focus / pressed 10%）；位置与尺寸用 spatial fast / default（350 / 500ms），透明度 / 颜色用 effects（150 / 200 / 300ms）。减少动态时空间动画归零、循环动画停用。阴影仅用于浮层，内容卡片以容器色分层。

## 布局

| 视口 | 导航 | 内容 |
| --- | --- | --- |
| <600px（compact） | 64px 底部栏 + 安全区 | 顶栏 64px；16px 页边距；层级详情独立页 |
| 600–839px（medium） | 96px 导航轨 | 24px 页边距 |
| 840–1199px（expanded） | 248px 展开轨 | 插件列表与详情并排；总览运行 / 统计双栏 |
| ≥1200px（large） | 同上 | 32px 页边距、1240px 最大宽；运行区略宽于统计区 |

总览按运行状态 → 指标 → 连接 → 最近日志组织；手机按 DOM 顺序纵排，桌面按网格视觉编排。插件详情在宽屏独立滚动，窄屏可返回列表。大标题在正文，滚出后顶栏接管。任何断点均不得出现页面横向滚动。

## 组件与交互契约

- **导航**：五个真实链接；当前页 `aria-current="page"`。路由更新文档标题，切页焦点进入 h1，前进 / 后退有效。
- **按钮 / 开关 / 筛选**：filled > tonal > outlined > text；开关为 52×32 M3 轨道、`role="switch"` 与 `aria-checked`，名字不随状态变；筛选芯片使用 `aria-pressed`，选中有形状 / 勾选标记而非只变颜色。`aria-checked`、`aria-pressed`、`aria-selected` 一律写成 `"true"` / `"false"`，不留空串（`h``` 会把 `false` 渲染成空，空串等于没写，读屏读不出「关」）。触屏目标 ≥48px，精确指针 ≥40px。
- **列表 / 编辑**：分段列表外端 20px、组内 4px，整行链接与尾部开关是两个独立命中目标；输入标签保持可见，错误在本字段下说明并以 `aria-invalid`、`aria-describedby` 关联。保持标签可见优先于浮动标签的视觉模式。
- **页签 / 对话框 / 提示**：页签支持方向键、Home、End；原生 dialog 初始焦点在取消，Esc 返回触发者；提示可关闭，悬停 / 聚焦暂停倒计时，错误提示保留至关闭或后续反馈；成功走 `status`，失败走 `alert`。
- **日志**：等宽数据行带级别文字，可键盘滚动；日志突发不逐行播报；前后台订阅、暂停、筛选、导出维持原行为。
- **安全**：动态 HTML 经 `h\`\`` 模板转义，只有可信片段用 `raw()`；不要在页面代码中直接拼未经转义的 `innerHTML`。

## 无障碍与验证

目标为 WCAG 2.2 AA：普通文本 ≥4.5:1，大字与有意义的非文本边界 ≥3:1；不只靠颜色传意；键盘可达、焦点清楚且不被顶栏 / 底栏遮挡；不设置单字符快捷键；320px 回流、200% 放大与文字间距可用；口令可粘贴及自动填充。自动审计不是无障碍认证；VoiceOver / TalkBack 与实际设备安全区需人工验收。

```sh
python3 scripts/make-tokens.py --check
cargo test --locked console
node tests/console.cjs
node tests/console-backend.cjs
python3 scripts/audit-contrast.py   # 对运行中的本机控制台只读检查
```

颜色与组件检查覆盖四种外观以及 320 / 390 / 800 / 1400px；测试还覆盖键盘、对话框、配置验证与日志突发。上线前必须运行隔离测试，并检查线上实际内嵌资源，不能只依据源码截图宣称通过。

隔离夹具生成的界面截图（演示数据，不是线上数据）：[桌面总览](design/overview-desktop.png) · [手机总览](design/overview-mobile.png) · [深色插件](design/plugins-desktop-dark.png) · [深色手机日志](design/logs-mobile-dark.png) · [导航轨](design/ambient-rail.png)。

## 聊天、内容与数据契约

- **信息卡版式**（手册、控制、智能体与模型列表共用 `render::web` 的文档模型与 `res/cards/reading.css`）：卡面统一 520 宽（成图 560，与智能回复卡同宽），手机全屏看时正文约 13dp，**内容多就让图变长，不让字变小**；一组条目用 M3E 分段列表（`md-seg`）收成一块——外端大圆角、组内小圆角、条间一道细缝，不再每条画一圈边。指令里照打的字用主色加粗，`<占位>` 与 `[可选]` 降一档；状态永远带文字，圆点另有实心 / 空心 / 带环三种形状；启用是常态，只给例外（已停用、待重启）挂标记。只剩一个成员的分组不单独立标题，并进「其他」，成员自己带上原本挂在标题上的那一项。
- help、ctl、oai（回复卡与两张列表）、ai_news 的信息卡出图成功后只发卡片图，不再补发等价文本；图片没出成（关掉出图或渲染失败）才退回文本。统计图与词云同此口径。长文本按适配器容量分段，资讯长内容沿用合并转发。图中来源保留完整 URL，OAI 卡片同时带来源与工具回执。`image_enabled=false` 仍可选择纯文本。
- 渲染不信任模型 HTML，原始标签作为文本转义。内置清单使用 Markdown，让图片与文本共用同一份内容。Markdown 卡的元素样式、分页与对比度检查见 [Markdown 转图](markdown.md)；代码着色只取 M3 角色色，前景与代码面的对比度由测试卡在 4.5:1 以上。
- 图表条长从零正比映射，名称与数值分开布局；多系列折线的编号同时出现在图例与数据点，不能仅凭颜色区分。
- 指令元数据以真实解析器为准，复制按钮只复制一个模板，按当前配置补词前缀；符号指令与智能体名称保持原样。尖括号表示需要替换的参数。
- 配置标签与说明来自配置参考；有限选项由后端校验规则提供。数组用 JSON 编辑，保留类型、引号与含逗号字符串，允许增删对象。包含已隐藏凭据的列表不可把占位文字当真实值提交。
- 保存成功仅指本次提交的数据已持久化；保存期间的新编辑必须继续排队或保留为草稿。OAI 保存采用原子替换，失败回滚内存并报告错误。刷新、断点切换与保存其他表单不得丢失输入或焦点；离开未保存表单需确认。**「未保存」只认用户亲手输入过且与基线不同的值**（`beforeinput` 标记）：打开页面什么都没改、点开再点关的开关、浏览器 / 密码管理器自动填充、扩展写值、表单恢复都不算，正在保存的控件也不算。访问令牌框只提交亲手输入的内容，自动填进来的（只可能是控制台口令）会被清掉；要清除已设的令牌用「清除已设的令牌」开关，留空只表示不改动。离开确认点名是哪几处没保存，默认焦点在「继续编辑」；取消后历史原路退回（后退栈不留重复条目，手势返回照常）。
- 删除智能体、删除历史、清空历史需操作者在相同会话中于 60 秒内重发同一指令确认；目标数据变化时重新确认。
- 用户主动指令的失败给出结果、原因与下一步；自动监听的无结果不强制制造群聊提示。全局白名单与插件 channel 是独立规则，界面必须描述真实优先级。
- 表情画廊默认静态第一帧，原图需主动播放，可随时停止。静态卡片无交互动效要求；控制台继续尊重减少动态与强制颜色偏好。
- Apple HIG 仅用于平台体验。用户提供的图片、视频、网页截图与角色创作内容不视为框架自定义视觉组件。

## 依据与验收边界

依据：[Material 3 Expressive](https://m3.material.io/)、[Google 官方 Material 3 实现说明](https://developer.android.com/develop/ui/compose/designsystems/material3)、[Apple HIG](https://developer.apple.com/design/human-interface-guidelines/)、[WCAG 2.2](https://www.w3.org/TR/WCAG22/)。

`node tests/console.cjs` 会从 Rust 注册表导出所有插件真实默认配置，再检查字段完整性、320 / 1400px 回流与对比度；独立的交互夹具用于确定性复现竞态。`node tests/console-state.cjs` 验证保存竞态与数组往返。`scripts/review-cards.sh` 生成所有卡片族与原生图表，检查完整布局与实际计算颜色对比度。`node tests/console-backend.cjs` 在无 Satori 适配器的隔离实例检查所有插件 API 与 release 内嵌资源。

自动测试与代码核验不能替代 VoiceOver、TalkBack、iOS 安全区与真实 QQ 客户端的人工验收；不得据此宣称获得 WCAG 认证。
