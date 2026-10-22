/* 控制台的「外观」引擎：种子色 → 一整套 M3 系统角色，与 make-tokens.py 同一套算法。
 *
 * 只在两处用到：外观页（实时预览、从图片取色）与「同步到的主题和本机缓存对不上」时重算。
 * 平时首帧走 index.html 里那段小脚本：直接把上次算好的 CSS 从 localStorage 贴回去，不加载本文件。
 *
 * 构建：scripts/make-theme.sh（esbuild 打成 res/console/theme.js，挂到 window.ZwTheme）。
 * 依赖：@material/material-color-utilities@0.4.0（Apache-2.0），2025 版规范。 */

import {
  Blend,
  Hct,
  MaterialDynamicColors,
  QuantizerCelebi,
  SchemeContent,
  SchemeExpressive,
  SchemeFidelity,
  SchemeMonochrome,
  SchemeNeutral,
  SchemeTonalSpot,
  SchemeVibrant,
  Score,
  TonalPalette,
  argbFromHex,
  hexFromArgb,
} from "@material/material-color-utilities";

const SCHEMES = {
  fidelity: SchemeFidelity,
  tonal: SchemeTonalSpot,
  vibrant: SchemeVibrant,
  expressive: SchemeExpressive,
  content: SchemeContent,
  neutral: SchemeNeutral,
  monochrome: SchemeMonochrome,
};

/* 与 make-tokens.py 同一份：默认种子与两支自定义语义色。 */
const DEFAULT_SEED = "#3f51b5";
const SUCCESS = 0xff2e8b57;
const WARNING = 0xffb8860b;
const ACCENT = 0xffffc65c;
/* 深色表面族在标准对比度下抬离近黑（make-tokens.py 的 DARK_SURFACES）。 */
const DARK_SURFACES = {
  surface: 6, surfaceDim: 6, surfaceBright: 24,
  surfaceContainerLowest: 4, surfaceContainerLow: 10, surfaceContainer: 12,
  surfaceContainerHigh: 17, surfaceContainerHighest: 22,
};
const CONTRAST = { standard: 0, medium: 0.5, high: 1 };

const ROLES = [
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
];
const CUSTOM = [
  ["primary", ""], ["onPrimary", "on-"], ["primaryContainer", "-container"], ["onPrimaryContainer", "on--container"],
];

const colors = new MaterialDynamicColors();
const kebab = (name) => name.replace(/[A-Z]/g, (c) => "-" + c.toLowerCase());
const hex = (argb) => hexFromArgb(argb);

function normalize(prefs = {}) {
  const seed = /^#[0-9a-f]{6}$/i.test(prefs.seed || "") ? prefs.seed.toLowerCase() : DEFAULT_SEED;
  const variant = SCHEMES[prefs.variant] ? prefs.variant : "tonal";
  const mode = ["light", "dark"].includes(prefs.mode) ? prefs.mode : "system";
  const contrast = CONTRAST[prefs.contrast] !== undefined ? prefs.contrast : "system";
  return { seed, variant, mode, contrast };
}

function scheme(seed, variant, dark, contrast) {
  return new SCHEMES[variant](Hct.fromInt(seed), dark, contrast, "2025");
}

/** 界面专属的几支：运行卡、日志台、光点与选中行文字。与 make-tokens.py 的 interface() 同一张表。 */
function interface_(base, dark, contrast, sys) {
  const p = base.primaryPalette;
  const n = base.neutralPalette;
  const lift = TonalPalette.fromHueAndChroma((p.hue + 18) % 360, p.chroma * 1.15);
  const amber = Hct.fromInt(ACCENT);
  const glow = TonalPalette.fromHueAndChroma(amber.hue, amber.chroma);
  const hc = contrast >= 0.5;
  const t = dark
    ? { "hero-a": [p, 22], "hero-b": [lift, 30], "hero-fg": [p, 95], "hero-dim": [p, hc ? 92 : 82],
        "on-hero-fill": [p, 86], "on-hero-ink": [p, 20], accent: [glow, 80],
        "log-bg": [n, 10], "log-fg": [n, hc ? 98 : 88], "log-dim": [n, hc ? 85 : 70], "log-line": [n, 22] }
    : { "hero-a": [p, 92], "hero-b": [lift, 86], "hero-fg": [p, 10], "hero-dim": [p, hc ? 20 : 30],
        "on-hero-fill": [p, 40], "on-hero-ink": [p, 100], accent: [glow, 62],
        "log-bg": [n, 95], "log-fg": [n, hc ? 6 : 15], "log-dim": [n, hc ? 25 : 38], "log-line": [n, 88] };
  const out = {};
  for (const [key, [palette, tone]] of Object.entries(t)) out[key] = hex(palette.tone(tone));
  out["selected-fg"] = sys["on-primary-container"];
  return out;
}

/** 一套角色：系统色、成功 / 警告、以及界面专属的几支。 */
function roles(prefs, dark, contrast) {
  const seed = argbFromHex(prefs.seed);
  const base = scheme(seed, prefs.variant, dark, contrast);
  const out = {};
  for (const role of ROLES) out[kebab(role)] = hex(colors[role]().getArgb(base));
  if (dark && contrast === 0) {
    for (const [role, tone] of Object.entries(DARK_SURFACES)) out[kebab(role)] = hex(base.neutralPalette.tone(tone));
  }
  for (const [name, raw] of [["success", SUCCESS], ["warning", WARNING]]) {
    const custom = scheme(Blend.harmonize(raw, seed), prefs.variant, dark, contrast);
    for (const [role, shape] of CUSTOM) {
      const key = shape === "" ? name : shape === "on-" ? `on-${name}` : shape === "-container" ? `${name}-container` : `on-${name}-container`;
      out[key] = hex(colors[role]().getArgb(custom));
    }
  }
  return { sys: out, zw: interface_(base, dark, contrast, out) };
}

function block(selector, prefs, dark, contrast, scheme) {
  const { sys, zw } = roles(prefs, dark, contrast);
  const lines = [`${selector} {`];
  if (scheme) lines.push(`  color-scheme: ${dark ? "dark" : "light"};`);
  for (const [key, value] of Object.entries(sys)) lines.push(`  --md-sys-color-${key}: ${value};`);
  for (const [key, value] of Object.entries(zw)) lines.push(`  --zw-${key}: ${value};`);
  if (contrast >= 0.5) {
    lines.push("  --zw-focus-width: 4px;", "  --zw-border-width: 2px;", "  --zw-hairline: var(--md-sys-color-outline);");
  }
  lines.push("}");
  return lines.join("\n");
}

const wrap = (query, text) => `@media ${query} {\n${text.replace(/^/gm, "  ")}\n}`;

/** 整张覆盖样式表。选择器写成 `:root:root`：比 tokens.css 的 `:root` 高一档，媒体查询里的也压得住。 */
export function build(input) {
  const prefs = normalize(input);
  const S = ":root:root";
  const fixed = prefs.contrast === "system" ? null : CONTRAST[prefs.contrast];
  const parts = [];
  const pair = (dark, forced) => {
    const scheme = forced || prefs.mode === "system";
    if (fixed !== null) return [block(S, prefs, dark, fixed, scheme)];
    return [block(S, prefs, dark, 0, scheme), wrap("(prefers-contrast: more)", block(S, prefs, dark, 0.5, false))];
  };
  if (prefs.mode === "system") {
    parts.push(...pair(false, false));
    parts.push(...pair(true, false).map((text) => wrap("(prefers-color-scheme: dark)", text)));
  } else {
    parts.push(...pair(prefs.mode === "dark", true));
  }
  const surface = (dark) => roles(prefs, dark, fixed ?? 0).sys.surface;
  return { prefs, css: parts.join("\n"), surface: { light: surface(false), dark: surface(true) } };
}

/** 外观页上每个方案的小样：浅色下的主色、次色、第三色与容器底。 */
export function sample(seedHex, variant, dark = false) {
  const prefs = normalize({ seed: seedHex, variant });
  const s = scheme(argbFromHex(prefs.seed), prefs.variant, dark, 0);
  const pick = (role) => hex(colors[role]().getArgb(s));
  return {
    primary: pick("primary"),
    secondary: pick("secondaryContainer"),
    tertiary: pick("tertiary"),
    surface: pick("surfaceContainer"),
  };
}

/** 从一张图里取种子：Celebi 量化 + Score 打分，与安卓的壁纸取色同一套。像素是 RGBA 字节。 */
export function fromImage(rgba, count = 4) {
  const pixels = [];
  for (let i = 0; i < rgba.length; i += 4) {
    if (rgba[i + 3] < 255) continue;
    pixels.push((0xff << 24) | (rgba[i] << 16) | (rgba[i + 1] << 8) | rgba[i + 2]);
  }
  if (!pixels.length) return [];
  const counts = QuantizerCelebi.quantize(pixels, 128);
  return Score.score(counts, { desired: count, filter: true }).map((argb) => hex(argb));
}

export { normalize, DEFAULT_SEED };
