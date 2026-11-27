"""Exercise real Codex App Server through Tend's resumed Router and ephemeral Feishu entry points."""
import argparse
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time

from chat import server, wait_for
from issue_chat import TESTS, free_port

MODEL = "gpt-5.4"
ASSESSED = "isolated assessment result"
PROMPTS = ("Save a synthetic note.", "Repeat the synthetic note.")
PROVIDER_USAGE = {"input_tokens": 31, "output_tokens": 9, "total_tokens": 40}


@contextmanager
def responses():
    calls = []

    class LocalResponses(BaseHTTPRequestHandler):
        def do_POST(self):
            assert self.path == "/v1/responses", self.path
            assert self.headers.get("Authorization") == "Bearer local-fixture-key"
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            prompts = [part["text"] for item in request["input"] if item.get("role") == "user"
                       for part in item.get("content", []) if part.get("type") == "input_text"]
            last = prompts[-1]
            if last.startswith("研判这组材料"):
                evidence = next(json.loads(part) for part in prompts if part.startswith('{"id":'))
                anchor = evidence["anchor"]["id"]
                assert not request.get("tools"), "assessment exposed runtime tools"
                text = json.dumps({"headline": ASSESSED, "overview": "synthetic local provider response",
                                   "open_questions": [], "evidence_ids": [anchor], "disposition": "unclear"})
                source = "feishu"
            else:
                assert last in PROMPTS, last
                if last == PROMPTS[1]:
                    assert PROMPTS[0] in prompts, "Router did not keep the original native thread history"
                text = "isolated Router result"
                source = "router"
            key = f"{source}-response-{len(calls)}"
            calls.append((source, key))
            item = {"id": f"msg-{key}", "type": "message", "role": "assistant", "status": "completed",
                    "phase": "final_answer", "content": [{"type": "output_text", "text": text, "annotations": []}]}
            response = {"id": key, "model": MODEL, "output": [item], "usage": PROVIDER_USAGE}
            events = [
                {"type": "response.created", "response": {"id": key}},
                {"type": "response.output_item.added", "output_index": 0, "item": item},
                {"type": "response.output_item.done", "output_index": 0, "item": item},
                {"type": "response.completed", "response": response},
            ]
            body = "".join(f"data: {json.dumps(event)}\n\n" for event in events).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    with ThreadingHTTPServer(("127.0.0.1", 0), LocalResponses) as listener:
        thread = threading.Thread(target=listener.serve_forever)
        thread.start()
        try:
            yield f"http://127.0.0.1:{listener.server_port}/v1", calls
        finally:
            listener.shutdown()
            thread.join()


def run(args, root, url, calls):
    def configure(home):
        home.mkdir(parents=True)
        (home / "config.toml").write_text(f'''model = "{MODEL}"
model_provider = "fixture"
[model_providers.fixture]
name = "Local metering fixture"
base_url = "{url}"
wire_api = "responses"
env_key = "ARK_API_KEY"
''')

    router_home = root / "router-home"
    assessor_home = root / "assessor-home"
    assessor_cwd = root / "assessor-cwd"
    assessor_cwd.mkdir()
    configure(router_home)
    configure(assessor_home / ".codex-ark")
    lark = root / "lark-local"
    lark.write_text(f"#!{sys.executable}\n" + (TESTS / "fixtures/feishu_cli.py").read_text())
    lark.chmod(0o700)
    feishu = root / "feishu.json"
    feishu.write_text(json.dumps({
        "owner_open_id": "ou_fixture_owner", "profile": "fixture-user", "cli": str(lark),
        "agent": {"id": "LocalRealAppServer", "program": str(args.codex), "model": MODEL,
                  "home": str(assessor_home), "cwd": str(assessor_cwd), "timeout_seconds": 15},
    }))
    now = int(time.time())
    scenario = root / "feishu-scenario.json"
    scenario.write_text(json.dumps({"mode": "empty", "at": now}))
    addr = free_port()
    env = {**os.environ, "CODEX_HOME": str(router_home), "ARK_API_KEY": "local-fixture-key",
           "TEND_TOKEN": "local-native-metering", "TEND_URL": f"http://{addr}",
           "NO_PROXY": "127.0.0.1,localhost"}

    def cli(*words):
        reply = subprocess.run([args.cli, *words], env=env, text=True, capture_output=True, timeout=10)
        assert reply.returncode == 0, reply.stderr
        return json.loads(reply.stdout)

    def sql(statement, *parameters):
        with sqlite3.connect(root / "server.sqlite", timeout=5) as conn:
            return conn.execute(statement, parameters).fetchall()

    def sources():
        return {row["source"]: row for row in cli("token-usage", "overview")["sources"]}

    def send(prompt):
        receipt = cli("chat", "send", "main", prompt)
        wait_for(lambda: cli("chat", "read", "main")["messages"],
                 lambda rows: any(row["reply_to"] == receipt["message_id"] for row in rows))

    def assert_usage(source, requests):
        usage = sources()[source]["usage"]
        assert usage["reported_requests"] == requests, usage
        assert usage["total_tokens"] == {"tokens": str(requests * 40), "reported_requests": requests}
        for field in ("cached_input_tokens", "cache_write_input_tokens", "reasoning_output_tokens"):
            assert usage[field] == {"tokens": None, "reported_requests": 0}, usage[field]
        assert usage["models"][0]["model"] is None and usage["estimate"]["usd"] is None

    options = dict(listen=addr, feishu_config=feishu, router_launcher=args.codex, router_model=MODEL)
    with server(args.server, root, env, **options):
        wait_for(lambda: cli("feishu-suggestions", "list")["scan"]["phase"], lambda state: state == "idle")
        assert not calls and sources()["feishu"]["usage"]["reported_requests"] == 0
        send(PROMPTS[0])
        assert_usage("router", 1)
        sql("UPDATE feishu_scan SET checkpoint_at=?, last_success_at=?", now - 7200, now - 7200)
        # Context-managed sqlite commits DML, even though the helper reads its result.
        scenario.write_text(json.dumps({"mode": "recover", "at": now}))
        cli("feishu-suggestions", "sync")
        wait_for(lambda: cli("feishu-suggestions", "list")["suggestions"],
                 lambda rows: len(rows) == 4 and all(r["evidence"].get("assessment") for r in rows))
        assert_usage("feishu", 4)
        assert not list(assessor_home.rglob("rollout-*.jsonl")), "private ephemeral rollout was retained"
        [attempts] = sql("SELECT count(*),count(rollout) FROM token_agent_attempts WHERE source='feishu'")
        assert attempts == (4, 0)
        meters = sql("""SELECT response,meter FROM token_agent_requests q JOIN token_agent_turns t
            USING(thread,turn) WHERE t.source='feishu'""")
        assert {key for key, _ in meters} == {key for source, key in calls if source == "feishu"}
        assert all(ASSESSED not in raw and json.loads(raw)["provider_counts"] for _, raw in meters)

    with server(args.server, root, env, **options):
        send(PROMPTS[1])
        assert_usage("router", 2)
        assert sql("""SELECT count(DISTINCT thread),count(DISTINCT turn) FROM token_agent_turns
            WHERE source='router'""") == [(1, 2)]
        copies = sql("""SELECT meter FROM token_agent_requests q JOIN token_agent_turns t
            USING(thread,turn) WHERE t.source='router'""")
        assert sorted(json.loads(raw).get("provider_counts", False) for raw, in copies) == [False, True]
        assert_usage("feishu", 4)
        job = "native-optional-zeros"
        cli("token-usage", "backfill", "--all", "--job", job)
        receipt = wait_for(lambda: cli("token-usage", "status", job),
                           lambda status: status["state"].startswith("finished"))
        assert any(r["source"] == "feishu" and r["detail"] == "ephemeral_no_history"
                   for r in receipt["server"])
        assert_usage("router", 2)
        assert_usage("feishu", 4)
    print("PASS actual Codex Router fresh/resumed and Feishu ephemeral lookup, exact usage, optional unknowns and no private rollout")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for field in ("server", "cli", "codex", "scratch"):
        parser.add_argument(field, type=lambda path: Path(path).resolve())
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-native-system-meter-", dir=args.scratch) as directory, responses() as provider:
        run(args, Path(directory), *provider)


if __name__ == "__main__":
    main()
