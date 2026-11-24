"""Delegate offline, run a work child, and resume the same parent Task on one slot."""
import argparse
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import sys
import tempfile

from chat import server, wait_for

FIXTURE = Path(__file__).resolve().parents[1] / "fixtures" / "coordination_app_server.py"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("server", "cli", "runner"):
        parser.add_argument(executable, type=lambda value: Path(value).resolve())
    parser.add_argument("--scenario", choices=("explicit-close", "task-only", "no-candidate"),
                        default="explicit-close")
    args = parser.parse_args()
    env = {**os.environ, "TEND_TOKEN": "tend-coordination-fixture"}

    with tempfile.TemporaryDirectory(prefix="tend-coordination-") as temporary:
        root = Path(temporary)
        source = root / "source"
        subprocess.run(["git", "init", "--quiet", "-b", "main", source], check=True)
        (source / "README.md").write_text("A fixed repository.\n")
        subprocess.run(["git", "-C", source, "add", "README.md"], check=True)
        subprocess.run(
            ["git", "-C", source, "-c", "user.name=Fixture",
             "-c", "user.email=fixture@example.invalid", "commit", "--quiet", "-m", "fixture"],
            check=True,
        )
        subprocess.run(["git", "clone", "--quiet", source, root / "example"], check=True)
        (root / "runner.json").write_text(json.dumps({
            "host_id": "coordination-host", "runner_id": "coordination-runner", "slot_limit": 1,
            "repos": [{"id": "example", "path": "example", "default_branch": "main"}],
            "agents": [
                {"id": "coordinator", "category": "issue-router", "revision": "coordination-1",
                 "model": "fixture-coordinator", "reasoning_effort": "low"},
                {"id": "worker", "category": "implementation", "revision": "worker-1",
                 "model": "fixture-worker", "capabilities": ["image_gen"]},
            ],
        }))
        with socket.socket() as reserve:
            reserve.bind(("127.0.0.1", 0))
            address = f"127.0.0.1:{reserve.getsockname()[1]}"
        connected = {**env, "TEND_URL": f"http://{address}"}

        def cli(*words):
            return json.loads(subprocess.check_output([args.cli, *words], env=connected, timeout=8))

        def sql(database, statement, *values):
            with sqlite3.connect(root / f"{database}.sqlite") as db:
                return db.execute(statement, values).fetchall()

        with (root / "runner.stderr").open("w") as stderr:
            runner = subprocess.Popen(
                [args.runner, "--config", root / "runner.json", "--db", root / "runner.sqlite",
                 "--app-server", sys.executable, FIXTURE, root, args.scenario],
                env=connected, stderr=stderr,
            )
            try:
                with server(args.server, root, env, listen=address):
                    cli("chat", "send", "main", "Dispatch from Chat.")
                    messages = wait_for(lambda: cli("chat", "read", "main")["messages"],
                                        lambda rows: len(rows) == 2)
                    [parent] = messages[0]["issue_ids"]
                    first = wait_for(lambda: cli("issue", "read", parent),
                                     lambda view: view["status"] == "running")
                (root / "delegate-now").touch()
                wait_for(lambda: sql("runner", "SELECT execution_state FROM received_assignments"),
                         lambda rows: rows == [("waiting",)])
                [(run_id, state)] = sql("runner", "SELECT id, state FROM local_runs")
                assert (run_id, state) == (first["run"]["run_id"], "stopped")
                assert sql("runner", "SELECT count(*) FROM run_outbox WHERE record_kind = 'operation' "
                                    "AND acknowledged = 0") == [(1,)]
                delegated = json.loads((root / "delegation.json").read_text())
                child = delegated["action"]["issue_id"]
                with server(args.server, root, env, listen=address):
                    final = wait_for(lambda: cli("issue", "read", parent),
                                     lambda view: view["status"] == "completed")
                    assert final["task_id"] == first["task_id"]
                    assert final["issue_state"] == "closed"
                    assert final["run"]["run_id"] != first["run"]["run_id"]
                    work = cli("issue", "read", child)
                    assert work["parent_issue_id"] == parent
                    assert work["task_id"] == delegated["action"]["task_id"]
                    observed = [(first["run"]["run_id"], "stopped")]
                    if args.scenario == "no-candidate":
                        assert work["status"] == "awaiting_assignment" and work.get("run") is None
                    else:
                        assert work["status"] == "completed"
                        observed.append((work["run"]["run_id"], "stopped"))
                        assert (root / "runner.runs" / "workspaces" / parent
                                / "repos" / "example" / "guide.txt").read_text() == "A fixture guide.\n"
                    assert work["issue_state"] == ("closed" if args.scenario == "explicit-close" else "open")
                    observed.append((final["run"]["run_id"], "stopped"))
                    assert sql("runner", "SELECT id, state FROM local_runs ORDER BY rowid") == observed
                    assert sql("server", "SELECT run_id FROM issue_inputs") == [(final["run"]["run_id"],)]
                    operations = 3 if args.scenario == "explicit-close" else 2
                    assert sql("server", "SELECT count(*) FROM runner_operations") == [(operations,)]
                    assert sql("server", "SELECT count(DISTINCT runner_id) FROM runner_assignments") == [(1,)]
                    wait_for(lambda: sql("runner", "SELECT count(*) FROM run_outbox WHERE acknowledged = 0"),
                             lambda rows: rows == [(0,)])
            except BaseException:
                print((root / "runner.stderr").read_text(), file=sys.stderr, end="")
                raise
            finally:
                runner.terminate()
                runner.wait(timeout=7)
    print(f"PASS offline child delegation and same-Task resumption with one Runner slot ({args.scenario})")


if __name__ == "__main__":
    main()
