#!/system/bin/sh
# 知微的 root 守护：开机把 Termux 与机器人拉起来，Termux 被系统清掉后再拉回来。
#
# 由 `acumen setup` 装到 /data/adb/service.d/acumen.sh，KernelSU / Magisk 每次开机执行一次。
# 内容随知微的可执行文件一起更新，不要手改。
#
#   sh acumen.sh           开机入口：后台起守护（已在跑就什么也不做）
#   sh acumen.sh restart   换上新版本后重起守护
#   sh acumen.sh stop      停掉守护（只管本次开机）
#   sh acumen.sh status    看守护眼里的状态
#
# 分工：机器人进程崩了由 Termux 里的 runit 拉起；这里只管 runit 自己管不到的那一层——
# Termux 整个应用没了（厂商清理、低内存、开机）。`acumen stop` 会在服务目录里留下 down
# 标记，守护看到它就什么也不做；在系统设置里「强行停止」Termux 也算你主动停的，不去拉。
PKG=com.termux
P=/data/data/com.termux/files/usr
SV=$P/var/service/acumen
BASE=/data/local/tmp/acumen-keeper
LOG=$BASE.log
PID=$BASE.pid
STATUS=$BASE.status
INTERVAL=60

log() {
    echo "$(date '+%F %T') $*" >> "$LOG"
    if [ "$(stat -c %s "$LOG" 2>/dev/null || echo 0)" -gt 262144 ]; then
        tail -n 300 "$LOG" > "$LOG.tmp" && mv -f "$LOG.tmp" "$LOG"
    fi
    chmod 0644 "$LOG" 2>/dev/null
}

running() {
    p=$(cat "$PID" 2>/dev/null)
    [ -n "$p" ] && [ "$p" != "$$" ] && grep -q 'acumen' "/proc/$p/cmdline" 2>/dev/null
}

# 解锁之前 Termux 的目录是加密的，连路径都解析不了。
unlocked() { [ -x "$P/bin/sh" ]; }

wants_bot() { [ -d "$SV" ] && [ ! -e "$SV/down" ]; }

runsvdir_alive() {
    p=$(cat "$P/var/run/service-daemon.pid" 2>/dev/null)
    [ -n "$p" ] && grep -q runsvdir "/proc/$p/cmdline" 2>/dev/null
}

force_stopped() { dumpsys package "$PKG" 2>/dev/null | grep -q ' stopped=true'; }

wakelock_held() { dumpsys power 2>/dev/null | grep -q "termux:service-wakelock' ACQ="; }

# 后台运行的豁免：省电白名单、不限制后台、待机分组保持活跃。系统更新或厂商的省电
# 策略会把它们改回去，所以每轮都核一次，只有被改掉了才重设。
exempt() {
    dumpsys deviceidle whitelist 2>/dev/null | grep -q ",$PKG," \
        || dumpsys deviceidle whitelist "+$PKG" >/dev/null 2>&1
    cmd appops set "$PKG" RUN_ANY_IN_BACKGROUND allow >/dev/null 2>&1
    cmd appops set "$PKG" RUN_IN_BACKGROUND allow >/dev/null 2>&1
    am set-standby-bucket "$PKG" active >/dev/null 2>&1
}

# 在 Termux 里执行 `acumen wake`：起 runit 监督树、拿唤醒锁。Termux 不在时这一下也会
# 把它的服务拉起来（需要 allow-external-apps，setup 时已打开）。
wake_in_termux() {
    am startservice --user 0 -n "$PKG/com.termux.app.RunCommandService" \
        -a com.termux.RUN_COMMAND \
        --es com.termux.RUN_COMMAND_PATH "$P/bin/acumen" \
        --esa com.termux.RUN_COMMAND_ARGUMENTS wake \
        --ez com.termux.RUN_COMMAND_BACKGROUND true >/dev/null 2>&1
}

revive() {
    log "拉起：$1"
    wake_in_termux
    sleep 15
    runsvdir_alive && return 0
    # 后台启动被系统拦下时退回打开 Termux 界面：新会话的登录脚本也会起监督树。
    am start --user 0 -n "$PKG/com.termux.app.TermuxActivity" >/dev/null 2>&1
    sleep 10
    wake_in_termux
    sleep 15
    runsvdir_alive
}

write_status() {
    {
        echo "ts=$(date +%s)"
        runsvdir_alive && echo "runit=1" || echo "runit=0"
        wakelock_held && echo "wakelock=1" || echo "wakelock=0"
        dumpsys deviceidle whitelist 2>/dev/null | grep -q ",$PKG," && echo "exempt=1" || echo "exempt=0"
    } > "$STATUS.tmp" && mv -f "$STATUS.tmp" "$STATUS"
    chmod 0644 "$STATUS" 2>/dev/null
}

loop() {
    echo $$ > "$PID"
    chmod 0644 "$PID"
    until [ "$(getprop sys.boot_completed)" = 1 ] && unlocked; do sleep 10; done
    log "守护启动（pid $$）"
    fails=0
    tick=0
    noted=0
    while true; do
        tick=$((tick + 1))
        [ $((tick % 10)) = 1 ] && exempt
        if wants_bot && ! runsvdir_alive; then
            if force_stopped; then
                [ "$noted" = 1 ] || log "Termux 被强行停止，当作你主动停的，不去拉"
                noted=1
            elif revive "监督树不在"; then
                log "已恢复"
                fails=0
                noted=0
            else
                fails=$((fails + 1))
                pause=$((INTERVAL * fails * fails))
                [ "$pause" -gt 1800 ] && pause=1800
                log "没拉起来（第 $fails 次），$pause 秒后再试"
                write_status
                sleep "$pause"
                continue
            fi
        elif wants_bot; then
            fails=0
            noted=0
            wakelock_held || { log "唤醒锁掉了，补上"; wake_in_termux; }
        fi
        write_status
        sleep "$INTERVAL"
    done
}

# 守护的 shell 正等着 sleep 时会把 TERM 攒到 sleep 结束才处理，所以直接连同整个进程组
# （setsid 起的，组号就是它自己）一起结束。
halt() {
    if running; then
        p=$(cat "$PID")
        kill -9 -- "-$p" 2>/dev/null || kill -9 "$p" 2>/dev/null
    fi
    rm -f "$PID"
}

case "${1:-start}" in
    start)
        running && exit 0
        if command -v setsid >/dev/null 2>&1; then
            setsid sh "$0" loop < /dev/null > /dev/null 2>&1 &
        else
            nohup sh "$0" loop < /dev/null > /dev/null 2>&1 &
        fi
        ;;
    restart)
        halt
        sh "$0" start
        ;;
    stop) halt ;;
    status)
        running && echo "守护：在跑（pid $(cat "$PID")）" || echo "守护：没在跑"
        cat "$STATUS" 2>/dev/null
        tail -n 5 "$LOG" 2>/dev/null
        ;;
    loop) loop ;;
esac
