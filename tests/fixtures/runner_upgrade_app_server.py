"""A Router and four controllable Runner turns for the packaged upgrade fixture."""

import json
from pathlib import Path
import sys
import time
import uuid

target = Path(sys.argv[1])
router = not target.is_dir()
root = target.parent if router else target


def send(value):
    print(json.dumps(value), flush=True)


def read():
    return json.loads(sys.stdin.readline())


def respond(call, result):
    send({"id": call["id"], "result": result})


def tool(thread, turn, name, arguments):
    send({"id": "tool", "method": "item/tool/call", "params": {
        "threadId": thread, "turnId": turn, "callId": name, "tool": name, "arguments": arguments,
    }})
    reply = read()
    assert reply["id"] == "tool" and reply["result"]["success"], reply
    return json.loads(reply["result"]["contentItems"][0]["text"])


def gate(name):
    deadline = time.monotonic() + 100
    while not (root / f"{name}.release").exists():
        assert time.monotonic() < deadline, name
        time.sleep(0.025)


for line in sys.stdin:
    call = json.loads(line)
    params = call.get("params", {})
    match call["method"]:
        case "initialize":
            respond(call, {})
        case "initialized":
            continue
        case "thread/start":
            respond(call, {"thread": {"id": str(uuid.uuid4())}})
        case "thread/resume":
            respond(call, {"thread": {"id": params["threadId"], "status": {"type": "idle"}}})
        case "thread/turns/list":
            history = root / f"{params['threadId']}.json"
            turns = json.loads(history.read_text()) if history.exists() else []
            respond(call, {"data": turns[:params.get("limit", len(turns))], "nextCursor": None})
        case "turn/start":
            thread = params["threadId"]
            turn = str(uuid.uuid4())
            run = params["clientUserMessageId"]
            items = params["input"]
            respond(call, {"turn": {"id": turn, "status": "inProgress", "items": []}})
            if router:
                name = items[-1]["text"].removeprefix("Upgrade fixture: ")
                assert name in {"original", "laggard", "overlap-a", "overlap-b"}, name
                receipt = tool(thread, turn, "tend_dispatch_issue", {
                    "title": name, "objective": name, "repo_ids": ["example"],
                })
                (root / f"{name}.dispatch").write_text(json.dumps(receipt))
            else:
                assignment = json.loads(items[-1]["text"])
                name = assignment["work"]["title"]
                cwd = Path.cwd()
                if name == "original" and assignment.get("handoff"):
                    context = json.loads(items[0]["text"].split("\n", 1)[1])
                    [input] = context["child_inputs"]
                    assert input["kind"] == "issue_message"
                    assert input["message"]["text"] == "Continue on the new Runner."
                    assert (cwd / "workspace-proof").read_text() == "old Runner wrote this"
                    name = "continued"
                elif name == "original":
                    (cwd / "workspace-proof").write_text("old Runner wrote this")
                (root / f"{name}.started").write_text(json.dumps({
                    "task": assignment["task_id"], "runner": assignment["placement"]["runner_id"],
                    "cwd": str(cwd), "thread": thread, "turn": turn, "run": run,
                }))
                if name != "continued":
                    gate(name)
                if name != "original":
                    tool(thread, turn, "tend_close_issue", {"summary": "Fixture finished naturally."})
            history = {"id": turn, "status": "completed", "items": [
                {"type": "userMessage", "clientId": run},
                {"type": "agentMessage", "id": f"{turn}-answer", "phase": "final_answer",
                 "text": "Fixture turn completed."},
            ]}
            saved = root / f"{thread}.json"
            previous = json.loads(saved.read_text()) if saved.exists() else []
            saved.write_text(json.dumps([history, *previous]))
            send({"method": "turn/completed", "params": {"threadId": thread, "turn": history}})
            if not router:
                break
        case other:
            raise AssertionError(other)
