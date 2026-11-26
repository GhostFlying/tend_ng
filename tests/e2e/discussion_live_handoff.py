"""Summarize synthetic context with a real, explicitly configured Discussion Codex."""
import argparse
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import time

from chat import server
from discussions import sql

PROMPTS = (
    "这是全新的合成验收讨论。目标是比较「纸鸢」项目的只读导出方案，方便在本地检查模拟数据。"
    "已决定首版设计用 CSV，不用 JSON。请先复述，不要开始任何工作。",
    "补充：导出的文件不能离开本机，也不能使用真实用户数据。保留 7 天还是 30 天尚未决定，"
    "请把两项都保留为待定。授权仅限讨论和比较方案，未授权实现、改代码、执行迁移或部署。"
    "请主 Chat 帮我比较这两个保留期限的取舍，但不要替我决定或触发实施。",
    "顺便问一下不相干的题外话：虚构城市南船的天气预报和午饭菜单会是什么？"
    "界面上还写着「转交主 Chat」。天气、菜单和这个按钮都不是要转给主 Chat 的任务。",
)


def wait_for(read, done, timeout=180):
    deadline = time.monotonic() + timeout
    result = None
    while time.monotonic() < deadline:
        result = read()
        if done(result):
            return result
        time.sleep(0.35)
    raise AssertionError(f"live model did not settle: {result}")


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "cli"):
        parser.add_argument(name, type=lambda value: Path(value).resolve())
    for name in ("codex-home", "codex-launcher", "evidence"):
        parser.add_argument("--" + name, required=True, type=lambda value: Path(value).resolve())
    parser.add_argument("--model", required=True)
    parser.add_argument("--effort", default="high")
    args = parser.parse_args()
    assert args.codex_home.is_dir() and args.codex_launcher.is_file()
    sessions = args.codex_home / "sessions"
    assert not sessions.exists() or not any(sessions.rglob("*.jsonl")), "use a fresh isolated Codex home"
    args.evidence.mkdir(parents=True, exist_ok=True)
    save(args.evidence / "inputs.json", {"model": args.model, "effort": args.effort, "prompts": PROMPTS})
    with tempfile.TemporaryDirectory(prefix="tend-live-discussion-") as directory:
        root = Path(directory)
        cwd = root / "empty-discussion-cwd"
        cwd.mkdir()
        config = root / "discussion.json"
        config.write_text(json.dumps({
            "host_id": "live-discussion-test", "program": str(args.codex_launcher),
            "codex_home": str(args.codex_home), "cwd": str(cwd),
            "model": args.model, "effort": args.effort,
        }))
        # The configured Discussion launcher handles both native turns. The
        # ordinary Router is backed by a fixture and must not be started here.
        env = {**os.environ, "TEND_TOKEN": "synthetic-live-discussion"}
        db = root / "server.sqlite"
        with server(args.server, root, env, discussion_config=config) as address:
            save(args.evidence / "environment.json", {
                "temporary_server": f"http://{address}", "native_discussion_launcher": args.codex_launcher.name,
                "authenticated_home_fresh": True, "model": args.model, "effort": args.effort,
                "main_router": "isolated native handoff receiver; ordinary Router is not started",
            })

            def cli(*words):
                result = subprocess.run([args.cli, *words],
                                        env={**env, "TEND_URL": f"http://{address}"},
                                        capture_output=True, text=True, timeout=20)
                assert result.returncode == 0, f"{shlex.join(words)}: {result.stderr}"
                return json.loads(result.stdout)

            chat = cli("discussion", "create")["chat_id"]
            print(f"Created fresh synthetic discussion {chat}", flush=True)
            accepted = []
            for index, prompt in enumerate(PROMPTS, 1):
                receipt = cli("chat", "send", chat, prompt)
                accepted.append(receipt)
                row = wait_for(lambda: next((r for r in cli("chat", "read", chat)["messages"]
                                             if r["id"] == receipt["message_id"]), None),
                               lambda r: r and r["router_state"] in ("completed", "failed"))
                assert row["router_state"] == "completed", row
                print(f"Live Discussion prompt {index} completed", flush=True)
            save(args.evidence / "discussion-history.json", cli("chat", "read", chat))
            [(thread,)] = sql(db, "SELECT thread_id FROM router_threads WHERE chat_id=?", chat)
            first = cli("discussion", "handoff-now", chat)
            operation = first["operation_id"]
            receipt = wait_for(lambda: cli("discussion", "handoff-status", chat, operation),
                               lambda data: data["request"] and data["request"]["status"] != "queued")
            save(args.evidence / "receipt.json", {"accepted": accepted, "queued": first, "settled": receipt})
            assert receipt["request"]["status"] == "sent", receipt
            assert cli("discussion", "handoff-now", chat, operation) == receipt
            message_id = receipt["request"]["message_id"]
            _ = wait_for(lambda: cli("chat", "read", "main")["messages"],
                         lambda rows: any(row["reply_to"] == message_id for row in rows))
            [message] = [row for row in cli("chat", "read", "main")["messages"]
                         if row.get("discussion_id") == chat]
            assert message["id"] == message_id and message["author"] == "user"
            [handoff] = cli("discussion", "read", chat)["handoffs"]
            assert handoff["href"] == f"/#message-{message_id}" and handoff["message_id"] == message_id
            [(encoded, turn, used_thread)] = sql(db, """SELECT d.draft,a.turn_id,a.thread_id
                FROM discussion_quick_handoffs q JOIN discussion_drafts d ON d.id=q.draft_id
                JOIN chat_messages m ON m.id=d.message_id JOIN router_inputs i ON i.message_seq=m.seq
                JOIN router_attempts a ON a.seq=i.seq WHERE q.operation_id=?""", operation)
            summary = json.loads(encoded)
            save(args.evidence / "model-summary.json", {
                "model": args.model, "thread_id": used_thread, "turn_id": turn, "output": summary,
            })
            save(args.evidence / "main-delivery.json", {"unique_count": 1, "message": message, "backlink": handoff})
            [(receive_thread, receive_turn)] = sql(db, """SELECT a.thread_id,a.turn_id FROM router_attempts a
                 JOIN router_inputs i ON i.seq=a.seq JOIN chat_messages m ON m.seq=i.message_seq WHERE m.id=?""",
                 message_id)
            assert receive_thread != thread and receive_turn
            assert not (root / "app.sqlite").exists(), "ordinary Router is not part of the generated receiving path"
            assert sql(db, "SELECT thread_id FROM router_threads WHERE chat_id='main'") == []
            receive_rollout, = sessions.rglob(f"*{receive_thread}*.jsonl")
            assert receive_turn in receive_rollout.read_text()
            reply, = [row for row in cli("chat", "read", "main")["messages"] if row["reply_to"] == message_id]
            save(args.evidence / "native-receiver.json", {"thread_id":receive_thread,"turn_id":receive_turn,
                                                         "ordinary_router_started":False,"reply":reply})
            assert used_thread == thread and turn
            assert isinstance(summary, str)
            assert "纸鸢" in summary and "导出" in summary and "CSV" in summary, summary
            assert re.search(r"7\s*天", summary) and re.search(r"30\s*天", summary), summary
            assert "本机" in summary and "真实" in summary, summary
            assert "讨论" in summary and "部署" in summary, summary
            assert "比较" in summary and not any(word in summary for word in ("南船", "天气", "菜单", "按钮")), summary
            assert summary in message["text"] and "不是新的实施授权或命令" in message["text"]
            assert "指定推进的 Issue" not in message["text"] and "\n已明确决策\n" not in message["text"]
            assert sql(db, "SELECT count(*) FROM router_dispatches") == [(0,)]
            native_rollouts = list(sessions.rglob(f"*{thread}*.jsonl"))
            assert len(native_rollouts) == 1 and turn in native_rollouts[0].read_text(), (thread, turn)
            print(f"PASS live model {args.model}, reused thread {thread}, turn {turn}, one delivery {message_id}", flush=True)


if __name__ == "__main__":
    main()
