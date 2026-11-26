"""Exercise the main-build upgrade entry with a real Server and two Runner generations."""

import argparse
from contextlib import ExitStack, closing
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time

from chat import server, wait_for
from issue_chat import TESTS, free_port, prepare

FIXTURE = TESTS / "fixtures/runner_upgrade_app_server.py"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bundle", type=lambda value: Path(value).resolve())
    parser.add_argument("--predecessor", type=lambda value: Path(value).resolve(),
                        help="optionally use a previous release's Runner binary")
    parser.add_argument("--trailing-dot-db", action="store_true",
                        help="start the predecessor with a relative database name ending in a dot")
    args = parser.parse_args()
    binaries = args.bundle / "bin"
    with tempfile.TemporaryDirectory(prefix="tend-runner-upgrade-") as directory, ExitStack() as stack:
        root = Path(directory)
        address, old_inspection = free_port(), free_port()
        prepare(root, old_inspection)
        config = root / "runner.json"
        db = root / ("runner." if args.trailing_dot_db else "runner.sqlite")
        config.write_text(json.dumps({**json.loads(config.read_text()), "slot_limit": 2}))
        home = root / "codex-home"
        home.mkdir()
        env = {**os.environ, "TEND_TOKEN": "runner-upgrade-fixture", "TEND_URL": f"http://{address}",
               "CODEX_HOME": str(home)}
        # Simulate interruption between durable retirement and local cleanup,
        # without modifying the bundle passed by CI or a previous release.
        entry = root / "bundle" / "bin"
        entry.mkdir(parents=True)
        shutil.copy2(args.bundle / "REVISION", entry.parent / "REVISION")
        shutil.copy2(binaries / "tend-runner-upgrade", entry / "tend-runner-upgrade")
        for executable in ("tend", "tend-runner"):
            (entry / executable).symlink_to(binaries / executable)
        helper = entry / "tend-runner-retire-files"
        interrupt = root / "interrupt-cleanup"
        interrupt.touch()
        helper.write_text(
            f"#!{sys.executable}\nimport os, sys\nfrom pathlib import Path\n"
            f"if sys.argv[1] == 'cleanup' and Path({str(interrupt)!r}).exists(): sys.exit(75)\n"
            f"os.execv({str(binaries / helper.name)!r}, sys.argv)\n"
        )
        helper.chmod(0o700)
        rollback = root / "previous" / "runner.json"
        rollback.parent.mkdir()
        shutil.copy2(config, rollback)
        retained_config = rollback.read_bytes()
        generation = db.with_name(f"{db.name}.upgrade")
        next_db = None

        def SQL(database, statement, *params):
            with closing(sqlite3.connect(database)) as connection:
                return connection.execute(statement, params).fetchall()

        def cli(*words):
            return json.loads(subprocess.check_output([binaries / "tend", *words], env=env, timeout=10))

        def roster():
            return {runner["profile"]["runner_id"]: runner for runner in cli("runner", "list")}

        def evidence(name, file="started"):
            path = root / f"{name}.{file}"
            return wait_for(lambda: path.exists() and path.read_text(), bool)

        def dispatch(name):
            cli("chat", "send", "main", f"Upgrade fixture: {name}")
            return json.loads(evidence(name, "dispatch"))

        def local_active(database):
            return SQL(database, "SELECT count(*) FROM local_runs WHERE state IN ('prepared','running')")[0][0]

        def terminate(process):
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=10)

        def release(name):
            (root / f"{name}.release").touch()

        def report_log(path):
            if path.exists() and sys.exc_info()[0] is not None:
                print(path.read_text(), file=sys.stderr)

        def cleanup_successor():
            # The upgrade intentionally detaches the Runner. Only signal the
            # process whose exact temporary database path is on its command line.
            for name in ("original", "laggard", "overlap-a", "overlap-b"):
                release(name)
            for proc in Path("/proc").iterdir():
                if not proc.name.isdecimal():
                    continue
                try:
                    command = (proc / "cmdline").read_bytes().split(b"\0")
                    if next_db is not None and os.fsencode(next_db) in command and Path(os.fsdecode(command[0])).name == "tend-runner":
                        pid = int(proc.name)
                        os.kill(pid, signal.SIGTERM)
                        wait_for(lambda: Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0]
                                 if Path(f"/proc/{pid}/stat").exists() else "Z",
                                 lambda state: state == "Z")
                except (FileNotFoundError, PermissionError, ProcessLookupError):
                    pass

        def start_server(stack):
            stack.enter_context(server(binaries / "tend-server", root, env, listen=address, github_cli="/bin/false",
                                       router_fixture=FIXTURE))

        server_context = stack.enter_context(ExitStack())
        start_server(server_context)
        prior_log = root / "old.log"
        with prior_log.open("wb") as log:
            old = subprocess.Popen([
                args.predecessor or binaries / "tend-runner", "--config", config,
                "--db", db.name if args.trailing_dot_db else db,
                "--app-server", sys.executable, FIXTURE, root,
            ], cwd=root if args.trailing_dot_db else None, env=env, stdout=log, stderr=log)
        stack.callback(terminate, old)
        stack.callback(cleanup_successor)
        stack.push(lambda *_: report_log(prior_log))
        stack.push(lambda *_: report_log(generation / "runner.log"))
        wait_for(roster, lambda rows: rows.get("demo-runner", {}).get("online"))
        # A different Runner on this Host is neither the predecessor nor
        # part of the temporary two-generation budget.
        peer_config, peer_db = root / "peer.json", root / "peer.sqlite"
        peer_config.write_text(json.dumps({**json.loads(config.read_text()), "runner_id": "unrelated",
                                           "standby": True, "inspect": None}))
        peer = subprocess.Popen([binaries / "tend-runner", "--config", peer_config, "--db", peer_db],
                                env=env, stderr=subprocess.DEVNULL)
        stack.callback(terminate, peer)
        wait_for(roster, lambda rows: rows.get("unrelated", {}).get("online"))

        alias = root / "alias.sqlite"
        alias.symlink_to(peer_db)
        alias_ready = root / "alias.ready"
        duplicate = subprocess.Popen([binaries / "tend-runner", "--config", peer_config, "--db", alias,
                                      "--ready-file", alias_ready],
                                     env=env, stderr=subprocess.PIPE, text=True)
        stack.callback(terminate, duplicate)
        _, refused = duplicate.communicate(timeout=5)
        assert duplicate.returncode and "already using this database" in refused, refused
        assert not alias_ready.exists() and peer.poll() is None

        # The operator may run the upgrade from a different Codex installation.
        upgrade_env = {**env, "CODEX_HOME": str(root / "operator-home")}

        def refuse_mismatched_identity(command):
            original_config = config.read_text()
            try:
                config.write_text(peer_config.read_text())
                rejected = subprocess.run(command, env=upgrade_env, capture_output=True, text=True, timeout=5)
                assert rejected.returncode == 1 and "identity saved in its database" in rejected.stderr, rejected.stderr
            finally:
                config.write_text(original_config)
            assert old.poll() is None and peer.poll() is None
            assert roster()["unrelated"]["successor"] is None

        original = dispatch("original")
        old_run = json.loads(evidence("original"))
        laggard = dispatch("laggard")
        wait_for(lambda: cli("issue", "read", laggard["issue_id"]), lambda row: row["status"] == "running")
        assert local_active(db) == 2

        with socket.socket() as blocked:
            blocked.bind(("127.0.0.1", 0))
            blocked.listen()
            listen = f"127.0.0.1:{blocked.getsockname()[1]}"
            command = [entry / "tend-runner-upgrade", "--config", config, "--db", db,
                       "--inspect-listen", listen, "--inspect-url", f"http://{listen}", "--timeout", "85"]
            refuse_mismatched_identity(command)
            assert not (generation / "upgrade.json").exists()
            assert set(roster()) == {"demo-runner", "unrelated"}
            failed = subprocess.run(command, env=upgrade_env, capture_output=True, text=True, timeout=10)
        assert failed.returncode == 1 and "Successor exited" in failed.stderr, failed.stderr
        ledger = json.loads((generation / "upgrade.json").read_text())
        successor = ledger["config"]["runner_id"]
        next_db = root / f"{successor}.sqlite"
        assert roster()["demo-runner"]["successor"] is None and successor not in roster()
        assert ledger["revision"] == (args.bundle / "REVISION").read_text().strip()
        assert ledger["config"]["workspace_root"] == str(root / "runner.runs")
        assert ledger["runtime"]["codex_home"] == str(home)
        # Changing the failed standby listener needs no manual configuration
        # edit and keeps the same planned Runner identity.
        listen = free_port()
        command[command.index("--inspect-listen") + 1] = listen
        command[command.index("--inspect-url") + 1] = f"http://{listen}"

        first = subprocess.Popen(command, env=upgrade_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        stack.callback(terminate, first)
        [mapping] = wait_for(lambda: SQL(root / "server.sqlite",
                                        "SELECT predecessor, successor FROM runner_replacements"), bool)
        assert mapping == ("demo-runner", successor)
        fact = cli("runner", "retirement", "demo-runner")
        assert fact == {"runner_id": "demo-runner", "host_id": ledger["config"]["host_id"],
                        "successor": successor, "host_data_policy": 1,
                        "drained_at": None, "retired_at": None, "artifacts": []}
        assert roster()["unrelated"]["host_data_policy"] == 1
        parallel = subprocess.run(command, env=upgrade_env, capture_output=True, text=True, timeout=5)
        assert parallel.returncode == 1 and "Another upgrade" in parallel.stderr, parallel.stderr
        too_early = subprocess.run([binaries / "tend", "runner", "retire", "demo-runner"],
                                   env=env, capture_output=True, text=True, timeout=5)
        assert too_early.returncode and "HTTP 409" in too_early.stderr, too_early.stderr
        assert roster()[successor]["execution_slot_limit"] == 0 and old.poll() is None
        assert roster()["unrelated"]["execution_slot_limit"] == 2
        assert cli("runner", "replace", "demo-runner", successor) == {
            "predecessor": "demo-runner", "successor": successor}

        overlap = {name: dispatch(name) for name in ("overlap-a", "overlap-b")}
        sent = cli("issue", "message", original["issue_id"], "Continue on the new Runner.")
        wait_for(lambda: SQL(db, "SELECT count(*) FROM local_issue_inputs WHERE run_id IS NULL"), lambda rows: rows == [(1,)])
        assert local_active(next_db) == 0
        release("laggard")
        if not args.predecessor:
            wait_for(roster, lambda rows: rows[successor]["execution_slot_limit"] == 1)
            def started():
                return [name for name in overlap if (root / f"{name}.started").exists()]
            [one] = wait_for(started, bool)
            wait_for(lambda: cli("issue", "read", overlap[one]["issue_id"]), lambda row: row["status"] == "running")
            assert local_active(db) == local_active(next_db) == 1
            assert started() == [one]
        else:
            wait_for(lambda: local_active(db), lambda count: count == 1)
            assert roster()[successor]["execution_slot_limit"] == 0

        terminate(first)
        first.communicate()
        refuse_mismatched_identity(command)
        assert roster()["demo-runner"]["successor"] == successor
        marker = next(generation.glob("ready-*"))
        next_pid = marker.read_text()
        runtime_env = (Path("/proc") / next_pid.strip() / "environ").read_bytes().split(b"\0")
        assert os.fsencode(f"CODEX_HOME={home}") in runtime_env
        server_context.close()
        start_server(server_context)
        assert roster()[successor]["execution_slot_limit"] == (0 if args.predecessor else 1)
        assert local_active(db) == 1 and local_active(next_db) == (0 if args.predecessor else 1)
        retry = subprocess.Popen(command, env=upgrade_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        stack.callback(terminate, retry)
        # The original process has started its native turn and still owns it.
        assert old.poll() is None and not (root / "continued.started").exists()
        assert roster()["demo-runner"]["retired_at"] is None
        release("original")
        assert old.wait(timeout=15) == 0
        stdout, stderr = retry.communicate(timeout=15)
        assert retry.returncode == 1 and "cleanup did not complete" in stderr, (stdout, stderr)
        assert roster()["demo-runner"]["retired_at"] and db.is_file()
        assert SQL(db, "SELECT state, thread_id FROM local_runs WHERE id = ?", old_run["run"]) == [
            ("stopped", old_run["thread"])]
        fact = cli("runner", "retirement", "demo-runner")
        assert fact["drained_at"] and fact["retired_at"] and fact["successor"] == successor
        assert (generation / "retirement.json").is_file()
        interrupt.unlink()
        completed = subprocess.run(command, env=upgrade_env, capture_output=True, check=True, text=True, timeout=15)
        receipt = json.loads(completed.stdout)
        assert receipt["successor"] == successor and receipt["state"] == "retired"
        assert Path(receipt["db"]) == next_db and next_db.is_file() and not db.exists()
        assert Path(receipt["retirement"]) == generation / "retirement.json"
        assert rollback.read_bytes() == retained_config and home.is_dir() and peer_db.is_file()
        history = cli("run", "rollout", old_run["run"])
        assert history["runner_id"] == "demo-runner" and history["reader_id"] == successor
        assert history["turn"]["id"] == old_run["turn"]
        assert marker.read_text() == next_pid
        assert len(list(generation.glob("ready-*"))) == 1
        assert roster()["demo-runner"]["retired_at"] and roster()[successor]["execution_slot_limit"] == 2

        for name in overlap:
            assert json.loads(evidence(name))["runner"] == successor
        wait_for(lambda: local_active(next_db), lambda count: count == 2)
        for name in overlap:
            release(name)
        continued = json.loads(evidence("continued"))
        assert continued["task"] == old_run["task"] == original["task_id"]
        assert continued["runner"] == successor and old_run["runner"] == "demo-runner"
        assert continued["thread"] != old_run["thread"] and continued["run"] != old_run["run"]
        assert continued["cwd"] == old_run["cwd"] and Path(continued["cwd"]).is_relative_to(root)
        wait_for(lambda: SQL(root / "server.sqlite",
                             "SELECT run_id FROM issue_inputs WHERE json_extract(payload, '$.message.id') = ?",
                             sent["message_id"]), lambda rows: rows == [(continued["run"],)])
        assert SQL(root / "server.sqlite", """
            SELECT a.runner_id, json_extract(r.observation, '$.phase.outcome')
            FROM task_runs r JOIN runner_assignments a ON a.id = r.assignment_id WHERE r.id = ?
            """, old_run["run"]) == [("demo-runner", "completed")]
        assert (root / f"{old_run['thread']}.json").exists()
        assert SQL(root / "server.sqlite", "SELECT count(*) FROM task_controls") == [(0,)]

        wait_for(lambda: local_active(next_db), lambda count: count == 0)
        repeated = subprocess.run(command, env=upgrade_env, capture_output=True, check=True, text=True, timeout=10)
        assert json.loads(repeated.stdout) == receipt and not db.exists()
        assert SQL(root / "server.sqlite", "SELECT predecessor, successor, slot_budget FROM runner_replacements") == [
            ("demo-runner", successor, 2)]
        print("PASS archive upgrade, natural exit, restart-safe shared capacity, original Task/workspace/input, and retries")


if __name__ == "__main__":
    main()
