"""Fixed Discussion App Server, with durable native history across processes."""
import json
import os
from pathlib import Path
import sqlite3
import sys
import time
import uuid

home = Path(os.environ["CODEX_HOME"])
assert Path(os.environ["HOME"]) == home
assert os.environ["FIXTURE_API_KEY"] == "configured-discussion-key"
assert not {"OPENAI_API_KEY", "TEND_TOKEN", "GH_TOKEN", "GITHUB_TOKEN", "SSH_AUTH_SOCK"} & os.environ.keys()
profile = "tend-discussion"
args = sys.argv[1:]
assert args[:4] == ["app-server", "--listen", "stdio://", "--strict-config"], args
config = dict(arg.split("=", 1) for arg in args[5::2])
assert config["default_permissions"] == json.dumps(profile)
assert config["permissions.tend-discussion.network.enabled"] == "false"
assert config["shell_environment_policy"] == '{inherit="none"}'
assert config["mcp_servers"] == "{}"
cwd, program = Path.cwd(), Path(sys.argv[0]).resolve()
filesystem = (":minimal", ":workspace_roots", str(cwd), str(program))
assert config["permissions.tend-discussion.filesystem"] == (
    "{" + ",".join(f'{json.dumps(path)}="read"' for path in filesystem) + "}"
)
assert config["features.plugins"] == config["features.apps"] == "false"
ledger = sqlite3.connect(home / "fixture.sqlite")
ledger.executescript("""
CREATE TABLE IF NOT EXISTS threads (id TEXT PRIMARY KEY, params TEXT);
CREATE TABLE IF NOT EXISTS resumes (id TEXT, roots TEXT, found INTEGER);
CREATE TABLE IF NOT EXISTS forks (id TEXT, parent TEXT, through_turn TEXT);
CREATE TABLE IF NOT EXISTS input_history (turn_id TEXT, inherited TEXT);
CREATE TABLE IF NOT EXISTS turns (id TEXT, thread_id TEXT, text TEXT, context TEXT, history TEXT, roots TEXT, PRIMARY KEY(thread_id, id));
CREATE TABLE IF NOT EXISTS dropped_turns (text TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS tool_results (turn_id TEXT, arguments TEXT, tool TEXT, result TEXT);
""")
tools = {"tend_read_discussion_reference", "tend_read_discussion_delivery", "tend_read_discussion_history"}


def output(frame):
    print(json.dumps(frame), flush=True)


def respond(request, result):
    output({"id": request["id"], "result": result})


def event(method, **params):
    output({"method": method, "params": params})


def scope(params):
    assert params["permissions"] == profile and params["approvalPolicy"] == "never"
    assert "sandbox" not in params and "sandboxPolicy" not in params
    assert params["cwd"] == str(cwd)
    roots = params["runtimeWorkspaceRoots"]
    assert roots == sorted({str(Path(path).resolve(strict=True)) for path in roots}), roots
    return roots


def mock_read(path, roots):
    path = Path(path).resolve(strict=True)
    if not any(path.is_relative_to(root) for root in roots):
        return None
    return path.read_text().strip()


loaded = {}
metering = (home / "system-meter-fixture").exists()


def meter_path(thread):
    return home / "sessions" / f"rollout-2026-01-02T03-04-05-{thread}.jsonl"


def meter(thread, turn, receiver, text):
    path = meter_path(thread)
    path.parent.mkdir(exist_ok=True)
    counts = {"total_tokens": 920, "input_tokens": 840, "cached_input_tokens": 280,
              "cache_write_input_tokens": 0, "output_tokens": 80, "reasoning_output_tokens": 0}
    with path.open("a") as output:
        if output.tell() == 0:
            print(json.dumps({"type": "session_meta", "payload": {"id": thread}}), file=output)
        print(json.dumps({"type": "token_usage_record", "payload": {
            "thread_id": thread, "turn_id": turn, "response_id": "discussion-response",
            "usage": counts, "thread_token_usage": {"total_tokens": 30000},
            "response_model": "gpt-6.2-sol",
        }}), file=output)
    for _ in range(2):
        event("rawResponse/completed", threadId=thread, turnId=turn,
              responseId="discussion-response", responseModel="gpt-6.2-sol",
              usageMetadata={"metadata": {
                  "total_tokens": 920, "input_tokens": 840, "output_tokens": 80,
                  "input_tokens_details": {"cached_tokens": 280},
                  "output_tokens_details": {"reasoning_tokens": 0},
              }} if text == "Compare two local export formats." else None,
              usage=None if text == "Discussion with unavailable counts." else {
                  "totalTokens": 920, "inputTokens": 840, "cachedInputTokens": 280,
                  "cacheWriteInputTokens": 0, "outputTokens": 80, "reasoningOutputTokens": 0})


def thread_result(thread, roots):
    result = {"thread": {"id": thread}, "approvalPolicy": "never",
            "sandbox": {"type": "readOnly", "networkAccess": False},
            "activePermissionProfile": {"id": profile}, "runtimeWorkspaceRoots": roots}
    if metering:
        result["thread"]["path"] = str(meter_path(thread))
    return result


def read(request, thread, turn, call_id, tool, arguments):
    output({"id": call_id, "method": "item/tool/call", "params": {
        "tool": tool, "namespace": None, "callId": call_id,
        "threadId": thread, "turnId": turn, "arguments": arguments,
    }})
    result = json.loads(sys.stdin.readline())
    assert result["id"] == call_id and "result" in result, result
    with ledger:
        ledger.execute("INSERT INTO tool_results VALUES (?,?,?,?)", (
            turn, json.dumps(arguments), tool, json.dumps(result["result"])))
    return result["result"]


for line in sys.stdin:
    request = json.loads(line)
    params = request.get("params", {})
    match request["method"]:
        case "initialize":
            respond(request, {})
        case "initialized":
            pass
        case "config/read":
            respond(request, {"config": {"mcp_servers": {}}})
        case "thread/start":
            roots = scope(params)
            if "Main Chat assistant receiving" in params["developerInstructions"]:
                assert params["dynamicTools"] == [] and roots == [], params
                for key in ("shell_tool", "unified_exec", "apply_patch_freeform", "browser_use",
                            "browser_use_external", "browser_use_full_cdp_access", "in_app_browser",
                            "view_image", "goals", "memories", "hooks", "image_generation"):
                    assert config[f"features.{key}"] == "false", config
                assert config["tools.experimental_request_user_input.enabled"] == "false"
                assert config["web_search"] == '"disabled"' and config["mcp_servers"] == "{}"
            else:
                assert set(tool["name"] for tool in params["dynamicTools"]) == tools
                assert "You cannot dispatch work" in params["developerInstructions"]
            thread = str(uuid.uuid4())
            with ledger:
                ledger.execute("INSERT INTO threads VALUES (?,?)", (thread, json.dumps(params)))
            loaded[thread] = roots
            binding = thread_result(thread, roots)
            if config.get("features.shell_tool") == "false" and (home / "receiver-profile-mismatch").exists():
                binding["sandbox"] = {"type": "dangerFullAccess"}
            respond(request, binding)
        case "thread/resume":
            thread = params["threadId"]
            roots = scope(params)
            if not ledger.execute("SELECT 1 FROM threads WHERE id=?", (thread,)).fetchone():
                with ledger:
                    ledger.execute("INSERT INTO resumes VALUES (?,?,0)", (thread, json.dumps(roots)))
                output({"id": request["id"], "error": {
                    "code": -32600, "message": f"no rollout found for thread id {thread}",
                }})
                continue
            # Fault injection affects only confirmation, not the persisted thread.
            if (home / "mismatch-roots").exists():
                respond(request, thread_result(thread, [str(cwd)]))
                continue
            loaded[thread] = roots
            with ledger:
                ledger.execute("INSERT INTO resumes VALUES (?,?,1)", (thread, json.dumps(roots)))
            respond(request, thread_result(thread, roots))
        case "thread/fork":
            parent, through = params["threadId"], params["lastTurnId"]
            roots = scope(params)
            fork = str(uuid.uuid4())
            included, = ledger.execute(
                "SELECT rowid FROM turns WHERE thread_id=? AND id=?", (parent, through)).fetchone()
            with ledger:
                ledger.execute("INSERT INTO threads VALUES (?,?)", (fork, json.dumps(params)))
                ledger.execute("INSERT INTO forks VALUES (?,?,?)", (fork, parent, through))
                ledger.execute(
                    """INSERT INTO turns SELECT id,?,text,context,history,roots
                       FROM turns WHERE thread_id=? AND rowid<=? ORDER BY rowid""",
                    (fork, parent, included))
            loaded[fork] = roots
            respond(request, thread_result(fork, roots))
        case "thread/turns/list":
            turns = ledger.execute("SELECT history FROM turns WHERE thread_id=? ORDER BY rowid DESC", (params["threadId"],))
            respond(request, {"data": [json.loads(history) for history, in turns], "nextCursor": None})
        case "turn/start":
            thread, turn = params["threadId"], str(uuid.uuid4())
            roots = scope(params)
            assert roots == loaded[thread], (roots, loaded)
            client_id = params["clientUserMessageId"]
            text = params["input"][-1]["text"]
            context = params["input"][0]["text"]
            receiver = config.get("features.shell_tool") == "false"
            if not receiver:
                resources = json.loads(context.split("Earlier references do not grant access.\n")[-1])
            case = text.split(":", 1)[0]
            if case == "Retry discussion":
                with ledger:
                    dropped = ledger.execute(
                        "INSERT OR IGNORE INTO dropped_turns VALUES (?)", (text,)).rowcount
                if dropped:
                    sys.exit(0)
            if case == "Exhaust discussion retries":
                sys.exit(0)
            answer = "可以。先区分要讨论的想法与之后可能交给 Router 的工作。"
            respond(request, {"turn": {"id": turn, "status": "inProgress", "items": []}})
            if receiver:
                assert not roots and text.startswith("讨论转交 · 上下文摘要。"), (roots, text)
                assert "agent-written handoff" in context
                for name in ("tend_dispatch_issue", "tend_backlog_issue", "tend_answer_attention"):
                    result = read(request, thread, turn, name, name, {})
                    assert not result["success"] and "cannot run tools" in str(result), result
                answer = "Router 已收到讨论转交，请直接确认需要采取的行动。"
            elif case == "Read attached":
                issue_id, other_id, file_id, other_file_id = text.removeprefix("Read attached: ").split()
                resource = next(row for row in resources if row["reference"]["kind"] == "repo")
                checkout = Path(resource["checkout_path"])
                assert str(checkout) in roots and resource["status"] == "available"
                history = read(request, thread, turn, "history", "tend_read_discussion_history", {})
                assert history["success"], history
                history_text = json.loads(history["contentItems"][0]["text"])["messages"]
                assert any(row["text"] == "先聊聊演示项目的目标。" for row in history_text)
                assert all(row["text"] not in ("Dispatch from Chat.", text) for row in history_text)
                facts = read(request, thread, turn, "attached", "tend_read_discussion_reference",
                             {"reference_id": f"issue:{issue_id}"})
                assert facts["success"], facts
                assert json.loads(facts["contentItems"][0]["text"])["issue"]["issue_id"] == issue_id
                refused = read(request, thread, turn, "unattached", "tend_read_discussion_reference",
                               {"reference_id": f"issue:{other_id}"})
                assert not refused["success"], refused
                refused = read(request, thread, turn, "dispatch", "tend_dispatch_issue",
                               {"reference_id": f"issue:{issue_id}"})
                assert not refused["success"], refused
                delivery = read(request, thread, turn, "delivery", "tend_read_discussion_delivery",
                                {"reference_id": f"issue:{issue_id}", "file_id": file_id})
                assert delivery["success"], delivery
                assert json.loads(delivery["contentItems"][0]["text"])["text"] == "immutable fixture deliverable\n"
                refused = read(request, thread, turn, "other-delivery", "tend_read_discussion_delivery",
                               {"reference_id": f"issue:{issue_id}", "file_id": other_file_id})
                assert not refused["success"], refused
                pull = read(request, thread, turn, "pull", "tend_read_discussion_reference",
                            {"reference_id": "github:demo/quickstart:7"})
                assert pull["success"] and "Synthetic PR body" in pull["contentItems"][0]["text"], pull
                answer = mock_read(checkout / "README.md", roots)
            elif case in ("Hold discussion across archive", "Hold before quick handoff", "Hold before failed quick handoff"):
                if case == "Hold before quick handoff":
                    answer = "LATE_ASSISTANT_AFTER_HANDOFF_CUTOFF"
                if case == "Hold before failed quick handoff":
                    answer = "LATE_FAILED_NATIVE_ITEM_OUTSIDE_CUTOFF"
                (home / "archive-turn-started").touch()
                deadline = time.monotonic() + 15
                while not (home / "archive-turn-release").exists():
                    assert time.monotonic() < deadline, "fixture did not release the archived turn"
                    time.sleep(0.02)
            elif case == "Scope probe":
                expected = json.loads(text.removeprefix("Scope probe: "))
                actual = {path: mock_read(path, roots) for path in expected}
                assert actual == expected, (actual, expected, roots)
                answer = "Fixture scope matched."
            elif case == "Read mount":
                resource = next(row for row in resources if row["reference"]["kind"] == "worktree"
                                and row["status"] == "available")
                checkout = Path(resource["checkout_path"])
                assert str(checkout) in roots
                answer = mock_read(checkout / "README.md", roots)
            elif text.startswith("Write the Main Chat handoff message"):
                assert "Handoff request boundary" in context
                assert "user authorization" in context
                answer = ("请帮我比较合成样例的只读导出。已定首版用 CSV，"
                          "保留天数尚未决定，需要协助比较。样例数据不得离开本机。"
                          "只讨论方案，未授权实现或部署。")
                inherited = ledger.execute(
                    "SELECT text, history FROM turns WHERE thread_id=? ORDER BY rowid", (thread,)).fetchall()
                with ledger:
                    ledger.execute("INSERT INTO input_history VALUES (?,?)", (turn, json.dumps(inherited)))
                history_page = read(request, thread, turn, "quick-history", "tend_read_discussion_history", {})
                assert history_page["success"], history_page
                current = json.loads(history_page["contentItems"][0]["text"])["messages"]
                if "Feishu evidence explicitly selected" in context:
                    assert "applink.feishu.cn" in context
                    assert "不相关的群聊" not in context
                    additions = [m["text"] for m in current if m["role"] == "user" and "仅记录" in m["text"]]
                    answer = "飞书来源请核对 Android 灰度窗口。测试范围和回滚值仍待确认。没有实施授权。"
                    if "om_docs_anchor" in context:
                        answer = "飞书文档项已关闭，仅供主 Chat 参考。没有实施授权。"
                    elif "om_release_final" in context:
                        answer = "记录发布负责人消息，尚待确认。没有实施授权。"
                    else:
                        assert "om_qa_anchor" in context
                    if additions:
                        answer += f" 用户补充：{additions[-1]}"
                if any(m["text"].startswith("Fixture final instruction") for m in current):
                    answer = "CSV marker implementation request. The assistant recommends deployment as well."
                if any(m["text"].startswith("Fixture authorizes relay") for m in current):
                    answer = "Forward this to the existing Issue. The assistant also suggests deploying."
                for message in current:
                    if message["text"].startswith("Fixture authorizes Issue control "):
                        answer = message["text"].removeprefix("Fixture authorizes Issue control ")
                (home / "handoff-started").touch()
                deadline = time.monotonic() + 20
                while (home / "handoff-hold").exists():
                    assert time.monotonic() < deadline, "quick handoff fixture not released"
                    time.sleep(0.02)
                if (home / "handoff-invalid").exists():
                    answer = json.dumps({"invalid": True})
            elif text.startswith("Prepare an editable handoff draft"):
                answer = json.dumps({
                    "goal": "整理演示仓库的上手说明",
                    "decisions": "保留一个运行示例，读者是第一次使用的人",
                    "pending_items": "还需确认第一版的目录",
                    "constraints": "只使用已核对的安装命令",
                    "authorization_end": "先给出草案，等我确认，不部署",
                }, ensure_ascii=False)
            elif text == "看看我选中的资料，再建议第一版写什么。":
                answer = "从登记仓库找到启动示例。从 Issue 了解到，第一版只需要覆盖安装和首次运行。"
            elif case == "Recover discussion":
                answer = "恢复的是原讨论的同一条答复。"
            items = [{"type": "userMessage", "clientId": client_id},
                     {"type": "agentMessage", "id": f"{turn}-answer", "phase": "final_answer", "text": answer}]
            history = {"id": turn, "status": "failed" if case == "Hold before failed quick handoff"
                       else "completed", "items": items}
            with ledger:
                ledger.execute("INSERT INTO turns VALUES (?,?,?,?,?,?)",
                               (turn, thread, text, context, json.dumps(history), json.dumps(roots)))
            if metering:
                meter(thread, turn, receiver, text)
            if case == "Recover discussion" or (
                text.startswith("Write the Main Chat handoff message")
                and (home / "handoff-recover").exists()
            ):
                sys.exit(0)
            for item in items:
                event("item/completed", threadId=thread, turnId=turn, item=item)
            event("turn/completed", threadId=thread, turn={**history, "items": []})
        case _:
            raise AssertionError(request["method"])
