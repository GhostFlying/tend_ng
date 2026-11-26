"""Upgrade a protocol-2 Server in place while the same Runner and Task stay alive."""

import argparse
import contextlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
from urllib.error import HTTPError
from urllib.request import Request, urlopen

from chat import server, wait_for
from issue_chat import TESTS, free_port, prepare


def exercise(args, root):
    address = free_port()
    prepare(root, free_port())
    config = root / "runner.json"
    db_path = root / "runner.sqlite"
    codex = root / "codex"
    codex.mkdir()
    env = {**os.environ, "TEND_TOKEN": "stage-compat-synthetic",
           "TEND_URL": f"http://{address}", "CODEX_HOME": str(codex)}
    runner = [args.runner, "--config", config, "--db", db_path]

    def cli(*words):
        return json.loads(subprocess.check_output([args.cli, *words], env=env, timeout=5))

    def sql(database, statement, *params):
        with contextlib.closing(sqlite3.connect(root / f"{database}.sqlite")) as conn:
            return conn.execute(statement, params).fetchall()

    def chat_message(snippet):
        return wait_for(lambda: next((row for row in cli("chat", "read", "main")["messages"]
                                     if snippet in row["text"]), None), bool)

    def answer(reply, text):
        cli("chat", "send", "main", text, "--reply-to", reply["id"])

    with (root / "runner.log").open("w") as log:
        process = subprocess.Popen([
            *runner, "--app-server", sys.executable,
            TESTS / "fixtures/direct_owner_app_server.py", root,
        ], env=env, stderr=log)
        try:
            with server(args.legacy_server, root, env, listen=address):
                [registered] = wait_for(lambda: cli("runner", "list"), bool)
                assert registered["protocol"] == 2
                receipt = cli("chat", "send", "main", "Check stage support across a Server upgrade.")
                question = chat_message("Resume after the Server upgrade?")
                [issue] = next(m["issue_ids"] for m in cli("chat", "read", "main")["messages"]
                               if m["id"] == receipt["message_id"])
                view = wait_for(lambda: cli("issue", "read", issue), lambda v: v["status"] == "waiting")
                task = view["task_id"]
                [report] = [row[0] for row in sql("server", "SELECT id FROM issue_reports")]
                assert sql("server", "SELECT outcome FROM router_inputs WHERE report_id=?", report) == [
                    ("failed",)]
                assert sql("server", "SELECT count(*) FROM issue_report_publications") == [(0,)]
                assert sql("server", "SELECT json_extract(request,'$.question') FROM attentions") == [
                    ("Resume after the Server upgrade?",)]
                assert sql("runner", "SELECT count(*) FROM local_work_operations WHERE "
                           "json_extract(operation,'$.action.kind') = 'ask_stage_attention'") == [(0,)]

                # Even a stale handshaking client cannot turn a new action into
                # an ordinary question by sending it directly to the old Server.
                profile = registered["profile"]
                premature = Request(
                    f"http://{address}/api/v1/runners/{profile['runner_id']}/sync",
                    data=json.dumps({
                        "protocol": 2, "profile": profile, "receipts": [],
                        "records": [{"kind": "operation", "data": {
                            "operation_id": "compat-stale-stage-operation",
                            "assignment_id": view["placement"]["assignment_id"],
                            "run_id": view["run"]["run_id"],
                            "action": {"kind": "ask_stage_attention", "report_id": report,
                                       "request": {"question": "Must not silently publish."}},
                        }}],
                    }).encode(),
                    headers={"Authorization": f"Bearer {env['TEND_TOKEN']}", "Content-Type": "application/json"},
                )
                try:
                    with urlopen(premature, timeout=5) as response:
                        raise AssertionError(f"Old Server accepted a new stage operation: {response.status}")
                except HTTPError as rejected:
                    assert rejected.code == 400
                assert sql("server", "SELECT count(*) FROM attentions") == [(1,)]

            with server(args.server, root, env, listen=address):
                # An ordinary old-server reply wakes the same assignment on the
                # process that is now receiving the new capability from sync.
                answer(question, "Resume the example after the upgrade.")
                delivered = chat_message("The original compatibility report.")
                assert delivered["text"].startswith("The Router could not relay this report.")
                assert delivered["reply_to"] == receipt["message_id"]
                accepted = chat_message("Accept the exact compatibility report?")
                [(dependency, original_report)] = sql(
                    "server", "SELECT attention_id, report_id FROM attention_report_dependencies")
                assert original_report == report
                assert sql("server", "SELECT json_extract(operation,'$.action.kind') FROM runner_operations "
                           "WHERE id=?", dependency) == [("ask_stage_attention",)]
                assert sql("server", "SELECT a.task_id, m.seq < a.question_seq FROM attentions a "
                           "JOIN chat_messages m ON m.id=? WHERE a.id=?", delivered["id"], dependency) == [
                               (task, 1)]
                answer(accepted, "Accept the bound report.")
                view = wait_for(lambda: cli("issue", "read", issue), lambda v: v["status"] == "completed")
                assert view["task_id"] == task and view["issue_state"] == "closed"
                chat_message("The bound report was accepted after upgrade.")

                # Reconcile original Runner records, including the report
                # accepted by the old Server and the new bound operation.
                for _ in range(2):
                    subprocess.run([*runner, "--reconcile", task], env=env, check=True, timeout=12)
                assert sql("server", "SELECT id, report_id FROM attentions a "
                           "JOIN attention_report_dependencies d ON a.id=d.attention_id") == [
                               (dependency, report)]
                assert sql("server", "SELECT message_id FROM issue_report_publications WHERE report_id=?",
                           report) == [(delivered["id"],)]
                assert sql("server", "SELECT id FROM tasks WHERE issue_id=?", issue) == [(task,)]
                assert sql("server", "SELECT count(*) FROM issue_inputs WHERE task_id=? "
                           "AND json_extract(payload,'$.kind') = 'attention_answered'", task) == [(2,)]
        except BaseException:
            print((root / "runner.log").read_text(), file=sys.stderr, end="")
            raise
        finally:
            if process.poll() is None:
                process.terminate()
                process.wait(7)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for binary in ("server", "cli", "runner", "legacy_server"):
        parser.add_argument(binary, type=lambda value: Path(value).resolve())
    with tempfile.TemporaryDirectory(prefix="tend-stage-compat-") as folder:
        exercise(parser.parse_args(), Path(folder))
    print("PASS legacy Server rejection, same-Task capability upgrade, late report fallback, and idempotent reconciliation")
