import json
import sqlite3
import sys
import time
import uuid
from pathlib import Path

cases = json.loads(Path(__file__).with_name("chat.json").read_text())
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


def dispatch(thread_id, turn_id, token, *, remaining=False):
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
    assert [repeated[key] for key in ("issue_id", "task_id", "operation_id")] == [
        receipt[key] for key in ("issue_id", "task_id", "operation_id")
    ], repeated
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
            if text.startswith("Tend Issue report for the user's original topic.\n"):
                report = json.loads(text.split("\n", 1)[1])
                assert report["original_request"] == "Dispatch from Chat."
                assert report["summary"] == "The requested guide is ready."
                [file] = report["artifacts"]
                assert file["name"] == "guide.txt"
                case = {"answer": "The guide is ready. Download the attached guide.txt.",
                        "drop_before_start": 1}
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
            if case.get("dispatch"):
                if starts == 1:
                    receipt = dispatch(thread_id, turn_id, params["clientUserMessageId"])
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
                answer = f"Issue {receipt['issue_id']} is waiting for assignment."
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
