"""Verify a real Runner finishes a local App Server turn while Server is down."""
import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile

from chat import lose_first_response, server, wait_for

ISSUE_APP_SERVER = Path(__file__).resolve().parents[1] / "fixtures" / "issue_app_server.py"


@contextmanager
def runner(binary, root, env):
    with (root / "runner.stderr").open("w") as log:
        process = subprocess.Popen(
            [binary, "--config", root / "runner.json", "--db", root / "runner.sqlite",
             "--app-server", sys.executable, ISSUE_APP_SERVER, root],
            stderr=log, env=env,
        )
        try:
            yield
        finally:
            process.terminate()
            process.wait(timeout=7)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server", type=lambda value: Path(value).resolve())
    parser.add_argument("cli", type=lambda value: Path(value).resolve())
    parser.add_argument("runner", type=lambda value: Path(value).resolve())
    args = parser.parse_args()

    with tempfile.TemporaryDirectory(prefix="tend-issue-run-") as temporary:
        root = Path(temporary)
        env = {**os.environ, "CODEX_HOME": str(root / "fixture-codex"), "TEND_TOKEN": "tend-issue-run-e2e"}
        subprocess.run(["git", "init", "--quiet", root / "example"], check=True)
        (root / "runner.json").write_text(json.dumps({
            "host_id": "issue-host", "runner_id": "issue-runner", "slot_limit": 1,
            "repos": [{"id": "example", "path": "example"}],
            "agents": [
                {"id": "coordinator", "category": "issue-router", "revision": "issue-9",
                 "model": "fixture-coordinator", "reasoning_effort": "low"},
                {"id": "worker", "category": "implementation", "revision": "work-2",
                 "capabilities": ["image_gen"]},
            ],
        }))

        def cli(address, *words):
            return json.loads(subprocess.check_output(
                [args.cli, *words], env={**env, "TEND_URL": f"http://{address}"}, timeout=8,
            ))

        def local_sql(sql):
            with sqlite3.connect(root / "runner.sqlite") as db:
                return db.execute(sql).fetchall()

        # Reserve an address for the Server's two independent process lifetimes.
        import socket
        with socket.socket() as temporary_socket:
            temporary_socket.bind(("127.0.0.1", 0))
            address = f"127.0.0.1:{temporary_socket.getsockname()[1]}"
        with runner(args.runner, root, {**env, "TEND_URL": f"http://{address}"}):
            with server(args.server, root, env, listen=address):
                message = cli(address, "chat", "send", "main", "Dispatch from Chat.")
                page = wait_for(
                    lambda: cli(address, "chat", "read", "main")["messages"],
                    lambda rows: rows and rows[0]["router_state"] == "completed",
                )
                [issue_id] = page[0]["issue_ids"]
                active = wait_for(
                    lambda: cli(address, "issue", "read", issue_id),
                    lambda view: view["status"] == "running",
                )
                assert active["source_message_id"] == message["message_id"]
                assert active["run"]["phase"]["state"] == "started"
                assert active["issue_state"] == "open"
                assignment = json.loads((root / "launched.json").read_text())
                assert assignment["task_id"] == active["task_id"]
                [active_run] = local_sql("SELECT id, state FROM local_runs")
                assert active_run == (active["run"]["run_id"], "running")
            # No Server process exists while the controlled native turn finishes.
            (root / "finish-issue").touch()
            stopped = wait_for(
                lambda: local_sql("SELECT state FROM local_runs"), lambda rows: rows == [("stopped",)],
            )
            assert stopped == [("stopped",)]
            unconfirmed = local_sql("SELECT id FROM run_outbox WHERE acknowledged = 0")
            assert unconfirmed

        def sync_once(target, expects_success):
            result = subprocess.run(
                [args.runner, "--config", root / "runner.json", "--db", root / "runner.sqlite", "--once"],
                env={**env, "TEND_URL": f"http://{target}"}, capture_output=True, text=True, timeout=8,
            )
            assert (result.returncode == 0) == expects_success, result.stderr

        with server(args.server, root, env) as address:
            with lose_first_response(address) as (proxy, posted):
                sync_once(proxy, False)
                assert set(posted[0][2]["confirmed_runs"]) == {event for event, in unconfirmed}
            final = cli(address, "issue", "read", issue_id)
            assert final["status"] == "completed" and final["issue_state"] == "open", final
            assert final["run"]["run_id"] == active["run"]["run_id"]
            assert final["run"]["turn_id"] == active["run"]["turn_id"]
            assert final["run"]["phase"]["outcome"] == "completed"
            sync_once(address, True)
            assert cli(address, "issue", "read", issue_id) == final
            assert local_sql("SELECT id, state FROM local_runs") == [(active["run"]["run_id"], "stopped")]
            assert local_sql("SELECT count(*) FROM run_outbox WHERE acknowledged = 0") == [(0,)]
            with sqlite3.connect(root / "server.sqlite") as db:
                assert db.execute("SELECT count(*) FROM runner_run_events").fetchone() == (2,)
                assert db.execute("SELECT json_extract(profile, '$.running_slots') FROM runners").fetchone() == (0,)
    print("PASS Issue Agent finished offline and replayed the original Run and stop receipt")


if __name__ == "__main__":
    main()
