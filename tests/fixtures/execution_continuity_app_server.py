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
fallback = mode in {"fallback", "workspace-missing", "workspace-replaced"}
two_rounds = mode in {"resume", "review-missing", "review-unknown", "replacement"}
artifact_handoff = mode == "artifact-replacement"
candidate_text = "Initial candidate\n" * (4096 if artifact_handoff else 1)
workspace_text = "Worktree changed after snapshot\n" if artifact_handoff else candidate_text


def gate(name):
    deadline = time.monotonic() + 25
    while not (root / name).exists():
        if time.monotonic() >= deadline:
            raise TimeoutError(name)
        time.sleep(0.02)


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
    if mode == "review-unknown" and thread.startswith("fixture-reviewer-") and not (root / "allow-review-resume").exists():
        attempt = root / "review-resume-rpc"
        if not attempt.exists():
            attempt.touch()
            send({"id": start["id"], "error": {"code": -32603, "message": "temporary server failure"}})
        else:
            (root / "review-resume-disconnected").touch()
        sys.exit(0)
    if (not metadata.is_file() or mode in {"missing", "broken"} and thread.startswith("fixture-worker-")
            or mode == "review-missing" and thread.startswith("fixture-reviewer-")):
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
    assert start["params"]["ephemeral"] is False and start["params"]["historyMode"] == "legacy"
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


def advance_head(message, add_guide=False):
    checkout = ["git", "-C", "repos/example"]
    if add_guide:
        subprocess.run([*checkout, "add", "guide.txt"], check=True)
    subprocess.run([*checkout, "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                    "commit", "--allow-empty", "-qm", message], check=True)
    return subprocess.check_output([*checkout, "rev-parse", "HEAD"], text=True).strip()


if model == "fixture-coordinator" and not fallback:
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
    suffix = 2 if binding["scope"] == "Verify the revised guide" else 1
    author_cwd = Path((root / "author-cwd").read_text())
    assert Path.cwd() == author_cwd
    assert binding["author_workspace"] == f"{author_cwd.parent.name}/{author_cwd.name}"
    if suffix == 1:
        path = Path(context["bound_review_artifact"])
        assert path.is_absolute() and not path.is_relative_to(author_cwd)
        assert path.name == binding["candidate"].removeprefix("artifact:")
        assert path.read_text() == candidate_text
        if artifact_handoff:
            assert assignment["placement"]["runner_id"] == "next-runner"
            assert path.parent == root / "shared" / ".tend-host" / assignment["placement"]["host_id"] / "artifacts"
            assert Path("repos/example/guide.txt").read_text() == workspace_text
    else:
        assert inputs[0]["kind"] == "review_requested"
        assert inputs[0]["review"] == binding
        if mode == "review-missing":
            assert not reused
            assert any(op["action"]["kind"] == "deliver_review"
                       and op["action"]["result"]["evidence"] == "Line one should include an example."
                       for op in context["recovery"]["prior_receipts_recent_first"])
        elif mode == "replacement":
            assert not reused and assignment["placement"]["runner_id"] == "next-runner"
            [previous] = assignment["review_history"]
            assert previous["kind"] == "review_finished" and previous["child_task_id"] == assignment["task_id"]
            assert previous["review"]["scope"] == "Guide example"
            assert previous["result"]["evidence"] == "Line one should include an example."
        else:
            assert reused
    if not fallback:
        assert Path("repos/example/guide.txt").is_file()
        checkout = ["git", "-C", "repos/example"]
        sha = assignment["work"]["base_refs"]["example"]
        head = subprocess.check_output([*checkout, "rev-parse", "HEAD"], text=True).strip()
        assert head != sha
        assert subprocess.check_output([*checkout, "symbolic-ref", "--short", "HEAD"], text=True).strip()
        item = "README.md" if suffix == 1 else "guide.txt"
        version = subprocess.check_output([*checkout, "show", f"{sha}:{item}"], text=True)
        assert version == ("Fixture quickstart repository.\n" if suffix == 1 else "Revised candidate with example\n")
    save(f"review-thread-{suffix}", thread)
    (root / f"review-running-{suffix}").touch()
    gate(f"deliver-review-{suffix}")
    result = ({"conclusion": "Request a revision", "evidence": "Line one should include an example."} if suffix == 1
              else {"conclusion": "Accepted", "evidence": "The immutable revision includes an example."})
    receipt = tool("tend_deliver_review", result, "result")
    assert receipt["action"]["request_id"] == binding["request_id"]
    assert tool("tend_deliver_review", result, "result", "replay") == receipt
    assert tool("tend_deliver_review", result, "duplicate-result") == receipt
    save(f"result-{suffix}", receipt)
    gate(f"allow-review-stop-{suffix}")
    tool("tend_close_issue", {"summary": "Review delivered."}, "close")
elif assignment["work"]["title"] == "Shared lock probe":
    assert Path("repos/example/guide.txt").read_text() == "Revised candidate with example\n"
    (root / "probe-ran").touch()
    tool("tend_close_issue", {"summary": "Author relinquished shared workspace."}, "close")
elif not inputs:
    if fallback:
        assert not assignment.get("workspace_key") and not assignment.get("parent_issue_id")
        assert Path.cwd().parent.name == "tasks" and Path.cwd().name == assignment["task_id"]
        save("author.json", {"action": {"task_id": assignment["task_id"], "issue_id": assignment["issue_id"]}})
    else:
        assert not assignment.get("workspace_key") and Path.cwd().name == assignment["parent_issue_id"]
    (root / "author-cwd").write_text(cwd)
    guide = Path("guide.txt" if fallback else "repos/example/guide.txt")
    guide.write_text(candidate_text)
    tool("tend_post_issue_message", {"text": "Initial candidate is ready for review."}, "post")
    file = tool("tend_submit_artifact", {"path": str(guide)}, "file")
    if artifact_handoff:
        guide.write_text(workspace_text)
    if mode == "drift":
        assert Path(os.environ["CODEX_HOME"]) == root / "author-codex-old"
        (root / "author-before-review").touch()
        gate("submit-review")
    args = {"candidate": f"artifact:{file['operation_id']}", "scope": "Guide example"}
    if not fallback:
        args["base_refs"] = {"example": (root / "base").read_text()}
    save("request-1", tool("tend_request_review", args, "review"))
    if not fallback:
        # Advance actual HEAD after binding the candidate. Review must not move it back.
        advance_head("Author keeps working after the review request")
    (root / "author-continued-1").touch()
    gate("allow-author-stop-1")
else:
    [review] = [entry for entry in inputs if entry["kind"] == "review_finished"]
    assert review["kind"] == "review_finished" and review["review"]["author_task_id"] == assignment["task_id"]
    if mode in {"workspace-missing", "workspace-replaced"}:
        error = "author workspace is missing" if mode == "workspace-missing" else "author workspace identity changed"
        assert error in review["summary"] and "result" not in review
        tool("tend_close_issue", {"summary": "Review prep failed on the original Task."}, "close")
    elif review["review"]["scope"] == "Verify the revised guide":
        assert review["result"]["evidence"] == "The immutable revision includes an example."
        assert reused
        assert Path("repos/example/guide.txt").read_text() == "Later uncommitted author work\n"
        tool("tend_close_issue", {"summary": "Revised guide complete."}, "close")
    else:
        assert review["result"]["evidence"] == "Line one should include an example."
        guide = Path("guide.txt" if fallback else "repos/example/guide.txt")
        assert guide.read_text() == workspace_text
        if mode in {"missing", "broken", "drift"}:
            assert not reused
            if mode == "drift":
                assert model == "fixture-worker-updated"
                assert Path(os.environ["CODEX_HOME"]) == root / "author-codex-new"
                assert turn["params"]["effort"] == "low"
            assert any(op["action"]["kind"] == "request_review"
                       for op in context["recovery"]["prior_receipts_recent_first"])
        elif mode in {"replacement", "artifact-replacement"}:
            assert not reused and assignment["placement"]["runner_id"] == "next-runner"
        else:
            assert reused
        guide.write_text("Revised candidate with example\n")
        tool("tend_post_issue_message", {"text": "Revised original candidate in place."}, "post")
        if two_rounds:
            ref = advance_head("Revised candidate", add_guide=True)
            args = {"candidate": ref, "scope": "Verify the revised guide",
                    "review_task_id": review["child_task_id"], "base_refs": {"example": ref}}
            request = tool("tend_request_review", args, "review-again")
            assert tool("tend_request_review", args, "new-review-call") == request
            save("request-2", request)
            advance_head("Advance the author HEAD again")
            guide.write_text("Later uncommitted author work\n")
            (root / "author-continued-2").touch()
            gate("allow-author-stop-2")
        else:
            tool("tend_close_issue", {"summary": "Revised guide complete."}, "close")

send({"method": "turn/completed", "params": {"threadId": thread, "turn": {
    "id": turn_id, "status": "completed", "items": [
        {"type": "userMessage", "clientId": turn["params"]["clientUserMessageId"]},
    ],
}}})
