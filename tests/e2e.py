#!/usr/bin/env python3
"""acumen 端到端冒烟：自起假 Satori 实现端，拉起隔离的 acumen 实例，走一遍主要链路。

覆盖：入群欢迎（验证、倒计时提醒、通过、超时移出）、防撤回（转存到私聊）、指令（echo）、复读（接力 + satori-qq 时效参数 + 跟读记录落盘）、跟随撤回、
引用类指令的报错回复（sticker）、/ctl 读写配置（原子写、0600）、自发消息入库、
禁言通知入库与禁言排行榜、文字榜（字数与纯文本消息条数）的入库与出图（出图需要字体：
E2E_FONT 指一个字体文件，缺省用 Termux 的 MiSans，都没有就只验入库与文字提示）。
奶龙拦截要识图模型：`E2E_NAILONG_MODEL` 指 MobileCLIP-S0 的 vision_model.onnx，`E2E_NAILONG_POS` /
`E2E_NAILONG_NEG` 各指一张奶龙图与一张普通图；缺任何一个就跳过这一段。`E2E_NAILONG_DOWNLOAD=1` 不预放模型，
走插件自己的下载（真连 Hugging Face，首张图最多等 10 分钟）。
`E2E_STARTUP=1` 改为「除 console / restart 外全部启用，连上后空等 25 秒」，只看启动有无
ERRO 与 panic（不碰真账号：被测实例连的是自己起的假端口）。

用法（需要 `pip install aiohttp`）：
    cargo build --release --locked
    python3 tests/e2e.py target/release/acumen [工作目录]

被测实例在独立目录里跑（拷一份二进制，数据库与 data/ 都落在那里），不会碰线上的 data/。
"""
import asyncio, base64, json, os, re, shutil, signal, sqlite3, subprocess, sys, tempfile, time
from aiohttp import web

BIN = os.path.abspath(sys.argv[1])
WORK = os.path.abspath(sys.argv[2]) if len(sys.argv) > 2 else os.path.join(tempfile.gettempdir(), "acumen-e2e")
PORT = 39117
GROUP = "900001"
BOT = "20001"
ADMIN = "10001"
ENABLED = {"meta_filter", "ctl", "recorder", "echo", "repeater", "recall", "sticker", "media", "stats", "welcome", "anti_recall"}
FONT = next((f for f in (os.environ.get("E2E_FONT"), "/data/data/com.termux/files/usr/share/fonts/TTF/MiSans/MiSans-Medium.ttf") if f and os.path.exists(f)), None)
STARTUP = os.environ.get("E2E_STARTUP") == "1"
NAILONG = {k: os.environ.get(f"E2E_NAILONG_{k.upper()}") for k in ("model", "pos", "neg")}
NAILONG = NAILONG if all(v and os.path.exists(v) for v in NAILONG.values()) else None
if NAILONG:
    ENABLED = ENABLED | {"nailong"}
ALL_PLUGINS = re.findall(r"^    ([a-z_]+) \{", open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "../src/plugins/registry.rs")).read(), re.M)

LOGIN = {"platform": "red", "adapter": "satori-qq", "status": 1, "user": {"id": BOT, "name": "测试机器人"}, "features": []}

posts = []          # (method, body)
ws_clients = []
next_msg = [1000]
sn = [0]


async def serve_image(request):
    return web.FileResponse(NAILONG[request.match_info["name"]])


def fresh_id():
    next_msg[0] += 1
    return str(next_msg[0])


async def events_ws(request):
    ws = web.WebSocketResponse()
    await ws.prepare(request)
    ws_clients.append(ws)
    async for msg in ws:
        data = json.loads(msg.data)
        if data.get("op") == 3:
            await ws.send_json({"op": 4, "body": {"logins": [LOGIN], "proxy_urls": []}})
        elif data.get("op") == 1:
            await ws.send_json({"op": 2, "body": {}})
    return ws


async def api(request):
    method = request.match_info["method"]
    try:
        body = await request.json()
    except Exception:
        body = {}
    posts.append((method, body))
    if method == "message.create":
        body["_id"] = fresh_id()  # 记下分给这条的 ID，引用机器人的话时要用
        return web.json_response([{"id": body["_id"]}])
    if method in ("message.delete", "reaction.create", "reaction.delete", "guild.member.kick", "guild.member.mute", "guild.member.approve"):
        return web.json_response({})
    if method == "user.channel.create":
        return web.json_response({"id": f"private:{body.get('user_id', '')}", "type": 1})
    if method == "guild.member.get":
        uid = body.get("user_id", "")
        return web.json_response({"user": {"id": uid, "name": f"用户{uid}"}, "nick": f"群友{uid}",
                                  "roles": [{"id": "admin" if uid == BOT else "member"}]})
    if method == "message.get":
        return web.json_response({"id": body.get("message_id", "0"), "content": "原消息", "created_at": int(time.time() * 1000),
                                  "channel": {"id": GROUP, "type": 0}, "user": {"id": ADMIN}})
    if method in ("guild.list", "channel.list", "guild.member.list"):
        return web.json_response({"data": []})
    if method == "login.get":
        return web.json_response(LOGIN)
    return web.json_response({"code": "not_found", "message": method}, status=404)


async def inject(kind, user, content=None, message_id=None, extra=None):
    sn[0] += 1
    body = {"sn": sn[0], "type": kind, "timestamp": int(time.time() * 1000), "login": LOGIN,
            "channel": {"id": GROUP, "type": 0}, "guild": {"id": GROUP, "name": "测试群"},
            "user": {"id": user, "name": f"用户{user}"}, "member": {"nick": f"群友{user}"}}
    if content is not None:
        mid = message_id or fresh_id()
        body["message"] = {"id": mid, "content": content}
    elif message_id:
        body["message"] = {"id": message_id}
    body.update(extra or {})
    for ws in ws_clients:
        await ws.send_json({"op": 0, "body": body})
    return body.get("message", {}).get("id")


async def inject_mute(user, duration_ms=None, operator=None, seconds_ago=0):
    """satori-qq 的禁言通知：guild-member-updated + _type=satori-qq/mute；duration_ms=0 是解除，None 是没读出时长。"""
    sn[0] += 1
    body = {"sn": sn[0], "type": "guild-member-updated", "timestamp": int((time.time() - seconds_ago) * 1000), "login": LOGIN,
            "channel": {"id": GROUP, "type": 0}, "guild": {"id": GROUP}, "user": {"id": user}, "member": {},
            "_type": "satori-qq/mute", "_data": {"duration": duration_ms} if duration_ms is not None else {"duration_unknown": True}}
    if operator:
        body["operator"] = {"id": operator}
    for ws in ws_clients:
        await ws.send_json({"op": 0, "body": body})


def sent_texts():
    return [b.get("content", "") for m, b in posts if m == "message.create"]


async def wait_for(pred, what, timeout=12.0):
    end = time.time() + timeout
    while time.time() < end:
        if pred():
            return True
        await asyncio.sleep(0.15)
    raise AssertionError(f"超时未等到：{what}\n已收 POST：{[(m, json.dumps(b, ensure_ascii=False)[:160]) for m, b in posts][-8:]}")


def write_config(directory):
    lines = ['command_prefix = ["/"]', "", "[global_filter]", "enable_blacklist = false", "blacklist = []",
             "enable_whitelist = false", "whitelist = []", "", "[[bots]]", 'protocol = "satori"', "enabled = true",
             f'url = "http://127.0.0.1:{PORT}"', ""]
    for name in ALL_PLUGINS:
        on = (name not in ("console", "restart")) if STARTUP else (name in ENABLED)
        lines += [f"[{name}]", f"enabled = {'true' if on else 'false'}"]
        if name == "ctl":
            lines += [f'admins = ["{ADMIN}"]']
        if name == "stats":
            # 定时推送一律关掉：被测实例不该在某个整点往假端口发图
            lines += [f"{k}_enabled = false" for k in ("daily_push", "morning_recap", "noon_brief", "weekly_recap", "monthly_recap")]
            if FONT:
                lines += [f'font_path = "{FONT}"', 'font_family = "MiSans"']
        if name == "welcome":
            lines += ["[welcome.verify]", "enabled = true", "timeout_seconds = 30", 'answers = ["我已阅读群规"]',
                      "remind_seconds = [25]", 'action = "kick"', "", "[welcome.approve]", "enabled = true",
                      'keywords = ["芝麻开门"]', "reject = true"]
        if name == "nailong":
            lines += ["mute_seconds = [0, 600]"]
        if name == "repeater":
            lines += ["min_times = 2", "cooldown_seconds = 0", "max_delay_ms = 600000", "remember_hours = 0"]
        lines.append("")
    open(os.path.join(directory, "config.toml"), "w").write("\n".join(lines))


class SystemExit_(Exception):
    pass


async def main():
    shutil.rmtree(WORK, ignore_errors=True)
    os.makedirs(WORK)
    exe = os.path.join(WORK, "acumen")
    shutil.copy(BIN, exe)
    write_config(WORK)

    app = web.Application(client_max_size=64 * 1024 * 1024)
    app.router.add_get("/v1/events", events_ws)
    app.router.add_post("/v1/{method:.*}", api)
    app.router.add_get("/img/{name}", serve_image)
    if NAILONG and os.environ.get("E2E_NAILONG_DOWNLOAD") != "1":
        os.makedirs(os.path.join(WORK, "data", "nailong"))
        shutil.copy(NAILONG["model"], os.path.join(WORK, "data", "nailong", "mobileclip_s0_vision.onnx"))
    runner = web.AppRunner(app)
    await runner.setup()
    await web.TCPSite(runner, "127.0.0.1", PORT).start()

    env = dict(os.environ)
    log = open(os.path.join(WORK, "run.log"), "wb")
    child = subprocess.Popen([exe], cwd=WORK, env=env, stdout=log, stderr=subprocess.STDOUT)
    failures = []

    def check(name, ok, detail=""):
        print(("  通过 " if ok else "  失败 ") + name + (f"  {detail}" if detail and not ok else ""))
        if not ok:
            failures.append(name)

    try:
        await wait_for(lambda: len(ws_clients) > 0, "acumen 连上假服务端", 40)
        await asyncio.sleep(2.0)  # 等插件初始化、连接钩子

        if STARTUP:
            await asyncio.sleep(25)
            print("启动检查：连接后空等 25 秒，看日志有无 ERRO / panic")
            raise SystemExit_(0)
        print("1. 指令：/echo")
        await inject("message-created", ADMIN, "/echo 你好重构")
        await wait_for(lambda: any("你好重构" in t for t in sent_texts()), "echo 回显")
        check("echo 回显", True)

        print("1b. 入群欢迎：验证提示、倒计时、答对放行、超时移出、申请按附言审批")
        mark = len(posts)
        await inject("guild-member-added", "10009")
        await inject("guild-member-added", "10010")
        hello = lambda uid: any(m == "message.create" and f'id="{uid}"' in b.get("content", "") and "验证" in b.get("content", "") for m, b in posts[mark:])
        await wait_for(lambda: hello("10009") and hello("10010"), "两人的欢迎带 @ 与验证说明")
        check("欢迎 @ 新人并给出验证说明", True)
        await inject("message-created", "10009", "好的，我已阅读群规")
        await wait_for(lambda: any("验证通过" in b.get("content", "") and 'id="10009"' in b.get("content", "") for m, b in posts[mark:] if m == "message.create"), "10009 验证通过")
        check("答对验证即放行", True)
        await wait_for(lambda: any("验证还剩" in b.get("content", "") and 'id="10010"' in b.get("content", "") for m, b in posts[mark:] if m == "message.create"), "10010 倒计时提醒", 15)
        check("剩余时间点 @ 提醒", True)
        await inject("guild-member-request", "10020", extra={"message": {"id": "flag-1", "content": "问题：暗号\n答案：芝麻开门"}})
        await inject("guild-member-request", "10021", extra={"message": {"id": "flag-2", "content": "广告位招租"}})
        await wait_for(lambda: sum(1 for m, b in posts[mark:] if m == "guild.member.approve") >= 2, "两条申请都被处理")
        verdicts = {b.get("message_id"): b.get("approve") for m, b in posts[mark:] if m == "guild.member.approve"}
        check("附言对上的同意、对不上的拒绝", verdicts == {"flag-1": True, "flag-2": False}, str(verdicts))
        await wait_for(lambda: any(m == "guild.member.kick" and b.get("user_id") == "10010" for m, b in posts[mark:]), "10010 超时被移出", 40)
        check("超时移出", not any(m == "guild.member.kick" and b.get("user_id") == "10009" for m, b in posts[mark:]))

        print("1c. 防撤回：去处留空，撤回的消息转存到号主自己的私聊")
        mark = len(posts)
        secret = await inject("message-created", "10002", "这句话我马上撤回")
        await asyncio.sleep(0.5)
        await inject("message-deleted", "10002", message_id=secret)
        await wait_for(lambda: any(m == "message.create" and b.get("channel_id") == f"private:{BOT}" and "这句话我马上撤回" in b.get("content", "") for m, b in posts[mark:]), "转存到私聊")
        check("撤回的原文转存到私聊", True)

        if NAILONG:
            print("1d. 奶龙拦截：撤回、第二次禁言、引用提醒说「不是」即解禁并记住、普通图不动")
            mark = len(posts)
            img = lambda name: f'<img src="http://127.0.0.1:{PORT}/img/{name}"/>'
            tips = lambda: [b.get("content", "") for m, b in posts[mark:] if m == "message.create" and "奶龙" in b.get("content", "")]
            first = await inject("message-created", "10030", img("pos"))
            await wait_for(lambda: any(m == "message.delete" and b.get("message_id") == first for m, b in posts[mark:]), "奶龙图被撤回", 600 if os.environ.get("E2E_NAILONG_DOWNLOAD") == "1" else 90)
            await wait_for(lambda: len(tips()) >= 1, "撤回后的提醒")
            check("认出奶龙图、撤回并 @ 提醒", 'id="10030"' in tips()[0], tips()[0])
            second = await inject("message-created", "10030", img("pos"))
            await wait_for(lambda: any(m == "guild.member.mute" and b.get("duration") == 600000 for m, b in posts[mark:]), "第二次禁言 10 分钟")
            check("第二次按阶梯禁言", any(m == "message.delete" and b.get("message_id") == second for m, b in posts[mark:]))
            await wait_for(lambda: len(tips()) >= 2, "禁言提醒")
            tip_id = [b["_id"] for m, b in posts[mark:] if m == "message.create" and "奶龙" in b.get("content", "")][-1]
            await inject("message-created", ADMIN, f'<quote id="{tip_id}"/>/奶龙 不是')
            await wait_for(lambda: any(m == "guild.member.mute" and b.get("duration") == 0 for m, b in posts[mark:]), "解除禁言")
            await wait_for(lambda: any("记住了" in t for t in tips()), "纠正回执")
            check("引用提醒说「不是」：解禁并回执", True)
            later = len(posts)
            third = await inject("message-created", "10031", img("pos"))
            neg = await inject("message-created", "10032", img("neg"))
            await asyncio.sleep(8)
            check("纠正过的图与普通图都不再撤", not any(m == "message.delete" and b.get("message_id") in (third, neg) for m, b in posts[later:]))
            await inject("message-created", "10033", "/奶龙")
            await wait_for(lambda: any("拦下" in t or "还没拦过" in t for t in sent_texts()[-3:]), "状态")
            status = next(t for t in reversed(sent_texts()) if "拦下" in t or "还没拦过" in t)
            check("/奶龙 状态：纠正掉的那次不计，另一次还在", "今天拦下 1 张" in status, status)
            for text in [b.get("content", "") for m, b in posts[mark:] if m == "message.create"]:
                print("     机器人：" + re.sub(r"\s+", " ", re.sub(r"<[^>]+>", lambda t: "@" if t.group(0).startswith("<at") else "", text))[:120])

        print("2. 复读：两个不同的人接力同一句")
        before = len(sent_texts())
        await inject("message-created", "10002", "今天天气不错")
        await asyncio.sleep(0.3)
        await inject("message-created", "10003", "今天天气不错")
        await wait_for(lambda: len(sent_texts()) > before and any("今天天气不错" in t for t in sent_texts()[before:]), "复读发出")
        check("复读发出", True)
        await asyncio.sleep(1.0)
        recent = os.path.join(WORK, "data", "repeater", "recent.json")
        check("跟读记录落盘(原子写)", os.path.exists(recent) and json.load(open(recent)) is not None, "recent.json 缺失")
        check("没有残留临时文件", not [f for f in os.listdir(os.path.dirname(recent)) if ".tmp-" in f])

        print("3. 跟随撤回：触发消息被撤回 → 机器人撤掉它的回复")
        trigger = fresh_id()
        mark = len(posts)
        await inject("message-created", ADMIN, "/echo 请撤回我", message_id=trigger)
        await wait_for(lambda: any(m == "message.create" and "请撤回我" in b.get("content", "") for m, b in posts[mark:]), "回复发出")
        await asyncio.sleep(0.5)
        await inject("message-deleted", ADMIN, message_id=trigger)
        await wait_for(lambda: any(m == "message.delete" for m, _ in posts[mark:]), "message.delete 发出")
        check("跟随撤回", True)

        print("4. sticker 无引用：回错误提示")
        mark = len(posts)
        await inject("message-created", ADMIN, "/收")
        await wait_for(lambda: any("请引用" in b.get("content", "") for m, b in posts[mark:] if m == "message.create"), "sticker 提示")
        check("sticker 提示", True)

        print("5. ctl list（PluginConfig 读取、注册表）")
        mark = len(posts)
        await inject("message-created", ADMIN, "/ctl list")
        await wait_for(lambda: any(m == "message.create" for m, b in posts[mark:]), "ctl list 回复（文本或卡片图）", 40)
        check("ctl list", True)

        print("5b. 号主（登录号亲手发的）不在 admins 里也能管；机器人自己的回声不行")
        mark = len(posts)
        await inject("message-created", BOT, "/ctl set repeater cooldown_seconds 1", extra={"satori_qq": {"manual_self": True, "actual_user_id": BOT}})
        await wait_for(lambda: any(m == "message.create" for m, _ in posts[mark:]), "号主的 ctl set 回复", 40)
        await asyncio.sleep(0.5)
        check("号主亲手发的 /ctl set 生效", re.search(r"cooldown_seconds = 1\b", open(os.path.join(WORK, "config.toml")).read()) is not None,
              str([b.get("content", "")[:120] for m, b in posts[mark:] if m == "message.create"]))
        mark = len(posts)
        await inject("message-created", BOT, "/ctl set repeater cooldown_seconds 7")
        await asyncio.sleep(3)
        check("机器人自己的回声不能改配置", re.search(r"cooldown_seconds = 7\b", open(os.path.join(WORK, "config.toml")).read()) is None)

        print("6. ctl set 写配置（storage 原子写 0600）")
        mark = len(posts)
        await inject("message-created", ADMIN, "/ctl set repeater min_times 3")
        await wait_for(lambda: any(m == "message.create" for m, _ in posts[mark:]), "ctl set 回复", 40)
        await asyncio.sleep(0.5)
        cfg = open(os.path.join(WORK, "config.toml")).read()
        check("config.toml 写回了新值", re.search(r"min_times = 3", cfg) is not None)
        mode = oct(os.stat(os.path.join(WORK, "config.toml")).st_mode & 0o777)
        check("config.toml 权限 0600", mode == "0o600", mode)
        check("config 目录没有残留临时文件", not [f for f in os.listdir(WORK) if ".tmp-" in f], str(os.listdir(WORK)))

        print("7. 消息记录：机器人自己发的消息进了库")
        await asyncio.sleep(1.0)
        db = sqlite3.connect(f"file:{os.path.join(WORK, 'data', 'bot.db')}?mode=ro", uri=True)
        tables = [r[0] for r in db.execute("select name from sqlite_master where type='table'")]
        rows = 0
        for t in tables:
            cols = [r[1] for r in db.execute(f"pragma table_info({t})")]
            if "member_role" in cols:
                rows = db.execute(f"select count(*) from {t} where member_role='self'").fetchone()[0]
        check("机器人发言入库", rows >= 1, f"self 行数={rows}")
        db.close()

        print("8. 禁言：通知入库（去重、解除、没读出时长），再查排行榜")
        # 10002：禁言 30 天、一分钟后被解除（只该算 1 分钟），又被禁言 10 分钟；10003：时长没读出；
        # 同一条灰条被推了两遍（只该记一次）；旧版 satori-qq 读不出时长时填的 1 秒（该当没读出）。
        await inject_mute("10002", 30 * 86400 * 1000, operator="10001", seconds_ago=900)
        await inject_mute("10002", 0, operator="10001", seconds_ago=840)
        await inject_mute("10002", 600_000, operator="10001", seconds_ago=500)
        await inject_mute("10002", 600_000, operator="10001", seconds_ago=499)
        await inject_mute("10003", None, seconds_ago=300)
        await inject_mute("10003", 1000, seconds_ago=100)
        await inject_mute("10004", None, operator="10001", seconds_ago=200)  # 有操作者、时长没读出
        # 累计 vs 单次：10005 被禁言三次、每次 2 小时（累计 6 小时），10006 只被禁言一次但是 5 小时。
        # 累计榜该是 10005 在前；单次最长榜该是 10006 在前（5 小时 > 2 小时）。
        for ago in (40000, 30000, 20000):
            await inject_mute("10005", 7200_000, operator="10001", seconds_ago=ago)
        await inject_mute("10006", 5 * 3600_000, operator="10001", seconds_ago=50000)
        await asyncio.sleep(1.5)
        db = sqlite3.connect(f"file:{os.path.join(WORK, 'data', 'bot.db')}?mode=ro", uri=True)
        rows = db.execute("select user_id, operator_id, duration from member_bans order by time").fetchall()
        db.close()
        check("禁言通知入库且去重（操作者随通知存下）",
              rows == [("10006", "10001", 18000), ("10005", "10001", 7200), ("10005", "10001", 7200), ("10005", "10001", 7200),
                       ("10002", "10001", 2592000), ("10002", "10001", 0), ("10002", "10001", 600), ("10003", "", -1), ("10004", "10001", -1), ("10003", "", -1)], str(rows))

        mark = len(posts)
        await inject("message-created", "10002", "/我的本月禁言排行榜")
        await wait_for(lambda: any("禁言只有本群" in b.get("content", "") for m, b in posts[mark:] if m == "message.create"), "个人范围的说明")
        check("禁言榜只支持本群：给出说明", True)

        mark = len(posts)
        await inject("message-created", "10002", "/跨群今日管理员禁言排行榜")
        await wait_for(lambda: any("管理员禁言排行榜" in b.get("content", "") for m, b in posts[mark:] if m == "message.create"), "管理员榜的范围说明")
        check("管理员禁言榜同样只支持本群：说明里点名了管理员榜", True)

        mark = len(posts)
        await inject("message-created", "10002", "/本群近7天禁言走势")
        await wait_for(lambda: any("禁言只有本群" in b.get("content", "") for m, b in posts[mark:] if m == "message.create"), "走势的说明")
        check("禁言没有走势：给出说明", True)

        if FONT:
            for command in ("/本群近7天禁言排行榜", "/本群近7天禁言时长排行榜", "/本群近7天管理员禁言排行榜", "/本群近7天管理员禁言时长排行榜", "/本群近7天禁言总时长排行榜", "/本群近7天单次最长禁言排行榜", "/本群近7天管理员最长禁言排行榜", "/本群近7天灰条禁言时长排行榜", "/本群近7天管理员标称禁言时长排行榜"):
                mark = len(posts)
                await inject("message-created", "10002", command)
                await wait_for(lambda: any("<img" in b.get("content", "") for m, b in posts[mark:] if m == "message.create"), f"{command} 出图", 60)
                check(f"{command} 出图", True)
                if os.environ.get("E2E_IMAGES"):  # 存下来人眼看
                    os.makedirs(os.environ["E2E_IMAGES"], exist_ok=True)
                    shot = next(b["content"] for m, b in reversed(posts[mark:]) if m == "message.create" and "<img" in b.get("content", ""))
                    blob = re.search(r"base64://([A-Za-z0-9+/=]+)", shot)
                    if blob:
                        with open(os.path.join(os.environ["E2E_IMAGES"], command.strip("/") + ".png"), "wb") as out:
                            out.write(base64.b64decode(blob.group(1)))
        else:
            print("  跳过 禁言榜出图（没有可用字体，设 E2E_FONT）")

        mark = len(posts)
        await inject("message-created", "10002", "/本群上月禁言排行榜")
        await wait_for(lambda: any("没有数据" in b.get("content", "") for m, b in posts[mark:] if m == "message.create"), "空榜提示")
        check("没有禁言的区间：给出空态说明", True)

        print("9. 文字榜：字数、纯文本消息（图片、表情包、语音不算）")
        # 10011：一条 20 字；10012：三段各 4 字；10013：只有表情包；
        # 10014：50 字配一张图——算字数，但不算一条纯文本消息。
        await inject("message-created", "10011", "一" * 20)
        for text in ("甲乙丙丁", "戊己庚辛", "壬癸子丑"):
            await inject("message-created", "10012", text)
        await inject("message-created", "10013", "<mface/>")
        await inject("message-created", "10014", "三" * 50 + '<img src="x.png"/>')
        await asyncio.sleep(1.5)
        db = sqlite3.connect(f"file:{os.path.join(WORK, 'data', 'bot.db')}?mode=ro", uri=True)
        rows = {uid: (chars, msgs) for uid, chars, msgs in db.execute(
            "select user_id, sum(text_chars), sum(text_msg_count) from message_user_stats_daily group by user_id")}
        db.close()
        check("字数与纯文本条数落库：分段三条胜过一条",
              rows.get("10014") == (50, 0) and rows.get("10011") == (20, 1) and rows.get("10012") == (12, 3), str(rows))
        check("只有表情包：既不算字也不算纯文本消息", rows.get("10013", (0, 0)) == (0, 0), str(rows.get("10013")))

        mark = len(posts)
        await inject("message-created", "10002", "/本群今日发言字数走势")
        await wait_for(lambda: any("只有排行榜" in b.get("content", "") for m, b in posts[mark:] if m == "message.create"), "字数走势的说明")
        check("文字榜没有走势：给出说明", True)

        if FONT:
            for command in ("/本群今日发言字数排行榜", "/本群今日文本消息排行榜",
                            "/本群今日字数排行榜 5", "/本群今日文本排行榜", "/我的总发言字数排行榜"):
                mark = len(posts)
                await inject("message-created", "10002", command)
                await wait_for(lambda: any("<img" in b.get("content", "") for m, b in posts[mark:] if m == "message.create"), f"{command} 出图", 60)
                check(f"{command} 出图", True)
                if os.environ.get("E2E_IMAGES"):
                    os.makedirs(os.environ["E2E_IMAGES"], exist_ok=True)
                    shot = next(b["content"] for m, b in reversed(posts[mark:]) if m == "message.create" and "<img" in b.get("content", ""))
                    blob = re.search(r"base64://([A-Za-z0-9+/=]+)", shot)
                    if blob:
                        with open(os.path.join(os.environ["E2E_IMAGES"], command.strip("/") + ".png"), "wb") as out:
                            out.write(base64.b64decode(blob.group(1)))
        else:
            print("  跳过 文字榜出图（没有可用字体，设 E2E_FONT）")
    except SystemExit_:
        pass
    except AssertionError as error:
        print("  失败 ", error)
        failures.append(str(error)[:80])
    finally:
        child.send_signal(signal.SIGTERM)
        try:
            child.wait(timeout=10)
        except subprocess.TimeoutExpired:
            child.kill()
        await runner.cleanup()
        log.close()

    logtext = open(os.path.join(WORK, "run.log"), errors="replace").read()
    errors = [l for l in logtext.splitlines() if "ERRO" in l]
    panics = [l for l in logtext.splitlines() if "panicked" in l]
    warns = [re.sub(r"\x1b\[[0-9;]*m", "", l)[:200] for l in logtext.splitlines() if "WARN" in l]
    print(f"\n日志里的 WARN {len(warns)} 条 / panic {len(panics)} 条")
    for line in warns[:15]:
        print("   ", line)
    if panics:
        failures.append("panic")
    print(f"\n日志里的 ERRO 行 {len(errors)} 条")
    for line in errors[:10]:
        print("   ", re.sub(r"\x1b\[[0-9;]*m", "", line)[:200])
    print("\n结果：" + ("全部通过" if not failures else f"{len(failures)} 项失败：{failures}"))
    # 跑完不留工作目录：通过则整个删掉；失败只删约 50MB 的二进制副本，日志和配置留着排查。
    if failures:
        os.remove(os.path.join(WORK, "acumen"))
    else:
        shutil.rmtree(WORK, ignore_errors=True)
    sys.exit(1 if failures else 0)


asyncio.run(main())
