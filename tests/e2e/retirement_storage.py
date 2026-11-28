"""Verify migration and cleanup of a stopped Runner's legacy private files."""
from contextlib import closing
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
from threading import Thread


HELPER = Path(__file__).resolve().parents[2] / "scripts/tend-runner-retire-files"
CONTENTS = b"immutable delivery\n"


def exercise(root, address, fact):
    db = root / "old.sqlite"
    config = root / "old.json"
    workspace = root / "workspaces"
    home = root / "codex-default"
    named_home = root / "codex-named"
    archive = root / "old.artifacts"
    shared = workspace / ".tend-host" / "host-a"
    receipt = root / "retired.json"
    config_bytes = b'{"runner_id":"old","host_id":"host-a","workspace_root":"workspaces"}'
    config.write_bytes(config_bytes)
    preserved = [workspace / "issue" / "work.txt", root / "backup" / "old.sqlite",
                 root / "active.sqlite", home / "sessions" / "thread", named_home / "sessions" / "thread"]
    for path in preserved:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"keep")
    archive.mkdir()
    original = archive / "guide"
    original.write_bytes(CONTENTS)
    marker = archive / "unrelated.part"
    marker.write_text("keep")
    env = {**os.environ, "CODEX_HOME": str(root / "fixture-codex"), "TEND_URL": address, "TEND_TOKEN": "fixture"}
    bin_dir = root / "bin"
    bin_dir.mkdir()
    for binary in ("codex", "codex-ark"):
        (bin_dir / binary).symlink_to("/bin/true")
    env["PATH"] = str(bin_dir) + os.pathsep + env["PATH"]
    named = {"program": "codex-ark", "args": ["app-server"], "codex_home": str(named_home)}

    def sql(query, *args):
        with closing(sqlite3.connect(db)) as conn, conn:
            return conn.execute(query, args).fetchall()

    with closing(sqlite3.connect(db)) as conn, conn:
        conn.executescript("""
            CREATE TABLE runner_instance(singleton,runner_id,host_id);
            INSERT INTO runner_instance VALUES(1,'old','host-a');
            CREATE TABLE received_assignments(id,task_id,execution_state,acknowledged,app_server);
            INSERT INTO received_assignments VALUES('a','task-a','waiting',1,NULL),('b','task-b','waiting',1,NULL);
            CREATE TABLE local_runs(id,assignment_id,state,thread_id,turn_id,native_context,app_server);
            INSERT INTO local_runs VALUES('r1','a','stopped','thread-default','turn-a',NULL,NULL),
                                         ('r2','b','stopped','thread-named','turn-b',NULL,NULL);
            CREATE TABLE local_work_operations(id,operation);
            CREATE TABLE local_drain(successor);
            INSERT INTO local_drain VALUES('next');
            CREATE TABLE run_outbox(acknowledged);
            CREATE TABLE local_workspace_reclaims(acknowledged);
        """)
        conn.execute("UPDATE received_assignments SET app_server=? WHERE id='b'", [json.dumps(named)])
        conn.execute("INSERT INTO local_work_operations VALUES('guide',?)",
                     [json.dumps({"action": {"kind": "submit_artifact", "bytes": len(CONTENTS)}})])

    def call(action, error=None):
        args = [HELPER, action, "--host", "host-a", "--runner", "old", "--db", db,
                "--config", config, "--workspace-root", workspace, "--codex-home", home]
        if action != "adopt":
            args.extend(["--successor", "next", "--receipt", receipt])
        result = subprocess.run(args, env=env, text=True, capture_output=True, timeout=7)
        if error is None:
            assert result.returncode == 0, result.stderr
            return json.loads(result.stdout)
        assert result.returncode and error in result.stderr, result
        assert db.is_file() and config.read_bytes() == config_bytes
        assert all(p.read_bytes() == b"keep" for p in preserved)

    # Active generation is adopted while its process is down. Tasks and their IDs survive.
    sql("UPDATE local_runs SET state='running' WHERE id='r1'")
    call("adopt", "no stop receipt")
    sql("UPDATE local_runs SET state='stopped'")
    original.unlink()
    call("adopt", "Missing immutable original")
    original.write_bytes(CONTENTS)
    assert call("adopt")["threads"] == 2
    assert sql("SELECT task_id,execution_state FROM received_assignments ORDER BY id") == [
        ("task-a", "waiting"), ("task-b", "waiting")]
    history = shared / "history" / "old.sqlite"
    with closing(sqlite3.connect(history)) as index:
        bindings = {thread: json.loads(launch) for thread, launch in index.execute(
            "SELECT threads.id,profiles.launch FROM threads JOIN profiles ON profile_id=profiles.id")}
    assert bindings["thread-default"]["codex_home"] == str(home)
    assert bindings["thread-default"]["program"] == str(bin_dir / "codex")
    assert bindings["thread-named"] == {**named, "program": str(bin_dir / "codex-ark")}
    dest = shared / "artifacts" / "guide"
    assert dest.read_bytes() == CONTENTS and config.read_bytes() == config_bytes
    dest.unlink()
    dest.write_bytes(b"!" * len(CONTENTS))
    call("adopt", "Different content")
    dest.unlink()

    call("prepare", "awaiting acknowledgement")
    sql("UPDATE received_assignments SET execution_state='handed_off'")
    call("prepare", "Published local originals")
    assert not receipt.exists() and dest.read_bytes() == CONTENTS
    fact["artifacts"][0]["available"] = True
    call("prepare")
    prepared = json.loads(receipt.read_text())
    assert prepared["threads"] == 2 and prepared["artifacts"] == {"guide": len(CONTENTS)}
    call("prepare")
    assert json.loads(receipt.read_text()) == prepared
    call("cleanup", "not retired")
    fact["retired_at"] = 2
    sql("INSERT INTO run_outbox VALUES(0)")
    call("cleanup", "run_outbox")
    sql("UPDATE run_outbox SET acknowledged=1")
    sql("CREATE TABLE local_token_usage(acknowledged)")
    sql("INSERT INTO local_token_usage VALUES(0)")
    call("cleanup", "local_token_usage")
    sql("UPDATE local_token_usage SET acknowledged=1")
    assert call("cleanup")["cleaned"]
    assert call("cleanup")["cleaned"]
    assert not db.exists() and not config.exists() and not original.exists()
    assert dest.read_bytes() == CONTENTS and history.is_file() and marker.read_text() == "keep"
    assert all(p.read_bytes() == b"keep" for p in preserved)


def main():
    fact = {"host_data_policy": 1, "runner_id": "old", "host_id": "host-a", "successor": "next",
            "drained_at": 1, "retired_at": None,
            "artifacts": [{"id": "guide", "bytes": len(CONTENTS), "available": False}]}

    class Server(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path != "/api/v1/runners/old/retirement" or self.headers.get("Authorization") != "Bearer fixture":
                self.send_error(404)
                return
            body = json.dumps(fact).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    with tempfile.TemporaryDirectory() as temp, HTTPServer(("127.0.0.1", 0), Server) as server:
        thread = Thread(target=server.serve_forever)
        thread.start()
        try:
            exercise(Path(temp), f"http://127.0.0.1:{server.server_port}", fact)
        finally:
            server.shutdown()
            thread.join()
    print("PASS legacy adoption, original publication, protected retirement and repeat cleanup")


if __name__ == "__main__":
    main()
