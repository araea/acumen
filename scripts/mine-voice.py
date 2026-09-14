#!/usr/bin/env python3
"""从本机记录里捞「号主本人手打的短句」，给 res/ambient/voice.md 补充候选。

搭话人格的调子由 `res/ambient/voice.md` 里的实物样本定住，样本越多越准。
号主在别的群里说的话也会沉淀进 `data/bot.db`，隔一阵子重新捞一遍，人工过一遍，
挑好的加进样本库——这就是「继续内化他本人的说法」这件事的全部操作。

用法::

    # 按场景打印候选（默认最近 30 天）
    python scripts/mine-voice.py --uid <号主 QQ>

    # 换个时间窗、放宽长度
    python scripts/mine-voice.py --uid <号主 QQ> --days 7 --max-chars 60

    # 核对现有样本库：每一条都必须在记录里找得到原文
    python scripts/mine-voice.py --uid <号主 QQ> --verify

记录里 `role='self'` 的是机器人自己发的，其余是这个号后面那个人手打的——
两者共用同一个 QQ 号，只能靠 role 分。

**加进样本库的每一句都必须是记录里的原文。** 自己润色过的句子会把口吻带偏，
`--verify` 就是拦这件事的。
"""

from __future__ import annotations

import argparse
import os
import re
import sqlite3
import sys
import unicodedata

# 这些开头的行是指令或贴进来的长提示词，不是闲聊。
COMMAND = re.compile(r"^\s*(/|#|\.|mj|ciyi|～gi2|～gggd|dsr|dsapp|dsc|ds|oai)")
# 消息元素占位符：留下来的话会把样本库污染成 [图片][表情]。
PLACEHOLDER = re.compile(r"\[(图片|视频|语音|表情|动画表情|转发|文件|音乐|合并转发|回复|@\d+)\]")

# 场景 → 关键词。和 `res/ambient/voice.md` 的分组一一对应，方便对照着挑。
BUCKETS: list[tuple[str, str]] = [
    ("通用口气", r"^(那|我|你|不|这|等|刚|先|好|行|嗯|是|有)"),
    ("嫌弃", r"(拉|顶不住|不行|坑|离谱|逆天|绷|裂开|麻|服了|绝了|废|捞|太差)"),
    ("夸与安利", r"(推荐|好伐|顶级|给到|夯|强|牛|绝|值|香|稳|顶|好用|可以)"),
    ("问与反问", r"(吗|呢|吧|么|怎么|为什么|是不是|有没有|咋|啥|多少)"),
    ("玩梗与夸张", r"(哈|笑死|破案|实锤|包|典|乐|整活|人机|无敌|克拉斯)"),
    ("损人开玩笑", r"(笨蛋|你小子|坏猫|傻|滚|闭嘴|别闹|整活)"),
    ("玩机与数码", r"(手机|系统|内存|运存|刷|root|模型|客户端|屏幕|电池|芯片|折叠|鸿蒙|小米|华为|oppo|vivo|苹果)"),
    ("生活日常", r"(上班|吃|睡|困|网吧|放假|游戏|买|加班|回家|放假)"),
]


def clean(raw: str) -> str:
    return PLACEHOLDER.sub("", raw).strip()


def width(text: str) -> int:
    """按显示宽度算长度，中英混排时不至于把长句判成短句。"""
    return sum(2 if unicodedata.east_asian_width(c) in "WF" else 1 for c in text)


def load(db: str, uid: str, days: int) -> list[str]:
    if not os.path.exists(db):
        sys.exit(f"找不到记录库：{db}（在 bot 的工作目录下跑，或用 --db 指定）")
    what = "select content_rich from message_records where user_id=? and role != 'self'"
    args: list[object] = [uid]
    if days > 0:
        what += " and time >= strftime('%s','now') - ?"
        args.append(days * 86_400)
    what += " order by time"
    con = sqlite3.connect(db)
    return [(row[0] or "") for row in con.execute(what, args)]


def candidates(rows: list[str], limit: int, max_chars: int) -> dict[str, list[str]]:
    out: dict[str, list[str]] = {}
    for raw in rows:
        if COMMAND.match(raw):
            continue
        text = clean(raw)
        if not text or width(text) > max_chars:
            continue
        for name, pattern in BUCKETS:
            if re.search(pattern, text, re.I):
                bucket = out.setdefault(name, [])
                if text not in bucket and len(bucket) < limit:
                    bucket.append(text)
                break
    return out


def verify(rows: list[str], path: str) -> int:
    samples = [
        line[2:].strip()
        for line in open(path, encoding="utf-8")
        if line.startswith("- ")
    ]
    pool = [clean(row) for row in rows]
    missing = [s for s in samples if not any(s in row for row in pool)]
    print(f"样本 {len(samples)} 条")
    if missing:
        print("下面这些在记录里找不到原文，要么是润色过的，要么已经不在时间窗里：")
        for text in missing:
            print(f"  {text}")
        return 1
    print("全部命中原文")
    return 0


def main() -> int:
    here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    parser = argparse.ArgumentParser(description="捞号主本人手打的短句")
    parser.add_argument("--uid", required=True, help="号主的 QQ 号")
    parser.add_argument("--db", default=os.path.join(here, "data", "bot.db"))
    parser.add_argument("--voice", default=os.path.join(here, "res", "ambient", "voice.md"))
    parser.add_argument("--days", type=int, default=30, help="只看最近这些天，0 表示全部")
    parser.add_argument("--limit", type=int, default=40, help="每组最多打印几条")
    parser.add_argument("--max-chars", type=int, default=80)
    parser.add_argument("--verify", action="store_true", help="核对样本库里的原文")
    args = parser.parse_args()

    rows = load(args.db, args.uid, args.days)
    print(f"取到 {len(rows)} 条本人发言（最近 {args.days} 天）", file=sys.stderr)
    if args.verify:
        return verify(rows, args.voice)
    for name, items in candidates(rows, args.limit, args.max_chars).items():
        print(f"\n## {name}")
        for text in items:
            print(f"  {text}")
    print("\n挑好的粘进 res/ambient/voice.md（原样，别改字），再跑 --verify 核一遍。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
