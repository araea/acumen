#!/usr/bin/env python3
"""内置 agent 房间（管家大人）的端到端：假 Satori 实现端 + 脚本化的假模型，拉起隔离的 acumen。

覆盖回复里的内嵌图片：模型用 bash 把图写进本轮临时目录、回复里写 `![说明](路径)`，
图应当按位置画进同一张卡片（而不是另发一条）；写了不存在的路径时只被提醒一次；
纯文字回复不受影响；`view_image` 读到的图要随下一条用户消息送回模型。另覆盖执行层的长任务能力：
同一批里的 `delegate` 并行、子助手拿不到 delegate 与群聊工具、步数用尽时最后一步收回工具并催收尾、
同一调用重复执行被驳回、空回复被催一次、系统提示词里有今天的日期。假模型按提问里的关键词走脚本，不需要任何真实密钥。

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
child_tools = []
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
    # 房间历史会带着前几轮：只看最近那句真正的提问（跳过「改错提醒」、系统提醒与附图的那条）。
    asked_at = max(i for i, m in enumerate(msgs) if m["role"] == "user" and "没能嵌进卡片" not in text_of(m) and "view_image 读到的图" not in text_of(m) and "系统提醒，用户看不到" not in text_of(m))
    asked = text_of(msgs[asked_at])
    last = text_of([m for m in msgs if m["role"] == "user"][-1])
    tools_done = any(m["role"] == "tool" for m in msgs[asked_at:])
    tool_results = [text_of(m) for m in msgs[asked_at:] if m["role"] == "tool"]

    def call(name, **args):
        return {"id": f"call_{len(tool_results)}_{name}", "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}

    if "被委派来完成一件子任务" in system:  # 子助手：慢一点，好看出并行
        child_tools.append([t["function"]["name"] for t in body.get("tools", [])])
        await asyncio.sleep(1.5)
        return completion({"role": "assistant", "content": f"报告：{asked.strip()} 已查完"})
    if "委派调研" in asked:
        if not tools_done:
            return completion({"role": "assistant", "content": None, "tool_calls": [call("delegate", task=f"查第{i}件事") for i in (1, 2)]})
        return completion({"role": "assistant", "content": "汇总：" + "；".join(tool_results)})
    if "无尽" in asked:
        if body.get("tool_choice") == "none":
            return completion({"role": "assistant", "content": f"收尾：只做了 {len(tool_results)} 步，剩下的没来得及。"})
        return completion({"role": "assistant", "content": None, "tool_calls": [call("bash", command=f"echo step{len(tool_results)}")]})
    if "大回执" in asked:  # 每步一份 3 万字的回执：八份就超过压缩预算
        if len(tool_results) >= 8:
            return completion({"role": "assistant", "content": "读完了。"})
        return completion({"role": "assistant", "content": None, "tool_calls": [call("bash", command=f"python3 -c \"print('字' * 30000)\" # {len(tool_results)}")]})
    if "拖沓" in asked:  # 每一步都慢：软期限一到就该收尾，而不是等硬超时丢掉整轮
        if body.get("tool_choice") == "none":
            return completion({"role": "assistant", "content": f"时间到，先交这些：做了 {len(tool_results)} 步。"})
        await asyncio.sleep(4)
        return completion({"role": "assistant", "content": None, "tool_calls": [call("bash", command=f"echo slow{len(tool_results)}")]})
    if "重复" in asked:
        if tool_results and "完全相同的参数" in tool_results[-1]:
            return completion({"role": "assistant", "content": "不再重复了。"})
        return completion({"role": "assistant", "content": None, "tool_calls": [call("bash", command="echo same")]})
    if "空回复" in asked:
        if "你刚才没有给出任何回复" in last:
            return completion({"role": "assistant", "content": "补上了。"})
        if "空消息" in asked:  # 连思考块都没有：接口层面的空回复
            return completion({"role": "assistant", "content": ""})
        return completion({"role": "assistant", "content": "", "reasoning_content": "想了想"})

    if "画图表" in asked:
        if not tools_done:
            cmd = f"printf '%s' {PNG_B64} | base64 -d > {scratch}/c.png && ls -l {scratch}/c.png"
            return completion({"role": "assistant", "content": None, "tool_calls": [
                {"id": "call_1", "type": "function", "function": {"name": "bash", "arguments": json.dumps({"command": cmd})}}]})
        return completion({"role": "assistant", "content": f"好的，图表如下：\n\n![本周走势]({scratch}/c.png)\n\n以上。"})
    if "看图" in asked:
        done = sum(1 for m in msgs[asked_at:] if m["role"] == "tool")
        if done == 0:
            cmd = f"printf '%s' {PNG_B64} | base64 -d > {scratch}/v.png"
            return completion({"role": "assistant", "content": None, "tool_calls": [
                {"id": "call_a", "type": "function", "function": {"name": "bash", "arguments": json.dumps({"command": cmd})}}]})
        if done == 1:
            return completion({"role": "assistant", "content": None, "tool_calls": [
                {"id": "call_b", "type": "function", "function": {"name": "view_image", "arguments": json.dumps({"source": f"{scratch}/v.png"})}}]})
        # 工具回执之后应当跟着一条带图的用户消息。
        tail = msgs[-1]
        parts = tail.get("content") if isinstance(tail.get("content"), list) else []
        if tail["role"] == "user" and any(p.get("type") == "image_url" for p in parts if isinstance(p, dict)):
            return completion({"role": "assistant", "content": "我看到了那张图。"})
        return completion({"role": "assistant", "content": "没有收到图。"})
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
request_timeout_seconds = 30

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

        print("E. 看图：view_image 读到的图附在下一条用户消息里")
        mark, n = len(posts), len(llm_requests)
        await inject("管家大人 看图")
        await wait_for(lambda: len(creates_since(mark)) >= 1, "收到回复")
        sent = creates_since(mark)
        check("模型确实收到了图", len(sent) == 1 and "我看到了那张图" in sent[0], str(sent))
        check("模型被问了三次（写图、看图、答）", len(llm_requests) - n == 3, str(len(llm_requests) - n))
        tools = [t["function"]["name"] for t in llm_requests[n].get("tools", [])]
        check("工具表里有 view_image", "view_image" in tools, str(tools))
        check("提示词建议先看一眼", "view_image 自己看一眼" in text_of(llm_requests[n]["messages"][0]))

        print("F. 委派：同一次回复里的两个 delegate 并行，子助手没有 delegate 与群聊工具")
        mark, n = len(posts), len(llm_requests)
        child_tools.clear()
        started = time.time()
        await inject("管家大人 委派调研")
        await wait_for(lambda: len(creates_since(mark)) >= 1, "收到回复")
        took = time.time() - started
        sent = creates_since(mark)
        check("汇总里带着两份子助手的报告", len(sent) == 1 and "报告：查第1件事 已查完" in sent[0] and "报告：查第2件事 已查完" in sent[0], str(sent))
        check("两个子助手并行跑（各慢 1.5 秒，总共不到 2.9 秒）", took < 2.9, f"{took:.1f} 秒")
        check("模型共被问了四次（主 2 + 子 2）", len(llm_requests) - n == 4, str(len(llm_requests) - n))
        main_tools = [t["function"]["name"] for t in llm_requests[n].get("tools", [])]
        check("主助手工具表里有 delegate", "delegate" in main_tools, str(main_tools))
        check("提示词里有委派说明", "delegate 交给子助手" in text_of(llm_requests[n]["messages"][0]))
        check("子助手没有 delegate 与 satori_*", len(child_tools) == 2 and all("delegate" not in t and not any(x.startswith("satori_") for x in t) and "bash" in t for t in child_tools), str(child_tools))

        print("G. 步数用尽：倒数第二步提醒，最后一步收回工具，回复照样发出而不是整轮报错")
        mark, n = len(posts), len(llm_requests)
        await inject("管家大人 无尽")
        await wait_for(lambda: len(creates_since(mark)) >= 1, "收到回复", 90)
        sent = creates_since(mark)
        check("发出的是收尾回复", len(sent) == 1 and "收尾：只做了 23 步" in sent[0], str(sent))
        check("模型共被问了 24 次", len(llm_requests) - n == 24, str(len(llm_requests) - n))
        check("最后一次请求禁用了工具", llm_requests[-1].get("tool_choice") == "none" and all(r.get("tool_choice") != "none" for r in llm_requests[n:-1]))
        users = lambda r: [text_of(m) for m in r["messages"] if m["role"] == "user"]
        check("倒数第二次请求带着「次数快用完」", any("次数快用完" in t for t in users(llm_requests[-2])) and not any("次数快用完" in t for t in users(llm_requests[-3])))
        check("最后一次请求带着「最后一次回复」", any("最后一次回复" in t for t in users(llm_requests[-1])))

        print("L. 上下文瘦身：较早的大回执被压成头尾，最近几份原样")
        mark, n = len(posts), len(llm_requests)
        await inject("管家大人 大回执")
        await wait_for(lambda: len(creates_since(mark)) >= 1, "收到回复", 60)
        tools_in = [text_of(m) for m in llm_requests[-1]["messages"] if m["role"] == "tool"]
        check("最后一次请求带着八份回执", len(tools_in) == 8, str(len(tools_in)))
        check("最老的几份已压缩、写明原长", tools_in[0].count("已压缩：原") == 1 and len(tools_in[0]) < 1_400, tools_in[0][:60])
        check("最近四份原样", all(len(t) >= 29_000 and "已压缩" not in t for t in tools_in[-4:]), str([len(t) for t in tools_in]))
        total = sum(len(text_of(m)) for m in llm_requests[-1]["messages"])
        check("整条请求落回预算附近（16 万字符上下）", total < 175_000, str(total))

        print("K. 时间将尽：软期限一到就收尾，赶在硬超时（30 秒）之前交回已有的东西")
        mark, n = len(posts), len(llm_requests)
        started = time.time()
        await inject("管家大人 拖沓")
        await wait_for(lambda: len(creates_since(mark)) >= 1, "收到回复", 60)
        took = time.time() - started
        sent = creates_since(mark)
        check("发出的是收尾回复而不是「请求超时」", len(sent) == 1 and "时间到，先交这些" in sent[0], str(sent))
        check("赶在硬超时之前", took < 28, f"{took:.1f} 秒")
        check("远没到步数上限", len(llm_requests) - n < 12, str(len(llm_requests) - n))
        check("最后一次请求禁用了工具", llm_requests[-1].get("tool_choice") == "none")

        print("H. 重复调用：同样的命令执行满 4 次后驳回")
        mark, n = len(posts), len(llm_requests)
        await inject("管家大人 重复")
        await wait_for(lambda: len(creates_since(mark)) >= 1, "收到回复", 60)
        sent = creates_since(mark)
        check("最终回复", len(sent) == 1 and "不再重复了" in sent[0], str(sent))
        check("模型共被问了 6 次（4 次执行 + 1 次被驳回 + 答）", len(llm_requests) - n == 6, str(len(llm_requests) - n))

        print("I. 空回复：被催一次，而不是整轮报错")
        mark, n = len(posts), len(llm_requests)
        await inject("管家大人 空回复")
        await wait_for(lambda: len(creates_since(mark)) >= 1, "收到回复", 60)
        sent = creates_since(mark)
        check("补上了回复", len(sent) == 1 and "补上了" in sent[0], str(sent))
        check("模型被问了两次", len(llm_requests) - n == 2, str(len(llm_requests) - n))

        mark, n = len(posts), len(llm_requests)
        await inject("管家大人 空回复 空消息")
        await wait_for(lambda: len(creates_since(mark)) >= 1, "收到回复", 60)
        sent = creates_since(mark)
        check("连思考块都没有的空消息也被催了一次", len(sent) == 1 and "补上了" in sent[0] and len(llm_requests) - n == 2, f"{sent} / {len(llm_requests) - n}")

        print("J. 系统提示词里有今天的日期")
        from datetime import datetime, timedelta, timezone
        now = datetime.now(timezone(timedelta(hours=8)))
        want = f"今天是 {now.year}年{now.month}月{now.day}日 星期{'一二三四五六日'[now.weekday()]}（北京时间）"
        check("日期与星期对得上", want in text_of(llm_requests[n]["messages"][0]), want)

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
