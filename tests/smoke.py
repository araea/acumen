#!/usr/bin/env python3
"""一条就够了的冒烟：拉起隔离实例，走一遍「配置 → 流水线 → 指令 → 回执 → 落盘 → 退出」。

不连真平台、不需要第三方库：控制台适配器从 stdin 读消息，把回复打到 stdout。
框架、注册表、配置读写、指令匹配与优雅退出都在这一条路上；其余细节交给单元测试。
构建后运行：

    cargo build --release --locked
    python3 tests/smoke.py target/release/acumen
"""

import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time

BIN = os.path.abspath(sys.argv[1])
WORK = os.path.abspath(sys.argv[2]) if len(sys.argv) > 2 else tempfile.mkdtemp(prefix="acumen-smoke-")
EXE = os.path.join(WORK, "acumen")
TIMEOUT = 25

CONFIG = """command_prefix = ["/"]
bots = []

[ctl]
enabled = true
image_enabled = false

[help]
enabled = true
image_enabled = false

[echo]
enabled = true

[recorder]
enabled = true

[logger]
enabled = false
"""


def wait_for(read, needle, what):
    end = time.time() + TIMEOUT
    while time.time() < end:
        if needle in read():
            return
        if child.poll() is not None:
            raise SystemExit(f"进程提前退出（等待 {what}）\n{read()[-4000:]}")
        time.sleep(0.05)
    raise SystemExit(f"等待超时：{what}\n{read()[-4000:]}")


try:
    os.makedirs(WORK, exist_ok=True)
    shutil.copy2(BIN, EXE)
    with open(os.path.join(WORK, "config.toml"), "w", encoding="utf-8") as file:
        file.write(CONFIG)

    child = subprocess.Popen(
        [EXE, "--console"],
        cwd=WORK,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
        env={**os.environ, "ACUMEN_WAKE_LOCK": "0"},
    )
    output = ""
    read = lambda: output
    # 逐行读会阻塞在 readline，放到线程里，主线程只看已积累的输出。
    import threading
    def pump():
        global output
        for line in child.stdout:
            output += line
    threading.Thread(target=pump, daemon=True).start()

    wait_for(read, "前台控制台已就绪", "控制台就绪")

    child.stdin.write("/echo 你好\n")
    child.stdin.flush()
    wait_for(read, "你好", "echo 回执")
    assert "[Bot Reply]" in output, output[-2000:]

    child.stdin.write("/ctl list\n")
    child.stdin.flush()
    wait_for(read, "插件状态", "ctl 状态")

    child.stdin.write("/ctl set help image_scale 2\n")
    child.stdin.flush()
    wait_for(read, "已保存 help.image_scale", "配置写回")

    child.send_signal(signal.SIGTERM)
    code = child.wait(timeout=TIMEOUT)
    assert code == 0, f"退出码 {code}\n{output[-3000:]}"

    saved = open(os.path.join(WORK, "config.toml"), encoding="utf-8").read()
    assert re.search(r"\[help\][\s\S]*?image_scale = 2\.0", saved), saved
    assert os.path.exists(os.path.join(WORK, "data", "bot.db")), "数据库未建立"
    print("smoke ok：控制台、指令流水线、配置写回、数据库与退出")
finally:
    try:
        if child.poll() is None:
            child.kill()
    except Exception:
        pass
    if len(sys.argv) <= 2:
        shutil.rmtree(WORK, ignore_errors=True)
