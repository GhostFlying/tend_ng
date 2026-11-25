"""Delegate offline, run a work child, and resume the same parent Task on one slot."""
import argparse
from contextlib import nullcontext
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import sys
import tempfile

from child_windows import expire_child_window
from chat import lose_first_response, server, wait_for

FIXTURE = Path(__file__).resolve().parents[1] / "fixtures" / "coordination_app_server.py"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("server", "cli", "runner"):
        parser.add_argument(executable, type=lambda value: Path(value).resolve())
    parser.add_argument("--scenario", choices=(
        "explicit-close", "task-only", "no-candidate", "late-close", "parent-failed",
    ),
                        default="explicit-close")
    args = parser.parse_args()
    late = args.scenario == "late-close"
    child_closes = args.scenario not in {"task-only", "no-candidate"}
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

        def start_runner(target, stderr):
            return subprocess.Popen(
                [args.runner, "--config", root / "runner.json", "--db", root / "runner.sqlite",
                 "--app-server", sys.executable, FIXTURE, root, args.scenario],
                env={**env, "TEND_URL": f"http://{target}"}, stderr=stderr,
            )

        def has_followup(reply):
            return any("followup" in assignment for assignment in reply.get("assignments", []))

        with (root / "runner.stderr").open("w") as stderr:
            runner = start_runner(address, stderr)
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
                         lambda rows: rows == [("finished" if late else "waiting",)])
                [(run_id, state)] = sql("runner", "SELECT id, state FROM local_runs")
                assert (run_id, state) == (first["run"]["run_id"], "stopped")
                assert sql("runner", "SELECT count(*) FROM run_outbox WHERE record_kind = 'operation' "
                                    "AND acknowledged = 0") == [(2 if late else 1,)]
                delegated = json.loads((root / "delegation.json").read_text())
                child = delegated["action"]["issue_id"]
                proxy = lose_first_response(address, drop_when=has_followup) if late else nullcontext((address, []))
                with server(args.server, root, env, listen=address), proxy as (target, posted):
                    if late:
                        runner.terminate()
                        runner.wait(timeout=7)
                        runner = start_runner(target, stderr)
                    if args.scenario != "no-candidate":
                        expire_child_window(root, "issue_closed" if child_closes else "task_stopped")
                    final = wait_for(lambda: cli("issue", "read", parent),
                                     lambda view: view["status"] == "completed"
                                     and (view["task_id"] != first["task_id"] if late else True))
                    assert (final["task_id"] != first["task_id"]) == late
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
                    assert work["issue_state"] == ("closed" if child_closes else "open")
                    observed.append((final["run"]["run_id"], "stopped"))
                    assert sql("runner", "SELECT id, state FROM local_runs ORDER BY rowid") == observed
                    assert sql("server", "SELECT run_id FROM issue_inputs") == [(final["run"]["run_id"],)]
                    operations = 3 if child_closes else 2
                    assert sql("server", "SELECT count(*) FROM runner_operations") == [(operations,)]
                    assert sql("server", "SELECT count(DISTINCT runner_id) FROM runner_assignments") == [(1,)]
                    if late:
                        assert sql("server", "SELECT id, status, previous_task_id FROM tasks "
                                             "WHERE issue_id = ? ORDER BY rowid", parent) == [
                            (first["task_id"], "completed", None),
                            (final["task_id"], "completed", first["task_id"]),
                        ]
                        first_delivery, repeated_delivery = [reply for _, _, reply in posted if has_followup(reply)]
                        assert first_delivery["assignments"] == repeated_delivery["assignments"]
                        assert first_delivery["inputs"] == repeated_delivery["inputs"]
                    if args.scenario == "parent-failed":
                        [first_run] = sql("server", "SELECT observation FROM task_runs WHERE id = ?",
                                          first["run"]["run_id"])
                        assert json.loads(first_run[0])["phase"] == {
                            "state": "stopped", "outcome": "failed",
                            "error": "Fixture Provider unavailable", "waiting_for_children": True,
                        }
                    wait_for(lambda: sql("runner", "SELECT count(*) FROM run_outbox WHERE acknowledged = 0"),
                             lambda rows: rows == [(0,)])
            except BaseException:
                print((root / "runner.stderr").read_text(), file=sys.stderr, end="")
                raise
            finally:
                runner.terminate()
                runner.wait(timeout=7)
    print(f"PASS offline child delegation and parent coordination with one Runner slot ({args.scenario})")


if __name__ == "__main__":
    main()
