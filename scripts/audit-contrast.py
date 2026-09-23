#!/usr/bin/env python3
# ============================================================================
# 界面的对比度与可访问名盘点：WCAG 2.2 AA 里能算出来的那几条。
#
# 与 tests/console.cjs 是一对：那一份在隔离夹具上测交互与出图，这一份管
# 「算出来过不过」。审美靠图，下限靠这台机器——两样都不能只靠眼睛。
#
# 查五件事，六个页面 × 三档宽度 × 明暗两档：
#   1.4.3  对比度：正文 4.5:1，大字（≥24px，或 ≥18.66px 且 ≥700）3:1；
#   1.4.11 非文本对比度：控件自己的边界（描边按钮、芯片、输入框、开关轨道）
#          与它相邻的颜色要有 3:1——**只查 outline 那一级**，分隔线不在此列；
#   4.1.2  可访问名：每个 button / link / role=switch 得有个名字；
#   2.5.8  触控目标：可点的东西不小于 24×24（本仓库触屏取 48、精确指针取 40，
#          那一条由 tests/console.cjs 管，这里只兜底）；
#   1.4.3  占位文字：它不在任何文本节点里，逐元素那套看不见它，得单独用
#          `::placeholder` 的伪元素样式取一次色——「还没写进去的答案」也是文字。
#
# 做法是逐元素算：文字颜色（含半透明）叠到祖先里第一个不透明的底色上，
# 边界颜色叠到宿主底色上，再按 WCAG 的相对亮度公式比。color-mix() 这类
# 现代写法由浏览器 getComputedStyle 先算成 rgb()，这里读到的是算完的值。
#
# 用法：
#   python3 scripts/audit-contrast.py                    # 用本仓库正在跑的那一份
#   python3 scripts/audit-contrast.py <带口令的地址>      # 指到别处
#   ACUMEN_AUDIT_OUT=dir python3 scripts/audit-contrast.py
# 前置：目标实例在跑，且 chromedriver 在 PATH 里（pkg install chromium-chromedriver）。
# 退出码：有任意一条不过就是 1。
# ============================================================================

import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PORT = int(os.environ.get("CHROMEDRIVER_PORT", "9531"))
OUT = os.environ.get("ACUMEN_AUDIT_OUT", os.path.join(os.environ.get("TMPDIR", "/tmp"), "acumen-contrast"))

PAGES = ["", "plugins", "plugins/oai", "ambient", "logs", "settings"]
VIEWPORTS = [("compact", 430, 1060), ("medium", 800, 1060), ("expanded", 1400, 900)]
THEMES = ["light", "dark"]

PROBE = r"""
return (function () {
  // color-mix() 的计算值是 color(srgb r g b / a)，分量在 0–1；其余是 rgb()/rgba()。
  function parseColor(s) {
    var text = String(s);
    var srgb = text.match(/color\(srgb\s+([^)]+)\)/);
    if (srgb) {
      var q = srgb[1].split(/[\s\/]+/).filter(function (x) { return x.length; }).map(Number);
      return { r: q[0] * 255, g: q[1] * 255, b: q[2] * 255, a: q.length > 3 ? q[3] : 1 };
    }
    var m = text.match(/rgba?\(([^)]+)\)/);
    if (!m) return null;
    var p = m[1].split(/[,\s\/]+/).filter(function (x) { return x.length; }).map(Number);
    return { r: p[0], g: p[1], b: p[2], a: p.length > 3 ? p[3] : 1 };
  }
  function f(v) { v /= 255; return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); }
  function lum(c) { return 0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b); }
  function ratio(a, b) {
    var la = lum(a), lb = lum(b);
    return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
  }
  function over(fg, bg) {
    return {
      r: fg.r * fg.a + bg.r * (1 - fg.a),
      g: fg.g * fg.a + bg.g * (1 - fg.a),
      b: fg.b * fg.a + bg.b * (1 - fg.a),
      a: 1,
    };
  }
  function opaque(c) { return c && c.a >= 0.999; }
  // 从元素往上找第一层真正不透明的底，半透明的层按顺序叠上去。
  function backdrop(el) {
    var node = el, acc = null;
    while (node && node.nodeType === 1) {
      var c = parseColor(getComputedStyle(node).backgroundColor);
      if (c && c.a > 0) {
        acc = acc ? over(acc, c) : c;
        if (opaque(acc)) return acc;
      }
      node = node.parentElement;
    }
    var root = parseColor(getComputedStyle(document.documentElement).backgroundColor);
    if (!root || root.a === 0) root = { r: 255, g: 255, b: 255, a: 1 };
    return acc ? over(acc, root) : root;
  }
  function shown(el) {
    var cs = getComputedStyle(el);
    if (cs.display === "none" || cs.visibility === "hidden" || Number(cs.opacity) === 0) return false;
    var r = el.getBoundingClientRect();
    return r.width >= 1 && r.height >= 1;
  }
  function path(el) {
    var bits = [], node = el;
    while (node && node.nodeType === 1 && bits.length < 4) {
      if (node.id) { bits.unshift(node.tagName.toLowerCase() + "#" + node.id); break; }
      var s = node.tagName.toLowerCase();
      if (node.classList.length) s += "." + Array.prototype.slice.call(node.classList, 0, 2).join(".");
      bits.unshift(s);
      node = node.parentElement;
    }
    return bits.join(" > ");
  }
  function rgb(c) { return "rgb(" + Math.round(c.r) + " " + Math.round(c.g) + " " + Math.round(c.b) + ")"; }

  var text = [], borders = [], nameless = [], tiny = [], hints = [];

  document.querySelectorAll("*").forEach(function (el) {
    if (!shown(el)) return;
    var cs = getComputedStyle(el);

    var own = Array.prototype.filter.call(el.childNodes, function (n) {
      return n.nodeType === 3 && n.textContent.trim().length > 0;
    });
    // 停用的控件不在 1.4.3 的范围内（WCAG 明文豁免「非活动的界面组件」）。
    var inactive = !!el.closest(":disabled, [aria-disabled=true]");
    if (own.length && !inactive) {
      var fg = parseColor(cs.color);
      var bg = backdrop(el);
      var eff = fg.a < 1 ? over(fg, bg) : fg;
      var size = parseFloat(cs.fontSize);
      var weight = parseInt(cs.fontWeight, 10) || 400;
      var need = (size >= 24 || (size >= 18.66 && weight >= 700)) ? 3 : 4.5;
      var r = ratio(eff, bg);
      if (r < need - 0.005) {
        text.push({ sel: path(el), size: size, weight: weight, need: need,
                    ratio: Math.round(r * 100) / 100, fg: cs.color, bg: rgb(bg),
                    sample: el.textContent.trim().slice(0, 28) });
      }
    }

    // 控件的边界。取的是「画出来那一圈线」与它**外面**那层底的关系：
    // 输入框自己有填充色，那圈线挨着的是宿主底色，不是自己的填充。
    if (el.matches("input, textarea, select, button, a[href], .switch")) {
      var bw = parseFloat(cs.borderTopWidth);
      var bc = parseColor(cs.borderTopColor);
      if (bw > 0 && bc && bc.a > 0.05) {
        var outer = backdrop(el.parentElement || el);
        var line = over(bc, outer);
        var rr = ratio(line, outer);
        if (rr < 3) {
          borders.push({ sel: path(el), ratio: Math.round(rr * 100) / 100,
                         border: cs.borderTopColor, around: rgb(outer) });
        }
      }
      var rect = el.getBoundingClientRect();
      if (rect.width < 24 || rect.height < 24) {
        tiny.push({ sel: path(el), w: Math.round(rect.width), h: Math.round(rect.height) });
      }
    }

    if (el.matches("button, a[href], [role=switch], [role=button]")) {
      var label = (el.getAttribute("aria-label") || el.textContent || "").trim();
      if (!label && !el.getAttribute("aria-labelledby")) nameless.push({ sel: path(el) });
    }

    // 占位文字。它一辈子进不了文本节点，逐元素那套扫不到，所以在同一个循环里
    // 用伪元素的样式单独取一次色；底就按输入框自己的填充算（它是不透明的）。
    if (el.matches("input, textarea") && el.getAttribute("placeholder")) {
      var pcs = getComputedStyle(el, "::placeholder");
      var pc = parseColor(pcs.color);
      if (pc && pc.a > 0.05) {
        var pbg = backdrop(el);
        var peff = pc.a < 1 ? over(pc, pbg) : pc;
        var pr = ratio(peff, pbg);
        if (pr < 4.5 - 0.005) {
          hints.push({ sel: path(el), ratio: Math.round(pr * 100) / 100,
                       fg: pcs.color, bg: rgb(pbg),
                       sample: el.getAttribute("placeholder").slice(0, 28) });
        }
      }
    }
  });

  return { text: text, borders: borders, nameless: nameless, tiny: tiny, hints: hints,
           dark: matchMedia("(prefers-color-scheme: dark)").matches };
})()
"""


def http(method, path, payload=None, timeout=90):
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(
        f"http://127.0.0.1:{PORT}{path}",
        data=data,
        method=method,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read() or b"{}")


def main():
    base = sys.argv[1] if len(sys.argv) > 1 else None
    if not base:
        url_file = os.path.join(REPO, "target/release/data/console/url")
        if not os.path.isfile(url_file) or os.path.getsize(url_file) == 0:
            print("没有控制台地址：先 ./bot start，或者把地址当第一个参数传进来。", file=sys.stderr)
            return 1
        with open(url_file) as fh:
            base = fh.readline().strip()
    if not shutil.which("chromedriver"):
        print("缺 chromedriver：pkg install chromium-chromedriver", file=sys.stderr)
        return 1

    os.makedirs(OUT, exist_ok=True)
    driver = subprocess.Popen(
        ["chromedriver", f"--port={PORT}", "--log-level=SEVERE"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    time.sleep(2.5)
    report = {}
    try:
        for label, width, height in VIEWPORTS:
            session = http("POST", "/session", {"capabilities": {"alwaysMatch": {
                "browserName": "chrome",
                "goog:chromeOptions": {"args": ["--headless=new", "--no-sandbox", "--disable-gpu"]},
            }}})["value"]["sessionId"]
            http("POST", f"/session/{session}/window/rect",
                 {"width": width, "height": height, "x": 0, "y": 0})
            for theme in THEMES:
                # 页面自己按 prefers-color-scheme 切深浅，所以从这一层压。
                http("POST", f"/session/{session}/goog/cdp/execute", {
                    "cmd": "Emulation.setEmulatedMedia",
                    "params": {"features": [{"name": "prefers-color-scheme", "value": theme}]},
                })
                for route in PAGES:
                    name = route or "overview"
                    http("POST", f"/session/{session}/url", {"url": f"{base}#/{route}"})
                    time.sleep(2.2)
                    report[f"{label}/{theme}/{name}"] = http(
                        "POST", f"/session/{session}/execute/sync", {"script": PROBE, "args": []}
                    )["value"]
            http("DELETE", f"/session/{session}")
    finally:
        driver.terminate()

    with open(os.path.join(OUT, "report.json"), "w") as fh:
        json.dump(report, fh, ensure_ascii=False, indent=1)

    counts = {"text": 0, "borders": 0, "nameless": 0, "tiny": 0, "hints": 0}
    seen = set()
    for page in sorted(report):
        got = report[page]
        rows = []
        for key in ("text", "borders", "nameless", "tiny", "hints"):
            for one in got[key]:
                # 同一处会在多页多档重复出现，按「选择器 + 是什么」去重后再报。
                ident = (key, one["sel"], one.get("sample") or one.get("border") or "")
                if ident in seen:
                    continue
                seen.add(ident)
                counts[key] += 1
                rows.append((key, one))
        if not rows:
            continue
        print(f"\n== {page}")
        for key, one in rows:
            if key == "text":
                print(f"   文字 {one['ratio']:>5} < {one['need']}  {one['size']}px/{one['weight']}"
                      f"  {one['fg']} on {one['bg']}  {one['sel']}  {one['sample']!r}")
            elif key == "borders":
                print(f"   边界 {one['ratio']:>5} < 3    {one['border']} 绕着 {one['around']}  {one['sel']}")
            elif key == "tiny":
                print(f"   目标 {one['w']}×{one['h']} < 24   {one['sel']}")
            elif key == "hints":
                print(f"   占位 {one['ratio']:>5} < 4.5  {one['fg']} on {one['bg']}  "
                      f"{one['sel']}  {one['sample']!r}")
            else:
                print(f"   无名  {one['sel']}")

    total = sum(counts.values())
    print(f"\n盘点了 {len(report)} 个页面快照（{len(VIEWPORTS)} 档宽度 × {len(THEMES)} 档明暗）。")
    if total:
        print(f"不过的：文字 {counts['text']} 处、控件边界 {counts['borders']} 处、"
              f"触控目标 {counts['tiny']} 处、无可访问名 {counts['nameless']} 处、"
              f"占位文字 {counts['hints']} 处。")
        print(f"明细：{os.path.join(OUT, 'report.json')}")
        return 1
    print("WCAG 2.2 AA 里能算的那几条全过：对比度、控件边界、触控目标、可访问名、占位文字。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
