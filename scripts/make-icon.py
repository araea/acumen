#!/usr/bin/env python3
"""生成知微的应用图标：一处几何，六份产物。

标记：一枚七瓣「曲奇」形（Material 3 Expressive 形状库里的 Cookie 7），
右上方挖出一个焦点圆——知微，见微知著：整体是被观察的事物，那一点是看见的细处。
挖空而不是叠色，所以单色版与彩色版是同一个轮廓。

网格沿用自适应图标的 108 单位：可见区 72，安全圆半径 33。曲奇外缘半径 32.6，
全部落在安全圆里；16px 下焦点圆直径约 2.4px，仍可辨认。

底板是连续曲率的超椭圆（n=5），不是圆角矩形：Miuix 与 Apple 的图标都用这种
没有曲率突变的边。只有 `any` 那一份自带底板轮廓；maskable 与 Apple 180 铺满
不透明方底，由系统自己裁形（HIG：不要自己画圆角）；monochrome 透明底纯白。

颜色取自 scripts/make-tokens.py 同一个种子的 HCT 色调板，图标不跟随明暗。

位图由无头 Chromium 直接渲染同一份 SVG，保证矢量与位图逐像素同源。
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

LOBES = 7
RADIUS = 30.0
AMPLITUDE = 2.6
FOCUS = (C + 8.5, C - 8.5, 7.5)
SAFE = 33.0

assert RADIUS + AMPLITUDE <= SAFE, "标记超出了安全圆"


def palette():
    # 图标是品牌的脸，用种子自己的彩度（界面里的 primary 被 Tonal Spot 收过彩度）；
    # 标记取同一支色相的 97 调，几乎是白但不刺眼。
    seed = TonalPalette.from_hct(Hct.from_int(SEED))
    tone = lambda t: "#%06x" % (seed.tone(t) & 0xFFFFFF)
    return tone(50), tone(28), tone(97)


def cookie() -> str:
    """七瓣形的闭合路径：极坐标正弦起伏取点，再用 Catmull-Rom 转三次贝塞尔。"""
    count = LOBES * 12
    points = []
    for i in range(count):
        theta = 2 * math.pi * i / count - math.pi / 2
        r = RADIUS + AMPLITUDE * math.cos(LOBES * (theta + math.pi / 2))
        points.append((C + r * math.cos(theta), C + r * math.sin(theta)))
    f = lambda v: f"{v:.2f}".rstrip("0").rstrip(".")
    out = [f"M{f(points[0][0])} {f(points[0][1])}"]
    for i in range(count):
        p0, p1 = points[i - 1], points[i]
        p2, p3 = points[(i + 1) % count], points[(i + 2) % count]
        c1 = (p1[0] + (p2[0] - p0[0]) / 6, p1[1] + (p2[1] - p0[1]) / 6)
        c2 = (p2[0] - (p3[0] - p1[0]) / 6, p2[1] - (p3[1] - p1[1]) / 6)
        out.append(f"C{f(c1[0])} {f(c1[1])} {f(c2[0])} {f(c2[1])} {f(p2[0])} {f(p2[1])}")
    x, y, r = FOCUS
    # 焦点圆反向画一圈，配合 evenodd 挖空。
    out.append(f"ZM{f(x + r)} {f(y)}A{f(r)} {f(r)} 0 1 0 {f(x - r)} {f(y)}A{f(r)} {f(r)} 0 1 0 {f(x + r)} {f(y)}Z")
    return "".join(out)


def squircle(n: float = 5.0, inset: float = 0.0) -> str:
    """|x|^n + |y|^n = 1 的超椭圆底板。"""
    half = GRID / 2 - inset
    steps = 144
    pts = []
    for i in range(steps):
        t = 2 * math.pi * i / steps
        ct, st = math.cos(t), math.sin(t)
        x = half * math.copysign(abs(ct) ** (2 / n), ct)
        y = half * math.copysign(abs(st) ** (2 / n), st)
        pts.append(f"{C + x:.2f} {C + y:.2f}")
    return "M" + "L".join(pts) + "Z"


def svg(background: str, *, tile: str | None, mark: str, gradient: tuple[str, str] | None) -> str:
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
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 108 108" role="img" aria-label="知微">'
        f"<title>知微</title>{defs}{base}"
        f'<path d="{cookie()}" fill="{mark}" fill-rule="evenodd"/></svg>\n'
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
    # 铺满的那两份（maskable 与 Apple）必须不透明，存成 RGB。
    image = Image.open(target)
    if opaque:
        image = image.convert("RGB")
    image.save(target, optimize=True)


def main() -> None:
    light, dark, on = palette()
    browser = shutil.which("chromium-browser") or shutil.which("chromium") or shutil.which("google-chrome")
    if not browser:
        raise SystemExit("找不到 Chromium：pkg install chromium")

    any_icon = svg(dark, tile="squircle", mark=on, gradient=(light, dark))
    full = svg(dark, tile="square", mark=on, gradient=(light, dark))
    mono = svg("none", tile=None, mark="#ffffff", gradient=None)
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
