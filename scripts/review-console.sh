#!/usr/bin/env bash
# ============================================================================
# 界面的样张：三种宽度 × 七页，摆在 ${TMPDIR:-/tmp}/ayjx-console 里。
#
# 与 scripts/review-cards.sh 是一对：那一份管五张卡片图，这一份管界面。
# 两边都要「看一眼真东西」——卡片的审美在图上，界面的手感也在图上。
#
# 本脚本只读，不切换线上插件。写操作、压力与前后台切换回归用 node tests/console.cjs。
# 除了出图，这里还跑三条交互断言。它们盯的是「点了有没有用」：
#   1. 底部导航每一格都换页（2026-09-16 之前这里是死的：点击只委派在 #view 上，
#      而导航是它的兄弟节点，整条导航点不动）；
#   2. 点行身进得了详情（窄屏换页、宽屏只换右边那一格）；
#   3. 「恢复默认」弹的是自家对话框，不是浏览器的 confirm。
#
# 为什么不用 `chromium --screenshot`：日志页有一条长连接（SSE），页面永远不进入
# 空闲，headless 的截图会一直等下去（实测挂满超时）。这里改用 chromedriver 的
# WebDriver 协议——导航之后自己数秒，再叫它截图，跟页面空闲不空闲无关。
#
# 用法：
#   bash scripts/review-console.sh                   # 用本仓库正在跑的那一份
#   bash scripts/review-console.sh <带口令的地址>     # 指到别处
# 前置：目标实例在跑，且 chromedriver 在 PATH 里（pkg install chromium-chromedriver）。
# ============================================================================
set -euo pipefail

repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=${AYJX_CONSOLE_SHOTS:-${TMPDIR:-/tmp}/ayjx-console}
port=${CHROMEDRIVER_PORT:-9516}
base=${1:-}

if [[ -z "$base" ]]; then
  url_file="$repo/target/release/data/console/url"
  if [[ ! -s "$url_file" ]]; then
    printf '没有控制台地址：先 ./bot start，或者把地址当第一个参数传进来。\n' >&2
    exit 1
  fi
  base=$(head -n 1 "$url_file")
fi

command -v chromedriver >/dev/null || {
  printf '缺 chromedriver：pkg install chromium-chromedriver\n' >&2
  exit 1
}

mkdir -p "$out"
printf '样张输出到 %s\n' "$out"

chromedriver --port="$port" --log-level=SEVERE >"$out/chromedriver.log" 2>&1 &
driver=$!
trap 'kill "$driver" 2>/dev/null || true' EXIT
sleep 2

AYJX_SHOT_URL="$base" AYJX_SHOT_OUT="$out" AYJX_SHOT_PORT="$port" python3 - <<'PY'
import base64
import json
import os
import sys
import time
import urllib.error
import urllib.request

base = os.environ["AYJX_SHOT_URL"]
out = os.environ["AYJX_SHOT_OUT"]
port = int(os.environ["AYJX_SHOT_PORT"])
root = f"http://127.0.0.1:{port}"

# 三档版式各拍一遍：紧凑是底部导航条，中等是导航轨，宽是抽屉。
# 高度取 1060 是有原因的——headless 的 window rect 含浏览器自己那圈，
# 落到视口里约 917，与一部手机差不多。
VIEWPORTS = [
    ("compact", 430, 1060),
    ("medium", 800, 1060),
    ("expanded", 1400, 900),
]

PAGES = [
    ("overview", ""),
    ("plugins", "plugins"),
    ("plugin-detail", "plugins/oai"),
    ("ambient", "ambient"),
    ("logs", "logs"),
    ("command", "command"),
    ("settings", "settings"),
]

failures = []


def call(method, path, payload=None):
    data = json.dumps(payload).encode() if payload is not None else None
    request = urllib.request.Request(
        root + path, data=data, method=method, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.loads(response.read() or b"{}")


def open_session(width, height):
    session = call("POST", "/session", {"capabilities": {"alwaysMatch": {
        "browserName": "chrome",
        "goog:chromeOptions": {"args": ["--headless=new", "--no-sandbox", "--disable-gpu"]},
    }}})["value"]["sessionId"]
    call("POST", f"/session/{session}/window/rect", {"width": width, "height": height, "x": 0, "y": 0})
    return session


def js(session, script, args=None):
    return call(
        "POST",
        f"/session/{session}/execute/sync",
        {"script": script, "args": args or []},
    )["value"]


def element(session, selector):
    return call(
        "POST", f"/session/{session}/element", {"using": "css selector", "value": selector}
    )["value"]["element-6066-11e4-a52e-4f735466cecf"]


def click(session, selector):
    """先滚到视口正中再点。WebDriver 自己那套 scrollIntoView 不看固定定位的
    底栏，元素会被停在导航条底下，点到的其实是导航。"""
    js(session, "document.querySelector(arguments[0]).scrollIntoView({block:'center'});", [selector])
    time.sleep(0.4)
    call("POST", f"/session/{session}/element/{element(session, selector)}/click", {})


def goto(session, route, wait=2.2):
    call("POST", f"/session/{session}/url", {"url": f"{base}#/{route}"})
    time.sleep(wait)


def shoot(session, name):
    shot = call("GET", f"/session/{session}/screenshot")["value"]
    (open(os.path.join(out, f"{name}.png"), "wb")).write(base64.b64decode(shot))


try:
    for label, width, height in VIEWPORTS:
        session = open_session(width, height)
        for name, route in PAGES:
            goto(session, route)
            shoot(session, f"{label}-{name}")
            print(f"  {label:9s} {name:14s} 已出图")

        if label == "compact":
            # 一：底部导航逐格换页
            for item in ["logs", "plugins", "ambient", "command", "overview"]:
                goto(session, "overview", wait=1.8)
                click(session, f"#nav [data-nav={item}]")
                time.sleep(1.0)
                got = js(session, "return location.hash")
                want = "#/overview" if item == "overview" else f"#/{item}"
                if got != want:
                    failures.append(f"点底部导航的「{item}」去到 {got!r}，应当是 {want!r}")
            print("  底部导航：逐格点过")

            # 二：点行身进详情；开关写入在隔离测试里验证
            goto(session, "plugins", wait=2.0)
            click(session, "#plugin-list [data-plugin=logger] .row-hit")
            time.sleep(1.2)
            got = js(session, "return location.hash")
            if got != "#/plugins/logger":
                failures.append(f"点行身没进详情（{got}）")
            print("  行身：点过（未改插件开关）")

            # 三：只打开确认框，随后取消，不执行恢复
            goto(session, "plugins/oai")
            click(session, "[data-reset]")
            time.sleep(0.8)
            if not js(session, "return !!document.querySelector('dialog[open]')"):
                failures.append("「恢复默认」没有弹出确认对话框")
            else:
                shoot(session, "compact-dialog")
                click(session, "[data-answer=no]")
            print("  确认对话框：弹出过")
        else:
            # 宽屏点一行只换右边那一格，列表留在原地；中等屏还是整页换，
            # 所以「并排」这一条只在抽屉那一档查。
            goto(session, "plugins")
            click(session, "#plugin-list [data-plugin=console] .row-hit")
            time.sleep(1.6)
            if label == "expanded":
                picked = js(session, "return (document.querySelector('#plugin-detail .key')||{}).textContent")
                if picked != "console":
                    failures.append(f"点列表右边没换成 console（拿到 {picked!r}）")
                if js(session, "return location.hash") != "#/plugins/console":
                    failures.append("点列表之后地址没跟上，刷新会回到空的那一页")
                shoot(session, f"{label}-plugin-split")
                print(f"  {label:9s} 列表与详情并排：右边换成 {picked}")
            else:
                got = js(session, "return location.hash")
                if got != "#/plugins/console":
                    failures.append(f"中等屏点列表没进详情（{got}）")
                print(f"  {label:9s} 点列表：整页换到 {got}")

        call("DELETE", f"/session/{session}")
finally:
    pass

print(f"看样张：ls {out}")
if failures:
    print("没过的项：")
    for line in failures:
        print(f"  - {line}")
    sys.exit(1)
print("三条只读交互断言全过。")
PY
