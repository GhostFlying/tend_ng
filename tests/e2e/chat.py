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
def server(binary, root, env):
    process = subprocess.Popen(
        [
            binary, "--db", root / "server.sqlite", "--listen", "127.0.0.1:0",
            "--router-cwd", root, "--app-server", sys.executable,
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
            _, stderr = process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            _, stderr = process.communicate(timeout=5)
        if stderr:
            print(stderr, file=sys.stderr, end="")


@contextmanager
def lose_first_response(upstream):
    posted = []

    class Proxy(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            request = self.rfile.read(int(self.headers["Content-Length"]))
            with_connection = http.client.HTTPConnection(upstream, timeout=5)
            try:
                with_connection.request("POST", self.path, request, {
                    "Authorization": self.headers["Authorization"],
                    "Content-Type": "application/json",
                })
                response = with_connection.getresponse()
                body = response.read()
                posted.append((json.loads(request), response.status, json.loads(body)))
                if len(posted) == 1:
                    self.close_connection = True
                    return
                self.send_response(response.status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
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

        def cli(address, *words, accepts=True):
            result = subprocess.run(
                [args.cli, "chat", *words], capture_output=True, text=True,
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

        with sqlite3.connect(root / "app.sqlite") as database:
            distinct_threads = database.execute("SELECT COUNT(*) FROM threads").fetchone()[0]
            turns = database.execute("SELECT COUNT(*) FROM turns").fetchone()[0]
            assert (distinct_threads, turns) == (3, 5), (distinct_threads, turns)
    print("PASS durable Chat receipt, replay, restart, native history, and per-Chat ordering")


if __name__ == "__main__":
    main()
