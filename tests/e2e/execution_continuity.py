"""Verify shared-workspace Tend review and grouped parent wake across restart and lost reply."""

import argparse
from contextlib import ExitStack, closing, contextmanager
from http.client import HTTPConnection
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import time
import uuid

from chat import lose_first_response, server, wait_for
from issue_chat import free_port, prepare

FIXTURE = Path(__file__).resolve().parents[1] / "fixtures/execution_continuity_app_server.py"


def exercise(args, root):
    address = free_port()
    prepare(root, free_port())
    config = root / "runner.json"
    settings = json.loads(config.read_text())
    fallback = args.mode in {"fallback", "workspace-missing", "workspace-replaced"}
    two_rounds = args.mode in {"resume", "review-missing", "review-unknown"}
    # Distinguish the directory lock from slot exhaustion, while also proving
    # dispatch to the local queue when the sole slot is in use in fallback mode.
    settings["slot_limit"] = 1 if fallback or args.mode == "drift" else 2
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

    def mark(name):
        (root / name).touch()

    def gate(name):
        wait_for(lambda: (root / name).is_file(), bool)

    def local_task(task):
        return sql("runner", "SELECT execution_state, retains_workspace FROM received_assignments WHERE task_id = ?", task)

    def task_runs(task):
        return sql("runner", "SELECT r.rowid, r.state, r.thread_id, r.id, r.process_pid FROM local_runs r "
                             "JOIN received_assignments a ON a.id = r.assignment_id "
                             "WHERE a.task_id = ? ORDER BY r.rowid", task)

    def is_review_result(reply):
        return any(i["kind"] == "review_finished" for i in reply.get("inputs", []))

    def stop_author(round_no, prior_review_runs):
        gate(f"author-continued-{round_no}")
        binding = wait_for(lambda: sql(
            "server", "SELECT author_issue_id, author_task_id, review_issue_id, review_task_id, "
                      "candidate, scope, request_id, author_workspace FROM tend_reviews ORDER BY rowid"),
            lambda rows: len(rows) == round_no)[-1]
        _, author_task, _, review_task, *_ = binding
        assert wait_for(lambda: local_task(review_task), lambda rows: rows == [("queued", 0)])
        assert local_task(author_task) == [("active", 0)]
        assert task_runs(author_task)[-1][1] == "running"
        assert len(task_runs(review_task)) == prior_review_runs
        if args.mode in {"workspace-missing", "workspace-replaced"}:
            directory = Path((root / "author-cwd").read_text())
            directory.rename(root / "saved-author-directory")
            if args.mode == "workspace-replaced":
                directory.mkdir()
        mark(f"allow-author-stop-{round_no}")
        return binding

    def deliver_review(round_no):
        gate(f"review-running-{round_no}")
        assert local_task(review_task) == [("active", 0)]
        assert local_task(author_task) == [("waiting", 0)]
        assert task_runs(author_task)[-1][1] == "stopped"
        assert task_runs(author_task)[-1][0] < task_runs(review_task)[-1][0]
        mark(f"deliver-review-{round_no}")
        gate(f"result-{round_no}")
        assert wait_for(lambda: local_task(author_task), lambda rows: rows == [("queued", 0)])
        assert local_task(review_task) == [("active", 0)]
        assert task_runs(review_task)[-1][1] == "running"
        assert len(task_runs(author_task)) == round_no
        mark(f"allow-review-stop-{round_no}")

    with ExitStack() as processes, lose_first_response(address, drop_when=is_review_result) as (proxy, posted):
        if not fallback:
            processes.enter_context(runner(proxy))
        if args.mode == "drift":
            with server(args.server, root, env, listen=address):
                cli("chat", "send", "main", "Dispatch from Chat.")
                gate("author-before-review")
                wait_for(delayed, lambda rows: len(rows) == 2)
            mark("submit-review")
            gate("author-continued-1")
            mark("allow-author-stop-1")
            author_task = json.loads((root / "author.json").read_text())["action"]["task_id"]
            wait_for(lambda: local_task(author_task), lambda rows: rows == [("waiting", 0)])
            processes.close()
            settings["app_servers"]["author-app"]["codex_home"] = "author-codex-new"
            settings["agents"][1].update(model="fixture-worker-updated", reasoning_effort="low", revision="two")
            config.write_text(json.dumps(settings))
            processes.enter_context(runner(proxy))
        with server(args.server, root, env, listen=address):
            if args.mode != "drift":
                cli("chat", "send", "main", "Dispatch from Chat.")
                if fallback:
                    # Top-level tasks start as coordinators, which cannot submit
                    # files. Set up a top-level work author before any allocation,
                    # leaving both workspace selectors absent on the real contract.
                    wait_for(lambda: sql("server", "SELECT id FROM tasks WHERE current_assignment IS NULL"), bool)
                    sql("server", "UPDATE tasks SET category = 'implementation' WHERE current_assignment IS NULL")
                    processes.enter_context(runner(proxy))
                first = stop_author(1, 0)
            else:
                [first] = wait_for(lambda: sql(
                    "server", "SELECT author_issue_id, author_task_id, review_issue_id, review_task_id, "
                              "candidate, scope, request_id, author_workspace FROM tend_reviews"), bool)
            author, author_task, review, review_task, _, _, _, workspace = first
            assert workspace == (f"tasks/{author_task}" if fallback else f"workspaces/{cli('issue', 'read', author)['parent_issue_id']}")
            assert sql("server", "SELECT count(DISTINCT runner_id), count(*) FROM runner_assignments "
                                 "WHERE task_id IN (?, ?)", author_task, review_task) == [(1, 2)]
            assert not (Path((root / "author-cwd").read_text()).parent / review).exists()
            if args.mode in {"workspace-missing", "workspace-replaced"}:
                wait_for(lambda: finished(author), bool)
                assert cli("issue", "read", review)["status"] == "failed"
                assert not (root / "review-running-1").exists()
                assert sql("server", "SELECT json_extract(p.payload, '$.run_id'), json_extract(p.payload, '$.summary') "
                                     "FROM tend_reviews v JOIN issue_inputs p ON p.id = v.result_input_id")[0][0] == task_runs(review_task)[0][3]
                print(f"PASS {args.mode}: original Review Task reports failure without starting a native thread")
                return
            gate("review-running-1")
            wait_for(lambda: cli("issue", "read", review)["status"], lambda state: state == "running")
            assert cli("issue", "read", author)["status"] == "waiting"
            if not fallback:
                parent = cli("issue", "read", author)["parent_issue_id"]
                parent_task = cli("issue", "read", parent)["task_id"]
                before = wait_for(delayed, lambda rows: len(rows) == 2)
                [(deadline,)] = sql("server", "SELECT DISTINCT wake_at FROM issue_inputs WHERE wake_at IS NOT NULL")
                assert 295_000 < deadline - time.time_ns() // 1_000_000 <= 300_000
                assert len(parent_runs()) == 1
                assert any(m["text"] == "Initial candidate is ready for review."
                           for m in cli("issue", "messages", parent)["messages"])
                assert cli("issue", "read", author)["artifacts"][0]["available"]
                if args.mode == "probe":
                    probe = json.loads((root / "probe.json").read_text())["action"]["issue_id"]
                    assert cli("issue", "read", probe).get("run") is None

        # Restart Server while the original reviewer is still running.
        with server(args.server, root, env, listen=address):
            if not fallback:
                assert delayed() == before
            deliver_review(1)
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
                processes.enter_context(runner(proxy))
                assert guarded() == [blocked]
                mark("allow-unknown")
                gate("reported-unknown")
                assert guarded() == [blocked]
                assert sql("server", "SELECT run_id FROM issue_inputs WHERE task_id = ?", author_task) == [(None,)]
                mark("allow-resume")
            if two_rounds:
                second = stop_author(2, 1)
                assert second[:4] == first[:4] and second[-1] == first[-1]
                assert second[4:7] != first[4:7]
                if args.mode == "review-unknown":
                    gate("review-resume-rpc")
                    gate("review-resume-disconnected")
                    prepared = task_runs(review_task)[-1]
                    assert prepared[1:3] == ("prepared", None)
                    assert not (root / "review-running-2").exists()
                    assert local_task(author_task) == [("waiting", 0)]
                    processes.close()
                    processes.enter_context(runner(proxy))
                    assert task_runs(review_task)[-1] == prepared
                    mark("allow-review-resume")
                gate("review-running-2")
                # Re-send the already confirmed first result, then send a late
                # duplicate with a different operation ID but the same old Run.
                operation = json.loads((root / "result-1").read_text())
                profile = posted[-1][0]["profile"]
                late = dict(operation, operation_id=str(uuid.uuid4()))
                with closing(HTTPConnection(address, timeout=5)) as conn:
                    payload = {"protocol": 2, "profile": profile, "receipts": [],
                               "records": [{"kind": "operation", "data": op} for op in (operation, late)]}
                    conn.request("POST", "/api/v1/runners/demo-runner/sync", json.dumps(payload),
                                 {"Authorization": f"Bearer {env['TEND_TOKEN']}", "Content-Type": "application/json"})
                    response = conn.getresponse()
                    assert response.status == 200, response.read()
                assert sql("server", "SELECT result_input_id IS NOT NULL FROM tend_reviews ORDER BY rowid") == [(1,), (0,)]
                deliver_review(2)
                review_runs = task_runs(review_task)
                assert len(review_runs) == 2 and all(row[2] for row in review_runs)
                assert (review_runs[0][2] == review_runs[1][2]) == (args.mode != "review-missing")
                assert review_runs[0][0] < task_runs(author_task)[1][0] < review_runs[1][0]
                if args.mode == "review-unknown":
                    assert review_runs[1][3] == prepared[3]
            wait_for(lambda: finished(author) and finished(review), bool)
            assert task_runs(review_task)[-1][1] == "stopped"
            assert task_runs(review_task)[-1][0] < task_runs(author_task)[-1][0]
            if args.mode == "probe":
                wait_for(lambda: finished(probe), bool)
                assert (root / "probe-ran").is_file()
            rounds = 2 if two_rounds else 1
            receipts = sql("server", "SELECT v.candidate, v.scope, v.request_id, json_extract(p.payload, '$.review.candidate'), "
                                     "json_extract(p.payload, '$.review.scope'), json_extract(p.payload, '$.review.request_id'), "
                                     "json_extract(p.payload, '$.result.conclusion'), json_extract(p.payload, '$.source_id'), p.run_id "
                                     "FROM tend_reviews v JOIN issue_inputs p ON p.id = v.result_input_id ORDER BY v.rowid")
            assert len(receipts) == rounds
            for i, row in enumerate(receipts, 1):
                assert row[:3] == row[3:6]
                result = json.loads((root / f"result-{i}").read_text())
                assert row[6:] == (result["action"]["result"]["conclusion"], result["operation_id"], task_runs(author_task)[i][3])
            assert sql("server", "SELECT count(*), count(DISTINCT run_id) FROM issue_inputs "
                                 "WHERE task_id = ? AND json_extract(payload, '$.kind') = 'review_finished'",
                       author_task) == [(rounds, rounds)]
            assert sql("server", "SELECT count(*) FROM tasks WHERE issue_id = ?", review) == [(1,)]
            assert sql("server", "SELECT count(*) FROM runner_assignments WHERE task_id = ?", review_task) == [(1,)]
            first_op = receipts[0][7]
            assert sum(first_op in rep.get("confirmed_operations", []) for _, _, rep in posted) >= 2
            assert sum(first_op in [record.get("data", {}).get("operation_id") for record in req.get("records", [])]
                       for req, _, _ in posted) >= 2
            author_runs = sql("runner", "SELECT count(*), count(DISTINCT r.thread_id) FROM local_runs r "
                                        "JOIN received_assignments a ON a.id = r.assignment_id WHERE a.task_id = ?",
                              author_task)
            assert author_runs == [(rounds + 1, 2 if args.mode in {"missing", "broken", "drift"} else 1)]
            if fallback:
                print("PASS fallback: single slot, original tasks/<author_task_id>, immutable artifact and handoff in both directions")
                return
            grouped = delayed()
            assert len(grouped) == (6 if args.mode == "probe" else 5), grouped
            assert all(task == parent_task and due == deadline for _, task, due in grouped)
            assert len(parent_runs()) == 1
            assert sql("server", "SELECT count(*) FROM issue_inputs WHERE task_id = ? "
                                 "AND json_extract(payload, '$.kind') = 'review_finished'", parent_task) == [(0,)]
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
          f"{author_runs[0][1]} author thread(s) over {author_runs[0][0]} Runs, {rounds} review rounds, 1 parent thread over 2 Runs, "
          f"{len(grouped)} facts in one parent wake")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("server", "cli", "runner"):
        parser.add_argument(executable, type=lambda value: Path(value).resolve())
    parser.add_argument("--mode", choices=("resume", "missing", "broken", "drift", "probe", "active", "fallback", "workspace-missing", "workspace-replaced", "review-missing", "review-unknown"), default="resume")
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
