import json
import os
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
ISSUE_ATTENTION_REQUEST = "请为示例指南整理两个独立问题。"
STAGE_REQUEST = "请发一份使用说明和附件，然后原地等我确认是否结案。"
STAGE_QUESTION = "请审阅附上的使用说明。确认结案，还是需要修改？"
STAGE_REPLY = "确认结案。"
PROPOSAL_REQUEST = "请设计任务摘要导出，等我批准对应版本后再实现。"
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
PARK_ISSUE = "Park fixture Issue "
PARK_FOREIGN_ISSUE = "Reject parking foreign fixture Issue "
REGISTERED_REVIEW = "review Lynx MR !75573"
DIRECT_CASES = {
    "Merge the example change and stop at remote main.": ("merge", "implementation"),
    "Merge the example change and wait for green CI on remote main.": ("ci", "implementation-economy"),
    "Coordinate two independent deliverables in example.": ("multi", "issue-router"),
    "Investigate example and get an independent review if necessary.": ("review", "investigation"),
    "Deliver an example plan, then wait for my decision.": ("stage", "investigation"),
    "Dispatch legacy example work.": ("legacy", "issue-router"),
    "Check stage support across a Server upgrade.": ("stage-compat", "issue-router"),
}
DIRECT_REVIEW_REPLY = "Review the example candidate."
DIRECT_STAGE_REPLY = "Close the delivered example plan."
DIRECT_STAGE_PROBE = "Probe blocked stage Attention."
COMPAT_REPLIES = ("Resume the example after the upgrade.", "Accept the bound report.")
for answer in (DIRECT_REVIEW_REPLY, DIRECT_STAGE_REPLY, *COMPAT_REPLIES):
    cases[answer] = {"answer": "Decision delivered to the owner."}
cases[DIRECT_STAGE_PROBE] = {"answer": "The stage question is still pending."}
for prompt in DIRECT_CASES:
    cases[prompt] = {"answer": "Assigned the requested work."}

cases[RECORD_ISSUE] = {"answer": "已记下，稍后再做。"}
cases[START_ISSUE] = {"answer": "已经安排原 Issue 开工。"}
cases[REGISTERED_REVIEW] = {"answer": "I’ve queued the read-only review."}
cases["Is UnregisteredRepo in the list?"] = {"answer": "No, that repository is not registered."}
cases[ISSUE_CHAT_REQUEST] = {"dispatch": True}
cases[ISSUE_CHAT_RELAY] = {"answer": "我已把补充要求交给负责指南的 Agent。"}
cases[WEB_REQUEST] = {"dispatch": True}
cases[ATTENTION_REQUEST] = {"dispatch": True}
cases[ISSUE_ATTENTION_REQUEST] = {"dispatch": True}
cases[ATTENTION_REPLY] = {"answer": "我已把精简版的决定交给负责的 Agent。"}
cases[STAGE_REQUEST] = {"dispatch": True}
cases[PROPOSAL_REQUEST] = {"dispatch": True}
cases[STAGE_REPLY] = {"answer": "我已把结案确认交给负责的 Agent。"}
cases["我稍后再决定，可以先继续聊吗？"] = {"answer": "可以。你准备好时，回复上面的问题就行。"}
cases["这项工作现在到哪一步了？"] = {"answer": "负责的 Agent 已经整理完使用说明，文件正在同步。", "wait": True}
cases["好，发我一份原文件就行。"] = {"answer": "收到。文件同步后，我会把下载入口发到这里。"}
database = sqlite3.connect(sys.argv[1], timeout=5)
database.executescript("""
    CREATE TABLE IF NOT EXISTS threads (id TEXT PRIMARY KEY, tools TEXT NOT NULL, instructions TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS resumes (thread_id TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS tool_calls (thread_id TEXT NOT NULL, turn_id TEXT NOT NULL, tool TEXT NOT NULL);
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
routing_config = dict(arg.split("=", 1) for arg in sys.argv[2:] if "=" in arg)
routing_only = routing_config.get("features.shell_tool") == "false"
metering = Path(sys.argv[1]).with_name("system-meter-fixture").exists()


def meter_path(thread):
    return Path(os.environ["CODEX_HOME"]) / "sessions" / f"rollout-2026-01-02T03-04-05-{thread}.jsonl"


def meter(thread, turn, text):
    response_id = "fixture-reused-response"
    input_count = 820 + len(text)
    counts = {"total_tokens": input_count + 32, "input_tokens": input_count,
              "cached_input_tokens": 180, "output_tokens": 32}
    path = meter_path(thread)
    path.parent.mkdir(exist_ok=True, parents=True)
    with path.open("a") as output:
        if output.tell() == 0:
            print(json.dumps({"type": "session_meta", "payload": {"id": thread}}), file=output)
        print(json.dumps({"type": "token_usage_record", "payload": {
            "thread_id": thread, "turn_id": turn, "response_id": response_id,
            "usage": counts, "thread_token_usage": {"total_tokens": 18000},
            "response_model": "gpt-6.2-sol",
        }}), file=output)
    for _ in range(2):
        notify("rawResponse/completed", threadId=thread, turnId=turn, responseId=response_id,
               responseModel="gpt-6.2-sol", usage={"totalTokens": counts["total_tokens"],
               "inputTokens": input_count, "cachedInputTokens": 180, "outputTokens": 32})


def thread_response(params, thread):
    result = {"thread": {"id": thread}}
    if metering:
        result["thread"]["path"] = str(meter_path(thread))
    if routing_only:
        assert "sandbox" not in params and params["permissions"] == "tend-router-routing"
        assert params["approvalPolicy"] == "never"
        assert params["runtimeWorkspaceRoots"] == []
        result.update(sandbox={"type": "readOnly", "networkAccess": False}, approvalPolicy="never",
                      runtimeWorkspaceRoots=[], activePermissionProfile={"id":"tend-router-routing"})
        if release.with_suffix(".routing-scope-mismatch").exists():
            result["sandbox"]["networkAccess"] = True
    return result


def respond(request, payload):
    print(json.dumps({"id": request["id"], "result": payload}), flush=True)


def notify(method, **params):
    print(json.dumps({"method": method, "params": params}), flush=True)


def tool_call(request_id, params):
    with database:
        database.execute("INSERT INTO tool_calls VALUES (?, ?, ?)",
                         (params["threadId"], params["turnId"], params["tool"]))
    print(json.dumps({"id": request_id, "method": "item/tool/call", "params": params}), flush=True)


def missing_thread(request):
    thread_id = request["params"]["threadId"]
    print(json.dumps({
        "id": request["id"],
        "error": {"code": -32600, "message": f"no rollout found for thread id {thread_id}"},
    }), flush=True)


def dispatch(thread_id, turn_id, token, *, remaining=False, guide=False, pending=False, pinned=False,
             stage=False, proposal=False, issue_attention=False):
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
    if issue_attention:
        params["arguments"].update(title="整理示例指南", objective="为示例指南确认篇幅和语言。")
    if stage:
        params["arguments"].update(title="交付使用说明并等候审阅",
                                   objective="阶段交付实际使用说明和附件，然后原地等待用户确认是否结案。")
    if proposal:
        params["arguments"].update(title="设计任务运行摘要导出", objective=PROPOSAL_REQUEST)
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
    tool_call("first-call", params)
    notify("item/started", threadId=thread_id, turnId=turn_id, item={
        "type": "userMessage", "clientId": token,
    })
    result = json.loads(sys.stdin.readline())
    assert result["id"] == "first-call" and result["result"]["success"], result
    receipt = json.loads(result["result"]["contentItems"][0]["text"])
    tool_call("replay-call", params)
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
    tool_call("read-receipt", {
        "threadId": thread_id, "turnId": turn_id, "callId": "read-work",
        "tool": "tend_read_dispatch", "namespace": None,
        "arguments": {"operation_id": operation_id},
    })
    result = json.loads(sys.stdin.readline())
    assert result["id"] == "read-receipt" and result["result"]["success"], result
    return json.loads(result["result"]["contentItems"][0]["text"])


def decision_tool(name, args, thread, turn, token, *, success=True, call_id=None):
    notify("item/started", threadId=thread, turnId=turn,
           item={"type": "userMessage", "clientId": token})
    tool_call(name, {
        "threadId": thread, "turnId": turn, "callId": call_id or name,
        "tool": name, "arguments": args,
    })
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
        case "config/read":
            assert routing_only and params["cwd"]
            config = {"approval_policy": "never", "web_search": "disabled", "agents":{"enabled": routing_config.get("agents.enabled") != "false"},
                      "mcp_servers": {}, "features": {k.removeprefix("features."): False
                            for k, v in routing_config.items() if k.startswith("features.") and v == "false"}}
            respond(request, {"config": config})
        case "thread/start":
            instructions = params["developerInstructions"]
            assert "Preserve the user's original goal, named targets, explicit limits, and stopping point" in instructions
            assert "Do not add operational restrictions the user did not specify." in instructions
            assert "the actual effect would materially expand the user's goal or authorization" in instructions
            assert "Keep read-only investigations and recommendations pending confirmation read-only." in instructions
            assert "not a fresh user request or authorization to implement" in instructions
            assert "publish the report with its files and the related Attention" in instructions
            thread_id = str(uuid.uuid4())
            with database:
                database.execute(
                    "INSERT INTO threads VALUES (?, ?, ?)",
                    (thread_id, json.dumps(params.get("dynamicTools", [])), instructions),
                )
            respond(request, thread_response(params, thread_id))
        case "thread/resume":
            if database.execute(
                "SELECT 1 FROM turns WHERE thread_id = ?", (params["threadId"],)
            ).fetchone():
                with database:
                    database.execute("INSERT INTO resumes VALUES (?)", (params["threadId"],))
                respond(request, thread_response(params, params["threadId"]))
            else:
                missing_thread(request)
        case "turn/start":
            thread_id = params["threadId"]
            turn_id = str(uuid.uuid4())
            text = params["input"][-1]["text"]
            context = params["input"][0]["text"] if len(params["input"]) > 1 else None
            if routing_only:
                assert params["approvalPolicy"] == "never" and params["runtimeWorkspaceRoots"] == []
                assert params["permissions"] == "tend-router-routing" and "sandboxPolicy" not in params
            if text.startswith("Tend Attention for the user's original topic.\n"):
                attention = json.loads(text.split("\n", 1)[1])
                if "original_message_provenance" in attention:
                    case = {"attention": attention, "question": attention["request"]["question"],
                            "answer": "TEND_NO_UPDATE", "handoff_background": attention}
                elif attention["original_request"] in DIRECT_CASES:
                    case = {"attention": attention, "question": attention["request"]["question"],
                            "answer": "TEND_NO_UPDATE"}
                elif attention["original_request"] == PROPOSAL_REQUEST:
                    if attention["proposal"]:
                        assert attention["proposal"]["version"] == 1
                        question = attention["proposal"]["question"]
                    else:
                        question = attention["request"]["question"]
                    case = {"attention": attention, "question": question, "answer": "TEND_NO_UPDATE"}
                elif attention["original_request"] == STAGE_REQUEST:
                    assert attention["request"]["options"] == ["确认结案", "需要修改"]
                    case = {"attention": attention, "question": STAGE_QUESTION, "answer": "TEND_NO_UPDATE"}
                elif attention["original_request"] == ISSUE_ATTENTION_REQUEST:
                    case = {"attention": attention, "question": attention["request"]["question"],
                            "answer": "TEND_NO_UPDATE"}
                else:
                    assert attention["original_request"] == ATTENTION_REQUEST
                    assert attention["request"]["options"] == ["精简版", "完整版"]
                    case = {"attention": attention, "question": ATTENTION_QUESTION,
                            "answer": "The question was published.", "drop_after_question": True}
            elif text.startswith("Tend external facts collected for this Chat.\n"):
                case = {"answer": "TEND_NO_UPDATE"}
                facts = json.loads(text.split("\n", 1)[1])
                if any("original_message_provenance" in fact for fact in facts):
                    assert routing_only and "not a current user request" in context
                    [generated] = [fact for fact in facts if "original_message_provenance" in fact]
                    assert generated["original_message_provenance"]["authorizes_work"] is False
                    assert "original_message_provenance" not in facts[0], facts
                    case["handoff_external"] = True
            elif text.startswith("Tend Issue report for the user's original topic.\n"):
                report = json.loads(text.split("\n", 1)[1])
                if "original_message_provenance" in report:
                    case = {"answer": report["summary"], "handoff_background": report}
                elif report["original_request"] in DIRECT_CASES:
                    case = {"answer": report["summary"]}
                    if report["summary"] in {"The example plan is ready.", "The original compatibility report."}:
                        # Both native report attempts disappear, forcing terminal failure.
                        case["drop_before_start"] = 2
                elif report["original_request"] == PROPOSAL_REQUEST:
                    case = {"answer": report["summary"]}
                elif report["original_request"] == STAGE_REQUEST:
                    assert report["summary"] == "使用说明已整理好，附上初稿供审阅。"
                    [file] = report["artifacts"]
                    assert file["name"] == "guide.txt"
                    case = {"answer": report["summary"]}
                elif report["summary"] == "The guide uses the repository's verified command.":
                    assert report["artifacts"] == []
                    case = {"answer": "I also verified the repository command."}
                else:
                    assert report["original_request"] in {"Dispatch from Chat.", WEB_REQUEST}
                    assert report["summary"] == "The requested guide is ready."
                    [file] = report["artifacts"]
                    assert file["name"] == "guide.txt"
                    case = {"answer": ("使用说明已整理好，包含环境准备和第一条运行命令。"
                                       if report["original_request"] == WEB_REQUEST
                                       else "The guide is ready. Download the attached guide.txt."),
                            "drop_before_start": 1}
            elif text.startswith(("讨论转交 · 上下文摘要。", "Tend Discussion handoff summary")):
                assert context and "Tend provenance:" in context and "not new implementation authorization" in context
                case = {"answer": "Router 已收到核对后的转交，先确认下一步。", "discussion_handoff": True}
                if text.startswith("Tend Discussion handoff summary"):
                    assert "the current turn is an agent-written Discussion handoff" in context
                    assert json.loads(text.splitlines()[1])["generated_handoff_text"].startswith("讨论转交 · 上下文摘要。")
                    line = next(line for line in context.splitlines() if line.startswith("Tend provenance:"))
                    case["handoff_source"] = json.loads(context.split(line + "\n", 1)[1].splitlines()[0])
            elif text.startswith("Storage measurement "):
                case = {"answer": "Accepted."}
            elif text.startswith(CROSS_CHAT_LOOKUP):
                case = {"answer": "The Issue is outside this Chat."}
            elif text.startswith(PARK_ISSUE):
                case = {"answer": "Backlog request accepted."}
            elif text.startswith(PARK_FOREIGN_ISSUE):
                case = {"answer": "Cannot park a different Chat's Issue."}
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
            if event := case.get("handoff_background"):
                assert routing_only and "not a current user request" in context
                assert event["original_message_provenance"]["kind"] == "agent_generated_discussion_handoff"
                assert event["original_message_provenance"]["authorizes_work"] is False
                assert event["generated_handoff_context"] and "original_request" not in event
                for name, arguments in (("tend_dispatch_issue", {"title":"Unexpected issue", "objective":"Do more"}),
                    ("tend_record_issue", {}), ("tend_start_issue", {"issue_id":event["issue_id"]}),
                    ("tend_backlog_issue", {}), ("tend_relay_issue_message", {}), ("tend_subscribe", {}),
                    ("tend_answer_attention", {})):
                    denied = decision_tool(name, arguments, thread_id, turn_id, params["clientUserMessageId"], success=False)
                    assert "cannot admit or change work" in denied["error"]
            if case.get("handoff_external"):
                denied = decision_tool("tend_dispatch_issue", {"title":"Unexpected","objective":"Do more"},
                                       thread_id, turn_id, params["clientUserMessageId"], success=False)
                assert "cannot admit or change work" in denied["error"]
            # Independent discussions may produce the same synthetic summary
            # text. A text match does not make the second delivery a replay.
            if starts > 1 and not case.get("discussion_handoff"):
                replay = json.loads(context.splitlines()[-1])
                assert replay["kind"] == "replay" and replay["attempt_number"] == 2, replay
                assert replay["source_message_id"]
                if not case.get("dispatch"):
                    assert replay["committed_dispatches"] == [], replay
            if source := case.get("handoff_source"):
                assert routing_only
                page = source["source_history"]
                assert source["href"] == f"/discussions?chat={source['discussion_chat_id']}"
                assert all(m["href"] == f"{source['href']}#message-{m['id']}" for m in page["messages"])
                assert all("后续再确认保留天数，但不是原转交" not in m["text"] for m in page["messages"])
                token = params["clientUserMessageId"]
                fixture_call = lambda name, args, **kw: decision_tool(name, args, thread_id, turn_id, token, **kw)
                work = {"title": "Implement synthetic CSV marker", "objective": "Implement the local CSV marker on synthetic data only. Do not deploy."}
                for name, arguments in (("tend_dispatch_issue", work), ("tend_backlog_issue", {}), ("tend_answer_attention", {})):
                    fixture_call(name, arguments, success=False)
                fixture_call("tend_dispatch_issue", {**work, "source_message_ids": [source["handoff_message_id"]]}, success=False)
                for m in page["messages"]:
                    if m["role"] == "assistant":
                        fixture_call("tend_dispatch_issue", {**work, "source_message_ids": [m["id"]]}, success=False)
                        break
                assert fixture_call("tend_read_handoff_source", {"before": 2**63 - 1}) == page
                answer = "Router 已收到讨论转交，可以在已授权范围内协助。"
                if "Fixture final instruction for authorized handoff" in str(page):
                    assert page["next_before"], "the original instruction should be on an older page"
                    more = fixture_call("tend_read_handoff_source", {"before": page["next_before"]})
                    [original] = [m for m in more["messages"] if m["text"].startswith("Fixture authorizes implementation")]
                    assert original["role"] == "user" and original["text"].endswith("Do not deploy.\n")
                    [limits] = [m for m in page["messages"] if m["text"].startswith("Fixture final instruction")]
                    assert limits["reply_to"] == original["id"]
                    args = {**work, "source_message_ids": [original["id"], limits["id"]]}
                    if starts == 1:
                        receipt = fixture_call("tend_dispatch_issue", args)
                        assert receipt == fixture_call("tend_dispatch_issue", args)
                        sys.exit(0)  # Lose the response after the committed dispatch; exercise Router replay.
                    replay = json.loads(context.splitlines()[-1])
                    [original_receipt] = replay["committed_dispatches"]
                    args.update(title="Add the synthetic CSV marker", objective="Add a local marker for synthetic CSV only, with no deployment.")
                    duplicate = fixture_call("tend_dispatch_issue", args, success=False)
                    assert duplicate["committed_dispatches"] == [original_receipt]
                    receipt = read_dispatch(thread_id, turn_id, token, original_receipt["operation_id"])
                    assert receipt == original_receipt
                    answer = f"Authorized work dispatched once to #{receipt['short_number']}."
                elif controls := [m for m in page["messages"] if m["role"] == "user"
                                  and m["text"].startswith("Fixture authorizes Issue control ")]:
                    [original] = controls
                    action, short = original["text"].removeprefix("Fixture authorizes Issue control ").split()
                    tool = f"tend_{action}_issue"
                    assert action in ("start", "backlog")
                    saved = release.with_suffix(f".control-{action}.json")
                    unblock = release.with_suffix(f".control-{action}-release")
                    if starts == 1:
                        receipt = fixture_call(tool, {"issue_id": short, "source_message_ids": [original["id"]]},
                                               call_id=f"original-{action}")
                        if action == "backlog":
                            assert receipt["stop_control_ids"], receipt
                        pending = saved.with_suffix(".tmp")
                        pending.write_text(json.dumps(receipt))
                        pending.rename(saved)
                        deadline = time.monotonic() + 20
                        while not unblock.exists():
                            assert time.monotonic() < deadline, "independent user action did not finish"
                            time.sleep(.02)
                        sys.exit(0)  # The control committed, but there is no recoverable native turn.
                    replay = json.loads(context.splitlines()[-1])
                    assert replay["attempt_number"] == 2 and replay["source_message_id"] == source["handoff_message_id"]
                    prior = json.loads(saved.read_text())
                    assert prior["issue_id"] != short
                    receipt = fixture_call(tool, {"issue_id": prior["issue_id"], "source_message_ids": [original["id"]]},
                                           call_id=f"different-{action}")
                    if receipt != prior:
                        release.with_suffix(f".control-{action}-duplicate.json").write_text(json.dumps(receipt))
                    assert receipt == prior, (prior, receipt)
                    answer = f"Recovered original {action} receipt without applying it again."
                elif "Fixture authorizes relay to #" in str(page):
                    [original] = [m for m in page["messages"] if m["text"].startswith("Fixture authorizes relay")]
                    issue = original["text"].split()[4].rstrip(".")
                    scoped = "Use synthetic CSV only and stop after the local check."
                    args = {"issue_id": issue, "source_message_ids": [original["id"]], "relay_text": scoped}
                    receipt = fixture_call("tend_relay_issue_message", args)
                    assert receipt == fixture_call("tend_relay_issue_message", args)
                    assert scoped in receipt["text"] and original["text"] in receipt["text"]
                    assert receipt["source_message_id"] == source["handoff_message_id"]
                    answer = "Authorized scope relayed."
            elif case.get("attention"):
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
            elif text.startswith((PARK_ISSUE, PARK_FOREIGN_ISSUE)):
                allowed = text.startswith(PARK_ISSUE)
                issue = text.removeprefix(PARK_ISSUE if allowed else PARK_FOREIGN_ISSUE)
                args = {"issue_id": issue}
                token = params["clientUserMessageId"]
                receipt = decision_tool("tend_backlog_issue", args, thread_id, turn_id, token, success=allowed)
                if allowed:
                    assert receipt["state"] == "backlog"
                    assert receipt == decision_tool("tend_backlog_issue", args, thread_id, turn_id, token)
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
            elif text == DIRECT_STAGE_PROBE:
                token = params["clientUserMessageId"]
                [fact] = [item for item in decision_tool("tend_read_attentions", {}, thread_id, turn_id, token)
                          if item["request"]["question"] == "Close the delivered plan?"]
                assert fact["state"] == "pending" and fact["question_message_id"] is None
                decision_tool("tend_publish_attention", {"attention_id": fact["id"], "text": "Close the delivered plan?"},
                              thread_id, turn_id, token, success=False)
                answer = case["answer"]
            elif text in {DIRECT_REVIEW_REPLY, DIRECT_STAGE_REPLY, *COMPAT_REPLIES}:
                assert context and "explicitly replies to this Tend Attention" in context
                fact = json.loads(context.split("explicitly replies to this Tend Attention.\n", 1)[1].splitlines()[0])
                decision_tool("tend_answer_attention", {"attention_id": fact["id"], "answer": text},
                              thread_id, turn_id, params["clientUserMessageId"])
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
            elif text in DIRECT_CASES:
                title, category = DIRECT_CASES[text]
                if title == "stage-compat":
                    decision_tool("tend_dispatch_issue", {"title": title, "objective": text, "repo_ids": ["example"]},
                                  thread_id, turn_id, params["clientUserMessageId"])
                else:
                    catalogue, instructions = database.execute(
                        "SELECT tools, instructions FROM threads WHERE id = ?", (thread_id,)
                    ).fetchone()
                    schema = next(tool["inputSchema"] for tool in json.loads(catalogue)
                                  if tool["name"] == "tend_dispatch_issue")
                    assert category in schema["properties"]["category"]["enum"]
                    for rule in ("one person can deliver the outcome", "verified remote main SHA",
                                 "only when the user explicitly asks for that endpoint",
                                 "one responsible Agent observe"):
                        assert rule in instructions, rule
                    args = {"title": title, "objective": text, "repo_ids": ["example"]}
                    if title != "legacy":
                        args["category"] = category
                    receipt = decision_tool("tend_dispatch_issue", args, thread_id, turn_id,
                                            params["clientUserMessageId"])
                    assert receipt["category"] == category and "parent_issue_id" not in receipt
                answer = case["answer"]
            elif text == REGISTERED_REVIEW:
                decision_tool("tend_dispatch_issue", {
                    "title": "Review Lynx MR !75573",
                    "objective": "Review Lynx MR !75573 read-only. Report review findings and stop. "
                                 "Do not modify or merge the MR.",
                    "repo_ids": ["lynx"], "category": "investigation",
                }, thread_id, turn_id, params["clientUserMessageId"])
                answer = case["answer"]
            elif case.get("dispatch"):
                if starts == 1:
                    receipt = dispatch(thread_id, turn_id, params["clientUserMessageId"],
                                       guide=text == ISSUE_CHAT_REQUEST, pending=text == BOARD_WAITING,
                                       pinned=text == PINNED_REVIEW, stage=text == STAGE_REQUEST,
                                       proposal=text == PROPOSAL_REQUEST,
                                       issue_attention=text == ISSUE_ATTENTION_REQUEST)
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
                          if text in {WEB_REQUEST, ATTENTION_REQUEST, ISSUE_CHAT_REQUEST, ISSUE_ATTENTION_REQUEST}
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
            if metering:
                meter(thread_id, turn_id, text)
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
