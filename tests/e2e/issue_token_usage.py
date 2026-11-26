"""Collect native usage through real Server and Runner processes with replay and optional Web QA."""
import argparse
from contextlib import contextmanager
import http.client
import http.server
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import threading
import urllib.error
import urllib.request

from chat import lose_first_response, server, wait_for
from issue_chat import TESTS, WEB, free_port, prepare


@contextmanager
def runner_process(command, env, root):
    with (root / "runner.stderr").open("w") as stderr:
        process = subprocess.Popen(command, env=env, stderr=stderr)
        try:
            yield
        except Exception:
            print((root / "runner.stderr").read_text(), file=sys.stderr)
            raise
        finally:
            process.terminate()
            process.wait(timeout=7)


@contextmanager
def legacy_to_current_reply(address, read_state):
    """Omit the feature bit on the first reconcile reply, then switch to v38."""
    seen = []

    class Gateway(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            data = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            sent_drained = data.get("drained", False)
            if len(seen) == 1:
                # Simulate an old/buggy client as well, so only checking the
                # new Runner flag cannot mask missing Server-side fencing.
                data["drained"] = True
            with_conn = http.client.HTTPConnection(address, timeout=5)
            try:
                with_conn.request("POST", self.path, json.dumps(data), {
                    "Authorization": self.headers["Authorization"], "Content-Type": "application/json",
                })
                response = with_conn.getresponse()
                reply = json.load(response)
                size = len(data.get("token_usage", []))
                seen.append((self.path, response.status, sent_drained, size, read_state()))
                if len(seen) == 1:
                    reply.pop("token_usage_supported", None)
                body = json.dumps(reply).encode()
                self.send_response(response.status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            finally:
                with_conn.close()

    with http.server.HTTPServer(("127.0.0.1", 0), Gateway) as proxy:
        worker = threading.Thread(target=proxy.serve_forever)
        worker.start()
        try:
            yield f"127.0.0.1:{proxy.server_port}", seen
        finally:
            proxy.shutdown()
            worker.join()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("server", "cli", "runner"):
        parser.add_argument(executable, type=lambda value: Path(value).resolve())
    parser.add_argument("--web", action="store_true")
    args = parser.parse_args()
    test_tree(args)
    test_stop(args)


def test_tree(args):
    with tempfile.TemporaryDirectory(prefix="tend-issue-token-usage-") as temporary:
        root = Path(temporary)
        address = free_port()
        prepare(root, free_port())
        config = root / "runner.json"
        settings = json.loads(config.read_text())
        settings["slot_limit"] = 3
        config.write_text(json.dumps(settings))
        token = "isolated-token-usage-fixture"
        env = {**os.environ, "TEND_TOKEN": token, "TEND_URL": f"http://{address}",
               "CODEX_HOME": str(root / "codex-home")}
        command = [args.runner, "--config", config, "--db", root / "runner.sqlite"]

        def cli(*words):
            return json.loads(subprocess.check_output([args.cli, *words], env=env, timeout=10))

        def usage(issue):
            return cli("issue", "token-usage", issue)

        def sql(db, statement, *params):
            with sqlite3.connect(root / f"{db}.sqlite") as conn:
                return conn.execute(statement, params).fetchall()

        def sync(*args, target=address, succeeds=True):
            result = subprocess.run([*command, *args], env={**env, "TEND_URL": f"http://{target}"},
                                    text=True, capture_output=True, timeout=10)
            assert (result.returncode == 0) == succeeds, result.stderr

        def get(path, expected=200):
            try:
                request = urllib.request.Request(env["TEND_URL"] + path, headers={"Authorization": f"Bearer {token}"})
                with urllib.request.urlopen(request, timeout=5) as response:
                    assert response.status == expected
                    return json.load(response)
            except urllib.error.HTTPError as error:
                assert error.code == expected, error
                return json.load(error)

        native = TESTS / "fixtures/token_usage_app_server.py"
        with runner_process([*command, "--app-server", sys.executable, native, root], env, root):
            with server(args.server, root, env, listen=address):
                cli("chat", "send", "main", "Dispatch from Chat.")
                timeline = wait_for(lambda: cli("chat", "read", "main")["messages"], lambda rows: len(rows) == 2)
                [parent] = timeline[0]["issue_ids"]
                original = wait_for(lambda: cli("issue", "read", parent), lambda issue: issue["status"] == "running")
                wait_for(lambda: (root / "notice-consumed").exists(), bool)
                assert usage(parent)["direct"]["reported_requests"] == 0
                (root / "append-evidence").touch()
                noticed = wait_for(lambda: usage(parent)["direct"], lambda direct: direct["reported_requests"] == 1)
                assert noticed["total_tokens"]["tokens"] == "24800", noticed
                assert cli("issue", "read", parent)["status"] == "running"
                assert sql("runner", "SELECT response_id FROM local_token_usage") == [("parent-first",)]
                steer_id = "usage-live-steer"
                cli("run", "steer", original["run"]["run_id"], "Retain live usage.", steer_id)
                wait_for(lambda: cli("control", "read", steer_id), lambda receipt: receipt["state"] == "delivered")
                wait_for(lambda: (root / "metered-active").exists(), bool)
                missing, metered = [json.loads((root / f"{key}.json").read_text())["action"]["issue_id"]
                                    for key in ("missing", "metered")]
                snapshot = wait_for(lambda: usage(parent), lambda u: u["subtree"]["started_runs"] == 3
                                    and u["direct"]["reported_requests"] == 2)
                assert snapshot["direct"]["total_tokens"]["tokens"] == "41200", snapshot
                assert snapshot["subtree"]["observed_runs"] == 1, snapshot
                task = cli("issue", "read", metered)["task_id"]
                wait_for(lambda: cli("issue", "read", missing)["status"], lambda status: status == "completed")
                cli("chat", "send", "main", "先记下稍后整理发布说明。")
                timeline = wait_for(lambda: cli("chat", "read", "main")["messages"], lambda rows: len(rows) >= 4)
                [empty] = next(row["issue_ids"] for row in timeline if row["text"] == "先记下稍后整理发布说明。")

            # The Runner finishes with no Server process. Its saved usage is unacknowledged.
            (root / "release-usage").touch()
            unconfirmed = wait_for(lambda: sql("runner", "SELECT id FROM local_token_usage WHERE acknowledged=0 ORDER BY rowid"),
                                   lambda rows: len(rows) == 2)
            wait_for(lambda: sql("runner", "SELECT state FROM local_runs WHERE state!='stopped'"), lambda rows: not rows)

        with server(args.server, root, env, listen=address, web_dir=WEB / "dist" if args.web else None):
            with lose_first_response(address, lambda reply: bool(reply.get("confirmed_token_usage"))) as (proxy, sent):
                sync("--once", target=proxy, succeeds=False)
                assert sent[0][1] == 200
                assert sent[0][2]["confirmed_token_usage"] == [row[0] for row in unconfirmed]
            assert sql("runner", "SELECT id FROM local_token_usage WHERE acknowledged=0 ORDER BY rowid") == unconfirmed
            after_loss = usage(parent)
            sync("--once")
            for _ in range(2):
                sync("--reconcile", task)
            assert sql("runner", "SELECT count(*) FROM local_token_usage WHERE acknowledged=0") == [(0,)]
            assert sql("runner", "SELECT count(*) FROM local_token_usage") == [(4,)]
            assert sql("server", "SELECT count(*) FROM token_requests") == [(4,)]
            assert usage(parent) == after_loss

            subtree = after_loss["subtree"]
            assert subtree["total_tokens"] == {"tokens": "127200", "reported_requests": 4}, subtree
            assert subtree["cache_write_input_tokens"] == {"tokens": None, "reported_requests": 0}
            assert after_loss["direct"]["reasoning_output_tokens"] == {"tokens": "0", "reported_requests": 2}
            assert subtree["cached_input_tokens"] == {"tokens": "32288", "reported_requests": 3}
            assert (subtree["started_runs"], subtree["observed_runs"], subtree["unobserved_no_usage"]) == (3, 2, 1)
            nodes = {node["issue_id"]: node for node in after_loss["nodes"]}
            assert set(nodes) == {parent, metered, missing}
            assert nodes[metered]["direct"]["total_tokens"]["tokens"] == "86000"
            assert usage(metered)["subtree"] == nodes[metered]["direct"]
            assert nodes[missing]["direct"]["total_tokens"]["tokens"] is None
            assert usage(empty)["direct"]["started_runs"] == 0
            number = nodes[parent]["number"]
            assert get(f"/api/v1/issues/{number}/token-usage") == after_loss
            assert cli("issue", "token-usage", f"#{number}") == after_loss
            get("/api/v1/issues/missing-fixture/token-usage", 404)

            if args.web:
                [empty_node] = usage(empty)["nodes"]
                subprocess.run(["node", "tests/issue-token-usage.mjs", str(number),
                                str(nodes[missing]["number"]), str(empty_node["number"])],
                               cwd=WEB, env=env, check=True, timeout=80)

            # Prepare the settled predecessor boundary directly; this section
            # isolates drain receipts, not handoff execution or collection.
            sql("runner", "INSERT INTO local_drain VALUES (1, 'demo-successor')")
            sql("runner", "UPDATE received_assignments SET execution_state='handed_off'")
            with sqlite3.connect(root / "runner.sqlite") as db:
                for i in range(33):
                    db.execute(
                        "INSERT INTO local_token_usage (id,run_id,assignment_id,thread_id,turn_id,response_id,last) "
                        "SELECT ?,run_id,assignment_id,thread_id,turn_id,?,last FROM local_token_usage "
                        "WHERE response_id='worker-first'", (f"usage-drain-event-{i}", f"usage-drain-response-{i}"),
                    )
            sql("server", "INSERT INTO runners (id,host_id,profile,last_seen,protocol) "
                "SELECT 'demo-successor',host_id,json_set(profile,'$.runner_id','demo-successor'),0,protocol "
                "FROM runners WHERE id='demo-runner'")
            sql("server", "INSERT INTO runner_replacements (predecessor,successor) VALUES ('demo-runner','demo-successor')")
            sql("server", "UPDATE tasks SET status='completed' WHERE status='waiting'")

            def drain_state():
                [(drained, count)] = sql("server", "SELECT drained_at, (SELECT count(*) FROM token_requests) "
                                        "FROM runner_replacements WHERE predecessor='demo-runner'")
                return drained, count

            with legacy_to_current_reply(address, drain_state) as (proxy, pages):
                sync("--reconcile", task, target=proxy)
                assert all(path.endswith(f"/reconcile/{task}") and status == 200 and not sent and state[0] is None
                           for path, status, sent, _, state in pages), pages
                assert [size for _, _, _, size, _ in pages if size] == [32, 3], pages
            assert drain_state() == (None, 37), pages
            sync("--once")
            drained, count = drain_state()
            assert drained is not None and count == 37, (drained, count)
    print("PASS live delayed evidence, synthetic snapshots excluded, usage during Steer, offline persistence, lost response, restart and paged reconcile with runner-wide drain fencing")


def test_stop(args):
    with tempfile.TemporaryDirectory(prefix="tend-issue-token-usage-stop-") as temporary:
        root = Path(temporary)
        address = free_port()
        prepare(root, free_port())
        env = {**os.environ, "TEND_TOKEN": "isolated-token-usage-stop", "TEND_URL": f"http://{address}",
               "CODEX_HOME": str(root / "codex-home")}

        def cli(*words):
            return json.loads(subprocess.check_output([args.cli, *words], env=env, timeout=10))

        native = TESTS / "fixtures/token_usage_app_server.py"
        command = [args.runner, "--config", root / "runner.json", "--db", root / "runner.sqlite",
                   "--app-server", sys.executable, native, root, "stop"]
        with server(args.server, root, env, listen=address), runner_process(command, env, root):
            cli("chat", "send", "main", "Dispatch from Chat.")
            timeline = wait_for(lambda: cli("chat", "read", "main")["messages"], lambda rows: len(rows) == 2)
            [issue] = timeline[0]["issue_ids"]
            before = wait_for(lambda: cli("issue", "token-usage", issue)["direct"],
                              lambda usage: usage["reported_requests"] == 1)
            assert before["total_tokens"] == {"tokens": "10", "reported_requests": 1}, before
            original = cli("issue", "read", issue)
            assert original["status"] == "running"
            stop = cli("task", "stop", original["task_id"], "stop-with-buffered-usage")
            wait_for(lambda: cli("control", "read", stop["operation_id"]),
                     lambda control: control["state"] == "stopped")
            after = wait_for(lambda: cli("issue", "token-usage", issue)["direct"],
                             lambda usage: usage["reported_requests"] == 2)
            assert after["total_tokens"] == {"tokens": "30", "reported_requests": 2}, after
            final = cli("issue", "read", issue)
            assert final["run"]["run_id"] == original["run"]["run_id"]
            assert final["run"]["phase"]["outcome"] == "cancelled"
            with sqlite3.connect(root / "runner.sqlite") as db:
                responses = db.execute("SELECT response_id FROM local_token_usage ORDER BY rowid").fetchall()
            assert responses == [("before-stop",), ("during-interrupt",)], responses
    print("PASS Stop preserves already-buffered verified usage with no waiting for a terminal notification")


if __name__ == "__main__":
    main()
