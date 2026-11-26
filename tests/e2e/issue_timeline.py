"""Verify dispatch provenance and confirmed takeover through real Server and Runner sync."""
import argparse
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import urllib.request

from chat import lose_first_response, server, wait_for
from issue_chat import REQUEST, WEB, free_port, prepare

OLD_REQUEST = "先记下稍后整理发布说明。"
OTHER_REQUEST = "顺带准备另一个仓库的发布记录。"
OBJECTIVE = "整理示例仓库的快速上手指南。"
CHILD_OBJECTIVE = "整理安装和首次运行步骤。"
ROUTER_NAME = "Tend"
RUNNER = "demo-runner"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("server", "cli", "runner"):
        parser.add_argument(executable, type=lambda value: Path(value).resolve())
    parser.add_argument("--web", action="store_true")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-issue-timeline-") as temp:
        root = Path(temp)
        listen, inspect = free_port(), free_port()
        prepare(root, inspect)
        env = {**os.environ, "TEND_TOKEN": "issue-timeline-fixture", "TEND_URL": f"http://{listen}",
               "CODEX_HOME": str(root / "fixture-codex")}
        (root / "fixture-codex").mkdir()
        web = WEB / "dist" if args.web else None

        def cli(*words, error=None):
            result = subprocess.run([args.cli, *words], env=env, text=True,
                                    capture_output=True, timeout=10)
            if error:
                assert result.returncode and error in result.stderr, result.stderr
                return None
            assert result.returncode == 0, result.stderr
            return json.loads(result.stdout)

        def sql(statement, *params):
            with sqlite3.connect((root / "server.sqlite").as_uri() + "?mode=ro", uri=True) as db:
                return db.execute(statement, params).fetchall()

        def dispatch(text, chat="main"):
            original = cli("chat", "send", chat, text)["message_id"]

            def source():
                return next(row for row in cli("chat", "read", chat)["messages"]
                            if row["id"] == original)

            sent = wait_for(source, lambda row: row.get("issue_numbers") and row["router_state"] == "completed")
            [number] = sent["issue_numbers"]
            return str(number), original

        def timeline(issue):
            return cli("issue", "messages", issue)["messages"]

        def browser(phase, **values):
            if args.web:
                subprocess.run(["node", WEB / "tests/issue-timeline.mjs"], check=True, timeout=30,
                               env={**env, "TEND_TIMELINE_PHASE": phase,
                                    **{f"TEND_TIMELINE_{key}": value for key, value in values.items()}})

        # The legacy row remains unnamed even when the next Server has a configured name.
        with server(args.server, root, env, listen=listen, web_dir=web):
            legacy, legacy_source = dispatch(OLD_REQUEST)
            [original] = timeline(legacy)
            assert original["dispatch_provenance"] == {"origin": "main_chat"}
            assert original["source_message_id"] == legacy_source

        with server(args.server, root, env, listen=listen, web_dir=web, router_name=ROUTER_NAME):
            config = json.loads((root / "runner.json").read_text())
            heartbeat = {"protocol": 2, "receipts": [], "profile": {
                "runner_id": RUNNER, "host_id": config["host_id"],
                "repos": ["example"], "agents": config["agents"], "slot_limit": 1,
                "running_slots": 0, "available_for_assignment": 1, "shared_reviews": True,
            }}

            def sync(payload, path=f"/api/v1/runners/{RUNNER}/sync"):
                request = urllib.request.Request(env["TEND_URL"] + path, data=json.dumps(payload).encode(),
                                                 headers={"Content-Type": "application/json",
                                                          "Authorization": "Bearer " + env["TEND_TOKEN"]})
                with urllib.request.urlopen(request, timeout=5) as response:
                    return json.load(response)

            assert sync(heartbeat)["assignments"] == []
            other, other_source = dispatch(OTHER_REQUEST, "other")
            other_page = cli("issue", "messages", other)
            [other_dispatch] = other_page["messages"]
            assert other_page["chat_id"] == "other"
            assert other_dispatch["source_message_id"] == other_source
            assert other_dispatch["dispatch_provenance"] == {"origin": "other_chat"}
            assert sql("SELECT dispatcher_name FROM issue_messages WHERE id=?", other_dispatch["id"]) == [(None,)]

            parent, source = dispatch(REQUEST)
            assert timeline(legacy) == [original]
            [dispatch_event] = timeline(parent)
            task = cli("issue", "read", parent)
            assert dispatch_event["dispatch_provenance"] == {"origin": "main_chat", "agent_name": ROUTER_NAME}
            assert dispatch_event["text"] == task["objective"] == OBJECTIVE
            assert dispatch_event["source_message_id"] == source
            [(assignment, state)] = sql("SELECT id,state FROM runner_assignments WHERE task_id=?", task["task_id"])
            assert state == "reserved"
            [contract] = sync(heartbeat)["assignments"]
            assert contract["placement"]["assignment_id"] == assignment
            assert sql("SELECT state FROM runner_assignments WHERE id=?", assignment) == [("issued",)]
            assert timeline(parent) == [dispatch_event]
            browser("issued", PARENT=parent, SOURCE=source, LEGACY=legacy, LEGACY_SOURCE=legacy_source,
                    OTHER=other, OTHER_SOURCE=other_source)

            # Lose the response after the Server committed the real Runner's acceptance.
            def drop_acceptance(reply):
                return assignment in reply.get("confirmed_receipts", [])

            with (lose_first_response(listen, drop_when=drop_acceptance) as (proxy, posted),
                  (root / "runner.stderr").open("w") as stderr):
                command = [args.runner, "--config", root / "runner.json", "--db", root / "runner.sqlite"]
                runner = subprocess.Popen(
                    [*command, "--app-server", sys.executable, WEB.parent / "tests/fixtures/coordination_app_server.py",
                     root, "issue-chat"], env={**env, "TEND_URL": f"http://{proxy}"}, stderr=stderr,
                )
                try:
                    rows = wait_for(lambda: timeline(parent),
                                    lambda rows: sum(row["kind"] == "takeover" for row in rows) == 2)
                    [parent_claim, child_claim] = [row for row in rows if row["kind"] == "takeover"]
                    assert parent_claim["author"] == contract["placement"]["agent"]["id"] == "coordinator"
                    assert parent_claim["issue_id"] == task["issue_id"]
                    assert child_claim["author"] == "worker"
                    assert rows[0] == dispatch_event and rows[1] == parent_claim
                    child = str(child_claim["short_number"])
                    child_rows = timeline(child)
                    child_dispatch = child_rows[0]
                    assert child_dispatch["dispatch_provenance"] == {
                        "origin": "issue_delegation", "agent_name": parent_claim["author"],
                    }
                    assert child_dispatch["kind"] == "dispatch" and child_dispatch["text"] == CHILD_OBJECTIVE
                    assert child_dispatch["source_message_id"] is None and child_rows[1] == child_claim
                    assert await_receipt_replay(posted, assignment)
                    cli("issue", "message", parent, "Cannot reply to an event", "--reply-to", parent_claim["id"],
                        error="HTTP 404")
                    browser("accepted", PARENT=parent, SOURCE=source, CHILD=child)
                except BaseException:
                    print((root / "runner.stderr").read_text(), end="", file=sys.stderr)
                    raise
                finally:
                    (root / "finish-first").touch()
                    runner.terminate()
                    runner.wait(timeout=8)

            with lose_first_response(listen, drop_when=lambda _: False) as (proxy, posted):
                subprocess.run([*command, "--reconcile", task["task_id"]], check=True, timeout=12,
                               env={**env, "TEND_URL": f"http://{proxy}"})
                assert await_receipt_replay(posted, assignment, count=1)
            assert [row for row in timeline(parent) if row["kind"] == "takeover"] == [parent_claim, child_claim]
            assert sql("SELECT id FROM issue_messages WHERE assignment_id=?", assignment) == [(parent_claim["id"],)]
    print("PASS dispatch provenance, reservation and issuance, accepted Runner takeover, retry and full reconcile")


def await_receipt_replay(posted, assignment, count=2):
    return wait_for(
        lambda: [reply for request, status, reply in posted
                 if status == 200 and assignment in reply.get("confirmed_receipts", [])
                 and {"assignment_id": assignment, "decision": {"status": "accepted"}} in request["receipts"]],
        lambda receipts: len(receipts) >= count,
    )


if __name__ == "__main__":
    main()
