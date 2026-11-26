"""Exercise the real Tend Server, CLI, and a fixed local App Server."""
import argparse
from contextlib import contextmanager
import http.client
import http.server
import json
import os
from pathlib import Path
import selectors
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time

FIXTURE = Path(__file__).resolve().parents[1] / "fixtures" / "chat_app_server.py"


@contextmanager
def server(binary, root, env, listen="127.0.0.1:0", web_dir=None, github_cli=None, web_origin=None, discussion_config=None):
    origin = ["--web-origin", web_origin] if web_origin is not None else []
    web = ["--web-dir", web_dir] if web_dir is not None else []
    provider = ["--github-cli", github_cli] if github_cli is not None else []
    discussion = ["--discussion-config", discussion_config] if discussion_config else []
    process = subprocess.Popen(
        [
            binary, "--db", root / "server.sqlite", "--listen", listen,
            "--router-cwd", root, *web, *provider, *origin, *discussion, "--app-server", sys.executable,
            FIXTURE, root / "app.sqlite",
        ],
        stdin=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, env=env,
    )
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stderr, selectors.EVENT_READ)
            assert selector.select(10), "Server did not report a listening address"
            line = process.stderr.readline().strip()
        assert line.startswith("Tend Server listening on "), line
        yield line.rsplit(" ", 1)[1]
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
        # A held App Server can outlive the Server under test.
        os.set_blocking(process.stderr.fileno(), False)
        raw = process.stderr.buffer.read()
        process.stderr.close()
        stderr = raw.decode(errors="replace") if raw else ""
        if stderr:
            print(stderr, file=sys.stderr, end="")


@contextmanager
def lose_first_response(upstream, drop_when=lambda _reply: True):
    posted = []
    dropped = []

    class Proxy(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            self.forward(drop=True)

        def do_PUT(self):
            self.forward(drop=False)

        def forward(self, drop):
            request = self.rfile.read(int(self.headers["Content-Length"]))
            with_connection = http.client.HTTPConnection(upstream, timeout=5)
            try:
                with_connection.request(self.command, self.path, request, {
                    "Authorization": self.headers["Authorization"],
                    "Content-Type": self.headers["Content-Type"],
                })
                response = with_connection.getresponse()
                body = response.read()
                if drop:
                    reply = json.loads(body)
                    posted.append((json.loads(request), response.status, reply))
                    if not dropped and drop_when(reply):
                        dropped.append(reply)
                        self.close_connection = True
                        return
                self.send_response(response.status)
                self.send_header("Content-Type", response.getheader("Content-Type", "application/octet-stream"))
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            except (ConnectionError, TimeoutError):
                self.close_connection = True
            finally:
                with_connection.close()

    with http.server.HTTPServer(("127.0.0.1", 0), Proxy) as proxy:
        thread = threading.Thread(target=proxy.serve_forever)
        thread.start()
        try:
            yield f"127.0.0.1:{proxy.server_port}", posted
        finally:
            proxy.shutdown()
            thread.join()


def wait_for(read, predicate):
    deadline = time.monotonic() + 12
    while time.monotonic() < deadline:
        result = read()
        if predicate(result):
            return result
        time.sleep(0.05)
    raise AssertionError(f"expected messages did not arrive: {result}")


def check_answer(messages, receipt, user_text, answer):
    assert len(messages) == 2, messages
    user, router = messages
    assert user["id"] == receipt["message_id"]
    assert user["author"] == "user" and user["text"] == user_text
    assert user["router_state"] == "completed", user
    assert router["author"] == "router" and router["text"] == answer, router
    assert router["reply_to"] == user["id"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server", type=lambda value: Path(value).resolve())
    parser.add_argument("cli", type=lambda value: Path(value).resolve())
    args = parser.parse_args()

    env = {**os.environ, "TEND_TOKEN": "tend-chat-e2e"}
    with tempfile.TemporaryDirectory(prefix="tend-chat-") as temp:
        root = Path(temp)

        def cli(address, *words, accepts=True, namespace="chat"):
            result = subprocess.run(
                [args.cli, namespace, *words], capture_output=True, text=True,
                env={**env, "TEND_URL": f"http://{address}"}, timeout=14,
            )
            if accepts:
                assert result.returncode == 0, result.stderr
                return json.loads(result.stdout)
            assert result.returncode != 0
            return result.stderr

        def messages(address, chat):
            return cli(address, "read", chat)["messages"]

        with server(args.server, root, env) as address:
            with lose_first_response(address) as (proxy, posted):
                receipt = cli(proxy, "send", "main", "Hello, Tend.")
                assert len(posted) == 2, posted
                assert posted[0] == posted[1]
                request, status, durable_receipt = posted[0]
                assert status == 200 and durable_receipt == receipt
                operation = request["operation_id"]
            first = wait_for(lambda: messages(address, "main"), lambda rows: len(rows) == 2)
            check_answer(first, receipt, "Hello, Tend.", "Hello from the Router.")
            error = cli(address, "send", "main", "Different text", operation, accepts=False)
            assert "HTTP 409" in error and operation in error
            recovered_receipt = cli(address, "send", "recover", "Recover this reply.")
            recovered = wait_for(
                lambda: messages(address, "recover"), lambda rows: len(rows) == 2
            )
            check_answer(
                recovered, recovered_receipt, "Recover this reply.", "This is the original reply."
            )

        with server(args.server, root, env) as address:
            assert cli(address, "send", "main", "Hello, Tend.", operation) == receipt
            assert messages(address, "main") == first
            assert messages(address, "recover") == recovered
            hold = cli(address, "send", "main", "Hold this turn.")
            wait_for(
                lambda: messages(address, "main"),
                lambda rows: rows[-1]["router_state"] == "awaiting_turn",
            )
            following = cli(address, "send", "main", "Next input.")
            elsewhere = cli(address, "send", "elsewhere", "Another chat.")
            parallel = wait_for(
                lambda: messages(address, "elsewhere"), lambda rows: len(rows) == 2
            )
            check_answer(parallel, elsewhere, "Another chat.", "The other chat can continue.")
            main = messages(address, "main")
            assert main[-1]["id"] == following["message_id"]
            assert main[-1]["router_state"] == "queued"
            (root / "app.release").touch()
            finished = wait_for(
                lambda: messages(address, "main"), lambda rows: len(rows) == 6
            )
            assert finished[-2]["reply_to"] == hold["message_id"]
            assert finished[-1]["reply_to"] == following["message_id"]
            assert finished[-1]["text"] == "The next input was consumed."

            replay = cli(address, "send", "main", "Replay once.", "replay-operation")
            replied = wait_for(
                lambda: messages(address, "main"),
                lambda rows: rows[-1]["text"] == "The second attempt answered.",
            )
            check_answer(replied[-2:], replay, "Replay once.", "The second attempt answered.")
            assert cli(address, "send", "main", "Replay once.", "replay-operation") == replay

            lost = cli(address, "send", "main", "Exhaust attempts.")
            after = cli(address, "send", "main", "Keep going.")
            continued = wait_for(
                lambda: messages(address, "main"),
                lambda rows: rows[-1]["reply_to"] == after["message_id"],
            )
            check_answer(continued[-2:], after, "Keep going.", "The queue continued.")
            lost_message = next(message for message in continued if message["id"] == lost["message_id"])
            assert lost_message["text"] == "Exhaust attempts."
            assert lost_message["router_state"] == "failed"
            assert "two attempts" in lost_message["router_error"]

            (root / "app.release").unlink()
            in_flight = cli(address, "send", "main", "Hold across restart.")
            wait_for(
                lambda: messages(address, "main"),
                lambda rows: rows[-1]["router_state"] == "awaiting_turn",
            )
            # Wait until the fixture persists the original, held native turn.
            with sqlite3.connect(root / "app.sqlite") as database:
                wait_for(
                    lambda: database.execute(
                        "SELECT count(*) FROM start_requests WHERE input = 'Hold across restart.'"
                    ).fetchone()[0],
                    lambda count: count == 1,
                )

        with server(args.server, root, env) as address:
            time.sleep(0.5)
            pending = messages(address, "main")[-1]
            assert pending["id"] == in_flight["message_id"]
            assert pending["router_state"] == "awaiting_turn"
            (root / "app.release").touch()
            restarted = wait_for(
                lambda: messages(address, "main"),
                lambda rows: rows[-1]["reply_to"] == in_flight["message_id"],
            )
            check_answer(
                restarted[-2:], in_flight, "Hold across restart.", "The original turn finished."
            )

            for text in ("Dispatch from Chat.", "Recover dispatched work."):
                receipt = cli(address, "send", "dispatch", text)
                dispatched = wait_for(
                    lambda: messages(address, "dispatch"),
                    lambda rows: rows[-1]["reply_to"] == receipt["message_id"],
                )
                issue_id = dispatched[-1]["text"].split()[1]
                issues = dispatched[-2]["issue_ids"]
                numbers = dispatched[-2]["issue_numbers"]
                assert len(issues) == len(numbers)
                assert issues[0] == issue_id, dispatched[-2]
                readback = cli(address, "read", issue_id, namespace="issue")
                assert readback["source_message_id"] == receipt["message_id"], readback
                assert readback["short_number"] == numbers[0]
                assert cli(address, "read", f"#{numbers[0]}", namespace="issue") == readback
                assert readback["status"] == "awaiting_assignment"
                assert readback["category"] == "issue-router"
                assert readback["repo_ids"] == ["example"]
                assert readback["required_capabilities"] == ["image_gen"]
                if text == "Recover dispatched work.":
                    assert len(issues) == 2, issues
                    remaining = cli(address, "read", issues[1], namespace="issue")
                    assert remaining["source_message_id"] == receipt["message_id"]
                    assert remaining["title"] == "Prepare release notes"
                    assert remaining["short_number"] == numbers[1] == numbers[0] + 1
                else:
                    assert issues == [issue_id], issues

        with sqlite3.connect(root / "app.sqlite") as database:
            turns = database.execute("SELECT COUNT(*) FROM turns").fetchone()[0]
            assert turns == 10, turns
            replay_attempts = database.execute(
                "SELECT thread_id, context FROM start_requests WHERE input = 'Replay once.' ORDER BY rowid"
            ).fetchall()
            assert len(replay_attempts) == 2
            assert replay_attempts[0][0] != replay_attempts[1][0]
            assert "The held turn finished." in replay_attempts[1][1]
            assert database.execute(
                "SELECT COUNT(*) FROM start_requests WHERE input = 'Hold across restart.'"
            ).fetchone()[0] == 1
        with sqlite3.connect(root / "server.sqlite") as database:
            attempts = database.execute(
                "SELECT a.number, a.id, a.thread_id FROM router_attempts a "
                "JOIN router_inputs i ON i.seq = a.seq "
                "JOIN chat_messages m ON m.seq = i.message_seq WHERE m.id = ? AND i.report_id IS NULL "
                "ORDER BY a.number",
                (lost["message_id"],),
            ).fetchall()
            assert [attempt[0] for attempt in attempts] == [1, 2], attempts
            assert attempts[0][1:] != attempts[1][1:], attempts
            for table in ("issues", "tasks", "router_dispatches", "issue_short_numbers"):
                assert database.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0] == 3
    print("PASS durable Chat receipts, bounded retries, and idempotent Router dispatch")


if __name__ == "__main__":
    main()
