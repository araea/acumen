#!/usr/bin/env python3
"""知微那个标记：一处几何，一组产物。

—— 这枚标记是什么 ——

**一环一点**。环是开阔的那个圈，点是圈里最小的那一处：视线先进环、再落到点上，
而认识一个人正好是从那一点开始的。取《周易·系辞下》「君子知微知彰」——从已经显
出来的，看出还没说出口的。

图元只有两只圆：外环与圆心那枚点，环的外径比点的直径是 2.7 : 1。这个比例是收过的
（先在 256px 上比过四组），再大就成了「圆里一个圆」，再小在 24px 上就没了。

接入层那个模块（satori-qq / 知弦）的标记也是几何图形而不是「弦」字，两枚放在一起是
一套笔画语言：白、等宽、圆头、旋转 180° 自重合。上一版的标记是「知言」那个「言」字；
名字换掉之后，把标记继续押在一个已经不对的概念上没有道理。

坐标只写在这里一遍，产物都是它的输出：

    res/console/icon.svg                网页标签页、清单里的矢量那一项、页头那个小标
    res/console/icon-192.png            装到桌面用的小图（Android 桌面、浏览器）
    res/console/icon-512.png            装到桌面用的大图
    res/console/icon-maskable-512.png   交给系统裁形状的那一张：底色铺满整个画布
    res/console/icon-monochrome.svg     单色层：透明的底 + 纯白的图形
    res/console/icon-monochrome-512.png 单色层的位图版
    res/console/apple-touch-icon.png    iOS 加到主屏幕用的那一张（180，不透明）

跑法：python3 scripts/make-icon.py（无第三方依赖，只在改图标时跑一次）。

—— 三层与四个尺寸的依据 ——

**Android 自适应图标（Adaptive Icons）**：画布 108×108，系统用自己的形状去裁
（圆、方、squircle、水滴……），裁掉的是画布的角。所以：

- **可见区**是正中 72×72——裁完之后一定露出来的是这一块；
- **安全区**是正中直径 66 的圆——任何形状的遮罩都不会切到圆里的东西。

标记的外缘因此收在半径 32 上，比安全半径 33 还留一点。上一版那个「言」字的
外接框是 60×64，右下角离中心 45.7——圆形遮罩一刀下去，口的那两个下角就没了。
这是本次重做最实质的一处修正，`main()` 里那条断言一直盯着它。

**Material Design 图标绘制**：图形画在画布正中，笔画宽度一致、端点与拐角都是圆的，
比例按光学修正定（点在视觉上要略大于它的几何直径才不显小），不是照抄坐标。

**Android 主题图标（Themed Icons，Android 13+）**：单独的 monochrome 层，透明的底 +
纯白的图形，原生平台可按壁纸取色。这一层不许有第二颜色、不许有阴影。
本仓库输出的是 Web App manifest 的 monochrome 图像，实际着色依赖浏览器/启动器，
不是 Android 原生 AdaptiveIconDrawable 资源；原生封装需要另行提供分层资源。

**Apple 的主屏图标（HIG）**：铺满、不透明、**不自己画圆角**——iOS 会拿自己的连续
圆角去裁，画了圆角就会被裁出两层边。`apple-touch-icon.png` 因此与遮罩版同一套几何。

普通图标（any）那一份自带圆角：它不会被任何遮罩裁，摆在标签页与桌面书签里就是它
自己，圆角半径取边长的 22.37%，与 iOS 的连续圆角同一个比例，两处才是一张脸。
"""

import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# —— 网格 ——
CANVAS = 108.0
CENTER = CANVAS / 2
VISIBLE = 72.0
SAFE = 66.0
CORNER = 0.2237 * CANVAS

# 底色。主色取自 res/cards/m3e.css 的 --md-sys-color-primary（控制台那套方案，
# 松绿）。图标是脸面，跟主题走会变成两张脸，所以这里钉死一个值：同色相的一段
# 斜向渐变，亮端在上左、暗端在下右。比一整块平涂多一点纵深，也没有多出第二个色相。
SHADE_LIGHT = (0x2B, 0x7C, 0x64)
SHADE_DARK = (0x14, 0x43, 0x37)

# 图形用白。写成一个十六进制串，不再往 SVG 里塞元组——旧版就是这里把
# `fill="(255, 255, 255)"` 写进了文件，浏览器按非法值处理、回落成黑色，
# 于是矢量那份是黑字而位图那份是白字，同一个产品两张脸。
ON_SHADE = (0xFF, 0xFF, 0xFF)
ON_SHADE_HEX = "#%02x%02x%02x" % ON_SHADE

# —— 标记 ——
RING_OUTER = 32.0
RING_STROKE = 6.5
DOT_RADIUS = 12.0

# 图元表。(kind, cx, cy, …)
#   ring —— 一只圆环，给外半径与内半径
#   disk —— 一枚实心圆
SHAPES = [
    ("ring", CENTER, CENTER, RING_OUTER, RING_OUTER - RING_STROKE),
    ("disk", CENTER, CENTER, DOT_RADIUS),
]

SUBSAMPLES = 4


def in_round_rect(px: float, py: float, box) -> bool:
    """点在不在这只圆角矩形里（圆角按四角各一个圆算）。底色的圆角用它。"""
    x, y, w, h, r = box
    if px < x or px > x + w or py < y or py > y + h:
        return False
    r = min(r, w / 2, h / 2)
    if r <= 0:
        return True
    # 中间那两条十字带一定在里面
    if x + r <= px <= x + w - r or y + r <= py <= y + h - r:
        return True
    cx = min(max(px, x + r), x + w - r)
    cy = min(max(py, y + r), y + h - r)
    return (px - cx) ** 2 + (py - cy) ** 2 <= r * r


def in_disk(px: float, py: float, cx: float, cy: float, r: float) -> bool:
    return (px - cx) ** 2 + (py - cy) ** 2 <= r * r


def in_ring(px: float, py: float, cx: float, cy: float, outer: float, inner: float) -> bool:
    d = (px - cx) ** 2 + (py - cy) ** 2
    return inner * inner <= d <= outer * outer


def shape_contains(shape, px: float, py: float) -> bool:
    if shape[0] == "ring":
        _, cx, cy, outer, inner = shape
        return in_ring(px, py, cx, cy, outer, inner)
    _, cx, cy, r = shape
    return in_disk(px, py, cx, cy, r)


MARK_BBOX = (
    min(s[1] - (s[3] if s[0] == "ring" else s[3]) for s in SHAPES),
    min(s[2] - (s[3] if s[0] == "ring" else s[3]) for s in SHAPES),
    max(s[1] + s[3] for s in SHAPES),
    max(s[2] + s[3] for s in SHAPES),
)


def in_mark(px: float, py: float) -> bool:
    """整枚标记。先过一次外接框：不在框里的点不必去试每一只图元。"""
    bx, by, bx1, by1 = MARK_BBOX
    if px < bx or px > bx1 or py < by or py > by1:
        return False
    return any(shape_contains(shape, px, py) for shape in SHAPES)


def shade(px: float, py: float) -> tuple:
    """底色：左上亮、右下暗的一段同色相渐变。"""
    t = min(1.0, max(0.0, (px + py) / (2.0 * CANVAS)))
    return tuple(
        round(SHADE_LIGHT[i] + (SHADE_DARK[i] - SHADE_LIGHT[i]) * t) for i in range(3)
    )


def paint(size: int, rounded: bool, mono: bool) -> bytes:
    """扫出一张 size×size 的 RGBA。

    `rounded` 为真时底色是圆角方块（普通图标自带圆角）；为假时铺满整块画布
    （遮罩版与 iOS 版交给系统去裁）。`mono` 为真时只画图形，底全透明。
    """
    scale = size / CANVAS
    bg = (0.0, 0.0, CANVAS, CANVAS, CORNER if rounded else 0.0)
    step = 1.0 / SUBSAMPLES
    offset = step / 2
    total = SUBSAMPLES * SUBSAMPLES
    rows = []
    for row in range(size):
        line = bytearray()
        for col in range(size):
            bg_hits = 0
            fg_hits = 0
            for sy in range(SUBSAMPLES):
                py = (row + offset + sy * step) / scale
                for sx in range(SUBSAMPLES):
                    px = (col + offset + sx * step) / scale
                    if not mono and not in_round_rect(px, py, bg):
                        continue
                    bg_hits += 1
                    if in_mark(px, py):
                        fg_hits += 1
            if mono:
                # 单色层：只有图形，颜色恒为白，透明的地方一点不着色。
                line += bytes((*ON_SHADE, round(255 * (fg_hits / total))))
                continue
            if not bg_hits:
                line += bytes(4)
                continue
            alpha = bg_hits / total
            white = (fg_hits / total) / alpha
            base = shade((col + 0.5) / scale, (row + 0.5) / scale)
            color = tuple(
                round(base[i] + (ON_SHADE[i] - base[i]) * white) for i in range(3)
            )
            line += bytes((*color, round(alpha * 255)))
        rows.append(bytes(line))
    return png(size, rows)


def png(size: int, rows: list) -> bytes:
    raw = b"".join(b"\x00" + row for row in rows)

    def chunk(tag: bytes, data: bytes) -> bytes:
        payload = tag + data
        return struct.pack(">I", len(data)) + payload + struct.pack(">I", zlib.crc32(payload))

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def ink_radius() -> float:
    """标记上离画布中心最远的那一处。给安全检查用：它必须小于 SAFE / 2。"""
    far = 0.0
    for shape in SHAPES:
        cx, cy = shape[1], shape[2]
        reach = shape[3]
        far = max(far, ((cx - CENTER) ** 2 + (cy - CENTER) ** 2) ** 0.5 + reach)
    return far


def mark_svg(indent: str) -> str:
    parts = []
    for shape in SHAPES:
        if shape[0] == "ring":
            _, cx, cy, outer, inner = shape
            parts.append(
                f'{indent}<circle cx="{cx:g}" cy="{cy:g}" r="{(outer + inner) / 2:g}" '
                f'fill="none" stroke="{ON_SHADE_HEX}" stroke-width="{outer - inner:g}"/>'
            )
        else:
            _, cx, cy, r = shape
            parts.append(
                f'{indent}<circle cx="{cx:g}" cy="{cy:g}" r="{r:g}" fill="{ON_SHADE_HEX}"/>'
            )
    return "\n".join(parts)


SYMBOLS = {}


def write_svg() -> None:
    light = "#%02x%02x%02x" % SHADE_LIGHT
    dark = "#%02x%02x%02x" % SHADE_DARK
    svg = f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {CANVAS:g} {CANVAS:g}" role="img" aria-label="{SYMBOLS['name']}">
  <title>{SYMBOLS['name']}</title>
  <defs>
    <linearGradient id="face" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="{light}"/>
      <stop offset="1" stop-color="{dark}"/>
    </linearGradient>
  </defs>
  <rect width="{CANVAS:g}" height="{CANVAS:g}" rx="{CORNER:.2f}" fill="url(#face)"/>
{mark_svg("  ")}
</svg>
"""
    target = ROOT / "res/console/icon.svg"
    target.write_text(svg, encoding="utf-8")
    print(f"已写入 {target.relative_to(ROOT)}")

    mono = f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {CANVAS:g} {CANVAS:g}" role="img" aria-label="{SYMBOLS['name']}">
  <title>{SYMBOLS['name']}</title>
{mark_svg("  ")}
</svg>
"""
    target = ROOT / "res/console/icon-monochrome.svg"
    target.write_text(mono, encoding="utf-8")
    print(f"已写入 {target.relative_to(ROOT)}")


PNGS = [
    ("icon-192.png", 192, True, False),
    ("icon-512.png", 512, True, False),
    ("icon-maskable-512.png", 512, False, False),
    ("icon-monochrome-512.png", 512, False, True),
    ("apple-touch-icon.png", 180, False, False),
]


def write_pngs() -> None:
    for name, size, rounded, mono in PNGS:
        target = ROOT / "res/console" / name
        target.write_bytes(paint(size, rounded, mono))
        print(f"已写入 {target.relative_to(ROOT)}（{size}×{size}，{target.stat().st_size} 字节）")


def main() -> None:
    # 应用名与图标的字面只在 `src/plugins/console/assets.rs` 那一处是权威；
    # 这里只为生成物上的 aria-label 与 <title> 取一次，改名前先改那一处。
    name = "知微"
    SYMBOLS["name"] = name
    radius = ink_radius()
    assert radius < SAFE / 2, (
        f"标记超出了安全区：最远的一处离中心 {radius:.1f}，"
        f"安全圆半径是 {SAFE / 2:.1f}——把环收小一档"
    )
    print(
        f"标记外缘 {radius:.1f}（可见区半径 {VISIBLE / 2:.1f}，安全区半径 {SAFE / 2:.1f}）；"
        f"环 {RING_OUTER:g}/{RING_STROKE:g}，点 {DOT_RADIUS:g}"
    )
    write_svg()
    write_pngs()


if __name__ == "__main__":
    main()
