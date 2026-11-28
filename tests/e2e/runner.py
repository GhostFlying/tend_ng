"""Verify Runner assignment and durable receipts through real processes."""
import argparse
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile

from chat import lose_first_response, seed_archived_input, server, wait_for


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server", type=lambda value: Path(value).resolve())
    parser.add_argument("cli", type=lambda value: Path(value).resolve())
    parser.add_argument("runner", type=lambda value: Path(value).resolve())
    args = parser.parse_args()

    with tempfile.TemporaryDirectory(prefix="tend-runner-") as temporary:
        root = Path(temporary)
        env = {**os.environ, "CODEX_HOME": str(root / "fixture-codex"), "TEND_TOKEN": "tend-runner-e2e"}
        repo = root / "example"
        subprocess.run(["git", "init", "--quiet", repo], check=True)

        def config(runner_id, capabilities):
            path = root / f"{runner_id}.json"
            path.write_text(json.dumps({
                "host_id": "example-host",
                "runner_id": runner_id,
                "slot_limit": 1,
                "repos": [{"id": "example", "path": "example"}],
                "agents": [
                    {"id": "coordinator", "category": "issue-router", "revision": "coord-1"},
                    {"id": "worker", "category": "implementation", "revision": "impl-1",
                     "capabilities": capabilities},
                ],
            }))
            return path

        def execute(at, binary, *words, expects_success=True):
            result = subprocess.run(
                [binary, *words], text=True, capture_output=True, timeout=8,
                env={**env, "TEND_URL": f"http://{at}"},
            )
            assert (result.returncode == 0) == expects_success, result.stderr
            return json.loads(result.stdout) if result.stdout else None

        def sync(address, name, expects_success=True):
            execute(
                address, args.runner, "--config", root / f"{name}.json",
                "--db", root / f"{name}.sqlite", "--once", expects_success=expects_success,
            )

        def issue(address):
            return execute(address, args.cli, "issue", "read", issue_id)

        config("wrong", [])
        config("matching", ["image_gen"])
        config("backup", ["image_gen"])
        with server(args.server, root, env) as address:
            sync(address, "wrong")
            receipt = execute(address, args.cli, "chat", "send", "main", "Dispatch from Chat.")
            timeline = wait_for(
                lambda: execute(address, args.cli, "chat", "read", "main")["messages"],
                lambda rows: rows and rows[0]["router_state"] == "completed",
            )
            [issue_id] = timeline[0]["issue_ids"]
            pending = issue(address)
            assert pending["source_message_id"] == receipt["message_id"]
            assert pending["status"] == "awaiting_assignment", pending

            sync(address, "matching")
            reserved = wait_for(lambda: issue(address), lambda row: row["status"] == "reserved")
            placement = reserved["placement"]
            assignment = placement["assignment_id"]
            assert placement["runner_id"] == "matching", placement
            assert placement["agent"]["revision"] == "coord-1", placement

            # First delivery enters the durable issued state before the response.
            with lose_first_response(address) as (proxy, posted):
                sync(proxy, "matching", expects_success=False)
                reply = posted[0][2]
                assert reply["assignments"][0]["placement"] == placement
                assert str(repo) not in json.dumps(posted[0][0])
            waiting = issue(address)
            assert waiting["status"] == "awaiting_receipt", waiting
            assert waiting["placement"] == placement
            with sqlite3.connect(root / "matching.sqlite") as local:
                assert local.execute("SELECT count(*) FROM received_assignments").fetchone() == (0,)

            sync(address, "backup")
            # Move past the original reservation deadline without waiting for wall time.
            with sqlite3.connect(root / "server.sqlite") as ledger:
                ledger.execute("UPDATE runner_assignments SET expires_at = 0 WHERE id = ?", (assignment,))
            sync(address, "backup")
            assert issue(address)["placement"] == placement

            sync(address, "matching")
            # The Runner owns this receipt before another HTTP request is made.
            with sqlite3.connect(root / "matching.sqlite") as local:
                rows = local.execute(
                    "SELECT id, contract, repositories, decision, acknowledged FROM received_assignments"
                ).fetchall()
            [(local_id, contract, repositories, decision, acknowledged)] = rows
            assert local_id == assignment and acknowledged == 0
            assert json.loads(contract)["placement"] == placement
            assert json.loads(repositories)[0]["path"] == str(repo)
            assert json.loads(decision)["status"] == "accepted"
            assert issue(address)["status"] == "awaiting_receipt"

            # The Server commits the receipt, then the reply disappears.
            with lose_first_response(address) as (proxy, posted):
                sync(proxy, "matching", expects_success=False)
                assert posted[0][2]["confirmed_receipts"] == [assignment]
            assert issue(address)["status"] == "accepted"
            with sqlite3.connect(root / "matching.sqlite") as local:
                assert local.execute("SELECT acknowledged FROM received_assignments").fetchone() == (0,)

        with server(args.server, root, env) as address:
            sync(address, "matching")
            assert issue(address)["placement"] == placement
            assert issue(address)["status"] == "accepted"
            with sqlite3.connect(root / "matching.sqlite") as local:
                assert local.execute("SELECT id, acknowledged FROM received_assignments").fetchall() == [(assignment, 1)]
            with sqlite3.connect(root / "server.sqlite") as ledger:
                assert ledger.execute("SELECT id, state FROM runner_assignments").fetchall() == [(assignment, "accepted")]
                assert ledger.execute("SELECT COUNT(*) FROM runners").fetchone() == (3,)

            sync(address, "backup")
            seed_archived_input(root, "other", "Expire this reservation.")
            timeline = wait_for(
                lambda: execute(address, args.cli, "chat", "read", "other")["messages"],
                lambda messages: len(messages) == 2,
            )
            [issue_id] = timeline[0]["issue_ids"]
            reserved = wait_for(lambda: issue(address), lambda row: row["status"] == "reserved")
            original = reserved["placement"]
            assert original["runner_id"] == "backup", reserved
            with sqlite3.connect(root / "server.sqlite") as ledger:
                ledger.execute("UPDATE runners SET last_seen = 0 WHERE id = 'backup'")
                ledger.execute("UPDATE runner_assignments SET expires_at = 0 WHERE id = ?", (original["assignment_id"],))
            config("replacement", ["image_gen"])
            sync(address, "replacement")
            renewed = wait_for(
                lambda: issue(address),
                lambda row: row.get("placement", {}).get("runner_id") == "replacement",
            )
            assert renewed["placement"]["assignment_id"] != original["assignment_id"]
            sync(address, "backup")
            with sqlite3.connect(root / "backup.sqlite") as local:
                assert local.execute("SELECT count(*) FROM received_assignments").fetchone() == (0,)
            with sqlite3.connect(root / "server.sqlite") as ledger:
                assert ledger.execute(
                    "SELECT state FROM runner_assignments WHERE id = ?", (original["assignment_id"],)
                ).fetchone() == ("revoked",)

    print("PASS Runner matching, reservation expiry, lost delivery, receipt replay, and restart")


if __name__ == "__main__":
    main()
