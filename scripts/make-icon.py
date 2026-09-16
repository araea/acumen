#!/usr/bin/env python3
"""知言那个标记：一处几何，一组产物。

标记是一个几何化的「言」字——最上面一点，三横，底下一只口。选它是因为这个字
本身就是「说出来的话」，而控制台做的正是把一屋子机器读出来的东西摆给人看。

坐标只写在这里一遍，产物都是它的输出：

    res/console/icon.svg              网页图标（标签页、清单里的矢量那一项）
    res/console/icon-192.png          装到桌面用的小图（Android）
    res/console/icon-512.png          装到桌面用的大图（Android、桌面浏览器）
    res/console/icon-maskable-512.png 交给系统裁形状的那一张：底色铺满整个画布
    res/console/apple-touch-icon.png  iOS 加到主屏幕用的那一张（180，不透明）

跑法：python3 scripts/make-icon.py（无第三方依赖，只在改图标时跑一次）。
位图是自己扫出来的：每个像素取 4×4 个子样算覆盖率，512 那一张约十几秒。

为什么不用 Pillow 或浏览器截图：这个仓库的脚本不引第三方依赖，也不该为了几张
图标去起一个 Chromium。
"""

import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# 画布 108，笔画收在中央 72×72 里，缩到 24 也不会糊成一团。
SIZE = 108
CORNER = 26.0

# 主色取自 res/cards/m3e.css 的 --md-sys-color-primary（控制台那套方案，松绿）。
# 图标是脸面，跟主题走会变成两张脸，所以这里钉死一个值。
PRIMARY = (0x1F, 0x63, 0x50)
ON_PRIMARY = (0xFF, 0xFF, 0xFF)

# 「言」的六笔：四条实心横（含最上面那一点）加一只有笔画的口。
# 数值按「上宽下窄」的楷体比例排：顶横最宽，两中横收进一档，
# 底下的口比中横略宽，向外撑住整块。
STROKES = [
    # (x, y, 宽, 高, 圆角) —— 亠上那一点
    (50.5, 24.5, 7.0, 7.0, 2.2),
    # 亠的长横
    (24.0, 38.0, 60.0, 5.0, 2.5),
    # 两中横
    (32.0, 52.0, 44.0, 5.0, 2.5),
    (32.0, 62.0, 44.0, 5.0, 2.5),
]

# 底下的「口」：外框与内框各一只圆角矩形，用描边画。
MOUTH = (30.5, 72.5, 47.0, 16.0, 3.4)
MOUTH_BORDER = 5.0

# 左上角那层微光：给平面一点纵深，且只在一角。
GLOW_CENTER = (0.30 * SIZE, 0.20 * SIZE)
GLOW_RADIUS = 0.90 * SIZE
GLOW_ALPHA = 0.16

SUBSAMPLES = 4


def in_round_rect(px: float, py: float, box) -> bool:
    """点在不在这只圆角矩形里（圆角按四角各一个圆算）。"""
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


def mouth_ring() -> tuple:
    """口那一圈的里外两只矩形。描边宽 b 沿着中线两边各铺 b/2。"""
    x, y, w, h, r = MOUTH
    b = MOUTH_BORDER
    return (x, y, w, h, r), (x + b, y + b, w - 2 * b, h - 2 * b, max(0.0, r - b / 2))


MOUTH_OUTER, MOUTH_INNER = mouth_ring()
SOLIDS = STROKES


def paint(size: int, rounded: bool) -> bytes:
    """扫出一张 size×size 的 RGBA。rounded 为假时底色铺满整块（遮罩与 iOS 用）。"""
    scale = size / SIZE
    bg = (0.0, 0.0, float(SIZE), float(SIZE), CORNER if rounded else 0.0)
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
                    if not in_round_rect(px, py, bg):
                        continue
                    bg_hits += 1
                    if any(in_round_rect(px, py, shape) for shape in SOLIDS):
                        fg_hits += 1
                    elif in_round_rect(px, py, MOUTH_OUTER) and not in_round_rect(
                        px, py, MOUTH_INNER
                    ):
                        fg_hits += 1
            if not bg_hits:
                line += bytes(4)
                continue
            alpha = bg_hits / total
            # 先铺底色，再按笔画覆盖率把白盖上去
            white = (fg_hits / total) / alpha
            # 左上角那层微光也是白，叠在底色上、笔画下
            gx = (col + 0.5) / scale - GLOW_CENTER[0]
            gy = (row + 0.5) / scale - GLOW_CENTER[1]
            t = min(1.0, (gx * gx + gy * gy) ** 0.5 / GLOW_RADIUS)
            white += GLOW_ALPHA * (1.0 - t) * (1.0 - white)
            color = tuple(
                round(PRIMARY[i] + (ON_PRIMARY[i] - PRIMARY[i]) * white) for i in range(3)
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


def write_svg() -> None:
    body = "\n".join(
        f'    <rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" fill="{ON_PRIMARY}"/>'
        for x, y, w, h, r in STROKES
    )
    x, y, w, h, r = MOUTH
    b = MOUTH_BORDER
    mouth = (
        f'    <rect x="{x + b / 2}" y="{y + b / 2}" width="{w - b}" height="{h - b}" '
        f'rx="{r - b / 4}" fill="none" stroke="{ON_PRIMARY}" stroke-width="{b}"/>'
    )
    primary = "#%02x%02x%02x" % PRIMARY
    on_primary = "#%02x%02x%02x" % ON_PRIMARY
    svg = f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {SIZE} {SIZE}" role="img" aria-label="知言">
  <title>知言</title>
  <defs>
    <radialGradient id="glow" cx="0.3" cy="0.2" r="0.9">
      <stop offset="0" stop-color="{on_primary}" stop-opacity="{GLOW_ALPHA}"/>
      <stop offset="1" stop-color="{on_primary}" stop-opacity="0"/>
    </radialGradient>
  </defs>
  <rect width="{SIZE}" height="{SIZE}" rx="{CORNER:g}" fill="{primary}"/>
  <rect width="{SIZE}" height="{SIZE}" rx="{CORNER:g}" fill="url(#glow)"/>
{body}
{mouth}
</svg>
"""
    target = ROOT / "res/console/icon.svg"
    target.write_text(svg, encoding="utf-8")
    print(f"已写入 {target.relative_to(ROOT)}")


PNGS = [
    ("icon-192.png", 192, True),
    ("icon-512.png", 512, True),
    ("icon-maskable-512.png", 512, False),
    ("apple-touch-icon.png", 180, False),
]


def write_pngs() -> None:
    for name, size, rounded in PNGS:
        target = ROOT / "res/console" / name
        target.write_bytes(paint(size, rounded))
        print(f"已写入 {target.relative_to(ROOT)}（{size}×{size}，{target.stat().st_size} 字节）")


if __name__ == "__main__":
    write_svg()
    write_pngs()
