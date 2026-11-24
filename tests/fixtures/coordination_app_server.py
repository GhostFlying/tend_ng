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
attention = scenario in {"attention", "attention-local"}
role = "child" if worker else "parent"
if not worker and (root / "delegation.json").exists():
    role = "resumed"
if worker and attention and (root / "question.json").exists():
    role = "decision"
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
    assert {"tend_delegate_work", "tend_close_issue"} <= tools
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
    if scenario == "late-close":
        (root / "original-parent.json").write_text(json.dumps(assignment))
        tool("tend_close_issue", {"summary": "The main objective is settled."}, "early-close", "early-close")
elif role == "child":
    assert {"tend_close_issue", "tend_submit_artifact"} <= tools
    assert assignment["placement"]["agent"]["id"] == "worker"
    assert Path.cwd() == root / "runner.runs" / "workspaces" / assignment["parent_issue_id"]
    assert (Path("repos/example") / ".git").is_file()
    assert (Path("repos/example") / "README.md").read_text() == "A fixed repository.\n"
    (Path("repos/example") / "guide.txt").write_text("A fixture guide.\n")
    if attention:
        gate("ask-now")
        question = {"question": "使用说明用精简版还是完整版？", "options": ["精简版", "完整版"],
                    "context": "使用说明需要匹配目标读者。", "priority": "normal"}
        receipt = tool("tend_request_attention", question, "ask", "ask-guide")
        assert tool("tend_request_attention", question, "replay-ask", "ask-guide") == receipt
        (root / "question.json").write_text(json.dumps({
            "receipt": receipt, "task_id": assignment["task_id"],
        }))
    elif scenario == "delivery":
        gate("deliver-now")
        guide = Path("repos/example/guide.txt")
        guide.write_bytes(b"A fixture guide.\n" * 32768)
        arguments = {"path": guide.as_posix()}
        delivered = tool("tend_submit_artifact", arguments, "submit", "delivery")
        guide.write_text("A later edit to the workspace file.\n")
        assert tool("tend_submit_artifact", arguments, "replay-submit", "delivery") == delivered
        (root / "submitted.json").write_text(json.dumps(delivered))
    if scenario in {"explicit-close", "late-close", "parent-failed", "delivery"}:
        tool("tend_close_issue", {"summary": "The requested guide is ready."}, "close-child", "child-close")
elif role == "decision":
    original = json.loads((root / "question.json").read_text())
    assert assignment["task_id"] == original["task_id"]
    assert assignment["placement"]["assignment_id"] == original["receipt"]["assignment_id"]
    [decision] = json.loads(items[0]["text"].split("\n", 1)[1])["child_inputs"]
    assert decision["kind"] == "attention_answered"
    assert decision["attention_id"] == original["receipt"]["operation_id"]
    assert decision["answer"] == "精简版"
    assert ("user_message_id" in decision) == (scenario == "attention")
    (Path("repos/example") / "guide.txt").write_text("Compact guide selected.\n")
    (root / "decision.json").write_text(json.dumps(decision))
    tool("tend_close_issue", {"summary": "The requested guide is ready."}, "close-child", "child-close")
else:
    assert {"tend_delegate_work", "tend_close_issue"} <= tools
    delegated = json.loads((root / "delegation.json").read_text())
    context = json.loads(items[0]["text"].split("\n", 1)[1])
    if scenario == "delivery" and assignment.get("followup"):
        [result] = context["child_inputs"]
        assert result["kind"] == "artifact_available"
        submitted = json.loads((root / "submitted.json").read_text())
        assert result["artifact"]["id"] == submitted["operation_id"]
        assert result["artifact"]["available"]
        args = {"summary": "The requested guide is ready.", "artifact_ids": [submitted["operation_id"]]}
        report = tool("tend_report_result", args, "report", "original-report")
        assert tool("tend_report_result", args, "replay-report", "original-report") == report
        (root / "read-delivery").touch()
        send({
            "method": "turn/completed",
            "params": {"threadId": thread_id, "turn": {"id": turn_id, "status": "completed", "items": []}},
        })
        sys.exit(0)
    assert context["previous_delegations"] == [delegated]
    if scenario == "late-close":
        original = json.loads((root / "original-parent.json").read_text())
        assert assignment["placement"]["assignment_id"] != delegated["assignment_id"]
        assert assignment["placement"]["agent"] == original["placement"]["agent"]
        assert context["followup"] == {
            "previous_task_id": original["task_id"], "issue_state": "closed",
        }
    else:
        assert assignment["placement"]["assignment_id"] == delegated["assignment_id"]
    [result] = context["child_inputs"]
    assert result["child_issue_id"] == delegated["action"]["issue_id"]
    assert result["child_task_id"] == delegated["action"]["task_id"]
    if attention and result["kind"] == "attention_requested":
        assert result["request"]["options"] == ["精简版", "完整版"]
        args = {"attention_id": result["attention_id"]}
        name = "tend_escalate_attention"
        if scenario == "attention-local":
            name = "tend_answer_child_attention"
            args["answer"] = "精简版"
        accepted = tool(name, args, "route-question", "route-question")
        assert tool(name, args, "replay-question", "route-question") == accepted
    elif attention or scenario in {"explicit-close", "late-close", "parent-failed", "delivery"}:
        assert result["kind"] == "issue_closed"
        assert result["summary"] == "The requested guide is ready."
    elif scenario == "no-candidate":
        assert result["kind"] == "no_candidate"
        assert result["required_capabilities"] == ["missing-capability"]
    else:
        assert result["kind"] == "task_stopped"
        assert result["outcome"] == "completed"
    if scenario != "late-close" and result["kind"] != "attention_requested":
        tool("tend_close_issue", {"summary": result["summary"]}, "close-parent", "parent-close")

completed = {"id": turn_id, "status": "completed", "items": []}
if scenario == "parent-failed" and role == "parent":
    completed.update(status="failed", error={"message": "Fixture Provider unavailable"})
send({
    "method": "turn/completed",
    "params": {"threadId": thread_id, "turn": completed},
})
