#!/data/data/com.termux/files/usr/bin/bash
# Termux 监督树看守。以 root 运行（service.d 或 `su -c`）。
#
# 为什么需要：ColorOS 的清理器会把整个 Termux 应用杀掉，Termux 里的一切
# （登录会话、tmux、runsvdir、bot）随之消失。runit 只能管进程级生死，管不到
# 「宿主应用没了」这一层，所以这一层放在 Termux 外面用 root 跑。
# 本机实测（`dumpsys activity exit-info com.termux`）：一周内 10 次
# `reason=13 (OTHER KILLS BY SYSTEM) description=o-kill(...)`，都是它干的。
#
# 判据：runsvdir 在不在（$SVDIR 的那一个）。
#   runsvdir 在   -> 什么都不做。bot 的生死归 runsv 管，进程崩了它自己会拉；
#                    `sv down ayjx` 是你的明确意图，看守不干预。
#   runsvdir 不在 -> 先分辨是「你手动停的」还是「被动被杀」，再决定动不动手。
#
# 手动 / 被动的分辨（2026-09-14 定，本机实测过映射）：
#   ① 包处于 stopped 状态（`dumpsys package` 的 `stopped=true`）——`am force-stop`、
#      设置里的「强行停止」会置位，你下次手动打开 Termux 会自动清掉。不复活。
#   ② 本次开机以来最新一次退出的 `reason` 属于「用户主动」集合
#      （默认 `10 11`；实测 `am force-stop` 报 `reason=10 (USER REQUESTED)
#      subreason=21 (FORCE STOP)`，从最近任务划掉在 AOSP 里也是 10）。不复活。
#   ③ 其余一律复活（ColorOS 的 o-kill 报 13，LMK 报 3，崩溃 4/5，ANR 6，都算被动）。
#   上一次开机留下的旧记录不算数（重启后照常拉起）。
#   判成手动之后会一直保持停止，直到监督树回来（你打开一次 Termux）才解除。
#
# 想「一直别起来」用硬开关，不依赖上面的判别：
#   ./termux-revive.sh hold    写 $HOLDFILE，跨重启有效，resume 才恢复
#   ./termux-revive.sh resume
# 手上刚做过 force-stop 想恢复：打开一次 Termux，或
#   su -c 'cmd package unstop com.termux'
#
# 手动停机器人只要 `sv down ayjx`（或 ./bot stop）：看守判据是 runsvdir 而不是
# bot，不会跟你抢。
#
# 停止本看守：kill $(cat $PIDFILE)；本次开机不再有看守，重启后 service.d 会再拉起。
# 自检：./termux-revive.sh --check   只看一轮判据，不动 Termux。
set -u
export PATH=/data/data/com.termux/files/usr/bin:/system/bin:/system/xbin:${PATH:-}

PKG=${TERMUX_REVIVE_PKG:-com.termux}
PREFIX=${TERMUX_REVIVE_PREFIX:-/data/data/com.termux/files/usr}
SVDIR=${TERMUX_REVIVE_SVDIR:-$PREFIX/var/service}
SERVICE=${TERMUX_REVIVE_SERVICE:-ayjx}
INTERVAL=${TERMUX_REVIVE_INTERVAL:-60}
RECOVER_WAIT=${TERMUX_REVIVE_RECOVER_WAIT:-60}
# 视为「用户主动杀」的 ApplicationExitInfo.reason 集合。
USER_REASONS=${TERMUX_REVIVE_USER_REASONS:-"10 11"}
MAX_LOG_BYTES=${TERMUX_REVIVE_MAX_LOG_BYTES:-2000000}

# 日志/PID/hold 一律放 /data/local/tmp：本看守总是以 root 跑，HOME 会随调用方式变化，
# 依赖 HOME 会让 hold 和 --check 认到不同文件。
LOG=${TERMUX_REVIVE_LOG:-/data/local/tmp/termux-revive.log}
PIDFILE=${TERMUX_REVIVE_PIDFILE:-${LOG%.log}.pid}
HOLDFILE=${TERMUX_REVIVE_HOLD:-${LOG%.log}.hold}

log() {
    printf '%s %s\n' "$(date +%FT%T)" "$*" >> "$LOG" 2>/dev/null
    chmod 0644 "$LOG" 2>/dev/null
    size=$(stat -c %s "$LOG" 2>/dev/null || echo 0)
    if [ "$size" -gt "$MAX_LOG_BYTES" ]; then
        tail -n 500 "$LOG" > "$LOG.tmp" 2>/dev/null && mv "$LOG.tmp" "$LOG"
    fi
}

# Termux 的 runsvdir 在不在。只看 cmdline 里带 $SVDIR 的那个，避免认错人。
runsvdir_pid() {
    local pid
    for pid in $(pgrep -f 'runsvdir' 2>/dev/null); do
        if tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null | grep -qF -- "$SVDIR"; then
            printf '%s' "$pid"
            return 0
        fi
    done
    return 1
}

termux_app_pid() { pgrep -f '^com\.termux$' 2>/dev/null | head -1; }

# 系统认为「用户已经把 App 停掉」。force-stop / 强行停止置位，用户手动打开即清除。
package_stopped() {
    dumpsys package "$PKG" 2>/dev/null | grep -q '[[:space:]]stopped=true'
}

# 本次开机时刻，格式与 dumpsys 的 timestamp 一致，可以直接按字符串比。
boot_ts() {
    local up
    up=$(cut -d. -f1 /proc/uptime 2>/dev/null) || up=0
    date -d "@$(( $(date +%s) - up ))" '+%Y-%m-%d %H:%M:%S' 2>/dev/null
}

newest_exit() {
    dumpsys activity exit-info "$PKG" 2>/dev/null \
        | awk '/ApplicationExitInfo #0:/{f=1} f{print} /ApplicationExitInfo #1:/{exit}'
}

exit_ts()      { printf '%s\n' "$1" | sed -n 's/^ *timestamp=\([0-9-]* [0-9:]*\)\..*/\1/p' | head -1; }
exit_reason()  { printf '%s\n' "$1" | sed -n 's/.*[[:space:]]reason=\([0-9][0-9]*\).*/\1/p' | head -1; }
exit_label()   { printf '%s\n' "$1" | sed -n 's/.*[[:space:]]reason=[0-9][0-9]* (\([^)]*\)).*/\1/p' | head -1; }
exit_sub()     { printf '%s\n' "$1" | sed -n 's/.*subreason=\([0-9][0-9]*\) (\([^)]*\)).*/\1 \2/p' | head -1; }
exit_desc()    { printf '%s\n' "$1" | sed -n 's/.*[[:space:]]description=\(.*\)[[:space:]]state=.*/\1/p' | head -1; }

# 本次开机以来最新一次退出是否属于「用户主动」。是则输出一句可读原因。
user_requested_exit() {
    local rec ts reason boot r
    rec=$(newest_exit)
    [ -n "$rec" ] || return 1
    ts=$(exit_ts "$rec")
    reason=$(exit_reason "$rec")
    [ -n "$ts" ] && [ -n "$reason" ] || return 1
    # 上次开机留下的旧记录不算数：重启后照常拉起。
    boot=$(boot_ts)
    if [ -n "$boot" ] && [[ "$ts" < "$boot" ]]; then
        return 1
    fi
    for r in $USER_REASONS; do
        if [ "$reason" = "$r" ]; then
            printf 'reason=%s (%s) subreason=%s desc=%s @%s' \
                "$reason" "$(exit_label "$rec")" "$(exit_sub "$rec")" "$(exit_desc "$rec")" "$ts"
            return 0
        fi
    done
    return 1
}

# 拉起 Termux。monkey 走 launcher activity（与 qq-revive 拉 QQ 同一套）；
# 5 秒后还没起监督树就再点名 activity 一次兜底。
revive_termux() {
    log "revive: $1"
    monkey -p "$PKG" -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1
    sleep 5
    if ! runsvdir_pid >/dev/null; then
        am start-activity -n "$PKG/com.termux.app.TermuxActivity" >/dev/null 2>&1
    fi
    sleep "$RECOVER_WAIT"
}

# 只看一轮判据、不动 Termux，用来确认探针本身工作正常。
if [ "${1:-}" = "--check" ]; then
    rv=$(runsvdir_pid) && echo "runsvdir: pid $rv ($SVDIR)" || echo "runsvdir: <不在>"
    app=$(termux_app_pid) && echo "termux app: pid $app" || echo "termux app: <不在>"
    if [ -n "${app:-}" ]; then
        # freezer=/ 表示没被冻；cpuset 是 foreground/top-app 表示按前台应用对待
        echo "termux cgroup: $(awk -F: '/:(freezer|cpuset):/ {printf "%s=%s ", $2, $3}' "/proc/$app/cgroup" 2>/dev/null)"
    fi
    package_stopped && echo "package stopped: true（系统认为你手动停过，不会复活）" \
                     || echo "package stopped: false"
    rec=$(newest_exit)
    if [ -n "$rec" ]; then
        echo "newest exit: reason=$(exit_reason "$rec") ($(exit_label "$rec")) subreason=$(exit_sub "$rec") desc=$(exit_desc "$rec")"
        echo "             @$(exit_ts "$rec")  本次开机于 $(boot_ts)"
        if detail=$(user_requested_exit); then
            echo "判据: 算「用户主动」，不复活 -> $detail"
        else
            echo "判据: 算「被动」，会复活"
        fi
    else
        echo "newest exit: <无记录>"
    fi
    [ -f "$HOLDFILE" ] && echo "hold: 已暂停（$(cat "$HOLDFILE" 2>/dev/null)）" || echo "hold: 未暂停"
    if [ -x "$PREFIX/bin/sv" ] && [ -d "$SVDIR/$SERVICE" ]; then
        echo "service $SERVICE: $(SVDIR="$SVDIR" "$PREFIX/bin/sv" status "$SERVICE" 2>&1)"
    else
        echo "service $SERVICE: <未安装>"
    fi
    [ -f "$PIDFILE" ] && echo "watchdog: pid $(cat "$PIDFILE" 2>/dev/null)" || echo "watchdog: 未在跑"
    exit 0
fi

case "${1:-run}" in
    hold)
        date +%FT%T > "$HOLDFILE" 2>/dev/null
        log "hold: 看守暂停（$HOLDFILE）；resume 之前不会拉起 Termux"
        printf '看守已暂停。恢复用 %s resume\n' "$0"
        exit 0
        ;;
    resume)
        rm -f "$HOLDFILE"
        log "resume: 看守恢复"
        printf '看守已恢复。\n'
        exit 0
        ;;
    run) ;;
    *)
        printf '用法：%s [run|hold|resume|--check]\n' "$0" >&2
        exit 2
        ;;
esac

echo $$ > "$PIDFILE" 2>/dev/null
log "watchdog start pid=$$ pkg=$PKG svdir=$SVDIR interval=${INTERVAL}s user_reasons='$USER_REASONS'"

held_logged=0
stop_reason=""

while true; do
    # 硬开关：暂停。跨重启有效，只有 resume 才解除。
    if [ -f "$HOLDFILE" ]; then
        if [ "$held_logged" = 0 ]; then
            log "held: 看守暂停中，跳过巡检"
            held_logged=1
        fi
        sleep "$INTERVAL"
        continue
    fi
    held_logged=0

    # 正常态：监督树在。bot 的生死归 runsv，这里不插手。
    if rv=$(runsvdir_pid); then
        if [ -n "$stop_reason" ]; then
            log "cleared: 监督树回来了（此前判为 $stop_reason），恢复正常巡检"
            stop_reason=""
        fi
        sleep "$INTERVAL"
        continue
    fi

    # 已经判过是手动停的：保持停止，等你把 Termux 打开。
    if [ -n "$stop_reason" ]; then
        sleep "$INTERVAL"
        continue
    fi

    # 第一次发现监督树不在：分辨手动还是被动。
    if package_stopped; then
        stop_reason="force-stop（package stopped=true）"
    elif detail=$(user_requested_exit); then
        stop_reason="用户主动退出（$detail）"
    fi
    if [ -n "$stop_reason" ]; then
        log "skip: 判为手动停止（$stop_reason），不复活。恢复：打开一次 Termux，或 su -c 'cmd package unstop $PKG'"
        sleep "$INTERVAL"
        continue
    fi

    if app=$(termux_app_pid); then
        revive_termux "监督树不在，但 Termux 应用还在（pid $app）"
    else
        revive_termux "Termux 应用不在了"
    fi
done
