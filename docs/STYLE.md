# 代码规范

acumen、知弦（satori-qq）与知言（satori-wx）三个仓库共用同一套底线，再按语言落到各自的工具上。

## 通用

- `.editorconfig`：UTF-8、LF、文件末尾换行、去行尾空白、4 空格（Markdown、JSON、CSS 为 2 空格）。三个仓库同一份。
- 提交信息写成 `类型(范围): 一句话`，类型取 feat / fix / refactor / style / docs / test / chore / release，正文讲为什么。
- 不留死代码：没人调用的函数直接删，历史在 git 里。框架工具箱模块（`message` / `event` / `command` / `db` / `scheduler` / `api` / `render`）成套保留，是有意为之。
- Shell 脚本里写 `CDPATH='' cd -- …`；`bot` 先以 sh 启动再换成 bash，文件头标了 `# shellcheck shell=bash`。

## Rust

- edition 2024，`rust-version` 与 `Cargo.toml` 一致。`cargo fmt` 用默认配置，`cargo clippy --all-targets --locked` 保持零告警。
- `Cargo.toml` 的 `[lints]` 在默认 clippy 之外强制一组现代写法：`let … else`、格式串内联变量、方法引用代替冗余闭包、
  `Duration::from_mins` 一类更大的时间单位、去掉多余的限定路径与原始字符串井号。它们都能由 `cargo clippy --fix` 机械改写。
- 配置、落盘、日志目标、发送错误等统一入口见 [架构](ARCHITECTURE.md) 的「框架统一写法」，新增插件先照那里写。
