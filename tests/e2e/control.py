"""Exercise persisted control, exact Codex targeting, and queue conversion."""

import json
import os
from pathlib import Path
import signal
import sqlite3
import subprocess
import sys
import tempfile

from chat import seed_archived_input, server, wait_for
from issue_chat import TESTS, free_port, prepare


def main():
    server_bin, cli_bin, runner_bin = (Path(value).resolve() for value in sys.argv[1:4])
    self_stop = "--agent-stop" in sys.argv[4:]
    backlog = "--backlog" in sys.argv[4:]
    with tempfile.TemporaryDirectory(prefix="tend-control-") as directory:
        root = Path(directory)
        addr = free_port()
        prepare(root, free_port())
        if self_stop:
            (root / "agent-stop").touch()
        env = {**os.environ, "CODEX_HOME": str(root / "fixture-codex"), "TEND_TOKEN": "control-fixture", "TEND_URL": f"http://{addr}"}

        def cli(*words):
            return json.loads(subprocess.check_output([cli_bin, *words], env=env, timeout=6))

        with server(server_bin, root, env, listen=addr, github_cli="/bin/false" if backlog else None):
            runner = subprocess.Popen([
                runner_bin, "--config", root / "runner.json", "--db", root / "runner.sqlite",
                "--app-server", sys.executable, TESTS / "fixtures/control_app_server.py", root,
            ], env=env)
            try:
                cli("chat", "send", "main", "Dispatch from Chat.")
                message = wait_for(lambda: cli("chat", "read", "main")["messages"][0],
                                   lambda message: message.get("issue_ids"))
                [issue] = message["issue_ids"]
                if self_stop:
                    wait_for(lambda: (root / "self-stop.json").exists(), bool)
                    requested = json.loads((root / "self-stop.json").read_text())
                    def read_control():
                        try:
                            return cli("control", "read", requested["operation_id"])
                        except subprocess.CalledProcessError:
                            return {}
                    receipt = wait_for(read_control,
                                       lambda result: result.get("state") == "stopped")
                    assert receipt["task_id"] == requested["action"]["task_id"]
                    assert cli("issue", "read", issue)["status"] == "cancelled"
                    with sqlite3.connect(root / "runner.sqlite") as db:
                        assert db.execute("SELECT count(DISTINCT id) FROM run_outbox WHERE id = ? OR "
                                          "json_extract(observation, '$.id') = ?", [requested["operation_id"]]*2).fetchone() == (2,)
                    print("PASS native stop request and its Runner receipt use separate persistent identities")
                    return
                original = wait_for(lambda: cli("issue", "read", issue),
                                    lambda issue: issue["status"] == "running")
                task, run = original["task_id"], original["run"]["run_id"]
                steer = cli("run", "steer", run, "Use the exact active turn.", "live-steer")
                assert steer["state"] == "pending"
                delivered = wait_for(lambda: cli("control", "read", "live-steer"),
                                     lambda control: control["state"] == "delivered")
                assert delivered["run_id"] == run
                assert cli("run", "steer", run, "Use the exact active turn.", "live-steer") == delivered

                if backlog:
                    number = original["short_number"]
                    pr = "https://github.com/example/guide/pull/123"
                    cli("pr", "link", issue, pr, "link-before-backlog")
                    runner.send_signal(signal.SIGSTOP)
                    try:
                        seed_archived_input(root, "other", f"Reject parking foreign fixture Issue #{number}")
                        wait_for(lambda: cli("chat", "read", "other")["messages"],
                                 lambda messages: messages[-1]["text"] == "Cannot park a different Chat's Issue.")
                        cli("chat", "send", "main", f"Park fixture Issue #{number}")
                        wait_for(lambda: cli("chat", "read", "main")["messages"],
                                 lambda messages: messages[-1]["text"] == "Backlog request accepted.")
                        parked = cli("issue", "backlog", f"#{number}", "park-live")
                        assert parked["issue_id"] == issue and parked["state"] == "backlog"
                        assert cli("issue", "backlog", issue, "park-live") == parked
                        pending = cli("issue", "list", "--scope=roots")["issues"][0]
                        assert pending["stage"] == "in_progress" and pending["running"] == 1
                        start = [cli_bin, "issue", "start", issue, "restart-live"]
                        not_ready = subprocess.run(start, env=env, capture_output=True, text=True, timeout=6)
                        assert not_ready.returncode and "HTTP 409" in not_ready.stderr, not_ready.stderr
                        assert not (root / "interrupted").exists()
                        cli("issue", "message", issue, "Queue the unsent correction.", "parked-message")
                    finally:
                        runner.send_signal(signal.SIGCONT)
                    card = wait_for(lambda: cli("issue", "list", "--scope=roots")["issues"][0],
                                    lambda view: view["stage"] == "backlog")
                    assert card["issue_state"] == "open" and card["running"] == 0
                    assert card["short_number"] == number and [link["url"] for link in card["pulls"]] == [pr]
                    assert (root / "interrupted").exists() and not (root / "queued-message.json").exists()
                    start_receipt = cli("issue", "start", f"#{number}", "restart-live")
                    assert start_receipt["issue_id"] == issue and start_receipt["task_id"] != task
                    wait_for(lambda: (root / "queued-message.json").exists(), bool)
                    resumed = wait_for(lambda: cli("issue", "read", issue), lambda view: view["status"] == "completed")
                    assert resumed["task_id"] == start_receipt["task_id"] and resumed["run"]["run_id"] != run
                    assert cli("issue", "backlog", issue, "park-live") == parked
                    assert cli("issue", "start", issue, "restart-live") == start_receipt
                    assert cli("issue", "read", issue)["task_id"] == resumed["task_id"]
                    with sqlite3.connect(root / "server.sqlite") as db:
                        assert db.execute("SELECT id FROM task_runs WHERE task_id = ?", [task]).fetchall() == [(run,)]
                        assert db.execute("SELECT count(*) FROM issue_parks").fetchone() == (0,)
                    print("PASS scoped Router and CLI backlog, offline active stop, original Issue/PR/Run, and restart")
                    return

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
