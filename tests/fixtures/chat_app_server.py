import json
import sqlite3
import sys
import time
import uuid
from pathlib import Path

cases = json.loads(Path(__file__).with_name("chat.json").read_text())
WEB_REQUEST = "在示例仓库整理一份使用说明，完成后把文件和简短结论发回来。"
ATTENTION_REQUEST = "请为示例仓库整理使用说明。需要我决定时就在这里问。"
ATTENTION_QUESTION = "使用说明你想要精简版，还是完整版？"
ATTENTION_REPLY = "用精简版。"
STAGE_REQUEST = "请发一份使用说明和附件，然后原地等我确认是否结案。"
STAGE_QUESTION = "请审阅附上的使用说明。确认结案，还是需要修改？"
STAGE_REPLY = "确认结案。"
ISSUE_CHAT_REQUEST = "请整理示例仓库的快速上手指南。"
CROSS_CHAT_LOOKUP = "Verify cross-chat Issue access "
ISSUE_CHAT_RELAY = "请让负责指南的 Agent 补一段常见问题。"
ROUTER_SUBSCRIBE = "Subscribe to demo CI."
cases[ROUTER_SUBSCRIBE] = {"answer": "I will follow demo CI."}
CHECK_ISSUE = "Check the finished fixture Issue."
cases[CHECK_ISSUE] = {"answer": "The fixture Issue is closed."}
PINNED_REVIEW = "Prepare a guide from the pinned demo branch."
cases[PINNED_REVIEW] = {"dispatch": True}
BOARD_WAITING = "顺带准备另一个仓库的发布记录。"
cases[BOARD_WAITING] = {"dispatch": True}
RECORD_ISSUE = "先记下稍后整理发布说明。"
START_ISSUE = "开始刚才记下的发布说明。"
cases[RECORD_ISSUE] = {"answer": "已记下，稍后再做。"}
cases[START_ISSUE] = {"answer": "已经安排原 Issue 开工。"}
cases[ISSUE_CHAT_REQUEST] = {"dispatch": True}
cases[ISSUE_CHAT_RELAY] = {"answer": "我已把补充要求交给负责指南的 Agent。"}
cases[WEB_REQUEST] = {"dispatch": True}
cases[ATTENTION_REQUEST] = {"dispatch": True}
cases[ATTENTION_REPLY] = {"answer": "我已把精简版的决定交给负责的 Agent。"}
cases[STAGE_REQUEST] = {"dispatch": True}
cases[STAGE_REPLY] = {"answer": "我已把结案确认交给负责的 Agent。"}
cases["我稍后再决定，可以先继续聊吗？"] = {"answer": "可以。你准备好时，回复上面的问题就行。"}
cases["这项工作现在到哪一步了？"] = {"answer": "负责的 Agent 已经整理完使用说明，文件正在同步。", "wait": True}
cases["好，发我一份原文件就行。"] = {"answer": "收到。文件同步后，我会把下载入口发到这里。"}
database = sqlite3.connect(sys.argv[1], timeout=5)
database.executescript("""
    CREATE TABLE IF NOT EXISTS threads (id TEXT PRIMARY KEY, tools TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS turns (
        seq INTEGER PRIMARY KEY,
        thread_id TEXT NOT NULL,
        turn TEXT NOT NULL,
        held INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS start_requests (
        client_id TEXT PRIMARY KEY,
        thread_id TEXT NOT NULL,
        input TEXT NOT NULL,
        context TEXT
    );
    CREATE TABLE IF NOT EXISTS dispatch_receipts (input TEXT PRIMARY KEY, receipt TEXT NOT NULL);
""")
release = Path(sys.argv[1]).with_suffix(".release")


def respond(request, payload):
    print(json.dumps({"id": request["id"], "result": payload}), flush=True)


def notify(method, **params):
    print(json.dumps({"method": method, "params": params}), flush=True)

def missing_thread(request):
    thread_id = request["params"]["threadId"]
    print(json.dumps({
        "id": request["id"],
        "error": {"code": -32600, "message": f"no rollout found for thread id {thread_id}"},
    }), flush=True)


def dispatch(thread_id, turn_id, token, *, remaining=False, guide=False, pending=False, pinned=False,
             stage=False):
    tools, = database.execute("SELECT tools FROM threads WHERE id = ?", (thread_id,)).fetchone()
    assert any(tool["name"] == "tend_dispatch_issue" for tool in json.loads(tools)), tools
    params = {
        "threadId": thread_id, "turnId": turn_id,
        "callId": "create-remaining-work" if remaining else "create-work",
        "tool": "tend_dispatch_issue", "namespace": None,
        "arguments": {
            "title": "Prepare a demo", "objective": "Write a small fixture document.",
            "repo_ids": ["example"], "required_capabilities": ["image_gen"],
        },
    }
    if guide:
        params["arguments"].update(title="示例仓库上手指南", objective="整理示例仓库的快速上手指南。")
    if stage:
        params["arguments"].update(title="交付使用说明并等候审阅",
                                   objective="阶段交付实际使用说明和附件，然后原地等待用户确认是否结案。")
    if pinned:
        requirements = {"category": "issue-router", "repo_ids": ["example"],
                        "required_capabilities": ["image_gen"]}
        result = decision_tool("tend_agent_candidates", requirements, thread_id, turn_id, token)
        [selected] = [row for row in result["candidates"] if row["agent"]["id"] == "selected-coordinator"]
        assert selected["available"]
        params["arguments"].update(base_refs={"example": "pinned-demo"},
                                   agent={"id": selected["agent"]["id"], "runner_id": selected["runner_id"]})
    if pending:
        params["arguments"].update(title="整理发布记录", objective="准备另一个仓库的发布记录。",
                                   repo_ids=["release-notes"], required_capabilities=[])
    if remaining:
        params["arguments"].update(
            title="Prepare release notes", objective="Summarize the separate release work."
        )
    # The start response is lost. The native input event supplies its identity.
    print(json.dumps({"id": "first-call", "method": "item/tool/call", "params": params}), flush=True)
    notify("item/started", threadId=thread_id, turnId=turn_id, item={
        "type": "userMessage", "clientId": token,
    })
    result = json.loads(sys.stdin.readline())
    assert result["id"] == "first-call" and result["result"]["success"], result
    receipt = json.loads(result["result"]["contentItems"][0]["text"])
    print(json.dumps({"id": "replay-call", "method": "item/tool/call", "params": params}), flush=True)
    replay = json.loads(sys.stdin.readline())
    assert replay["id"] == "replay-call" and replay["result"]["success"], replay
    repeated = json.loads(replay["result"]["contentItems"][0]["text"])
    for key in ("issue_id", "task_id", "operation_id", "short_number"):
        assert repeated.get(key) == receipt.get(key), repeated
    return receipt


def read_dispatch(thread_id, turn_id, token, operation_id):
    notify("item/started", threadId=thread_id, turnId=turn_id, item={
        "type": "userMessage", "clientId": token,
    })
    print(json.dumps({
        "id": "read-receipt", "method": "item/tool/call",
        "params": {
            "threadId": thread_id, "turnId": turn_id, "callId": "read-work",
            "tool": "tend_read_dispatch", "namespace": None,
            "arguments": {"operation_id": operation_id},
        },
    }), flush=True)
    result = json.loads(sys.stdin.readline())
    assert result["id"] == "read-receipt" and result["result"]["success"], result
    return json.loads(result["result"]["contentItems"][0]["text"])


def decision_tool(name, args, thread, turn, token, *, success=True):
    notify("item/started", threadId=thread, turnId=turn,
           item={"type": "userMessage", "clientId": token})
    print(json.dumps({
        "id": name, "method": "item/tool/call",
        "params": {
            "threadId": thread, "turnId": turn, "callId": name,
            "tool": name, "arguments": args,
        },
    }), flush=True)
    result = json.loads(sys.stdin.readline())
    assert result["id"] == name and result["result"]["success"] == success, result
    return json.loads(result["result"]["contentItems"][0]["text"])


for line in sys.stdin:
    request = json.loads(line)
    params = request.get("params", {})
    match request["method"]:
        case "initialize":
            assert params["capabilities"]["experimentalApi"]
            respond(request, {})
        case "initialized":
            pass
        case "thread/start":
            assert "publish the report with its files and the related Attention" in params["developerInstructions"]
            thread_id = str(uuid.uuid4())
            with database:
                database.execute(
                    "INSERT INTO threads VALUES (?, ?)",
                    (thread_id, json.dumps(params.get("dynamicTools", []))),
                )
            respond(request, {"thread": {"id": thread_id}})
        case "thread/resume":
            if database.execute(
                "SELECT 1 FROM turns WHERE thread_id = ?", (params["threadId"],)
            ).fetchone():
                respond(request, {"thread": {"id": params["threadId"]}})
            else:
                missing_thread(request)
        case "turn/start":
            thread_id = params["threadId"]
            turn_id = str(uuid.uuid4())
            text = params["input"][-1]["text"]
            context = params["input"][0]["text"] if len(params["input"]) > 1 else None
            if text.startswith("Tend Attention for the user's original topic.\n"):
                attention = json.loads(text.split("\n", 1)[1])
                if attention["original_request"] == STAGE_REQUEST:
                    assert attention["request"]["options"] == ["确认结案", "需要修改"]
                    case = {"attention": attention, "question": STAGE_QUESTION, "answer": "TEND_NO_UPDATE"}
                else:
                    assert attention["original_request"] == ATTENTION_REQUEST
                    assert attention["request"]["options"] == ["精简版", "完整版"]
                    case = {"attention": attention, "question": ATTENTION_QUESTION,
                            "answer": "The question was published.", "drop_after_question": True}
            elif text.startswith("Tend external facts collected for this Chat.\n"):
                case = {"answer": "TEND_NO_UPDATE"}
            elif text.startswith("Tend Issue report for the user's original topic.\n"):
                report = json.loads(text.split("\n", 1)[1])
                assert report["original_request"] in {"Dispatch from Chat.", WEB_REQUEST, STAGE_REQUEST}
                if report["original_request"] == STAGE_REQUEST:
                    assert report["summary"] == "使用说明已整理好，附上初稿供审阅。"
                    [file] = report["artifacts"]
                    assert file["name"] == "guide.txt"
                    case = {"answer": report["summary"]}
                elif report["summary"] == "The guide uses the repository's verified command.":
                    assert report["artifacts"] == []
                    case = {"answer": "I also verified the repository command."}
                else:
                    assert report["summary"] == "The requested guide is ready."
                    [file] = report["artifacts"]
                    assert file["name"] == "guide.txt"
                    case = {"answer": ("使用说明已整理好，包含环境准备和第一条运行命令。"
                                       if report["original_request"] == WEB_REQUEST
                                       else "The guide is ready. Download the attached guide.txt."),
                            "drop_before_start": 1}
            elif text.startswith("Storage measurement "):
                case = {"answer": "Accepted."}
            elif text.startswith(CROSS_CHAT_LOOKUP):
                case = {"answer": "The Issue is outside this Chat."}
            else:
                case = cases[text]
            with database:
                database.execute(
                    "INSERT INTO start_requests VALUES (?, ?, ?, ?)",
                    (params["clientUserMessageId"], thread_id, text, context),
                )
            starts = database.execute(
                "SELECT COUNT(*) FROM start_requests WHERE input = ?", (text,)
            ).fetchone()[0]
            if starts <= case.get("drop_before_start", 0):
                sys.exit(0)
            if starts > 1:
                replay = json.loads(context.splitlines()[-1])
                assert replay["kind"] == "replay" and replay["attempt_number"] == 2, replay
                assert replay["source_message_id"]
                if not case.get("dispatch"):
                    assert replay["committed_dispatches"] == [], replay
            if case.get("attention"):
                fact = case["attention"]
                receipt = decision_tool(
                    "tend_publish_attention", {"attention_id": fact["attention_id"], "text": case["question"]},
                    thread_id, turn_id, params["clientUserMessageId"],
                )
                assert receipt["source_message_id"] == fact["original_message_id"]
                if case.get("drop_after_question"):
                    saved = Path(sys.argv[1]).with_suffix(".question")
                    if starts == 1:
                        saved.write_text(receipt["question_message_id"])
                        sys.exit(0)
                    assert receipt["question_message_id"] == saved.read_text()
                answer = case["answer"]
            elif text == RECORD_ISSUE:
                arguments = {"title": "稍后整理发布说明", "objective": text, "repo_ids": ["release-notes"]}
                token = params["clientUserMessageId"]
                receipt = decision_tool("tend_record_issue", arguments, thread_id, turn_id, token)
                assert receipt == decision_tool("tend_record_issue", arguments, thread_id, turn_id, token)
                assert "task_id" not in receipt
                answer = case["answer"]
            elif text == START_ISSUE:
                token = params["clientUserMessageId"]
                board = decision_tool("tend_list_issues", {}, thread_id, turn_id, token)
                [record] = board["issues"]
                recorded = decision_tool("tend_read_issue", {"issue_id": record["id"]}, thread_id, turn_id, token)
                assert record["stage"] == "backlog" and "task_id" not in recorded
                receipt = decision_tool("tend_start_issue", {"issue_id": record["id"]}, thread_id, turn_id, token)
                assert receipt["issue_id"] == record["id"] and receipt["task_id"]
                answer = case["answer"]
            elif text == CHECK_ISSUE:
                token = params["clientUserMessageId"]
                board = decision_tool("tend_list_issues", {}, thread_id, turn_id, token)
                [root] = [item for item in board["issues"] if item.get("parent_issue_id") is None]
                issue = decision_tool("tend_read_issue", {"issue_id": root["id"]}, thread_id, turn_id, token)
                assert issue["issue_state"] == "closed" and issue["status"] == "completed"
                assert issue["issue_id"] == root["id"]
                answer = case["answer"]
            elif text == ROUTER_SUBSCRIBE:
                condition = {"url": "https://github.com/example/guide/pull/82",
                             "event": "ci", "state": "pending"}
                receipt = decision_tool("tend_subscribe", condition, thread_id, turn_id,
                                        params["clientUserMessageId"])
                repeated = decision_tool("tend_subscribe", condition, thread_id, turn_id,
                                         params["clientUserMessageId"])
                assert repeated == receipt
                answer = case["answer"]
            elif text in {ATTENTION_REPLY, STAGE_REPLY}:
                if context and "explicitly replies to this Tend Attention" in context:
                    fact = json.loads(context.split("explicitly replies to this Tend Attention.\n", 1)[1].splitlines()[0])
                else:
                    [fact] = decision_tool(
                        "tend_read_attentions", {}, thread_id, turn_id, params["clientUserMessageId"],
                    )
                receipt = decision_tool(
                    "tend_answer_attention", {"attention_id": fact["id"],
                                              "answer": "确认结案" if text == STAGE_REPLY else "精简版"},
                    thread_id, turn_id, params["clientUserMessageId"],
                )
                assert receipt["user_message_id"] and receipt["question_message_id"] == fact["question_message_id"]
                answer = case["answer"]
            elif text.startswith(CROSS_CHAT_LOOKUP):
                reference = text.removeprefix(CROSS_CHAT_LOOKUP)
                for tool in ("tend_read_issue", "tend_relay_issue_message"):
                    decision_tool(tool, {"issue_id": reference}, thread_id, turn_id,
                                  params["clientUserMessageId"], success=False)
                answer = case["answer"]
            elif text == ISSUE_CHAT_RELAY:
                quoted = json.loads(context.split("replies to this saved Chat message.\n", 1)[1].splitlines()[0])
                [issue] = quoted["issue_ids"]
                [number] = quoted["issue_numbers"]
                board = decision_tool("tend_list_issues", {}, thread_id, turn_id,
                                      params["clientUserMessageId"])
                [card] = [row for row in board["issues"] if row["id"] == issue]
                assert card["short_number"] == number
                for reference in (f"#{number}", str(number), issue):
                    detail = decision_tool("tend_read_issue", {"issue_id": reference}, thread_id, turn_id,
                                           params["clientUserMessageId"])
                    assert detail["issue_id"] == issue and detail["short_number"] == number
                relay = {"issue_id": f"#{number}"}
                receipt = decision_tool("tend_relay_issue_message", relay, thread_id, turn_id,
                                        params["clientUserMessageId"])
                assert receipt == decision_tool("tend_relay_issue_message", relay, thread_id, turn_id,
                                                params["clientUserMessageId"])
                assert receipt["issue_id"] == issue and receipt["text"] == text
                assert receipt["short_number"] == number
                answer = case["answer"]
            elif case.get("dispatch"):
                if starts == 1:
                    receipt = dispatch(thread_id, turn_id, params["clientUserMessageId"],
                                       guide=text == ISSUE_CHAT_REQUEST, pending=text == BOARD_WAITING,
                                       pinned=text == PINNED_REVIEW, stage=text == STAGE_REQUEST)
                    with database:
                        database.execute(
                            "INSERT INTO dispatch_receipts VALUES (?, ?)", (text, json.dumps(receipt))
                        )
                    if case.get("drop_after_dispatch"):
                        sys.exit(0)
                else:
                    receipt, = database.execute(
                        "SELECT receipt FROM dispatch_receipts WHERE input = ?", (text,)
                    ).fetchone()
                    receipt = json.loads(receipt)
                    assert replay["source_message_id"] == receipt["source_message_id"]
                    assert replay["committed_dispatches"] == [receipt], replay
                    confirmed = read_dispatch(
                        thread_id, turn_id, params["clientUserMessageId"], receipt["operation_id"]
                    )
                    assert confirmed == receipt, (confirmed, receipt)
                    dispatch(thread_id, turn_id, params["clientUserMessageId"], remaining=True)
                answer = ("我已把这项工作交给负责的 Agent。整理好后，我会把文件发回这里。"
                          if text in {WEB_REQUEST, ATTENTION_REQUEST, ISSUE_CHAT_REQUEST}
                          else f"Issue {receipt['issue_id']} is waiting for assignment.")
            else:
                answer = case["answer"]
            turn = {
                "id": turn_id,
                "status": "completed",
                "items": [
                    {"type": "userMessage", "clientId": params["clientUserMessageId"]},
                    {
                        "type": "agentMessage", "id": f"{turn_id}-progress",
                        "phase": "commentary", "text": "Fixture process commentary",
                    },
                    {
                        "type": "agentMessage", "id": f"{turn_id}-answer",
                        "phase": "final_answer", "text": answer,
                    },
                ],
            }
            with database:
                database.execute(
                    "INSERT INTO turns (thread_id, turn, held) VALUES (?, ?, ?)",
                    (thread_id, json.dumps(turn), case.get("wait", False)),
                )
            if case.get("drop_start_response"):
                sys.exit(0)
            if not case.get("dispatch") or starts > 1:
                respond(request, {"turn": {"id": turn_id, "status": "inProgress", "items": []}})
            if case.get("wait"):
                deadline = time.monotonic() + 30
                while not release.exists():
                    assert time.monotonic() < deadline, "fixture never released the turn"
                    time.sleep(0.02)
            for item in turn["items"]:
                notify("item/completed", threadId=thread_id, turnId=turn_id, item=item)
            notify("turn/completed", threadId=thread_id, turn={**turn, "items": []})
        case "thread/turns/list":
            assert params["itemsView"] == "full"
            rows = database.execute(
                "SELECT turn, held FROM turns WHERE thread_id = ? ORDER BY seq DESC",
                (params["threadId"],),
            ).fetchall()
            if not rows:
                missing_thread(request)
                continue
            turns = []
            for encoded, held in rows:
                turn = json.loads(encoded)
                if held and not release.exists():
                    turn.update(status="inProgress", items=turn["items"][:1])
                turns.append(turn)
            respond(request, {"data": turns, "nextCursor": None})
        case _:
            raise AssertionError(request["method"])
