#!/usr/bin/env python3
"""知言那个标记：一处几何，一个产物。

标记是一个几何化的「言」字——最上面一点，三横，底下一只口。选它是因为这个字
本身就是「说出来的话」，而控制台做的正是把一屋子机器读出来的东西摆给人看。

坐标只写在这里一遍，产物是它的输出：

    res/console/icon.svg    控制台的图标（favicon 与 PWA manifest 共用）

跑法：python3 scripts/make-icon.py（无第三方依赖，只在改图标时跑一次）。
"""

from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# 画布 108，笔画收在中央 72×72 里，缩到 24 也不会糊成一团。
SIZE = 108

# 主色取自 res/cards/m3e.css 的 --md-sys-color-primary（控制台那套方案，松绿）。
# 图标是脸面，跟主题走会变成两张脸，所以这里钉死一个值。
PRIMARY = "#1f6350"
ON_PRIMARY = "#ffffff"

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
    svg = f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {SIZE} {SIZE}" role="img" aria-label="知言">
  <title>知言</title>
  <defs>
    <radialGradient id="glow" cx="0.3" cy="0.2" r="0.9">
      <stop offset="0" stop-color="{ON_PRIMARY}" stop-opacity="0.16"/>
      <stop offset="1" stop-color="{ON_PRIMARY}" stop-opacity="0"/>
    </radialGradient>
  </defs>
  <rect width="{SIZE}" height="{SIZE}" rx="26" fill="{PRIMARY}"/>
  <rect width="{SIZE}" height="{SIZE}" rx="26" fill="url(#glow)"/>
{body}
{mouth}
</svg>
"""
    target = ROOT / "res/console/icon.svg"
    target.write_text(svg, encoding="utf-8")
    print(f"已写入 {target.relative_to(ROOT)}")


if __name__ == "__main__":
    write_svg()
