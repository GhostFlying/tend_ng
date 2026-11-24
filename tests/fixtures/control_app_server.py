"""Hold one native turn for steer and stop, then accept the explicitly queued text."""

import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
secondary = (root / "interrupted").exists()
thread = "fixture-second-thread" if secondary else "fixture-original-thread"
turn = "fixture-second-turn" if secondary else "fixture-original-turn"


def read(method):
    value = json.loads(sys.stdin.readline())
    assert value["method"] == method, value
    return value


def send(payload):
    print(json.dumps(payload), flush=True)


def respond(request, result):
    send({"id": request["id"], "result": result})


respond(read("initialize"), {})
read("initialized")
respond(read("thread/start"), {"thread": {"id": thread}})
request = read("turn/start")
respond(request, {"turn": {"id": turn, "status": "inProgress", "items": []}})

if secondary:
    payload = json.loads(request["params"]["input"][0]["text"].split("\n", 1)[1])
    [message] = payload["child_inputs"]
    assert message["message"]["text"] == "Queue the unsent correction."
    (root / "queued-message.json").write_text(json.dumps(message))
else:
    steer = read("turn/steer")
    args = steer["params"]
    assert args["threadId"] == thread and args["expectedTurnId"] == turn
    assert args["clientUserMessageId"] == "live-steer"
    assert args["input"][0]["text"] == "Use the exact active turn."
    respond(steer, {"turnId": turn})
    stop = read("turn/interrupt")
    assert stop["params"] == {"threadId": thread, "turnId": turn}
    (root / "interrupted").touch()
    respond(stop, {})

send({"method": "turn/completed", "params": {"threadId": thread, "turn": {
    "id": turn, "status": "completed" if secondary else "interrupted", "items": [],
}}})
