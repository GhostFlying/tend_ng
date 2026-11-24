"""Exercise persisted control, exact Codex targeting, and queue conversion."""

import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile

from chat import server, wait_for
from issue_chat import TESTS, free_port, prepare


def main():
    server_bin, cli_bin, runner_bin = (Path(value).resolve() for value in sys.argv[1:])
    with tempfile.TemporaryDirectory(prefix="tend-control-") as directory:
        root = Path(directory)
        addr = free_port()
        prepare(root, free_port())
        env = {**os.environ, "TEND_TOKEN": "control-fixture", "TEND_URL": f"http://{addr}"}

        def cli(*words):
            return json.loads(subprocess.check_output([cli_bin, *words], env=env, timeout=6))

        with server(server_bin, root, env, listen=addr):
            runner = subprocess.Popen([
                runner_bin, "--config", root / "runner.json", "--db", root / "runner.sqlite",
                "--app-server", sys.executable, TESTS / "fixtures/control_app_server.py", root,
            ], env=env)
            try:
                cli("chat", "send", "main", "Dispatch from Chat.")
                message = wait_for(lambda: cli("chat", "read", "main")["messages"][0],
                                   lambda message: message.get("issue_ids"))
                [issue] = message["issue_ids"]
                original = wait_for(lambda: cli("issue", "read", issue),
                                    lambda issue: issue["status"] == "running")
                task, run = original["task_id"], original["run"]["run_id"]
                steer = cli("run", "steer", run, "Use the exact active turn.", "live-steer")
                assert steer["state"] == "pending"
                delivered = wait_for(lambda: cli("control", "read", "live-steer"),
                                     lambda control: control["state"] == "delivered")
                assert delivered["run_id"] == run
                assert cli("run", "steer", run, "Use the exact active turn.", "live-steer") == delivered

                stop = cli("task", "stop", task, "stop-original")
                assert stop["state"] == "pending"
                settled = wait_for(lambda: cli("control", "read", "stop-original"),
                                   lambda control: control["state"] == "stopped")
                assert settled["task_id"] == task and (root / "interrupted").exists()
                final = cli("issue", "read", issue)
                assert final["status"] == "cancelled" and final["run"]["phase"]["outcome"] == "cancelled"
                with sqlite3.connect(root / "runner.sqlite") as db:
                    assert db.execute("SELECT state FROM local_runs WHERE id = ?", [run]).fetchone() == ("stopped",)

                missed = cli("run", "steer", run, "Queue the unsent correction.", "missed-steer")
                assert missed["state"] == "undelivered"
                queued = cli("control", "queue", "missed-steer")
                assert queued["state"] == "queued" and cli("control", "queue", "missed-steer") == queued
                wait_for(lambda: (root / "queued-message.json").exists(), bool)
                ready = wait_for(lambda: cli("issue", "read", issue), lambda issue: issue["status"] == "completed")
                assert ready["task_id"] != task and ready["run"]["run_id"] != run
                entries = cli("issue", "messages", issue)["messages"]
                assert sum(entry["text"] == "Queue the unsent correction." for entry in entries) == 1
                cli("issue", "cancel", issue, "Stop work in this tree.", "cancel-issue")
                assert cli("issue", "read", issue)["issue_state"] == "cancelled"
                cli("issue", "reopen", issue, "Resume under the original owner.", "reopen-issue")
                assert cli("issue", "read", issue)["issue_state"] == "open"
                cli("issue", "cancel", issue, "Stop work in this tree.", "cancel-issue")
                assert cli("issue", "read", issue)["issue_state"] == "open"
            finally:
                runner.terminate()
                runner.wait(timeout=7)
    print("PASS live Steer, actual stop, old-Run rejection, idempotent queue and reopen")


if __name__ == "__main__":
    main()
