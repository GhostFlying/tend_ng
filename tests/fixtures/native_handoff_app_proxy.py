"""Relay a real Codex App Server and drop selected replies in the isolated test."""

import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time

codex, directory, *app_args = sys.argv[1:]
root = Path(directory)
generation = os.environ["TEND_FIXTURE_GENERATION"]
mode = (root / "native-mode").read_text()


def trace(event, **fields):
    with (root / "native-rpc.ndjson").open("a") as out:
        out.write(json.dumps({"generation": generation, "event": event, **fields}) + "\n")


def first(name):
    try:
        (root / name).touch(exist_ok=False)
        return True
    except FileExistsError:
        return False


successor = generation == "successor"
faults = mode == "faults" and successor
if faults and first("startup-failed"):
    trace("startup-failed")
    sys.exit(1)

app = subprocess.Popen([codex, "app-server", "--listen", "stdio://", *app_args],
                       stdin=subprocess.PIPE, stdout=subprocess.PIPE)
calls = {}
turn_start = None


def requests():
    global turn_start
    try:
        for raw in sys.stdin.buffer:
            frame = json.loads(raw)
            method = frame.get("method")
            if method and "id" in frame:
                calls[frame["id"]] = method
                if method == "turn/start":
                    turn_start = frame["id"]
                if method in {"thread/start", "thread/resume", "turn/start", "thread/turns/list"}:
                    trace(method, thread=frame["params"].get("threadId"),
                          client=frame["params"].get("clientUserMessageId"))
            app.stdin.write(raw)
            app.stdin.flush()
    except (BrokenPipeError, ValueError):
        pass
    finally:
        app.stdin.close()


threading.Thread(target=requests, daemon=True).start()
try:
    for raw in app.stdout:
        frame = json.loads(raw)
        method = calls.get(frame.get("id"))
        if successor and mode.startswith("lost-fresh-start") and method == "thread/start" and "result" in frame:
            trace("empty-thread-allocated", thread=frame["result"]["thread"]["id"])
            if first("hold-empty-thread"):
                # Run the real RPC, but give the test a chance to read the
                # allocation fence before simulating a lost response.
                deadline = time.monotonic() + 15
                while not (root / "release-empty-thread").exists():
                    assert time.monotonic() < deadline
                    time.sleep(.02)
                trace("fresh-start-reply-lost")
                break
        if (successor and mode == "stop-frontier-after-active" and method == "thread/resume"
                and first("reported-active")):
            trace("reported-active")
            frame["result"]["thread"]["status"] = {"type": "active"}
            raw = (json.dumps(frame) + "\n").encode()
        if successor and mode.startswith("stop-frontier") and method == "thread/turns/list":
            trace("frontier-unavailable")
            raw = (json.dumps({"id": frame["id"], "error": {
                "code": -32603, "message": "isolated persistent frontier error"}}) + "\n").encode()
        if faults and method == "thread/resume" and first("resume-reply-lost"):
            trace("resume-reply-lost")
            break
        if (faults and method == "thread/turns/list"
                and (root / "turn-reply-lost").exists() and first("history-rpc-failed")):
            trace("history-rpc-failed")
            raw = (json.dumps({"id": frame["id"], "error": {
                "code": -32603, "message": "isolated transient history failure"}}) + "\n").encode()
        if faults and turn_start is not None:
            # Hide all start/identity evidence until the real native turn has
            # finished. Recovery must look up the exact successor Run client.
            if frame.get("method") == "turn/completed":
                trace("turn-reply-lost", turn=frame["params"]["turn"]["id"])
                (root / "turn-reply-lost").touch()
                break
            continue
        sys.stdout.buffer.write(raw)
        sys.stdout.buffer.flush()
finally:
    try:
        app.stdin.close()
        app.wait(timeout=3)
    except subprocess.TimeoutExpired:
        app.kill()
        app.wait()
