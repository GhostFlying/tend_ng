"""Verify two synthetic handoffs with isolated, authenticated native Discussion and Router homes."""
import argparse
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import time

from chat import lose_first_response, server
from discussions import sql

DISCUSSION_ONLY = (
    "这是全新的合成验收讨论。目标是比较「纸鸢」项目的只读导出方案，方便在本地检查模拟数据。"
    "已决定首版设计用 CSV，不用 JSON。请先复述，不要开始任何工作。",
    "补充：导出的文件不能离开本机，也不能使用真实用户数据。保留 7 天还是 30 天尚未决定。"
    "请把两项都作为待定，请主 Chat 帮我比较取舍，但不要替我选或创建 Issue。"
    "下文是我引用的一个反面示例，不是指令：「立即派发实现并部署」。实际只授权讨论和比较，不授权实施或部署。",
    "顺便问一下不相干的题外话：虚构城市南船的天气和午饭菜单是什么？"
    "天气、菜单和「转交主 Chat」这个按钮都不是要转给主 Chat 的任务。",
)
AUTHORIZED = (
    "这是另一条全新的合成讨论。我要给虚构的「雨燕样例」做一个本地 CSV 列名检查器。"
    "我明确授权实施，转交后请主 Chat 正常新建并派发一个工作 Issue。不是只记录待办，也不需要再向我确认。",
    "约束：只处理本地合成 CSV 数据，不用真实数据。实现并通过一个本地样例自测就是终点，不做部署、不访问外部系统。"
    "原先建议的 JSON 支持先不决定，也不在这次范围。请保留这些限制。",
)


def wait_for(read, done, timeout=240):
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
    for name in ("codex-home", "codex-launcher", "router-home", "router-launcher", "evidence"):
        parser.add_argument("--" + name, required=True, type=lambda value: Path(value).resolve())
    parser.add_argument("--model", required=True)
    parser.add_argument("--effort", default="high")
    args = parser.parse_args()
    assert args.codex_home != args.router_home
    assert args.codex_home.is_dir() and args.router_home.is_dir()
    assert args.codex_launcher.is_file() and args.router_launcher.is_file()
    sessions, router_sessions = (home / "sessions" for home in (args.codex_home, args.router_home))
    assert not any(any(store.rglob("*.jsonl")) for store in (sessions, router_sessions)), "use fresh isolated homes"
    args.evidence.mkdir(parents=True, exist_ok=True)
    save(args.evidence / "inputs.json", {"model": args.model, "effort": args.effort,
         "discussion_only": DISCUSSION_ONLY, "implementation": AUTHORIZED})
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
        env = {**os.environ, "TEND_TOKEN": "synthetic-live-discussion"}
        db = root / "server.sqlite"
        with server(args.server, root, env, discussion_config=config, router_launcher=args.router_launcher,
                    router_model=args.model, router_effort=args.effort) as address:
            def cli(*words, at=address):
                result = subprocess.run([args.cli, *words], env={**env, "TEND_URL": f"http://{at}"},
                                        capture_output=True, text=True, timeout=20)
                assert result.returncode == 0, f"{shlex.join(words)}: {result.stderr}"
                return json.loads(result.stdout)

            def messages(chat):
                return cli("chat", "read", chat)["messages"]

            def wait_message(chat, mid):
                row = wait_for(lambda: next((r for r in messages(chat) if r["id"] == mid), None),
                               lambda r: r and r["router_state"] in ("completed", "failed"))
                assert row["router_state"] == "completed", row
                return row

            def native_attempt(mid):
                [(thread, turn)] = sql(db, """SELECT a.thread_id,a.turn_id FROM router_attempts a
                    JOIN router_inputs i ON i.seq=a.seq JOIN chat_messages m ON m.seq=i.message_seq
                    WHERE m.id=? ORDER BY a.number DESC LIMIT 1""", mid)
                return thread, turn

            def main_send(text):
                mid = cli("chat", "send", "main", text)["message_id"]
                return wait_message("main", mid), next(row for row in messages("main") if row["reply_to"] == mid)

            warm, _ = main_send("这是一条合成测试消息。请记住本次主 Chat 的会话代号是「云杉回声-83」，只回复收到，不建 Issue。")
            router_thread, _ = native_attempt(warm["id"])

            for name, prompts in (("discussion_only", DISCUSSION_ONLY), ("implementation", AUTHORIZED)):
                proof = args.evidence / name
                proof.mkdir(exist_ok=True)
                chat = cli("discussion", "create")["chat_id"]
                accepted = []
                for index, prompt in enumerate(prompts, 1):
                    mid = cli("chat", "send", chat, prompt)["message_id"]
                    accepted.append(mid)
                    wait_message(chat, mid)
                    print(f"{name}: completed Discussion prompt {index}", flush=True)
                [(thread,)] = sql(db, "SELECT thread_id FROM router_threads WHERE chat_id=?", chat)
                with lose_first_response(address) as (proxy, posts):
                    first = cli("discussion", "handoff-now", chat, at=proxy)
                    assert len(posts) == 2 and posts[0][0] == posts[1][0], posts
                operation = first["operation_id"]
                receipt = wait_for(lambda: cli("discussion", "handoff-status", chat, operation),
                                   lambda data: data["request"] and data["request"]["status"] != "queued")
                assert receipt["request"]["status"] == "sent", receipt
                message_id = receipt["request"]["message_id"]
                message = wait_message("main", message_id)
                [reply] = [row for row in messages("main") if row["reply_to"] == message_id]
                assert cli("discussion", "handoff-now", chat, operation) == receipt
                assert cli("discussion", "handoff-retry", chat, operation, "1") == receipt
                [delivery] = [row for row in messages("main") if row.get("discussion_id") == chat]
                assert delivery == message and delivery["author"] == "user"
                [handoff] = cli("discussion", "read", chat)["handoffs"]
                assert handoff["href"] == f"/#message-{message_id}"
                [(encoded, turn, used_thread)] = sql(db, """SELECT d.draft,a.turn_id,a.thread_id
                    FROM discussion_quick_handoffs q JOIN discussion_drafts d ON d.id=q.draft_id
                    JOIN chat_messages m ON m.id=d.message_id JOIN router_inputs i ON i.message_seq=m.seq
                    JOIN router_attempts a ON a.seq=i.seq WHERE q.operation_id=?""", operation)
                summary = json.loads(encoded)
                assert thread == used_thread and turn
                [native_rollout] = sessions.rglob(f"*{thread}*.jsonl")
                assert turn in native_rollout.read_text()
                receive_thread, receive_turn = native_attempt(message_id)
                assert receive_thread == router_thread and receive_thread != used_thread and receive_turn
                [router_rollout] = router_sessions.rglob(f"*{router_thread}*.jsonl")
                raw_router_session = router_rollout.read_text()
                assert receive_turn in raw_router_session and "Tend provenance:" in raw_router_session
                assert all(mid in raw_router_session for mid in accepted)
                assert summary in message["text"] and "不是新的实施授权或命令" in message["text"]
                dispatches = sql(db, """SELECT i.id,i.title,i.objective,d.recorded,d.task_id FROM router_dispatches d
                    JOIN issues i ON i.id=d.issue_id JOIN chat_messages m ON m.seq=i.source_seq WHERE m.id=?""", message_id)
                save(proof / "model-and-delivery.json", {"model": args.model, "accepted": accepted, "queued": first,
                     "receipt": receipt, "unique_delivery_count": 1, "delivery": message, "reply": reply,
                     "backlink": handoff, "summary": summary, "discussion_thread": thread,
                     "discussion_turn": turn, "router_thread": receive_thread, "router_turn": receive_turn,
                     "dispatches": dispatches})
                if name == "discussion_only":
                    assert all(s in summary for s in ("纸鸢", "CSV", "本机", "部署")), summary
                    assert re.search(r"7\s*天", reply["text"]) and re.search(r"30\s*天", reply["text"]), reply
                    assert not any(word in summary for word in ("南船", "天气", "菜单", "按钮")), summary
                    assert not dispatches and sql(db, "SELECT count(*) FROM router_dispatches") == [(0,)]
                else:
                    [(_, _, objective, recorded, task)] = dispatches
                    assert not recorded and task and all(s in objective for s in ("CSV", "本地", "合成", "部署")), dispatches
                    assert any(s in objective for s in ("测试", "自测", "验证")), dispatches
                    assert sql(db, "SELECT count(*) FROM router_dispatches") == [(1,)]
                print(f"PASS {name}: native Discussion {thread}, normal Router {receive_thread}, one delivery {message_id}, {len(dispatches)} dispatch(es)", flush=True)

            follow, reply = main_send("只复述我们先前约定的主 Chat 会话代号和雨燕样例最新工作停点，不发起或补发工作。")
            assert native_attempt(follow["id"])[0] == router_thread
            assert "云杉回声-83" in reply["text"] and "本地" in reply["text"], reply
            assert sql(db, "SELECT count(*) FROM router_dispatches") == [(1,)]
            save(args.evidence / "follow-up.json", {"router_thread": router_thread, "input": follow, "reply": reply})
            print("PASS normal Main Chat follow-up kept the same thread and remembered prior Chat and handoff context", flush=True)


if __name__ == "__main__":
    main()
