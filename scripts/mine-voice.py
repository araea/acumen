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

    # 核对样本库的「形状」：长短分布与标点习惯得跟他本人对得上
    python scripts/mine-voice.py --uid <号主 QQ> --shape

记录里 `role='self'` 的是机器人自己发的，其余是这个号后面那个人手打的——
两者共用同一个 QQ 号，只能靠 role 分。

**加进样本库的每一句都必须是记录里的原文。** 自己润色过的句子会把口吻带偏，
`--verify` 就是拦这件事的。

**样本库的长短分布也要跟他本人对得上。** 模型是照着样本的形状写字的：一屋子七八个
字的样本，它就再也写不出两个字的回话。`--shape` 把两边的分布并排打出来，差得远就去
补那一档，别靠感觉。

**已经在库里的句子不会再打印。** 隔一阵子重捞一遍时，看到的就只有这期间新出现的说法，
不必对着几百条旧句再挑一次。
"""

from __future__ import annotations

import argparse
import os
import re
import sqlite3
import sys
import unicodedata

# 这些开头的行是指令或贴进来的长提示词，不是闲聊。清理掉占位符之后还要再查一遍
# ——引用别人那条消息时，指令前面会先落一个 `[回复]`。
COMMAND = re.compile(r"^\s*(/|#|-|\.|～|~|mj|ciyi|c\s|dsr|dsapp|dsc|ds|oai)")
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


def load(db: str, uid: str, days: int) -> list[tuple[int, int, str]]:
    """按时间正序取他手打的消息：`(群号, 时刻, 正文)`。

    群号与时刻不是为了展示，是为了认出词意猜词——那些词只能靠「在哪、什么时候」
    分辨（见 [`guesses`]）。
    """
    if not os.path.exists(db):
        sys.exit(f"找不到记录库：{db}（在 bot 的工作目录下跑，或用 --db 指定）")
    what = (
        "select group_id, time, content_rich from message_records "
        "where user_id=? and role != 'self'"
    )
    args: list[object] = [uid]
    if days > 0:
        what += " and time >= strftime('%s','now') - ?"
        args.append(days * 86_400)
    what += " order by time"
    con = sqlite3.connect(db)
    return [(row[0], row[1], row[2] or "") for row in con.execute(what, args)]


# 光秃秃的两三个汉字：没有标点、没有语气词、没有字母数字。
BARE = re.compile(r"^[\u4e00-\u9fff]{2,3}$")
# 猜词成串的判定：同一个群、这么长的一段里有这么多条光秃秃的词。
GUESS_WINDOW = 90
GUESS_RUN = 3


def guesses(rows: list[tuple[int, int, str]]) -> set[int]:
    """认出词意游戏里那些猜词，返回它们在 `rows` 里的下标。

    群里玩词意的时候，他一口气打十几个两字词（「悲伤」「伤心」「伤人」「重伤」），
    每条隔几秒。这些词占了他全部短消息的一大半，混进样本库就等于教人格用名词接话。
    只看单条是分不出来的——「刚睡醒」和「悲伤」长得一样——所以看它是不是成串来的：
    同一个群 90 秒内有三条以上光秃秃的两三字词，那一串就是在猜词，不是在说话。
    """
    bare = [
        (index, group, at)
        for index, (group, at, raw) in enumerate(rows)
        if BARE.match(clean(raw))
    ]
    out: set[int] = set()
    for spot, (index, group, at) in enumerate(bare):
        run = [
            other
            for other, group_of, at_of in bare[max(0, spot - GUESS_RUN) : spot + GUESS_RUN + 1]
            if group_of == group and abs(at_of - at) <= GUESS_WINDOW
        ]
        if len(run) >= GUESS_RUN:
            out.update(run)
    return out


def chatter(rows: list[tuple[int, int, str]]) -> list[str]:
    """记录 → 他真正拿来说话的那些句子（去掉指令与词意猜词）。"""
    skip = guesses(rows)
    out = []
    for index, (_, _, raw) in enumerate(rows):
        if index in skip or COMMAND.match(raw):
            continue
        text = clean(raw)
        if text and not COMMAND.match(text):
            out.append(text)
    return out


def candidates(
    rows: list[tuple[int, int, str]], limit: int, max_chars: int, known: set[str]
) -> dict[str, list[str]]:
    """按场景分组的候选；`known` 是已经在样本库里的那些，不必再看第二遍。"""
    out: dict[str, list[str]] = {}
    for text in chatter(rows):
        if width(text) > max_chars or not speech_like(text) or text in known:
            continue
        for name, pattern in BUCKETS:
            if re.search(pattern, text, re.I):
                bucket = out.setdefault(name, [])
                if text not in bucket and len(bucket) < limit:
                    bucket.append(text)
                break
    return out


# 一个字都没有、还只有一两个拉丁词：那是发给别的 bot 的指令或答题（`alb 每日魔方`、
# `MCDLE 裸猜`、`p5letter`、`b7e7`），不是他在说话。
HAN = re.compile(r"[\u4e00-\u9fff]")
LATIN_WORD = re.compile(r"[A-Za-z]{2,}")
# 光秃秃的数字或标点（`22`、`？`、`✅`）。
BARE_MARKS = re.compile(r"^[\W_]+$|^[\d\s.]+$")


def speech_like(text: str) -> bool:
    """像不像一句人在群里说的话。粗筛，挑还是要人来挑。"""
    if "@" in text:
        return False
    if BARE_MARKS.match(text):
        return False
    return bool(HAN.search(text)) or len(LATIN_WORD.findall(text)) >= 2


def verify(rows: list[tuple[int, int, str]], path: str) -> int:
    samples = samples_of(path)
    pool = [clean(raw) for _, _, raw in rows]
    missing = [s for s in samples if not any(s in row for row in pool)]
    print(f"样本 {len(samples)} 条")
    if missing:
        print("下面这些在记录里找不到原文，要么是润色过的，要么已经不在时间窗里：")
        for text in missing:
            print(f"  {text}")
        return 1
    print("全部命中原文")
    return 0


# 长短分档。按显示宽度算，中文两字一档、一句话一档地往上走。
BANDS: list[tuple[str, int]] = [
    ("≤6（两三个字）", 6),
    ("7-12（一句短话）", 12),
    ("13-20（一句整话）", 20),
    ("21-36（说开了）", 36),
    (">36（长句）", 10**9),
]
# 这几样各自是一种说话习惯，库里一样都不能是零——模型只会用样本里出现过的东西。
MARKS: list[tuple[str, str]] = [
    ("！", r"！|!"),
    ("～", r"～|~"),
    ("？", r"？|\?"),
    ("，、", r"，|、"),
    ("空格断句", r"\S \S"),
    ("哈/笑", r"哈|笑"),
    ("英文数字", r"[A-Za-z0-9]"),
]


def samples_of(path: str) -> list[str]:
    return [
        line[2:].strip()
        for line in open(path, encoding="utf-8")
        if line.startswith("- ")
    ]


def profile(texts: list[str]) -> tuple[list[float], list[float]]:
    """一组句子的形状：各长短档的占比，以及各标点习惯的出现率。"""
    total = max(len(texts), 1)
    bands = []
    for _, ceiling in BANDS:
        floor = 0
        for name, edge in BANDS:
            if edge == ceiling:
                break
            floor = edge
        bands.append(sum(1 for t in texts if floor < width(t) <= ceiling) / total * 100)
    marks = [
        sum(1 for t in texts if re.search(pattern, t)) / total * 100
        for _, pattern in MARKS
    ]
    return bands, marks


def shape(rows: list[tuple[int, int, str]], path: str) -> int:
    """把「他本人」和「样本库」的形状并排打出来。

    样本是模型唯一的标尺，所以库的形状就是它写出来的字的形状。差得远的那一档
    就是下一批要补的：他本人四成多的话只有两三个字，而库里如果只有半成，
    人格就会把每句话都写成一句完整的话——那正是人机感最常见的来源。
    """
    mine = chatter(rows)
    library = samples_of(path)
    if not mine or not library:
        print("记录或样本库是空的，没法比形状")
        return 1
    real_bands, real_marks = profile(mine)
    lib_bands, lib_marks = profile(library)
    print(f"\n长短分布（他本人 {len(mine)} 条 / 样本库 {len(library)} 条）")
    print(f"{'档位':<18}{'他本人':>8}{'样本库':>8}{'差':>8}")
    for (name, _), real, lib in zip(BANDS, real_bands, lib_bands):
        print(f"{name:<18}{real:7.1f}%{lib:7.1f}%{lib - real:+7.1f}")
    print("\n说话习惯的出现率")
    print(f"{'习惯':<18}{'他本人':>8}{'样本库':>8}{'差':>8}")
    for (name, _), real, lib in zip(MARKS, real_marks, lib_marks):
        print(f"{name:<18}{real:7.1f}%{lib:7.1f}%{lib - real:+7.1f}")
    gaps = [
        f"{name}（{lib - real:+.0f}）"
        for (name, _), real, lib in list(zip(BANDS, real_bands, lib_bands))
        + list(zip(MARKS, real_marks, lib_marks))
        if abs(lib - real) >= 10
    ]
    if gaps:
        print("\n差得最多的：" + "、".join(gaps) + "——下一批就补这几样。")
    else:
        print("\n每一项都在十个百分点以内，形状对得上。")
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
    parser.add_argument("--shape", action="store_true", help="比对样本库与他本人的长短分布")
    args = parser.parse_args()

    rows = load(args.db, args.uid, args.days)
    print(f"取到 {len(rows)} 条本人发言（最近 {args.days} 天）", file=sys.stderr)
    if args.verify:
        return verify(rows, args.voice)
    if args.shape:
        return shape(rows, args.voice)
    known = set(samples_of(args.voice))
    for name, items in candidates(rows, args.limit, args.max_chars, known).items():
        print(f"\n## {name}")
        for text in items:
            print(f"  {text}")
    print(f"\n已在样本库里的 {len(known)} 条没有打印。挑好的粘进 res/ambient/voice.md（原样，别改字），再跑 --verify 核一遍。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
