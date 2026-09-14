#!/system/bin/sh
# KernelSU / Magisk service.d：开机把 Termux 监督树看守拉起来（看守以 root 运行）。
#
# 装法（两个文件，权限都给 755）：
#   /data/adb/service.d/97-termux-revive.sh    <- 本文件
#   /data/adb/termux-revive/termux-revive.sh   <- scripts/termux-revive.sh
#
# 看守日志：/data/local/tmp/termux-revive.log
# 手动起一次：su -c "/data/adb/termux-revive/termux-revive.sh &"
# 停：       kill "$(cat /data/local/tmp/termux-revive.pid)"
# 暂停：     su -c "/data/adb/termux-revive/termux-revive.sh hold"    （resume 恢复）
# 自检：     su -c "/data/adb/termux-revive/termux-revive.sh --check"
BIN=/data/data/com.termux/files/usr/bin/bash
WATCH=/data/adb/termux-revive/termux-revive.sh
PIDFILE=/data/local/tmp/termux-revive.pid

[ -x "$BIN" ] || exit 0
[ -f "$WATCH" ] || exit 0

# service.d 在 late_start 就跑，比 boot_completed 早；这时候拉 activity 拉不起来。
until [ "$(getprop sys.boot_completed)" = "1" ]; do
    sleep 5
done

if [ -f "$PIDFILE" ]; then
    pid=$(cat "$PIDFILE" 2>/dev/null)
    [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null && exit 0
fi
nohup "$BIN" "$WATCH" >/dev/null 2>&1 &
