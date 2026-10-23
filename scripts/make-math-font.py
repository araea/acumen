#!/usr/bin/env python3
"""重做 res/fonts/newcm-math.woff2：New Computer Modern Math Book 的子集（GUST 字体许可）。

    curl -LO https://mirrors.ctan.org/fonts/newcomputermodern/otf/NewCMMath-Book.otf
    python3 scripts/make-math-font.py NewCMMath-Book.otf ~/.cargo/registry/src/*/math-core-renderer-internal-*/src/symbol.rs

留下 math-core 能输出的全部符号，加上数学运算、箭头、定界符、字母数字这几个整块
（根号 U+221A 不在 symbol.rs 里——msqrt 由浏览器自己取这个字形，漏了就只剩一条横线）。
排版特性只留 kern：Chromium 不看 ssty，留着白多一千多个字形。MATH 表的伸缩变体
fontTools 会随字形闭包一起留下。需要 fonttools 与 brotli。
"""
import re
import subprocess
import sys

font, symbols = sys.argv[1], sys.argv[2]
source = open(symbols, encoding="utf-8").read()
chars = set(re.findall(r"^pub const [^:]+: \S+ =[^']+'(.)'[^;]*;", source, re.M))
chars |= {chr(int(code, 16)) for code in re.findall(r"^pub const [^:]+: \S+ =[^']+'\\u\{([0-9A-Fa-f]+)\}'[^;]*;", source, re.M)}
ranges = [
    (0x20, 0x7F), (0xA0, 0x100), (0x131, 0x132), (0x237, 0x238), (0x391, 0x3CA), (0x3D0, 0x3F7),
    (0x2000, 0x20A0), (0x20D0, 0x20F1), (0x2100, 0x2150), (0x2190, 0x2300), (0x2308, 0x230C),
    (0x2320, 0x2322), (0x2329, 0x232B), (0x239B, 0x23B4), (0x23DC, 0x23E2), (0x25A0, 0x2600),
    (0x27C0, 0x2800), (0x2900, 0x2B00), (0x3008, 0x300C), (0x1D400, 0x1D800),
]
points = {ord(c) for c in chars}
for start, end in ranges:
    points |= set(range(start, end))
subprocess.run(
    [
        "pyftsubset", font,
        "--unicodes=" + ",".join(f"U+{p:04X}" for p in sorted(points)),
        "--layout-features=kern", "--flavor=woff2",
        "--output-file=res/fonts/newcm-math.woff2",
    ],
    check=True,
)
