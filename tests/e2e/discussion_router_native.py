"""Verify the installed Router's actual tools and runtime on a generated handoff."""
import argparse
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shlex
import sqlite3
import subprocess
import tempfile
import threading

from chat import server, wait_for

MODEL = "gpt-5.4"
WARM = "Store synthetic Router context: BLUE RIVER"
DISCUSS = "Compare two local retention options without starting any work."
SUMMARY = "Compare the two synthetic options. Do not start work."
FOLLOW = "Repeat the synthetic Router context."
SENTINEL = "native-routing-canary"


@contextmanager
def provider(marker):
    requests = {}
    counter = 0
    blocked_output = []

    def assert_previous_router_stopped():
        with sqlite3.connect(marker.parent / "server.sqlite") as db:
            pids = db.execute("""SELECT a.process_pid FROM router_attempts a
                JOIN router_inputs i ON i.seq=a.seq WHERE i.chat_id='main' ORDER BY a.rowid""").fetchall()
        previous, current = (row[0] for row in pids[-2:])
        assert previous != current and not Path(f"/proc/{previous}").exists(), pids

    class Fixture(BaseHTTPRequestHandler):
        def do_POST(self):
            nonlocal counter
            data = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            prompts = [part.get("text", "") for item in data["input"]
                       if item.get("role") == "user" for part in item.get("content", [])]
            last = prompts[-1] if prompts else ""
            counter += 1
            message = True
            if last.startswith("Write the Main Chat handoff message"):
                kind, answer = "generation", SUMMARY
            elif last.startswith("Tend Discussion handoff summary") or (not last and any(
                    item.get("call_id") == "call_probe" for item in data["input"])):
                kind, answer = "restricted", "I compared the options without taking action."
                if "restricted" not in requests:
                    assert_previous_router_stopped()
                    assert SUMMARY in last and any("Tend provenance:" in part and DISCUSS in part for part in prompts)
                    assert WARM in prompts, "a process switch lost ordinary Router history"
                    item = {"type": "function_call", "name": "exec_command", "call_id": "call_probe",
                            "arguments": json.dumps({"cmd": f"touch {shlex.quote(str(marker))}"}), "status": "completed"}
                    message = False
                else:
                    [output] = [i["output"] for i in data["input"] if i.get("type") == "function_call_output"
                                and i.get("call_id") == "call_probe"]
                    blocked_output.append(output)
            elif last == DISCUSS:
                kind, answer = "discussion", "Let's compare two options."
            elif last == WARM:
                kind, answer = "ordinary_before", "I remembered BLUE RIVER."
            elif last == FOLLOW:
                kind, answer = "ordinary_after", "BLUE RIVER."
                assert_previous_router_stopped()
                assert WARM in prompts and any(SUMMARY in prompt for prompt in prompts)
            else:
                raise AssertionError(f"unexpected synthetic model prompt {last!r}")
            requests.setdefault(kind, []).append([t.get("name", t["type"]) for t in data.get("tools", [])])
            if message:
                item = {"type": "message", "role": "assistant", "status": "completed", "phase": "final_answer",
                        "content": [{"type": "output_text", "text": answer, "annotations": []}]}
            item["id"] = f"item_{counter}"
            response = {"id": f"resp_{counter}", "model": MODEL, "output": [item]}
            events = [
                {"type": "response.created", "response": {"id": response["id"]}},
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

    with ThreadingHTTPServer(("127.0.0.1", 0), Fixture) as http:
        worker = threading.Thread(target=http.serve_forever)
        worker.start()
        try:
            yield f"http://127.0.0.1:{http.server_port}", requests, blocked_output
        finally:
            http.shutdown()
            worker.join()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in ("server", "cli", "codex", "scratch"):
        parser.add_argument(arg, type=lambda p: Path(p).resolve())
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-router-native-", dir=args.scratch) as path:
        root = Path(path)
        marker = root / SENTINEL
        with provider(marker) as (url, requests, blocked):
            router_home, discussion_home, discussion_cwd = (root / name for name in ("router", "discussion", "empty"))
            for directory in (router_home, discussion_home, discussion_cwd):
                directory.mkdir()
            config = f'''
model = "{MODEL}"
model_provider = "fixture"
web_search = "disabled"
[model_providers.fixture]
name = "Isolated fixture"
base_url = "{url}/v1"
wire_api = "responses"
env_key = "FIXTURE_API_KEY"
[features]
shell_tool = true
unified_exec = true
apply_patch_freeform = true
view_image = true
goals = true
[tools.experimental_request_user_input]
enabled = true
'''
            for home in (router_home, discussion_home):
                (home / "config.toml").write_text(config)
            (router_home / "config.toml").write_text(config + "\n[agents]\nenabled = true\n")
            (discussion_home / "key").write_text("fixture-only")
            launcher = root / "router-launcher"
            launcher.write_text("#!/usr/bin/python3\nimport os,sys\n"
                                f"env = dict(os.environ, CODEX_HOME={str(router_home)!r})\n"
                                f"os.execve({str(args.codex)!r}, [{str(args.codex)!r}, *sys.argv[1:]], env)\n")
            launcher.chmod(0o700)
            discussion_config = root / "discussion.json"
            discussion_config.write_text(json.dumps({"host_id": "native-fixture", "program": str(args.codex),
                "model": MODEL, "codex_home": str(discussion_home), "cwd": str(discussion_cwd),
                "model_api_keys": {"FIXTURE_API_KEY": "discussion/key"}}))
            env = {**os.environ, "TEND_TOKEN": "test-only-native-router", "FIXTURE_API_KEY": "fixture-only",
                   "CODEX_HOME": str(router_home)}
            with server(args.server, root, env, router_launcher=launcher, router_model=MODEL,
                        discussion_config=discussion_config) as address:
                def cli(*words):
                    result = subprocess.run([args.cli, *words], env=dict(env, TEND_URL=f"http://{address}"),
                                            capture_output=True, text=True, timeout=10)
                    assert not result.returncode, result.stderr
                    return json.loads(result.stdout)

                def reply(chat, message):
                    return next(row for row in wait_for(lambda: cli("chat", "read", chat)["messages"],
                                lambda rows: any(r["reply_to"] == message for r in rows)) if row["reply_to"] == message)

                warm = cli("chat", "send", "main", WARM)["message_id"]
                reply("main", warm)
                chat = cli("discussion", "create")["chat_id"]
                reply(chat, cli("chat", "send", chat, DISCUSS)["message_id"])
                cli("discussion", "handoff-now", chat)
                delivery = wait_for(lambda: cli("discussion", "handoff-status", chat)["request"],
                                    lambda r: r and r["status"] == "sent")["message_id"]
                reply("main", delivery)
                for tools in requests["restricted"]:
                    assert "tend_dispatch_issue" in tools and "tend_read_handoff_source" in tools, tools
                    assert all(tool.startswith("tend_") for tool in tools), tools
                follow = cli("chat", "send", "main", FOLLOW)["message_id"]
                assert "BLUE RIVER" in reply("main", follow)["text"]
                assert not marker.exists(), "unadvertised native tool wrote a marker"
                assert blocked and any(term in str(blocked).lower() for term in ("not found", "unknown", "unsupported", "not available")), blocked
                for kind in ("ordinary_before", "ordinary_after", "discussion"):
                    assert any("exec_command" in tools for tools in requests[kind]), requests
                # Codex 0.172 persists the disabled multi-agent version in
                # the rollout. Other ordinary tools and the thread continue.
                ordinary_tools = lambda kind: set(requests[kind][0]) - {"multi_agent_v1"}
                assert ordinary_tools("ordinary_before") == ordinary_tools("ordinary_after")
                with sqlite3.connect(root / "server.sqlite") as db:
                    attempts = db.execute("""SELECT a.thread_id,a.process_pid,a.turn_id FROM router_attempts a
                        JOIN router_inputs i ON i.seq=a.seq JOIN chat_messages m ON m.seq=i.message_seq
                        WHERE m.id IN (?,?,?) ORDER BY m.seq""", (warm, delivery, follow)).fetchall()
                    assert db.execute("SELECT count(*) FROM router_dispatches").fetchone() == (0,)
                assert len(attempts) == 3 and len({row[0] for row in attempts}) == 1, attempts
                assert len({row[1] for row in attempts}) == 3, attempts
                [rollout] = (router_home / "sessions").rglob(f"*{attempts[0][0]}*.jsonl")
                context = [entry["payload"] for line in rollout.read_text().splitlines()
                           if (entry := json.loads(line))["type"] == "turn_context"]
                restricted_context = next(row for row in context if row.get("turn_id") == attempts[1][2])
                sandbox = restricted_context["permission_profile"]
                assert restricted_context["approval_policy"] == "never"
                assert restricted_context["active_permission_profile"]["id"] == "tend-router-routing"
                assert sandbox["type"] == "managed" and sandbox["network"] == "restricted", sandbox
                assert not any(entry.get("access") == "write" for entry in sandbox["file_system"]["entries"]), sandbox
                print("Restricted native turn permission profile:", sandbox)
                print("PASS installed App Server exposed only Tend tools and rejected an unadvertised native write")
                print("PASS same Router thread across confirmed process stops, with ordinary shell and Tend tools restored; Codex persists native multi-agent mode")
                print(json.dumps({kind: tools[0] for kind, tools in requests.items()}, sort_keys=True))


if __name__ == "__main__":
    main()
