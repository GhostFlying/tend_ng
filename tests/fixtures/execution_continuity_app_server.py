"""Fixed serial author/reviewer flow with persistent native thread identities."""

import json
import os
from pathlib import Path
import subprocess
import sys
import time
import uuid

root = Path(sys.argv[1])
mode = sys.argv[2]


def read():
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    return json.loads(line)


def send(message):
    print(json.dumps(message), flush=True)


def respond(call, result):
    send({"id": call["id"], "result": result})


respond(read(), {})
assert read()["method"] == "initialized"
start = read()
reused = False
if start["method"] == "thread/resume":
    thread = start["params"]["threadId"]
    metadata = root / f"{thread}.json"
    if not metadata.is_file() or mode in {"missing", "broken"} and thread.startswith("fixture-worker-"):
        error = {"code": -32603, "message": "Cannot decode saved rollout"} if mode == "broken" else {
            "code": -32600, "message": f"no rollout found for thread id {thread}"}
        send({"id": start["id"], "error": error})
        start = read()
    elif mode == "active" and thread.startswith("fixture-worker-") and not (root / "allow-resume").is_file():
        if (root / "allow-unknown").is_file():
            respond(start, {"thread": {"id": thread}})
            (root / "reported-unknown").touch()
        else:
            respond(start, {"thread": {"id": thread, "status": {"type": "active"}}})
        sys.exit(0)
    else:
        reused = True
        model, cwd = json.loads(metadata.read_text())
        assert start["params"]["cwd"] == cwd and start["params"]["model"] == model
        respond(start, {"thread": {"id": thread, "status": {"type": "idle"}}, "cwd": cwd, "model": model})
if not reused:
    assert start["method"] == "thread/start", start
    model = start["params"]["model"]
    cwd = start["params"]["cwd"]
    thread = f"{model}-{uuid.uuid4().hex}"
    tool_names = {spec["name"] for spec in start["params"]["dynamicTools"]}
    assert ("tend_deliver_review" if model == "fixture-reviewer" else "tend_request_review") in tool_names
    (root / f"{thread}.json").write_text(json.dumps([model, cwd]))
    respond(start, {"thread": {"id": thread}})
assert Path(cwd) == Path.cwd()

turn = read()
assert turn["method"] == "turn/start", turn
assert turn["params"]["threadId"] == thread
turn_id = str(uuid.uuid4())
items = turn["params"]["input"]
assignment = json.loads(items[-1]["text"])
context = json.loads(items[0]["text"].split("\n", 1)[1]) if len(items) > 1 else {}
inputs = context.get("child_inputs", [])
respond(turn, {"turn": {"id": turn_id, "status": "inProgress", "items": []}})


def tool(name, args, call_id, rpc_id=None):
    rpc_id = rpc_id or call_id
    send({"id": rpc_id, "method": "item/tool/call", "params": {
        "threadId": thread, "turnId": turn_id, "callId": call_id, "tool": name, "arguments": args,
    }})
    reply = read()
    assert reply["id"] == rpc_id and reply["result"]["success"], reply
    return json.loads(reply["result"]["contentItems"][0]["text"])


def save(name, value):
    (root / name).write_text(json.dumps(value))


if model == "fixture-coordinator":
    if not inputs:
        work = {"title": "Write a guide", "objective": "Design, draft, review, and revise a guide.",
                "category": "implementation"}
        save("author.json", tool("tend_delegate_work", work, "author"))
        if mode == "probe":
            work.update(title="Shared lock probe", objective="Wait for the author's workspace.")
            save("probe.json", tool("tend_delegate_work", work, "probe"))
    else:
        save("parent-inputs.json", inputs)
        tool("tend_close_issue", {"summary": "Reviewed guide complete."}, "close")
elif model == "fixture-reviewer":
    binding = assignment["review"]
    author = json.loads((root / "author.json").read_text())["action"]
    assert binding["author_task_id"] == author["task_id"]
    assert binding["author_issue_id"] == author["issue_id"]
    assert assignment["workspace_key"] == assignment["issue_id"]
    assert Path.cwd().name == assignment["issue_id"]
    assert Path("review-input", binding["candidate"].removeprefix("artifact:")).read_text() == "Initial candidate\n"
    assert not Path("repos/example/guide.txt").exists()
    assert Path("repos/example/README.md").is_file()
    checkout = ["git", "-C", "repos/example"]
    assert subprocess.check_output([*checkout, "rev-parse", "HEAD"], text=True).strip() == (root / "base").read_text()
    assert subprocess.run([*checkout, "symbolic-ref", "-q", "HEAD"], capture_output=True).returncode == 1
    (root / "review-running").touch()
    deadline = time.monotonic() + 20
    while not (root / "deliver-review").exists():
        if time.monotonic() >= deadline:
            raise TimeoutError("review gate")
        time.sleep(0.02)
    result = {"conclusion": "Request a revision", "evidence": "Line one should include an example."}
    receipt = tool("tend_deliver_review", result, "result")
    assert tool("tend_deliver_review", result, "result", "replay") == receipt
    tool("tend_deliver_review", result, "duplicate-result")
    tool("tend_close_issue", {"summary": "Review delivered."}, "close")
elif assignment["work"]["title"] == "Shared lock probe":
    assert Path("repos/example/guide.txt").read_text() == "Revised candidate with example\n"
    (root / "probe-ran").touch()
    tool("tend_close_issue", {"summary": "Author relinquished shared workspace."}, "close")
elif not inputs:
    guide = Path("repos/example/guide.txt")
    guide.write_text("Initial candidate\n")
    tool("tend_post_issue_message", {"text": "Initial candidate is ready for review."}, "post")
    file = tool("tend_submit_artifact", {"path": str(guide)}, "file")
    if mode == "drift":
        assert Path(os.environ["CODEX_HOME"]) == root / "author-codex-old"
        (root / "author-before-review").touch()
        deadline = time.monotonic() + 20
        while not (root / "submit-review").exists():
            if time.monotonic() >= deadline:
                raise TimeoutError("author gate")
            time.sleep(0.02)
    tool("tend_request_review", {
        "candidate": f"artifact:{file['operation_id']}", "scope": "Guide example",
        "base_refs": {"example": (root / "base").read_text()},
    }, "review")
else:
    [review] = inputs
    assert review["kind"] == "review_finished" and review["review"]["author_task_id"] == assignment["task_id"]
    assert review["result"]["evidence"] == "Line one should include an example."
    assert Path("repos/example/guide.txt").read_text() == "Initial candidate\n"
    if mode in {"missing", "broken", "drift"}:
        assert not reused
        if mode == "drift":
            assert model == "fixture-worker-updated"
            assert Path(os.environ["CODEX_HOME"]) == root / "author-codex-new"
            assert turn["params"]["effort"] == "low"
        assert any(op["action"]["kind"] == "request_review"
                   for op in context["recovery"]["prior_receipts_recent_first"])
    else:
        assert reused
    Path("repos/example/guide.txt").write_text("Revised candidate with example\n")
    tool("tend_post_issue_message", {"text": "Revised original candidate in place."}, "post")
    tool("tend_close_issue", {"summary": "Revised guide complete."}, "close")

send({"method": "turn/completed", "params": {"threadId": thread, "turn": {
    "id": turn_id, "status": "completed", "items": [
        {"type": "userMessage", "clientId": turn["params"]["clientUserMessageId"]},
    ],
}}})
