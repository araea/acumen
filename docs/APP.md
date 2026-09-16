# 应用形态 · Android

知言有两种形态，同一份核心。

```text
                 ┌──────────────────────────────┐
                 │  ayjx（Rust 核心）           │
                 │  23 个插件 · 一份配置 · 一个库 │
                 └──────────────┬───────────────┘
                                │ 本机 HTTP（回环 + 口令）
                 ┌──────────────┴───────────────┐
   形态一 终端    │  终端与 runit                 │
   形态二 应用    │  Android 壳（WebView）        │
                 └──────────────────────────────┘
```

两种形态不是两个程序。核心仍然是一个可执行文件，界面是它自己发出来的一张网页。壳只负责把核心跑起来、把那张网页放进 WebView。所以：

- 终端里 `./bot start`、`./bot logs`、`/ctl` 一条不少，没有图形界面也照常跑；
- 应用里看到的每一格都是核心此刻的真实状态，不是壳另存的一份；
- 关掉控制台服务（`[console] enabled = false` 或启动带 `--no-ui`），群里的一切也不受影响。

## 壳的两种用法

应用启动时会问一件事：核心在哪儿跑。

| 用法 | 什么时候用 | 怎么切 |
| --- | --- | --- |
| 自带核心（默认） | 只想装一个应用就用起来 | 首屏的「重新启动核心」 |
| 连已有实例 | 核心已经在 Termux 里由 runit 托管着——出图、字体、唤醒锁那一套都齐了 | 首屏的「连接到已有实例」，填那份实例带口令的地址 |

Termux 那份部署有 Chromium，卡片图能出。应用自带的核心在 Android 应用沙箱里跑，够不着任何浏览器，于是**出图那一路会安静地退回纯文本**（这是既有降级路径，见 [`INTERACTION.md`](INTERACTION.md) 第五节）。想让群里的卡片图照旧，就把核心留在 Termux，应用只当一块屏幕。

两种用法都只用本机回环：应用与 Termux 在同一台机器上，`127.0.0.1` 是通的。

## 打包

没有 Gradle。这台机器上有 `aapt` / `d8` / `zipalign` / `apksigner`，而壳只有三个 Java 类、一份清单、一个自带的核心，用不上构建系统。

```sh
bash app/build.sh                       # 全流程，产物 app/build/Zhiyan.apk
ZHIYAN_SKIP_CORE=1 bash app/build.sh    # 只重打壳，复用上一次编好的核心
```

六步：交叉编译核心 → 剥符号 → javac → d8 → aapt → 对齐签名。几个环境变量可以改路径：

| 变量 | 默认 | 用途 |
| --- | --- | --- |
| `ANDROID_JAR` | `~/android/platform/android-35/android.jar` | 编译与 d8 用的 android.jar |
| `ANDROID_BUILD_TOOLS` | `~/android/android-sdk-tools/build-tools` | `aapt` 与 `zipalign` 所在目录 |
| `R8_JAR` | `app/libs/r8.jar`，找不到就借 satori-qq 那份 | d8 在 r8 里 |
| `ZHIYAN_SKIP_CORE` | `0` | 设为 `1` 跳过 Rust 那一步 |

缺 r8：

```sh
curl -fsSL -o app/libs/r8.jar https://maven.google.com/com/android/tools/r8/8.9.35/r8-8.9.35.jar
```

### 为什么核心叫 `libayjx_core.so`

Android 10 起，`targetSdk` 29 以上的应用**不能对自家可写目录里的文件执行 execve**（W^X）。所以可执行文件不能先解包到 `filesDir` 再跑，只能随 APK 走、由系统解包到 `nativeLibraryDir`。那边的约定是「文件名以 `.so` 结尾」，内容是什么系统不管。

代价是 `nativeLibraryDir` 的父目录只读，而核心默认「数据落在可执行文件旁边」，那条约定就不成立了。所以核心认一个环境变量 `AYJX_DATA_DIR`：壳把它指到 `filesDir/core/data`，工作目录设成 `filesDir/core`，于是配置、数据库、插件数据全在一个可卸载即清空的目录里。

## 装机

```sh
su -c "cp app/build/Zhiyan.apk /data/local/tmp/ && pm install -r /data/local/tmp/Zhiyan.apk"
```

`/data/local/tmp` 应用身份写不进去，所以要先拷到那儿再 `pm install`。装完第一次打开：核心会从 `assets/config.toml` 落一份配置到 `filesDir/core/config.toml`（内容与仓库的 `config.example.toml` 一字不差，由构建脚本复制，不另存第二份），然后按 `[[bots]]` 去连实现端。默认地址就是本机的 `http://127.0.0.1:3001`。

改配置有三条路，都是同一份文件：

- 应用里「设置」页：连接（地址、令牌）、指令前缀、全局群名单；
- 应用里「插件」页：任意插件的开关与全部配置项；
- 直接编辑 `filesDir/core/config.toml`（需要 root 或 `run-as`），改完在应用里重启核心。

应用形态下没有 `./bot logs` 那个窗口，所以核心的 stdout 另存一份在 `filesDir/core/ayjx.log`（超过 1 MB 轮转一次，只留上一份）。控制台里那个日志页跟的是同一个流，但看它得先打得开界面。这一份留给打不开界面的时候。

```sh
su -c 'tail -50 /data/data/com.araea.zhiyan/files/core/ayjx.log'
```

**别让两份核心同时跑。** Termux 那份与应用自带的那份都连同一个实现端，同一条群消息会被回两次。想用应用看，就在首屏点「连接到已有实例」；想用应用自带的核心，先 `./bot stop` 把 Termux 那份停掉。两边的数据目录是分开的（Termux 在 `target/release/data`，应用在 `filesDir/core/data`），所以配置、数据库、记忆互不影响。它们是两个实例，不是同一份的两个窗口。

## 壳里有什么

三个类，各管一件事。

| 类 | 管什么 |
| --- | --- |
| `ConsoleActivity` | 一屏。等核心把「控制台已就绪 <地址>」打出来，再把那个地址装进 WebView；核心没起来时显示等待屏 |
| `CoreService` | 让核心活着。前台服务 + 唤醒锁 + 一条常驻通知（点开回到控制台，通知上有停止） |
| `Core` | 那个进程。起停、读 stdout、握手、以及「活得够久才重启」的策略 |

几条刻意如此的地方：

- **WebView 只停在这个应用的页面上。** 控制台里点出去的链接交给系统浏览器，否则一张网页就能把整块屏幕变成一个没有地址栏的浏览器。
- **地址里的口令不进日志。** 通知上只写主机与端口。
- **重启有条件。** 核心活过 10 秒才算「偶发退出」，隔 3 秒拉一次。秒退说明配置坏了（端口被占、TOML 写错），一直重试只会变成热循环。等待屏上会把核心最后那句话留着，不用去翻日志。
- **`access_token` 读不到就当作「别动」。** 设置页永远看不到已经存下的令牌（核心只回一个 `has_token` 布尔值），所以留空即保持原值，写一个空串才清掉。

## 网络与权限

| 权限 | 为什么 |
| --- | --- |
| `INTERNET` / `ACCESS_NETWORK_STATE` | 连实现端；控制台走回环 |
| `FOREGROUND_SERVICE` + `FOREGROUND_SERVICE_SPECIAL_USE` | 核心是常驻进程，系统不该按内存压力收掉它 |
| `POST_NOTIFICATIONS` | 常驻通知要看得见；被拒也不影响核心运行 |
| `WAKE_LOCK` | 熄屏时群消息不会因为你没看屏幕就不来 |

明文 HTTP 是放行的（`res/xml/network_security_config.xml`）：控制台是这台机器上的一个服务，没有证书可说。鉴权靠那道口令：核心首次启动生成 32 位十六进制，写在 `data/console/token`（0600），只有 WebView 拿得到，`/api/*` 一律要它。

## 图标

主色圆角方 + 一个几何化的「言」：最上面一点、三横、底下一只口。选这个字是因为它本身就是「说出来的话」，而这台机器人做的是在同一句话上读人与应人。

一处几何，五个产物，都由 `scripts/make-icon.py` 生成（改图标只改那一个脚本）：

| 产物 | 用在哪 |
| --- | --- |
| `res/console/icon.svg` | 网页图标与 PWA manifest |
| `app/res/drawable/ic_launcher_foreground.xml` | 自适应图标的前景层（透明底白笔画） |
| `app/res/drawable/ic_launcher_background.xml` | 自适应图标的背景层 |
| `app/res/drawable/ic_launcher_monochrome.xml` | 主题图标（Android 13+）的单色层 |
| `app/res/drawable/ic_notification.xml` | 通知栏那颗小图标 |

等待屏上另有一枚 `ic_mark.xml`。前景层是给系统裁切用的透明底白笔画，直接摆在浅色纸上什么也看不见。

```sh
python3 scripts/make-icon.py   # 改完坐标跑一次
```

## 看界面

```sh
bash scripts/review-console.sh            # 用本仓库正在跑的那一份
bash scripts/review-console.sh <带口令的地址>
```

它把八页拍成本地图片，落在 `${TMPDIR:-/tmp}/ayjx-console`。与 `scripts/review-cards.sh` 是一对：那一份管五张卡片图，这一份管应用界面。走的是 chromedriver 的 WebDriver 协议而不是 `chromium --screenshot`。日志页有一条长连接（SSE），页面永远不进入空闲，headless 的截图会一直等下去。

## 这台机器上的实测边界

2026-09-16 装机实测（Android 16 / ColorOS，`com.araea.zhiyan` 0.1.0）。两边都先量过再下结论，别把「跑得起来」当成「能一直跑」。

**装完就能用。** 首屏等到核心起来（约十秒），网页就是那张控制台，QQ 也连上了（`login=3373167460`）。深色主题、日志实时流、群消息都正常。

**两件该做的事，和它们解决不了的那一件。**

| 现象 | 判据 | 处置 |
| --- | --- | --- |
| 进程被冻住：TCP 连得上但一个字节都不回 | `/proc/<pid>/wchan` 是 `do_freezer_trap` | `su -c "dumpsys deviceidle whitelist +com.araea.zhiyan"` |
| 整台应用被清理器杀掉 | `su -c "dumpsys activity exit-info com.araea.zhiyan"` 里 `reason=13` + `o-stop` | 同上；ColorOS 自己的「允许后台运行」还要在系统设置里放行一次 |

装完先做这两件。但**它们挡不住冻结**，这一点实测得很干净，别抱希望：

- 加白名单之前：启动约六分钟后整个进程被 ColorOS 清掉（`reason=13 (OTHER KILLS BY SYSTEM)`、`description=stop … due to o-stop(2)`）；在那之前它已经先被冻过一轮。
- 加白名单之后：`am get-standby-bucket` 回 `5 (EXEMPTED)`，被杀确实不再发生了；**但只要它不在前台，仍然被冻**。判据是同一套——`wchan=do_freezer_trap`、控制台连得上不回、核心子进程一起冻住（同一个 cgroup）。
- 前台时一切正常：一直应答，日志实时流、群消息、深色主题全都在。冻结的触发条件是「它退到后台」，与唤醒锁、前台服务、电池白名单都无关。

**所以：应用形态适合「看着它」，不适合「跑着它」。** 24 小时常驻交给 Termux 那份——那里有 runit、有 Chromium（卡片图能出）、还有 root 侧的看守补上「Termux 被厂商清理器杀掉」这一层；应用侧没有对应的看守，被冻住就是被冻住，而一个冻住的核心等于一个不说话的机器人（QQ 的消息还在往实现端堆，它却不动）。应用最合适的用法是首屏选「连接到已有实例」，当一块更好看的屏幕。

**出图在应用里一定是退化的。** 应用沙箱里够不着任何浏览器，启动时 `cdp-html-shot` 会 panic 一次（被 `catch_unwind` 接住，只留一行 WARN），之后所有卡片走纯文本。这是既有的降级路径，不是故障，但要看卡片图就得用 Termux 那份。

**应用里的 HTTPS 曾是全线不可用的（已修）。** reqwest 的 rustls 在 Android 上走 `rustls-platform-verifier`，它要 JVM 侧的 `init_hosted` 注入证书；应用里没有那个注入，而 `http.rs` 从前只认 Termux 那几条「一份 bundle」的路径，在应用里一条都不存在——于是回落到默认校验器，每次 TLS 握手都在 tokio 工作线程里 panic（`Expect rustls-platform-verifier to be initialized`），表现为 `ai_news` 的实时推送每 60 秒抛错一次。现在多认一种摆法：Android 自己那种「一目录一证书」（`/apex/com.android.conscrypt/cacerts` 与 `/system/etc/security/cacerts`，各 149 张），拼成一份 bundle 再用。修完实测那条 60 秒轮询不再报错。

## 边界

- **只打 `arm64-v8a`。** 本机是 aarch64，交叉编译别的架构要另一套工具链。真要发别的架构，`app/build.sh` 里那一步换成对应的 target 即可，Rust 那侧不用动。
- **应用里没有 Chromium。** 出图降级为纯文本，理由见上面「壳的两种用法」。
- **签名是自签的。** `app/build/zhiyan.keystore`，口令写在构建脚本里，它保证的是「覆盖安装时签名一致」。换机器要重新生成一份，代价是必须卸载重装，所以别把它弄丢。
- **不碰系统的自动启动。** 壳没有 `BOOT_COMPLETED`：应用没被打开过就不该有东西在后台跑。要开机自启，用 Termux 那份部署（它本来就有 root 侧的看守）。
