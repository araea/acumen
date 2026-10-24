#!/usr/bin/env python3
"""生成控制台的配色令牌：`res/console/tokens.css` 里 @generated 之间的那一段。

颜色不手调。种子色经 Material Color Utilities 的 HCT 算法（2025 版规范，
Fidelity 变体：保留种子色本身的色相与彩度，主色更贴近品牌色）
推出全部系统角色，浅色、深色各一套，另各加一套「提高对比度」
（contrast_level 0.5，供 `prefers-contrast: more`，即 Apple 的「增强对比度」
与 Windows 的对比度主题之外的浏览器偏好）。

在算法之上只做两处为阅读舒适的取舍（`scripts/theme/engine.mjs` 逐条同步）：
- 深色的表面族不用 2025 规范的近黑（页面 T4、最低一层纯黑）：纯黑底上的浅字会
  在 OLED 上拖影、在眼里起光晕。标准对比度下改取中性调色板 T4–T24 这一档
  （页面 T6，与 M3 基线深色相同）；增强对比度时照算法来。
- 运行卡、日志台、选中行文字这几支界面专属色（`--zw-*`）也从同一个方案的调色板里取，
  随明暗走：浅色里是一张淡色调卡片，深色里才是深底，不再是一块压在纸面上的黑。

M3 只有 error 一种语义色；日志与连接状态还要「成功」「警告」。这两支按 M3 的
自定义色做法：先向种子色相做 harmonize，再用同一套方案取
primary / on-primary / primary-container / on-primary-container 四个角色，
于是它们与主色在明度结构上完全同构，对比度也由算法保证。

同一次运行还把 theme-color 同步写进 index.html 与 manifest，避免两处漂移。

卡片的 `res/cards/tokens.css` 自 2026-10-19 起不在这里生成：界面的字阶与卡片出图
两侧已经分家，卡片要按群聊缩略图的 20px 根字号单独维护。这里只继续生成
`src/render/tokens.rs`（统计图与词云的角色色）。

依赖：pip install materialyoucolor
运行：python3 scripts/make-tokens.py [--check]
"""

import json
import re
import sys
from pathlib import Path

from materialyoucolor.blend import Blend
from materialyoucolor.dynamiccolor.material_dynamic_colors import MaterialDynamicColors as M
from materialyoucolor.hct.hct import Hct
from materialyoucolor.palettes.tonal_palette import TonalPalette
from materialyoucolor.scheme.scheme_fidelity import SchemeFidelity

ROOT = Path(__file__).resolve().parent.parent
TOKENS = ROOT / "res/console/tokens.css"
INDEX = ROOT / "res/console/index.html"
MANIFEST = ROOT / "res/console/manifest.webmanifest"

# 种子：靛青。知微是「看见细处」，取书写与夜读的颜色，而不是告警式的高饱和蓝。
SEED = 0xFF3F51B5
# 自定义语义色的原始色相，harmonize 之后才用。
SUCCESS = 0xFF2E8B57
WARNING = 0xFFB8860B
# 运行卡上那粒「在运行」的暖色光点。
ACCENT = 0xFFFFC65C
# 深色表面族在标准对比度下的取值（中性调色板的色调档）。
DARK_SURFACES = {
    "surface": 6, "surfaceDim": 6, "surfaceBright": 24,
    "surfaceContainerLowest": 4, "surfaceContainerLow": 10, "surfaceContainer": 12,
    "surfaceContainerHigh": 17, "surfaceContainerHighest": 22,
}

ROLES = [
    "primary", "onPrimary", "primaryContainer", "onPrimaryContainer",
    "secondary", "onSecondary", "secondaryContainer", "onSecondaryContainer",
    "tertiary", "onTertiary", "tertiaryContainer", "onTertiaryContainer",
    "error", "onError", "errorContainer", "onErrorContainer",
    "surface", "surfaceDim", "surfaceBright",
    "surfaceContainerLowest", "surfaceContainerLow", "surfaceContainer",
    "surfaceContainerHigh", "surfaceContainerHighest",
    "onSurface", "onSurfaceVariant", "outline", "outlineVariant",
    "inverseSurface", "inverseOnSurface", "inversePrimary",
    "scrim", "shadow",
]


def kebab(name: str) -> str:
    return re.sub(r"([A-Z])", lambda m: "-" + m.group(1).lower(), name)


def hexs(argb: int) -> str:
    return "#%06x" % (argb & 0xFFFFFF)


def scheme(source: int, dark: bool, contrast: float):
    return SchemeFidelity(Hct.from_int(source), dark, contrast, spec_version="2025")


def interface(base, dark: bool, contrast: float, sys: dict) -> dict:
    """界面专属的几支颜色：运行卡、日志台、光点与选中行文字。与 engine.mjs 的 interface() 同一张表。"""
    p, n = base.primary_palette, base.neutral_palette
    lift = TonalPalette.from_hue_and_chroma((p.hue + 18) % 360, p.chroma * 1.15)
    accent = Hct.from_int(ACCENT)
    glow = TonalPalette.from_hue_and_chroma(accent.hue, accent.chroma)
    hc = contrast >= 0.5
    if dark:
        tones = dict(hero_a=(p, 22), hero_b=(lift, 30), hero_fg=(p, 95), hero_dim=(p, 92 if hc else 82),
                     fill=(p, 86), ink=(p, 20), accent=(glow, 80),
                     log_bg=(n, 10), log_fg=(n, 98 if hc else 88), log_dim=(n, 85 if hc else 70), log_line=(n, 22))
    else:
        tones = dict(hero_a=(p, 92), hero_b=(lift, 86), hero_fg=(p, 10), hero_dim=(p, 20 if hc else 30),
                     fill=(p, 40), ink=(p, 100), accent=(glow, 62),
                     log_bg=(n, 95), log_fg=(n, 6 if hc else 15), log_dim=(n, 25 if hc else 38), log_line=(n, 88))
    pick = {key: hexs(pal.tone(tone)) for key, (pal, tone) in tones.items()}
    return {
        "hero-a": pick["hero_a"], "hero-b": pick["hero_b"], "hero-fg": pick["hero_fg"], "hero-dim": pick["hero_dim"],
        "on-hero-fill": pick["fill"], "on-hero-ink": pick["ink"], "accent": pick["accent"],
        "log-bg": pick["log_bg"], "log-fg": pick["log_fg"], "log-dim": pick["log_dim"], "log-line": pick["log_line"],
        "selected-fg": sys["on-primary-container"],
    }


def block(dark: bool, contrast: float) -> list[str]:
    base = scheme(SEED, dark, contrast)
    sys = {kebab(role): hexs(getattr(M, role).get_argb(base)) for role in ROLES}
    if dark and contrast == 0:
        for role, tone in DARK_SURFACES.items():
            sys[kebab(role)] = hexs(base.neutral_palette.tone(tone))
    lines = [f"  --md-sys-color-{key}: {value};" for key, value in sys.items()]
    for name, raw in (("success", SUCCESS), ("warning", WARNING)):
        custom = scheme(Blend.harmonize(raw, SEED), dark, contrast)
        for role, suffix in (
            ("primary", ""),
            ("onPrimary", "on-"),
            ("primaryContainer", "container"),
            ("onPrimaryContainer", "on-container"),
        ):
            key = {"": name, "on-": f"on-{name}", "container": f"{name}-container",
                   "on-container": f"on-{name}-container"}[suffix]
            lines.append(f"  --md-sys-color-{key}: {hexs(getattr(M, role).get_argb(custom))};")
    lines += [f"  --zw-{key}: {value};" for key, value in interface(base, dark, contrast, sys).items()]
    return lines


def generated() -> tuple[str, dict]:
    light, dark = block(False, 0.0), block(True, 0.0)
    light_hc, dark_hc = block(False, 0.5), block(True, 0.5)
    out = [
        "/* @generated by scripts/make-tokens.py — 不要手改这一段 */",
        f"/* seed {hexs(SEED)} · Fidelity · spec 2025 */",
        ":root {",
        "  color-scheme: light;",
        *light,
        "}",
        "@media (prefers-color-scheme: dark) {",
        "  :root {",
        "    color-scheme: dark;",
        *["  " + line for line in dark],
        "  }",
        "}",
        "@media (prefers-contrast: more) {",
        "  :root {",
        *["  " + line for line in light_hc],
        "  }",
        "}",
        "@media (prefers-contrast: more) and (prefers-color-scheme: dark) {",
        "  :root {",
        *["  " + line for line in dark_hc],
        "  }",
        "}",
        "/* @end generated */",
    ]
    surface = lambda lines: re.search(r"--md-sys-color-surface: (#[0-9a-f]{6})", "\n".join(lines)).group(1)
    return "\n".join(out), {"light": surface(light), "dark": surface(dark)}


def native_tokens() -> str:
    values = dict(re.findall(r"--md-sys-color-([a-z-]+): (#[0-9a-f]+);", "\n".join(block(False, 0.0))))
    out = ["// @generated by scripts/make-tokens.py; shared M3E light scheme.", "#![allow(dead_code)]", "use plotters::style::RGBColor;"]
    for name, value in values.items():
        key = name.upper().replace("-", "_")
        rgb = [int(value[i:i+2], 16) for i in (1,3,5)]
        out += [f'pub const {key}_HEX: &str = "{value}";', f'pub const {key}: RGBColor = RGBColor({rgb[0]}, {rgb[1]}, {rgb[2]});']
    out += ["pub const HUES: [RGBColor; 5] = [PRIMARY, SECONDARY, TERTIARY, SUCCESS, WARNING];",
            "pub const WORD_COLORS: [&str; 5] = [", "    PRIMARY_HEX,", "    SECONDARY_HEX,", "    TERTIARY_HEX,", "    SUCCESS_HEX,", "    WARNING_HEX,", "];"]
    return "\n".join(out) + "\n"


def main() -> int:
    check = "--check" in sys.argv
    text, surfaces = generated()
    tokens = TOKENS.read_text()
    new_tokens = re.sub(
        r"/\* @generated by scripts/make-tokens\.py.*?/\* @end generated \*/",
        lambda _: text,
        tokens,
        flags=re.S,
    )
    index = INDEX.read_text()
    new_index = re.sub(
        r'(<meta name="theme-color" content=")#[0-9a-f]{6}(" media="\(prefers-color-scheme: (light|dark)\)">)',
        lambda m: m.group(1) + surfaces[m.group(3)] + m.group(2),
        index,
    )
    manifest = json.loads(MANIFEST.read_text())
    manifest["theme_color"] = surfaces["light"]
    manifest["background_color"] = surfaces["light"]
    new_manifest = json.dumps(manifest, ensure_ascii=False, indent=2) + "\n"

    native = ROOT / "src/render/tokens.rs"
    generated_files = [(native, native_tokens())]
    stale = [
        path.name
        for path, old, new in (
            (TOKENS, tokens, new_tokens),
            (INDEX, index, new_index),
            (MANIFEST, MANIFEST.read_text(), new_manifest),
        )
        if old != new
    ]
    stale += [str(path.relative_to(ROOT)) for path, new in generated_files if not path.exists() or path.read_text() != new]
    if check:
        if stale:
            print("过期：" + "、".join(stale) + "；运行 python3 scripts/make-tokens.py")
            return 1
        print("配色令牌与种子一致。")
        return 0
    for path, new in generated_files:
        path.write_text(new)
    TOKENS.write_text(new_tokens)
    INDEX.write_text(new_index)
    MANIFEST.write_text(new_manifest)
    print("已写入：" + ("、".join(stale) if stale else "无变化"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
