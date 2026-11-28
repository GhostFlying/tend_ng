"""Exercise cancellation of buffered reviews and named review handoff across Runners."""

import argparse
from contextlib import contextmanager
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
from threading import Event, Thread

from chat import server, wait_for
from issue_chat import free_port, prepare

FIXTURE = Path(__file__).resolve().parents[1] / "fixtures/execution_continuity_app_server.py"


@contextmanager
def hold_rereview(address):
    intercepted, release = Event(), Event()

    class Proxy(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def forward(self):
            raw = self.rfile.read(int(self.headers["Content-Length"]))
            request = json.loads(raw) if self.command == "POST" else {}
            records = request.get("records", [])
            round_index = next((i for i, record in enumerate(records)
                                if record.get("data", {}).get("action", {}).get("rereview")), None)
            if round_index is not None and not intercepted.is_set():
                # Runner can batch the previous Reviewer's stop and this
                # request. Commit the prefix, then replay the intact sync.
                status, payload = self.exchange(json.dumps({**request, "records": records[:round_index]}))
                assert status == 200, payload
                intercepted.set()
                assert release.wait(12), "the test did not release the held request"
            status, body = self.exchange(raw)
            try:
                self.send_response(status)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            except ConnectionError:
                self.close_connection = True

        def exchange(self, body):
            conn = HTTPConnection(address, timeout=6)
            try:
                conn.request(self.command, self.path, body, {
                    "Authorization": self.headers["Authorization"],
                    "Content-Type": self.headers["Content-Type"],
                })
                result = conn.getresponse()
                return result.status, result.read()
            finally:
                conn.close()

        do_POST = do_PUT = forward

    with HTTPServer(("127.0.0.1", 0), Proxy) as http:
        thread = Thread(target=http.serve_forever)
        thread.start()
        try:
            yield f"127.0.0.1:{http.server_port}", intercepted, release
        finally:
            release.set()
            http.shutdown()
            thread.join()


def exercise(args, root):
    address = free_port()
    prepare(root, free_port())
    path = root / "runner.json"
    settings = json.loads(path.read_text())
    settings.update(slot_limit=2, workspace_root=str(root / "shared"))
    settings["agents"].append({"id": "reviewer", "category": "review", "revision": "one",
                               "model": "fixture-reviewer"})
    path.write_text(json.dumps(settings))
    (root / "base").write_text(subprocess.check_output(
        ["git", "-C", root / "example", "rev-parse", "HEAD"], text=True).strip())
    env = {**os.environ, "TEND_TOKEN": "review-transition-fixture", "TEND_URL": f"http://{address}",
           "CODEX_HOME": str(root / "fixture-codex")}

    def cli(*words):
        return json.loads(subprocess.check_output([args.cli, *words], env=env, timeout=5))

    def sql(db, query, *values):
        with sqlite3.connect(root / f"{db}.sqlite") as connection:
            return connection.execute(query, values).fetchall()

    def gate(name):
        wait_for((root / name).exists, bool)

    def mark(name):
        (root / name).touch()

    def local(db, task):
        return sql(db, "SELECT execution_state FROM received_assignments WHERE task_id = ?", task)

    def runs(db, task):
        return sql(db, "SELECT r.state, r.thread_id, r.rowid FROM local_runs r "
                       "JOIN received_assignments a ON a.id = r.assignment_id "
                       "WHERE a.task_id = ? ORDER BY r.rowid", task)

    @contextmanager
    def runner(config, db, endpoint, mode):
        with (root / f"{db}.log").open("a") as log:
            process = subprocess.Popen([
                args.runner, "--config", config, "--db", root / f"{db}.sqlite",
                "--app-server", sys.executable, FIXTURE, root, mode,
            ], env={**env, "TEND_URL": f"http://{endpoint}"}, stderr=log)
            try:
                yield process
            finally:
                if process.poll() is None:
                    process.terminate()
                process.wait(timeout=6)

    def first_result():
        cli("chat", "send", "main", "Dispatch from Chat.")
        gate("author-continued-1")
        [ids] = wait_for(lambda: sql(
            "server", "SELECT v.author_task_id, v.review_task_id, v.review_issue_id, i.parent_issue_id "
                      "FROM tend_reviews v JOIN issues i ON i.id = v.author_issue_id"), bool)
        _, _, review_issue, parent = ids
        [(operation, sender, owner_issue)] = sql(
            "server", "SELECT v.request_id, json_extract(a.contract, '$.placement.agent.id'), t.issue_id "
                      "FROM tend_reviews v JOIN runner_operations op ON op.id = v.request_id "
                      "JOIN runner_assignments a ON a.id = json_extract(op.operation, '$.assignment_id') "
                      "JOIN tasks t ON t.id = a.task_id "
                      "WHERE v.review_issue_id = ? AND v.author_issue_id = t.issue_id", review_issue)
        assert cli("issue", "read", review_issue)["parent_issue_id"] == parent != owner_issue
        [dispatch] = [row for row in cli("issue", "messages", review_issue)["messages"] if row["kind"] == "dispatch"]
        assert dispatch["id"] == operation and dispatch["source_message_id"] is None
        assert dispatch["dispatch_provenance"] == {"origin": "issue_delegation", "agent_name": sender}
        assert sender == "worker"
        mark("allow-author-stop-1")
        gate("review-running-1")
        mark("deliver-review-1")
        gate("result-1")
        wait_for(lambda: local("runner", ids[0]), lambda rows: rows == [("queued",)])
        return ids

    with server(args.server, root, env, listen=address):
        if args.scenario.startswith("cancel"):
            with hold_rereview(address) as (proxy, intercepted, release), runner(path, "runner", proxy, "resume"):
                author, reviewer, _, parent_issue = first_result()
                mark("allow-review-stop-1")
                assert intercepted.wait(12), "round two was not buffered at the sync boundary"
                gate("author-continued-2")
                assert sql("server", "SELECT status FROM tasks WHERE id = ?", reviewer) == [("completed",)]
                assert sql("server", "SELECT count(*) FROM tend_reviews") == [(1,)]
                cli("issue", "cancel", parent_issue, "Cancel the entire temporary tree.", "cancel-fixture")
                assert sql("server", "SELECT count(*) FROM task_controls WHERE task_id = ?", reviewer) == [(0,)]
                if args.scenario == "cancel-reopen":
                    cli("issue", "reopen", parent_issue, "Keep the Issue open, not its stopped work.", "reopen-fixture")
                release.set()
                [result] = wait_for(lambda: sql(
                    "server", "SELECT i.payload FROM tend_reviews v JOIN issue_inputs i ON i.id = v.result_input_id "
                              "WHERE v.review_task_id = ? AND v.scope = 'Verify the revised guide'", reviewer), bool)
                payload = json.loads(result[0])
                assert "result" not in payload and "run_id" not in payload, payload
                assert "cancelled, stopping" in payload["summary"], payload
                mark("allow-author-stop-2")
                wait_for(lambda: local("runner", author), lambda rows: rows == [("cancelled",)])
                assert local("runner", reviewer) == [("finished",)] and len(runs("runner", reviewer)) == 1
                assert sql("server", "SELECT count(*) FROM issue_inputs WHERE task_id = ? "
                                     "AND json_extract(payload, '$.kind') = 'review_requested'", reviewer) == [(0,)]
                assert not (root / "review-running-2").exists()
            print(f"PASS {args.scenario}: buffered round settles once without reactivating the completed Reviewer")
            return

        if args.scenario == "artifact-replacement":
            with runner(path, "runner", address, args.scenario) as predecessor:
                cli("chat", "send", "main", "Dispatch from Chat.")
                gate("author-continued-1")
                request = json.loads((root / "request-1").read_text())
                file = request["action"]["candidate"].removeprefix("artifact:")
                reviewer = request["action"]["task_id"]
                wait_for(lambda: local("runner", reviewer), lambda rows: rows == [("queued",)])
                wait_for(lambda: sql("server", "SELECT available FROM artifacts WHERE id = ?", file),
                         lambda rows: rows == [(1,)])
                assert runs("runner", reviewer) == []
                author_cwd = Path((root / "author-cwd").read_text())
                original = root / "shared" / ".tend-host" / settings["host_id"] / "artifacts" / file
                saved_inode = original.stat().st_ino
                assert original.stat().st_size > 32768
                assert original.read_bytes() != (author_cwd / "repos/example/guide.txt").read_bytes()

                new = {**settings, "runner_id": "next-runner", "standby": True}
                new.pop("inspect")
                successor_cfg = root / "next.json"
                successor_cfg.write_text(json.dumps(new))
                with runner(successor_cfg, "next", address, args.scenario):
                    wait_for(lambda: cli("runner", "list"), lambda rows: len(rows) == 2)
                    cli("runner", "replace", "demo-runner", "next-runner")
                    wait_for(lambda: sql("runner", "SELECT successor FROM local_drain"), bool)
                    mark("allow-author-stop-1")
                    gate("review-running-1")
                    assert runs("runner", reviewer) == []
                    assert original.stat().st_ino == saved_inode
                    assert not list(original.parent.glob("*.part"))
                    mark("deliver-review-1")
                    gate("result-1")
                    mark("allow-review-stop-1")
                    wait_for(lambda: sql("server", "SELECT status FROM tasks WHERE id = ?", reviewer),
                             lambda rows: rows == [("completed",)])
                    assert predecessor.wait(timeout=10) == 0
                    cli("runner", "retire", settings["runner_id"])
                    stored = root / "server.artifacts" / file
                    stored.unlink()
                    wait_for(lambda: stored.is_file() and stored.read_bytes() == original.read_bytes(), bool)
                    assert sql("server", "SELECT runner_id, available FROM artifacts WHERE id = ?", file) == [
                        (settings["runner_id"], 1)]
                    assert sql("server", "SELECT count(*) FROM issue_inputs WHERE id = ?", file) == [(1,)]
                [(source_task,)] = sql("server", "SELECT task_id FROM artifacts WHERE id = ?", file)
                stored.unlink()
                subprocess.run([args.runner, "--config", successor_cfg, "--db", root / "next.sqlite",
                                "--reconcile", source_task], env=env, check=True, timeout=12)
                assert stored.read_bytes() == original.read_bytes()
                assert sql("server", "SELECT count(*) FROM issue_inputs WHERE id = ?", file) == [(1,)]
            print("PASS same-host review and incremental/full file restoration after its producer retires")
            return

        queued_on_predecessor = args.scenario == "replacement-queued"
        with runner(path, "runner", address, "replacement") as predecessor:
            author, reviewer, review_issue, _ = first_result()
            if queued_on_predecessor:
                mark("allow-review-stop-1")
                gate("author-continued-2")
                assert local("runner", author) == [("active",)]
                wait_for(lambda: local("runner", reviewer), lambda rows: rows == [("queued",)])
                assert [row[0] for row in runs("runner", reviewer)] == ["stopped"]
            new = {**settings, "runner_id": "next-runner", "standby": True}
            new.pop("inspect")
            successor_cfg = root / "next.json"
            successor_cfg.write_text(json.dumps(new))
            with runner(successor_cfg, "next", address, "replacement"):
                wait_for(lambda: cli("runner", "list"), lambda rows: len(rows) == 2)
                cli("runner", "replace", "demo-runner", "next-runner")
                wait_for(lambda: sql("runner", "SELECT successor FROM local_drain"), bool)
                assert runs("next", author) == runs("next", reviewer) == []
                if queued_on_predecessor:
                    assert not (root / "review-running-2").exists()
                    mark("allow-author-stop-2")
                else:
                    assert local("runner", reviewer) == [("active",)]
                    mark("allow-review-stop-1")
                    gate("author-continued-2")
                    wait_for(lambda: local("next", reviewer), lambda rows: rows == [("queued",)])
                    assert local("next", author) == [("active",)] and runs("next", reviewer) == []
                    assert runs("next", author)[0][1] == runs("runner", author)[0][1]
                    mark("allow-author-stop-2")
                gate("review-running-2")
                assert local("runner", reviewer) == [("handed_off",)]
                assert sql("server", "SELECT json_extract(proof, '$.context.phase') FROM runner_handoff_proofs "
                                     "WHERE predecessor = 'demo-runner' AND json_extract(proof, '$.task_id') = ?",
                           reviewer) == [("queued" if queued_on_predecessor else "finished",)]
                assert sql("server", "SELECT review_task_id, review_issue_id FROM tend_reviews ORDER BY rowid") == [
                    (reviewer, review_issue), (reviewer, review_issue)]
                assert sql("server", "SELECT runner_id, json_extract(contract, '$.placement.agent.id') "
                                     "FROM runner_assignments WHERE task_id = ? ORDER BY rowid", reviewer) == [
                    ("demo-runner", "reviewer"), ("next-runner", "reviewer")]
                if queued_on_predecessor:
                    assert runs("next", author) == []
                    assert runs("runner", author)[-1][0] == "stopped"
                else:
                    assert runs("next", author)[0][0] == "stopped"
                    assert runs("next", author)[0][2] < runs("next", reviewer)[0][2]
                assert runs("runner", reviewer)[0][1] != runs("next", reviewer)[0][1]
                assert sql("next", "SELECT json_extract(contract, '$.handoff.native_thread') "
                                   "FROM received_assignments WHERE task_id = ?", reviewer) == [(None,)]
                mark("deliver-review-2")
                gate("result-2")
                wait_for(lambda: local("next", author), lambda rows: rows == [("queued",)])
                assert local("next", reviewer) == [("active",)]
                assert len(runs("next", author)) == (0 if queued_on_predecessor else 1)
                mark("allow-review-stop-2")
                for task in (author, reviewer):
                    wait_for(lambda: sql("server", "SELECT status FROM tasks WHERE id = ?", task),
                             lambda rows: rows == [("completed",)])
                assert runs("next", reviewer)[0][0] == "stopped"
                assert runs("next", reviewer)[0][2] < runs("next", author)[-1][2]
                assert runs("next", author)[-1][1] == runs("runner", author)[-1][1]
                assert predecessor.wait(timeout=10) == 0
                assert sql("server", "SELECT i.task_id, json_extract(i.payload, '$.result.conclusion') "
                                     "FROM tend_reviews v JOIN issue_inputs i ON i.id = v.result_input_id ORDER BY v.rowid") == [
                    (author, "Request a revision"), (author, "Accepted")]
    print(f"PASS {args.scenario}: same Review Issue, Task and named owner, with a new thread and durable history on the proven successor")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for binary in ("server", "cli", "runner"):
        parser.add_argument(binary, type=lambda value: Path(value).resolve())
    parser.add_argument("--scenario", choices=("cancel", "cancel-reopen", "replacement", "replacement-queued", "artifact-replacement"), required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-review-transition-") as directory:
        root = Path(directory)
        try:
            exercise(args, root)
        except BaseException:
            for path in root.glob("*.log"):
                print(path.read_text(), file=sys.stderr)
            raise


if __name__ == "__main__":
    main()
