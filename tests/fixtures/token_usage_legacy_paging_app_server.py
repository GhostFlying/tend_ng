"""Ordinary App Server frames and sealed rollout, including Codex's optional defaults."""
import json
import os
from pathlib import Path
import sys
import uuid

thread = str(uuid.uuid4())
turn = str(uuid.uuid4())
folder = Path(os.environ["CODEX_HOME"]) / "sessions/2026/01/02"
folder.mkdir(parents=True, exist_ok=True)
rollout = folder / f"rollout-2026-01-02T03-04-05-{thread}.jsonl"
counts = {"total_tokens": 5, "input_tokens": 4, "output_tokens": 1,
          "cached_input_tokens": 0, "cache_write_input_tokens": 0, "reasoning_output_tokens": 0}
usage = {"totalTokens": 5, "inputTokens": 4, "outputTokens": 1,
         "cachedInputTokens": 0, "cacheWriteInputTokens": 0, "reasoningOutputTokens": 0}


def notify(method, **params):
    print(json.dumps({"method": method, "params": params}), flush=True)


def reply(request, **result):
    print(json.dumps({"id": request["id"], "result": result}), flush=True)


for line in sys.stdin:
    request = json.loads(line)
    match request["method"]:
        case "initialize":
            reply(request)
        case "initialized":
            pass
        case "thread/start":
            reply(request, thread={"id": thread, "path": str(rollout)})
        case "turn/start":
            with rollout.open("w") as file:
                print(json.dumps({"type": "session_meta", "payload": {"id": thread}}), file=file)
                for i in range(70):
                    print(json.dumps({"type": "token_usage_record", "payload": {
                        "thread_id": thread, "turn_id": turn, "response_id": f"response-{i:02}",
                        "usage": counts, "thread_token_usage": {"total_tokens": (i + 1) * 5},
                    }}), file=file)
            reply(request, turn={"id": turn, "status": "inProgress", "items": []})
            notify("thread/tokenUsage/updated", threadId=thread, turnId=turn,
                   tokenUsage={"last": usage, "total": {"totalTokens": 5}})
            notify("turn/completed", threadId=thread, turn={"id": turn, "status": "completed", "items": []})
        case _:
            raise AssertionError(request["method"])
