"""Exercise offline delivery, Router publication, and task-scoped restoration."""
import argparse
from contextlib import contextmanager
import http.client
import http.server
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading

from chat import server, wait_for

FIXTURE = Path(__file__).resolve().parents[1] / "fixtures" / "coordination_app_server.py"
CONTENTS = b"A fixture guide.\n" * 32768


@contextmanager
def hold_upload(address):
    release = threading.Event()
    uploads = []

    class Proxy(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def forward(self, hold):
            size = int(self.headers["Content-Length"])
            body = self.rfile.read(size)
            connection = http.client.HTTPConnection(address, timeout=15)
            try:
                connection.putrequest(self.command, self.path)
                for name in ("Authorization", "Content-Type", "Content-Length"):
                    connection.putheader(name, self.headers[name])
                connection.endheaders()
                if hold and not release.is_set():
                    connection.send(body[:size // 2])
                    assert release.wait(14), "test did not release the upload"
                    connection.send(body[size // 2:])
                else:
                    connection.send(body)
                response = connection.getresponse()
                data = response.read()
                if hold:
                    uploads.append((self.path, response.status))
                self.send_response(response.status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
            finally:
                connection.close()

        def do_POST(self):
            self.forward(False)

        def do_PUT(self):
            self.forward(True)

    with http.server.ThreadingHTTPServer(("127.0.0.1", 0), Proxy) as proxy:
        thread = threading.Thread(target=proxy.serve_forever)
        thread.start()
        try:
            yield f"127.0.0.1:{proxy.server_port}", release, uploads
        finally:
            release.set()
            proxy.shutdown()
            thread.join()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("server", "cli", "runner"):
        parser.add_argument(executable, type=lambda value: Path(value).resolve())
    args = parser.parse_args()
    env = {**os.environ, "TEND_TOKEN": "tend-delivery-fixture"}

    with tempfile.TemporaryDirectory(prefix="tend-delivery-") as temporary:
        root = Path(temporary)
        source = root / "source"
        subprocess.run(["git", "init", "--quiet", "-b", "main", source], check=True)
        (source / "README.md").write_text("A fixed repository.\n")
        subprocess.run(["git", "-C", source, "add", "README.md"], check=True)
        subprocess.run(["git", "-C", source, "-c", "user.name=Fixture",
                        "-c", "user.email=fixture@example.invalid", "commit", "--quiet", "-m", "fixture"],
                       check=True)
        subprocess.run(["git", "clone", "--quiet", source, root / "example"], check=True)
        (root / "runner.json").write_text(json.dumps({
            "host_id": "delivery-host", "runner_id": "delivery-runner", "slot_limit": 1,
            "repos": [{"id": "example", "path": "example"}],
            "agents": [
                {"id": "coordinator", "category": "issue-router", "revision": "one",
                 "model": "fixture-coordinator", "reasoning_effort": "low"},
                {"id": "worker", "category": "implementation", "revision": "one",
                 "model": "fixture-worker", "capabilities": ["image_gen"]},
            ],
        }))
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            address = f"127.0.0.1:{reservation.getsockname()[1]}"

        def cli(*words):
            return subprocess.run([args.cli, *words], capture_output=True, timeout=8,
                                  env={**env, "TEND_URL": f"http://{address}"})

        def read(*words):
            result = cli(*words)
            result.check_returncode()
            return json.loads(result.stdout)

        def sql(db, statement, *values):
            with sqlite3.connect(root / f"{db}.sqlite") as connection:
                return connection.execute(statement, values).fetchall()

        @contextmanager
        def runner(target):
            with (root / "runner.stderr").open("a") as stderr:
                process = subprocess.Popen(
                    [args.runner, "--config", root / "runner.json", "--db", root / "runner.sqlite",
                     "--app-server", sys.executable, FIXTURE, root, "delivery"],
                    env={**env, "TEND_URL": f"http://{target}"}, stderr=stderr,
                )
                try:
                    yield
                except BaseException:
                    print((root / "runner.stderr").read_text(), file=sys.stderr, end="")
                    raise
                finally:
                    process.terminate()
                    process.wait(timeout=7)

        (root / "delegate-now").touch()
        recovered = root / "recovered"
        recovered.mkdir()
        with runner(address):
            with server(args.server, root, env, listen=address):
                origin = read("chat", "send", "main", "Dispatch from Chat.")
                messages = wait_for(lambda: read("chat", "read", "main")["messages"], lambda rows: len(rows) == 2)
                [parent] = messages[0]["issue_ids"]
                wait_for(lambda: (root / "delegation.json").is_file(), bool)
                child = json.loads((root / "delegation.json").read_text())["action"]["issue_id"]
                wait_for(lambda: sql("server", "SELECT id FROM issues WHERE id = ?", child), bool)
                before = wait_for(lambda: read("issue", "read", child), lambda view: view["status"] == "running")
                with sqlite3.connect(root / "server.sqlite") as live, sqlite3.connect(recovered / "server.sqlite") as backup:
                    live.backup(backup)
            (root / "deliver-now").touch()
            wait_for(lambda: sql("runner", "SELECT state FROM local_runs ORDER BY rowid"),
                     lambda rows: rows == [("stopped",), ("stopped",)])
            file_id = json.loads((root / "submitted.json").read_text())["operation_id"]
            assert (root / "runner.artifacts" / file_id).read_bytes() == CONTENTS
            original = root / "runner.runs" / "workspaces" / parent / "repos" / "example" / "guide.txt"
            assert original.read_text() == "A later edit to the workspace file.\n"

        with server(args.server, root, env, listen=address), hold_upload(address) as (proxy, release, uploads):
            with runner(proxy):
                wait_for(lambda: list((root / "server.artifacts").glob("*.part")), bool)
                work = wait_for(lambda: read("issue", "read", child), lambda view: view["status"] == "completed")
                assert work["run"]["phase"]["state"] == "stopped"
                assert work["artifacts"] == [{"id": file_id, "name": "guide.txt", "bytes": len(CONTENTS),
                                              "available": False}]
                assert cli("artifact", "download", file_id).returncode != 0
                read("chat", "send", "other", "Hello, Tend.")
                wait_for(lambda: read("chat", "read", "other")["messages"], lambda rows: len(rows) == 2)
                wait_for(lambda: read("issue", "read", parent)["status"], lambda status: status == "completed")
                read("chat", "send", "main", "Hold this turn.")
                wait_for(lambda: sql("app", "SELECT 1 FROM start_requests WHERE input = 'Hold this turn.'"), bool)
                release.set()
                wait_for(lambda: (root / "read-delivery").exists(), bool)
                wait_for(lambda: read("issue", "read", parent)["status"], lambda status: status == "completed")
                wait_for(lambda: sql("server", "SELECT seq FROM router_inputs "
                                               "WHERE report_id IS NOT NULL AND outcome IS NULL"), bool)
                read("chat", "send", "main", "Next input.")
                (root / "app.release").touch()
                answer = "The guide is ready. Download the attached guide.txt."
                page = wait_for(lambda: read("chat", "read", "main")["messages"],
                                lambda rows: any(message["text"] == answer for message in rows))
                [result] = [message for message in page if message["text"] == answer]
                assert result["author"] == "router" and result["reply_to"] == origin["message_id"]
                assert result["artifacts"] == [{**work["artifacts"][0], "available": True}]
                starts = sql("app", "SELECT input FROM start_requests ORDER BY rowid")
                assert starts[-3] == ("Next input.",) and starts[-2] == starts[-1]
                assert starts[-1][0].startswith("Tend Issue report")
                assert len(page) == 7, page
                downloaded = cli("artifact", "download", file_id)
                assert downloaded.returncode == 0 and downloaded.stdout == CONTENTS
                assert uploads == [(f"/api/v1/runners/delivery-runner/artifacts/{file_id}", 200)]
                stored = root / "server.artifacts" / file_id
                stored.unlink()
                assert cli("artifact", "download", file_id).returncode != 0
                wait_for(lambda: read("issue", "read", child)["artifacts"][0]["available"], bool)
                assert cli("artifact", "download", file_id).stdout == CONTENTS
                assert sql("server", "SELECT count(*) FROM issue_inputs WHERE id = ?", file_id) == [(1,)]
                wait_for(lambda: sql("runner", "SELECT 1 FROM run_outbox WHERE acknowledged = 0 LIMIT 1"),
                         lambda rows: not rows)
            assert cli("artifact", "download", file_id).stdout == CONTENTS
        with server(args.server, recovered, env, listen=address):
            assert read("issue", "read", child)["status"] == "running"

            def reconcile():
                subprocess.run(
                    [args.runner, "--config", root / "runner.json", "--db", root / "runner.sqlite",
                     "--reconcile", before["task_id"]],
                    env={**env, "TEND_URL": f"http://{address}"}, check=True, timeout=12,
                )

            reconcile()
            restored = read("issue", "read", child)
            assert restored["status"] == "completed"
            assert restored["run"]["run_id"] == before["run"]["run_id"]
            assert restored["artifacts"] == result["artifacts"]
            assert cli("artifact", "download", file_id).stdout == CONTENTS
            (recovered / "server.artifacts" / file_id).unlink()
            reconcile()
            assert cli("artifact", "download", file_id).stdout == CONTENTS
            with sqlite3.connect(recovered / "server.sqlite") as db:
                assert db.execute("SELECT count(*) FROM issue_inputs WHERE id = ?", [file_id]).fetchone() == (1,)
    print("PASS offline file delivery, one Router report, and restoration by full Task reconciliation")


if __name__ == "__main__":
    main()
