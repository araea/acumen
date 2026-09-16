#!/usr/bin/env bash
# ============================================================================
# 控制台的样张：把每一页拍成本地图片，摆在 ${TMPDIR:-/tmp}/ayjx-console 里。
#
# 与 scripts/review-cards.sh 是一对：那一份管六张卡片图，这一份管应用界面。
# 两边都要「看一眼真东西」——卡片的审美在图上，界面的手感也在图上。
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
  url_file="${AYJX_DATA_DIR:-$repo/target/release/data}/console/url"
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
import json
import os
import time
import urllib.error
import urllib.request
import base64

base = os.environ["AYJX_SHOT_URL"]
out = os.environ["AYJX_SHOT_OUT"]
port = int(os.environ["AYJX_SHOT_PORT"])
root = f"http://127.0.0.1:{port}"

PAGES = [
    ("overview", ""),
    ("plugins", "plugins"),
    ("plugin-ambient", "plugins/ambient"),
    ("plugin-console", "plugins/console"),
    ("ambient", "ambient"),
    ("logs", "logs"),
    ("command", "command"),
    ("settings", "settings"),
]


def call(method, path, payload=None):
    data = json.dumps(payload).encode() if payload is not None else None
    request = urllib.request.Request(
        root + path, data=data, method=method,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.loads(response.read() or b"{}")


session = call("POST", "/session", {
    "capabilities": {"alwaysMatch": {
        "browserName": "chrome",
        "goog:chromeOptions": {"args": ["--headless=new", "--no-sandbox", "--disable-gpu"]},
    }},
})["value"]["sessionId"]
sid = f"/session/{session}"

# 手机尺寸，与设计时的版心和 ±44px 触摸目标同一口径。
call("POST", sid + "/window/rect", {"width": 430, "height": 932, "x": 0, "y": 0})

for name, route in PAGES:
    call("POST", sid + "/url", {"url": f"{base}#/{route}"})
    # 页面自己拉数据、画骨架、再换内容；给足三个节拍，不靠轮询某个元素。
    time.sleep(3.5)
    shot = call("GET", sid + "/screenshot")["value"]
    target = os.path.join(out, f"{name}.png")
    with open(target, "wb") as handle:
        handle.write(base64.b64decode(shot))
    size = os.path.getsize(target)
    print(f"  {name:16s} {size:>8d} 字节")

call("DELETE", sid)
PY

printf '看样张：ls %s\n' "$out"
