"""Fixed dynamic-tool calls issued from original Router and Work sessions."""

import json
import os
import select
from pathlib import Path
import sys
import time
import uuid

location = Path(sys.argv[1])
router = not location.is_dir()
root = location.parent if router else location


def send(**data):
    print(json.dumps(data), flush=True)


def reply(request, result):
    send(id=request["id"], result=result)


def write(name, data):
    path = root / name
    temporary = path.with_suffix(f".tmp-{os.getpid()}")
    temporary.write_text(json.dumps(data))
    temporary.replace(path)


def answer_history(request):
    assert request["method"] == "thread/turns/list", request
    file = root / f"history-{request['params']['threadId']}.json"
    reply(request, {"data": [json.loads(file.read_text())] if file.exists() else [],
                    "nextCursor": None})


def gate(name):
    deadline = time.monotonic() + 40
    while not (root / name).exists():
        assert time.monotonic() < deadline, name
        if select.select([sys.stdin], [], [], 0.025)[0]:
            raw = sys.stdin.readline()
            if not raw:
                sys.exit(0)
            answer_history(json.loads(raw))


def tool(name, arguments, success=True, call_id=None, unlisted=False):
    if not unlisted:
        schema = tools[name]
        assert set(arguments).issuperset(schema.get("required", [])), (name, schema)
        for key, value in arguments.items():
            assert value in schema["properties"][key].get("enum", [value]), (name, key, value)
    call = str(uuid.uuid4())
    send(id=call, method="item/tool/call", params={
        "threadId": thread, "turnId": turn, "callId": call_id or call,
        "tool": name, "arguments": arguments,
    })
    response = json.loads(sys.stdin.readline())
    while "method" in response:
        answer_history(response)
        response = json.loads(sys.stdin.readline())
    assert response["id"] == call and response["result"]["success"] is success, (name, response)
    return json.loads(response["result"]["contentItems"][0]["text"])


def info():
    return {"pid": os.getpid(), "thread": thread, "turn": turn}


def work(name, category, **extra):
    return {"title": name, "objective": name, "category": category, **extra}


def route(name):
    result = info()
    result["directory"] = tool("tend_agent_candidates", {})["categories"]
    if name == "warm":
        assert "hot-work" not in {entry["id"] for entry in result["directory"]}
        route("delayed-owner")
        write("route-warm.json", result)
        gate("router-hot.release")
        route("no-candidate")
        gate("router-top.release")
        route("edited")
        route("hot-top")
        gate("router-disabled.release")
        route("disabled")
        return
    elif name in ("no-candidate", "hot-top", "economy", "coordinator"):
        category = {
            "economy": "implementation-economy", "coordinator": "hot-coordinator",
        }.get(name, "hot-work")
        result["candidates"] = tool("tend_agent_candidates", {"category": category})
        if name in ("hot-top", "economy", "coordinator"):
            assert result["candidates"]["candidates"], result
        if name == "no-candidate":
            assert result["candidates"]["candidates"] == [] and result["candidates"]["reason"]
            # Leave no fallback; use an impossible pin so the Issue stays
            # pending even after a real matching profile is published.
            result["dispatch"] = tool("tend_dispatch_issue", work(
                name, category, agent={"id": "not-registered", "runner_id": "hot"}))
        else:
            result["dispatch"] = tool("tend_dispatch_issue", work(name, category))
    elif name in ("owner", "delayed-owner", "new-owner"):
        result["dispatch"] = tool("tend_dispatch_issue", work(name, "early-work"))
    elif name == "disabled":
        result["error"] = tool("tend_dispatch_issue", work(name, "hot-work"), False)["error"]
        record = tool("tend_record_issue", work("future", "hot-work"))
        result["backlog_id"] = record["issue_id"]
        result["start_error"] = tool("tend_start_issue", {"issue_id": record["issue_id"]}, False)["error"]
    elif name == "scope":
        result["local"] = tool("tend_dispatch_issue", work(name, "feishu-router"), False)["error"]
        result["review"] = tool("tend_dispatch_issue", work(name, "hot-review"), False)["error"]
    elif name != "edited":
        # Background feedback is not a new dispatch.
        return
    write(f"route-{name}.json", result)


def execute(assignment, items):
    name = assignment["work"]["title"]
    result = {**info(), "assignment": assignment, "tools": sorted(tools)}
    if name in ("owner", "delayed-owner", "new-owner"):
        version = "new" if name == "new-owner" else "original"
        result["launcher"] = {
            "argument": sys.argv[2],
            "home": os.environ["CODEX_HOME"],
            "model": model,
            "effort": effort,
        }
        assert result["launcher"] == {
            "argument": version,
            "home": str(root / f"{version}-home"),
            "model": f"fixture-{version}",
            "effort": "high" if version == "new" else "low",
        }, result["launcher"]
    if name == "owner" and len(items) == 1:
        result["before"] = tool("tend_agent_candidates", {})["categories"]
        write("owner-before.json", result)
        gate("delegate.release")
        result["after"] = tool("tend_agent_candidates", {})["categories"]
        result["candidates"] = tool("tend_agent_candidates", {"category": "hot-work"})
        delegation = work("hot-child", "hot-work", isolated_workspace=True)
        result["child"] = tool("tend_delegate_work", delegation, call_id="same-delegation")
        result["review"] = tool("tend_delegate_work", work(
            "child-review", "hot-review", isolated_workspace=True))
        write("owner-delegated.json", result)
        gate("disable.release")
        result["disabled"] = tool("tend_delegate_work", work("must-not-create", "hot-work"), False)
        Path("review-input.txt").write_text("Category fixture review target.")
        artifact = tool("tend_submit_artifact", {"path": "review-input.txt"})
        result["disabled_review"] = tool("tend_request_review", {
            "candidate": f"artifact:{artifact['operation_id']}", "scope": "Review the supplied artifact",
        }, False)
        result["replayed"] = tool("tend_delegate_work", delegation, call_id="same-delegation")
        assert result["replayed"] == result["child"]
        result["attention"] = tool("tend_request_attention", {"question": "Continue original Owner?"})
        write("owner-waiting.json", result)
        return
    if name == "owner":
        context = json.loads(items[0]["text"].split("\n", 1)[1])
        assert any(item["kind"] == "attention_answered" for item in context["child_inputs"]), context
        result["catalogue"] = tool("tend_agent_candidates", {})["categories"]
        tool("tend_close_issue", {"summary": "Original Owner resumed"})
        write("owner-resumed.json", result)
        return
    if name in ("hot-child", "child-review"):
        assert "tend_delegate_work" not in tools
        assert "tend_report_result" not in tools
        result["forbidden"] = tool(
            "tend_delegate_work", work("must-not-create", "hot-work"), False, unlisted=True)
    if name == "child-review":
        assert "tend_request_review" not in tools
        result["review_forbidden"] = tool("tend_request_review", {}, False, unlisted=True)
    if name == "coordinator":
        assert "tend_delegate_work" in tools
        assert "tend_submit_artifact" not in tools
        result["forbidden"] = tool("tend_submit_artifact", {"path": "anything"}, False, unlisted=True)
    if name in ("hot-top", "economy"):
        assert "tend_submit_artifact" in tools and "tend_report_result" in tools
    # Do not close the children while the Owner is testing its own Attention.
    write(f"work-{name}.json", result)


for raw in sys.stdin:
    request = json.loads(raw)
    params = request.get("params", {})
    match request["method"]:
        case "initialize":
            reply(request, {})
        case "initialized":
            continue
        case "thread/start":
            thread = str(uuid.uuid4())
            model = params.get("model")
            tools = {tool["name"]: tool["inputSchema"] for tool in params["dynamicTools"]}
            write(f"tools-{thread}.json", tools)
            reply(request, {"thread": {"id": thread}})
        case "thread/resume":
            thread = params["threadId"]
            model = params.get("model")
            tools = json.loads((root / f"tools-{thread}.json").read_text())
            reply(request, {"thread": {"id": thread, "status": {"type": "idle"}}})
        case "turn/start":
            turn = str(uuid.uuid4())
            client_id = params["clientUserMessageId"]
            items = params["input"]
            effort = params.get("effort")
            reply(request, {"turn": {"id": turn, "status": "inProgress", "items": []}})
            if router:
                text = items[-1]["text"]
                if text.startswith("category fixture "):
                    route(text.removeprefix("category fixture "))
                elif text.startswith("Tend Attention for the user's original topic.\n"):
                    fact = json.loads(text.split("\n", 1)[1])
                    tool("tend_publish_attention", {"attention_id": fact["attention_id"],
                                                     "text": fact["request"]["question"]})
            else:
                execute(json.loads(items[-1]["text"]), items)
            history = {"id": turn, "status": "completed", "items": [
                {"type": "userMessage", "clientId": client_id},
                {"type": "agentMessage", "id": f"{turn}-answer", "phase": "final_answer",
                 "text": "Category fixture done."},
            ]}
            write(f"history-{thread}.json", history)
            send(method="turn/completed", params={"threadId": thread, "turn": history})
        case "thread/turns/list":
            answer_history(request)
        case other:
            raise AssertionError(other)
