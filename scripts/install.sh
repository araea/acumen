#!/data/data/com.termux/files/usr/bin/bash
# 知微（acumen）安装脚本：在 Termux 里运行
#
#   curl -fsSL https://github.com/araea/acumen/releases/latest/download/install.sh | bash
#
# 连不上 GitHub 时在前面加个加速前缀：
#
#   curl -fsSL https://gh-proxy.com/https://github.com/araea/acumen/releases/latest/download/install.sh | bash
#
# 下载最新的发布版到 ~/acumen，装好依赖、中文字体、浏览器与保活，然后启动并打开控制台。
# 重复运行是安全的：已经装好的部分会跳过，配置与数据不动。
#
# 整个脚本包在函数里：经管道交给 bash 时，要先读完全文再开始执行，中途的命令才不会
# 把后半截脚本当成自己的输入吃掉。
main() {
    set -uo pipefail
    local prefix=${PREFIX:-/data/data/com.termux/files/usr}
    local app=${ACUMEN_HOME:-$HOME/acumen}
    local manifest=https://github.com/araea/acumen/releases/latest/download/update.json
    # 直连 GitHub 与几家加速站（与可执行文件里内置的同一份），挨个试。
    local mirrors=("" https://gh-proxy.com/ https://cdn.gh-proxy.com/ https://gh.llkk.cc/ https://gh.idayer.com/ https://ghfast.top/)

    if [[ ! -d /data/data/com.termux || $prefix != /data/data/com.termux/* ]]; then
        echo "知微只支持安卓上的 Termux。" >&2
        return 1
    fi
    if [[ $(uname -m) != aarch64 ]]; then
        echo "知微只提供 arm64（aarch64）版本，这台设备是 $(uname -m)。" >&2
        return 1
    fi
    command -v curl >/dev/null || yes | pkg install -y curl >/dev/null 2>&1

    echo "正在获取最新版本 …"
    local info='' version url sha mirror
    for mirror in "${mirrors[@]}"; do
        info=$(curl -fsSL --connect-timeout 8 --max-time 20 "$mirror$manifest") && break
        info=''
    done
    [[ -n $info ]] || { echo "GitHub 与加速站都连不上，检查网络后再试。" >&2; return 1; }
    version=$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' <<< "$info")
    url=$(sed -n 's/.*"url"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' <<< "$info")
    sha=$(sed -n 's/.*"sha256"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' <<< "$info")
    [[ -n $version && -n $url && -n $sha ]] || { echo "更新清单读不出来。" >&2; return 1; }

    mkdir -p "$app"
    if [[ -x $app/acumen && $("$app/acumen" --version 2>/dev/null) == "$version" ]]; then
        echo "已是最新版本 $version。"
    else
        echo "正在下载知微 $version …"
        local got=''
        for mirror in "${mirrors[@]}"; do
            # 半分钟平均不到 48 KB/s 就换下一家。
            if curl -fL --connect-timeout 8 --speed-limit 49152 --speed-time 30 --progress-bar "$mirror$url" | gzip -dc > "$app/acumen.new" 2>/dev/null &&
                [[ $(sha256sum "$app/acumen.new" | cut -d' ' -f1) == "$sha" ]]; then
                got=1
                break
            fi
            echo "  ${mirror:-GitHub 直连} 没下成，换下一个 …" >&2
        done
        if [[ -z $got ]]; then
            rm -f "$app/acumen.new"
            echo "下载失败，再运行一次试试。" >&2
            return 1
        fi
        chmod 700 "$app/acumen.new"
        mv -f "$app/acumen.new" "$app/acumen"
    fi

    "$app/acumen" --install-cli || return 1
    "$prefix/bin/acumen" setup </dev/null
    echo
    # 已经在跑（重复安装、手动升级）就重启换上新版本，否则启动。
    if pgrep -f "$app/acumen" >/dev/null; then
        "$prefix/bin/acumen" restart </dev/null
        "$prefix/bin/acumen" ui </dev/null
    else
        "$prefix/bin/acumen" start </dev/null
    fi
    echo
    echo "装好了。以后用 acumen 管理它，acumen help 看全部命令。"
}

main "$@"
