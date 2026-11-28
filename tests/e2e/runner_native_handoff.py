"""Run two real Runner generations and a real Codex App Server on loopback only."""

import argparse
from contextlib import ExitStack, closing, contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import sys
import tempfile
from threading import Event, Thread

from chat import lose_first_response, server, wait_for
from issue_chat import TESTS, free_port, prepare

REQUEST = "Merge the example change and wait for green CI on remote main."
PULL = "https://github.com/example/guide/pull/82"
QUEUED = "Isolated acceptance input queued while the original artifact was syncing."
ORIGINAL = "The original native turn stopped without closing its Issue."
CONTINUED = "The successor ended a new native turn."
FILE = "Original isolated handoff evidence.\n"
MODEL = "gpt-5.4"
SHA = "d" * 40
PROXY = TESTS / "fixtures/native_handoff_app_proxy.py"


@contextmanager
def provider():
    calls = []
    finish_original = Event()
    steps = [
        ("tend_submit_artifact", {"path": "repos/example/README.md"}),
        ("tend_post_issue_message", {"text": "Original handoff receipt."}),
        ("tend_waitfor", {"url": PULL, "event": "ci", "state": "success", "head_sha": SHA}),
        ORIGINAL,
        CONTINUED,
    ]

    class Responses(BaseHTTPRequestHandler):
        def do_POST(self):
            assert self.path == "/v1/responses"
            assert self.headers["Authorization"] == "Bearer isolated-only"
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            assert request["model"] == MODEL
            index = len(calls)
            calls.append(request)
            if index == 3:
                assert finish_original.wait(45)
            step = steps[index]
            response = f"isolated-{index}"
            if isinstance(step, tuple):
                name, arguments = step
                assert name in {tool.get("name") for tool in request["tools"]}, name
                item = {"type": "function_call", "id": f"fc-{index}", "call_id": f"call-{index}",
                        "name": name, "arguments": json.dumps(arguments), "status": "completed"}
            else:
                item = {"type": "message", "role": "assistant", "id": f"msg-{index}",
                        "status": "completed", "phase": "final_answer",
                        "content": [{"type": "output_text", "text": step, "annotations": []}]}
            events = [
                {"type": "response.created", "response": {"id": response}},
                {"type": "response.output_item.added", "output_index": 0, "item": item},
                {"type": "response.output_item.done", "output_index": 0, "item": item},
                {"type": "response.completed", "response": {"id": response, "model": MODEL,
                    "output": [item], "usage": {"input_tokens": 31, "output_tokens": 9, "total_tokens": 40}}},
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
        thread = Thread(target=listener.serve_forever)
        thread.start()
        try:
            yield f"http://127.0.0.1:{listener.server_port}/v1", calls, finish_original
        finally:
            finish_original.set()
            listener.shutdown()
            thread.join()


def exercise(args, root, upstream, calls, finish_original):
    address = free_port()
    prepare(root, free_port(), FILE)
    settings = json.loads((root / "runner.json").read_text())
    settings.pop("inspect")
    settings["workspace_root"] = str(root / "workspace")
    home = root / "runner-home"
    home.mkdir(mode=0o700)
    config = f'''model = "{MODEL}"
model_provider = "isolated"
[model_providers.isolated]
name = "Local isolated provider"
base_url = "{upstream}"
wire_api = "responses"
env_key = "TEND_FIXTURE_MODEL_KEY"
'''
    (home / "config.toml").write_text(config)
    settings["agents"] = [{"id": "named", "category": "implementation-economy", "revision": "one",
                           "model": MODEL, "reasoning_effort": "low", "app_server": "native"}]
    settings["app_servers"] = {"native": {"program": sys.executable,
        "args": [str(PROXY), str(args.codex), str(root)], "codex_home": str(home)}}
    next_settings = {**settings, "runner_id": "next-runner", "standby": True}
    (root / "native-mode").write_text(args.mode)
    (root / "direct-main-sha").write_text(SHA)
    (root / "wait-mode").write_text("pending")
    os_home = root / "os-home"
    os_home.mkdir()
    env = {"PATH": os.environ["PATH"], "HOME": str(os_home), "NO_PROXY": "127.0.0.1,localhost",
           "TEND_URL": f"http://{address}", "TEND_TOKEN": "isolated-native-handoff",
           "TEND_FIXTURE_MODEL_KEY": "isolated-only", "CODEX_HOME": str(root / "router-home"),
           "GH_TOKEN": "fixture-read-token", "TEND_GITHUB_FIXTURE_DIR": str(root),
           "PYTHONDONTWRITEBYTECODE": "1"}
    legacy = bool(args.predecessor or args.legacy_server)
    if args.legacy_server:
        (root / "legacy-categories").touch()
    lost_fresh = args.mode.startswith("lost-fresh-start")
    hold_upload = Event()
    hold_upload.set()

    def cli(*words, raw=False):
        result = subprocess.check_output([args.cli, *words], env=env, timeout=8)
        return result if raw else json.loads(result)

    def sql(db, statement, *params):
        with closing(sqlite3.connect(root / f"{db}.sqlite", timeout=3)) as connection:
            with connection:
                return connection.execute(statement, params).fetchall()

    def trace():
        path = root / "native-rpc.ndjson"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def spawn(stack, name, runner_settings, destination, binary):
        cfg = root / f"{name}.json"
        cfg.write_text(json.dumps(runner_settings))
        log = stack.enter_context((root / f"{name}.log").open("ab"))
        process = subprocess.Popen([
            binary, "--db", root / f"{name}.sqlite", "--config", cfg, "--ready-file", root / f"{name}.ready",
            "--app-server", "/bin/false",
        ], env={**env, "TEND_URL": f"http://{destination}", "TEND_FIXTURE_GENERATION": name},
           stderr=log, cwd=root)

        def cleanup():
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        stack.callback(cleanup)
        return process

    def run_view(issue):
        return cli("issue", "read", issue)

    def assignment(db, task):
        return sql(db, "SELECT id, execution_state, retains_workspace FROM received_assignments WHERE task_id=?", task)

    with ExitStack() as stack:
        stack.enter_context(server(args.legacy_server or args.server, root, env, listen=address, github_cli=TESTS / "fixtures/github_cli.py"))
        old_address, posted = stack.enter_context(lose_first_response(
            address, drop_when=lambda response: bool(transfer_id and transfer_id in response.get("confirmed_operations", [])),
            block_upload=hold_upload.is_set))
        transfer_id = None
        old = spawn(stack, "predecessor", settings, old_address, args.predecessor or args.runner)
        wait_for(lambda: cli("runner", "list"), bool)
        source = cli("chat", "send", "main", REQUEST)["message_id"]
        [issue] = wait_for(lambda: next((m.get("issue_ids") for m in cli("chat", "read", "main")["messages"]
                                        if m["id"] == source), None), bool)
        running = wait_for(lambda: run_view(issue), lambda view: view["status"] == "running")
        task = running["task_id"]
        wait_for(lambda: len(calls), lambda count: count == 4, timeout=20)
        [(file_id, available)] = wait_for(lambda: sql("server", "SELECT id,available FROM artifacts WHERE task_id=?", task), bool)
        [(watch_id,)] = wait_for(lambda: sql("server", "SELECT id FROM external_watches WHERE task_id=?", task), bool)
        assert available == 0
        current = spawn(stack, "successor", next_settings, old_address, args.runner)
        wait_for(lambda: cli("runner", "list"), lambda roster: len(roster) == 2)
        cli("runner", "replace", settings["runner_id"], next_settings["runner_id"])
        assert old.poll() is None and current.poll() is None
        assert not [event for event in trace() if event["generation"] == "successor"]
        finish_original.set()
        stopped = wait_for(lambda: run_view(issue), lambda view: view["status"] == "waiting")
        assert stopped["issue_state"] == "open" and stopped["run"]["phase"]["waiting_for_external"]
        old_run = stopped["run"]["run_id"]
        [(original_thread, original_turn, workspace_json)] = sql(
            "predecessor", "SELECT thread_id,turn_id,workspace_stamp FROM local_runs WHERE id=?", old_run)
        [rollout] = (home / "sessions").rglob(f"rollout-*-{original_thread}.jsonl")
        if args.mode == "missing" or lost_fresh:
            # Remove the original native record after actual stop, leaving
            # the isolated service home itself unchanged.
            rollout.rename(root / "removed-original.jsonl")
        if args.mode == "home-changed":
            # Preserve the absolute path and config, change the directory
            # identity only, after the original App Server has exited.
            home.rename(root / "removed-original-home")
            home.mkdir(mode=0o700)
            (home / "config.toml").write_text(config)
        workspace_stamp = json.loads(workspace_json)
        directory = Path(workspace_stamp["path"])
        (directory / "acceptance-marker").write_text(old_run)
        if not legacy:
            [(transfer_id,)] = wait_for(lambda: sql(
                "predecessor", "SELECT id FROM run_outbox WHERE assignment_id=? AND record_kind='handoff'",
                stopped["placement"]["assignment_id"]), bool)
            wait_for(lambda: [reply for _, _, reply in posted if reply.get("uploads") == [file_id]], bool)
            assert assignment("successor", task) == []
            assert old.poll() is None
        else:
            # The legacy predecessor publishes no native proof and need not
            # wait for upload availability at the handoff point.
            hold_upload.clear()

        # Both the user's message and the original wait resolution are
        # pending before the successor can run. The predecessor is fenced.
        wait_for(lambda: (root / "direct-ci-reads").exists(), bool)
        (root / "direct-ci-success").touch()
        sql("server", "UPDATE pulls SET due_at = 0 WHERE url = ?", PULL)
        wait_for(lambda: "SUCCESS" in (root / "direct-ci-reads").read_text(), bool)
        wait_for(lambda: sql("server", "SELECT due_at FROM external_batches WHERE task_id=?", task), bool)
        sql("server", "UPDATE external_batches SET due_at = 0 WHERE task_id=?", task)
        wait_for(lambda: sql("server", "SELECT id FROM issue_inputs WHERE task_id=? "
                                    "AND json_extract(payload,'$.kind')='external_fact'", task), bool)
        if not legacy:
            pending = cli("issue", "message", issue, QUEUED, "fixture-queued-input")
            assert sql("server", "SELECT available FROM artifacts WHERE id=?", file_id) == [(0,)]
            assert assignment("successor", task) == []
        hold_upload.clear()
        if args.mode.startswith("stop-frontier"):
            if args.mode.endswith("after-active"):
                wait_for(lambda: sql("successor", "SELECT resume_guard FROM local_runs"),
                         lambda rows: rows == [(original_thread,)])
            wait_for(lambda: "frontier-unavailable" in {row["event"] for row in trace()}, bool)
            [(prepared, guard, thread, pid)] = wait_for(lambda: sql(
                "successor", "SELECT id,resume_guard,thread_id,process_pid FROM local_runs "
                             "WHERE state='prepared' AND resume_retry_at > 0"),
                lambda rows: bool(rows) and (not args.mode.endswith("after-active") or rows[0][1] is None))
            assert guard is thread is pid is None
            control = cli("task", "stop", task, "stop-with-unreadable-frontier")
            wait_for(lambda: cli("control", "read", "stop-with-unreadable-frontier"),
                     lambda item: item.get("state") == "stopped")
            ended = wait_for(lambda: run_view(issue), lambda view: view["status"] == "cancelled")
            assert ended["task_id"] == task and ended["run"]["run_id"] == prepared
            assert sql("successor", "SELECT state,thread_id,turn_id,process_pid FROM local_runs") == [
                ("stopped", None, None, None)]
            assert [(e["generation"], e["client"]) for e in trace() if e["event"] == "turn/start"] == [
                ("predecessor", old_run)]
            assert old.wait(timeout=15) == 0 and len(calls) == 4
            assert cli("artifact", "download", file_id, raw=True) == FILE.encode()
            print(json.dumps({"mode": args.mode, "task": task, "old_run": old_run,
                              "stopped_prepared_run": prepared, "control": control,
                              "verified_control_state": "stopped", "events": trace()}), flush=True)
            return
        if lost_fresh:
            wait_for((root / "hold-empty-thread").exists, bool)
            [(allocated_run, allocation_pid, allocated_thread, allocated_turn, state)] = sql(
                "successor", "SELECT id,process_pid,thread_id,turn_id,state FROM local_runs")
            assert allocation_pid and allocated_thread is allocated_turn is None and state == "prepared"
            os.kill(allocation_pid, 0)  # Exact isolated allocation is still alive.
            assert len(calls) == 4
            if args.mode == "lost-fresh-start-restart":
                # Restart just the isolated Runner. Its original App Server
                # allocation process remains live behind the fixture gate.
                current.kill()
                current.wait(timeout=8)
                current = spawn(stack, "successor", next_settings, old_address, args.runner)
                ready = root / "successor.ready"
                wait_for(lambda: ready.exists() and ready.read_text().strip() == str(current.pid), bool)
                assert sql("successor", "SELECT id,process_pid,thread_id FROM local_runs") == [
                    (allocated_run, allocation_pid, None)]
                os.kill(allocation_pid, 0)
                assert sum(e["event"] == "empty-thread-allocated" for e in trace()) == 1
            (root / "release-empty-thread").touch()
        end = wait_for(lambda: run_view(issue), lambda view: view["status"] == "completed", timeout=40)
        assert old.wait(timeout=15) == 0
        new_run = end["run"]["run_id"]
        assert end["task_id"] == task and end["issue_state"] == "open"
        assert end["placement"]["runner_id"] == "next-runner"
        [(thread, turn, stamp)] = sql("successor", "SELECT thread_id,turn_id,workspace_stamp FROM local_runs")
        assert new_run != old_run and turn != original_turn and stamp == workspace_json
        assert (directory / "acceptance-marker").read_text() == old_run
        reused = args.mode in {"resume", "faults"} and not legacy
        assert (thread == original_thread) == reused
        if reused:
            records = [json.loads(line) for line in rollout.read_text().splitlines()]
            assert records[0]["payload"]["id"] == original_thread
            assert {r["payload"]["turn_id"] for r in records if r["type"] == "turn_context"} == {original_turn, turn}
        assert len(calls) == 5, len(calls)
        assert (ORIGINAL in json.dumps(calls[-1])) == reused
        if not legacy:
            assert QUEUED in json.dumps(calls[-1])
            assert sql("server", "SELECT run_id FROM issue_inputs WHERE task_id=? AND "
                                 "json_extract(payload,'$.message.id')=?", task, pending["message_id"]) == [(new_run,)]
            assert sum(transfer_id in reply.get("confirmed_operations", []) for _, _, reply in posted) >= 2
        assert sql("server", "SELECT run_id FROM issue_inputs WHERE task_id=? "
                             "AND json_extract(payload,'$.watch_id')=?", task, watch_id) == [(new_run,)]
        assert sql("server", "SELECT count(*) FROM external_watches WHERE id=?", watch_id) == [(1,)]
        assert sql("server", "SELECT id,available FROM artifacts WHERE task_id=?", task) == [(file_id, 1)]
        assert cli("artifact", "download", file_id, raw=True) == FILE.encode()
        assert len(sql("predecessor", "SELECT id FROM local_work_operations")) == 3
        assert sql("successor", "SELECT id FROM local_work_operations") == []
        for (operation_id,) in sql("predecessor", "SELECT id FROM local_work_operations"):
            assert sql("server", "SELECT count(*) FROM runner_operations WHERE id=?", operation_id) == [(1,)]
        events = trace()
        assert [(e["generation"], e["client"]) for e in events if e["event"] == "turn/start"] == [
            ("predecessor", old_run), ("successor", new_run)], events
        if lost_fresh:
            first_empty, second_empty = [e["thread"] for e in events if e["event"] == "empty-thread-allocated"]
            assert first_empty != second_empty == thread and new_run == allocated_run
            assert sum(e["event"] == "fresh-start-reply-lost" for e in events) == 1
            assert all(e["thread"] != first_empty for e in events if e["event"] == "turn/start")
        assert sum(e["event"] == "thread/start" for e in events) == (1 if reused else 3 if lost_fresh else 2)
        if args.mode == "faults":
            assert {"startup-failed", "resume-reply-lost", "turn-reply-lost", "history-rpc-failed"} <= {e["event"] for e in events}
        if args.mode in {"missing", "home-changed"} or lost_fresh:
            log = (root / "successor.log").read_text()
            assert re.search("original native thread no longer exists|isolated Codex home identity changed", log)
        print(json.dumps({"mode": args.mode, "legacy": "server" if args.legacy_server else "predecessor" if args.predecessor else None, "task": task, "workspace": workspace_stamp,
                          "old": [old_run, original_thread, original_turn], "new": [new_run, thread, turn],
                          "watch": watch_id, "original_file": file_id, "events": events, "model_requests": len(calls)}),
              flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "cli", "runner", "codex", "scratch"):
        parser.add_argument(name, type=lambda path: Path(path).resolve())
    old = parser.add_mutually_exclusive_group()
    old.add_argument("--predecessor", type=lambda path: Path(path).resolve())
    old.add_argument("--legacy-server", type=lambda path: Path(path).resolve())
    parser.add_argument("--mode", choices=("resume", "faults", "missing", "home-changed",
                                          "stop-frontier", "stop-frontier-after-active", "lost-fresh-start",
                                          "lost-fresh-start-restart"),
                        default="resume")
    args = parser.parse_args()
    if (args.predecessor or args.legacy_server) and args.mode != "resume":
        parser.error("legacy binaries only run the default continuity comparison")
    with tempfile.TemporaryDirectory(prefix="native-handoff-", dir=args.scratch) as directory, provider() as upstream:
        root = Path(directory)
        try:
            exercise(args, root, *upstream)
        except BaseException:
            for path in (*root.glob("*.log"), *root.glob("native-rpc.ndjson")):
                print(path.name, path.read_text(), file=sys.stderr)
            with sqlite3.connect(root / "server.sqlite") as failed:
                for table in ("external_watches", "pulls", "external_deliveries", "issue_inputs"):
                    print(table, failed.execute(f"SELECT * FROM {table}").fetchall(), file=sys.stderr)
            raise


if __name__ == "__main__":
    main()
