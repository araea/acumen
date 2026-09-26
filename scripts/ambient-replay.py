#!/usr/bin/env python3
"""从 data/bot.db 导出一段真实群聊，供 `ambient::integration_tests::live_replay` 回放。

    scripts/ambient-replay.py 818965288 '2026-09-26 10:00' '2026-09-26 10:55' > /tmp/r.json

默认把「机器人当时开口」的那几个时刻当作回放点（只给它看那一刻之前的消息），
这样新旧两版人格对同一处现场各说一句，能并排比。`--every N` 改成每 N 条取一个点。
只读数据库，不碰线上状态。
"""
import argparse, datetime, json, re, sqlite3, sys

ME = 3373167460
TZ = datetime.timezone(datetime.timedelta(hours=8))


def clean(text: str) -> str:
    text = re.sub(rf"\[@{ME}\]@\S+", "@我 ", text)
    text = re.sub(r"\[@(\d+)\]@\S+", r"[at:\1] ", text)
    text = text.replace("[回复]", "[引用] ")
    return " ".join(text.split())[:400]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("group", type=int)
    ap.add_argument("start")
    ap.add_argument("end")
    ap.add_argument("--db", default="data/bot.db")
    ap.add_argument("--every", type=int, default=0)
    args = ap.parse_args()
    stamp = lambda s: int(datetime.datetime.fromisoformat(s).replace(tzinfo=TZ).timestamp())
    db = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
    rows = db.execute(
        "select id, user_id, user_name, content_rich, time from message_records "
        "where group_id=? and time between ? and ? order by time, id",
        (args.group, stamp(args.start), stamp(args.end)),
    ).fetchall()
    lines = [
        {"id": i, "user_id": u, "name": n, "text": clean(c), "at": t, "me": u == ME}
        for i, u, n, c, t in rows
    ]
    if args.every:
        cuts = list(range(args.every, len(lines) + 1, args.every))
    else:
        cuts = [i for i, line in enumerate(lines) if line["me"] and (i == 0 or not lines[i - 1]["me"])]
    json.dump({"group": args.group, "lines": lines, "cuts": cuts}, sys.stdout, ensure_ascii=False)


if __name__ == "__main__":
    main()
