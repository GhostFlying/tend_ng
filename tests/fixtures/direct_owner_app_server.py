"""Fixed native calls exercising a root Work Agent and a root coordinator."""

import json
from pathlib import Path
import subprocess
import sys
import uuid


def line():
    raw = sys.stdin.readline()
    if not raw:
        sys.exit(0)
    return json.loads(raw)


def send(**value):
    print(json.dumps(value), flush=True)


def respond(request, result):
    send(id=request["id"], result=result)


def tool(name, *, accepted=True, **arguments):
    assert name in tools, name
    call = str(uuid.uuid4())
    send(id=call, method="item/tool/call", params={
        "threadId": thread, "turnId": turn, "callId": call, "tool": name, "arguments": arguments,
    })
    response = line()
    assert response["id"] == call and response["result"]["success"] is accepted, response
    return json.loads(response["result"]["contentItems"][0]["text"])


def git(*words):
    return subprocess.check_output(["git", "-C", Path.cwd() / "repos/example", *words],
                                   text=True).strip()


def publish(summary, ids=()):
    tool("tend_report_result", summary=summary, artifact_ids=list(ids))
    tool("tend_close_issue", summary=summary)


root = Path(sys.argv[1])
respond(line(), {})
assert line()["method"] == "initialized"
start = line()
if start["method"] == "thread/resume":
    thread = start["params"]["threadId"]
    send(id=start["id"], error={"code": -32600, "message": f"no rollout found for thread id {thread}"})
    start = line()
assert start["method"] == "thread/start", start
tools = {entry["name"]: entry for entry in start["params"]["dynamicTools"]}
instructions = start["params"]["developerInstructions"]
thread, turn = str(uuid.uuid4()), str(uuid.uuid4())
respond(start, {"thread": {"id": thread}})
request = line()
assert request["method"] == "turn/start", request
items = request["params"]["input"]
assignment = json.loads(items[-1]["text"])
inputs = json.loads(items[0]["text"].split("\n", 1)[1])["child_inputs"] if len(items) > 1 else []
issue, task = assignment["issue_id"], assignment["task_id"]
title = assignment["work"]["title"]
owner = assignment["placement"]["agent"]
direct = "parent_issue_id" not in assignment and owner["category"] != "issue-router"
assert start["params"]["model"] == owner["model"]
assert Path(start["params"]["cwd"]) == Path.cwd()
respond(request, {"turn": {"id": turn, "status": "inProgress", "items": []}})

if direct:
    assert {"tend_report_result", "tend_delegate_work", "tend_submit_artifact", "tend_request_attention",
            "tend_find_pull_request", "tend_link_pull_request", "tend_request_review"} <= tools.keys()
    assert "direct Owner of this top-level Issue" in instructions
    assert "Do not create a placeholder child" in instructions
    assert Path.cwd() == root / "runner.runs" / "workspaces" / issue
    assert (Path.cwd() / "AGENTS.md").is_file()
if owner["category"] != "review" and title != "legacy":
    for rule in ("existing merge gates", "remote main SHA", "only if the user explicitly asks",
                 "one responsible Agent", "End the current turn"):
        assert rule in instructions, rule

if title in {"merge", "ci"}:
    assert direct and title in assignment["work"]["objective"].lower()
    marker = Path.cwd() / "delivery.txt"
    if not inputs:
        assert git("status", "--porcelain") == ""
        repo = Path.cwd() / "repos/example"
        (repo / f"{title}.txt").write_text(f"{title} fixture\n")
        assert (repo / f"{title}.txt").read_text() == f"{title} fixture\n"
        git("add", f"{title}.txt")
        git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
            "commit", "-m", f"feat: record {title} fixture")
        git("push", "--quiet", "origin", "HEAD:main")
        sha = git("ls-remote", "origin", "refs/heads/main").split()[0]
        assert sha == git("rev-parse", "HEAD")
        marker.write_text(sha + "\n")
        file_id = tool("tend_submit_artifact", path=marker.name)["operation_id"]
        if title == "ci":
            (root / "direct-main-sha").write_text(sha)
            tool("tend_waitfor", event="ci", state="success", head_sha=sha,
                 url="https://github.com/example/guide/pull/82")
        else:
            publish(f"Remote main {sha}; merge endpoint reached.", [file_id])
    elif title == "merge":
        assert assignment["placement"]["runner_id"] == "next-runner"
        assert assignment["followup"]["previous_task_id"]
        assert any(item.get("message", {}).get("text") == "Recheck the original delivery." for item in inputs)
        assert (Path.cwd() / "repos/example/merge.txt").is_file()
        assert marker.read_text().strip() == git("rev-parse", "HEAD")
        publish("Reopened with the original owner and workspace.")
    else:
        sha = (root / "direct-main-sha").read_text()
        assert marker.read_text().strip() == sha
        assert any(x["kind"] == "external_fact" and x["fact"]["head_sha"] == sha
                   and x["fact"]["state"] == "success" for x in inputs), inputs
        assert assignment["placement"]["runner_id"] == "next-runner"
        [file] = [item for item in assignment["handoff"]["operations"]
                  if item["action"]["kind"] == "submit_artifact"]
        publish(f"Remote main {sha}; CI success.", [file["operation_id"]])
elif title == "stage":
    assert direct
    if not inputs:
        assert "after_report_id" in instructions
        (Path.cwd() / "stage.md").write_text("The isolated stage plan.\n")
        artifact = tool("tend_submit_artifact", path="stage.md")["operation_id"]
        report = tool("tend_report_result", summary="The example plan is ready.", artifact_ids=[artifact])
        tool("tend_request_attention", question="Close the delivered plan?",
             after_report_id=report["operation_id"], options=["Close the delivered example plan."])
    else:
        [decision] = [item for item in inputs if item["kind"] == "attention_answered"]
        assert decision["user_message_id"], decision
        publish("The delivered plan was confirmed.")
elif title == "stage-compat":
    assert owner["category"] == "issue-router"
    properties = tools["tend_request_attention"]["inputSchema"]["properties"]
    id_path = root / "compat-report-id"
    if not inputs:
        assert "after_report_id" not in properties
        stage = tool("tend_report_result", summary="The original compatibility report.")
        id_path.write_text(stage["operation_id"])
        bad = tool("tend_request_attention", accepted=False, question="Must not silently publish.",
                   after_report_id=stage["operation_id"])
        assert "does not support report-bound Attention" in bad["error"], bad
        tool("tend_request_attention", question="Resume after the Server upgrade?",
             options=["Resume the example after the upgrade."])
    else:
        [decision] = [item for item in inputs if item["kind"] == "attention_answered"]
        assert decision["user_message_id"], decision
        assert "after_report_id" in properties
        if decision["user_text"] == "Resume the example after the upgrade.":
            tool("tend_request_attention", question="Accept the exact compatibility report?",
                 after_report_id=id_path.read_text(), options=["Accept the bound report."])
        else:
            assert decision["user_text"] == "Accept the bound report."
            publish("The bound report was accepted after upgrade.")
elif title == "legacy":
    assert owner["category"] == "issue-router" and "tend_submit_artifact" not in tools
    assert "workspace_key" not in assignment
    publish("Legacy work completed.")
elif title == "review":
    assert direct
    if not inputs:
        tool("tend_request_attention", question="Run the independent review?",
             options=["Review the example candidate.", "Stop without review."])
    elif any(item["kind"] == "attention_answered" for item in inputs):
        [decision] = [item for item in inputs if item["kind"] == "attention_answered"]
        assert decision["user_message_id"], decision
        sha = git("rev-parse", "HEAD")
        (Path.cwd() / "candidate").write_text(sha)
        tool("tend_request_review", candidate=sha, scope="Check the exact fixture commit.",
             base_refs={"example": sha})
    else:
        [result] = [item for item in inputs if item["kind"] == "review_finished"]
        assert result["result"]["conclusion"] == "Fixture candidate is correct."
        publish(f"Independent review returned to {task}.")
elif owner["category"] == "review":
    assert assignment["review"]["author_workspace"] == f"workspaces/{assignment['parent_issue_id']}"
    assert "tend_report_result" not in tools and "tend_delegate_work" not in tools
    assert git("rev-parse", "HEAD") == (Path.cwd() / "candidate").read_text()
    tool("tend_deliver_review", conclusion="Fixture candidate is correct.", evidence="Exact Git SHA verified.")
elif title == "multi":
    assert owner["category"] == "issue-router" and "tend_submit_artifact" not in tools
    if not inputs:
        for child in ("a", "b"):
            tool("tend_delegate_work", title=child, objective=f"Deliver independent result {child}.",
                 category="implementation")
    else:
        assert len({item["child_issue_id"] for item in inputs
                    if item["kind"] == "issue_closed"}) == 2, inputs
        publish("Both independent deliveries are complete.")
elif title in {"a", "b"}:
    assert "parent_issue_id" in assignment and "tend_report_result" not in tools
    assert (Path.cwd() / "repos/example/README.md").is_file()
    tool("tend_close_issue", summary=f"Delivered independent result {title}.")
else:
    raise AssertionError(assignment)

send(method="turn/completed", params={"threadId": thread, "turn": {
    "id": turn, "status": "completed",
    "items": [{"type": "userMessage", "clientId": request["params"]["clientUserMessageId"]}],
}})
