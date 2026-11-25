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
if start["method"] == "thread/resume":
    # This legacy fixture does not preserve a resumable native session.
    thread = start["params"]["threadId"]
    send({"id": start["id"], "error": {
        "code": -32600, "message": f"no rollout found for thread id {thread}"}})
    start = request("thread/start")
assert start["method"] == "thread/start", start
model = start["params"]["model"]
worker = model == "fixture-worker"
attention = scenario in {"attention", "attention-local"}
role = "child" if worker else "parent"
if not worker and (root / "delegation.json").exists():
    role = "resumed"
if worker and attention and (root / "question.json").exists():
    role = "decision"
if worker and scenario == "issue-attention" and (root / "issue-questions.json").exists():
    role = "issue-attention-input"
if scenario == "stage-delivery" and not worker and (root / "stage-delivery.json").exists():
    role = "stage-decision"
if scenario == "proposal" and not worker:
    if (root / "proposal-v2.json").exists():
        role = "proposal-continue"
    elif (root / "proposal-v1.json").exists():
        role = "proposal-revise"
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
instructions = start["params"]["developerInstructions"]
if "tend_delegate_work" in tools:
    assert "Deliver one readable main report" in instructions
    assert "When implementation is directly authorized, carry it through within scope" in instructions
    assert "A report or attachment receipt is not Issue completion." in instructions
elif worker:
    assert "Deliver investigations directly by default." in instructions
    assert "only if the user explicitly asks for it or new evidence could change a key conclusion" in instructions
respond(start, {"thread": {"id": thread_id}})
turn = request("turn/start")
items = turn["params"]["input"]
assignment = json.loads(items[-1]["text"])
if scenario == "external-wait" and worker and assignment["work"]["title"] in {"Isolated B", "Shared C"}:
    role = "other-work"
if role == "proposal-continue":
    (root / "continuation-prepared").touch()
    gate("start-continuation")
    if (root / "fail-before-native").exists():
        sys.exit(0)
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
    if scenario == "issue-attention":
        args.update(title="撰写示例指南", objective="确认篇幅和语言后整理使用说明。")
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
        assert "For complex implementation, seek independent review of the actual candidate" in instructions
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
    elif scenario == "issue-attention":
        questions = [
            "示例指南要采用精简版还是完整版？",
            "示例指南要用简体中文还是英文？",
            "仅由上级处理的内部问题？",
        ]
        receipts = [
            tool("tend_request_attention", {"question": question}, f"ask-{n}", f"ask-{n}")
            for n, question in enumerate(questions)
        ]
        (root / "issue-questions.json").write_text(json.dumps({
            "ids": [receipt["operation_id"] for receipt in receipts], "task_id": assignment["task_id"],
        }))
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
    elif scenario == "proposal":
        for version in (1, 2):
            path = Path(f"summary-export-v{version}.md")
            path.write_text(
                f"# 任务运行摘要导出方案 v{version}\n\n"
                "## 结论\n\n导出格式为 **Markdown**，保留任务标题、结果和更新时间。\n\n"
                "- 无运行记录时，显示暂无可导出的运行。\n"
                "- 有运行记录时，保留结果的段落结构。\n\n"
                "| 任务字段 | 示例值 | 输出说明 | 更新策略 | 适用范围 |\n"
                "| --- | --- | --- | --- | --- |\n"
                "| 示例任务 A | 运行完成 | 保留可读的工作摘要 | 使用记录中的时间 | 合成数据 |\n\n"
                "```text\n" + "This is a deliberately long synthetic example for checking horizontal scrolling inside a narrow report. " * 2 + "\n```\n"
            )
            tool("tend_submit_artifact", {"path": str(path)}, f"submit-v{version}", f"delivery-v{version}")
        note = Path("wording-notes.md")
        note.write_text("# 独立文案记录\n\n这份记录不是审批版本。\n")
        tool("tend_submit_artifact", {"path": str(note)}, "submit-note", "delivery-note")
        tool("tend_close_issue", {"summary": "已整理示例草稿。"}, "close-child", "child-close")
        gate("finish-stage-child")
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
elif role == "issue-attention-input":
    original = json.loads((root / "issue-questions.json").read_text())
    inputs = json.loads(items[0]["text"].split("\n", 1)[1])["child_inputs"]
    if assignment.get("followup"):
        assert assignment["followup"]["previous_task_id"] == original["task_id"]
        assert [item["kind"] for item in inputs] == ["issue_message"], inputs
    else:
        assert assignment["task_id"] == original["task_id"]
    with (root / "issue-attention-inputs.jsonl").open("a") as records:
        for item in inputs:
            records.write(json.dumps({**item, "task_id": assignment["task_id"],
                                      "run_id": turn["params"]["clientUserMessageId"]}) + "\n")
    if any(item.get("attention_id") == original["ids"][1] for item in inputs):
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
elif scenario == "proposal":
    inputs = json.loads(items[0]["text"].split("\n", 1)[1])["child_inputs"]
    if role == "proposal-continue":
        [decision] = inputs
        original = json.loads((root / "proposal-v1.json").read_text())
        current = json.loads((root / "proposal-v2.json").read_text())
        assert assignment["task_id"] == original["task_id"]
        assert decision["kind"] == "attention_answered" and decision["attention_id"] == original["id"]
        assert "user_message_id" not in decision and "user_text" not in decision
        assert "v2" in decision["answer"] and current["report_id"] in decision["answer"]
        (root / "decision.json").write_text(json.dumps({**decision, "run_id": turn["params"]["clientUserMessageId"]}))
        gate("finish-continuation")
        tool("tend_close_issue", {"summary": "已处理示例中的精确批准。"}, "close-parent", "parent-close")
    else:
        assert "tend_propose_approval" in tools
        version = 2 if role == "proposal-revise" else 1
        if version == 1:
            assert sorted(fact["kind"] for fact in inputs) == ["artifact_available"] * 3 + ["issue_closed"]
            files = {fact["artifact"]["name"]: fact["artifact"]["id"] for fact in inputs if fact["kind"] == "artifact_available"}
            tool("tend_report_result", {"summary": "独立文案记录，不属于审批版本。", "artifact_ids": [files["wording-notes.md"]]},
                 "independent", "independent")
        else:
            [revision] = inputs
            assert revision["message"]["text"] == "请把空结果行为补进同一方案。"
            original = json.loads((root / "proposal-v1.json").read_text())
            files = original["files"]
        file = files[f"summary-export-v{version}.md"]
        report = tool("tend_report_result", {"summary": f"已交付任务摘要导出方案 v{version}。", "artifact_ids": [file]},
                      "report", "report")
        args = {
            "question": f"是否按 v{version} 进入实现？",
            "version": version,
            "report_id": report["operation_id"],
            "artifact_id": file,
            "title": "任务运行摘要导出方案",
            "highlights": ["首版导出任务摘要为 Markdown。", "保留标题、结果和更新时间。"]
                          + (["v2 补充空结果行为和导出字段。"] if version == 2 else []),
            "action": f"批准按 v{version} 实现",
        }
        if version == 2:
            args["replaces"] = original["id"]
        op = tool("tend_propose_approval", args, "proposal", "proposal")
        assert tool("tend_propose_approval", args, "retry-proposal", "proposal") == op
        if version == 2 and (root / "independent-decision").exists():
            tool("tend_request_attention", {"question": "示例发布说明要使用简体中文还是英文？",
                                           "options": ["简体中文", "英文"]}, "legacy", "legacy")
        (root / f"proposal-v{version}.json").write_text(json.dumps({
            "id": op["operation_id"], "task_id": assignment["task_id"], "files": files,
            "report_id": report["operation_id"], "run_id": turn["params"]["clientUserMessageId"],
        }))
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
    if scenario == "issue-attention":
        for n, result in enumerate(context["child_inputs"]):
            if result["kind"] != "attention_requested" or "内部问题" in result["request"]["question"]:
                continue
            question = result["attention_id"]
            tool("tend_escalate_attention", {"attention_id": question}, f"route-{n}", f"route-{question}")
        send({"method": "turn/completed", "params": {"threadId": thread_id, "turn": {
            "id": turn_id, "status": "completed", "items": [],
        }}})
        sys.exit(0)
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
