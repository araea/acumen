#!/data/data/com.termux/files/usr/bin/bash
# ============================================================================
# 知言 · 打包成 APK
# ----------------------------------------------------------------------------
# 没有 Gradle。这台机器上只有 aapt / d8 / zipalign / apksigner 这几件原生工具，
# 而这个应用只有三个 Java 类、一份清单、一个自带的核心，用不上构建系统——
# 用 Gradle 的代价是几十兆的缓存与一套要联网的依赖解析，换来的是这里一行都用不到的
# 增量编译与变体管理。（同仓库的 satori-qq 也是这么打的。）
#
# 整个流程六步：
#   1. 交叉编译核心（Rust → aarch64-linux-android）
#   2. 剥符号，放进 lib/arm64-v8a/ 当 .so —— 见下面那段「为什么是 .so」
#   3. aapt 编资源，顺带吐出 R.java 与未签名包
#   4. javac 编译 Java（含第 3 步生成的 R.java）
#   5. d8 转 dex
#   6. 把 dex 与 .so 塞进那个未签名包
#   7. zipalign + apksigner 签名
#
# 用法：
#   bash app/build.sh                 # 全流程，产物 app/build/Zhiyan.apk
#   ZHIYAN_SKIP_CORE=1 bash app/build.sh   # 只重打壳，复用上一次的核心
# ============================================================================
set -e

APP=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$APP/.." && pwd)

ANDROID_JAR=${ANDROID_JAR:-$HOME/android/platform/android-35/android.jar}
BUILD_TOOLS=${ANDROID_BUILD_TOOLS:-$HOME/android/android-sdk-tools/build-tools}
AAPT=$BUILD_TOOLS/aapt
ZIPALIGN=$BUILD_TOOLS/zipalign
FRAMEWORK=/system/framework/framework-res.apk

# r8 里有 d8。它是可重新下载的构建工具（约 18 MB），不进仓库：
#   curl -fsSL -o app/libs/r8.jar https://maven.google.com/com/android/tools/r8/8.9.35/r8-8.9.35.jar
R8=${R8_JAR:-$APP/libs/r8.jar}
if [ ! -f "$R8" ] && [ -f "$HOME/dev/araea/satori-qq/libs/r8.jar" ]; then
  R8=$HOME/dev/araea/satori-qq/libs/r8.jar
fi

KS=$APP/build/zhiyan.keystore
KS_PASS=zhiyan-sideload
OUT=$APP/build
TARGET=aarch64-linux-android
CORE=$ROOT/target/$TARGET/release/ayjx

for tool in "$AAPT" "$ZIPALIGN" "$ANDROID_JAR" "$FRAMEWORK"; do
  [ -e "$tool" ] || { echo "缺 $tool；检查 ANDROID_JAR / ANDROID_BUILD_TOOLS" >&2; exit 1; }
done
[ -f "$R8" ] || { echo "缺 r8.jar；见本脚本上方那行下载命令" >&2; exit 1; }

mkdir -p "$OUT/classes" "$OUT/dex" "$OUT/lib/arm64-v8a" "$APP/assets"

# ---- 1 & 2. 核心 ----
# 为什么是 .so：Android 10 起，targetSdk 29 以上的应用不能对自家可写目录里的
# 文件执行 execve（W^X），所以可执行文件必须随 APK 走、由系统解包到 nativeLibraryDir。
# 那边的约定是「文件名以 .so 结尾」，内容是什么系统不管。
if [ "${ZHIYAN_SKIP_CORE:-0}" = "1" ] && [ -f "$OUT/lib/arm64-v8a/libayjx_core.so" ]; then
  echo "== 1/2. 复用已有的核心 =="
else
  echo "== 1. 交叉编译核心（$TARGET） =="
  ( cd "$ROOT" && cargo build --release --locked --target "$TARGET" )
  echo "== 2. 剥符号 =="
  STRIP=$(command -v llvm-strip || command -v strip)
  "$STRIP" -o "$OUT/lib/arm64-v8a/libayjx_core.so" "$CORE"
fi
ls -lh "$OUT/lib/arm64-v8a/libayjx_core.so" | awk '{print "   核心 " $5}'

echo "== 3. aapt 生成 R.java 与未签名包 =="
# 首启那份 config.toml 直接取仓库里那一份示例，不另存一份，免得两边越走越远。
cp "$ROOT/config.example.toml" "$APP/assets/config.toml"
APK_UNSIGNED=$OUT/zhiyan.unsigned.apk
rm -f "$APK_UNSIGNED"
rm -rf "$OUT/gen" && mkdir -p "$OUT/gen"
# 资源 id 得先有，javac 才编得过 `R.string.…`。所以 aapt 排在最前面：
# 它一边把资源编进 APK，一边把 R.java 吐到 $OUT/gen。类与 .so 之后再塞进去。
"$AAPT" package -f -M "$APP/AndroidManifest.xml" -I "$FRAMEWORK" \
  -A "$APP/assets" -S "$APP/res" -J "$OUT/gen" -F "$APK_UNSIGNED"

echo "== 4. javac =="
rm -rf "$OUT/classes" && mkdir -p "$OUT/classes"
find "$APP/src" "$OUT/gen" -name '*.java' > "$OUT/sources.txt"
javac -classpath "$ANDROID_JAR" -source 8 -target 8 -encoding UTF-8 \
  -nowarn -d "$OUT/classes" @"$OUT/sources.txt"
echo "   编译了 $(find "$OUT/classes" -name '*.class' | wc -l) 个类"

echo "== 5. d8 -> dex =="
rm -rf "$OUT/dex" && mkdir -p "$OUT/dex"
find "$OUT/classes" -name '*.class' > "$OUT/classlist.txt"
java -cp "$R8" com.android.tools.r8.D8 --release --min-api 26 \
  --lib "$ANDROID_JAR" --output "$OUT/dex" @"$OUT/classlist.txt"
echo "   dex: $(wc -c < "$OUT/dex/classes.dex") 字节"

echo "== 6. 把 dex 与核心塞进包里 =="
( cd "$OUT/dex" && "$AAPT" add "$APK_UNSIGNED" classes.dex >/dev/null )
( cd "$OUT" && "$AAPT" add "$APK_UNSIGNED" lib/arm64-v8a/libayjx_core.so >/dev/null )
echo "   未签名包 $(du -h "$APK_UNSIGNED" | cut -f1)"

echo "== 7. 对齐与签名 =="
if [ ! -f "$KS" ]; then
  # sideload 用的自签密钥：与 satori-qq 同一套待遇，口令写在脚本里是因为
  # 它保护的不是身份而是「覆盖安装时签名一致」。换机器要重新生成一份，
  # 代价是必须卸载重装——所以别把它弄丢。
  keytool -genkeypair -keystore "$KS" -alias zhiyan \
    -storepass "$KS_PASS" -keypass "$KS_PASS" \
    -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=Zhiyan" >/dev/null 2>&1
  echo "   生成了签名密钥"
fi
APK=$OUT/Zhiyan.apk
rm -f "$APK" "$OUT/zhiyan.aligned.apk"
"$ZIPALIGN" -f -p 4 "$APK_UNSIGNED" "$OUT/zhiyan.aligned.apk"
apksigner sign --ks "$KS" --ks-pass "pass:$KS_PASS" --key-pass "pass:$KS_PASS" \
  --out "$APK" "$OUT/zhiyan.aligned.apk"

echo "== 完成 =="
ls -lh "$APK" | awk '{print "   " $9 "  " $5}'
echo "   装机：su -c \"cp $APK /data/local/tmp/ && pm install -r /data/local/tmp/Zhiyan.apk\""
