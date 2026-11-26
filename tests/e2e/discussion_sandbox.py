"""Run native Discussion turns with a real App Server and fixed local model output."""
import argparse
from collections import deque
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
from queue import Queue
import shlex
import subprocess
import tempfile
from threading import Thread
from urllib.parse import urlsplit

PROFILE = "tend-discussion"
MODEL = "gpt-5.4"
TOOLS = ("tend_read_discussion_reference", "tend_read_discussion_delivery", "tend_read_discussion_history")
PROBE = r"""
import json, os, pathlib, socket, subprocess, sys
scope = json.loads(sys.argv[1])
for path, expected, oid in scope["allowed"]:
    assert pathlib.Path(path, "visible.txt").read_text() == expected
    assert subprocess.check_output(
        ["/usr/bin/git", "-C", path, "cat-file", "blob", oid], text=True) == expected
assert "OPENAI_API_KEY" not in os.environ and "TEND_TOKEN" not in os.environ
try:
    app_environment = pathlib.Path(f"/proc/{scope['server_pid']}/environ").read_bytes()
except OSError:
    pass
else:
    assert b"service-env-secret" not in app_environment
for path in scope["denied"]:
    try:
        pathlib.Path(path).read_text()
    except PermissionError:
        pass
    except FileNotFoundError:
        # The sandbox can mask an inaccessible path; the test checks it exists outside.
        pass
    else:
        raise AssertionError(f"Unattached path read: {path}")
for directory in scope["no_write"]:
    path = pathlib.Path(directory, "write-probe")
    try:
        path.write_text("denied")
    except OSError:
        pass
    else:
        raise AssertionError(f"Writable path: {path}")
assert os.environ["CODEX_SANDBOX_NETWORK_DISABLED"] == "1"
try:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
except PermissionError:
    pass
else:
    raise AssertionError("TCP bind was not blocked")
try:
    with socket.create_connection(("127.0.0.1", scope["model_port"]), timeout=2):
        raise AssertionError("TCP connect to the local fixture was not blocked")
except PermissionError:
    pass
print("native probe passed: scoped reads, Git objects, no writes, no credentials, no TCP")
"""


@contextmanager
def model_fixture():
    commands = {}

    class Responses(BaseHTTPRequestHandler):
        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            inputs = request["input"]
            probe = next(key for key in commands if any(
                part.get("text") == key for item in inputs if item.get("role") == "user"
                for part in item.get("content", [])
            ))
            done = any(item.get("type") == "function_call_output"
                       and item.get("call_id") == probe for item in inputs)
            if done:
                item = {"id": f"msg_{probe}", "type": "message", "role": "assistant",
                        "status": "completed", "phase": "final_answer",
                        "content": [{"type": "output_text", "text": "Fixture turn done.", "annotations": []}]}
            else:
                assert any(tool.get("name") == "exec_command" for tool in request["tools"]), request["tools"]
                item = {"id": f"fc_{probe}", "type": "function_call", "name": "exec_command",
                        "status": "completed", "call_id": probe,
                        "arguments": json.dumps({"cmd": commands[probe], "login": False})}
            response = {"id": f"resp_{probe}_{int(done)}", "model": MODEL, "output": [item]}
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

    with ThreadingHTTPServer(("127.0.0.1", 0), Responses) as server:
        worker = Thread(target=server.serve_forever)
        worker.start()
        try:
            yield f"http://127.0.0.1:{server.server_port}", commands
        finally:
            server.shutdown()
            worker.join()


class AppServer:
    def __init__(self, process):
        self.process = process
        self.counter = 0
        self.frames = Queue()
        self.pending = deque()
        self.worker = Thread(target=self.read_frames)
        self.worker.start()

    def read_frames(self):
        for line in self.process.stdout:
            self.frames.put(json.loads(line))
        self.frames.put(None)

    def receive(self):
        return self.pending.popleft() if self.pending else self.next_frame()

    def next_frame(self):
        message = self.frames.get(timeout=30)
        assert message is not None, "App Server closed its stream"
        return message

    def call(self, method, params, notify=False):
        self.counter += 1
        frame = {"method": method, "params": params}
        if not notify:
            frame["id"] = self.counter
        self.process.stdin.write(json.dumps(frame) + "\n")
        self.process.stdin.flush()
        if notify:
            return
        while True:
            message = self.next_frame()
            if message.get("id") == self.counter:
                assert "result" in message, message
                return message["result"]
            self.pending.append(message)


@contextmanager
def app_server(codex, home, cwd, profile_paths):
    allowed = (":minimal", *profile_paths, str(cwd), str(codex))
    table = "{" + ",".join(f'{json.dumps(path)}="read"' for path in allowed) + "}"
    config = [
        f'default_permissions="{PROFILE}"', f"permissions.{PROFILE}.filesystem={table}",
        f"permissions.{PROFILE}.network.enabled=false", 'shell_environment_policy={inherit="none"}',
        'approval_policy="never"', 'web_search="disabled"', "mcp_servers={}", "features.apps=false",
        "features.plugins=false", "features.remote_plugin=false", "features.multi_agent=false",
        "features.multi_agent_v2=false", "features.skill_mcp_dependency_install=false",
        "features.code_mode=false", "features.code_mode_host=false",
    ]
    launch = [codex, "app-server", "--listen", "stdio://", "--strict-config"]
    for setting in config:
        launch.extend(("-c", setting))
    with subprocess.Popen(launch, cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,
                          env={"HOME": str(home), "CODEX_HOME": str(home),
                               "OPENAI_API_KEY": "service-env-secret",
                               "PATH": "/usr/local/bin:/usr/bin:/bin"}) as process:
        app = AppServer(process)
        try:
            app.call("initialize", {"clientInfo": {"name": "tend-sandbox-test", "version": "1"},
                                    "capabilities": {"experimentalApi": True}})
            app.call("initialized", {}, notify=True)
            config = app.call("config/read", {"includeLayers": False})["config"]
            assert not config.get("mcp_servers"), config.get("mcp_servers")
            yield app
        finally:
            process.stdin.close()
            try:
                process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=8)
            app.worker.join(timeout=8)


def run_probe(app, commands, cwd, paths, turn_spec, probe, thread=None, legacy_start=False):
    settings = {"cwd": str(cwd), "permissions": PROFILE, "approvalPolicy": "never"}
    if not legacy_start:
        settings["runtimeWorkspaceRoots"] = paths
    if thread is None:
        result = app.call("thread/start", {
            **settings, "ephemeral": False, "historyMode": "legacy", "dynamicTools": [
                {"name": name, "description": "Scoped Tend Discussion fixture tool",
                 "inputSchema": {"type": "object", "properties": {}}} for name in TOOLS
            ],
        })
        thread = result["thread"]["id"]
    else:
        result = app.call("thread/resume", {**settings, "threadId": thread, "excludeTurns": True})
        assert result["thread"]["id"] == thread, result
    assert result["sandbox"] == {"type": "readOnly", "networkAccess": False}, result
    assert result["activePermissionProfile"] == {"id": PROFILE, "extends": None}, result
    assert result["approvalPolicy"] == "never", result
    if not legacy_start:
        assert result["runtimeWorkspaceRoots"] == paths, result

    commands.clear()
    spec = {**turn_spec, "server_pid": app.process.pid}
    commands[probe] = shlex.join(["/usr/bin/python3", "-c", PROBE, json.dumps(spec)])
    turn = app.call("turn/start", {
        **settings, "threadId": thread, "input": [{"type": "text", "text": probe}],
    })["turn"]["id"]
    executed = []
    while True:
        event = app.receive()
        params = event.get("params", {})
        if params.get("threadId") != thread:
            continue
        if event.get("method") == "item/completed" and params["turnId"] == turn:
            item = params["item"]
            if item["type"] == "commandExecution":
                executed.append(item)
        elif event.get("method") == "turn/completed" and params["turn"]["id"] == turn:
            assert params["turn"]["status"] == "completed", params
            break
    assert len(executed) == 1, executed
    result = executed[0]
    marker = "native probe passed: scoped reads, Git objects, no writes, no credentials, no TCP"
    assert result["exitCode"] == 0 and marker in result["aggregatedOutput"], result
    print(f"{probe} {thread} {paths} — {marker}", flush=True)
    return thread


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--codex", required=True, type=lambda value: Path(value).resolve())
    parser.add_argument("--scratch", required=True, type=lambda value: Path(value).resolve(),
                        help="Use a local scratch parent outside /tmp for Codex's sandbox helpers")
    args = parser.parse_args()
    args.scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=args.scratch, prefix="discussion-sandbox-") as directory, \
            model_fixture() as (endpoint, commands):
        root = Path(directory).resolve()
        home, cwd, a, b, forbidden = (root / part for part in ("service-home", "empty-cwd", "a", "b", "forbidden"))
        for path in (home, cwd, a, b, forbidden):
            path.mkdir()
        (home / "config.toml").write_text(f"""
model = "{MODEL}"
model_provider = "fixture"
[model_providers.fixture]
name = "Local fixture"
base_url = "{endpoint}/v1"
wire_api = "responses"
env_key = "OPENAI_API_KEY"
""")
        (forbidden / "private.txt").write_text("unattached-source-secret")
        (home / "synthetic-auth").write_text("service-file-secret")
        (a / "escape").symlink_to(b / "visible.txt")
        (b / "escape").symlink_to(a / "visible.txt")
        contents = {}
        for repo in (a, b):
            text = f"attached checkout {repo.name} contents"
            (repo / "visible.txt").write_text(text)
            subprocess.run(["git", "init", "-q", "-b", "main", repo], check=True)
            oid = subprocess.check_output(["git", "-C", repo, "hash-object", "-w", "visible.txt"],
                                          text=True).strip()
            contents[repo] = (str(repo), text, oid)

        def probe(case, selected, thread=None, legacy_start=False):
            roots = [str(repo) for repo in selected]
            spec = {
                "allowed": [contents[repo] for repo in selected],
                "denied": [str(repo / "visible.txt") for repo in (a, b) if repo not in selected]
                          + [str(repo / "escape") for repo in selected]
                          + [str(forbidden / "private.txt"), str(home / "synthetic-auth")],
                "no_write": [*roots, str(cwd), str(forbidden)],
                "model_port": urlsplit(endpoint).port,
            }
            assert all(Path(path).is_file() for path in spec["denied"]), spec
            profile_paths = roots if legacy_start else [":workspace_roots"]
            with app_server(args.codex, home, cwd, profile_paths) as app:
                return run_probe(app, commands, cwd, roots, spec, case, thread, legacy_start)

        thread = probe("probe_blank", [])
        assert probe("probe_a", [a], thread) == thread
        assert probe("probe_b", [b], thread) == thread
        assert probe("probe_empty", [], thread) == thread
        legacy = probe("probe_legacy_a", [a], legacy_start=True)
        assert probe("probe_legacy_to_b", [b], legacy) == legacy
        assert all(not (path / "write-probe").exists() for path in (a, b, cwd, forbidden))
    print("PASS fixed local model output drove real native turns; original IDs and sandbox boundaries verified")


if __name__ == "__main__":
    main()
