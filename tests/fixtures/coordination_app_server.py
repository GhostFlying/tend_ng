"""Fixed native callbacks for a parent, its work child, and the resumed parent."""
import json
import os
from pathlib import Path
import sys
import time


def request(method):
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    call = json.loads(line)
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


def issue_chat(turn, assignment, items, tools):
    assert "tend_post_issue_message" in tools
    context = json.loads(items[0]["text"].split("\n", 1)[1]) if len(items) > 1 else None
    if not worker:
        if not (root / "delegation.json").exists():
            args = {"title": "整理快速上手指南", "objective": "整理安装和首次运行步骤。",
                    "category": "implementation", "required_capabilities": ["image_gen"]}
            delegated = tool("tend_delegate_work", args, "delegate", "delegate")
            (root / "delegation.json").write_text(json.dumps(delegated))
        else:
            # Wake facts may share a turn. Persist their actual recipient for the test.
            with (root / "owner-inputs.jsonl").open("a") as evidence:
                for input in context["child_inputs"]:
                    evidence.write(json.dumps(input) + "\n")
            for input in context["child_inputs"]:
                message = input.get("message", {})
                if message.get("kind") == "relay":
                    child = json.loads((root / "delegation.json").read_text())["action"]["issue_id"]
                    args = {"child_issue_id": child, "message_id": message["id"]}
                    tool("tend_forward_issue_message", args, "forward", "forward")
                    tool("tend_forward_issue_message", args, "duplicate-forward", "duplicate-forward")
    else:
        assert Path.cwd() == root / "runner.runs" / "workspaces" / assignment["parent_issue_id"]
        if context is None:
            args = {"text": "初稿已整理，接下来核对首次运行步骤。"}
            first = tool("tend_post_issue_message", args, "first", "first")
            assert tool("tend_post_issue_message", args, "replay-first", "first") == first
            (root / "first-post.json").write_text(json.dumps(first))
            gate("finish-first")
        else:
            [input] = context["child_inputs"]
            assert input["kind"] == "issue_message"
            message = input["message"]
            if message["kind"] == "relay":
                assert message["author"] == "coordinator" and message["forwarded_from"]
                assert message["text"] == "请让负责指南的 Agent 补一段常见问题。"
                text = "常见问题已补进指南。"
            else:
                assert message["author"] == "user"
            original = json.loads((root / "first-post.json").read_text())
            first_task = json.loads((root / "delegation.json").read_text())["action"]["task_id"]
            if message["text"] == "请把验证命令也放进去。":
                assert assignment["task_id"] == first_task
                assert input["message"]["reply_to"] == original["operation_id"]
                text = "验证命令已补进指南。"
                tool("tend_close_issue", {"summary": "快速上手指南已完成。"}, "close", "close")
            elif message["kind"] == "user":
                assert assignment["task_id"] != first_task
                assert assignment["followup"] == {"previous_task_id": first_task, "issue_state": "closed"}
                assert input["message"]["text"] == "谢谢，命令适用于 macOS 吗？"
                text = "适用于 macOS，使用终端执行即可。"
            tool("tend_post_issue_message", {"text": text, "reply_to": input["message"]["id"]}, "reply", "reply")
    history = {"id": turn_id, "status": "completed", "items": [
        {"type": "userMessage", "clientId": turn["params"]["clientUserMessageId"]},
        {"type": "agentMessage", "id": turn_id + "-detail", "phase": "commentary",
         "text": "核对安装步骤和验证命令。" if worker else "协调指南的整理工作。"},
    ]}
    with (history_home / f"{thread_id}.json").open("w") as saved:
        json.dump(history, saved)
    send({"method": "turn/completed", "params": {"threadId": thread_id, "turn": history}})


root = Path(sys.argv[1])
scenario = sys.argv[2]
history_home = Path(os.environ.get("CODEX_HOME", root))
respond(request("initialize"), {})
request("initialized")
start = json.loads(sys.stdin.readline())
if scenario == "issue-chat" and start["method"] == "thread/turns/list":
    thread = start["params"]["threadId"]
    with (root / "inspection.jsonl").open("a") as visits:
        visits.write(json.dumps(thread) + "\n")
    saved = history_home / f"{thread}.json"
    respond(start, {"data": [json.loads(saved.read_text())] if saved.exists() else [], "nextCursor": None})
    sys.exit(0)
assert start["method"] == "thread/start", start
model = start["params"]["model"]
worker = model == "fixture-worker"
attention = scenario in {"attention", "attention-local"}
role = "child" if worker else "parent"
if not worker and (root / "delegation.json").exists():
    role = "resumed"
if worker and attention and (root / "question.json").exists():
    role = "decision"
if scenario == "stage-delivery" and not worker and (root / "stage-delivery.json").exists():
    role = "stage-decision"
if scenario == "issue-chat":
    import uuid
    role = f"{role}-{uuid.uuid4().hex}"
if scenario == "external-wait":
    if worker and (root / "wait-registration.json").exists():
        role = "external-resumed"
    elif worker:
        role = "child"
thread_id = f"fixture-{role}-thread"
turn_id = f"fixture-{role}-turn"
assert Path(start["params"]["cwd"]) == Path.cwd()
tools = {spec["name"] for spec in start["params"]["dynamicTools"]}
if "tend_delegate_work" in tools:
    assert "A report or attachment receipt is not Issue completion." in start["params"]["developerInstructions"]
respond(start, {"thread": {"id": thread_id}})
turn = request("turn/start")
items = turn["params"]["input"]
assignment = json.loads(items[-1]["text"])
if scenario == "external-wait" and worker and assignment["work"]["title"] in {"Isolated B", "Shared C"}:
    role = "other-work"
respond(turn, {"turn": {"id": turn_id, "status": "inProgress", "items": []}})

if scenario == "issue-chat":
    issue_chat(turn, assignment, items, tools)
    sys.exit(0)

if role == "parent":
    assert {"tend_delegate_work", "tend_close_issue"} <= tools
    assert turn["params"]["effort"] == "low"
    assert len(items) == 1
    if scenario == "stage-delivery":
        assert assignment["work"]["objective"] == "阶段交付实际使用说明和附件，然后原地等待用户确认是否结案。"
    gate("delegate-now")
    args = {"title": "Write the guide", "objective": "Add a one-line guide",
            "category": "implementation", "required_capabilities": ["image_gen"]}
    if scenario == "pull-board":
        args.update(title="核对安装步骤", objective="准备可用的安装说明。")
    if scenario == "native-review":
        found = tool("tend_agent_candidates", {"category": "implementation", "required_capabilities": ["image_gen"]},
                     "work-candidates", "work-candidates")
        assert found["runner_id"] == "demo-runner"
        assert [candidate["id"] for candidate in found["candidates"]] == ["worker", "selected-worker"]
        args["agent_id"] = "selected-worker"
    if scenario == "no-candidate":
        args["required_capabilities"] = ["missing-capability"]
    delegated = tool("tend_delegate_work", args, "delegate", "original-delegate")
    assert tool("tend_delegate_work", args, "replay-delegate", "original-delegate") == delegated
    (root / "delegation.json").write_text(json.dumps(delegated))
    if scenario == "external-wait":
        shared = tool("tend_delegate_work", {**args, "title": "Shared C"}, "delegate-c", "delegate-c")
        isolated = tool("tend_delegate_work", {**args, "title": "Isolated B", "isolated_workspace": True},
                        "delegate-b", "delegate-b")
        (root / "siblings.json").write_text(json.dumps({"isolated": isolated, "shared": shared}))
    if scenario == "late-close":
        (root / "original-parent.json").write_text(json.dumps(assignment))
        tool("tend_close_issue", {"summary": "The main objective is settled."}, "early-close", "early-close")
elif role == "child":
    assert {"tend_close_issue", "tend_submit_artifact"} <= tools
    expected_worker = "selected-worker" if scenario == "native-review" else "worker"
    assert assignment["placement"]["agent"]["id"] == expected_worker
    assert Path.cwd() == root / "runner.runs" / "workspaces" / assignment["parent_issue_id"]
    assert (Path("repos/example") / ".git").is_file()
    assert (Path("repos/example") / "README.md").read_text() == "A fixed repository.\n"
    (Path("repos/example") / "guide.txt").write_text("A fixture guide.\n")
    if scenario == "native-review":
        assert "seek independent review" in start["params"]["developerInstructions"]
        receiver = "native-review-child"
        history = [
            {"id": "spawn", "tool": "spawnAgent", "status": "completed", "agentsStates": {receiver: {"status": "running"}}},
            {"id": "wait-pending", "tool": "wait", "status": "completed", "agentsStates": {receiver: {"status": "running"}}},
            {"id": "wait-finished", "tool": "wait", "status": "completed", "agentsStates": {receiver: {"status": "completed"}}},
        ]
        for item in history:
            item.update(type="collabAgentToolCall", senderThreadId=thread_id, receiverThreadIds=[receiver])
            notification = {"method": "item/completed", "params": {"threadId": thread_id, "turnId": turn_id, "item": item}}
            send(notification)
            send(notification)
        review = tool("tend_record_review", {"child_thread_id": receiver, "candidate": "guide-v1",
                      "scope": "installation instructions", "conclusion": "accepted"},
                      "record-review", "record-review")
        (root / "review.json").write_text(json.dumps(review))
    elif scenario == "external-wait":
        url = "https://github.com/example/guide/pull/82"
        condition = {"url": url, "event": "ci", "check": "build-linux", "state": "terminal",
                     "head_sha": "a" * 40, "run_id": "7"}
        receipt = tool("tend_waitfor", condition, "wait-pr", "wait-pr")
        tool("tend_subscribe", {**condition, "state": "success", "required": True}, "required-pr", "required-pr")
        tool("tend_subscribe", {"url": url, "event": "ci", "state": "pending"}, "optional-pr", "optional-pr")
        (root / "wait-registration.json").write_text(json.dumps(receipt))
    elif scenario == "pull-discovery":
        assert "tend_find_pull_request" in tools
        requested = tool("tend_find_pull_request", {}, "find-pr", "find-pr")
        assert tool("tend_find_pull_request", {}, "repeat-find-pr", "find-pr") == requested
        (root / "find-pull.json").write_text(json.dumps(requested))
        gate("finish-work")
    elif scenario == "pull-board":
        assert "tend_link_pull_request" in tools
        args = {"url": "https://github.com/Example/Guide/pull/73/"}
        attached = tool("tend_link_pull_request", args, "attach-pr", "attach-pr")
        assert tool("tend_link_pull_request", args, "replay-pr", "attach-pr") == attached
        (root / "pull-link.json").write_text(json.dumps(attached))
    elif attention:
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
    elif scenario == "stage-delivery":
        tool("tend_submit_artifact", {"path": "repos/example/guide.txt"}, "submit", "delivery")
        tool("tend_close_issue", {"summary": "The draft guide is ready."}, "close-child", "child-close")
        # Keep the only slot until both facts are queued for the original parent Task.
        gate("finish-stage-child")
    if scenario in {"explicit-close", "late-close", "parent-failed", "delivery", "pull-board", "pull-discovery", "native-review"}:
        tool("tend_close_issue", {"summary": "The requested guide is ready."}, "close-child", "child-close")
elif role == "other-work":
    if assignment["work"]["title"] == "Isolated B":
        assert Path.cwd() == root / "runner.runs" / "workspaces" / assignment["issue_id"]
        assert not (root / "resumed-wait.json").exists()
        (root / "isolated-ran").touch()
    else:
        assert Path.cwd() == root / "runner.runs" / "workspaces" / assignment["parent_issue_id"]
        assert (root / "resumed-wait.json").exists() or (root / "cancelled-wait").exists()
        (root / "shared-ran").touch()
    tool("tend_close_issue", {"summary": "The requested guide is ready."}, "close-other", "close-other")
elif role == "external-resumed":
    saved = json.loads((root / "wait-registration.json").read_text())
    if (root / "handoff-happened").exists():
        assert assignment["placement"]["runner_id"] == "next-runner"
        assert assignment["placement"]["assignment_id"] != saved["assignment_id"]
        assert assignment["handoff"]["phase"] == "waiting_external"
    else:
        assert assignment["placement"]["assignment_id"] == saved["assignment_id"]
    assert Path.cwd() == root / "runner.runs" / "workspaces" / assignment["parent_issue_id"]
    inputs = json.loads(items[0]["text"].split("\n", 1)[1])["child_inputs"]
    assert {fact["mode"] for fact in inputs} == {"wait", "required", "optional"}, inputs
    hit = next(fact for fact in inputs if fact["mode"] == "wait")
    assert hit["watch_id"] == saved["operation_id"] and hit["fact"]["run_id"] == "7", hit
    assert hit["fact"]["state"] == "success"
    cached = tool("tend_waitfor", saved["action"]["condition"], "already-observed", "already-observed")
    assert cached["action"]["kind"] == "confirm_external"
    assert cached["action"]["fact_id"] == hit["fact_id"]
    (root / "resumed-wait.json").write_text(json.dumps(inputs))
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
elif role == "stage-decision":
    original = json.loads((root / "stage-delivery.json").read_text())
    assert assignment["task_id"] == original["task_id"]
    [decision] = json.loads(items[0]["text"].split("\n", 1)[1])["child_inputs"]
    assert decision["kind"] == "attention_answered"
    assert decision["attention_id"] == original["attention_id"] and decision["answer"] == "确认结案"
    (root / "decision.json").write_text(json.dumps({**decision, "run_id": turn["params"]["clientUserMessageId"]}))
    tool("tend_close_issue", {"summary": "用户已审阅使用说明并确认结案。"}, "close-parent", "parent-close")
elif scenario == "stage-delivery":
    delegated = json.loads((root / "delegation.json").read_text())["action"]
    inputs = json.loads(items[0]["text"].split("\n", 1)[1])["child_inputs"]
    assert sorted(fact["kind"] for fact in inputs) == ["artifact_available", "issue_closed"], inputs
    assert all(fact["child_issue_id"] == delegated["issue_id"] for fact in inputs)
    [file] = [fact["artifact"] for fact in inputs if fact["kind"] == "artifact_available"]
    assert file["available"]
    report = tool("tend_report_result", {"summary": "使用说明已整理好，附上初稿供审阅。", "artifact_ids": [file["id"]]},
                  "stage-report", "stage-report")
    question = {"question": "请审阅附上的使用说明。确认结案，还是需要修改？", "options": ["确认结案", "需要修改"]}
    attention_receipt = tool("tend_request_attention", question, "stage-question", "stage-question")
    (root / "stage-delivery.json").write_text(json.dumps({
        "report_id": report["operation_id"], "attention_id": attention_receipt["operation_id"],
        "task_id": assignment["task_id"], "run_id": turn["params"]["clientUserMessageId"], "file": file,
    }))
else:
    assert {"tend_delegate_work", "tend_close_issue"} <= tools
    delegated = json.loads((root / "delegation.json").read_text())
    context = json.loads(items[0]["text"].split("\n", 1)[1])
    if scenario == "external-wait":
        send({
            "method": "turn/completed",
            "params": {"threadId": thread_id, "turn": {"id": turn_id, "status": "completed", "items": []}},
        })
        sys.exit(0)
    if scenario == "delivery" and assignment.get("followup"):
        [result] = context["child_inputs"]
        assert result["kind"] == "artifact_available"
        submitted = json.loads((root / "submitted.json").read_text())
        assert result["artifact"]["id"] == submitted["operation_id"]
        assert result["artifact"]["available"]
        args = {"summary": "The requested guide is ready.", "artifact_ids": [submitted["operation_id"]]}
        report = tool("tend_report_result", args, "report", "original-report")
        assert tool("tend_report_result", args, "replay-report", "original-report") == report
        if (root / "repeat-report").exists():
            supplemental = {**args, "summary": "The guide uses the repository's verified command."}
            later = tool("tend_report_result", supplemental, "later-report", "later-report")
            assert later["operation_id"] != report["operation_id"]
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
    elif attention or scenario in {"explicit-close", "late-close", "parent-failed", "delivery", "pull-board", "pull-discovery", "external-wait", "native-review"}:
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
