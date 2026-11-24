"""A fixed local Issue Agent turn with a controllable completion point."""
import json
from pathlib import Path
import sys
import time


def request(method):
    call = json.loads(sys.stdin.readline())
    assert call["method"] == method, call
    return call


def respond(call, result):
    print(json.dumps({"id": call["id"], "result": result}), flush=True)


root = Path(sys.argv[1])
respond(request("initialize"), {})
request("initialized")
start = request("thread/start")
assert start["params"]["model"] == "fixture-coordinator", start
assert Path(start["params"]["cwd"]) == Path.cwd()
respond(start, {"thread": {"id": "fixture-issue-thread"}})
call = request("turn/start")
assert call["params"]["effort"] == "low", call
assignment = json.loads(call["params"]["input"][0]["text"])
(root / "launched.json").write_text(json.dumps(assignment))
respond(call, {"turn": {"id": "fixture-issue-turn", "status": "inProgress", "items": []}})
deadline = time.monotonic() + 20
while not (root / "finish-issue").exists():
    assert time.monotonic() < deadline, "fixture did not release the Issue Agent"
    time.sleep(.02)
print(json.dumps({
    "method": "turn/completed",
    "params": {
        "threadId": "fixture-issue-thread",
        "turn": {"id": "fixture-issue-turn", "status": "completed", "items": []},
    },
}), flush=True)
