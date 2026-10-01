#!/usr/bin/env python3
"""从本机记录里量号主本人的在线节奏，给 `ambient::mood` 的作息曲线出表。

搭话人格顶替的是号主本人，它的「精神头」该跟他真实的在线规律走，而不是一条凭印象
画的曲线（原先那条把凌晨三点当成最困的时候，而他恰恰常在那会儿还在群里说话）。

量法：数「他有没有在这个钟点的某个十分钟里亲手发过消息」——量的是**在线**，不是
话多话少（词意猜词那类连发会把话量撑歪）。分工作日与周末各一张 24 格表，做三点环形
平滑，再线性拉到 0.30–0.92 之间当精神头基线。周末只有十来天样本，太单薄，与工作日
对半混一混再用。

    python3 scripts/owner-rhythm.py --uid 3373167460          # 打印 Rust 常量
    python3 scripts/owner-rhythm.py --uid 3373167460 --raw    # 另打印未平滑的在线率

新旧两份库（`data/bot.db` 与迁移前备份）都读；列名按各自的结构自动认。
"""

from __future__ import annotations

import argparse
import collections
import datetime
import os
import sqlite3

LOW, HIGH = 0.30, 0.92


def times(db: str, uid: str) -> list[int]:
    if not os.path.exists(db):
        return []
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    cols = {row[1] for row in con.execute("pragma table_info(message_records)")}
    role = "member_role" if "member_role" in cols else "role"
    # 号主手打的消息：同一个号里 role 不是 self 的那些。
    query = f"select time from message_records where cast(user_id as text)=? and {role} != 'self'"
    return [row[0] for row in con.execute(query, (uid,))]


def presence(stamps: list[int]) -> tuple[dict[str, list[float]], collections.Counter]:
    slots = {(d.date(), d.hour, d.minute // 10) for d in map(datetime.datetime.fromtimestamp, stamps)}
    first = datetime.datetime.fromtimestamp(min(stamps)).date()
    last = datetime.datetime.fromtimestamp(max(stamps)).date()
    days: collections.Counter = collections.Counter()
    day = first
    while day <= last:
        days["weekend" if day.weekday() >= 5 else "weekday"] += 1
        day += datetime.timedelta(days=1)
    table = {"weekday": [0.0] * 24, "weekend": [0.0] * 24}
    for day, hour, _ in slots:
        table["weekend" if day.weekday() >= 5 else "weekday"][hour] += 1
    for kind in table:
        table[kind] = [count / (days[kind] * 6) for count in table[kind]]
    return table, days


def smooth(values: list[float]) -> list[float]:
    return [(values[(i - 1) % 24] + values[i] + values[(i + 1) % 24]) / 3 for i in range(24)]


def scale(values: list[float], low: float, high: float) -> list[float]:
    return [round(LOW + (HIGH - LOW) * (v - low) / (high - low), 2) for v in values]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--uid", required=True)
    ap.add_argument("--db", action="append", default=None)
    ap.add_argument("--raw", action="store_true")
    args = ap.parse_args()
    dbs = args.db or ["data/bot.db.pre-string-ids", "data/bot.db"]
    stamps = sorted(t for db in dbs for t in times(db, args.uid))
    if not stamps:
        raise SystemExit("没有读到号主的手打记录")
    table, days = presence(stamps)
    print(f"// 样本：{len(stamps)} 条，{days['weekday']} 个工作日 + {days['weekend']} 个周末日")
    if args.raw:
        for kind, values in table.items():
            print(f"// 在线率 {kind}: {[round(v, 2) for v in values]}")
    weekday = smooth(table["weekday"])
    weekend = [(a + b) / 2 for a, b in zip(smooth(table["weekend"]), weekday)]
    low = min(weekday + weekend)
    high = max(weekday + weekend)
    for name, values in (("WEEKDAY", weekday), ("WEEKEND", weekend)):
        body = ", ".join(f"{v:.2f}" for v in scale(values, low, high))
        print(f"const {name}: [f32; 24] = [{body}];")


if __name__ == "__main__":
    main()
