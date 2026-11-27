"""Emit native Codex usage notifications for isolated Issue tree Runs."""
import json
from pathlib import Path
import re
import sys
import time


def read(method):
    call = json.loads(sys.stdin.readline())
    assert call["method"] == method, call
    return call


def send(message):
    print(json.dumps(message), flush=True)


def respond(call, result):
    send({"id": call["id"], "result": result})


def native_line(kind, payload):
    with rollout.open("a") as ledger:
        print(json.dumps({"type": kind, "payload": payload}), file=ledger)


def evidence(cumulative, last, response, thread_id=None, turn_id=None, response_model=None):
    counts = {re.sub(r"[A-Z]", lambda m: "_" + m[0].lower(), key): value for key, value in last.items()}
    payload = {
        "thread_id": thread_id or thread, "turn_id": turn_id or turn,
        "response_id": response, "usage": counts,
        "thread_token_usage": {"total_tokens": cumulative},
    }
    if response_model is not None:
        payload["response_model"] = response_model
    native_line("token_usage_record", payload)


def usage(cumulative, last, thread_id=None, turn_id=None, response=None, response_model=None):
    if response:
        evidence(cumulative, last, response, thread_id, turn_id, response_model)
    send({"method": "thread/tokenUsage/updated", "params": {
        "threadId": thread_id or thread, "turnId": turn_id or turn,
        "tokenUsage": {"total": {"totalTokens": cumulative}, "last": last,
                       "modelContextWindow": 200_000},
    }})


def tool(name, key, arguments):
    send({"id": key, "method": "item/tool/call", "params": {
        "threadId": thread, "turnId": turn, "callId": key, "tool": name, "arguments": arguments,
    }})
    response = json.loads(sys.stdin.readline())
    assert response["id"] == key and response["result"]["success"], response
    return json.loads(response["result"]["contentItems"][0]["text"])


root = Path(sys.argv[1])
stop_case = sys.argv[2:] == ["stop"]
respond(read("initialize"), {})
read("initialized")
start = read("thread/start")
thread = f"fixture-usage-thread-{Path.cwd().name}"
rollout = root / f"rollout-live-{thread}.jsonl"
respond(start, {"thread": {"id": thread, "path": str(rollout)}})
call = read("turn/start")
assignment = json.loads(call["params"]["input"][-1]["text"])
turn = f'fixture-usage-turn-{call["params"]["clientUserMessageId"]}'
role = assignment["placement"]["agent"]["category"]
title = assignment["work"]["title"]
if role == "issue-router" and not stop_case:
    # A notification can beat the response that establishes the turn identity.
    first = {"totalTokens": 24_800, "inputTokens": 24_000, "outputTokens": 800,
             "cachedInputTokens": 8_192, "reasoningOutputTokens": 0}
    usage(24_800, first, turn_id="unrelated-turn", response="other-turn")
    usage(24_800, first)
respond(call, {"turn": {"id": turn, "status": "inProgress", "items": []}})

if stop_case:
    usage(10, {"inputTokens": 9, "outputTokens": 1, "totalTokens": 10}, response="before-stop")
    interrupt = read("turn/interrupt")
    assert interrupt["params"] == {"threadId": thread, "turnId": turn}
    usage(200, {"totalTokens": 200}, thread_id="unrelated-thread", response="wrong-stop-thread")
    usage(300, {"totalTokens": 300}, turn_id="unrelated-turn", response="wrong-stop-turn")
    usage(30, {"inputTokens": 19, "outputTokens": 1, "totalTokens": 20}, response="during-interrupt")
    respond(interrupt, {})
    # Stop does not need a terminal notification to settle the stopped child.
    sys.exit(0)

if role == "issue-router":
    native_line("turn_context", {"turn_id": turn, "model": "gpt-6.2-sol"})
    usage(24_800, first)
    # This ordered round trip proves the Runner processed the notices while
    # there was still no matching append. After appending, send no new frames
    # until the test sees the persisted usage and requests Steer.
    tool("tend_agent_candidates", "usage-observed", {"category": "implementation"})
    (root / "notice-consumed").touch()
    deadline = time.monotonic() + 18
    while not (root / "append-evidence").exists():
        assert time.monotonic() < deadline, "test did not release the original evidence"
        time.sleep(.02)
    evidence(24_800, first, "parent-first", response_model="gpt-6.2-sol")
    steer = read("turn/steer")
    assert steer["params"]["threadId"] == thread and steer["params"]["expectedTurnId"] == turn
    assert steer["params"]["clientUserMessageId"] == "usage-live-steer"
    usage(41_200, {"totalTokens": 16_400, "inputTokens": 16_000, "outputTokens": 400,
                   "cachedInputTokens": 4_096, "cacheWriteInputTokens": 2_000, "reasoningOutputTokens": 0},
          response="parent-during-steer", response_model="gpt-6.2-astra")
    respond(steer, {"turnId": turn})
    # Both are real shapes of TokenCount, but neither proves a model request.
    usage(41_200, {"totalTokens": 10_000, "inputTokens": 0, "outputTokens": 0})
    usage(200_000, {"totalTokens": 158_800, "inputTokens": 0, "outputTokens": 0})
    for key, child in [("missing", "未收到用量的文档检查"), ("metered", "整理安装步骤与首次运行指南")]:
        child_result = tool("tend_delegate_work", f"delegate-{key}", {
            "category": "implementation", "title": child, "objective": "隔离环境中的用量统计验收。",
            "isolated_workspace": True,
        })
        (root / f"{key}.json").write_text(json.dumps(child_result))
elif title == "整理安装步骤与首次运行指南":
    native_line("turn_context", {"turn_id": turn, "model": "deepseek-v4.1-flash"})
    (root / "metered-active").touch()
    deadline = time.monotonic() + 18
    while not (root / "release-usage").exists():
        assert time.monotonic() < deadline, "test did not release the worker"
        time.sleep(.02)
    first = {"totalTokens": 82_400, "inputTokens": 80_000, "cachedInputTokens": 20_000, "cacheWriteInputTokens": 0,
             "outputTokens": 2_400, "reasoningOutputTokens": 700}
    usage(82_400, first, thread_id="unrelated-thread", response="other-thread")
    evidence(82_400, first, "worker-first", response_model="deepseek-v4.1-flash")
    send({"method": "rawResponse/completed", "params": {
        "threadId": thread, "turnId": turn, "responseId": "worker-first",
        "responseModel": "deepseek-v4.1-flash", "usage": first,
        "usageMetadata": {"metadata": {"total_tokens": 82400, "input_tokens": 80000,
            "output_tokens": 2400, "input_tokens_details": {"cached_tokens": 20000, "cache_write_tokens": 0},
            "output_tokens_details": {"reasoning_tokens": 700}}},
    }})
    usage(86_000, {"totalTokens": 3_600, "inputTokens": 3_200, "outputTokens": 400}, response="worker-second")
    usage(90_000, {"totalTokens": 4_000}, turn_id="unrelated-turn")

send({"method": "turn/completed", "params": {"threadId": thread,
      "turn": {"id": turn, "status": "completed", "items": []}}})
