#!/usr/bin/env python3
"""群聊搭话（ambient）的端到端：假 Satori 实现端 + 脚本化的假模型，拉起隔离的 acumen。

覆盖搭话够得着窗口之外的三条路：
  1. 引用的那条消息早已滚出窗口：向平台 `message.get` 要回原话，摆进记录里；引到的是机器人自己
     那条，等于点了名，不 @ 也被叫醒。
  2. 群友扔进群的日志与图片：`satori_read` 的 save=true 经实现端的资源代理取回本轮工作目录，
     `read` 读得到文本，`view_image` 看得到图；翻不到地址或超过体积的只记在 skipped 里。
  3. 「之前谁说过」：`satori_history` 翻本机记录库（需要 recorder 在记），按关键词、发言人、
     围着某条看前后文；翻到的原文也算眼前，不被翻旧账守卫拦下。
假模型按提问里的关键词走脚本，不需要任何真实密钥。

用法（需要 `pip install aiohttp`）：
    cargo build --release --locked
    python3 tests/e2e_ambient.py target/release/acumen [工作目录]
"""
import asyncio, base64, json, os, re, shutil, struct, subprocess, sys, tempfile, time, zlib
from aiohttp import web

BIN = os.path.abspath(sys.argv[1])
WORK = os.path.abspath(sys.argv[2]) if len(sys.argv) > 2 else os.path.join(tempfile.gettempdir(), "acumen-e2e-ambient")
SATORI_PORT, LLM_PORT = 39129, 39130
GROUP, BOT = "900002", "20002"
LOGIN = {"platform": "red", "adapter": "satori-qq", "status": 1, "user": {"id": BOT, "name": "测试机器人"}, "features": []}
LOG_TEXT = "ERROR: no space left on device\nhint: free some disk space\n"


def tiny_png(w=64, h=40, rgb=(60, 120, 100)):
    raw = b"".join(b"\x00" + bytes(rgb) * w for _ in range(h))

    def chunk(t, d):
        c = struct.pack(">I", len(d)) + t + d
        return c + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")


PNG = tiny_png(300, 300)  # 够大：太小的图会被当成坏图
FILES = {  # 资源代理上放着什么：路径尾巴 → 字节
    "log.file": LOG_TEXT.encode(),
    "pic.image": PNG,
    "huge.file": b"x" * (21 * 1024 * 1024),
}

posts, ws_clients, llm_requests = [], [], []
stored = {}  # message.get 能查到的旧消息：id → 消息体
sn, next_msg = [0], [7000]


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


async def proxy(request):
    tail = request.match_info["rest"].rsplit("/", 1)[-1]
    if tail not in FILES:
        return web.Response(status=404)
    return web.Response(body=FILES[tail])


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
    if method == "message.get":
        found = stored.get(str(body.get("message_id")))
        if found:
            return web.json_response(found)
        return web.json_response({"code": "not_found", "message": "no such message"}, status=404)
    if method in ("guild.list", "channel.list", "guild.member.list", "message.list"):
        return web.json_response({"data": []})
    if method == "login.get":
        return web.json_response(LOGIN)
    return web.json_response({"code": "not_found", "message": method}, status=404)


def old_message(mid, user, name, content):
    return {"id": mid, "content": content, "channel": {"id": GROUP, "type": 0}, "guild": {"id": GROUP},
            "user": {"id": user, "name": name}, "member": {"nick": name}, "created_at": int(time.time() * 1000) - 7200_000}


async def inject(content, user="10001", name="群友甲", mid=None):
    sn[0] += 1
    mid = mid or fresh_id()
    body = {"sn": sn[0], "type": "message-created", "timestamp": int(time.time() * 1000), "login": LOGIN,
            "channel": {"id": GROUP, "type": 0}, "guild": {"id": GROUP, "name": "测试群"},
            "user": {"id": user, "name": name}, "member": {"nick": name},
            "message": {"id": mid, "content": content}}
    for ws in ws_clients:
        await ws.send_json({"op": 0, "body": body})
    return mid


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


def is_speak(body):
    system = text_of(body["messages"][0]) if body["messages"] and body["messages"][0]["role"] == "system" else ""
    return "你在一个 QQ 群里，作为群成员之一说话" in system


async def chat(request):
    body = await request.json()
    llm_requests.append(body)
    msgs = body["messages"]
    if not is_speak(body):  # 复盘、判定之类：这个夹具里不该有，给个沉默的答复
        return completion({"role": "assistant", "content": '{"score":0,"reason":"测试"}'})
    first_user = text_of([m for m in msgs if m["role"] == "user"][0])
    tool_results = [text_of(m) for m in msgs if m["role"] == "tool"]
    tail = msgs[-1]

    def call(name, **args):
        return {"id": f"call_{len(tool_results)}_{name}", "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}

    def tools(*calls):
        return completion({"role": "assistant", "content": None, "tool_calls": list(calls)})

    def say(text):
        return completion({"role": "assistant", "content": text})

    # 提示词里带着前几轮的记录：按最后出现的那个暗号走脚本。
    scene = max(("翻记录", "看日志", "看图片", "太大了"), key=first_user.rfind)
    if first_user.rfind(scene) < 0:
        scene = ""
    if scene == "翻记录":
        if len(tool_results) == 0:
            return tools(call("satori_history", keyword="Magisk"))
        if len(tool_results) == 1:
            hit = re.search(r'"id":\s*"(\d+)"', tool_results[0])
            return tools(call("satori_history", around=hit.group(1) if hit else "0"))
        if len(tool_results) == 2:
            return tools(call("satori_history"))  # 什么条件都没给：该被拒绝
        return say("上次有人提过 Magisk 27.0")  # 带「上次」字眼：翻过记录，守卫不该拦
    if scene == "看日志":
        target = LOG_MESSAGE[0]
        if len(tool_results) == 0:
            return tools(call("satori_read", message_id=target, save=True))
        if len(tool_results) == 1:
            path = re.search(r'"path":\s*"([^"]*error\.log)"', tool_results[0].replace("\\\\", "\\"))
            return tools(call("read", path=path.group(1) if path else "/nope"))
        return say("日志里写的是：" + ("磁盘满了" if "no space left" in tool_results[-1] else "没读到"))
    if scene == "看图片":
        target = PIC_MESSAGE[0]
        if len(tool_results) == 0:
            return tools(call("satori_read", message_id=target, save=True))
        if len(tool_results) == 1:
            path = re.search(r'"path":\s*"([^"]*\.png)"', tool_results[0])
            return tools(call("view_image", source=path.group(1) if path else "/nope"))
        parts = tail.get("content") if isinstance(tail.get("content"), list) else []
        if tail["role"] == "user" and any(p.get("type") == "image_url" for p in parts if isinstance(p, dict)):
            return say("看到图了")
        return say("没收到图")
    if scene == "太大了":
        if len(tool_results) == 0:
            return tools(call("satori_read", message_id=HUGE_MESSAGE[0], save=True))
        skipped = re.search(r'"skipped":\s*\[(.*?)\]', tool_results[0])
        return say("结果：" + (skipped.group(1) if skipped else tool_results[0][:300]))
    return say("好")


LOG_MESSAGE, PIC_MESSAGE, HUGE_MESSAGE = [""], [""], [""]


def creates_since(mark):
    return [b.get("content", "") for m, b in posts[mark:] if m == "message.create"]


async def wait_for(pred, what, timeout=60.0):
    end = time.time() + timeout
    while time.time() < end:
        if pred():
            return
        await asyncio.sleep(0.2)
    raise AssertionError(f"超时：{what}\n最近 POST：{[(m, json.dumps(b, ensure_ascii=False)[:200]) for m, b in posts][-6:]}")


def speak_requests(since):
    return [b for b in llm_requests[since:] if is_speak(b)]


async def main():
    shutil.rmtree(WORK, ignore_errors=True)
    os.makedirs(os.path.join(WORK, "data", "oai"))
    shutil.copy(BIN, os.path.join(WORK, "acumen"))
    json.dump({"api_base": f"http://127.0.0.1:{LLM_PORT}/v1", "api_key": "k", "default_model": "scripted"},
              open(os.path.join(WORK, "data", "oai", "config.json"), "w"))
    others = re.findall(r"^    ([a-z_]+) \{", open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "../src/plugins/registry.rs")).read(), re.M)
    off = "".join(f"[{n}]\nenabled = false\n\n" for n in others if n not in ("oai", "ambient", "recorder", "meta_filter"))
    open(os.path.join(WORK, "config.toml"), "w").write(f'''command_prefix = ["/"]

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
request_timeout_seconds = 30

[oai.providers.fake]
api_base = "http://127.0.0.1:{LLM_PORT}/v1"
api_key = "k"

[oai.search]
enabled = false

[recorder]
enabled = true

[ambient]
enabled = true
groups = ["{GROUP}"]
gate_model = "fake/scripted"
reply_model = "fake/scripted"
search_enabled = false
debounce_seconds = 1
think_seconds = 0.0
typing_cpm = 60000
voice_cpm = 60000
owner_grace_seconds = 0
send_freshness_seconds = 0
mood_enabled = false
reflect_enabled = false

''' + off)

    app = web.Application(client_max_size=64 * 1024 * 1024)
    app.router.add_get("/v1/events", events_ws)
    app.router.add_get("/v1/proxy/{rest:.*}", proxy)
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
        await asyncio.sleep(3.0)

        print("A. 引用滚出窗口的旧消息：向平台要原话；引到机器人自己那条就算点了名")
        stored["5001"] = old_message("5001", BOT, "测试机器人", "把 Magisk 降到 26.4 再试试")
        stored["5002"] = old_message("5002", "10009", "老群友", "这个周末聚餐去哪家")
        mark, n = len(posts), len(llm_requests)
        await inject('<quote id="5001"/>这个咋整', mid="5101")
        await wait_for(lambda: len(speak_requests(n)) >= 1, "引用机器人旧消息后被叫醒")
        prompt = text_of([m for m in speak_requests(n)[0]["messages"] if m["role"] == "user"][0])
        check("记录里带着被引原话", "〔引用 把 Magisk 降到 26.4 再试试〕" in prompt, prompt[-300:])
        check("标出了「引用了你的消息」", "〔引用了你的消息〕" in prompt, prompt[-300:])
        await wait_for(lambda: len(creates_since(mark)) >= 1, "收到回复")
        mark, n = len(posts), len(llm_requests)
        await inject('<quote id="5002"/>确实', user="10003", name="群友乙", mid="5102")
        await asyncio.sleep(4.0)
        check("引别人的旧消息不算点名：没叫醒它", len(speak_requests(n)) == 0 and len(creates_since(mark)) == 0)
        mark, n = len(posts), len(llm_requests)
        await inject(f'<at id="{BOT}"/>接着上面那句，说说看', user="10003", name="群友乙", mid="5103")
        await wait_for(lambda: len(speak_requests(n)) >= 1, "@ 之后被叫醒")
        prompt = text_of([m for m in speak_requests(n)[0]["messages"] if m["role"] == "user"][0])
        check("引别人旧消息的那条，摆出了作者与原话", "〔引用 老群友：这个周末聚餐去哪家〕" in prompt, prompt[-500:])
        await wait_for(lambda: len(creates_since(mark)) >= 1, "收到回复")

        print("A2. 引用一张滚出窗口的旧图：图随提示词一起递给模型，出处标成被引那条")
        stored["5003"] = old_message("5003", "10010", "老群友乙", '<img src="internal:red/20002/_tmp/pic.image"/>')
        mark, n = len(posts), len(llm_requests)
        await inject(f'<quote id="5003"/><at id="{BOT}"/> 这图是啥', user="10003", name="群友乙", mid="5104")
        await wait_for(lambda: len(speak_requests(n)) >= 1, "引用旧图 @ 之后被叫醒")
        first = [m for m in speak_requests(n)[0]["messages"] if m["role"] == "user"][0]
        parts = first["content"] if isinstance(first.get("content"), list) else []
        check("被引的旧图随提示词附上了", any(p.get("type") == "image_url" for p in parts if isinstance(p, dict)), str(parts)[:200])
        check("出处写着被引那条的编号", "id=5003 的第 1 张" in text_of(first), text_of(first)[-300:])
        await wait_for(lambda: len(creates_since(mark)) >= 1, "收到回复")

        print("B. 取附件：日志 → read 读得到；图片 → view_image 看得到；超大的只记 skipped")
        mark, n = len(posts), len(llm_requests)
        LOG_MESSAGE[0] = await inject('<file src="internal:red/20002/_tmp/log.file" title="error.log" file-size="52"/>', user="10004", name="群友丙")
        PIC_MESSAGE[0] = await inject('<img src="internal:red/20002/_tmp/pic.image"/>', user="10004", name="群友丙")
        HUGE_MESSAGE[0] = await inject('<file src="internal:red/20002/_tmp/huge.file" title="big.bin"/>', user="10004", name="群友丙")
        await asyncio.sleep(1.0)
        await inject(f'<at id="{BOT}"/> 帮我看日志', user="10004", name="群友丙")
        await wait_for(lambda: len(creates_since(mark)) >= 1, "日志那一轮回复")
        sent = creates_since(mark)
        check("读到了日志内容并据此作答", any("磁盘满了" in s for s in sent), str(sent))
        reqs = speak_requests(n)
        tools = [t["function"]["name"] for t in reqs[0].get("tools", [])]
        check("工具表里有 satori_read / view_image / satori_history", all(x in tools for x in ("satori_read", "view_image", "satori_history")), str(tools))
        check("提示词交代了窗口之外怎么够", "save=true" in text_of(reqs[0]["messages"][0]) and "satori_history" in text_of(reqs[0]["messages"][0]))
        mark, n = len(posts), len(llm_requests)
        await inject(f'<at id="{BOT}"/> 帮我看图片', user="10004", name="群友丙")
        await wait_for(lambda: len(creates_since(mark)) >= 1, "图片那一轮回复")
        check("模型收到了取回的图", any("看到图了" in s for s in creates_since(mark)), str(creates_since(mark)))
        mark, n = len(posts), len(llm_requests)
        await inject(f'<at id="{BOT}"/> 这个太大了', user="10004", name="群友丙")
        await wait_for(lambda: len(creates_since(mark)) >= 1, "超大文件那一轮回复")
        reply = creates_since(mark)[0]
        check("超过 20 MiB 的不取，原因记在 skipped", "超过" in reply and "MiB" in reply, reply)

        print("C. 翻记录：关键词、围着某条看前后文、没给条件被拒；翻到的原文让「上次」不被守卫拦下")
        await inject("我昨天把 Magisk 升到 27.0 了，没出问题", user="10005", name="群友丁")
        await inject("Delta 面板这周更新了吗", user="10006", name="群友戊")
        await inject("还没更新，等等看", user="10005", name="群友丁")
        await asyncio.sleep(2.0)
        mark, n = len(posts), len(llm_requests)
        await inject(f'<at id="{BOT}"/> 帮我翻记录，之前谁说过 Magisk', user="10007", name="群友己")
        await wait_for(lambda: len(creates_since(mark)) >= 1, "翻记录那一轮回复", 90)
        sent = creates_since(mark)
        check("最终回复发出去了（「上次」没被守卫咽回）", any("上次有人提过 Magisk 27.0" in s for s in sent), str(sent))
        reqs = speak_requests(n)
        results = [text_of(m) for m in reqs[-1]["messages"] if m["role"] == "tool"]
        check("按关键词翻到了原话与发言人", len(results) >= 1 and "Magisk 升到 27.0" in results[0] and "群友丁" in results[0], str(results[:1])[:300])
        check("围着那条看前后文，带出了相邻的话", len(results) >= 2 and "Delta 面板" in results[1], str(results[1:2])[:300])
        check("什么条件都不给被拒绝", len(results) >= 3 and "给个关键词或发言人" in results[2], str(results[2:3])[:300])
        check("提示翻不到就是不知道", len(results) >= 1 and "别凭印象补" in results[0])
        check("每条原话都带着「是不是你自己说的」标记", all('"self"' in r for r in results[:2]))
    except Exception as error:
        failures.append(str(error))
        print("  异常：", error)
    finally:
        child.terminate()
        try:
            child.wait(10)
        except Exception:
            child.kill()
        for runner in runners:
            await runner.cleanup()
    if failures:
        print(f"\n失败 {len(failures)} 项；日志在 {os.path.join(WORK, 'run.log')}")
        sys.exit(1)
    print("\n全部通过")


asyncio.run(main())
