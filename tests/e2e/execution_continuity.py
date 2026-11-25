"""Verify isolated Tend review and grouped parent wake across restart and lost reply."""

import argparse
from contextlib import ExitStack, contextmanager
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import time

from chat import lose_first_response, server, wait_for
from issue_chat import free_port, prepare

FIXTURE = Path(__file__).resolve().parents[1] / "fixtures/execution_continuity_app_server.py"


def exercise(args, root):
    address = free_port()
    prepare(root, free_port())
    config = root / "runner.json"
    settings = json.loads(config.read_text())
    settings["agents"].append({"id": "reviewer", "category": "review", "revision": "one",
                               "model": "fixture-reviewer"})
    if args.mode == "drift":
        settings["agents"][1]["app_server"] = "author-app"
        settings["app_servers"] = {"author-app": {
            "program": sys.executable, "args": [str(FIXTURE), str(root), args.mode],
            "codex_home": "author-codex-old",
        }}
    config.write_text(json.dumps(settings))
    (root / "base").write_text(subprocess.check_output(
        ["git", "-C", root / "example", "rev-parse", "HEAD"], text=True).strip())
    env = {**os.environ, "TEND_TOKEN": "continuity-fixture", "TEND_URL": f"http://{address}",
           "CODEX_HOME": str(root / "fixture-codex")}

    def cli(*words):
        return json.loads(subprocess.check_output([args.cli, *words], env=env, timeout=10))

    def sql(database, query, *bind):
        with sqlite3.connect(root / f"{database}.sqlite") as db:
            return db.execute(query, bind).fetchall()

    @contextmanager
    def runner(target):
        with (root / "runner.log").open("a") as log:
            process = subprocess.Popen([
                args.runner, "--config", config, "--db", root / "runner.sqlite",
                "--app-server", sys.executable, FIXTURE, root, args.mode,
            ], env={**env, "TEND_URL": f"http://{target}"}, stderr=log)
            try:
                yield
            finally:
                process.terminate()
                process.wait(timeout=8)

    def delayed():
        return sql("server", "SELECT id, task_id, wake_at FROM issue_inputs "
                             "WHERE wake_at IS NOT NULL AND run_id IS NULL ORDER BY rowid")

    def parent_runs():
        return sql("runner", "SELECT r.id, r.thread_id FROM local_runs r "
                             "JOIN received_assignments a ON a.id = r.assignment_id "
                             "WHERE a.task_id = ? ORDER BY r.rowid", parent_task)

    def finished(issue):
        return cli("issue", "read", issue)["status"] == "completed"

    with ExitStack() as processes:
        processes.enter_context(runner(address))
        if args.mode == "drift":
            with server(args.server, root, env, listen=address):
                cli("chat", "send", "main", "Dispatch from Chat.")
                wait_for(lambda: (root / "author-before-review").is_file(), bool)
                wait_for(delayed, lambda rows: len(rows) == 2)
            (root / "submit-review").touch()
            wait_for(lambda: sql("runner", "SELECT retains_workspace FROM received_assignments"),
                     lambda rows: (1,) in rows)
            processes.close()
            settings["app_servers"]["author-app"]["codex_home"] = "author-codex-new"
            settings["agents"][1].update(model="fixture-worker-updated", reasoning_effort="low", revision="two")
            config.write_text(json.dumps(settings))
            processes.enter_context(runner(address))
        with server(args.server, root, env, listen=address):
            if args.mode != "drift":
                cli("chat", "send", "main", "Dispatch from Chat.")
            wait_for(lambda: (root / "review-running").is_file(), bool)
            binding = sql("server", "SELECT author_issue_id, author_task_id, review_issue_id, "
                                    "review_task_id, candidate FROM tend_reviews")
            [(author, author_task, review, review_task, candidate)] = binding
            parent = cli("issue", "read", author)["parent_issue_id"]
            parent_task = cli("issue", "read", parent)["task_id"]
            before = wait_for(delayed, lambda rows: len(rows) == 2)
            [(deadline,)] = sql("server", "SELECT DISTINCT wake_at FROM issue_inputs WHERE wake_at IS NOT NULL")
            assert 295_000 < deadline - time.time_ns() // 1_000_000 <= 300_000
            assert cli("issue", "read", author)["status"] == "waiting"
            wait_for(lambda: cli("issue", "read", review)["status"], lambda state: state == "running")
            assert sql("runner", "SELECT execution_state, retains_workspace FROM received_assignments "
                                 "WHERE task_id = ?", author_task) == [("waiting", 1)]
            assert len(parent_runs()) == 1
            assert any(m["text"] == "Initial candidate is ready for review."
                       for m in cli("issue", "messages", parent)["messages"])
            assert cli("issue", "read", author)["artifacts"][0]["available"]
            if args.mode == "probe":
                probe = json.loads((root / "probe.json").read_text())["action"]["issue_id"]
                assert cli("issue", "read", probe).get("run") is None

        # Restart the Server while the window and the independent review are open.
        with server(args.server, root, env, listen=address):
            assert delayed() == before
            (root / "deliver-review").touch()
            if args.mode == "active":
                def guarded():
                    return sql("runner", "SELECT r.id, r.resume_guard, r.turn_id FROM local_runs r "
                                         "JOIN received_assignments a ON a.id = r.assignment_id "
                                         "WHERE a.task_id = ? AND r.state = 'prepared' AND r.resume_guard IS NOT NULL",
                               author_task)

                [blocked] = wait_for(guarded, bool)
                assert blocked[2] is None
                assert sql("server", "SELECT run_id FROM issue_inputs WHERE task_id = ?", author_task) == [(None,)]
                assert cli("issue", "read", author)["status"] == "waiting"
                processes.close()
                processes.enter_context(runner(address))
                assert guarded() == [blocked]
                (root / "allow-unknown").touch()
                wait_for(lambda: (root / "reported-unknown").is_file(), bool)
                assert guarded() == [blocked]
                assert sql("server", "SELECT run_id FROM issue_inputs WHERE task_id = ?", author_task) == [(None,)]
                (root / "allow-resume").touch()
            wait_for(lambda: finished(author) and finished(review), bool)
            if args.mode == "probe":
                wait_for(lambda: finished(probe), bool)
                assert (root / "probe-ran").is_file()
            grouped = delayed()
            assert len(grouped) == (6 if args.mode == "probe" else 5), grouped
            assert all(task == parent_task and due == deadline for _, task, due in grouped)
            assert len(parent_runs()) == 1
            assert sql("server", "SELECT count(*), count(DISTINCT run_id) FROM issue_inputs "
                                 "WHERE task_id = ? AND json_extract(payload, '$.kind') = 'review_finished'",
                       author_task) == [(1, 1)]
            assert sql("server", "SELECT count(*) FROM issue_inputs WHERE task_id = ? "
                                 "AND json_extract(payload, '$.kind') = 'review_finished'", parent_task) == [(0,)]
            author_runs = sql("runner", "SELECT count(*), count(DISTINCT r.thread_id) FROM local_runs r "
                                        "JOIN received_assignments a ON a.id = r.assignment_id WHERE a.task_id = ?",
                              author_task)
            assert author_runs == [(2, 2 if args.mode in {"missing", "broken", "drift"} else 1)]
            assert not sql("runner", "SELECT 1 FROM local_issue_inputs WHERE assignment_id = "
                                     "(SELECT id FROM received_assignments WHERE task_id = ?)", parent_task)

    def is_parent_batch(reply):
        return any(i["input_id"] in {row[0] for row in grouped} for i in reply.get("inputs", []))

    with server(args.server, root, env, listen=address), lose_first_response(
            address, drop_when=is_parent_batch) as (proxy, posted), runner(proxy):
        # Advance the deadline in this temporary database after verifying the
        # real deadline above. The isolated Server performs its normal release.
        sql("server", "UPDATE issue_inputs SET wake_at = 1 WHERE task_id = ? AND wake_at = ?",
            parent_task, deadline)
        wait_for(lambda: finished(parent), bool)
        assert len([reply for _, _, reply in posted if is_parent_batch(reply)]) >= 2
        ids = {row[0] for row in grouped}
        assert {i["input_id"] for i in json.loads((root / "parent-inputs.json").read_text())} == ids
        assert sql("server", "SELECT count(*), count(DISTINCT run_id) FROM issue_inputs WHERE task_id = ?",
                   parent_task) == [(len(grouped), 1)]
        assert len(parent_runs()) == 2 and len({thread for _, thread in parent_runs()}) == 1
        assert len(cli("issue", "messages", parent)["messages"]) > 1
    print(f"PASS {args.mode}: {2 + (args.mode == 'probe')} work child Issues, 1 parent wake, "
          f"{author_runs[0][1]} author thread(s) over 2 Runs, 1 parent thread over 2 Runs, "
          f"{len(grouped)} facts in one parent wake")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("server", "cli", "runner"):
        parser.add_argument(executable, type=lambda value: Path(value).resolve())
    parser.add_argument("--mode", choices=("resume", "missing", "broken", "drift", "probe", "active"), default="resume")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-execution-continuity-") as directory:
        root = Path(directory)
        try:
            exercise(args, root)
        except BaseException:
            if (root / "runner.log").is_file():
                print((root / "runner.log").read_text(), file=sys.stderr)
            raise


if __name__ == "__main__":
    main()
