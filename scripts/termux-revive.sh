#!/data/data/com.termux/files/usr/bin/bash
# Termux 监督树看守。以 root 运行（service.d 或 `su -c`）。
#
# 为什么需要：ColorOS 的清理器会把整个 Termux 应用杀掉，Termux 里的一切
# （登录会话、tmux、runsvdir、bot）随之消失。runit 只能管进程级生死，管不到
# 「宿主应用没了」这一层，所以这一层放在 Termux 外面用 root 跑。
# 本机实测（`dumpsys activity exit-info com.termux`）：截至 2026-09-20 共 11 条退出记录，
# 其中 10 条是 `reason=13 (OTHER KILLS BY SYSTEM) description=o-kill(...)`，
# 1 条 `reason=3 (LOW_MEMORY)`；最近一次 2026-09-20 02:05:38。都是它干的。
#
# 判据：runsvdir 在不在（$SVDIR 的那一个）。
#   runsvdir 在   -> 什么都不做。bot 的生死归 runsv 管，进程崩了它自己会拉；
#                    `sv down acumen` 是你的明确意图，看守不干预。
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
# 手动停机器人只要 `sv down acumen`（或 ./bot stop）：看守判据是 runsvdir 而不是
# bot，不会跟你抢。
#
# 除了复活 Termux，本看守还负责三件事：
#   · 应用还在、只是 runsvdir 没了 → 走 RUN_COMMAND（allow-external-apps）在应用
#     进程里执行 `acumen-guard start-services`。这种情况拉 activity 是没用的（不会
#     新建登录 shell，profile.d 不会重新拉起监督树）；命令自己会先查重。
#   · Doze 自愈：每 5 分钟核一次 `mLightEnabled/mDeepEnabled`，被系统/厂商重新打开
#     就再关掉（只在真的被打开时才动手，不重复配置）。
#   · 每轮写一份机器可读快照 STATUSFILE（/data/local/tmp，0644），Termux 侧的
#     acumen-guard 靠它读唤醒锁与监督树状态，不用每次都 su dumpsys。
#
# 停止 / 自检：
#   su -c "$0 --check"          只看一轮判据，不动 Termux
#   su -c "$0 hold" / resume    硬开关，跨重启有效
#   kill $(cat $PIDFILE)        只停本次开机——Termux 侧的 acumen-guard 看守会在
#                               ~2 分钟内用 su 把它重新拉起；要长期停就用 hold
set -u
export PATH=/data/data/com.termux/files/usr/bin:/system/bin:/system/xbin:${PATH:-}
# 相对路径一个都不用，但 sv/dumpsys 这些工具要求 cwd 可读（从 /tmp 之类
# 不可遍历的目录里跑会报 "unable to open current directory"）。root 总能进 /。
cd / 2>/dev/null || true

PKG=${TERMUX_REVIVE_PKG:-com.termux}
PREFIX=${TERMUX_REVIVE_PREFIX:-/data/data/com.termux/files/usr}
SVDIR=${TERMUX_REVIVE_SVDIR:-$PREFIX/var/service}
SERVICE=${TERMUX_REVIVE_SERVICE:-acumen}
INTERVAL=${TERMUX_REVIVE_INTERVAL:-60}
RECOVER_WAIT=${TERMUX_REVIVE_RECOVER_WAIT:-60}
# bot 二进制的绝对路径：Termux 侧部署在 <checkout>/target/release/acumen。
# 只用来在状态快照里报出「bot 在不在」，不参与复活判定（bot 的生死归 runsv）。
BOT_EXE=${TERMUX_REVIVE_BOT_EXE:-/data/data/com.termux/files/home/dev/araea/acumen/target/release/acumen}
# 连续拉不起来时的退避上限（秒）。runsvdir 一直起不来说明不是被清理器杀一次，
# 而是别的地方坏了，这时不该每 65 秒反复拉起一次。
MAX_WAIT=${TERMUX_REVIVE_MAX_WAIT:-1800}
# 视为「用户主动杀」的 ApplicationExitInfo.reason 集合。
USER_REASONS=${TERMUX_REVIVE_USER_REASONS:-"10 11"}
MAX_LOG_BYTES=${TERMUX_REVIVE_MAX_LOG_BYTES:-2000000}
# Doze 自愈的检查间隔（轮数，每轮 INTERVAL 秒）。5 轮 = 5 分钟。
DOZE_EVERY=${TERMUX_REVIVE_DOZE_EVERY:-5}

# 日志/PID/hold 一律放 /data/local/tmp：本看守总是以 root 跑，HOME 会随调用方式变化，
# 依赖 HOME 会让 hold 和 --check 认到不同文件。
LOG=${TERMUX_REVIVE_LOG:-/data/local/tmp/termux-revive.log}
PIDFILE=${TERMUX_REVIVE_PIDFILE:-${LOG%.log}.pid}
HOLDFILE=${TERMUX_REVIVE_HOLD:-${LOG%.log}.hold}
# 每次巡检写一份机器可读快照。Termux 里读不了 /data/adb（700 root），
# 但 /data/local/tmp 是 0711，普通应用按绝对路径能读 0644 的文件，
# 所以 Termux 侧看守（acumen-guard）就是靠这个文件知道唤醒锁有没有掉。
STATUSFILE=${TERMUX_REVIVE_STATUS:-/data/local/tmp/termux-revive.status}

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

# 有个坑：pgrep 找不到时返回 1，但 `pgrep ... | head -1` 的退出码是 head 的 0，
# 于是调用方 `if app=$(termux_app_pid)` 永远为真，日志里出现过「应用还在（pid ）」。
# 这里不用管道，找不到就返回 1，让调用方自己判空。
termux_app_pid() {
    local pid
    for pid in $(pgrep -f '^com\.termux$' 2>/dev/null); do
        [ -d "/proc/$pid" ] && { printf '%s' "$pid"; return 0; }
    done
    return 1
}

# bot 进程（按可执行文件校验，避免把命令行里恰好带这个路径的 shell 认进来）。
bot_pid() {
    local pid exe
    for pid in $(pgrep -f '/target/release/acumen' 2>/dev/null); do
        exe=$(readlink "/proc/$pid/exe" 2>/dev/null) || continue
        [ "${exe% (deleted)}" = "$BOT_EXE" ] && { printf '%s' "$pid"; return 0; }
    done
    return 1
}

# 唤醒锁在不在。termux-wake-lock 会以 'termux:service-wakelock' 名字挂上；
# 「Restored Wake Locks」历史里也有同名行，所以认带引号 + ACQ= 的那一行。
termux_wakelock_held() {
    dumpsys power 2>/dev/null | grep -q "termux:service-wakelock' ACQ="
}

service_state() {
    local f="$SVDIR/$SERVICE/supervise/stat"
    [ -r "$f" ] || { printf 'missing'; return; }
    awk '{print $1; exit}' "$f" 2>/dev/null || printf 'unknown'
}

# Doze：一旦又生效，经 FlClash tun 的流量会整段断掉（2026-09-08 实测，mState=IDLE
# 期间 6 分钟 proxy/tun 全失败）。开机有 service.d/99-no-doze.sh 兑一次，但系统更新、
# 省电开关、厂商服务都可能把它打开；看守每 5 分钟核一次，只在真的被打开时才重关。
doze_disabled() {
    dumpsys deviceidle 2>/dev/null | grep -qE 'mLightEnabled=false[[:space:]]+mDeepEnabled=false'
}

ensure_doze_disabled() {
    doze_disabled && return 0
    log "doze: 检测到 Doze 又被打开了，重新关闭（deviceidle disable）"
    {
        echo "$(date '+%F %T') watchdog: Doze 重新打开，自动关闭"
        dumpsys deviceidle disable 2>&1
        dumpsys deviceidle 2>/dev/null | grep -E 'mLightEnabled|mState=' 2>&1
    } >> "${TERMUX_REVIVE_NO_DOZE_LOG:-/data/local/tmp/no-doze.log}" 2>&1
    if doze_disabled; then
        log "doze: 已重新关掉"
    else
        log "doze: 关闭失败，请手动查 dumpsys deviceidle"
    fi
}

write_status() {
    local rv app bot wl state tmp
    rv=$(runsvdir_pid 2>/dev/null) || rv=""
    app=$(termux_app_pid 2>/dev/null) || app=""
    bot=$(bot_pid 2>/dev/null) || bot=""
    if termux_wakelock_held; then wl=yes; else wl=no; fi
    state=$(service_state)
    tmp="$STATUSFILE.tmp"
    {
        printf 'ts=%s\n' "$(date '+%F %T')"
        printf 'runsvdir_pid=%s\n' "$rv"
        printf 'app_pid=%s\n' "$app"
        printf 'bot_pid=%s\n' "$bot"
        printf 'wakelock=%s\n' "$wl"
        printf 'service_state=%s\n' "$state"
        printf 'hold=%s\n' "$( [ -f "$HOLDFILE" ] && echo yes || echo no )"
        if doze_disabled; then printf 'doze=disabled\n'; else printf 'doze=ENABLED\n'; fi
    } > "$tmp" 2>/dev/null && mv "$tmp" "$STATUSFILE" 2>/dev/null
    chmod 0644 "$STATUSFILE" 2>/dev/null
}

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
# 5 秒后还没起监督树就再点名 activity 一次兜底。返回 0 表示监督树回来了。
revive_termux() {
    log "revive: $1"
    monkey -p "$PKG" -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1
    sleep 5
    if ! runsvdir_pid >/dev/null; then
        am start-activity -n "$PKG/com.termux.app.TermuxActivity" >/dev/null 2>&1
    fi
    sleep "$RECOVER_WAIT"
    runsvdir_pid >/dev/null
}

# 应用进程还在、只是监督树没了。这种情况拉 activity 没用——activity 只是切回前台，
# 不会新建登录 shell，profile.d 也就不会重新拉起 runsvdir。走 Termux 自己的
# RUN_COMMAND（需要 ~/.termux/termux.properties 里 allow-external-apps=true），
# 在应用进程里执行 `acumen-guard start-services`；那条命令会先查 runsvdir 在不在，
# 所以不会起出第二个 runsvdir（同一个 SVDIR 两个 runsvdir 会把服务起两遍）。
restart_runsvdir_in_app() {
    local guard=$PREFIX/bin/acumen-guard
    if [ ! -x "$guard" ]; then
        log "revive: 应用内重起监督树需要 $guard，但它不存在/不可执行，跳过"
        return 1
    fi
    log "revive: 应用还在但监督树不在，尝试在应用内重起 runsvdir"
    # 注意：RunCommandService 在 manifest 里是 <service>，不是 <receiver>，
    # 所以必须用 am startservice；用 am broadcast 会「成功」但什么都不发生。
    am startservice --user 0 \
        -n "$PKG/com.termux.app.RunCommandService" \
        -a com.termux.RUN_COMMAND \
        --es com.termux.RUN_COMMAND_PATH "$guard" \
        --esa com.termux.RUN_COMMAND_ARGUMENTS 'start-services' \
        --ez com.termux.RUN_COMMAND_BACKGROUND true \
        >/dev/null 2>&1
    sleep 10
    runsvdir_pid >/dev/null
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
    write_status
    [ -r "$STATUSFILE" ] && echo "快照: $(tr '\n' ' ' < "$STATUSFILE")"
    if termux_wakelock_held; then
        echo "wakelock: 在（termux:service-wakelock）"
    else
        echo "wakelock: 不在——CPU 可被挂起、应用会退回 cached，必须补 termux-wake-lock"
    fi
    if bp=$(bot_pid); then
        echo "bot: pid $bp"
    else
        echo "bot: 不在（bot 的生死归 runsv：sv status $SERVICE）"
    fi
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
revive_fails=0
doze_tick=0

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

    # 每轮先写一份快照，供 Termux 侧看守（acumen-guard status）读取。
    write_status

    # Doze 自愈：只在真的被重新打开时才动手（不重复配置）。
    doze_tick=$((doze_tick + 1))
    if [ $((doze_tick % DOZE_EVERY)) -eq 0 ]; then
        ensure_doze_disabled
    fi

    # 正常态：监督树在。bot 的生死归 runsv，这里不插手。
    if rv=$(runsvdir_pid); then
        if [ -n "$stop_reason" ]; then
            log "cleared: 监督树回来了（此前判为 $stop_reason），恢复正常巡检"
            stop_reason=""
        fi
        revive_fails=0
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
        if restart_runsvdir_in_app; then
            log "revive: 监督树已在应用内恢复（Termux 应用 pid $app）"
        else
            revive_termux "应用内重起监督树没成，退回拉 activity（应用 pid $app）"
        fi
    else
        revive_termux "Termux 应用不在了"
    fi

    # 拉不起来就退避，别每 65 秒反复拉：连续失败次数翻倍，封顶 MAX_WAIT。
    if runsvdir_pid >/dev/null; then
        revive_fails=0
        continue
    fi
    revive_fails=$((revive_fails + 1))
    shift_n=$revive_fails
    [ "$shift_n" -gt 6 ] && shift_n=6
    wait=$(( RECOVER_WAIT * (1 << shift_n) ))
    [ "$wait" -gt "$MAX_WAIT" ] && wait=$MAX_WAIT
    log "revive 未成功（连续 $revive_fails 次），${wait}s 后再试；Termux 起不来时要手动查"
    sleep "$wait"
done
