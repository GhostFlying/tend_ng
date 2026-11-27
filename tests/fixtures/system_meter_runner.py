"""One isolated Runner turn with direct, causal descendant and unrelated responses."""
import json
import os
from pathlib import Path
import sys
import uuid

home = Path(os.environ["CODEX_HOME"])
root = str(uuid.uuid4())
child = str(uuid.uuid4())
unrelated = str(uuid.uuid4())
folder = home / "sessions/2026/01/02"
folder.mkdir(parents=True, exist_ok=True)


def record(thread, kind, **payload):
    path = folder / f"rollout-2026-01-02T03-04-05-{thread}.jsonl"
    with path.open("a") as output:
        print(json.dumps({"type": kind, "payload": payload}), file=output)
    return str(path)


def send(method, **params):
    print(json.dumps({"method": method, "params": params}), flush=True)


def result(request, body):
    print(json.dumps({"id": request["id"], "result": body}), flush=True)


for line in sys.stdin:
    request = json.loads(line)
    match request["method"]:
        case "initialize":
            assert "rawResponseItem/completed" in request["params"]["capabilities"]["optOutNotificationMethods"]
            result(request, {})
        case "initialized":
            pass
        case "thread/start":
            assert request["params"]["experimentalRawEvents"]
            path = record(root, "session_meta", id=root, session_id=root, source="vscode")
            record(child, "session_meta", id=child, session_id=root,
                   source={"subagent": {"thread_spawn": {"parent_thread_id": root}}})
            result(request, {"thread": {"id": root, "path": path}})
        case "turn/start":
            root_turn = str(uuid.uuid4())
            result(request, {"turn": {"id": root_turn, "status": "inProgress", "items": []}})
            record(root, "turn_context", turn_id=root_turn, model="gpt-6.2-sol")
            for thread, turn, causal, count in [
                (root, root_turn, root_turn, 8000),
                (child, str(uuid.uuid4()), root_turn, 1350),
                (child, str(uuid.uuid4()), root_turn, 650),
                (unrelated, str(uuid.uuid4()), str(uuid.uuid4()), 999999),
            ]:
                send("turn/started", threadId=thread, turn={"id": turn, "rootTurnId": causal})
                for _ in range(2):
                    send("rawResponse/completed", threadId=thread, turnId=turn, responseId="shared-id",
                         responseModel="gpt-6.2-sol", usageMetadata={"metadata": {
                             "total_tokens": count, "input_tokens": count - 20, "output_tokens": 20,
                             "input_tokens_details": {"cached_tokens": 100},
                             "output_tokens_details": {"reasoning_tokens": 0},
                         }}, usage={"totalTokens": count, "inputTokens": count - 20,
                         "cachedInputTokens": 100, "cacheWriteInputTokens": 0,
                         "outputTokens": 20, "reasoningOutputTokens": 0})
                data = {"thread_id": thread, "turn_id": turn, "root_turn_id": causal,
                        "session_id": root, "response_id": "shared-id",
                        "usage": {"total_tokens": count, "input_tokens": count - 20, "cached_input_tokens": 100,
                                  "cache_write_input_tokens": 0, "output_tokens": 20, "reasoning_output_tokens": 0},
                        "thread_token_usage": {"total_tokens": 800_000}, "response_model": "gpt-6.2-sol"}
                record(thread, "token_usage_record", **data)
                if thread == child:
                    # Copied prefix in a child rollout is root evidence, not a new response.
                    record(thread, "token_usage_record", **{**data, "thread_id": root, "turn_id": root_turn})
            send("thread/tokenUsage/updated", threadId=root, turnId=root_turn,
                 tokenUsage={"last": {"totalTokens": 800000}, "total": {"totalTokens": 800000}})
            send("turn/completed", threadId=root, turn={"id": root_turn, "status": "completed", "items": []})
        case _:
            raise AssertionError(request["method"])
