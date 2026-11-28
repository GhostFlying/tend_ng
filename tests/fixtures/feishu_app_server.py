"""Isolated JSON-RPC fixture for the named Feishu assessor."""
import fcntl
import json
import os
from pathlib import Path
import signal
import sys
import time
import uuid

root = Path(os.environ["HOME"])
home = Path(os.environ["CODEX_HOME"])
assert home == root / ".codex-ark"
assert not os.environ.get("TEND_TOKEN")
assert sys.argv[1:4] == ["app-server", "--listen", "stdio://"]
flags = dict(a.split("=", 1) for a in sys.argv[4:] if "=" in a)
assert flags["agents.enabled"] == "false" and flags["mcp_servers"] == "{}"
assert flags["default_permissions"] == '"tend-router-routing"'
assert flags["permissions.tend-router-routing.network.enabled"] == "false"


def result(request, body):
    print(json.dumps({"id": request["id"], "result": body}), flush=True)


for line in sys.stdin:
    request = json.loads(line)
    params = request.get("params", {})
    match request["method"]:
        case "initialize":
            result(request, {"codexHome": str(home)})
        case "initialized":
            pass
        case "config/read":
            result(request, {"config": {"approval_policy": "never", "web_search": "disabled",
                                       "agents": {"enabled": False}, "mcp_servers": {}, "features": {
                                           k.split(".", 1)[1]: False for k, v in flags.items()
                                           if k.startswith("features.") and v == "false"
                                       }}})
        case "thread/start":
            assert params["ephemeral"] is False and params["historyMode"] == "legacy"
            assert params["dynamicTools"] == []
            if (root / "system-meter-fixture").exists():
                assert params["experimentalRawEvents"]
            assert params["model"] == "deepseek-v4.1-flash"
            thread = str(uuid.uuid4())
            result(request, {"thread": {"id": thread, "ephemeral": False},
                             "model": "fixture-confirmed-model", "approvalPolicy": "never", "runtimeWorkspaceRoots": [],
                             "activePermissionProfile": {"id": "tend-router-routing"},
                             "sandbox": {"type": "readOnly", "networkAccess": False}})
            stall = root / "stall-next"
            if stall.exists():
                stall.unlink()
                capacity = fcntl.fcntl(sys.stdin.fileno(), fcntl.F_SETPIPE_SZ, 4096)
                (root / "stalled").write_text(f"{os.getpid()} {capacity}")
                while True:
                    signal.pause()
        case "turn/start":
            assert params["threadId"] == thread and params["effort"] == "high"
            assert params["permissions"] == "tend-router-routing"
            context = json.loads(params["input"][0]["text"])
            anchor = context["anchor"]["id"]
            assert "不相关的群聊" not in str(context)
            replies = context["replies"]
            if anchor == "om_qa_anchor":
                hold = root / "hold-qa-assessment"
                if hold.exists():
                    (root / "qa-assessment-has-input").touch()
                    while hold.exists():
                        time.sleep(0.01)
                headline = "确认 Android 灰度窗口与回滚阈值"
                overview = "候选包已通过冒烟，灰度比例先维持 5%。" + (
                    " QA 新增 Android 12 和 15 验收范围。" if len(replies) > 1 else "")
                questions = ["请 QA 确认触发回滚的阈值。"]
                disposition = "open"
            elif anchor == "om_docs_anchor":
                assert [reply["id"] for reply in replies] in (["om_docs_reply"], ["om_docs_reply", "om_docs_followup"])
                headline, overview, questions, disposition = "本地启动文档已补齐", "来源回复明确关闭此项。", [], "resolved"
            elif anchor == "om_release_anchor":
                assert len(context["mentions"]) in (2, 3)
                headline, overview, questions, disposition = "确认周五发布负责人", "发布窗口已确认。", ["由谁确认发布？"], "open"
            else:
                headline, overview, questions, disposition = "补齐另一个发布话题", "单独保留另一个 thread。", [], "unclear"
            with (root / "assessed.jsonl").open("a", encoding="utf-8") as log:
                log.write(json.dumps([anchor, context["revision"], [reply["id"] for reply in replies]]) + "\n")
            response = {"headline": headline, "overview": overview, "open_questions": questions,
                        "disposition": disposition, "evidence_ids": [anchor] + [reply["id"] for reply in replies[:11]]}
            turn = str(uuid.uuid4())
            result(request, {"turn": {"id": turn, "status": "inProgress", "items": []}})
            if (root / "system-meter-fixture").exists():
                assert params["threadId"] == thread
                for _ in range(2):
                    print(json.dumps({"method": "rawResponse/completed", "params": {
                        "threadId": thread, "turnId": turn, "responseId": "fixture-feishu-response",
                        "usage": {"totalTokens": 1220, "inputTokens": 1000, "outputTokens": 220},
                    }}), flush=True)
            item = {"type": "agentMessage", "id": turn + "-answer", "phase": "final_answer",
                    "text": json.dumps(response, ensure_ascii=False)}
            print(json.dumps({"method": "item/completed", "params": {"threadId": thread, "turnId": turn, "item": item}},
                             ensure_ascii=False), flush=True)
            print(json.dumps({"method": "turn/completed", "params": {
                "threadId": thread, "turn": {"id": turn, "status": "completed", "items": [item]}}},
                ensure_ascii=False), flush=True)
        case _:
            raise AssertionError(request)
