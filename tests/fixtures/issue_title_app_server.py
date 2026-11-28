"""Fixed Router, coordinator and worker turns for the isolated Issue rename test."""
import json
from pathlib import Path
import sys
import time
import uuid

RENAME = "tend_rename_issue"
RECORD = "为稍后准备发布说明留下记录。"
ROOT = "协调一份示例仓库的使用说明。"
ROOT_ORIGINAL = "整理示例仓库说明"
ROOT_RENAMED = "协调安装说明并核对首次运行"
CHILD_ORIGINAL = "准备示例仓库使用说明"
CHILD_OBJECTIVE = "整理安装与首次运行步骤，保留原始验证命令。"
CHILD_FIRST = "补齐安装命令并验证首次运行"
CHILD_SECOND = "核对安装步骤与首次运行示例"
CHILD_USER = "核对示例仓库的安装步骤与首次运行命令"
LEGACY = "核对老版本是否能继续处理已领工作。"
BUDGET = "Verify bounded title changes and ordered record page splitting."
TITLE_BYTES = 1024


def boundary_title(index):
    prefix = f"{index}:"
    return prefix + '"' * (TITLE_BYTES - len(prefix))


def receive(method=None):
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    request = json.loads(line)
    assert method is None or request["method"] == method, request
    return request


def send(**message):
    print(json.dumps(message), flush=True)


def reply(request, result):
    send(id=request["id"], result=result)


def wait(name):
    deadline = time.monotonic() + 25
    while not (root / name).exists():
        assert time.monotonic() < deadline, f"waiting for {name}"
        time.sleep(.02)


def save(name, value):
    target = root / name
    temp = target.with_suffix(".partial")
    temp.write_text(json.dumps(value))
    temp.replace(target)


def call(name, arguments, *, call_id=None, success=True):
    rpc = str(uuid.uuid4())
    send(id=rpc, method="item/tool/call", params={
        "threadId": thread, "turnId": turn, "callId": call_id or rpc,
        "tool": name, "arguments": arguments,
    })
    result = receive()
    assert result["id"] == rpc and result["result"]["success"] is success, result
    return json.loads(result["result"]["contentItems"][0]["text"])


def serve_router():
    assert RENAME not in tools
    if text == RECORD:
        original = "稍后撰写发布说明"
        receipt = call("tend_record_issue", {"title": original, "objective": RECORD})
        save("recorded.json", receipt)
        wait("read-recorded-receipt")
        replay = call("tend_read_dispatch", {"operation_id": receipt["operation_id"]})
        assert replay["title"] == original, replay
        save("recorded-replayed.json", replay)
    elif text == ROOT:
        receipt = call("tend_dispatch_issue", {
            "title": ROOT_ORIGINAL, "objective": ROOT, "repo_ids": ["example"],
        })
        save("root-dispatch.json", receipt)
        call(RENAME, {"title": "Router cannot rename"}, success=False)
        wait("read-root-receipt")
        replay = call("tend_read_dispatch", {"operation_id": receipt["operation_id"]})
        assert replay["title"] == ROOT_ORIGINAL, replay
        save("root-replayed.json", replay)
    elif text in (LEGACY, BUDGET):
        title = "Compatibility fixture" if text == LEGACY else "Verify title transport bounds"
        call("tend_dispatch_issue", {"title": title, "objective": text, "repo_ids": ["example"]})
    else:
        raise AssertionError(text)


def check_budget(assignment):
    save("budget-started.json", assignment)
    wait("exercise-budget")
    for key, oversized in (("oversized-title", "x" * (256 * 1024)),
                           ("multibyte-title", "题" * (TITLE_BYTES // 3 + 1))):
        error = call(RENAME, {"title": oversized}, call_id=key, success=False)
        assert "1024 UTF-8 bytes" in error["error"], error

    operations = []
    # Each record fits by itself. Their mix exceeds 256 KiB before the
    # existing count limit, even while titles remain individually valid.
    for index in range(6):
        # Include trim and JSON's worst-case title escaping near the limit.
        operations.append(call(RENAME, {"title": " " + boundary_title(index) + " "}))
        operations.append(call("tend_post_issue_message", {"text": f"{index}:" + "x" * (60 * 1024)}))
    operations.append(call("tend_close_issue", {"summary": "Legal close after refused titles."}))
    save("budget-operations.json", operations)


def serve_worker(assignment):
    if assignment["work"]["objective"] == BUDGET:
        check_budget(assignment)
        return
    if assignment["work"]["objective"] == RECORD:
        save("started-record.json", assignment)
        return
    if assignment["work"]["objective"] == LEGACY:
        label = "legacy-worker" if (root / "old-runner").exists() else "new-worker"
        enabled = (root / "expect-rename").exists()
        assert (RENAME in tools) is enabled, tools
        if enabled:
            call(RENAME, {"title": "Renamed after Server upgrade"})
        elif label == "new-worker":
            denied = call(RENAME, {"title": "Do not send to old Server"}, success=False)
            assert "not advertised" in denied["error"], denied
        call("tend_close_issue", {"summary": "Compatibility work processed."})
        save(f"{label}-{enabled}.json", assignment)
        return

    assert set(tools[RENAME]["inputSchema"]["properties"]) == {"title"}
    if "parent_issue_id" not in assignment:
        assert assignment["work"]["title"] == ROOT_ORIGINAL
        save("root-rename.json", call(RENAME, {"title": ROOT_RENAMED}))
        child = call("tend_delegate_work", {
            "category": "implementation", "title": CHILD_ORIGINAL, "objective": CHILD_OBJECTIVE,
        })
        save("delegated.json", child["action"])
        return

    assert assignment["work"]["title"] == CHILD_ORIGINAL
    denied = call(RENAME, {"title": "Wrong target", "issue_id": assignment["parent_issue_id"]},
                  success=False)
    assert "issue_id" in denied["error"], denied
    call(RENAME, {"title": "   "}, success=False)
    call(RENAME, {"title": "two\nlines"}, success=False)
    save("worker-contract.json", assignment)
    wait("rename-online")
    args = {"title": f"  {CHILD_FIRST}  "}
    first = call(RENAME, args, call_id="original-rename")
    assert call(RENAME, args, call_id="original-rename") == first
    call(RENAME, {"title": "Conflict"}, call_id="original-rename", success=False)
    # Different native call, same set-title intent. Preserve a receipt without a history row.
    same = call(RENAME, {"title": CHILD_FIRST})
    assert same["operation_id"] != first["operation_id"]
    save("online.json", [first, same])
    wait("rename-offline")
    save("offline.json", call(RENAME, {"title": CHILD_SECOND}))
    wait("finish-worker")
    call("tend_close_issue", {"summary": "Fixture worker completed."})


def main():
    global root, thread, turn, tools, text
    first_arg = Path(sys.argv[1])
    router = first_arg.name == "app.sqlite"
    root = first_arg.parent if router else first_arg
    reply(receive("initialize"), {})
    receive("initialized")
    start = receive()
    if start["method"] == "thread/resume":
        wanted = start["params"]["threadId"]
        saved = root / f"tools-{wanted}.json"
        if router and saved.exists():
            thread = wanted
            tools = json.loads(saved.read_text())
            reply(start, {"thread": {"id": thread}})
            start = None
        else:
            send(id=start["id"], error={"code": -32600, "message": f"no rollout found for thread id {wanted}"})
            start = receive("thread/start")
    if start is not None:
        assert start["method"] == "thread/start", start
        tools = {entry["name"]: entry for entry in start["params"]["dynamicTools"]}
        thread = str(uuid.uuid4())
        if router:
            save(f"tools-{thread}.json", tools)
        reply(start, {"thread": {"id": thread}})
    while True:
        run_turn(router)
        if not router:
            return


def run_turn(router):
    global turn, text
    request = receive("turn/start")
    turn = str(uuid.uuid4())
    params = request["params"]
    text = params["input"][-1]["text"]
    reply(request, {"turn": {"id": turn, "status": "inProgress", "items": []}})
    if router:
        incoming = {"type": "userMessage", "clientId": params["clientUserMessageId"], "content": []}
        send(method="item/started", params={"threadId": thread, "turnId": turn, "item": incoming})
        serve_router()
    else:
        serve_worker(json.loads(text))
    send(method="turn/completed", params={"threadId": thread, "turn": {
        "id": turn, "status": "completed",
        "items": [{"type": "userMessage", "clientId": params["clientUserMessageId"]}],
    }})


if __name__ == "__main__":
    main()
