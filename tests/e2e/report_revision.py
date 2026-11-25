"""Backfill only explicit synthetic report identities on isolated Server/Runner processes."""

import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from http.client import HTTPConnection
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile

from chat import lose_first_response, server, wait_for
from child_windows import expire_child_window
from issue_chat import TESTS, WEB, free_port, prepare
from proposal_approval import REQUEST as PROPOSAL_REQUEST, REVISION

API = "/api/v1/report-revisions"
ISSUE = "ordinary-fixture"
CHAT = "report-fixture"
SOURCE = "showcase-request"
FIXTURE_TOKEN = "report-backfill-fixture"
OLD_MARKDOWN = """# 初版｜导出格式与边界

这是最初随报告交付的合成原件。新版收敛了字段范围，旧版保留作对照。

| 导出字段及允许的值 | 当前范围与默认行为 | 异常或缺失时的处理 | 供后续复核的合成样例 |
| --- | --- | --- | --- |
| 任务名称、完成时间、结果状态 | 仅导出当前筛选范围 | 没有数据时保留表头 | 不复制真实记录或私有链接 |

```text
summary.export.columns = ["task_name", "completed_at", "status", "original_run_reference"]
```

- 文件名使用稳定格式。
- 先用空结果和单行结果验收。
- 附件中的 HTML 只提供原件下载。
"""
FILES = {
    "old-md": ("export-notes-v1.md", OLD_MARKDOWN.encode()),
    "old-html": ("export-sketch-v1.html", b"<!doctype html><title>Synthetic export sketch</title>"),
    "new-md": ("export-notes-v2.md", b"# Synthetic current export notes\n"),
    "solo-file": ("field-checklist.txt", b"Synthetic notes for an independent report.\n"),
}


def seed(root):
    with sqlite3.connect(root / "server.sqlite") as db:
        db.execute("PRAGMA foreign_keys = ON")
        db.executemany(
            "INSERT INTO chat_messages (id, chat_id, text, operation_id) VALUES (?, ?, ?, ?)",
            [
                (SOURCE, CHAT, "请交付任务摘要导出说明，并保留原报告以便比较。", "showcase-send"),
                ("another-request", CHAT, "Synthetic separate topic", "other-send"),
                ("outside-request", "outside-fixture", "Synthetic separate Chat", "outside-send"),
            ],
        )
        source = db.execute("SELECT seq FROM chat_messages WHERE id = ?", (SOURCE,)).fetchone()[0]
        db.executemany(
            """INSERT INTO issues (id, source_seq, title, objective, state)
               SELECT ?, seq, ?, ?, 'closed' FROM chat_messages WHERE id = ?""",
            [
                (ISSUE, "任务摘要导出交付", "核对导出说明与示意稿，保留可追溯的历史版本。", SOURCE),
                ("sibling-fixture", "Separate Issue, same request", "", SOURCE),
                ("outside-fixture", "Separate Chat and Issue", "", "outside-request"),
            ],
        )
        db.executemany(
            """INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities)
               VALUES (?, ?, 'issue-router', 'completed', '[]', '[]')""",
            [("showcase-task", ISSUE), ("sibling-task", "sibling-fixture")],
        )
        dispatch = db.execute(
            """INSERT INTO router_inputs (chat_id, message_seq, outcome)
               VALUES (?, ?, 'completed') RETURNING seq""",
            (CHAT, source),
        ).fetchone()[0]
        db.execute("INSERT INTO router_attempts (seq, number, id, thread_id) VALUES (?, 1, 'showcase-attempt', 'showcase-thread')", (dispatch,))
        db.execute(
            """INSERT INTO router_dispatches (operation_id, issue_id, task_id, attempt_id, call_id, request)
               VALUES ('showcase-dispatch', ?, 'showcase-task', 'showcase-attempt', 'fixture-call', '{}')""",
            (ISSUE,),
        )
        db.execute("INSERT INTO runners (id, host_id, profile, last_seen) VALUES ('showcase-runner', 'local', '{}', 0)")

        def work(id, action):
            db.execute(
                "INSERT INTO runner_operations (id, runner_id, operation) VALUES (?, 'showcase-runner', ?)",
                (id, json.dumps({"operation_id": id, "assignment_id": "synthetic-assignment",
                                 "run_id": "synthetic-run", "action": action})),
            )

        reports = {
            "old": ["old-md", "old-html"],
            "new": ["new-md"],
            "solo": ["solo-file"],
            **{name: [] for name in ("next", "fork", "merge", "sibling", "outside",
                                     "wrong-input-chat", "wrong-input-seq", "candidate",
                                     "one-old", "one-new", "other-old", "other-new",
                                     "two-old", "two-new", "ambiguous")},
        }
        for name, files in reports.items():
            work(name, {"kind": "report_issue", "summary": f"Synthetic {name}", "artifact_ids": files})
            issue = {"sibling": "sibling-fixture", "outside": "outside-fixture"}.get(name, ISSUE)
            db.execute("INSERT INTO issue_reports VALUES (?, ?, ?)", (name, issue, f"Synthetic {name}"))

        work("declared-candidate", {"kind": "ask_attention", "proposal": {"report_id": "candidate"}})
        work("known-attention", {"kind": "ask_attention"})
        for id, (name, contents) in FILES.items():
            work(id, {"kind": "submit_artifact", "name": name, "bytes": len(contents)})
            db.execute("INSERT INTO artifacts VALUES (?, ?, 'showcase-task', 'showcase-runner', ?, ?, 1, 1)",
                       (id, ISSUE, name, len(contents)))
            (root / "server.artifacts" / id).write_bytes(contents)

        texts = {
            "old": "已回传**初版导出说明**和示意稿，记录字段草案与已知限制。",
            "new": "已回传**修订后的导出说明**。新版明确空结果行为，并保留原附件入口。",
            "solo": "这是独立的**字段核对记录**，不属于导出说明的版本链。",
        }
        for name, files in reports.items():
            chat, reply = ("outside-fixture", "outside-request") if name == "outside" else (CHAT, SOURCE)
            db.execute(
                "INSERT INTO chat_messages (id, chat_id, text, reply_to) VALUES (?, ?, ?, ?)",
                (f"msg-{name}", chat, texts.get(name, f"Synthetic reply for {name}"), reply),
            )
            for id in files:
                db.execute("INSERT INTO report_artifacts VALUES (?, ?)", (name, id))
                db.execute("INSERT INTO chat_artifacts VALUES (?, ?)", (f"msg-{name}", id))

        db.executemany(
            "INSERT INTO chat_messages (id, chat_id, text, reply_to, operation_id) VALUES (?, ?, 'Synthetic unrelated reply', ?, ?)",
            [
                ("wrong-chat", "outside-fixture", SOURCE, None),
                ("wrong-reply", CHAT, "another-request", None),
                ("not-a-reply", CHAT, None, None),
                ("user-reply", CHAT, SOURCE, "user-reply-operation"),
                ("attention-reply", CHAT, SOURCE, None),
                ("alternative", CHAT, SOURCE, None),
                ("ambiguous-second", CHAT, SOURCE, None),
            ],
        )
        db.execute(
            """INSERT INTO attentions (id, issue_id, task_id, source_seq, request, question_seq)
               SELECT 'known-attention', 'sibling-fixture', 'sibling-task', ?, '{"question":"Fixture question"}', seq
               FROM chat_messages WHERE id = 'attention-reply'""",
            (source,),
        )
        for name, chat, request in [
            ("old", CHAT, SOURCE), ("new", CHAT, SOURCE), ("solo", CHAT, SOURCE),
            ("wrong-input-chat", "outside-fixture", SOURCE),
            ("wrong-input-seq", CHAT, "another-request"),
        ]:
            db.execute(
                """INSERT INTO router_inputs (chat_id, message_seq, report_id, outcome)
                   SELECT ?, seq, ?, 'completed' FROM chat_messages WHERE id = ?""",
                (chat, name, request),
            )
        db.executemany(
            "INSERT INTO issue_report_publications VALUES (?, ?)",
            [
                ("msg-solo", "solo"),
                ("msg-one-old", "one-old"),
                ("msg-other-new", "other-new"),
                ("msg-two-old", "two-old"),
                ("msg-two-new", "two-new"),
                ("msg-ambiguous", "ambiguous"),
                ("ambiguous-second", "ambiguous"),
            ],
        )
        db.executemany(
            "INSERT INTO issue_messages (id, issue_id, kind, author, text) VALUES (?, ?, 'agent', 'Fixture Owner', ?)",
            [
                ("showcase-note", ISSUE, "已核对合成样例与附件来源。"),
                ("showcase-close", ISSUE, "交付已归档，原始报告可从 Chat 查看。"),
            ],
        )


def database_rows(root, database, tables):
    with sqlite3.connect(root / f"{database}.sqlite") as db:
        return {table: db.execute(f"SELECT * FROM {table} ORDER BY rowid").fetchall()
                for table in tables}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "cli", "runner"):
        parser.add_argument(name, type=lambda path: Path(path).resolve())
    parser.add_argument("--web", action="store_true")
    args = parser.parse_args()

    with tempfile.TemporaryDirectory(prefix="tend-report-backfill-") as directory:
        root = Path(directory)
        address = free_port()
        prepare(root, free_port(), "A fixed repository.\n")
        (root / "delegate-now").touch()
        config = json.loads((root / "runner.json").read_text())
        config.pop("inspect")
        config["agents"][0]["reasoning_effort"] = "low"
        (root / "runner.json").write_text(json.dumps(config))
        env = {**os.environ, "TEND_TOKEN": FIXTURE_TOKEN, "TEND_URL": f"http://{address}",
               "NO_PROXY": "127.0.0.1,localhost", "CODEX_HOME": str(root / "fixture-codex")}
        (root / "fixture-codex").mkdir()

        def cli(*words, at=address):
            result = subprocess.run(
                [args.cli, *words], text=True, capture_output=True, timeout=10,
                env={**env, "TEND_URL": f"http://{at}"},
            )
            assert result.returncode == 0, result.stderr
            return json.loads(result.stdout)

        def http(path, body=None, token=FIXTURE_TOKEN, method=None):
            with closing(HTTPConnection(address, timeout=8)) as connection:
                connection.request(method or ("GET" if body is None else "POST"), path,
                                   json.dumps(body) if body is not None else None,
                                   {"Authorization": f"Bearer {token}", "Content-Type": "application/json"})
                reply = connection.getresponse()
                data = reply.read()
                return reply.status, (json.loads(data) if "application/json" in
                                      reply.getheader("Content-Type", "") else data)

        def publications():
            return database_rows(root, "server", ["report_revisions", "report_revision_receipts",
                                                   "issue_report_publications"])

        def state():
            # Deterministic snapshots include all queues and lifecycle rows that
            # could change, with real waiting and stopped Runs in the fixture.
            return (
                database_rows(root, "server", ["chat_messages", "chat_artifacts", "issue_messages",
                              "issue_reports", "report_artifacts", "artifacts", "issues", "tasks",
                              "runner_operations", "runner_assignments", "task_runs", "runner_run_events",
                              "issue_inputs", "router_inputs", "router_attempts", "attentions",
                              "proposal_versions", "proposal_decisions"]),
                database_rows(root, "runner", ["received_assignments", "local_issue_inputs", "local_runs"]),
                database_rows(root, "app", ["turns", "start_requests"]),
            )

        def request(op, old="old", new="new", old_message="msg-old", new_message="msg-new"):
            body = {"operation_id": op, "old_report_id": old, "new_report_id": new}
            if old_message is not None:
                body["old_message_id"] = old_message
            if new_message is not None:
                body["new_message_id"] = new_message
            return body

        def reject(code, body, reason):
            before = publications()
            status, error = http(API, body)
            assert status == code and reason in error["error"], (status, error)
            assert publications() == before, (body, publications(), before)

        with server(args.server, root, env, listen=address, web_dir=WEB / "dist" if args.web else None):
            runner_log = root / "runner.log"
            with runner_log.open("w") as stderr:
                runner = subprocess.Popen([
                    args.runner, "--config", root / "runner.json", "--db", root / "runner.sqlite",
                    "--app-server", sys.executable, TESTS / "fixtures/coordination_app_server.py",
                    root, "proposal",
                ], env=env, stderr=stderr)
                try:
                    message = cli("chat", "send", "main", PROPOSAL_REQUEST)["message_id"]
                    [proposal_issue] = wait_for(
                        lambda: next((row.get("issue_ids") for row in cli("chat", "read", "main")["messages"]
                                      if row["id"] == message), None), bool,
                    )
                    task = cli("issue", "read", proposal_issue)["task_id"]
                    with sqlite3.connect(root / "server.sqlite") as db:
                        # The isolated fixture may release its child-result batch
                        # early without waiting for the five-minute window.
                        def child_inputs():
                            return db.execute("SELECT count(*) FROM issue_inputs WHERE task_id = ?", (task,)).fetchone()[0]
                        wait_for(child_inputs, lambda count: count == 4)
                    expire_child_window(root, "artifact_available", task=task)
                    with sqlite3.connect(root / "runner.sqlite") as db:
                        wait_for(lambda: db.execute(
                            """SELECT count(*) FROM local_issue_inputs i JOIN received_assignments a
                               ON a.id = i.assignment_id WHERE a.task_id = ? AND i.run_id IS NULL""",
                            (task,)).fetchone()[0], lambda count: count == 4)
                    (root / "finish-stage-child").touch()
                    proposal_path = f"/api/v1/issues/{proposal_issue}/overview"
                    def ready_version(version):
                        return wait_for(
                            lambda: http(proposal_path)[1],
                            lambda view: any(
                                p["status"]["version"] == version and p["status"]["can_approve"]
                                and p["versions"][0].get("message_id")
                                for p in view.get("proposals", [])
                            ),
                        )

                    ready_version(1)
                    wait_for(lambda: cli("issue", "read", proposal_issue), lambda row: row["status"] == "waiting")
                    cli("issue", "message", proposal_issue, REVISION)
                    proposal_view = ready_version(2)
                    wait_for(lambda: cli("issue", "read", proposal_issue), lambda row: row["status"] == "waiting")
                    proposal = proposal_view["proposals"][0]
                    candidate = proposal["versions"][0]
                    independent = proposal_view["reports"][0]

                    seed(root)
                    before = state()
                    assert before[0]["task_runs"] and before[1]["local_runs"] and before[1]["local_issue_inputs"]
                    assert http(API, request("unauthorized"), token="not-the-fixture-token")[0] == 401
                    assert http(API)[0] == 405
                    reject(400, request("bad id"), "invalid operation")
                    reject(400, request("self", new="old"), "cannot revise itself")
                    reject(404, request("absent-old", old="absent"), "old report not found")
                    reject(404, request("absent-new", new="absent"), "new report not found")
                    reject(400, request("missing-old-id", old_message=None), "old_message_id")
                    reject(400, request("missing-new-id", new_message=None), "new_message_id")
                    reject(404, request("absent-reply", new_message="absent"), "message not found")
                    for unrelated in ("sibling", "outside"):
                        reject(409, request(unrelated, new=unrelated, new_message=f"msg-{unrelated}"),
                               "same Issue and original Chat")
                    for wrong in ("wrong-chat", "wrong-reply", "not-a-reply", "user-reply", "attention-reply"):
                        reject(409, request(wrong, new_message=wrong), "not a Router reply")
                    for wrong in ("wrong-input-chat", "wrong-input-seq"):
                        reject(409, request(wrong, new=wrong, new_message=f"msg-{wrong}"), "Router input")
                    reject(409, request("duplicate-message", new_message="msg-old"), "share a publication")
                    reject(409, request("owned-message", new_message="msg-solo"), "belongs to another report")
                    reject(409, request("ambiguous-mapping", new="ambiguous", new_message="msg-ambiguous"), "ambiguous publication")
                    reject(400, request("candidate", new="candidate", new_message="msg-candidate"), "structured proposal")
                    reject(400, request("registered", old=independent["report_id"], new=candidate["report_id"],
                                        old_message=independent["message_id"], new_message=candidate["message_id"]),
                           "structured proposal")

                    # Discard the first response after the real Server commits.
                    with lose_first_response(address) as (proxy, posted):
                        saved = cli("report", "backfill", "old", "new", "lost",
                                    "--new-message", "msg-new", "--old-message", "msg-old", at=proxy)
                        assert posted == [(request("lost"), 200, saved)] * 2
                    assert saved == {
                        "operation_id": "lost", "issue_id": ISSUE, "old_report_id": "old",
                        "new_report_id": "new", "old_message_id": "msg-old",
                        "new_message_id": "msg-new", "created": True,
                    }
                    assert cli("report", "backfill", "old", "new", "lost") == saved
                    with ThreadPoolExecutor(4) as pool:
                        assert list(pool.map(lambda _: http(API, request("lost")), range(4))) == [(200, saved)] * 4
                    ledger = publications()
                    assert ledger["report_revisions"] == [("old", "new", "msg-old", "msg-new")]
                    assert ledger["report_revision_receipts"] == [("lost", "old", 1)]
                    assert sorted(row for row in ledger["issue_report_publications"]
                                  if row[1] in ("old", "new")) == [("msg-new", "new"), ("msg-old", "old")]
                    alias = cli("report", "backfill", "old", "new")
                    assert not alias["created"] and alias["operation_id"] != "lost"
                    assert cli("report", "backfill", "old", "new", alias["operation_id"]) == alias
                    reject(409, request("lost", new_message="alternative"), "operation ID")
                    reject(409, request("mapped-conflict", new_message="alternative"), "different or ambiguous")
                    reject(409, request("fork", new="fork", new_message="msg-fork"), "successor")
                    reject(409, request("merge", old="merge", old_message="msg-merge"), "predecessor")
                    reject(409, request("cycle", "new", "old", None, None), "cycle")

                    status, view = http(f"/api/v1/issues/{ISSUE}/overview")
                    assert status == 200
                    current, solo, *other = view["reports"]
                    assert current["report_id"] == "new" and current["message_id"] == "msg-new"
                    assert [a["id"] for a in current["artifacts"]] == ["new-md"]
                    [old] = current["history"]
                    assert old["report_id"] == "old" and old["message_id"] == "msg-old"
                    assert "初版导出说明" in old["text"]
                    assert [file["id"] for file in old["artifacts"]] == ["old-md", "old-html"]
                    assert old["artifacts"][0]["source"]["task_id"] == "showcase-task"
                    assert solo["report_id"] == "solo" and not solo.get("history")
                    # These independent text-only publications are also used by
                    # the backfill conflict cases. Show each once in Chat order.
                    assert [(r["report_id"], r["message_id"]) for r in other] == [
                        ("one-old", "msg-one-old"), ("other-new", "msg-other-new"),
                        ("two-old", "msg-two-old"), ("two-new", "msg-two-new"),
                        ("ambiguous", "ambiguous-second"),
                    ]
                    assert all(not r["artifacts"] and not r.get("history") for r in other)
                    assert cli("issue", "read", f"#{view['short_number']}")["reports"] == view["reports"]
                    assert cli("issue", "read", ISSUE)["reports"] == view["reports"]
                    assert http(f"/api/v1/artifacts/{old['artifacts'][0]['id']}") == (200, FILES["old-md"][1])
                    if args.web:
                        subprocess.run(["node", WEB / "tests/report-revision.mjs"], check=True, timeout=60,
                                       env={**env, "TEND_FIXTURE_ISSUE": str(view["short_number"])})

                    assert http(API, request("next", "new", "next", None, "msg-next"))[0] == 200
                    reject(409, request("long-cycle", "next", "old", None, None), "cycle")
                    reports = http(f"/api/v1/issues/{ISSUE}/overview")[1]["reports"]
                    current = next(r for r in reports if r["report_id"] == "next")
                    assert not current["artifacts"] and [r["report_id"] for r in current["history"]] == ["new", "old"]

                    for label, old_id, new_id, old_message, new_message in [
                        ("old-exists", "one-old", "one-new", None, "msg-one-new"),
                        ("new-exists", "other-old", "other-new", "msg-other-old", None),
                        ("both-exist", "two-old", "two-new", "msg-two-old", None),
                    ]:
                        status, result = http(API, request(label, old_id, new_id, old_message, new_message))
                        assert status == 200 and result["created"], (status, result)
                        assert result["old_message_id"] == f"msg-{old_id}" and result["new_message_id"] == f"msg-{new_id}"
                    assert state() == before
                    proposal_after = http(proposal_path)[1]
                    assert proposal_after["proposals"] == proposal_view["proposals"]
                    assert proposal_after["reports"] == proposal_view["reports"]
                    approval_path = f"/api/v1/chats/main/attentions/{proposal['attention_id']}/approve"
                    status, approved = http(approval_path, {"version": 1})
                    assert status == 409
                    status, approved = http(approval_path, {"version": 2})
                    assert status == 200 and approved["version"] == 2
                    wait_for(lambda: (root / "continuation-prepared").is_file(), bool)
                    (root / "start-continuation").touch()
                    wait_for(lambda: (root / "decision.json").is_file(), bool)
                    assert json.loads((root / "decision.json").read_text())["input_id"] == approved["id"]
                    (root / "finish-continuation").touch()
                    continued = wait_for(lambda: cli("issue", "read", proposal_issue),
                                         lambda row: row["status"] == "completed")
                    assert continued["task_id"] == task and continued["issue_state"] == "closed"
                except BaseException:
                    print(runner_log.read_text(), file=sys.stderr)
                    raise
                finally:
                    runner.terminate()
                    try:
                        runner.wait(timeout=7)
                    except subprocess.TimeoutExpired:
                        runner.kill()
                        runner.wait(timeout=5)
    print("PASS explicit backfill, conflicts/atomicity, lost/concurrent retries, no delivery or lifecycle side effects, original links and proposal approval")


if __name__ == "__main__":
    main()
