"""Verify external conditions, fixed-window deduplication, and original Task resume."""

import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile

from chat import server, wait_for
from issue_chat import TESTS, free_port, prepare

PULL = "https://github.com/example/guide/pull/82"


def main():
    server_bin, cli_bin, runner_bin = (Path(value).resolve() for value in sys.argv[1:4])
    cancelling = "--cancel" in sys.argv[4:]
    with tempfile.TemporaryDirectory(prefix="tend-wait-") as directory:
        root = Path(directory)
        addr = free_port()
        prepare(root, free_port(), "A fixed repository.\n")
        config = json.loads((root / "runner.json").read_text())
        config["agents"][0]["reasoning_effort"] = "low"
        (root / "runner.json").write_text(json.dumps(config))
        mode = root / "wait-mode"
        mode.write_text("pending")
        (root / "delegate-now").touch()
        env = {**os.environ, "TEND_TOKEN": "wait-fixture", "TEND_URL": f"http://{addr}"}
        server_env = {**env, "GH_TOKEN": "fixture-read-token", "TEND_GITHUB_FIXTURE_DIR": str(root)}

        def cli(*words):
            return json.loads(subprocess.check_output([cli_bin, *words], env=env, timeout=5))

        def sql(db_name, statement, *values):
            with sqlite3.connect(root / f"{db_name}.sqlite") as db:
                return db.execute(statement, values).fetchall()

        def reschedule(observe):
            log = root / "github-reads"
            before = len(log.read_text().splitlines()) if log.exists() else 0
            mode.write_text(observe)
            sql("server", "UPDATE github_pulls SET due_at = 0 WHERE url = ?", PULL)
            wait_for(lambda: len(log.read_text().splitlines()), lambda count: count > before)
            wait_for(lambda: sql("server", "SELECT due_at > unixepoch() FROM github_pulls WHERE url = ?", PULL),
                     lambda rows: rows == [(1,)])

        with server(server_bin, root, server_env, listen=addr, github_cli=TESTS / "fixtures/github_cli.py"):
            runner = subprocess.Popen([
                runner_bin, "--config", root / "runner.json", "--db", root / "runner.sqlite",
                "--app-server", sys.executable, TESTS / "fixtures/coordination_app_server.py",
                root, "external-wait",
            ], env=env)
            try:
                cli("chat", "send", "main", "Dispatch from Chat.")
                wait_for(lambda: (root / "wait-registration.json").exists(), bool)
                work = json.loads((root / "delegation.json").read_text())["action"]
                issue, task = work["issue_id"], work["task_id"]
                waiting = wait_for(lambda: cli("issue", "read", issue), lambda result: result["status"] == "waiting")
                assert waiting["run"]["phase"]["waiting_for_external"]
                assert sql("runner", "SELECT execution_state, retains_workspace FROM received_assignments WHERE task_id = ?", task) == [
                    ("waiting_external", 1)]
                wait_for(lambda: (root / "isolated-ran").exists(), bool)
                assert not (root / "shared-ran").exists()
                shared = json.loads((root / "siblings.json").read_text())["shared"]["action"]["task_id"]
                assert sql("runner", "SELECT execution_state FROM received_assignments WHERE task_id = ?", shared) == [
                    ("queued",)]
                wait_for(lambda: cli("pr", "watches", issue), lambda rows: len(rows) == 3)
                [(due,)] = wait_for(lambda: sql("server", "SELECT due_at FROM external_batches WHERE task_id = ?", task), bool)
                assert sql("server", "SELECT count(*) FROM external_deliveries") == [(1,)]

                reschedule("stale")
                assert {watch["state"] for watch in cli("pr", "watches", issue)} == {"active"}
                assert sql("server", "SELECT count(*) FROM external_deliveries") == [(1,)]
                reschedule("matching")
                assert sql("server", "SELECT due_at FROM external_batches WHERE task_id = ?", task) == [(due,)]
                assert sql("server", "SELECT count(*) FROM external_deliveries") == [(3,)]
                reschedule("matching")
                assert sql("server", "SELECT count(*) FROM external_deliveries") == [(3,)]
                assert not (root / "resumed-wait.json").exists()
                if cancelling:
                    (root / "cancelled-wait").touch()
                    cli("task", "stop", task, "stop-wait")
                    wait_for(lambda: cli("control", "read", "stop-wait"),
                             lambda command: command["state"] == "stopped")
                    wait_for(lambda: cli("issue", "read", issue), lambda result: result["status"] == "cancelled")
                    sql("server", "UPDATE external_batches SET due_at = 0 WHERE task_id = ?", task)
                    wait_for(lambda: sql("server", "SELECT COUNT(*) FROM external_batches"),
                             lambda rows: rows == [(0,)])
                    wait_for(lambda: (root / "shared-ran").exists(), bool)
                    assert not (root / "resumed-wait.json").exists()
                    assert sql("runner", "SELECT retains_workspace FROM received_assignments WHERE task_id = ?", task) == [(0,)]
                    print("PASS cancelled wait releases the workspace and preserves the stopped Run")
                    return
                sql("server", "UPDATE external_batches SET due_at = 0 WHERE task_id = ?", task)
                wait_for(lambda: (root / "resumed-wait.json").exists(), bool)
                ended = wait_for(lambda: cli("issue", "read", issue), lambda result: result["status"] == "completed")
                assert ended["task_id"] == task and ended["run"]["run_id"] != waiting["run"]["run_id"]
                assert sql("runner", "SELECT retains_workspace FROM received_assignments WHERE task_id = ?", task) == [(0,)]
                assert sql("server", "SELECT count(DISTINCT run_id) FROM issue_inputs WHERE task_id = ?", task) == [(1,)]
                assert sql("server", "SELECT count(*) FROM issue_inputs WHERE task_id = ?", task) == [(3,)]
                wait_for(lambda: (root / "shared-ran").exists(), bool)
            finally:
                runner.terminate()
                runner.wait(timeout=7)
    print("PASS GitHub waits, old-head rejection, durable coalescing, and original Task resume")


if __name__ == "__main__":
    main()
