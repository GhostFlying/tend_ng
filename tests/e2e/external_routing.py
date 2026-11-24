"""Exercise the original issue and main Chat receipt paths for GitHub facts."""

import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile

from chat import server, wait_for
from issue_chat import TESTS, free_port, prepare

PULL = "https://github.com/example/guide/pull/"


def main():
    server_bin, cli_bin, _ = (Path(value).resolve() for value in sys.argv[1:])
    with tempfile.TemporaryDirectory(prefix="tend-external-routing-") as directory:
        root = Path(directory)
        addr = free_port()
        prepare(root, free_port())
        (root / "wait-mode").write_text("pending")
        env = {**os.environ, "TEND_TOKEN": "external-routing", "TEND_URL": f"http://{addr}"}
        server_env = {**env, "GH_TOKEN": "fixture-read-token", "TEND_GITHUB_FIXTURE_DIR": str(root)}

        def cli(*words):
            return json.loads(subprocess.check_output([cli_bin, *words], env=env, timeout=6))

        def sql(statement, *values, database="server"):
            with sqlite3.connect(root / f"{database}.sqlite") as db:
                return db.execute(statement, values).fetchall()

        def require(message, number, event, state, op, **fields):
            return cli("pr", "require", message, json.dumps({
                "url": PULL + str(number), "event": event, "state": state, **fields
            }), op)

        with server(server_bin, root, server_env, listen=addr, github_cli=TESTS / "fixtures/github_cli.py"):
            original = cli("chat", "send", "main", "Dispatch from Chat.")["message_id"]
            [issue] = wait_for(lambda: cli("issue", "list", "main")["issues"], bool)
            cli("pr", "link", issue["id"], PULL + "73")
            wait_for(lambda: cli("pr", "list", issue["id"])[0].get("facts"), bool)
            assert sql("SELECT COUNT(*) FROM external_batches") == [(0,)]
            assert sql("SELECT COUNT(*) FROM external_route_batches") == [(0,)]
            for event, state, operation in [("pull", "open", "owned-open"), ("ci", "success", "owned-ci")]:
                assert require(original, 73, event, state, operation) == {"watch_id": operation}
            assert require(original, 73, "pull", "open", "owned-open") == {"watch_id": "owned-open"}
            [[task, due]] = sql("SELECT task_id, due_at FROM external_batches")
            assert task == cli("issue", "read", issue["id"])["task_id"] and due > 0
            assert sql("SELECT COUNT(*) FROM external_deliveries") == [(2,)]
            sql("UPDATE external_batches SET due_at = 0")
            rows = wait_for(lambda: sql("SELECT task_id,payload FROM issue_inputs"), lambda rows: len(rows) == 2)
            for target, raw in rows:
                receipt = json.loads(raw)
                assert target == task and receipt["child_issue_id"] == issue["id"]
                assert receipt["mode"] == "required"
            assert sql("SELECT COUNT(*) FROM external_route_batches") == [(0,)]

            topic = cli("chat", "send", "main", "Subscribe to demo CI.")["message_id"]
            wait_for(lambda: sql("SELECT id FROM external_routes WHERE mode = 'optional'"), bool)
            require(topic, 82, "pull", "open", "unassigned-open")
            wait_for(lambda: sql("SELECT COUNT(*) FROM external_route_deliveries"), lambda rows: rows == [(2,)])
            [[batch, due]] = sql("SELECT id,due_at FROM external_route_batches WHERE released_at IS NULL")
            assert due > 0 and sql("SELECT COUNT(*) FROM router_inputs WHERE external_batch IS NOT NULL") == [(0,)]
            sql("UPDATE github_pulls SET due_at = 0 WHERE number = 82")
            wait_for(lambda: sql("SELECT due_at > unixepoch() FROM github_pulls WHERE number = 82"),
                     lambda rows: rows == [(1,)])
            assert sql("SELECT COUNT(*) FROM external_route_deliveries") == [(2,)]
            assert sql("SELECT id,due_at FROM external_route_batches") == [(batch, due)]

            sql("UPDATE external_route_batches SET due_at = 0 WHERE id = ?", batch)
            wait_for(lambda: sql("SELECT outcome FROM router_inputs WHERE external_batch = ?", batch),
                     lambda rows: rows == [("completed",)])
            [[text]] = sql("SELECT input FROM start_requests WHERE input LIKE 'Tend external facts collected%'", database="app")
            entries = json.loads(text.split("\n", 1)[1])
            assert {entry["mode"] for entry in entries} == {"required", "optional"}
            assert {entry["original_message_id"] for entry in entries} == {topic}
            messages = cli("chat", "read", "main")["messages"]
            assert sum(message["id"] == topic for message in messages) == 1
            assert all(message["text"] != "TEND_NO_UPDATE" for message in messages)

            require(topic, 82, "ci", "success", "follow-head", check="build-linux")
            require(topic, 82, "ci", "success", "pinned-head", head_sha="b" * 40,
                    check="build-linux", run_id="13")
            (root / "wait-mode").write_text("advanced")
            sql("UPDATE github_pulls SET due_at = 0 WHERE number = 82")
            wait_for(lambda: sql("SELECT json_extract(facts, '$.head_sha') FROM github_pulls WHERE number = 82"),
                     lambda rows: rows == [("c" * 40,)])
            routed = sql("SELECT d.route_id, f.fact FROM external_route_deliveries d "
                         "JOIN external_facts f ON f.id = d.fact_id WHERE d.batch_id != ?", batch)
            facts = {name: json.loads(value) for name, value in routed}
            [optional] = [entry["watch_id"] for entry in entries if entry["mode"] == "optional"]
            assert facts.keys() == {"pinned-head", optional, "unassigned-open"}, facts
            assert facts["pinned-head"]["head_sha"] == "b" * 40
            assert facts["pinned-head"]["run_id"] == "13"

    print("PASS observed PR state, required Issue and Router routes, native subscriptions, and one batch per recipient")


if __name__ == "__main__":
    main()
