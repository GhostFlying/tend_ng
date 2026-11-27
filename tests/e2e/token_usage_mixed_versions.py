"""Same-key live ingestion with an old Runner, and exact paging across its replacement."""
import argparse
from contextlib import contextmanager
import hashlib
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import threading

from chat import server, wait_for
from issue_chat import TESTS, free_port, prepare
from issue_token_usage import runner_process


@contextmanager
def first_page_only(address):
    class Gate(BaseHTTPRequestHandler):
        def do_POST(self):
            body = self.rfile.read(int(self.headers["Content-Length"]))
            page = (json.loads(body).get("token_backfill") or {}).get("page")
            if page and page["offset"] >= 32:
                self.send_error(503, "fixture only allows the first historical page")
                return
            with_conn = http.client.HTTPConnection(address, timeout=6)
            try:
                with_conn.request("POST", self.path, body, {
                    "Authorization": self.headers["Authorization"], "Content-Type": "application/json",
                })
                response = with_conn.getresponse()
                reply = response.read()
                self.send_response(response.status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", len(reply))
                self.end_headers()
                self.wfile.write(reply)
            finally:
                with_conn.close()

        def log_message(self, *_):
            pass

    with ThreadingHTTPServer(("127.0.0.1", 0), Gate) as listener:
        worker = threading.Thread(target=listener.serve_forever)
        worker.start()
        try:
            yield f"http://127.0.0.1:{listener.server_port}"
        finally:
            listener.shutdown()
            worker.join()


def run(args, root, old_server):
    prepare(root, free_port())
    address = free_port()
    home = root / "codex"
    home.mkdir()
    env = {**os.environ, "TEND_TOKEN": "mixed-model-usage-fixture",
           "TEND_URL": f"http://{address}", "CODEX_HOME": str(home)}
    flags = ["--config", root / "runner.json", "--db", root / "runner.sqlite"]
    launcher = ["--app-server", sys.executable, TESTS / "fixtures/token_usage_legacy_paging_app_server.py"]

    def cli(*words):
        reply = subprocess.run([args.cli, *words], env=env, text=True, capture_output=True, timeout=10)
        assert reply.returncode == 0, reply.stderr
        return json.loads(reply.stdout)

    def sql(statement, *parameters):
        with sqlite3.connect(root / "server.sqlite", timeout=5) as db:
            return db.execute(statement, parameters).fetchall()

    def usage(issue):
        return cli("issue", "token-usage", issue)["direct"]

    def assert_unknown(summary):
        for field in ("cached_input_tokens", "cache_write_input_tokens", "reasoning_output_tokens"):
            assert summary[field] == {"tokens": None, "reported_requests": 0}, summary

    def start():
        msg = cli("chat", "send", "main", "Dispatch from Chat.")["message_id"]
        timeline = wait_for(lambda: cli("chat", "read", "main")["messages"],
                            lambda rows: any(r["id"] == msg and r.get("issue_ids") for r in rows))
        [issue] = next(r["issue_ids"] for r in timeline if r["id"] == msg)
        summary = wait_for(lambda: usage(issue), lambda s: s["reported_requests"] == 1)
        wait_for(lambda: sql("SELECT id FROM task_runs WHERE json_extract(observation, '$.phase.state')='stopped'"), bool)
        [(run, task)] = sql("SELECT r.run_id,a.task_id FROM token_runs r JOIN runner_assignments a ON a.id=r.assignment_id")
        return issue, run, task, summary

    if not old_server:
        with server(args.server, root, env, listen=address):
            with runner_process([args.legacy_runner, *flags, *launcher], env, root):
                issue, run, task, first = start()
                assert first["total_tokens"]["tokens"] == "5"
                assert_unknown(first)
                cli("token-usage", "backfill", "--run", run, "--job", "old-root-current-server")
                state = wait_for(lambda: cli("token-usage", "status", "old-root-current-server"),
                                 lambda row: row["state"].startswith("finished"))
                assert state["runs"][0]["gap"] == "child_reader_unsupported", state
                assert state["runs"][0]["native_keys"] == 70, state
            subprocess.run([args.legacy_runner, *flags, "--reconcile", task], env=env, check=True, timeout=10)
            after = usage(issue)
            assert after["reported_requests"] == 70 and after["total_tokens"]["tokens"] == "350", after
            assert_unknown(after)
            cli("token-usage", "rebuild", "--all")
            assert usage(issue) == after
        print("PASS actual legacy Runner realtime, historical-key replay and rebuild retain unknown optional details")
        return

    with server(args.legacy_server, root, env, listen=address), first_page_only(address) as proxy:
        with runner_process([args.legacy_runner, *flags, *launcher], {**env, "TEND_URL": proxy}, root):
            issue, run, _, _ = start()
            cli("token-usage", "backfill", "--run", run, "--job", "continue-original")
            old = wait_for(lambda: cli("token-usage", "status", "continue-original"),
                           lambda result: result["runs"][0]["native_keys"] == 32)
            assert old["runs"][0]["state"] == "pending", old
            [(digest,)] = sql("SELECT snapshot FROM token_backfill_items WHERE job_id='continue-original'")
            [original] = list(home.rglob("rollout-*.jsonl"))
            original_hash = hashlib.sha256(original.read_bytes()).hexdigest()
        with runner_process([args.runner, *flags, *launcher], env, root):
            done = wait_for(lambda: cli("token-usage", "status", "continue-original"),
                            lambda result: result["state"].startswith("finished"))
            [row] = done["runs"]
            assert row["state"] == "scanned" and row["native_keys"] == 70, done
            assert sql("SELECT snapshot FROM token_backfill_items WHERE job_id='continue-original'") == [(digest,)]
            assert hashlib.sha256(original.read_bytes()).hexdigest() == original_hash
            total = usage(issue)
            assert total["reported_requests"] == 70 and total["total_tokens"]["tokens"] == "350", total
    print("PASS actual legacy Server resumes the same 32→70-key job across Runner replacement with identical digest")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "legacy_server", "cli", "runner", "legacy_runner"):
        parser.add_argument(name, type=lambda path: Path(path).resolve())
    args = parser.parse_args()
    for old_server in (False, True):
        with tempfile.TemporaryDirectory(prefix="tend-meter-mixed-") as path:
            run(args, Path(path), old_server)


if __name__ == "__main__":
    main()
