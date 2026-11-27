"""Fixed App Server choices that exercise main Chat feedback, not model judgment."""

import json
from pathlib import Path
import subprocess
import sys
import time
import uuid

root = Path(sys.argv[1]).parent
case = json.loads(Path(__file__).with_name("router_feedback.json").read_text())
work = {
    "title": case["issue_title"],
    "objective": case["dispatch_request"],
    "repo_ids": ["lynx"],
    "category": "investigation",
}


def emit(message):
    print(json.dumps(message), flush=True)


def respond(request, result):
    emit({"id": request["id"], "result": result})


def hold(name):
    deadline = time.monotonic() + 50
    while not (root / name).exists():
        assert time.monotonic() < deadline, name
        time.sleep(0.02)


for line in sys.stdin:
    request = json.loads(line)
    params = request.get("params", {})
    match request["method"]:
        case "initialize":
            respond(request, {})
        case "initialized":
            pass
        case "thread/start":
            instructions = params["developerInstructions"]
            for rule in (
                "prefer routing it to an investigation Agent",
                "not keywords",
                "Reuse a matching Issue",
                "call tend_publish_progress before starting the multi-step tool work",
                "Do not rely on native commentary",
                "Acceptance does not mean the Runner claimed the work",
            ):
                assert rule in instructions, rule
            assert any(t["name"] == "tend_publish_progress" for t in params["dynamicTools"])
            thread = str(uuid.uuid4())
            respond(request, {"thread": {"id": thread}})
        case "thread/resume":
            lost = root / "lost-native-thread"
            thread = params["threadId"]
            if lost.exists() and lost.read_text() == thread:
                (root / "native-history-missing").touch()
                emit({"id": request["id"], "error": {
                    "code": -32600, "message": f"no rollout found for thread id {thread}",
                }})
            else:
                respond(request, {"thread": {"id": thread}})
        case "thread/turns/list":
            # Model an attempt with no recoverable native terminal turn.
            respond(request, {"data": [], "nextCursor": None})
        case "turn/start":
            thread = params["threadId"]
            client = params["clientUserMessageId"]
            turn = str(uuid.uuid4())
            text = params["input"][-1]["text"]
            context = "\n".join(item["text"] for item in params["input"][:-1])
            respond(request, {"turn": {"id": turn, "status": "inProgress", "items": []}})

            def notify(method, **fields):
                emit({"method": method, "params": {"threadId": thread, "turnId": turn, **fields}})

            def tool(name, arguments, *, call_id=None, success=True):
                rpc = str(uuid.uuid4())
                emit({"id": rpc, "method": "item/tool/call", "params": {
                    "threadId": thread, "turnId": turn, "callId": call_id or name,
                    "tool": name, "arguments": arguments,
                }})
                response = json.loads(sys.stdin.readline())
                assert response["id"] == rpc and response["result"]["success"] is success, response
                return json.loads(response["result"]["contentItems"][0]["text"])

            def mark_or_read(label, result):
                file = root / label
                if file.exists():
                    assert json.loads(file.read_text()) == result, result
                    return False
                draft = file.with_suffix(".tmp")
                draft.write_text(json.dumps(result))
                draft.replace(file)
                return True

            item = {"type": "userMessage", "clientId": client}
            if text == case["dispatch_request"]:
                saved = root / "dispatch-held"
                if not saved.exists():
                    receipt = tool("tend_dispatch_issue", work)
                    assert receipt == tool("tend_dispatch_issue", work)
                    mark_or_read(saved.name, receipt)
                    hold("release-dispatch")
                    sys.exit(0)
                receipt = json.loads(saved.read_text())
                assert receipt == tool("tend_read_dispatch", {"operation_id": receipt["operation_id"]})
                assert json.loads(context.splitlines()[-1])["committed_dispatches"] == [receipt]
                answer = "TEND_NO_UPDATE"
            elif text == case["multi_request"]:
                first = tool("tend_dispatch_issue", {**work, "title": "核对样例分类规则"},
                             call_id="dispatch-rules")
                second = tool("tend_dispatch_issue", {**work, "title": "核对样例时间窗口"},
                              call_id="dispatch-window")
                mark_or_read("multi-receipts", [first, second])
                answer = "TEND_NO_UPDATE"
            elif text == case["cold_request"]:
                replies = json.loads((root / "multi-receipts").read_text())
                lines = context.splitlines()
                exchange = json.loads(next(lines[i + 1] for i, line in enumerate(lines)
                                           if line.startswith("Recent settled messages")))
                facts = json.loads(next(lines[i + 1] for i, line in enumerate(lines)
                                        if line.startswith("Recent work acceptances")))
                source = replies[0]["source_message_id"]
                assert {"author": "user", "text": case["multi_request"], "outcome": "completed",
                        "message_id": source} in exchange, exchange
                assert case["progress"] not in context and case["dispatch_acceptance"] not in context
                accepted = [fact for fact in facts if fact["source_message_id"] == source]
                assert [(fact["issue_id"], fact["issue_number"]) for fact in accepted] == [
                    (receipt["issue_id"], receipt["short_number"]) for receipt in replies
                ], accepted
                mark_or_read("cold-acceptances", accepted)
                first_num, second_num = [fact["issue_number"] for fact in accepted]
                answer = f"已受理 #{first_num} 和 #{second_num}，单凭回执不能确认 Agent 已领取。"
            elif text in (case["relay_request"], case["second_issue_reply"], case["relay_receipt_reply"]):
                reference = json.loads(context.splitlines()[-1])
                [number] = reference["issue_numbers"]
                if text != case["relay_request"]:
                    _, selected = json.loads((root / "multi-receipts").read_text())
                    assert reference["issue_ids"] == [selected["issue_id"]], reference
                    assert number == selected["short_number"], reference
                args = {"issue_id": f"#{number}"}
                saved = tool("tend_relay_issue_message", args)
                assert saved == tool("tend_relay_issue_message", args)
                answer = "TEND_NO_UPDATE"
            elif text == case["self_request"]:
                notify("item/completed", item={"type": "agentMessage", "id": f"{turn}-early-commentary",
                       "phase": "commentary", "text": "不会在主 Chat 展示的原生 commentary"})
                progress = tool("tend_publish_progress", {"text": case["progress"]})
                assert progress == tool("tend_publish_progress", {"text": "不应成为第二条进展。"},
                                        call_id="new-native-call")
                if mark_or_read("progress-receipt", progress):
                    command = {"type": "commandExecution", "id": f"{turn}-compare",
                               "command": "compare synthetic weekly inputs", "status": "inProgress"}
                    notify("item/started", item=command)
                    # Run real local work, held inside the tool. No Lynx repository or service is used.
                    code = """
import json, os, time
from pathlib import Path
Path('tool-running.tmp').write_text(str(os.getpid()))
Path('tool-running.tmp').replace('tool-running')
deadline = time.monotonic() + 50
while not Path('release-progress').exists():
    assert time.monotonic() < deadline
    time.sleep(0.02)
before, after = (json.loads(Path(p).read_text()) for p in ('before.json', 'after.json'))
print(json.dumps({k: (v, after[k]) for k, v in before.items() if v != after[k]}))
"""
                    changed = subprocess.check_output([sys.executable, "-c", code], cwd=root, timeout=52)
                    assert json.loads(changed) == {"构建失败": [3, 1]}
                    notify("item/completed", item={**command, "status": "completed"})
                    # Commit on the Tend side survived losing both native history and final answer.
                    sys.exit(0)
                assert json.loads(context.splitlines()[-1])["attempt_number"] == 2
                answer = case["answer"]
            elif text == "派发失败场景":
                tool("tend_dispatch_issue", {**work, "title": ""}, success=False)
                (root / "failure-held").touch()
                hold("release-failure")
                answer = "派发未成功。"
            elif text == "先记作待办":
                tool("tend_record_issue", work)
                answer = "已保存为待办。"
            elif text == "你好":
                answer = "你好。"
            elif text == "其他 Chat 的请求":
                tool("tend_publish_progress", {"text": "不应发布。"}, success=False)
                answer = "这里不发布主 Chat 进展。"
            else:
                raise AssertionError(text)
            notify("item/completed", item=item)
            notify("item/completed", item={"type": "agentMessage", "id": f"{turn}-commentary",
                   "phase": "commentary", "text": "不会在主 Chat 展示的原生 commentary"})
            notify("item/completed", item={"type": "agentMessage", "id": f"{turn}-answer",
                   "phase": "final_answer", "text": answer})
            notify("turn/completed", turn={"id": turn, "status": "completed", "items": []})
        case _:
            raise AssertionError(request["method"])
