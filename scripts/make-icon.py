#!/usr/bin/env python3
"""生成知微的应用图标：一处几何，六份产物。

标记是一枚「缺了一角的印」：一个圆角方环（超椭圆外缘、圆形内孔）在右上角
留出一道缺口，缺口里嵌着一粒暖色光点。环是「看见的整体」，光点是「被看见的
细处」——知微，见微知著。缺口与光点都朝右上，静态里带一点方向感。

几何全部落在自适应图标的安全圆内（108 画布，半径 33）：环的外缘最远处
32.8，光点中心到画布中心 22.3、半径 6.2。16px 下环宽约 1.7 个设备像素，
光点约 1.8 像素，两者都还分得开。

底板是连续曲率的超椭圆（n=5），不是圆角矩形。只有 any 那一份自带底板；
maskable 与 Apple 180 铺满不透明方底，由系统自己裁形；monochrome 透明底纯白。

颜色取自 scripts/make-tokens.py 同一个种子的 HCT 色调板，图标不跟随明暗。

位图由无头 Chromium 直接渲染同一份 SVG，保证矢量与位图同源。
运行：python3 scripts/make-icon.py（需要 chromium-browser、materialyoucolor 与 Pillow）
"""

import math
import shutil
import subprocess
import tempfile
from pathlib import Path

from materialyoucolor.hct.hct import Hct
from materialyoucolor.palettes.tonal_palette import TonalPalette
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "res/console"

SEED = 0xFF3F51B5
GRID = 108.0
C = GRID / 2

# 环与缺口。
OUTER = 27.6          # 超椭圆外缘的轴半径
INNER = 16.2          # 圆形内孔
SQUIRCLE_N = 4.0      # 外缘的方度（2 是正圆，越大越方）
GAP_DEG = 46.0        # 缺口张角
GAP_ROT = -45.0       # 缺口朝向：右上
DOT_R = 6.2
DOT_RADIUS = 22.2     # 光点在缺口里的半径位置
SAFE = 33.0
LAMP_COLOR = "#ffc65c"

assert (OUTER * 2 ** (0.5 - 1 / SQUIRCLE_N)) <= SAFE, "环的外缘超出了安全圆"
assert DOT_RADIUS + DOT_R <= SAFE, "光点超出了安全圆"
# 缺口在光点所在半径上的弧长，必须容得下光点再留一点缝。
_arc = math.radians(GAP_DEG) * DOT_RADIUS
assert _arc > DOT_R * 2 + 2.0, "缺口太窄，光点会碰到环"
# 16px 下的环宽至少 1 个设备像素。
assert (OUTER - INNER) / GRID * 16 >= 1.0, "16px 下环会细到看不见"


def palette():
    # 图标是品牌的脸，用种子自己的彩度；标记取 97 调，几乎是白但不刺眼。
    seed = TonalPalette.from_hct(Hct.from_int(SEED))
    tone = lambda t: "#%06x" % (seed.tone(t) & 0xFFFFFF)
    return tone(57), tone(16), tone(97)


def squircle(n: float = 5.0, half: float = GRID / 2) -> str:
    """|x|^n + |y|^n = 1 的超椭圆；作为底板或环的外缘。"""
    steps = 240
    pts = []
    for i in range(steps):
        t = 2 * math.pi * i / steps
        ct, st = math.cos(t), math.sin(t)
        x = half * math.copysign(abs(ct) ** (2 / n), ct)
        y = half * math.copysign(abs(st) ** (2 / n), st)
        pts.append(f"{C + x:.2f} {C + y:.2f}")
    return "M" + "L".join(pts) + "Z"


def circle(cx: float, cy: float, r: float) -> str:
    return f"M{cx - r:.2f} {cy:.2f}a{r:.2f} {r:.2f} 0 1 0 {2 * r:.2f} 0a{r:.2f} {r:.2f} 0 1 0 {-2 * r:.2f} 0Z"


def ring() -> str:
    """外缘超椭圆 + 内孔圆形，合在一个路径里用 evenodd 挖空。"""
    return squircle(SQUIRCLE_N, OUTER) + " " + circle(C, C, INNER)


def gap_wedge() -> str:
    """从画布中心向外的一大块扇形，用作遮罩把缺口切掉。"""
    a = math.radians(GAP_ROT + GAP_DEG / 2)
    b = math.radians(GAP_ROT - GAP_DEG / 2)
    far = GRID
    return (
        f"M{C} {C} L{C + far * math.cos(a):.2f} {C + far * math.sin(a):.2f} "
        f"A{far} {far} 0 0 0 {C + far * math.cos(b):.2f} {C + far * math.sin(b):.2f} Z"
    )


def dot() -> tuple[float, float]:
    rad = math.radians(GAP_ROT)
    return C + DOT_RADIUS * math.cos(rad), C + DOT_RADIUS * math.sin(rad)


def svg(background: str, *, tile: str | None, mark: str, lamp: str, gradient: tuple[str, str] | None) -> str:
    defs, fill = "", background
    if gradient:
        defs = (
            '<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">'
            f'<stop offset="0" stop-color="{gradient[0]}"/><stop offset="1" stop-color="{gradient[1]}"/>'
            "</linearGradient></defs>"
        )
        fill = "url(#g)"
    base = ""
    if tile == "squircle":
        base = f'<path d="{squircle()}" fill="{fill}"/>'
    elif tile == "square":
        base = f'<rect width="108" height="108" fill="{fill}"/>'
    dx, dy = dot()
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 108 108" role="img" aria-label="知微">'
        f"<title>知微</title>{defs}{base}"
        '<mask id="gap"><rect width="108" height="108" fill="#fff"/>'
        f'<path d="{gap_wedge()}" fill="#000"/></mask>'
        f'<g mask="url(#gap)"><path d="{ring()}" fill="{mark}" fill-rule="evenodd"/></g>'
        f'<circle cx="{dx:.2f}" cy="{dy:.2f}" r="{DOT_R}" fill="{lamp}"/></svg>\n'
    )


def rasterize(markup: str, size: int, target: Path, browser: str, work: Path, opaque: bool = False) -> None:
    page = work / f"{target.stem}.html"
    page.write_text(
        "<!doctype html><meta charset=utf-8><style>html,body{margin:0;background:transparent}"
        f"img{{display:block;width:{size}px;height:{size}px}}</style>"
        f'<img src="{target.stem}.svg">'
    )
    (work / f"{target.stem}.svg").write_text(markup)
    subprocess.run(
        [browser, "--headless=new", "--no-sandbox", "--disable-gpu", "--hide-scrollbars",
         "--default-background-color=00000000", f"--window-size={size},{size}",
         f"--screenshot={target}", page.as_uri()],
        check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=60,
    )
    # 截图是未压缩级别的 PNG；重新编码一次，内容不变、体积小一半以上。
    image = Image.open(target)
    if opaque:
        image = image.convert("RGB")
    image.save(target, optimize=True)


def main() -> None:
    light, dark, on = palette()
    browser = shutil.which("chromium-browser") or shutil.which("chromium") or shutil.which("google-chrome")
    if not browser:
        raise SystemExit("找不到 Chromium：pkg install chromium")

    any_icon = svg(dark, tile="squircle", mark=on, lamp=LAMP_COLOR, gradient=(light, dark))
    full = svg(dark, tile="square", mark=on, lamp=LAMP_COLOR, gradient=(light, dark))
    mono = svg("none", tile=None, mark="#ffffff", lamp="#ffffff", gradient=None)
    (OUT / "icon.svg").write_text(any_icon)
    (OUT / "icon-monochrome.svg").write_text(mono)

    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        rasterize(any_icon, 192, OUT / "icon-192.png", browser, work)
        rasterize(any_icon, 512, OUT / "icon-512.png", browser, work)
        rasterize(full, 512, OUT / "icon-maskable-512.png", browser, work, opaque=True)
        rasterize(full, 180, OUT / "apple-touch-icon.png", browser, work, opaque=True)
    stale = OUT / "icon-monochrome-512.png"
    if stale.exists():
        stale.unlink()
    print(f"已生成：icon.svg、icon-monochrome.svg 与四张 PNG（底色 {light}→{dark}，标记 {on}）")


if __name__ == "__main__":
    main()
