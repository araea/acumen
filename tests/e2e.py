#!/usr/bin/env python3
"""acumen 端到端冒烟：自起假 Satori 实现端，拉起隔离的 acumen 实例，走一遍主要链路。

覆盖：指令（echo）、复读（接力 + satori-qq 时效参数 + 跟读记录落盘）、跟随撤回、
引用类指令的报错回复（sticker）、/ctl 读写配置（原子写、0600）、自发消息入库。
`E2E_STARTUP=1` 改为「除 console / restart 外全部启用，连上后空等 25 秒」，只看启动有无
ERRO 与 panic（不碰真账号：被测实例连的是自己起的假端口）。

用法（需要 `pip install aiohttp`）：
    cargo build --release --locked
    python3 tests/e2e.py target/release/acumen [工作目录]

被测实例在独立目录里跑（拷一份二进制，数据库与 data/ 都落在那里），不会碰线上的 data/。
"""
import asyncio, json, os, re, shutil, signal, sqlite3, subprocess, sys, tempfile, time
from aiohttp import web

BIN = os.path.abspath(sys.argv[1])
WORK = os.path.abspath(sys.argv[2]) if len(sys.argv) > 2 else os.path.join(tempfile.gettempdir(), "acumen-e2e")
PORT = 39117
GROUP = "900001"
BOT = "20001"
ADMIN = "10001"
ENABLED = {"meta_filter", "ctl", "recorder", "echo", "repeater", "recall", "sticker", "media"}
STARTUP = os.environ.get("E2E_STARTUP") == "1"
ALL_PLUGINS = re.findall(r"^    ([a-z_]+) \{", open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "../src/plugins/registry.rs")).read(), re.M)

LOGIN = {"platform": "red", "adapter": "satori-qq", "status": 1, "user": {"id": BOT, "name": "测试机器人"}, "features": []}

posts = []          # (method, body)
ws_clients = []
next_msg = [1000]
sn = [0]


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
        return web.json_response([{"id": fresh_id()}])
    if method in ("message.delete", "reaction.create", "reaction.delete"):
        return web.json_response({})
    if method == "message.get":
        return web.json_response({"id": body.get("message_id", "0"), "content": "原消息", "created_at": int(time.time() * 1000),
                                  "channel": {"id": GROUP, "type": 0}, "user": {"id": ADMIN}})
    if method in ("guild.list", "channel.list", "guild.member.list"):
        return web.json_response({"data": []})
    if method == "login.get":
        return web.json_response(LOGIN)
    return web.json_response({"code": "not_found", "message": method}, status=404)


async def inject(kind, user, content=None, message_id=None):
    sn[0] += 1
    body = {"sn": sn[0], "type": kind, "timestamp": int(time.time() * 1000), "login": LOGIN,
            "channel": {"id": GROUP, "type": 0}, "guild": {"id": GROUP, "name": "测试群"},
            "user": {"id": user, "name": f"用户{user}"}, "member": {"nick": f"群友{user}"}}
    if content is not None:
        mid = message_id or fresh_id()
        body["message"] = {"id": mid, "content": content}
    elif message_id:
        body["message"] = {"id": message_id}
    for ws in ws_clients:
        await ws.send_json({"op": 0, "body": body})
    return body.get("message", {}).get("id")


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
    sys.exit(1 if failures else 0)


asyncio.run(main())
