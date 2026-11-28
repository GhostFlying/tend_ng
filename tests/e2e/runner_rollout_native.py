"""Verify real Runner creation and original-turn inspection through a frozen launcher home."""

import argparse
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import threading

from chat import server, wait_for
from issue_chat import REQUEST, free_port, prepare
from issue_token_usage import runner_process

MODEL = "gpt-5.4"
ANSWER = "Native Runner wrote this synthetic turn."
KEY = "native-runner-response"


@contextmanager
def provider():
    requests = []

    class Responses(BaseHTTPRequestHandler):
        def do_POST(self):
            assert self.path == "/v1/responses"
            assert self.headers.get("Authorization") == "Bearer fixture-only"
            received = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            assert received["model"] == MODEL
            requests.append(received["model"])
            item = {"type": "message", "role": "assistant", "id": "fixture-answer",
                    "status": "completed", "phase": "final_answer",
                    "content": [{"type": "output_text", "text": ANSWER, "annotations": []}]}
            events = [
                {"type": "response.created", "response": {"id": KEY}},
                {"type": "response.output_item.added", "output_index": 0, "item": item},
                {"type": "response.output_item.done", "output_index": 0, "item": item},
                {"type": "response.completed", "response": {"id": KEY, "model": MODEL, "output": [item],
                    "usage": {"input_tokens": 31, "output_tokens": 9, "total_tokens": 40}}},
            ]
            body = "".join(f"data: {json.dumps(event)}\n\n" for event in events).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    with ThreadingHTTPServer(("127.0.0.1", 0), Responses) as listener:
        worker = threading.Thread(target=listener.serve_forever)
        worker.start()
        try:
            yield f"http://127.0.0.1:{listener.server_port}/v1", requests
        finally:
            listener.shutdown()
            worker.join()


def run(args, root, url, requests):
    addr = free_port()
    prepare(root, free_port())
    config = root / "runner.json"
    settings = json.loads(config.read_text())
    home = root / "named-runner-home"
    home.mkdir(mode=0o700)
    (home / "config.toml").write_text(f'''
model = "{MODEL}"
model_provider = "fixture"
[model_providers.fixture]
name = "Local Runner fixture"
base_url = "{url}"
wire_api = "responses"
env_key = "LOCAL_FIXTURE_API_KEY"
''')
    settings["agents"][0].update(model=MODEL, app_server="named")
    settings["app_servers"] = {"named": {"program": str(args.codex), "codex_home": home.name}}
    config.write_text(json.dumps(settings))
    os_home = root / "os-home"
    os_home.mkdir()
    env = {"PATH": os.environ["PATH"], "HOME": str(os_home),
           "LOCAL_FIXTURE_API_KEY": "fixture-only", "TEND_TOKEN": "real-runner-rollout",
           "TEND_URL": f"http://{addr}", "NO_PROXY": "127.0.0.1,localhost",
           "CODEX_HOME": str(root / "router-home")}
    runner_env = {**env, "CODEX_HOME": str(root / "unused-runner-default")}
    runner = [args.runner, "--config", config, "--db", root / "runner.sqlite",
              "--app-server", "/bin/false"]

    def cli(*cmd):
        return json.loads(subprocess.check_output([args.cli, *cmd], env=env, timeout=10))

    with server(args.server, root, env, listen=addr):
        with runner_process(runner, runner_env, root):
            wait_for(lambda: cli("runner", "list"), bool)
            source = cli("chat", "send", "main", REQUEST)["message_id"]
            messages = wait_for(lambda: cli("chat", "read", "main")["messages"],
                                lambda rows: any(row["id"] == source and row.get("issue_ids") for row in rows))
            [issue] = next(row for row in messages if row["id"] == source)["issue_ids"]
            result = wait_for(lambda: cli("issue", "read", issue),
                              lambda row: ((row.get("run") or {}).get("phase") or {}).get("outcome") == "completed")
            run_id = result["run"]["run_id"]
            with sqlite3.connect(root / "runner.sqlite") as db:
                [(thread, turn)] = db.execute("SELECT thread_id,turn_id FROM local_runs WHERE id=?", (run_id,)).fetchall()
            assert thread and turn and requests == [MODEL], requests
            [rollout] = (home / "sessions").rglob(f"rollout-*-{thread}.jsonl")
            with rollout.open() as lines:
                records = [json.loads(line) for line in lines]
            assert records[0]["payload"]["id"] == thread
            assert any(r["type"] == "turn_context" and r["payload"]["turn_id"] == turn for r in records)
            wait_for(lambda: cli("issue", "token-usage", issue),
                     lambda row: row["direct"]["reported_requests"] == 1)

        # The inspect process must use the original launch even after both defaults
        # and the Agent's configured launcher have changed.
        config.write_text(json.dumps({**settings, "app_servers": {
            "named": {"program": "/bin/false", "codex_home": "unused-replacement"}}}))
        changed = {**runner_env, "CODEX_HOME": str(root / "unused-replacement-default")}
        with runner_process(runner, changed, root):
            def inspect():
                output = subprocess.run([args.cli, "run", "rollout", run_id], env=env,
                                        capture_output=True, timeout=8, check=False)
                return json.loads(output.stdout) if not output.returncode else None

            detail = wait_for(inspect, bool)
            assert detail["turn"]["id"] == turn
            assert any(item.get("text") == ANSWER for item in detail["turn"]["items"])
            assert requests == [MODEL], "original-turn lookup caused a model request"

    assert not (os_home / ".codex").exists() and not (root / "unused-runner-default").exists()
    print("PASS real Runner used named home, persisted the original thread/turn and inspected it after launcher/default replacement")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "cli", "runner", "codex", "scratch"):
        parser.add_argument(name, type=lambda path: Path(path).resolve())
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-runner-native-", dir=args.scratch) as directory, provider() as upstream:
        run(args, Path(directory), *upstream)


if __name__ == "__main__":
    main()
