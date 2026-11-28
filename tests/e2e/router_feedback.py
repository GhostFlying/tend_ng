"""Prove main Chat receives durable feedback before a Runner claim or Router completion."""

import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
from urllib.request import Request, urlopen

from chat import seed_archived_input, server, wait_for

TESTS = Path(__file__).resolve().parents[1]
WEB = TESTS.parent / "web"
FIXTURE = TESTS / "fixtures/router_feedback_app_server.py"
CASE = json.loads(FIXTURE.with_name("router_feedback.json").read_text())


@contextmanager
def browser(args, root, env):
    if args.screenshots is None:
        yield
        return
    process = subprocess.Popen([args.node, WEB / "tests/router-feedback.mjs"], env={
        **env, "TEND_FIXTURE_ROOT": str(root), "TEND_SCREENSHOTS": str(args.screenshots),
    })
    try:
        wait_for(lambda: (root / "browser-ready").exists(), bool)
        yield
        assert process.wait(timeout=15) == 0
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=5)


def exercise(args, root):
    env = {**os.environ, "TEND_TOKEN": "router-feedback-fixture", "CODEX_HOME": str(root / "codex")}
    for name in ("before", "after"):
        (root / f"{name}.json").write_text(json.dumps(CASE[name]))

    def sql(statement, *values):
        with sqlite3.connect(root / "server.sqlite") as db:
            return db.execute(statement, values).fetchall()

    with server(args.server, root, env, router_fixture=FIXTURE,
                web_dir=WEB / "dist" if args.screenshots else None) as address:
        env["TEND_URL"] = f"http://{address}"

        def cli(*words):
            return json.loads(subprocess.check_output([args.cli, *words], env=env, timeout=5))

        def send(text, *options, chat="main"):
            return cli("chat", "send", chat, text, *options)["message_id"]

        def replies(original, chat="main"):
            rows = cli("chat", "read", chat)["messages"]
            user = next(m for m in rows if m["id"] == original)
            return user["router_state"], [m for m in rows if m.get("reply_to") == original]

        def settled(original, chat="main"):
            return wait_for(lambda: replies(original, chat), lambda v: v[0] == "completed")[1]

        def held(name):
            path = root / name
            wait_for(path.exists, bool)
            return path

        def attempts(original):
            return sql("SELECT a.number FROM router_attempts a JOIN router_inputs i ON i.seq=a.seq "
                       "JOIN chat_messages m ON m.seq=i.message_seq WHERE m.id=? ORDER BY a.number",
                       original)

        # Advertise a compatible Runner once, before the dispatch. Never request
        # its assignment or submit a claim. The Server should reserve capacity.
        register = Request(f"http://{address}/api/v1/runners/fixture-runner/sync", method="POST",
                           headers={"Authorization": f"Bearer {env['TEND_TOKEN']}",
                                    "Content-Type": "application/json"},
                           data=json.dumps({
                               "protocol": 2, "records": [], "receipts": [],
                               "profile": {
                                   "runner_id": "fixture-runner", "host_id": "fixture-host",
                                   "repos": ["lynx"], "agents": [{
                                       "id": "fixture-investigation", "category": "investigation",
                                       "revision": "one", "model": "fixture-model",
                                   }],
                                   "slot_limit": 1, "running_slots": 0, "available_for_assignment": 1,
                                   "direct_issue_owners": True, "workspace_root": str(root / "workspaces"),
                               },
                           }).encode())
        with urlopen(register, timeout=5) as response:
            assert response.status == 200

        with browser(args, root, env):
            original = send(CASE["dispatch_request"])
            receipt = json.loads(held("dispatch-held").read_text())
            issue, number, task = (receipt[key] for key in ("issue_id", "short_number", "task_id"))
            assert receipt["category"] == "investigation"
            assert receipt["placement"]["runner_id"] == "fixture-runner"
            assert sql("SELECT state FROM runner_assignments WHERE task_id=?", task) == [("reserved",)]
            state, rows = replies(original)
            assert state == "awaiting_turn", state
            [acceptance] = rows
            assert acceptance["author"] == "router" and acceptance["text"] == CASE["dispatch_acceptance"]
            assert acceptance["issue_ids"] == [issue] and acceptance["issue_numbers"] == [number]
            detail = cli("issue", "read", f"#{number}")
            assert detail["source_message_id"] == original and "run" not in detail

            if args.screenshots:
                held("browser-dispatch-seen")
            assert replies(original) == ("awaiting_turn", [acceptance])
            assert sql("SELECT count(*) FROM runner_assignments WHERE task_id=? AND state='accepted'",
                       task) == [(0,)]
            (root / "release-dispatch").touch()
            assert settled(original) == [acceptance] and attempts(original) == [(1,), (2,)]

            relay = send(CASE["relay_request"], "--reply-to", acceptance["id"])
            [relayed] = settled(relay)
            assert relayed["text"] == CASE["relay_acceptance"] and relayed["issue_ids"] == [issue]
            assert sql("SELECT count(*) FROM issues") == [(1,)]
            assert sql("SELECT count(*) FROM issue_messages WHERE kind='relay'") == [(1,)]

            own = send(CASE["self_request"])
            running = held("tool-running")
            pid = int(running.read_text())
            progress_tool = json.loads(held("progress-receipt").read_text())
            state, rows = replies(own)
            assert state == "awaiting_turn", state
            [progress] = rows
            assert progress["text"] == CASE["progress"] and progress["author"] == "router"
            assert progress["id"] == progress_tool["message_id"] and "issue_ids" not in progress
            if args.screenshots:
                held("browser-progress-seen")
            os.kill(pid, 0)
            assert replies(own) == ("awaiting_turn", [progress])
            (root / "release-progress").touch()
            first, final = settled(own)
            assert first == progress and final["text"] == CASE["answer"]
            assert attempts(own) == [(1,), (2,)]

            failed = send("派发失败场景")
            held("failure-held")
            assert replies(failed) == ("awaiting_turn", [])
            (root / "release-failure").touch()
            assert [row["text"] for row in settled(failed)] == ["派发未成功。"]
            assert sql("SELECT count(*) FROM issues") == [(1,)]

            recorded = send("先记作待办")
            assert [row["text"] for row in settled(recorded)] == ["已保存为待办。"]
            assert sql("SELECT count(*) FROM tasks") == [(1,)]
            ordinary = send("你好")
            assert [row["text"] for row in settled(ordinary)] == ["你好。"]
            elsewhere = seed_archived_input(root, "other", "其他 Chat 的请求")["message_id"]
            assert [row["text"] for row in settled(elsewhere, "other")] == ["这里不发布主 Chat 进展。"]
            assert sql("SELECT count(*) FROM router_chat_feedback") == [(3,)]

            source = send(CASE["multi_request"])
            first, second = settled(source)
            assert all(row["text"] == CASE["dispatch_acceptance"] for row in (first, second))
            [first_number], [second_number] = first["issue_numbers"], second["issue_numbers"]
            [selected] = second["issue_ids"]
            [(previous_thread,)] = sql("SELECT thread_id FROM router_threads WHERE chat_id='main'")
            (root / "lost-native-thread").write_text(previous_thread)

    # Tend retains its ledger. The fixture App Server no longer has the native
    # thread, so an unquoted follow-up must be answered from restored context.
    with server(args.server, root, env, router_fixture=FIXTURE) as address:
        env["TEND_URL"] = f"http://{address}"
        cold = send(CASE["cold_request"])
        [answer] = settled(cold)
        assert answer["text"] == f"已受理 #{first_number} 和 #{second_number}，单凭回执不能确认 Agent 已领取。"
        assert (root / "native-history-missing").exists()
        [(new_thread,)] = sql("SELECT thread_id FROM router_threads WHERE chat_id='main'")
        assert new_thread != previous_thread and attempts(cold) == [(1,)]
        facts = json.loads((root / "cold-acceptances").read_text())
        assert [fact["receipt_message_id"] for fact in facts] == [first["id"], second["id"]]

        precise = send(CASE["second_issue_reply"], "--reply-to", second["id"])
        [precise_ack] = settled(precise)
        chained = send(CASE["relay_receipt_reply"], "--reply-to", precise_ack["id"])
        [chained_ack] = settled(chained)
        for reply in (precise_ack, chained_ack):
            assert reply["text"] == CASE["relay_acceptance"] and reply["issue_ids"] == [selected]
        assert sql("SELECT issue_id FROM issue_messages WHERE kind='relay'"
                   " AND source_message_id IN (?,?) ORDER BY seq", precise, chained) == [(selected,), (selected,)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for binary in ("server", "cli"):
        parser.add_argument(binary, type=lambda value: Path(value).resolve())
    parser.add_argument("--node", default="node")
    parser.add_argument("--screenshots", type=lambda value: Path(value).resolve())
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-router-feedback-") as temp:
        exercise(args, Path(temp))
    print("PASS live acceptance and progress, replay and failure, precise Issue replies, and rebuilt Router context")


if __name__ == "__main__":
    main()
