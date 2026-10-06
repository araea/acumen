#!/usr/bin/env python3
"""内置 agent 房间（管家大人）的端到端：假 Satori 实现端 + 脚本化的假模型，拉起隔离的 acumen。

覆盖回复里的内嵌图片：模型用 bash 把图写进本轮临时目录、回复里写 `![说明](路径)`，
图应当按位置画进同一张卡片（而不是另发一条）；写了不存在的路径时只被提醒一次；
纯文字回复不受影响。假模型按提问里的关键词走脚本，不需要任何真实密钥。

用法（需要 `pip install aiohttp` 与 Chromium；浏览器路径取 `CHROME_BIN`）：
    cargo build --release --locked
    python3 tests/e2e_agent.py target/release/acumen [工作目录]
"""
import asyncio, base64, json, os, re, shutil, signal, subprocess, sys, tempfile, time
from aiohttp import web

BIN = os.path.abspath(sys.argv[1])
WORK = os.path.abspath(sys.argv[2]) if len(sys.argv) > 2 else os.path.join(tempfile.gettempdir(), "acumen-e2e-agent")
SATORI_PORT, LLM_PORT = 39119, 39120
GROUP, BOT, USER = "900001", "20001", "10001"
LOGIN = {"platform": "red", "adapter": "satori-qq", "status": 1, "user": {"id": BOT, "name": "测试机器人"}, "features": []}

# 一张 64×40 的纯色 PNG。
import struct, zlib
def tiny_png(w=64, h=40, rgb=(60, 120, 100)):
    raw = b"".join(b"\x00" + bytes(rgb) * w for _ in range(h))
    def chunk(t, d):
        c = struct.pack(">I", len(d)) + t + d
        return c + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")
PNG_B64 = base64.b64encode(tiny_png()).decode()

posts, ws_clients, llm_requests = [], [], []
sn, next_msg = [0], [1000]


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
    if method == "upload.create":
        return web.json_response({"code": "not_found", "message": method}, status=404)
    try:
        body = await request.json()
    except Exception:
        body = {}
    posts.append((method, body))
    if method == "message.create":
        return web.json_response([{"id": fresh_id()}])
    if method in ("message.delete", "reaction.create", "reaction.delete"):
        return web.json_response({})
    if method in ("guild.list", "channel.list", "guild.member.list", "message.list"):
        return web.json_response({"data": []})
    if method == "login.get":
        return web.json_response(LOGIN)
    return web.json_response({"code": "not_found", "message": method}, status=404)


async def inject(content):
    sn[0] += 1
    mid = fresh_id()
    body = {"sn": sn[0], "type": "message-created", "timestamp": int(time.time() * 1000), "login": LOGIN,
            "channel": {"id": GROUP, "type": 0}, "guild": {"id": GROUP, "name": "测试群"},
            "user": {"id": USER, "name": "用户"}, "member": {"nick": "群友"},
            "message": {"id": mid, "content": content}}
    for ws in ws_clients:
        await ws.send_json({"op": 0, "body": body})


def text_of(message):
    c = message.get("content")
    if isinstance(c, list):
        return "".join(p.get("text", "") for p in c if isinstance(p, dict))
    return c or ""


def completion(message):
    return web.json_response({
        "id": "x", "object": "chat.completion", "created": 0, "model": "scripted",
        "choices": [{"index": 0, "finish_reason": "tool_calls" if message.get("tool_calls") else "stop", "message": message}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
    })


async def chat(request):
    body = await request.json()
    llm_requests.append(body)
    msgs = body["messages"]
    system = text_of(msgs[0]) if msgs and msgs[0]["role"] == "system" else ""
    scratch = (re.search(r"临时目录 (/\S+?)，", system) or [None, "/nonexistent"])[1]
    # 房间历史会带着前几轮：只看最近那句真正的提问（跳过「改错提醒」）。
    asked_at = max(i for i, m in enumerate(msgs) if m["role"] == "user" and "没能嵌进卡片" not in text_of(m))
    asked = text_of(msgs[asked_at])
    last = text_of([m for m in msgs if m["role"] == "user"][-1])
    tools_done = any(m["role"] == "tool" for m in msgs[asked_at:])

    if "画图表" in asked:
        if not tools_done:
            cmd = f"printf '%s' {PNG_B64} | base64 -d > {scratch}/c.png && ls -l {scratch}/c.png"
            return completion({"role": "assistant", "content": None, "tool_calls": [
                {"id": "call_1", "type": "function", "function": {"name": "bash", "arguments": json.dumps({"command": cmd})}}]})
        return completion({"role": "assistant", "content": f"好的，图表如下：\n\n![本周走势]({scratch}/c.png)\n\n以上。"})
    if "坏图" in asked:
        if "没能嵌进卡片" in last:
            return completion({"role": "assistant", "content": "改好了，这回不放图。"})
        return completion({"role": "assistant", "content": "看这张：\n\n![错的](/nope/none.png)"})
    if "死图" in asked:  # 改了也不好：第二次还是坏的
        return completion({"role": "assistant", "content": "看这张：\n\n![依旧错](/nope/none.png)\n\n完。"})
    return completion({"role": "assistant", "content": "你好，这是一句话。"})


def creates_since(mark):
    return [b.get("content", "") for m, b in posts[mark:] if m == "message.create"]


async def wait_for(pred, what, timeout=60.0):
    end = time.time() + timeout
    while time.time() < end:
        if pred():
            return
        await asyncio.sleep(0.2)
    raise AssertionError(f"超时：{what}\n最近 POST：{[(m, json.dumps(b, ensure_ascii=False)[:200]) for m, b in posts][-6:]}")


async def main():
    shutil.rmtree(WORK, ignore_errors=True)
    os.makedirs(os.path.join(WORK, "data", "oai"))
    shutil.copy(BIN, os.path.join(WORK, "acumen"))
    json.dump({"api_base": f"http://127.0.0.1:{LLM_PORT}/v1", "api_key": "k", "default_model": "scripted"},
              open(os.path.join(WORK, "data", "oai", "config.json"), "w"))
    browser = os.environ.get("CHROME_BIN", "/data/data/com.termux/files/usr/bin/chromium-browser")
    others = re.findall(r"^    ([a-z_]+) \{", open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "../src/plugins/registry.rs")).read(), re.M)
    off = "".join(f"[{n}]\nenabled = false\n\n" for n in others if n not in ("oai", "meta_filter"))
    open(os.path.join(WORK, "config.toml"), "w").write(f'''command_prefix = ["/"]
browser_path = "{browser}"

[global_filter]
enable_blacklist = false
blacklist = []
enable_whitelist = false
whitelist = []

[[bots]]
protocol = "satori"
enabled = true
url = "http://127.0.0.1:{SATORI_PORT}"

[oai]
enabled = true
api_base = "http://127.0.0.1:{LLM_PORT}/v1"
api_key = "k"
agent_default_model = "fake/scripted"
plain_text_max_chars = 120

[oai.providers.fake]
api_base = "http://127.0.0.1:{LLM_PORT}/v1"
api_key = "k"

[oai.search]
enabled = false

''' + off)

    app = web.Application(client_max_size=64 * 1024 * 1024)
    app.router.add_get("/v1/events", events_ws)
    app.router.add_post("/v1/{method:.*}", api)
    llm = web.Application(client_max_size=64 * 1024 * 1024)
    llm.router.add_post("/v1/chat/completions", chat)
    runners = []
    for application, port in ((app, SATORI_PORT), (llm, LLM_PORT)):
        runner = web.AppRunner(application)
        await runner.setup()
        await web.TCPSite(runner, "127.0.0.1", port).start()
        runners.append(runner)

    log = open(os.path.join(WORK, "run.log"), "wb")
    child = subprocess.Popen([os.path.join(WORK, "acumen")], cwd=WORK, env=dict(os.environ), stdout=log, stderr=subprocess.STDOUT)
    failures = []

    def check(name, ok, detail=""):
        print(("  通过 " if ok else "  失败 ") + name + (f"  {detail}" if detail and not ok else ""))
        if not ok:
            failures.append(name)

    try:
        await wait_for(lambda: len(ws_clients) > 0, "acumen 连上假服务端", 40)
        await asyncio.sleep(2.5)

        print("A. 画图表：bash 把图写进临时目录 → 回复里写 ![](路径) → 图嵌进卡片")
        mark, n = len(posts), len(llm_requests)
        await inject("管家大人 画图表")
        await wait_for(lambda: len(creates_since(mark)) >= 1, "收到回复")
        await asyncio.sleep(1.5)
        sent = creates_since(mark)
        system = text_of(llm_requests[n]["messages"][0])
        check("系统提示词里有卡片说明", "![一句说明](地址)" in system and "临时目录" in system)
        check("只发了一条（卡片），没有另发图片", len(sent) == 1, str([s[:80] for s in sent]))
        check("这一条是图片", bool(sent) and ("<img" in sent[0] or "<image" in sent[0] or "base64://" in sent[0]), sent[0][:120] if sent else "")
        check("模型只被问了两次", len(llm_requests) - n == 2, str(len(llm_requests) - n))
        runs = os.path.join(WORK, "data", "oai", "runs")
        check("临时目录已清理", not os.path.isdir(runs) or not os.listdir(runs), str(os.listdir(runs)) if os.path.isdir(runs) else "")
        card = re.search(r"base64://([A-Za-z0-9+/=]+)", sent[0]) if sent else None
        if card:
            open(os.path.join(WORK, "cardA.jpg"), "wb").write(base64.b64decode(card.group(1)))
            print("     卡片已存到", os.path.join(WORK, "cardA.jpg"))

        print("B. 坏图：第一次写了不存在的路径 → 被提醒一次 → 改成不放图")
        mark, n = len(posts), len(llm_requests)
        await inject("管家大人 坏图")
        await wait_for(lambda: len(creates_since(mark)) >= 1, "收到回复")
        await asyncio.sleep(1.0)
        sent = creates_since(mark)
        check("模型被问了两次（含一次改错提醒）", len(llm_requests) - n == 2, str(len(llm_requests) - n))
        nudge = text_of([m for m in llm_requests[n + 1]["messages"] if m["role"] == "user"][-1]) if len(llm_requests) - n >= 2 else ""
        check("提醒里写了路径与原因", "/nope/none.png" in nudge and "找不到文件" in nudge, nudge[:200])
        check("最终回复是改好的文本", len(sent) == 1 and "改好了" in sent[0], str(sent))

        print("C. 死图：改一次仍是坏的 → 不再磨，卡片里留占位")
        mark, n = len(posts), len(llm_requests)
        await inject("管家大人 死图")
        await wait_for(lambda: len(creates_since(mark)) >= 1, "收到回复")
        await asyncio.sleep(1.5)
        sent = creates_since(mark)
        check("模型被问了两次（只提醒一次）", len(llm_requests) - n == 2, str(len(llm_requests) - n))
        check("仍然发出了一张卡片", len(sent) == 1 and ("<img" in sent[0] or "<image" in sent[0] or "base64://" in sent[0]), str([s[:80] for s in sent]))

        print("D. 纯文字：不受影响")
        mark, n = len(posts), len(llm_requests)
        await inject("管家大人 随便聊聊")
        await wait_for(lambda: len(creates_since(mark)) >= 1, "收到回复")
        sent = creates_since(mark)
        check("纯文字回复", len(sent) == 1 and "你好，这是一句话" in sent[0], str(sent))
        check("只问了一次模型", len(llm_requests) - n == 1)
    except AssertionError as error:
        print("  失败 ", error)
        failures.append(str(error)[:80])
    finally:
        child.send_signal(signal.SIGTERM)
        try:
            child.wait(timeout=10)
        except subprocess.TimeoutExpired:
            child.kill()
        for runner in runners:
            await runner.cleanup()
        log.close()

    logtext = open(os.path.join(WORK, "run.log"), errors="replace").read()
    clean = lambda l: re.sub(r"\x1b\[[0-9;]*m", "", l)[:220]
    for tag in ("ERRO", "WARN"):
        lines = [clean(l) for l in logtext.splitlines() if tag in l]
        print(f"\n日志 {tag} {len(lines)} 条")
        for l in lines[:12]:
            print("   ", l)
    if "panicked" in logtext:
        failures.append("panic")
    print("\n结果：" + ("全部通过" if not failures else f"{len(failures)} 项失败：{failures}"))
    sys.exit(1 if failures else 0)


asyncio.run(main())
