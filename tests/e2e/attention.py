"""Exercise durable decisions on the original Work Agent or Issue Agent Task."""
import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import sys
import tempfile

from chat import server, wait_for

RUNNER_FIXTURE = Path(__file__).resolve().parents[1] / "fixtures" / "coordination_app_server.py"
WEB_ROOT = Path(__file__).resolve().parents[2] / "web"
REQUEST = "请为示例仓库整理使用说明。需要我决定时就在这里问。"
QUESTION = "使用说明你想要精简版，还是完整版？"
REPLY = "用精简版。"
STAGE_REQUEST = "请发一份使用说明和附件，然后原地等我确认是否结案。"
STAGE_REPORT = "使用说明已整理好，附上初稿供审阅。"
STAGE_QUESTION = "请审阅附上的使用说明。确认结案，还是需要修改？"


def stage_delivery(root, read, cli, sql):
    source = read("chat", "send", "main", STAGE_REQUEST)["message_id"]
    messages = wait_for(lambda: read("chat", "read", "main")["messages"],
                        lambda rows: any(row["id"] == source and row.get("issue_ids") for row in rows))
    [request] = [row for row in messages if row["id"] == source]
    [parent] = request["issue_ids"]
    task = read("issue", "read", parent)["task_id"]
    # Let the child finish only when both inputs can be consumed by one parent Run.
    wait_for(lambda: sql("runner", "SELECT json_extract(i.payload, '$.kind') FROM local_issue_inputs i "
                                   "JOIN received_assignments a ON i.assignment_id = a.id "
                                   "WHERE a.task_id = ? AND i.run_id IS NULL ORDER BY 1", task),
             lambda rows: rows == [("artifact_available",), ("issue_closed",)])
    (root / "finish-stage-child").touch()

    page = wait_for(lambda: read("chat", "read", "main")["messages"],
                    lambda rows: any(row.get("attention") for row in rows)
                    and any(row["text"] == STAGE_REPORT for row in rows))
    waiting = wait_for(lambda: read("issue", "read", parent), lambda view: view["status"] == "waiting"
                       and view["run"]["phase"].get("waiting_for_attention"))
    [report] = [row for row in page if row["text"] == STAGE_REPORT]
    [question] = [row for row in page if row.get("attention")]
    stage = json.loads((root / "stage-delivery.json").read_text())
    assert stage["task_id"] == task
    assert report["author"] == question["author"] == "router"
    assert report["reply_to"] == question["reply_to"] == source
    assert report["artifacts"] == [stage["file"]]
    downloaded = cli("artifact", "download", stage["file"]["id"])
    assert downloaded.returncode == 0 and downloaded.stdout == b"A fixture guide.\n"
    assert sql("server", "SELECT id, summary FROM issue_reports WHERE issue_id = ?", parent) == [
        (stage["report_id"], STAGE_REPORT),
    ]
    assert question["text"] == STAGE_QUESTION
    assert question["attention"] == {"id": stage["attention_id"], "state": "awaiting_user"}
    [attention] = read("attention", "list", "main")
    assert attention["issue_id"] == parent and attention["task_id"] == task
    assert attention["source_message_id"] == source and attention["question_message_id"] == question["id"]
    assert attention["request"]["options"] == ["确认结案", "需要修改"]

    assert waiting["issue_state"] == "open" and waiting["task_id"] == task
    assert waiting["run"]["run_id"] == stage["run_id"] and waiting["run"]["phase"]["state"] == "stopped"
    assert sql("runner", "SELECT state FROM local_runs WHERE id = ?", stage["run_id"]) == [("stopped",)]
    assert sql("runner", "SELECT execution_state FROM received_assignments WHERE task_id = ?", task) == [("waiting",)]
    assert sql("runner", "SELECT id FROM local_runs WHERE state IN ('prepared', 'running')") == []
    previous_runs = sql("server", "SELECT id FROM task_runs WHERE task_id = ? ORDER BY rowid", task)
    assert len(previous_runs) == 2 and previous_runs[-1] == (stage["run_id"],)
    assert not (root / "decision.json").exists()

    user = read("chat", "send", "main", "确认结案。", "stage-confirmation", "--reply-to", question["id"])
    finished = wait_for(lambda: read("issue", "read", parent), lambda view: view["status"] == "completed")
    assert finished["issue_state"] == "closed" and finished["task_id"] == task
    resumed = finished["run"]["run_id"]
    decision = json.loads((root / "decision.json").read_text())
    assert decision["kind"] == "attention_answered" and decision["attention_id"] == stage["attention_id"]
    assert decision["run_id"] == resumed and decision["user_message_id"] == user["message_id"]
    assert decision["answer"] == "确认结案"
    assert sql("server", "SELECT id FROM task_runs WHERE task_id = ? ORDER BY rowid", task) == (
        previous_runs + [(resumed,)]
    )
    [delivered] = read("attention", "list", "main")
    assert delivered["state"] == "delivered" and delivered["user_message_id"] == user["message_id"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("server", "cli", "runner"):
        parser.add_argument(executable, type=lambda value: Path(value).resolve())
    route = parser.add_mutually_exclusive_group()
    route.add_argument("--owner", action="store_true", help="answer in the parent Issue")
    route.add_argument("--implicit", action="store_true", help="answer in Chat without an explicit quote")
    route.add_argument("--web", action="store_true")
    route.add_argument("--stage-delivery", action="store_true", help="deliver a report with an artifact and await review")
    args = parser.parse_args()
    env = {**os.environ, "TEND_TOKEN": "attention-fixture"}
    scenario = "stage-delivery" if args.stage_delivery else "attention-local" if args.owner else "attention"

    with tempfile.TemporaryDirectory(prefix="tend-attention-") as temporary:
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
            "host_id": "attention-host", "runner_id": "attention-runner", "slot_limit": 1,
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
        connected = {**env, "TEND_URL": f"http://{address}"}
        web_dir = WEB_ROOT / "dist" if args.web else None

        def cli(*words):
            return subprocess.run([args.cli, *words], capture_output=True, timeout=8,
                                  env=connected)

        def read(*words):
            result = cli(*words)
            assert result.returncode == 0, result.stderr.decode()
            return json.loads(result.stdout)

        def sql(db, statement, *values):
            with sqlite3.connect(root / f"{db}.sqlite") as connection:
                return connection.execute(statement, values).fetchall()

        @contextmanager
        def runner():
            with (root / "runner.stderr").open("w") as stderr:
                process = subprocess.Popen(
                    [args.runner, "--config", root / "runner.json", "--db", root / "runner.sqlite",
                     "--app-server", sys.executable, RUNNER_FIXTURE, root,
                     scenario],
                    env=connected, stderr=stderr,
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
        with runner():
            with server(args.server, root, env, listen=address, web_dir=web_dir):
                if args.stage_delivery:
                    stage_delivery(root, read, cli, sql)
                    print("PASS stage report and artifact reach the original Chat, and the same Task resumes to close")
                    return
                source_message = read("chat", "send", "main", REQUEST)["message_id"]
                messages = wait_for(lambda: read("chat", "read", "main")["messages"],
                                    lambda rows: len(rows) == 2)
                [parent] = messages[0]["issue_ids"]
                wait_for(lambda: (root / "delegation.json").exists(), bool)
                child = json.loads((root / "delegation.json").read_text())["action"]["issue_id"]
                wait_for(lambda: sql("server", "SELECT id FROM issues WHERE id = ?", child), bool)
                initial = wait_for(lambda: read("issue", "read", child),
                                   lambda issue: issue["status"] == "running")
                [card] = read("issue", "list", "--scope=roots")["issues"]
                assert card["id"] == parent and card["stage"] == "in_progress" and card["running"] > 0
                placement = initial["placement"]
                assert {
                    "runner_id": placement["runner_id"],
                    "agent_id": placement["agent"]["id"],
                    "revision": placement["agent"]["revision"],
                    "running": 1,
                } in read("agent", "list")["activity"]
            (root / "ask-now").touch()
            wait_for(lambda: sql("runner", "SELECT execution_state FROM received_assignments WHERE task_id = ?",
                                 initial["task_id"]), lambda rows: rows == [("waiting",)])
            fact = json.loads((root / "question.json").read_text())["receipt"]
            original_attention = fact["operation_id"]
            assert sql("runner", "SELECT count(*) FROM local_work_operations WHERE id = ?", original_attention) == [(1,)]

            with server(args.server, root, env, listen=address, web_dir=web_dir):
                if not args.owner:
                    rows = wait_for(lambda: read("chat", "read", "main")["messages"],
                                    lambda rows: any(row.get("attention") for row in rows))
                    [question] = [row for row in rows if row.get("attention")]
                    assert question["text"] == QUESTION and question["reply_to"] == source_message
                    assert question["attention"] == {"id": original_attention, "state": "awaiting_user"}
                    [pending] = read("attention", "list", "main")
                    assert pending["task_id"] == initial["task_id"] and pending["question_message_id"] == question["id"]
                    [card] = read("issue", "list", "--scope=roots")["issues"]
                    assert card["attention"] == {
                        "chat_id": "main", "question_message_id": question["id"],
                    }
                    waiting = read("issue", "read", child)
                    assert waiting["status"] == "waiting" and waiting["run"]["phase"]["waiting_for_attention"]
                    assert cli("chat", "send", "other", REPLY, "--reply-to", question["id"]).returncode != 0

                    read("chat", "send", "main", "我稍后再决定，可以先继续聊吗？")
                    wait_for(lambda: read("chat", "read", "main")["messages"],
                             lambda rows: any(row["text"] == "可以。你准备好时，回复上面的问题就行。" for row in rows))
                    if args.web:
                        subprocess.run(["node", WEB_ROOT / "tests/attention.mjs"], env=connected,
                                       check=True, timeout=40)
                        matches = [row for row in read("chat", "read", "main")["messages"] if row["text"] == REPLY]
                        [reply] = matches
                        user_message = reply["id"]
                        assert reply["reply_to"] == question["id"]
                    else:
                        words = ("chat", "send", "main", REPLY, "fixture-decision")
                        if not args.implicit:
                            words += ("--reply-to", question["id"])
                        user_message = read(*words)["message_id"]
                        assert read(*words) == {"message_id": user_message}

                finished = wait_for(lambda: read("issue", "read", child),
                                    lambda issue: issue["status"] == "completed")
                assert finished["issue_state"] == "closed"
                assert finished["task_id"] == initial["task_id"]
                assert finished["run"]["run_id"] != initial["run"]["run_id"]
                wait_for(lambda: read("issue", "read", parent)["status"], lambda status: status == "completed")
                assert (root / "runner.runs" / "workspaces" / parent
                        / "repos" / "example" / "guide.txt").read_text() == "Compact guide selected.\n"
                decision = json.loads((root / "decision.json").read_text())
                assert decision["attention_id"] == original_attention
                assert decision["answer"] == "精简版"
                assert sql("server", "SELECT count(*) FROM issue_inputs WHERE id = ?", decision["input_id"]) == [(1,)]
                assert sql("server", "SELECT count(*) FROM task_runs WHERE task_id = ?", initial["task_id"]) == [(2,)]
                if args.owner:
                    assert read("attention", "list", "main") == []
                    assert sql("server", "SELECT answer_seq FROM attentions") == [(None,)]
                else:
                    [delivered] = read("attention", "list", "main")
                    assert delivered["state"] == "delivered" and delivered["user_message_id"] == user_message
                    assert decision["user_text"] == REPLY and decision["user_message_id"] == user_message
                    [posted] = [row for row in read("chat", "read", "main")["messages"] if row.get("attention")]
                    assert posted["attention"] == {"id": original_attention, "state": "delivered"}
                    assert sql("server", "SELECT count(*) FROM router_attempts "
                                         "WHERE seq IN (SELECT seq FROM router_inputs WHERE attention_id IS NOT NULL)") == [(2,)]
    print("PASS offline Attention reaches its original Task after a durable decision")


if __name__ == "__main__":
    main()
