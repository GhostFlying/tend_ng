"""Use real Server, CLI and Runner processes to verify version-bound proposal decisions."""
import argparse
from concurrent.futures import ThreadPoolExecutor
from http.client import HTTPConnection, RemoteDisconnected
import json
import os
from pathlib import Path
import signal
import sqlite3
import subprocess
import sys
import tempfile

from chat import lose_first_response, seed_archived_input, server, wait_for
from child_windows import expire_child_window
from issue_chat import TESTS, WEB, free_port, prepare

REQUEST = "请设计任务摘要导出，等我批准对应版本后再实现。"
REVISION = "请把空结果行为补进同一方案。"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "cli", "runner"):
        parser.add_argument(name, type=lambda path: Path(path).resolve())
    parser.add_argument("--old-runner", type=lambda path: Path(path).resolve())
    parser.add_argument("--old-cli", type=lambda path: Path(path).resolve())
    parser.add_argument("--web", action="store_true")
    scenario = parser.add_mutually_exclusive_group()
    scenario.add_argument("--ambiguous", action="store_true")
    scenario.add_argument("--prestart-failure", action="store_true")
    scenario.add_argument("--stop-before-delivery", action="store_true")
    parser.add_argument("--archived-origin", action="store_true")
    scenario.add_argument("--multiple-proposals", action="store_true")
    args = parser.parse_args()
    if args.web and args.archived_origin != args.multiple_proposals:
        parser.error("--web runs either the standard main case or the multiple-proposal archive case")
    if args.archived_origin and (args.ambiguous or args.prestart_failure or args.stop_before_delivery):
        parser.error("--archived-origin runs either the standard case or --multiple-proposals")
    chat = "legacy-proposal" if args.archived_origin else "main"

    with tempfile.TemporaryDirectory(prefix="tend-proposal-") as directory:
        root = Path(directory)
        address = free_port()
        prepare(root, free_port(), "A fixed repository.\n")
        config = json.loads((root / "runner.json").read_text())
        config.pop("inspect")
        config["workspace_root"] = str(root / "runner.runs")
        config["agents"][0]["reasoning_effort"] = "low"
        (root / "runner.json").write_text(json.dumps(config))
        (root / "delegate-now").touch()
        if args.ambiguous:
            (root / "independent-decision").touch()
        if args.multiple_proposals:
            (root / "two-proposals").touch()
        if args.prestart_failure:
            (root / "fail-before-native").touch()
        env = {**os.environ, "CODEX_HOME": str(root / "fixture-codex"), "TEND_URL": f"http://{address}", "TEND_TOKEN": "proposal-fixture"}

        def cli(*words, binary=args.cli, override=None):
            return subprocess.run([binary, *words], env=override or env, timeout=8,
                                  text=True, capture_output=True)

        def read(*words, binary=args.cli):
            result = cli(*words, binary=binary)
            assert result.returncode == 0, result.stderr
            return json.loads(result.stdout)

        def http(path, body=None, token=env["TEND_TOKEN"], target=address):
            with_connection = HTTPConnection(target, timeout=8)
            try:
                with_connection.request("GET" if body is None else "POST", path,
                                        json.dumps(body) if body is not None else None,
                                        {"Authorization": f"Bearer {token}",
                                         "Content-Type": "application/json"})
                response = with_connection.getresponse()
                return response.status, json.loads(response.read())
            finally:
                with_connection.close()

        def sql(db, statement, *values):
            with sqlite3.connect(root / f"{db}.sqlite") as connection:
                return connection.execute(statement, values).fetchall()

        def start_runner(binary, name):
            return subprocess.Popen([
                binary, "--config", root / f"{name}.json", "--db", root / f"{name}.sqlite",
                "--app-server", sys.executable, TESTS / "fixtures/coordination_app_server.py",
                root, "proposal",
            ], env=env)

        daemons = []
        with server(args.server, root, env, listen=address, web_dir=WEB / "dist" if args.web else None):
            try:
                runner = start_runner(args.runner, "runner")
                daemons.append(runner)
                source = (seed_archived_input(root, chat, REQUEST) if args.archived_origin
                          else read("chat", "send", chat, REQUEST))["message_id"]
                [request] = wait_for(
                    lambda: [message for message in read("chat", "read", chat)["messages"]
                             if message["id"] == source and message.get("issue_ids")], bool)
                [issue] = request["issue_ids"]
                task = read("issue", "read", issue)["task_id"]
                path = f"/api/v1/issues/{issue}"

                wait_for(lambda: sql("server", "SELECT count(*) FROM issue_inputs WHERE task_id = ?", task),
                         lambda rows: rows == [(4,)])
                expire_child_window(root, "artifact_available", task=task)
                wait_for(lambda: sql("runner", "SELECT count(*) FROM local_issue_inputs i "
                                     "JOIN received_assignments a ON a.id = i.assignment_id "
                                     "WHERE a.task_id = ? AND i.run_id IS NULL", task),
                         lambda rows: rows == [(4,)])
                (root / "finish-stage-child").touch()

                def overview():
                    status, view = http(f"{path}/overview")
                    assert status == 200, view
                    return view

                initial = wait_for(lambda: next(iter(overview().get("proposals", [])), {}),
                                   lambda value: value.get("status", {}).get("can_approve"))
                approval_id = initial["attention_id"]

                def proposal():
                    return next(p for p in overview()["proposals"] if p["attention_id"] == approval_id)
                assert [v["version"] for v in initial["versions"]] == [1]
                question = initial["question_message_id"]
                initial_op = json.loads((root / "proposal-v1.json").read_text())
                assert approval_id == initial_op["id"] and initial_op["task_id"] == task
                wait_for(lambda: read("issue", "read", issue), lambda result: result["status"] == "waiting")

                read("issue", "message", issue, REVISION)
                current = wait_for(proposal, lambda value: value.get("status", {}).get("version") == 2)
                assert current["attention_id"] == approval_id and current["question_message_id"] == question
                assert [v["version"] for v in current["versions"]] == [2, 1]
                assert current["versions"][1]["report_id"] == initial_op["report_id"]
                assert current["versions"][0]["report_id"] != initial_op["report_id"]
                wait_for(lambda: read("issue", "read", issue), lambda result: result["status"] == "waiting")
                wait_for(lambda: sql("server", "SELECT count(*) FROM router_inputs WHERE outcome IS NULL"),
                         lambda rows: rows == [(0,)])
                if args.multiple_proposals:
                    view = wait_for(overview, lambda view: len(view["proposals"]) == 2
                                    and all(p["status"]["can_approve"] for p in view["proposals"]))
                    second = next(p for p in view["proposals"] if p["attention_id"] != approval_id)
                    second_id = second["attention_id"]
                    assert second["status"]["version"] == 1 and current["status"]["version"] == 2
                    assert view.get("source_chat_archived", False) == args.archived_origin
                    assert view.get("proposal_requires_chat", False) != args.archived_origin
                    routes = {id: f"/api/v1/chats/{chat}/attentions/{id}/approve"
                              for id in (approval_id, second_id)}
                    chosen = {approval_id: 2, second_id: 1}
                    tables = ("chat_messages", "router_inputs", "tasks", "issue_messages")
                    before = [sql("server", f"SELECT * FROM {table} ORDER BY rowid") for table in tables]
                    runner.send_signal(signal.SIGSTOP)
                    try:
                        for id, version in chosen.items():
                            denied = {"version": version, **({} if args.archived_origin else {"from_issue": True})}
                            assert http(routes[id], denied)[0] == (403 if args.archived_origin else 409)
                            assert http(f"{path}/attentions/{id}/answer",
                                        {"operation_id": f"not-approval-{version}", "text": "批准"})[0] == 404
                        assert http(routes[approval_id], {"version": 1, "from_issue": args.archived_origin})[0] == 409
                        assert sql("server", "SELECT count(*) FROM proposal_decisions") == [(0,)]

                        options = ["--from-issue"] if args.archived_origin else []
                        if args.web:
                            subprocess.run(["node", WEB / "tests/proposal-archive.mjs"], check=True, timeout=35,
                                           env={**env, "TEND_ISSUE": issue, "TEND_SECOND_ATTENTION": second_id})
                        separate = read("attention", "approve", chat, second_id, "v1", *options)
                        assert "decision" not in proposal()["status"]
                        revised = read("attention", "approve", chat, approval_id, "v2", *options)
                        assert revised["id"] != separate["id"]
                        for id, version, receipt in ((approval_id, 2, revised), (second_id, 1, separate)):
                            assert http(routes[id], {"version": version}) == (200, receipt)
                        assert [sql("server", f"SELECT * FROM {table} ORDER BY rowid") for table in tables] == before
                        assert sql("server", """SELECT d.attention_id, d.version, x.task_id, x.id
                                   FROM proposal_decisions d JOIN issue_inputs x ON x.id = d.input_id
                                   ORDER BY d.rowid""") == [
                            (second_id, 1, task, separate["id"]), (approval_id, 2, task, revised["id"])]
                    finally:
                        runner.send_signal(signal.SIGCONT)
                    (root / "start-continuation").touch()
                    ledger = root / "both-decisions.jsonl"
                    consumed = wait_for(lambda: ledger.read_text().splitlines() if ledger.exists() else [],
                                        lambda rows: len(rows) == 2)
                    assert {json.loads(row)["attention_id"] for row in consumed} == set(chosen)
                    wait_for(overview, lambda view: all(
                        p["status"]["decision"].get("continuation_run_id") for p in view["proposals"]))
                    assert read("issue", "read", issue)["task_id"] == task
                    print(f"PASS two exact proposal decisions and original Task takeover (source {chat})")
                    return
                if args.ambiguous:
                    view = http(f"{path}/overview")[1]
                    assert view["proposal_requires_chat"]
                    [legacy] = view["decisions"]
                    assert legacy["text"] == "示例发布说明要使用简体中文还是英文？"
                    records = read("attention", "list", chat)
                    assert len(records) == 2
                    assert all(record["state"] == "awaiting_user" for record in records)
                    [plain] = [record for record in records if "proposal" not in record]
                    [structured] = [record for record in records if "proposal" in record]
                    assert plain["question_message_id"] == legacy["message_id"]
                    assert structured["id"] == approval_id and structured["proposal"]["can_approve"]
                    stale = f"/api/v1/chats/main/attentions/{approval_id}/approve"
                    assert http(stale, {"version": 2, "from_issue": True})[0] == 409
                    assert http(stale.replace(approval_id, plain["id"]), {"version": 2})[0] == 404
                    assert sql("server", "SELECT answer_input_id FROM attentions") == [(None,), (None,)]
                    assert sql("runner", "SELECT execution_state FROM received_assignments WHERE task_id = ?", task) == [("waiting",)]
                    print("PASS ambiguous inline action falls back to two intact decisions in original Chat")
                    return

                assert sql("server", "SELECT count(*) FROM attentions") == [(1,)]
                assert sql("runner", "SELECT execution_state FROM received_assignments WHERE task_id = ?", task) == [("waiting",)]
                assert len(http(f"{path}/overview")[1]["reports"]) == 1

                def facts():
                    return [sql("server", f"SELECT count(*) FROM {table}")
                            for table in ("chat_messages", "router_inputs", "issues", "tasks")]

                if args.web:
                    # Move the original question outside the latest page without
                    # queuing unrelated Router turns in this fixture.
                    with sqlite3.connect(root / "server.sqlite") as db:
                        db.executemany("INSERT INTO chat_messages(id, chat_id, text) VALUES (?, 'main', ?)",
                                       [(f"later-fixture-{n}", f"Synthetic history {n}") for n in range(55)])
                before = facts()
                approve = f"/api/v1/chats/{chat}/attentions/{approval_id}/approve"
                assert http(approve, {"version": 1})[0] == 409
                assert http(approve, {"version": 2}, token="wrong")[0] == 401
                assert http(approve.replace(f"/{chat}/", "/different/"), {"version": 2})[0] == 404
                assert http(f"{path}/attentions/{approval_id}/answer",
                            {"operation_id": "fixture-not-an-approval", "text": "批准 v2"})[0] == 404
                approval = {"version": 2}
                if args.archived_origin:
                    code, error = http(approve, approval)
                    assert code == 403 and "archived and read-only" in error["error"]
                    approval = {**approval, "from_issue": True}
                assert sql("server", "SELECT count(*) FROM proposal_decisions") == [(0,)]
                assert sql("server", "SELECT count(*) FROM issue_attention_replies") == [(0,)]

                if args.stop_before_delivery:
                    runner.terminate()
                    runner.wait(timeout=8)
                    code, saved = http(approve, {"version": 2})
                    assert code == 200 and "continuation_run_id" not in saved
                    read("task", "stop", task, "stop-original")
                    restarted = start_runner(args.runner, "runner")
                    daemons.append(restarted)
                    wait_for(lambda: read("control", "read", "stop-original"),
                             lambda view: view["state"] == "stopped")
                    restarted.terminate()
                    restarted.wait(timeout=8)
                    read("issue", "message", issue, "继续处理其他补充记录。")
                    assert read("issue", "read", issue)["task_id"] != task
                    assert sql("server", "SELECT task_id, run_id FROM issue_inputs WHERE id = ?", saved["id"]) == [(task, None)]
                    retry = read("attention", "approve", chat, approval_id, "2")
                    assert retry == {**saved, "continuation_blocked": True}
                    assert not (root / "continuation-prepared").exists()
                    print("PASS stopped original Task never transfers approval to follow-up Task")
                    return

                active_db = "runner"
                if args.old_runner:
                    active_db = "next"
                    (root / "next.json").write_text(json.dumps({**config, "runner_id": "next-runner", "standby": True}))
                    daemons.append(start_runner(args.old_runner, "next"))
                    wait_for(lambda: read("runner", "list"), lambda records: len(records) == 2)
                    read("runner", "replace", "demo-runner", "next-runner")
                    moved = wait_for(lambda: read("issue", "read", issue),
                                     lambda value: value["status"] == "waiting" and
                                     value["placement"]["runner_id"] == "next-runner")
                    assert moved["task_id"] == task
                    assert runner.wait(timeout=10) == 0
                    proof = sql("server", "SELECT proof FROM runner_handoff_proofs WHERE json_extract(proof, '$.task_id') = ?", task)
                    [operation] = [op for op in json.loads(proof[0][0])["context"]["operations"]
                                   if op["action"]["kind"] == "ask_attention"]
                    assert operation["operation_id"] == approval_id and "proposal" not in operation["action"]

                if args.web:
                    subprocess.run(["node", WEB / "tests/proposal-approval.mjs"], cwd=WEB,
                                   env={**env, "TEND_ISSUE": issue, "TEND_QUESTION_ID": question}, check=True)
                    stored = proposal()["status"]["decision"]
                else:
                    with lose_first_response(address) as (proxy, posts):
                        with ThreadPoolExecutor(max_workers=6) as pool:
                            lost = pool.submit(http, approve, approval, target=proxy)
                            first = list(pool.map(lambda _: http(approve, approval), range(6)))
                        assert isinstance(lost.exception(), RemoteDisconnected), lost.exception()
                        assert len(posts) == 1 and posts[0][1] == 200, posts
                        stored = posts[0][2]
                        assert all(code == 200 and receipt == stored for code, receipt in first)
                with ThreadPoolExecutor(max_workers=6) as pool:
                    receipts = list(pool.map(lambda _: http(approve, {"version": 2}), range(12)))
                assert all(code == 200 and receipt == stored for code, receipt in receipts)
                assert read("attention", "approve", chat, approval_id, "v2") == stored
                if args.archived_origin:
                    print("PASS archived Chat rejects new direct approval; Issue approval and exact receipt replay continue")
                assert http(approve, {"version": 1})[0] == 409
                assert facts() == before
                assert sql("server", "SELECT task_id, id FROM issue_inputs "
                           "WHERE json_extract(payload, '$.kind') = 'attention_answered'") == [(task, stored["id"])]
                assert sql("server", "SELECT answer_seq FROM attentions") == [(None,)]
                wait_for(lambda: (root / "continuation-prepared").exists(), bool)
                assert "continuation_run_id" not in proposal()["status"]["decision"]
                [event] = http(f"{path}/messages")[1]["approvals"]
                assert event["decision"] == stored and event["issue_id"] == issue
                read_status, original = http(approve.removesuffix("/approve"))
                assert read_status == 200 and original["question_message_id"] == question
                assert original["proposal"]["decision"] == stored
                if not args.web:
                    [posted] = [m for m in read("chat", "read", chat)["messages"] if m["id"] == question]
                    assert posted["attention"]["proposal"]["decision"] == stored
                if args.old_cli:
                    [attention] = read("attention", "list", chat, binary=args.old_cli)
                    assert attention["id"] == approval_id and attention["state"] == "answered" and "proposal" not in attention
                    assert "messages" in read("issue", "messages", issue, binary=args.old_cli)

                (root / "start-continuation").touch()
                if args.prestart_failure:
                    blocked = wait_for(lambda: proposal()["status"]["decision"],
                                       lambda value: value.get("continuation_blocked"))
                    assert blocked == {**stored, "continuation_blocked": True}
                    [event] = http(f"{path}/messages")[1]["approvals"]
                    assert event["decision"] == blocked
                    assert not (root / "decision.json").exists()
                    assert facts() == before
                    print("PASS failed prestart is visible and never counted as a taking-over Run")
                    return
                decision = wait_for(lambda: proposal()["status"]["decision"],
                                    lambda value: "continuation_run_id" in value)
                received = wait_for(lambda: (root / "decision.json").read_text()
                                    if (root / "decision.json").exists() else "", bool)
                received = json.loads(received)
                assert decision == {**stored, "continuation_run_id": received["run_id"]}
                assert received["input_id"] == stored["id"]
                [event] = http(f"{path}/messages")[1]["approvals"]
                assert event["before_message_id"] == received["run_id"]
                assert sql(active_db, "SELECT id FROM local_issue_inputs "
                           "WHERE json_extract(payload, '$.kind') = 'attention_answered'") == [(stored["id"],)]
                (root / "finish-continuation").touch()
                finished = wait_for(lambda: read("issue", "read", issue), lambda view: view["status"] == "completed")
                assert finished["task_id"] == task
                assert facts() == before
                print("PASS exact proposal revision, stale click, duplicate and lost receipts, original Task takeover")
            finally:
                for process in reversed(daemons):
                    if process.poll() is None:
                        process.terminate()
                        process.wait(timeout=8)


if __name__ == "__main__":
    main()
