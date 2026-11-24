import json
import sqlite3
import sys
import time
import uuid
from pathlib import Path

cases = json.loads(Path(__file__).with_name("chat.json").read_text())
database = sqlite3.connect(sys.argv[1], timeout=5)
database.executescript("""
    CREATE TABLE IF NOT EXISTS threads (id TEXT PRIMARY KEY);
    CREATE TABLE IF NOT EXISTS turns (
        seq INTEGER PRIMARY KEY,
        thread_id TEXT NOT NULL,
        turn TEXT NOT NULL,
        held INTEGER NOT NULL
    );
""")
release = Path(sys.argv[1]).with_suffix(".release")


def respond(request, payload):
    print(json.dumps({"id": request["id"], "result": payload}), flush=True)


def notify(method, **params):
    print(json.dumps({"method": method, "params": params}), flush=True)


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
                database.execute("INSERT INTO threads VALUES (?)", (thread_id,))
            respond(request, {"thread": {"id": thread_id}})
        case "thread/resume":
            assert database.execute(
                "SELECT 1 FROM threads WHERE id = ?", (params["threadId"],)
            ).fetchone()
            respond(request, {"thread": {"id": params["threadId"]}})
        case "turn/start":
            thread_id = params["threadId"]
            turn_id = str(uuid.uuid4())
            text = params["input"][0]["text"]
            case = cases[text]
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
                        "phase": "final_answer", "text": case["answer"],
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
            )
            turns = []
            for encoded, held in rows:
                turn = json.loads(encoded)
                if held and not release.exists():
                    turn.update(status="inProgress", items=turn["items"][:1])
                turns.append(turn)
            respond(request, {"data": turns, "nextCursor": None})
        case _:
            raise AssertionError(request["method"])
