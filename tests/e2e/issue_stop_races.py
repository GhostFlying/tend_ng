"""Exercise the original Stop and batch identities across sync boundaries."""

import argparse
from contextlib import closing, contextmanager
from http.client import HTTPConnection
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import time

from chat import lose_first_response, server, wait_for

RUNNER = "fixture-runner"
HOST = "fixture-host"
TOKEN = "stop-races-fixture"
GITHUB = "https://github.com/fixture/project/pull/1"
CODEBASE = "https://code.byted.org/fixture/project/merge_requests/1"
AGENT = {"id": "owner", "category": "issue-router", "revision": "1", "model": "fixture"}


@contextmanager
def fixture(binary):
    with tempfile.TemporaryDirectory(prefix="tend-stop-races-") as temporary:
        root = Path(temporary)
        env = {**os.environ, "TEND_TOKEN": TOKEN, "CODEX_HOME": str(root / "codex")}
        with server(binary, root, env, github_cli="/bin/false", codebase_cli="/bin/false") as address:
            def api(path, data=None):
                with closing(HTTPConnection(address, timeout=5)) as connection:
                    connection.request("GET" if data is None else "POST", f"/api/v1/{path}",
                                       None if data is None else json.dumps(data), {
                                           "Authorization": f"Bearer {TOKEN}",
                                           "Content-Type": "application/json",
                                       })
                    response = connection.getresponse()
                    body = json.load(response)
                    assert response.status == 200, (path, response.status, body)
                    return body

            def sync(*records, capable=True):
                return api(f"runners/{RUNNER}/sync", {"protocol": 2, "receipts": [], "records": records,
                    "profile": {"runner_id": RUNNER, "host_id": HOST, "repos": [], "agents": [],
                                "slot_limit": 1, "running_slots": 0, "available_for_assignment": 0,
                                "codebase_review_comments": capable}})

            @contextmanager
            def ledger():
                with closing(sqlite3.connect(root / "server.sqlite")) as db, db:
                    db.execute("PRAGMA foreign_keys=ON")
                    yield db

            sync()
            with ledger() as db:
                db.execute("INSERT INTO chat_messages(id,chat_id,text,operation_id) VALUES('source','main','Fixture','source-op')")
            yield root, address, env, api, sync, ledger


def task(db, issue, status, parent=None):
    name, assignment = f"{issue}-task", f"{issue}-claim"
    db.execute("INSERT INTO issues(id,source_seq,title,objective,parent_issue_id) VALUES(?,1,?,'Fixture',?)",
               (issue, issue, parent))
    db.execute("INSERT INTO tasks(id,issue_id,category,status,repo_ids,required_capabilities) "
               "VALUES(?,?,'issue-router',?,'[]','[]')", (name, issue, status))
    contract = {"placement": {"assignment_id": assignment, "agent": AGENT, "host_id": HOST, "runner_id": RUNNER},
                "task_id": name, "issue_id": issue, "parent_issue_id": parent, "source_message_id": "source",
                "chat_id": "main", "work": {"title": issue, "objective": "Fixture"}}
    state = "reserved" if status == "reserved" else "accepted"
    db.execute("INSERT INTO runner_assignments(id,task_id,runner_id,contract,state,expires_at) "
               "VALUES(?,?,?,?,?,unixepoch()+600)", (assignment, name, RUNNER, json.dumps(contract), state))
    db.execute("UPDATE tasks SET current_assignment=? WHERE id=?", (assignment, name))
    return name, assignment


def add_child_fact(db, name, label):
    data = {"input_id": label, "child_issue_id": "child", "child_task_id": "child-task",
            "summary": label, "kind": "issue_closed"}
    db.execute("INSERT INTO issue_inputs(id,task_id,payload,wake_at) VALUES(?,?,?,?)",
               (label, name, json.dumps(data), time.time_ns() // 1_000_000 + 300_000))
    db.execute("INSERT INTO issue_input_order(input_id) VALUES(?)", (label,))


def add_required(db, name):
    condition = {"url": GITHUB, "event": "pull", "state": "open"}
    db.execute("INSERT INTO pulls(url,repository,number) VALUES(?,'fixture/project',1)", (GITHUB,))
    db.execute("INSERT INTO external_facts(id,url,fact) VALUES('fact',?,?)", (GITHUB, json.dumps(condition)))
    db.execute("INSERT INTO external_routes(id,message_seq,url,condition,mode) VALUES('route',1,?,?,'required')",
               (GITHUB, json.dumps(condition)))
    db.execute("INSERT INTO external_watches(id,issue_id,task_id,url,condition,mode,state,route_id,origin_issue) "
               "VALUES('watch','owner',?,?,?,'required','routed','route','owner')", (name, GITHUB, json.dumps(condition)))
    db.execute("INSERT INTO external_deliveries(id,watch_id,fact_id) VALUES('github','watch','fact')")
    db.execute("INSERT INTO external_batches VALUES(?,unixepoch()+300)", (name,))
    db.execute("INSERT INTO issue_input_order(input_id) VALUES('github')")


def ack(command, assignment):
    return {"kind": "control", "data": {"event_id": "proof", "id": command, "assignment_id": assignment,
                                         "state": "stopped"}}


def issued_pair(server_bin, runner_bin):
    with fixture(server_bin) as (root, address, env, api, sync, ledger):
        with ledger() as db:
            task(db, "root", "waiting")
            for i in range(31):
                task(db, f"earlier-{i:02}", "waiting", "root")
            name, assignment = task(db, "lost", "reserved", "root")
        config = root / "runner.json"
        config.write_text(json.dumps({"runner_id": RUNNER, "host_id": HOST, "slot_limit": 1, "repos": [], "agents": [AGENT]}))
        probe = root / "unexpected-app-server"
        command = [runner_bin, "--config", config, "--db", root / "runner.sqlite",
                   "--app-server", sys.executable, "-c",
                   f"from pathlib import Path; Path({str(probe)!r}).touch()"]

        def once(uri, check=True):
            return subprocess.run([command[0], "--once", *command[1:]],
                                  env={**env, "TEND_URL": f"http://{uri}"}, check=check, capture_output=True, timeout=8)

        with lose_first_response(address, lambda body: bool(body.get("assignments"))) as (proxy, sent):
            assert once(proxy, check=False).returncode != 0
            assert len(sent) == 1 and sent[0][2]["assignments"][0]["task_id"] == name, sent
        receipt = api("issues/root/backlog", {"operation_id": "park"})
        assert len(receipt["stop_control_ids"]) == 33
        reply = sync()
        [old] = reply["assignments"]
        assert old["placement"]["assignment_id"] == assignment
        matching = [control for control in reply["controls"] if control["assignment_id"] == assignment]
        assert len(matching) == 1, "retransmitted assignment lacks its original Stop"
        [paired] = matching
        assert len(reply["controls"]) == 33 and api(f"controls/{paired['id']}")["state"] == "pending"
        once(address)
        # Lose connectivity before the queued-execution loop could use the
        # first persisted copy. The actual Runner has already fenced it.
        with closing(sqlite3.connect(root / "runner.sqlite")) as local:
            assert local.execute("SELECT execution_state FROM received_assignments WHERE id=?", (assignment,)).fetchone() == ("cancelled",)
            assert local.execute("SELECT count(*) FROM local_runs").fetchone() == (0,)
        disconnected = root / "disconnected.log"
        with disconnected.open("wb") as stderr:
            process = subprocess.Popen(command, env={**env, "TEND_URL": "http://127.0.0.1:0"}, stderr=stderr)
            try:
                wait_for(lambda: disconnected.read_text().count(f"Runner {RUNNER} sync:"), lambda count: count >= 2)
                assert process.poll() is None and not probe.exists()
                with closing(sqlite3.connect(root / "runner.sqlite")) as local:
                    assert local.execute("SELECT count(*) FROM local_runs").fetchone() == (0,)
            finally:
                process.terminate()
                process.wait(timeout=5)
        once(address)
        assert api(f"controls/{paired['id']}")["state"] == "stopped"
        with ledger() as db:
            assert db.execute("SELECT state FROM runner_assignments WHERE id=?", (assignment,)).fetchone() == ("accepted",)
            assert db.execute("SELECT status FROM tasks WHERE id=?", (name,)).fetchone() == ("cancelled",)
            assert db.execute("SELECT count(*) FROM task_runs").fetchone() == (0,)


def natural_completion_and_required(server_bin):
    with fixture(server_bin) as (_, _, _, api, sync, ledger):
        with ledger() as db:
            name, assignment = task(db, "owner", "accepted")
            add_child_fact(db, name, "child-before-stop")
            add_required(db, name)
        run = {"event_id": "started", "run_id": "run", "assignment_id": assignment, "task_id": name,
               "thread_id": "thread", "turn_id": "turn", "phase": {"state": "started"}}
        assert sync({"kind": "run", "data": run})["confirmed_runs"] == ["started"]
        assert api(f"tasks/{name}/stop", {"operation_id": "stop"})["state"] == "pending"
        done = {**run, "event_id": "naturally-stopped", "phase": {"state": "stopped", "outcome": "completed"}}
        reply = sync({"kind": "run", "data": done})
        assert reply["confirmed_runs"] == ["naturally-stopped"] and not reply.get("inputs"), reply
        assert api("controls/stop")["state"] == "pending"
        with ledger() as db:
            tasks = db.execute("SELECT id,status FROM tasks").fetchall()
            assert tasks == [(name, "completed")], tasks
            assert db.execute("SELECT count(*) FROM external_route_deliveries").fetchone() == (0,)
        for _ in range(2):
            reply = sync(ack("stop", assignment))
            assert reply["confirmed_operations"] == ["proof"] and not reply.get("inputs"), reply
        with ledger() as db:
            assert db.execute("SELECT id,status FROM tasks").fetchall() == [(name, "completed")]
            assert db.execute("SELECT id,task_id,run_id FROM issue_inputs ORDER BY id").fetchall() == [
                ("child-before-stop", name, None), ("github", name, None),
            ]
            assert db.execute("SELECT route_id,fact_id,origin_issue FROM external_route_deliveries").fetchall() == [
                ("route", "fact", "owner"),
            ]
        # An independently requested continuation may inherit ordinary input
        # but must not consume the already rerouted required delivery again.
        api("issues/owner/messages", {"operation_id": "explicit-message", "text": "Continue separately"})
        with ledger() as db:
            assert db.execute("SELECT count(*) FROM tasks WHERE previous_task_id=?", (name,)).fetchone() == (1,)
            assert db.execute("SELECT task_id FROM issue_inputs WHERE id='github'").fetchone() == (name,)


def required_on_stop_or_park(server_bin, park):
    with fixture(server_bin) as (_, _, _, api, sync, ledger):
        with ledger() as db:
            name, assignment = task(db, "owner", "waiting")
            add_required(db, name)
        if park:
            [command] = api("issues/owner/backlog", {"operation_id": "park"})["stop_control_ids"]
        else:
            command = api(f"tasks/{name}/stop", {"operation_id": "stop"})["operation_id"]
        for _ in range(2):
            settled = sync(ack(command, assignment))
            assert settled["confirmed_operations"] == ["proof"] and not settled.get("inputs"), settled
        with ledger() as db:
            assert db.execute("SELECT id,status FROM tasks").fetchall() == [(name, "cancelled")]
            assert db.execute("SELECT task_id,run_id FROM issue_inputs WHERE id='github'").fetchall() == [(name, None)]
            actual = db.execute("SELECT route_id,fact_id FROM external_route_deliveries").fetchall()
            assert actual == ([] if park else [("route", "fact")]), actual


def mixed_pages(server_bin, comments):
    with fixture(server_bin) as (_, _, _, api, sync, ledger):
        with ledger() as db:
            name, assignment = task(db, "owner", "waiting")
            add_child_fact(db, name, "child-1")
            add_required(db, name)
            db.execute("INSERT INTO pulls(url,repository,number,provider) VALUES(?,'fixture/project',1,'codebase')", (CODEBASE,))
            db.execute("INSERT INTO issue_pulls VALUES('owner',?)", (CODEBASE,))
            db.execute("INSERT INTO codebase_comment_polls(url,due_at) VALUES(?,unixepoch()+600)", (CODEBASE,))
            db.execute("INSERT INTO codebase_comment_links(issue_id,url,baseline_at) VALUES('owner',?,unixepoch())", (CODEBASE,))
            due = time.time_ns() // 1_000_000 + 300_000
            db.execute("INSERT INTO codebase_comment_batches(issue_id,due_at,retry_at) VALUES('owner',?,?)", (due, due))
            expected = [f"comment-{i:02}" for i in range(comments)]
            for label in expected:
                fact = {"id": label, "source": "thread_comment", "repository": "fixture/project", "number": 1,
                        "url": CODEBASE, "created_at": "2026-11-26T00:00:00Z", "body": label}
                db.execute("INSERT INTO codebase_comment_facts VALUES(?,?,?)", (CODEBASE, label, json.dumps(fact)))
                db.execute("INSERT INTO codebase_comment_sightings(issue_id,url,comment_key,input_id) VALUES('owner',?,?,?)",
                           (CODEBASE, label, label))
                db.execute("INSERT INTO issue_input_order(input_id) VALUES(?)", (label,))
            add_child_fact(db, name, "child-2")
        assert not sync().get("inputs")
        message = {"operation_id": "barrier", "text": "C"}
        user = api("issues/owner/messages", message)
        with ledger() as db:
            add_child_fact(db, name, "child-3")
        assert api("issues/owner/messages", message) == user
        unsupported = sync(capable=False)["inputs"]
        assert [row["input_id"] for row in unsupported[:-1]] == ["child-1", "github", "child-2"]
        assert unsupported[-1]["message"]["id"] == user["message_id"]
        supported = sync()["inputs"]
        ids = [row["input_id"] for row in supported]
        assert ids == ["child-1", "github", *expected, "child-2", unsupported[-1]["input_id"]], ids
        run = {"event_id": "start-prefix", "run_id": "prefix", "assignment_id": assignment, "task_id": name,
               "thread_id": "thread", "turn_id": "turn", "input_ids": ids, "phase": {"state": "started"}}
        done = {**run, "event_id": "stop-prefix", "phase": {"state": "stopped", "outcome": "completed",
                                                           "waiting_for_children": True}}
        after = sync({"kind": "run", "data": run}, {"kind": "run", "data": done})
        assert after["confirmed_runs"] == ["start-prefix", "stop-prefix"] and not after.get("inputs"), after


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "runner"):
        parser.add_argument(name, type=lambda p: Path(p).resolve())
    args = parser.parse_args()
    issued_pair(args.server, args.runner)
    natural_completion_and_required(args.server)
    for park in (False, True):
        required_on_stop_or_park(args.server, park)
    for comments in (29, 33):
        mixed_pages(args.server, comments)
    print("PASS lost issued reply paired Stop, natural completion and required fallback, mixed atomic prefixes across sync pages")


if __name__ == "__main__":
    main()
