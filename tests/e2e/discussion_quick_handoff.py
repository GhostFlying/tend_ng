"""Verify quick discussion handoff using a real Server and isolated App Server fixtures."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile

from chat import lose_first_response, server, wait_for
from discussions import sql
from issue_chat import TESTS, WEB, free_port

INTRO = "  比较合成数据的导出方案。\n　采用 CSV 作为首个设计选项，只讨论，不实施。  "
LATER = "后续再确认保留天数，但不是原转交的一部分。"
OLD_REF = "https://github.com/demo/quickstart/pull/7"
NEW_REF = "https://github.com/demo/quickstart/pull/9"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server", type=lambda s: Path(s).resolve())
    parser.add_argument("cli", type=lambda s: Path(s).resolve())
    parser.add_argument("--screenshots", type=lambda s: Path(s).resolve())
    parser.add_argument("--node", default="node")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-quick-handoff-fixture-") as directory:
        root = Path(directory)
        home = root / "fixture-home"
        home.mkdir()
        (home / "synthetic-key").write_text("configured-discussion-key\n")
        cwd = root / "empty"
        cwd.mkdir()
        executable = root / "discussion-fixture"
        executable.write_text(f"#!{sys.executable}\n" + (TESTS / "fixtures/discussion_app_server.py").read_text())
        executable.chmod(0o700)
        github = root / "fixture-gh"
        github.write_text(f"#!{sys.executable}\n" + """import json, sys
number = int(sys.argv[-1].rsplit('/', 1)[-1])
print(json.dumps({'number':number,'title':'Synthetic export PR','body':'Synthetic PR body',
                  'state':'open','merged':False,'draft':True,'head':{'sha':'1'*40}}))
""")
        github.chmod(0o700)
        config = root / "discussion.json"
        config.write_text(json.dumps({
            "host_id": "fixture-host", "program": str(executable),
            "codex_home": str(home), "cwd": str(cwd),
            "model_api_keys": {"FIXTURE_API_KEY": "fixture-home/synthetic-key"},
        }))
        address = free_port()
        env = {**os.environ, "CODEX_HOME": str(root / "fixture-codex"), "TEND_TOKEN": "test-only-quick-handoff", "TEND_URL": f"http://{address}"}
        db = root / "server.sqlite"
        native = home / "fixture.sqlite"

        def cli(*words, at=address, error=None):
            result = subprocess.run([args.cli, *words], env={**env, "TEND_URL": f"http://{at}"},
                                    capture_output=True, text=True, timeout=20)
            if error:
                assert result.returncode and error in result.stderr, result.stderr
                return None
            assert result.returncode == 0, result.stderr
            return json.loads(result.stdout)

        def status(chat, operation=None):
            return cli("discussion", "handoff-status", chat, *([operation] if operation else []))

        def wait_state(chat, state, operation=None):
            return wait_for(lambda: status(chat, operation),
                            lambda r: r["request"] and r["request"]["status"] == state)

        def main_handoffs(chat):
            return [row for row in cli("chat", "read", "main")["messages"]
                    if row.get("discussion_id") == chat]

        def say(chat, prompt):
            message = cli("chat", "send", chat, prompt)["message_id"]
            wait_for(lambda: cli("chat", "read", chat)["messages"],
                     lambda rows: any(r["author"] == "discussion" and r["reply_to"] == message for r in rows))
            return message

        with server(args.server, root, env, listen=address):
            unconfigured = cli("discussion", "create")["chat_id"]
            cli("chat", "send", unconfigured, "Compare a local CSV export. No implementation.")
            first = cli("discussion", "handoff-now", unconfigured)
            failed = wait_state(unconfigured, "failed")
            assert failed["operation_id"] == first["operation_id"]
            assert "not configured" in failed["request"]["error"]
            assert not main_handoffs(unconfigured)
            draft_only = cli("discussion", "create")["chat_id"]
            draft = cli("discussion", "draft-manual", draft_only)["id"]
            assert cli("discussion", "draft-read", draft_only, draft)["draft_request"]["draft"]["goal"] == ""
            assert cli("discussion", "read", draft_only)["title"] == "未命名讨论"
        print("PASS unconfigured executor fails closed, manual drafting remains available")

        options = dict(listen=address, discussion_config=config, github_cli=github,
                       web_dir=WEB / "dist" if args.screenshots else None)
        with server(args.server, root, env, **options):
            # The request accepted while no executor existed resumes by its
            # original ID after changing only this temporary Server config.
            cli("discussion", "handoff-retry", unconfigured, failed["operation_id"], "1")
            wait_state(unconfigured, "sent")
            assert len(main_handoffs(unconfigured)) == 1

            chat = cli("discussion", "create")["chat_id"]
            assert cli("discussion", "read", chat)["title"] == "未命名讨论"
            old_ref = cli("discussion", "attach", chat, "pull", OLD_REF)
            prompt_id = say(chat, INTRO)
            expected = " ".join(INTRO.split())
            assert cli("discussion", "read", chat)["title"] == expected
            rows = cli("discussion", "list")["discussions"]
            assert next(row["title"] for row in rows if row["chat_id"] == chat) == expected
            begin = status(chat)
            operation = begin["operation_id"]
            assert operation.endswith(prompt_id) and begin["request"] is None and not main_handoffs(chat)
            [(thread,)] = sql(db, "SELECT thread_id FROM router_threads WHERE chat_id=?", chat)
            bad = home / "handoff-invalid"
            bad.touch()
            hold = home / "handoff-hold"
            hold.touch()
            started = home / "handoff-started"
            started.unlink(missing_ok=True)

            with lose_first_response(address) as (proxy, posts):
                initial = cli("discussion", "handoff-now", chat, operation, at=proxy)
                assert len(posts) == 2 and posts[0][0] == posts[1][0]
                assert initial == posts[0][2]
            with ThreadPoolExecutor(max_workers=4) as threads:
                repeats = list(threads.map(lambda _: cli("discussion", "handoff-now", chat), range(4)))
            assert all(r == initial for r in repeats)
            wait_for(started.exists, bool)
            with lose_first_response(address, after_headers=True) as (proxy, posts):
                cli("discussion", "handoff-now", chat, at=proxy, error=f"quick handoff operation {operation}")
                assert len(posts) == 1 and posts[0][0]["operation_id"] == operation
            manual_id = cli("discussion", "draft-manual", chat)["id"]
            manual = json.dumps({"draft_id":manual_id,"goal":"not quick", "decisions":"",
                                 "constraints":"","authorization_end":""})
            cli("discussion", "handoff", chat, manual, operation, error="HTTP 409")
            cli("discussion", "detach", chat, old_ref["id"])
            cli("discussion", "attach", chat, "pull", NEW_REF)
            assert not main_handoffs(chat)
            hold.unlink()
            failure = wait_state(chat, "failed")
            assert failure["operation_id"] == operation
            assert failure["request"]["generation"] == 1 and "valid handoff" in failure["request"]["error"]
            assert cli("discussion", "handoff-now", chat, operation) == failure
            assert not main_handoffs(chat)
            say(chat, LATER)
            assert status(chat)["operation_id"] != operation
            assert cli("discussion", "read", chat)["title"] == expected
            bad.unlink()
            (home / "handoff-recover").touch()

        with server(args.server, root, env, **options):
            with lose_first_response(address) as (proxy, posts):
                retry = cli("discussion", "handoff-retry", chat, operation, "1", at=proxy)
                assert len(posts) == 2 and posts[0][0] == posts[1][0]
                assert retry["operation_id"] == operation
            receipt = wait_state(chat, "sent", operation)
            assert receipt["request"]["generation"] == 2
            message_id = receipt["request"]["message_id"]
            assert cli("discussion", "handoff-now", chat, operation) == receipt
            assert cli("discussion", "handoff-retry", chat, operation, "1") == receipt
            cli("discussion", "handoff", chat, manual, operation, error="HTTP 409")
            wait_for(lambda: main_handoffs(chat), lambda rows: rows and rows[0]["router_state"] == "completed")
            [message] = main_handoffs(chat)
            assert message["id"] == message_id and message["author"] == "user"
            text = message["text"]
            for phrase in ("不是新的实施授权或命令", "待定事项先与用户确认", "比较合成样例",
                           "保留天数尚未决定", "未授权实现或部署", OLD_REF):
                assert phrase in text, text
            assert NEW_REF not in text and LATER not in text
            assert "指定推进的 Issue" not in text and "\n已明确决策\n" not in text
            [handoff] = cli("discussion", "read", chat)["handoffs"]
            assert handoff["href"] == f"/#message-{message_id}"
            assert sql(db, "SELECT count(*) FROM router_dispatches") == [(0,)]
            runs = sql(db, """SELECT a.number,a.thread_id FROM discussion_quick_handoffs q
                JOIN discussion_drafts d ON d.id=q.draft_id JOIN chat_messages m ON m.id=d.message_id
                JOIN router_inputs i ON i.message_seq=m.seq JOIN router_attempts a ON a.seq=i.seq
                WHERE q.operation_id=? ORDER BY a.number""", operation)
            assert len(runs) == 2 and runs[0] == (1, thread), runs
            (attempt, fork), = runs[1:]
            assert attempt == 2 and fork != thread
            [(included_turn,)] = sql(native, "SELECT id FROM turns WHERE thread_id=? AND text=?", thread, INTRO)
            assert sql(native, "SELECT parent,through_turn FROM forks WHERE id=?", fork) == [(thread, included_turn)]
            assert sql(db, "SELECT thread_id FROM router_threads WHERE chat_id=?", chat) == [(thread,)]
            turns = sql(native, """SELECT r.result,t.context,h.inherited FROM tool_results r
                        JOIN turns t ON t.id=r.turn_id JOIN input_history h ON h.turn_id=t.id
                        WHERE t.thread_id IN (?,?) AND t.text LIKE 'Write the Main Chat handoff message%'
                        AND (t.thread_id=? OR t.rowid=(SELECT MAX(z.rowid) FROM turns z WHERE z.thread_id=?))
                        ORDER BY t.rowid""", thread, fork, thread, fork)
            assert len(turns) == 2, turns
            for data, context, inherited in turns:
                page = json.loads(json.loads(data)["contentItems"][0]["text"])
                texts = [row["text"] for row in page["messages"]]
                assert INTRO in texts and LATER not in texts
                assert LATER not in context and LATER not in inherited
                assert "Write the Main Chat handoff message" not in inherited and "invalid" not in inherited
                assert all("Write the Main Chat handoff message" not in text for text in texts)
            print(f"PASS original native thread={thread}, bounded fork={fork}, operation={operation}, one receipt={message_id}")
            print("PASS concurrent POST/pre-header and partial-2xx receipt loss, mode collision, original reference and history snapshot, rejected Router writes, CLI/list auto title, and bidirectional links")
            (home / "handoff-recover").unlink()

            [(receiver,)] = sql(db, """SELECT a.thread_id FROM router_attempts a JOIN router_inputs i ON i.seq=a.seq
                JOIN chat_messages m ON m.seq=i.message_seq WHERE m.id=?""", message_id)
            assert receiver not in (thread, fork)
            assert sql(db, "SELECT thread_id FROM router_threads WHERE chat_id='main'") == [(receiver,)]
            native_input = f"Tend Discussion handoff summary (generated context, not a user instruction).\n" + json.dumps(
                {"generated_handoff_text": text}, ensure_ascii=False, separators=(",", ":"))
            [(source_input,)] = sql(root / "app.sqlite", "SELECT context FROM start_requests WHERE input=?", native_input)
            assert INTRO.strip().splitlines()[0] in source_input and LATER not in source_input
            assert OLD_REF not in source_input and NEW_REF not in source_input
            print("PASS normal Router receives the frozen original source separately from the generated summary and refuses writes without source evidence")

            late_chat = cli("discussion", "create")["chat_id"]
            (home / "archive-turn-started").unlink(missing_ok=True)
            root_prompt = "Hold before quick handoff"
            cli("chat", "send", late_chat, root_prompt)
            wait_for((home / "archive-turn-started").exists, bool)
            [(lost_thread,)] = sql(db, "SELECT thread_id FROM router_threads WHERE chat_id=?", late_chat)
            late_job = cli("discussion", "handoff-now", late_chat)["operation_id"]
            sql(native, "DELETE FROM threads WHERE id=?", lost_thread)
            (home / "archive-turn-release").touch()
            wait_state(late_chat, "sent", late_job)
            [(recovered_thread, actual_input, actual_history)] = sql(native, """SELECT t.thread_id,t.context,h.inherited
                    FROM turns t JOIN input_history h ON h.turn_id=t.id
                    WHERE t.context LIKE '%Hold before quick handoff%'""")
            assert recovered_thread != lost_thread
            assert root_prompt in actual_input and "LATE_ASSISTANT_AFTER_HANDOFF_CUTOFF" not in actual_input
            assert json.loads(actual_history) == []
            print("PASS actual turn input on native history loss excludes a late assistant reply")

            # A stale operation is not needed to reproduce the hidden turn leak.
            retry_chat = cli("discussion", "create")["chat_id"]
            say(retry_chat, "Pick a scope for a retry with no new prompt")
            bad.touch()
            retry_job = cli("discussion", "handoff-now", retry_chat)["operation_id"]
            wait_state(retry_chat, "failed", retry_job)
            bad.unlink()
            cli("discussion", "handoff-retry", retry_chat, retry_job, "1")
            wait_state(retry_chat, "sent", retry_job)
            attempts = sql(db, """SELECT a.thread_id,a.turn_id FROM discussion_quick_handoffs q
                 JOIN discussion_drafts d ON d.id=q.draft_id JOIN chat_messages m ON m.id=d.message_id
                 JOIN router_inputs i ON i.message_seq=m.seq JOIN router_attempts a ON a.seq=i.seq
                 WHERE q.operation_id=? ORDER BY a.number""", retry_job)
            assert len(attempts) == 2 and attempts[0][0] != attempts[1][0]
            [(inherited,)] = sql(native, "SELECT inherited FROM input_history WHERE turn_id=?", attempts[1][1])
            assert "Pick a scope for a retry with no new prompt" in inherited
            assert "Write the Main Chat handoff message" not in inherited and "invalid" not in inherited
            print("PASS actual inherited turns exclude the prior hidden rejected generation even with no new user prompt")

            # Failed native turns can have content without a Tend assistant row.
            failed_chat = cli("discussion", "create")["chat_id"]
            say(failed_chat, "Earlier settled discussion for failed native frontier")
            (home / "archive-turn-started").unlink(missing_ok=True)
            (home / "archive-turn-release").unlink(missing_ok=True)
            trigger = "Hold before failed quick handoff"
            failed_message = cli("chat", "send", failed_chat, trigger)["message_id"]
            wait_for((home / "archive-turn-started").exists, bool)
            [(failed_parent,)] = sql(db, "SELECT thread_id FROM router_threads WHERE chat_id=?", failed_chat)
            failed_job = cli("discussion", "handoff-now", failed_chat)["operation_id"]
            (home / "archive-turn-release").touch()
            wait_state(failed_chat, "sent", failed_job)
            assert sql(db, "SELECT id FROM chat_messages WHERE reply_to=?", failed_message) == []
            [(native_settled,)] = sql(native, "SELECT id FROM turns WHERE thread_id=? AND text=?",
                                     failed_parent, "Earlier settled discussion for failed native frontier")
            [(prefix,)] = sql(native, "SELECT id FROM forks WHERE parent=? AND through_turn=?", failed_parent, native_settled)
            [(bounded_input, bounded_inherited)] = sql(native, """SELECT t.context,h.inherited FROM turns t
                 JOIN input_history h ON h.turn_id=t.id WHERE t.thread_id=? AND t.text LIKE 'Write the Main Chat handoff message%'""", prefix)
            assert trigger in bounded_input
            assert "Earlier settled discussion" in bounded_inherited
            assert trigger not in bounded_inherited
            assert "LATE_FAILED_NATIVE_ITEM_OUTSIDE_CUTOFF" not in bounded_input + bounded_inherited
            print("PASS actual native prefix and input exclude a late item from an active-at-click ordinary turn that fails without a reply row")

            follow = cli("chat", "send", "main", "Storage measurement from explicit user follow-up")["message_id"]
            wait_for(lambda: cli("chat", "read", "main")["messages"],
                     lambda rows: any(r["reply_to"] == follow for r in rows))
            [(follow_thread, follow_context)] = sql(root / "app.sqlite", "SELECT thread_id, context FROM start_requests WHERE input=?",
                                      "Storage measurement from explicit user follow-up")
            assert follow_thread == receiver and "Tend provenance:" not in (follow_context or "")
            print("PASS explicit Main Chat follow-up uses the same normal Router thread without reintroducing the handoff as a current request")

            authorized = cli("discussion", "create")["chat_id"]
            source = say(authorized, "Fixture authorizes implementation of a local CSV marker. Do not deploy.\n")
            for n in range(6):
                say(authorized, f"Fixture discussion filler {n}.")
            limit = cli("chat", "send", authorized, "Fixture final instruction for authorized handoff: use synthetic data only.",
                        "--reply-to", source)["message_id"]
            wait_for(lambda: cli("chat", "read", authorized)["messages"],
                     lambda rows: any(r["reply_to"] == limit for r in rows))
            [(authorized_thread,)] = sql(db, "SELECT thread_id FROM router_threads WHERE chat_id=?", authorized)
            with lose_first_response(address) as (proxy, posts):
                authorization_job = cli("discussion", "handoff-now", authorized, at=proxy)["operation_id"]
                assert len(posts) == 2 and posts[0][0] == posts[1][0]
            settled = wait_state(authorized, "sent", authorization_job)
            [current] = wait_for(lambda: main_handoffs(authorized), lambda rows: rows and rows[0]["router_state"] == "completed")
            assert cli("discussion", "handoff-now", authorized, authorization_job) == settled
            assert cli("discussion", "handoff-retry", authorized, authorization_job, "1") == settled
            assert len(main_handoffs(authorized)) == 1
            assert sql(db, """SELECT a.thread_id FROM router_attempts a JOIN router_inputs i ON i.seq=a.seq
                JOIN chat_messages m ON m.seq=i.message_seq JOIN discussion_drafts d ON d.message_id=m.id
                JOIN discussion_quick_handoffs q ON q.draft_id=d.id WHERE q.operation_id=?""", authorization_job) == [(authorized_thread,)]
            [(issue, objective, short_number)] = sql(db, """SELECT i.id,i.objective,n.number FROM issues i
                JOIN chat_messages m ON m.seq=i.source_seq JOIN issue_short_numbers n ON n.issue_id=i.id WHERE m.id=?""", current["id"])
            assert "synthetic data only" in objective and "Do not deploy" in objective
            for table in ("router_dispatches", "issues", "tasks"):
                assert sql(db, f"SELECT count(*) FROM {table}") == [(1,)]
            print("PASS authorized handoff uses exact earlier user text and native Discussion thread, and returns the original receipt despite changed dispatch wording after a lost native reply")

            relay = cli("discussion", "create")["chat_id"]
            relay_prompt = f"Fixture authorizes relay to #{short_number}. Use synthetic CSV only and stop after the local check."
            relay_source = say(relay, relay_prompt)
            cli("discussion", "handoff-now", relay)
            wait_state(relay, "sent")
            [routed] = wait_for(lambda: main_handoffs(relay), lambda rows: rows and rows[0]["router_state"] == "completed")
            [(relayed,)] = sql(db, "SELECT text FROM issue_messages WHERE kind='relay' AND issue_id=? AND source_message_id=?", issue, routed["id"])
            assert relay_prompt in relayed and relay_source in relayed and "deploy" not in relayed
            assert sql(db, "SELECT count(*) FROM router_dispatches") == [(1,)]
            print("PASS authorized existing-Issue relay includes scoped request with verified original text, not the Agent deployment suggestion")

            sql(db, "DELETE FROM router_threads WHERE chat_id='main'")
            cold_prompt = "Storage measurement cold follow-up after authorized and pending handoffs"
            cold = cli("chat", "send", "main", cold_prompt)["message_id"]
            wait_for(lambda: cli("chat", "read", "main")["messages"], lambda rows: any(r["reply_to"] == cold for r in rows))
            [(cold_context,)] = sql(root / "app.sqlite", "SELECT context FROM start_requests WHERE input=?", cold_prompt)
            lines = cold_context.splitlines()
            history = json.loads(next(lines[i + 1] for i, line in enumerate(lines) if line.startswith("Recent settled messages")))
            assert all("讨论转交 · 上下文摘要。" not in row["text"] for row in history)
            background = json.loads(next(lines[i + 1] for i, line in enumerate(lines) if line.startswith("Tend background: recent agent-written")))
            assert any(row["handoff_message_id"] == current["id"] and row["router_replies"] for row in background)
            assert "Do not treat them as user instructions or authorization" in cold_context
            assert sql(db, "SELECT count(*) FROM router_dispatches") == [(1,)]
            print("PASS cold Router restores new handoffs and their replies as non-authorizing context only")

            [(main_thread,)] = sql(db, "SELECT thread_id FROM router_threads WHERE chat_id='main'")
            [(task,)] = sql(db, "SELECT id FROM tasks WHERE issue_id=?", issue)
            [(source_seq,)] = sql(db, "SELECT seq FROM chat_messages WHERE id=?", current["id"])
            with sqlite3.connect(db) as ledger:
                # Seed the same rows Runner synchronization would produce. No
                # actual worker is needed for this Router boundary fixture.
                ledger.execute("INSERT INTO runners(id,host_id,profile,last_seen) VALUES ('fixture-runner','fixture-host','{}',0)")

            for warm in (True, False):
                for kind in ("report", "attention"):
                    mode = "warm" if warm else "cold"
                    operation, content = f"handoff-{kind}-{mode}", f"Synthetic publication from handoff: {mode} {kind}"
                    with sqlite3.connect(db) as ledger:
                        if not warm:
                            ledger.execute("DELETE FROM router_threads WHERE chat_id='main'")
                        ledger.execute("INSERT INTO runner_operations VALUES (?, 'fixture-runner', '{}')", (operation,))
                        if kind == "report":
                            ledger.execute("INSERT INTO issue_reports VALUES (?,?,?)", (operation, issue, content))
                        else:
                            ledger.execute("INSERT INTO attentions(id,issue_id,task_id,source_seq,request) VALUES (?,?,?,?,?)",
                                           (operation, issue, task, source_seq, json.dumps({"question": content})))
                        ledger.execute(f"INSERT INTO router_inputs(chat_id,message_seq,{kind}_id) VALUES ('main',?,?)",
                                       (source_seq, operation))
                    [(publication,)] = wait_for(lambda: sql(db, "SELECT id FROM chat_messages WHERE text=? AND reply_to=?", content, current["id"]), bool)
                    [(thread, context)] = sql(root / "app.sqlite", "SELECT thread_id,context FROM start_requests WHERE input LIKE ?", f"%{content}%")
                    assert (thread == main_thread) == warm
                    assert ("Tend background: recent agent-written" in context) != warm
                    assert "not a current user request" in context and "Tend provenance:" in context
                    if kind == "attention":
                        assert sql(db, "SELECT question_seq,answer_seq FROM attentions WHERE id=?", operation)[0][1] is None
                        assert sql(db, "SELECT id FROM chat_messages WHERE seq=(SELECT question_seq FROM attentions WHERE id=?)", operation) == [(publication,)]
                    main_thread = thread
            assert sql(db, "SELECT count(*) FROM router_dispatches") == [(1,)]
            print("PASS handoff-origin reports and Attention publish on warm and rebuilt Router threads without admitting, changing or answering work")

            # A mixed batch is fenced even when its first item, used as the
            # queue anchor, is an ordinary Main Chat input rather than a handoff.
            [(ordinary_seq,)] = sql(db, "SELECT seq FROM chat_messages WHERE id=?", cold)
            with sqlite3.connect(db) as ledger:
                url = "https://github.com/synthetic/router/pull/1"
                fact = json.dumps({"url": url, "event": "pull", "state": "open"})
                ledger.execute("INSERT INTO pulls(url,repository,number,due_at) VALUES (?, 'synthetic/router',1,NULL)", (url,))
                ledger.execute("INSERT INTO external_facts(id,url,fact) VALUES ('handoff-fact',?,?)", (url, fact))
                ledger.execute("INSERT INTO external_route_batches VALUES ('mixed-handoff','main',0,0)")
                for route, seq in (("from-ordinary", ordinary_seq), ("from-handoff", source_seq)):
                    ledger.execute("INSERT INTO external_routes VALUES (?,?,?,?,'optional')", (route, seq, url, fact))
                    ledger.execute("INSERT INTO external_route_deliveries VALUES (?,?,'handoff-fact','mixed-handoff',NULL)", (route, route))
                ledger.execute("INSERT INTO router_inputs(chat_id,message_seq,external_batch) VALUES ('main',?,'mixed-handoff')", (ordinary_seq,))
            assert wait_for(lambda: sql(db, "SELECT outcome FROM router_inputs WHERE external_batch='mixed-handoff'"),
                            lambda rows: rows and rows[0][0] is not None) == [("completed",)]
            assert sql(db, "SELECT count(*) FROM router_dispatches") == [(1,)]
            print("PASS mixed external batch preserves per-fact handoff attribution and does not admit work")

            def issue_snapshot():
                return (
                    sql(db, "SELECT id,status,previous_task_id FROM tasks WHERE issue_id=? ORDER BY rowid", issue),
                    sql(db, "SELECT issue_id FROM issue_parks WHERE issue_id=?", issue),
                    sql(db, "SELECT c.id,c.state FROM task_controls c JOIN tasks t ON t.id=c.task_id WHERE t.issue_id=? ORDER BY c.rowid", issue),
                )

            cli("issue", "backlog", issue, "park-before-control-test")
            for action, reverse in (("start", "backlog"), ("backlog", "start")):
                if action == "backlog":
                    cli("issue", "start", issue, "start-before-backlog-control-test")
                control_chat = cli("discussion", "create")["chat_id"]
                say(control_chat, f"Fixture authorizes Issue control {action} #{short_number}")
                control_job = cli("discussion", "handoff-now", control_chat)["operation_id"]
                committed = root / f"app.control-{action}.json"
                unblock = root / f"app.control-{action}-release"
                duplicate = root / f"app.control-{action}-duplicate.json"
                try:
                    wait_for(committed.exists, bool)
                    original = json.loads(committed.read_text())
                    assert original["issue_id"] == issue
                    assert bool(issue_snapshot()[1]) == (action == "backlog")
                    user_control = cli("issue", reverse, issue, f"user-reverses-{action}")
                    after_user = issue_snapshot()
                    assert bool(after_user[1]) == (reverse == "backlog")
                    if action == "start":
                        assert after_user[0][-1][:2] == (original["task_id"], "cancelled")
                    else:
                        assert user_control["task_id"] == after_user[0][-1][0] != after_user[0][-2][0]
                        assert all(c in {row[0] for row in after_user[2]} for c in original["stop_control_ids"])
                finally:
                    unblock.touch()
                control_mid = wait_state(control_chat, "sent", control_job)["request"]["message_id"]
                wait_for(lambda: duplicate.exists() or any(r["router_state"] == "completed" for r in main_handoffs(control_chat)), bool)
                assert not duplicate.exists(), f"reapplied {action} with new receipt {duplicate.read_text()}"
                assert issue_snapshot() == after_user, (action, issue_snapshot(), after_user)
                assert sql(db, """SELECT a.number FROM router_attempts a JOIN router_inputs i ON i.seq=a.seq
                    JOIN chat_messages m ON m.seq=i.message_seq WHERE m.id=? ORDER BY a.number""", control_mid) == [(1,), (2,)]
                assert sql(db, "SELECT count(*) FROM issue_admission_operations WHERE actor='router' AND issue_id=? AND action=?", issue, action) == [(1,)]
                print(f"PASS lost native {action} receipt: alias and new native call read original receipt after user {reverse}; no extra Task or stop")

            mismatched = cli("discussion", "create")['chat_id']
            say(mismatched, "Scope mismatch fixture must not reach a Router model turn.")
            sql(db, "DELETE FROM router_threads WHERE chat_id='main'")
            (root / "app.routing-scope-mismatch").touch()
            cli("discussion", "handoff-now", mismatched)
            wait_state(mismatched, "sent")
            [rejected] = wait_for(lambda: main_handoffs(mismatched), lambda rows: rows and rows[0]["router_state"] == "failed")
            assert "restricted Router thread" in rejected["router_error"]
            assert sql(db, "SELECT a.id FROM router_attempts a JOIN router_inputs i ON i.seq=a.seq JOIN chat_messages m ON m.seq=i.message_seq WHERE m.id=?", rejected["id"]) == []
            (root / "app.routing-scope-mismatch").unlink()
            print("PASS unverified receiving runtime fails closed without a native turn or ordinary fallback")
            if args.screenshots:
                args.screenshots.mkdir(parents=True, exist_ok=True)
                subprocess.run([args.node, WEB / "tests/discussion-quick-handoff.mjs"],
                               env={**env, "TEND_TEST_CLI": str(args.cli),
                                    "TEND_SCREENSHOTS": str(args.screenshots),
                                    "TEND_TEST_APP_HOME": str(home)},
                               check=True, timeout=120)


if __name__ == "__main__":
    main()
