#!/usr/bin/env python3
"""生成知微的「开环 / 焦点」标记，SVG 与所有 PNG 共用几何。

108 网格：270° 开环是观察的边界，中心是被理解的细节，右上圆点是新的发现。
开环半径 25、笔画 8，圆头；中心点 7，右上焦点 5.5。16px 下笔画仍超过 1px。
图形收在半径 33 的安全圆中。any 自带圆角；maskable 与 Apple 铺满不透明背景，
由系统裁切；monochrome 只有白色前景。平台规范优先于品牌外形。

此文件编辑的是程序定义的矢量图元；PNG 为同源导出，无第三方依赖。
运行：python3 scripts/make-icon.py。
"""

import math
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
SHADE_LIGHT = (0x30, 0x70, 0x50)
SHADE_DARK = (0x16, 0x41, 0x32)

# 图形用白。写成一个十六进制串，不再往 SVG 里塞元组——旧版就是这里把
# `fill="(255, 255, 255)"` 写进了文件，浏览器按非法值处理、回落成黑色，
# 于是矢量那份是黑字而位图那份是白字，同一个产品两张脸。
ON_SHADE = (0xFF, 0xFF, 0xFF)
ON_SHADE_HEX = "#%02x%02x%02x" % ON_SHADE

# —— 标记：唯一几何源 ——
RING_OUTER = 29.0
RING_STROKE = 8.0
DOT_RADIUS = 7.0
SHAPES = [
    ("arc", CENTER, CENTER, RING_OUTER, RING_OUTER - RING_STROKE),
    ("disk", CENTER, CENTER, DOT_RADIUS),
    ("disk", 72.0, 36.0, 5.5),
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
    if shape[0] == "arc":
        _, cx, cy, outer, inner = shape
        radius, cap = (outer + inner) / 2, (outer - inner) / 2
        # 屏幕坐标顺时针：右 → 下 → 左 → 上，右上留开口。
        angle = math.atan2(py - cy, px - cx) % (2 * math.pi)
        return (angle <= 1.5 * math.pi and in_ring(px, py, cx, cy, outer, inner)
                or in_disk(px, py, cx + radius, cy, cap)
                or in_disk(px, py, cx, cy - radius, cap))
    _, cx, cy, r = shape
    return in_disk(px, py, cx, cy, r)


MARK_BBOX = (
    min(s[1] - s[3] for s in SHAPES),
    min(s[2] - s[3] for s in SHAPES),
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
        if shape[0] == "arc":
            _, cx, cy, outer, inner = shape
            r = (outer + inner) / 2
            parts.append(
                f'{indent}<path d="M{cx+r:g} {cy:g} A{r:g} {r:g} 0 1 1 {cx:g} {cy-r:g}" '
                f'fill="none" stroke="{ON_SHADE_HEX}" stroke-width="{outer-inner:g}" stroke-linecap="round"/>'
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


# 最小那一档：标签页图标。三个数的第三条判据按它算。
SMALLEST = 16.0


def check_geometry() -> tuple:
    """三条判据，改坐标时先在这里撞一次。返回打印用的那几个数。"""
    radius = ink_radius()
    assert radius < SAFE / 2, (
        f"标记超出了安全区：最远的一处离中心 {radius:.1f}，"
        f"安全圆半径是 {SAFE / 2:.1f}——把环收小一档"
    )
    stroke = RING_STROKE * SMALLEST / CANVAS
    assert stroke >= 1.0, (
        f"环宽在 {SMALLEST:g}px 上只有 {stroke:.2f} 个设备像素，比一个像素还细——"
        f"环会被抗锯齿摊灰，比里面的点还轻，视线次序就反了"
    )
    gap = RING_OUTER - RING_STROKE - DOT_RADIUS
    assert gap > 0, f"环与点叠在一起了：那道空只有 {gap:.1f}"
    assert RING_STROKE <= gap * 2 / 3, (
        f"环宽 {RING_STROKE:g} 超过了「环内缘与点之间那道空」{gap:.1f} 的三分之二——"
        f"两个图元会糊成一个；要么收环、要么收点，并保留最小尺寸的可辨识性"
    )
    return radius, stroke, gap


def main() -> None:
    # 应用名与图标的字面只在 `src/plugins/console/assets.rs` 那一处是权威；
    # 这里只为生成物上的 aria-label 与 <title> 取一次，改名前先改那一处。
    name = "知微"
    SYMBOLS["name"] = name
    radius, stroke, gap = check_geometry()
    print(
        f"标记外缘 {radius:.1f}（可见区半径 {VISIBLE / 2:.1f}，安全区半径 {SAFE / 2:.1f}）；"
        f"环 {RING_OUTER:g}/{RING_STROKE:g}，点 {DOT_RADIUS:g}；"
        f"{SMALLEST:g}px 上环宽 {stroke:.2f}px、那道空 {gap * SMALLEST / CANVAS:.2f}px"
    )
    write_svg()
    write_pngs()


if __name__ == "__main__":
    main()
