"""Fixed native callbacks for a parent, its work child, and the resumed parent."""
import json
from pathlib import Path
import sys
import time


def request(method):
    call = json.loads(sys.stdin.readline())
    assert call["method"] == method, call
    return call


def send(payload):
    print(json.dumps(payload), flush=True)


def respond(call, result):
    send({"id": call["id"], "result": result})


def tool(name, arguments, rpc_id, call_id):
    send({
        "id": rpc_id, "method": "item/tool/call",
        "params": {"threadId": thread_id, "turnId": turn_id,
                   "callId": call_id, "tool": name, "arguments": arguments},
    })
    response = json.loads(sys.stdin.readline())
    assert response["id"] == rpc_id and response["result"]["success"], response
    return json.loads(response["result"]["contentItems"][0]["text"])


def gate(name):
    deadline = time.monotonic() + 20
    while not (root / name).exists():
        assert time.monotonic() < deadline, name
        time.sleep(.02)


root = Path(sys.argv[1])
scenario = sys.argv[2]
respond(request("initialize"), {})
request("initialized")
start = request("thread/start")
model = start["params"]["model"]
worker = model == "fixture-worker"
role = "child" if worker else "parent"
if not worker and (root / "delegation.json").exists():
    role = "resumed"
thread_id = f"fixture-{role}-thread"
turn_id = f"fixture-{role}-turn"
assert Path(start["params"]["cwd"]) == Path.cwd()
respond(start, {"thread": {"id": thread_id}})
turn = request("turn/start")
items = turn["params"]["input"]
assignment = json.loads(items[-1]["text"])
tools = {spec["name"] for spec in start["params"]["dynamicTools"]}
respond(turn, {"turn": {"id": turn_id, "status": "inProgress", "items": []}})

if role == "parent":
    assert tools == {"tend_delegate_work", "tend_close_issue"}
    assert turn["params"]["effort"] == "low"
    assert len(items) == 1
    gate("delegate-now")
    args = {"title": "Write the guide", "objective": "Add a one-line guide",
            "category": "implementation", "required_capabilities": ["image_gen"]}
    if scenario == "no-candidate":
        args["required_capabilities"] = ["missing-capability"]
    delegated = tool("tend_delegate_work", args, "delegate", "original-delegate")
    assert tool("tend_delegate_work", args, "replay-delegate", "original-delegate") == delegated
    (root / "delegation.json").write_text(json.dumps(delegated))
elif role == "child":
    assert tools == {"tend_close_issue"}
    assert assignment["placement"]["agent"]["id"] == "worker"
    assert Path.cwd() == root / "runner.runs" / "workspaces" / assignment["parent_issue_id"]
    assert (Path("repos/example") / ".git").is_file()
    assert (Path("repos/example") / "README.md").read_text() == "A fixed repository.\n"
    (Path("repos/example") / "guide.txt").write_text("A fixture guide.\n")
    if scenario == "explicit-close":
        tool("tend_close_issue", {"summary": "The requested guide is ready."}, "close-child", "child-close")
else:
    assert tools == {"tend_delegate_work", "tend_close_issue"}
    delegated = json.loads((root / "delegation.json").read_text())
    assert assignment["placement"]["assignment_id"] == delegated["assignment_id"]
    context = json.loads(items[0]["text"].split("\n", 1)[1])
    assert context["previous_delegations"] == [delegated]
    [result] = context["child_inputs"]
    assert result["child_issue_id"] == delegated["action"]["issue_id"]
    assert result["child_task_id"] == delegated["action"]["task_id"]
    if scenario == "explicit-close":
        assert result["kind"] == "issue_closed"
        assert result["summary"] == "The requested guide is ready."
    elif scenario == "no-candidate":
        assert result["kind"] == "no_candidate"
        assert result["required_capabilities"] == ["missing-capability"]
    else:
        assert result["kind"] == "task_stopped"
        assert result["outcome"] == "completed"
    tool("tend_close_issue", {"summary": result["summary"]}, "close-parent", "parent-close")

send({
    "method": "turn/completed",
    "params": {"threadId": thread_id, "turn": {"id": turn_id, "status": "completed", "items": []}},
})
