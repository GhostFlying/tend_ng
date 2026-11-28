"""Check visibility and delivery order at a same-Issue child-window barrier."""

import argparse
from contextlib import closing
from http.client import HTTPConnection
import json
import os
from pathlib import Path
import sqlite3
import tempfile

from chat import server


def exercise(root, address, token, baseline):
    def api(method, path, payload=None):
        with closing(HTTPConnection(address, timeout=5)) as connection:
            connection.request(method, f"/api/v1/{path}",
                               None if payload is None else json.dumps(payload), {
                                   "Authorization": f"Bearer {token}",
                                   "Content-Type": "application/json",
                               })
            response = connection.getresponse()
            result = json.load(response)
            assert response.status == 200, (method, path, response.status, result)
            return result

    profile = {"runner_id": "fixture", "host_id": "fixture-host", "repos": [], "agents": [],
               "slot_limit": 1, "running_slots": 0, "available_for_assignment": 0}

    def sync(*records):
        return api("POST", "runners/fixture/sync", {
            "protocol": 2, "profile": profile, "receipts": [], "records": list(records),
        })

    sync()
    with closing(sqlite3.connect(root / "server.sqlite")) as db, db:
        db.execute("PRAGMA foreign_keys = ON")
        source = db.execute("INSERT INTO chat_messages (id, chat_id, text) "
                            "VALUES ('source', 'main', 'Synthetic work')").lastrowid
        for issue, parent in [("parent", None), ("child", "parent")]:
            db.execute("INSERT INTO issues (id, source_seq, title, objective, parent_issue_id) "
                       "VALUES (?, ?, ?, 'Synthetic work', ?)", (issue, source, issue, parent))
            db.execute("INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities) "
                       "VALUES (?, ?, 'fixture', 'accepted', '[]', '[]')", (f"{issue}-task", issue))
            db.execute("INSERT INTO runner_assignments (id, task_id, runner_id, contract, state, expires_at) "
                       "VALUES (?, ?, 'fixture', ?, 'accepted', 0)",
                       (f"{issue}-claim", f"{issue}-task", json.dumps({"placement": {"agent": {"id": issue}}})))
            db.execute("UPDATE tasks SET current_assignment = ? WHERE id = ?",
                       (f"{issue}-claim", f"{issue}-task"))

    def observation(issue, phase):
        return {"kind": "run", "data": {
            "event_id": f"{issue}-{phase['state']}", "assignment_id": f"{issue}-claim",
            "task_id": f"{issue}-task", "run_id": f"{issue}-run", "thread_id": f"{issue}-thread",
            "turn_id": f"{issue}-turn", "phase": phase,
        }}

    sync(observation("parent", {"state": "started"}),
         observation("parent", {"state": "stopped", "outcome": "completed", "waiting_for_children": True}),
         observation("child", {"state": "started"}))

    def fact(label):
        return {"kind": "operation", "data": {
            "operation_id": label, "run_id": "child-run", "assignment_id": "child-claim",
            "action": {"kind": "post_issue_message", "text": label},
        }}

    def delivery(reply):
        return [item["summary"] for item in reply.get("inputs", [])
                if item["assignment_id"] == "parent-claim"]

    def timeline():
        return [row["text"] for row in api("GET", "issues/parent/messages")["messages"]
                if row["kind"] in ("agent", "user")]

    def ledger():
        with closing(sqlite3.connect(root / "server.sqlite")) as db:
            counts = db.execute("SELECT count(*) FROM task_runs WHERE task_id = 'parent-task'").fetchone()
            assert counts == (1,), counts
            return db.execute("SELECT json_extract(payload, '$.summary'), wake_at FROM issue_inputs "
                              "WHERE task_id = 'parent-task' AND run_id IS NULL ORDER BY rowid").fetchall()

    ingress = sync(fact("E1"), fact("E2"))
    assert ingress["confirmed_operations"] == ["E1", "E2"], ingress
    before = ledger()
    assert [label for label, _ in before] == ["E1", "E2"], before
    assert before[0][1] is not None and before[0][1] == before[1][1], before
    for _ in range(2):
        assert timeline() == ["E1", "E2"]
    assert ledger() == before and not delivery(sync())

    command = {"operation_id": "barrier-1", "text": "C"}
    receipt = api("POST", "issues/parent/messages", command)
    after_c = delivery(sync())
    assert sync(fact("E3"))["confirmed_operations"] == ["E3"]
    after_e3 = ledger()
    assert [label for label, _ in after_e3] == ["E1", "E2", "C", "E3"], after_e3
    assert timeline() == ["E1", "E2", "C", "E3"]
    assert delivery(sync()) == after_c, "E3 must not join the delivered prefix"
    assert api("POST", "issues/parent/messages", command) == receipt
    assert ledger() == after_e3 and delivery(sync()) == after_c

    stop = api("POST", "tasks/parent-task/stop", {"operation_id": "stop-parent"})
    assert stop["state"] == "pending", stop
    assert [control["id"] for control in sync()["controls"]] == ["stop-parent"]
    stopped = sync({"kind": "control", "data": {
        "event_id": "stop-parent-ack", "id": "stop-parent", "assignment_id": "parent-claim",
        "state": "stopped",
    }})
    assert stopped["confirmed_operations"] == ["stop-parent-ack"] and not delivery(stopped), stopped
    assert api("GET", "controls/stop-parent")["state"] == "stopped"
    assert timeline() == ["E1", "E2", "C", "E3"]
    after_stop = ledger()
    assert [label for label, _ in after_stop] == ["E1", "E2", "C", "E3"], after_stop

    if not baseline:
        assert api("POST", "issues/parent/backlog", {"operation_id": "park-parent"})["state"] == "backlog"
        before_stop = api("GET", "issues/parent/overview")
        assert before_stop["stage"] == "in_progress"
        late = sync(fact("E4"))
        assert late["confirmed_operations"] == ["E4"] and not delivery(late), late
        [command] = late["controls"]
        assert command["task_id"] == "child-task"
        now_parked = sync(
            observation("child", {"state": "stopped", "outcome": "cancelled"}),
            {"kind": "control", "data": {
                "event_id": "stop-child-ack", "id": command["id"], "assignment_id": "child-claim",
                "state": "stopped",
            }},
        )
        assert not delivery(now_parked) and now_parked["confirmed_operations"] == ["stop-child-ack"]
        assert timeline() == ["E1", "E2", "C", "E3", "E4"]
        card = api("GET", "issues/parent/overview")
        assert card["stage"] == "backlog" and card["short_number"] == before_stop["short_number"]
        assert any(label == "E4" for label, _ in ledger())

    return after_c, before, after_e3, after_stop


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server", type=lambda value: Path(value).resolve())
    parser.add_argument("--expect-current-gap", action="store_true",
                        help="Reproduce the pre-change behavior instead of asserting the target order.")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-input-order-") as directory:
        root = Path(directory)
        env = {**os.environ, "CODEX_HOME": str(root / "fixture-codex"), "TEND_TOKEN": "isolated-input-order-fixture"}
        with server(args.server, root, env) as address:
            after_c, before, after_e3, after_stop = exercise(root, address, env["TEND_TOKEN"], args.expect_current_gap)

    if args.expect_current_gap:
        assert after_c == ["C"], after_c
        assert after_e3[-1][1] == before[0][1] == after_stop[-1][1], (before, after_e3, after_stop)
        print("REPRODUCED: E1/E2 are readable, C is offered first, and E3 joins the pre-C window; "
              "Stop leaves the window unchanged but acknowledgement prevents delivery to the stopped Task.")
    else:
        assert after_c == ["E1", "E2", "C"], f"Runner delivery after C: {after_c}"
        print("PASS earlier facts offered before C, E3 stays out of the offered prefix, "
              "and confirmed Stop retains facts without another Run")


if __name__ == "__main__":
    main()
