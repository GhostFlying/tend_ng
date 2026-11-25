"""Answer independently bound Attention IDs using the real local Server and Runner."""
import argparse
from concurrent.futures import ThreadPoolExecutor
from http.client import HTTPConnection, RemoteDisconnected
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile

from chat import lose_first_response, server, wait_for
from issue_chat import TESTS, WEB, free_port, prepare

REQUEST = "请为示例指南整理两个独立问题。"
NOTE = "普通留言：请补充示例命令。"
FIRST = "  用精简版。\n保留 **原文**。  "
SECOND = "使用简体中文，并保留命令原名。"


def seed_history(root, source, parent, child, task, run, numbers=range(1, 65), *, large=True):
    with sqlite3.connect(root / "server.sqlite") as db:
        source_seq, = db.execute("SELECT seq FROM chat_messages WHERE id = ?", (source,)).fetchone()
        for n in numbers:
            attention = f"historical-fixture-{n}"
            input_id = f"history-input-{n}"
            question_seq = db.execute("INSERT INTO chat_messages (id, chat_id, text) VALUES (?, 'main', ?)",
                                      (f"history-question-{n}", f"历史问题 {n}")).lastrowid
            db.execute("INSERT INTO runner_operations (id, runner_id, operation) VALUES (?, 'demo-runner', '{}')",
                       (attention,))
            db.execute("INSERT INTO issue_inputs (id, task_id, payload, run_id) VALUES (?, ?, '{}', ?)",
                       (input_id, task, run))
            db.execute("INSERT INTO attentions (id, issue_id, task_id, source_seq, request, question_seq, "
                       "answer_input_id, answer) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                       (attention, child, task, source_seq, json.dumps({"question": f"历史问题 {n}"}),
                        question_seq, input_id, "字" * 5462 if large else "新增的合成答复。"))
            db.execute("INSERT INTO issue_attention_replies (operation_id, scope_issue_id, owner_issue_id, attention_id, input_id) "
                       "VALUES (?, ?, ?, ?, ?)", (f"history-operation-{n}", parent, child, attention, input_id))
        if large:
            db.executemany("INSERT INTO chat_messages (id, chat_id, text) VALUES (?, 'main', ?)",
                           [(f"later-fixture-{i}", f"合成的后续消息 {i}") for i in range(55)])


def append_history_fixture(root, first, last):
    with sqlite3.connect(root / "server.sqlite") as db:
        source, parent, child, task, run = db.execute(
            "SELECT m.id, r.scope_issue_id, a.issue_id, a.task_id, i.run_id "
            "FROM issue_attention_replies r JOIN attentions a ON a.id = r.attention_id "
            "JOIN issue_inputs i ON i.id = r.input_id JOIN chat_messages m ON m.seq = a.source_seq "
            "WHERE r.operation_id = 'history-operation-1'"
        ).fetchone()
    seed_history(root, source, parent, child, task, run, range(first, last + 1), large=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "cli", "runner"):
        parser.add_argument(name, type=lambda path: Path(path).resolve())
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--web", action="store_true")
    modes.add_argument("--history", action="store_true")
    modes.add_argument("--stop-before-delivery", action="store_true")
    args = parser.parse_args()

    with tempfile.TemporaryDirectory(prefix="tend-issue-attention-") as directory:
        root = Path(directory)
        address = free_port()
        prepare(root, free_port(), "A fixed repository.\n")
        config = root / "runner.json"
        settings = json.loads(config.read_text())
        settings.pop("inspect")
        settings["agents"][0]["reasoning_effort"] = "low"
        config.write_text(json.dumps(settings))
        (root / "delegate-now").touch()
        codex_home = root / "fixture-codex"
        codex_home.mkdir()
        env = {**os.environ, "TEND_URL": f"http://{address}", "TEND_TOKEN": "issue-attention-fixture",
               "CODEX_HOME": str(codex_home)}

        def cli(*words):
            result = subprocess.run([args.cli, *words], env=env, timeout=8,
                                    text=True, capture_output=True)
            assert result.returncode == 0, result.stderr
            return json.loads(result.stdout)

        def http(path, body=None, token=env["TEND_TOKEN"], target=address):
            con = HTTPConnection(target, timeout=8)
            try:
                con.request("GET" if body is None else "POST", path,
                            json.dumps(body) if body is not None else None,
                            {"Authorization": f"Bearer {token}", "Content-Type": "application/json"})
                response = con.getresponse()
                return response.status, json.loads(response.read())
            finally:
                con.close()

        def sql(database, statement, *values):
            with sqlite3.connect(root / f"{database}.sqlite") as connection:
                return connection.execute(statement, values).fetchall()

        def launch():
            return subprocess.Popen([
                args.runner, "--config", config, "--db", root / "runner.sqlite",
                "--app-server", sys.executable, TESTS / "fixtures/coordination_app_server.py",
                root, "issue-attention",
            ], env=env)

        def get(path):
            status, body = http(path)
            assert status == 200, (status, body)
            return body

        def seen():
            evidence = root / "issue-attention-inputs.jsonl"
            return [json.loads(line) for line in evidence.read_text().splitlines()] if evidence.exists() else []

        def counters():
            return [sql("server", f"SELECT count(*) FROM {table}") for table in
                    ("chat_messages", "router_inputs", "issue_messages WHERE kind != 'run'",
                     "issues", "tasks", "proposal_decisions")]

        with server(args.server, root, env, listen=address, web_dir=WEB / "dist" if args.web or args.history else None):
            runner = launch()
            try:
                source = cli("chat", "send", "main", REQUEST)["message_id"]
                [origin] = wait_for(lambda: [message for message in get("/api/v1/chats/main/messages")["messages"]
                                             if message["id"] == source and message.get("issue_ids")], bool)
                [parent] = origin["issue_ids"]
                delegation = root / "delegation.json"
                child = json.loads(wait_for(lambda: delegation.read_text() if delegation.exists() else "", bool))["action"]["issue_id"]
                questions = root / "issue-questions.json"
                authored = json.loads(wait_for(lambda: questions.read_text() if questions.exists() else "", bool))
                first, second, internal = authored["ids"]
                task = authored["task_id"]
                prefix = f"/api/v1/issues/{parent}"

                def answer_path(attention):
                    return f"{prefix}/attentions/{attention}/answer"

                def chat_fact(id):
                    return get(f"/api/v1/chats/main/attentions/{id}")

                def card():
                    [found] = [row for row in cli("issue", "list", "--scope=roots")["issues"] if row["id"] == parent]
                    return found

                def child_view():
                    return cli("issue", "read", child)

                wait_for(lambda: get(f"{prefix}/overview")["decisions"], lambda rows: len(rows) == 2)
                initial = wait_for(child_view, lambda view: view["status"] == "waiting")
                assert initial["run"]["phase"]["waiting_for_attention"]
                assert initial["task_id"] == task
                facts = {id: chat_fact(id) for id in (first, second)}
                for scope in (prefix, f"/api/v1/issues/{child}"):
                    overview = get(f"{scope}/overview")
                    assert {d["reply"]["attention_id"]: d["message_id"] for d in overview["decisions"]} == {
                        id: fact["question_message_id"] for id, fact in facts.items()}
                assert card()["attention"]["question_message_id"] == facts[first]["question_message_id"]

                cli("issue", "message", child, NOTE)
                [note] = wait_for(seen, lambda rows: any(row.get("message", {}).get("text") == NOTE for row in rows))
                assert note["kind"] == "issue_message" and note["message"]["text"] == NOTE
                wait_for(child_view, lambda view: view["status"] == "waiting"
                         and view["run"]["run_id"] == note["run_id"])
                assert all(chat_fact(id)["state"] == "awaiting_user" for id in (first, second))
                assert card()["attention"]["question_message_id"] == facts[first]["question_message_id"]
                runs = sql("server", "SELECT id FROM task_runs WHERE task_id = ? ORDER BY rowid", task)
                assert len(runs) == 2
                wait_for(lambda: sql("server", "SELECT count(*) FROM router_inputs WHERE outcome IS NULL"),
                         lambda rows: rows == [(0,)])
                before = counters()
                body = {"operation_id": "fixture-first-reply", "text": FIRST}
                assert http(answer_path(first), body, token="not-authorized")[0] == 401
                assert http(answer_path(first), {**body, "text": " \n "})[0] == 400
                assert http(answer_path(first), {**body, "version": 1})[0] == 400
                assert http(answer_path(internal), body)[0] == 404
                assert http(answer_path(first).replace(parent, "unrelated-fixture-issue"), body)[0] == 404
                assert http(f"/api/v1/chats/main/attentions/{first}/approve", {"version": 1})[0] == 404
                assert sql("server", "SELECT count(*) FROM issue_attention_replies") == [(0,)]
                assert counters() == before

                if args.history:
                    seed_history(root, source, parent, child, task, note["run_id"])
                    overview = get(f"{prefix}/overview")
                    initial_page = overview["decisions"]
                    pending = [d["reply"]["attention_id"] for d in initial_page if d["reply"]["answer"] is None]
                    assert set(pending) == {first, second}
                    assert len(json.dumps(overview, ensure_ascii=False).encode()) < 90_000
                    recorded = [d["message_id"] for d in initial_page if d["reply"]["answer"] is not None]
                    assert recorded == [f"history-question-{n}" for n in range(64, 59, -1)]
                    before_cursor = overview["decision_history_before"]
                    cursor = before_cursor
                    history_path = f"{prefix}/attention-replies"
                    assert http(history_path, token="not-authorized")[0] == 401
                    assert http(f"{history_path}?before=0")[0] == 400
                    assert http(history_path.replace(parent, "other-fixture"))[0] == 404
                    first_older = get(f"{history_path}?before={before_cursor}")
                    assert cli("issue", "attention-replies", f"#{overview['short_number']}",
                               str(before_cursor)) == first_older
                    while cursor is not None:
                        page = get(f"{history_path}?before={cursor}")
                        assert 0 < len(page["decisions"]) <= 5
                        recorded.extend(d["message_id"] for d in page["decisions"])
                        if page["next_before"] is not None:
                            assert page["next_before"] < cursor
                        cursor = page["next_before"]
                    assert recorded == [f"history-question-{n}" for n in range(64, 0, -1)]
                    # Keep the browser fixture readable after proving the size bound.
                    with sqlite3.connect(root / "server.sqlite") as db:
                        db.execute("UPDATE attentions SET answer = '早先的合成答复。' "
                                   "WHERE id LIKE 'historical-fixture-%'")
                    latest = get("/api/v1/chats/main/messages")["messages"]
                    assert not any(m["id"] == facts[first]["question_message_id"] for m in latest)
                    subprocess.run(["node", WEB / "tests/issue-attention.mjs"], timeout=100, check=True, env={
                        **env, "TEND_ISSUE": str(overview["short_number"]), "TEND_ATTENTION_HISTORY": "1",
                        "TEND_FIRST_QUESTION_ID": facts[first]["question_message_id"],
                        "TEND_SECOND_QUESTION_ID": facts[second]["question_message_id"],
                        "TEND_FIRST_ATTENTION_ID": first,
                        "TEND_HISTORY_FIXTURE_DIR": str(root),
                        "TEND_FIXTURE_PYTHON": sys.executable,
                    })
                    assert chat_fact(first)["state"] == "delivered" and chat_fact(first)["issue_reply"] == FIRST
                    assert chat_fact(second)["state"] == "awaiting_user"
                    assert [d["message_id"] for d in get(f"{history_path}?before={before_cursor}")["decisions"]] == [
                        d["message_id"] for d in first_older["decisions"]]
                    print("PASS bounded overview with 64 large older replies, unchanged open questions and stable keyset pages")
                    return

                if args.stop_before_delivery:
                    runner.terminate()
                    runner.wait(timeout=8)
                    status, receipt = http(answer_path(first), body)
                    assert status == 200 and receipt["answer"] == FIRST
                    assert chat_fact(first)["state"] == "answered"
                    assert counters() == before
                    cli("task", "stop", task, "fixture-stop-original")
                    other = {"operation_id": "fixture-stopping", "text": SECOND}
                    assert http(answer_path(second), other)[0] == 409
                    runner = launch()
                    wait_for(lambda: cli("control", "read", "fixture-stop-original"),
                             lambda view: view["state"] == "stopped")
                    assert http(answer_path(second), other)[0] == 409
                    assert http(answer_path(first), body) == (200, receipt)
                    assert child_view()["task_id"] == task
                    assert sql("server", "SELECT id FROM task_runs WHERE task_id = ? ORDER BY rowid", task) == runs
                    cli("issue", "message", child, "另一条独立留言，不接管原答复。")
                    followup = child_view()["task_id"]
                    assert followup != task
                    wait_for(seen, lambda rows: any(r["task_id"] == followup for r in rows))
                    assert all(r["kind"] == "issue_message" for r in seen())
                    assert sql("server", "SELECT i.task_id, i.run_id FROM issue_inputs i "
                               "JOIN issue_attention_replies r ON r.input_id = i.id") == [(task, None)]
                    assert chat_fact(first)["state"] == "answered" and chat_fact(first)["issue_reply"] == FIRST
                    assert {d["reply"]["attention_id"] for d in get(f"{prefix}/overview")["decisions"] if "reply" in d} == {first}
                    print("PASS stopped original Task retains the answer without a native continuation or successor transfer")
                    return

                if args.web:
                    subprocess.run(["node", WEB / "tests/issue-attention.mjs"], timeout=70, check=True, env={
                        **env, "TEND_ISSUE": str(get(f"{prefix}/overview")["short_number"]),
                        "TEND_FIRST_QUESTION_ID": facts[first]["question_message_id"],
                        "TEND_SECOND_QUESTION_ID": facts[second]["question_message_id"],
                    })
                else:
                    with lose_first_response(address) as (proxy, posts):
                        with ThreadPoolExecutor(max_workers=5) as pool:
                            lost = pool.submit(http, answer_path(first), body, target=proxy)
                            replays = list(pool.map(lambda _: http(answer_path(first), body), range(5)))
                        assert isinstance(lost.exception(), RemoteDisconnected), lost.exception()
                        [(posted, code, original)] = posts
                        assert posted == body and code == 200
                        assert replays == [(200, original)] * 5
                    assert cli("issue", "answer", f"#{get(f'{prefix}/overview')['short_number']}",
                               first, FIRST, body["operation_id"]) == original
                    assert http(answer_path(first), {**body, "text": SECOND})[0] == 409
                    assert http(answer_path(second), body)[0] == 409
                    assert http(answer_path(first).replace(parent, child), body)[0] == 409
                    assert http(answer_path(first), {**body, "operation_id": "fixture-duplicate"})[0] == 409
                    wait_for(lambda: chat_fact(first), lambda fact: fact["state"] == "delivered")
                    assert chat_fact(second)["state"] == "awaiting_user"
                    assert card()["attention"]["question_message_id"] == facts[second]["question_message_id"]
                    assert cli("issue", "answer", child, second, SECOND, "fixture-second-reply")["answer"] == SECOND

                assert wait_for(lambda: chat_fact(second), lambda fact: fact["state"] == "delivered")
                assert counters() == before
                assert "attention" not in card()
                expected = {first: FIRST, second: SECOND}
                decisions = {d["reply"]["attention_id"]: d["reply"]["answer"]
                             for d in get(f"{prefix}/overview")["decisions"]}
                assert decisions == expected
                received = [row for row in seen() if row["kind"] == "attention_answered"]
                assert len(received) == 2
                assert {row["attention_id"]: row["answer"] for row in received} == expected
                assert all(row["task_id"] == task and row["child_task_id"] == task
                           and "user_message_id" not in row and "user_text" not in row for row in received)
                assert sql("server", "SELECT id FROM task_runs WHERE task_id = ? ORDER BY rowid", task) == (
                    runs + [(row["run_id"],) for row in received])
                deliveries = sql("server", "SELECT i.id, i.task_id, i.run_id, a.answer_seq "
                                 "FROM issue_attention_replies r JOIN issue_inputs i ON i.id = r.input_id "
                                 "JOIN attentions a ON a.id = r.attention_id ORDER BY i.rowid")
                assert deliveries == [(row["input_id"], task, row["run_id"], None) for row in received]
                assert sql("runner", "SELECT id, run_id FROM local_issue_inputs "
                           "WHERE json_extract(payload, '$.kind') = 'attention_answered' ORDER BY rowid") == [
                               (row["input_id"], row["run_id"]) for row in received]
                for id, answer in expected.items():
                    fact = chat_fact(id)
                    assert fact["state"] == "delivered" and fact["answer"] == fact["issue_reply"] == answer
                    assert fact["user_message_id"] is None
                print("PASS each explicit reply records original text and wakes the exact original Task once")
            finally:
                if runner.poll() is None:
                    runner.terminate()
                    runner.wait(timeout=8)


if __name__ == "__main__":
    main()
