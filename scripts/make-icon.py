#!/usr/bin/env python3
"""知言的应用图标：一处几何，三个产物。

这个标记是一个几何化的「言」字——最上面一点，三横，底下一只口。选它是因为
这个字本身就是「说出来的话」，而这台机器人做的是在同一句话上读人与应人。

三份产物必须同源，所以只在这里写一遍坐标，其余地方都是它的输出：

    res/console/icon.svg                      网页图标（favicon 与 PWA）
    app/res/drawable/ic_launcher_foreground.xml   Android 自适应图标的前景层
    app/res/drawable/ic_launcher_background.xml   Android 自适应图标的背景层

跑法：python3 scripts/make-icon.py（无第三方依赖，只在改图标时跑一次）。
"""

from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# 画布 108，与 Android 自适应图标的前景层同一坐标系；内容收在中央 72×72
# 的安全区里（18—90），旋转与裁切都不会碰到笔画。
SIZE = 108
SAFE = 18

# 主色取自 res/cards/m3e.css 的 --md-sys-color-primary（手册方案，松绿）。
# 图标是应用的脸，跟主题走会变成两张脸，所以这里钉死一个值。
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

# 底下的「口」：外框与内框各一只圆角矩形，用 even-odd 挖空。
MOUTH = (30.5, 72.5, 47.0, 16.0, 3.4)
MOUTH_BORDER = 5.0


def rounded_rect(x: float, y: float, w: float, h: float, r: float) -> str:
    """圆角矩形的路径。SVG 与 VectorDrawable 共用同一串命令。"""
    r = min(r, w / 2, h / 2)
    return (
        f"M{x + r:.2f},{y:.2f}"
        f"H{x + w - r:.2f}"
        f"A{r:.2f},{r:.2f} 0 0 1 {x + w:.2f},{y + r:.2f}"
        f"V{y + h - r:.2f}"
        f"A{r:.2f},{r:.2f} 0 0 1 {x + w - r:.2f},{y + h:.2f}"
        f"H{x + r:.2f}"
        f"A{r:.2f},{r:.2f} 0 0 1 {x:.2f},{y + h - r:.2f}"
        f"V{y + r:.2f}"
        f"A{r:.2f},{r:.2f} 0 0 1 {x + r:.2f},{y:.2f}Z"
    )


def mouth_path() -> str:
    x, y, w, h, r = MOUTH
    b = MOUTH_BORDER
    outer = rounded_rect(x, y, w, h, r)
    inner = rounded_rect(x + b, y + b, w - 2 * b, h - 2 * b, max(r - b / 2, 0.6))
    return outer + inner


def glyph_path() -> str:
    """六笔连成一条路径：四条横 + 挖空的口。"""
    return "".join(rounded_rect(*s) for s in STROKES) + mouth_path()


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


def write_foreground() -> None:
    """前景层：只有笔画，透明底。安全区内要留出约 1/3 的余量，故整体缩到 0.8。"""
    scale = 0.8
    offset = SIZE * (1 - scale) / 2
    vector = f"""<!-- 由 scripts/make-icon.py 生成，不要手改。知言的「言」字标记。 -->
<vector xmlns:android="http://schemas.android.com/apk/res/android"
    android:width="{SIZE}dp"
    android:height="{SIZE}dp"
    android:viewportWidth="{SIZE}"
    android:viewportHeight="{SIZE}">
    <group
        android:pivotX="{SIZE / 2:.1f}"
        android:pivotY="{SIZE / 2:.1f}"
        android:scaleX="{scale}"
        android:scaleY="{scale}"
        android:translateX="{offset - SIZE * (1 - scale) / 2:.2f}"
        android:translateY="{offset - SIZE * (1 - scale) / 2:.2f}">
        <path
            android:fillColor="{ON_PRIMARY}"
            android:pathData="{glyph_path()}" />
    </group>
</vector>
"""
    target = ROOT / "app/res/drawable/ic_launcher_foreground.xml"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(vector, encoding="utf-8")
    print(f"已写入 {target.relative_to(ROOT)}")


def write_background() -> None:
    """背景层：主色底加一角微光，与卡片上 `.md-card::before` 那层是同一手法。"""
    vector = f"""<!-- 由 scripts/make-icon.py 生成，不要手改。 -->
<vector xmlns:android="http://schemas.android.com/apk/res/android"
    android:width="{SIZE}dp"
    android:height="{SIZE}dp"
    android:viewportWidth="{SIZE}"
    android:viewportHeight="{SIZE}">
    <path
        android:fillColor="{PRIMARY}"
        android:pathData="M0,0h{SIZE}v{SIZE}h-{SIZE}z" />
    <path
        android:fillAlpha="0.16"
        android:fillColor="{ON_PRIMARY}"
        android:pathData="M0,0h{SIZE}v{SIZE * 0.42}a{SIZE * 0.5},{SIZE * 0.5} 0 0 1 -{SIZE},0z" />
</vector>
"""
    target = ROOT / "app/res/drawable/ic_launcher_background.xml"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(vector, encoding="utf-8")
    print(f"已写入 {target.relative_to(ROOT)}")


def write_mark() -> None:
    """等待屏上那枚标记：主色圆角方 + 白笔画，与 res/console/icon.svg 同形。

    与前景层分开，是因为前景层是**透明底的白笔画**（给系统裁切用），
    直接摆在浅色纸上什么也看不见。
    """
    S = SIZE
    plate = rounded_rect(0, 0, S, S, 26)
    vector = f"""<!-- 由 scripts/make-icon.py 生成，不要手改。 -->
<vector xmlns:android="http://schemas.android.com/apk/res/android"
    android:width="{S}dp"
    android:height="{S}dp"
    android:viewportWidth="{S}"
    android:viewportHeight="{S}">
    <path
        android:fillColor="{PRIMARY}"
        android:pathData="{plate}" />
    <path
        android:fillColor="{ON_PRIMARY}"
        android:fillType="evenOdd"
        android:pathData="{glyph_path()}" />
</vector>
"""
    target = ROOT / "app/res/drawable/ic_mark.xml"
    target.write_text(vector, encoding="utf-8")
    print(f"已写入 {target.relative_to(ROOT)}")


def write_notification() -> None:
    """通知栏那颗小图标：只有笔画、纯白剪影，24dp。

    系统会把它当模板染色，所以这里不能有底色，也不能有多色。
    """
    vector = f"""<!-- 由 scripts/make-icon.py 生成，不要手改。 -->
<vector xmlns:android="http://schemas.android.com/apk/res/android"
    android:width="24dp"
    android:height="24dp"
    android:viewportWidth="{SIZE}"
    android:viewportHeight="{SIZE}">
    <path
        android:fillColor="#ffffff"
        android:fillType="evenOdd"
        android:pathData="{glyph_path()}" />
</vector>
"""
    target = ROOT / "app/res/drawable/ic_notification.xml"
    target.write_text(vector, encoding="utf-8")
    print(f"已写入 {target.relative_to(ROOT)}")


def write_monochrome() -> None:
    """主题图标（Android 13+）用的单色层：只有笔画，由系统决定颜色。"""
    vector = f"""<!-- 由 scripts/make-icon.py 生成，不要手改。 -->
<vector xmlns:android="http://schemas.android.com/apk/res/android"
    android:width="{SIZE}dp"
    android:height="{SIZE}dp"
    android:viewportWidth="{SIZE}"
    android:viewportHeight="{SIZE}">
    <path
        android:fillColor="#ffffff"
        android:pathData="{glyph_path()}" />
</vector>
"""
    target = ROOT / "app/res/drawable/ic_launcher_monochrome.xml"
    target.write_text(vector, encoding="utf-8")
    print(f"已写入 {target.relative_to(ROOT)}")


if __name__ == "__main__":
    write_svg()
    write_foreground()
    write_background()
    write_monochrome()
    write_notification()
    write_mark()
