"""Exercise real Issue rename permissions, offline receipts, replay and read-only Web."""
import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
from threading import Event
import urllib.error
import urllib.request

from chat import lose_first_response, server, wait_for
from issue_chat import TESTS, WEB, free_port, prepare

sys.path.insert(0, str(TESTS / "fixtures"))
import issue_title_app_server as sample

FIXTURE = TESTS / "fixtures/issue_title_app_server.py"


class Fixture:
    def __init__(self, args, root):
        self.args = args
        self.root = root
        self.address = free_port()
        prepare(root, free_port())
        config = root / "runner.json"
        self.settings = json.loads(config.read_text())
        self.settings.pop("inspect")
        config.write_text(json.dumps(self.settings))
        self.env = {**os.environ, "TEND_URL": f"http://{self.address}",
                    "TEND_TOKEN": "issue-title-fixture", "CODEX_HOME": str(root / "codex")}
        (root / "codex").mkdir()

    def start_server(self, binary=None):
        return server(binary or self.args.server, self.root, self.env, listen=self.address,
                      web_dir=WEB / "dist" if self.args.web else None,
                      router_fixture=FIXTURE, router_name="Tend")

    def command(self, *flags, binary=None):
        # Registration and execution must freeze the same fixture launcher.
        return [binary or self.args.runner, "--config", self.root / "runner.json",
                "--db", self.root / "runner.sqlite", *flags,
                "--app-server", sys.executable, FIXTURE, self.root]

    @contextmanager
    def run(self, proxy=None, binary=None):
        env = {**self.env, "TEND_URL": f"http://{proxy or self.address}"}
        with (self.root / "runner.log").open("w+") as log:
            child = subprocess.Popen(self.command(binary=binary), env=env, stderr=log)
            try:
                yield
            except BaseException:
                log.seek(0)
                print(log.read(), file=sys.stderr)
                raise
            finally:
                child.terminate()
                child.wait(timeout=8)

    def cli(self, *words, error=None):
        reply = subprocess.run([self.args.cli, *words], env=self.env, capture_output=True,
                               text=True, timeout=10)
        if error is not None:
            assert reply.returncode and error in reply.stderr, reply.stderr
            return None
        assert reply.returncode == 0, reply.stderr
        return json.loads(reply.stdout)

    def request(self, path, body=None, status=200):
        request = urllib.request.Request(self.env["TEND_URL"] + path,
                                         data=None if body is None else json.dumps(body).encode(),
                                         headers={"Content-Type": "application/json",
                                                  "Authorization": f"Bearer {self.env['TEND_TOKEN']}"})
        try:
            with urllib.request.urlopen(request, timeout=5) as reply:
                assert reply.status == status, reply.status
                return json.load(reply)
        except urllib.error.HTTPError as error:
            assert error.code == status, (path, error.code, error.read())
            return json.load(error)

    def read(self, name):
        file = self.root / name
        wait_for(file.exists, bool)
        return json.loads(file.read_text())

    def gate(self, name):
        (self.root / name).touch()

    def sql(self, statement, *values, database="server"):
        with sqlite3.connect(self.root / f"{database}.sqlite") as db:
            return db.execute(statement, values).fetchall()

    def title(self, issue):
        return self.cli("issue", "read", issue)["title"]

    def history(self, issue, *cursor):
        return self.cli("issue", "title-history", issue, *cursor)

    def sync(self, profile, *, protocol=2, status=200, **records):
        runner = profile["runner_id"]
        return self.request(f"/api/v1/runners/{runner}/sync",
                            {"protocol": protocol, "profile": profile, "receipts": [], **records}, status)

    def send(self, text):
        return self.cli("chat", "send", "main", text)["message_id"]

    def settled_router(self, message):
        wait_for(lambda: next((row["router_state"] for row in self.cli("chat", "read", "main")["messages"]
                               if row["id"] == message), None), lambda value: value == "completed")

    def owners_ready(self):
        return wait_for(lambda: self.cli("runner", "list"), bool)

    def reconcile(self, task, address=None):
        subprocess.run(self.command("--reconcile", task), check=True, timeout=15,
                       env={**self.env, "TEND_URL": f"http://{address or self.address}"})

    def unchanged_by_rename(self):
        return (
            self.sql("SELECT id, source_seq, objective, state, parent_issue_id, board_stage, "
                     "board_entered_at, board_ready_at FROM issues ORDER BY rowid"),
            *[self.sql(f"SELECT * FROM {name} ORDER BY rowid") for name in (
                "tasks", "task_runs", "runner_assignments", "issue_short_numbers", "issue_inputs",
                "router_inputs", "chat_messages", "issue_messages", "router_dispatches")],
            self.cli("notification", "list", "main"),
        )


def exercise(fixture):
    f = fixture
    with f.start_server():
        source_recorded = f.send(sample.RECORD)
        recorded = f.read("recorded.json")
        backlog = f"#{recorded['short_number']}"
        preclaim = f.cli("issue", "rename", backlog, "拟定发布说明结构并保留验证结果", "preclaim")
        assert preclaim["old_title"] == recorded["title"]
        assert preclaim["actor"] == {"kind": "user"}
        assert f.title(backlog) == preclaim["new_title"]
        assert f.cli("issue", "read", backlog).get("task_id") is None
        f.gate("read-recorded-receipt")
        assert f.read("recorded-replayed.json")["title"] == recorded["title"]
        f.settled_router(source_recorded)

        subprocess.run(f.command("--once"), check=True, timeout=10, env=f.env)
        [runner] = f.owners_ready()
        profile = runner["profile"]
        assert runner["protocol"] == 2
        source = f.send(sample.ROOT)
        dispatch = f.read("root-dispatch.json")
        parent = dispatch["issue_id"]
        [issued] = f.sync(profile)["assignments"]
        assignment = issued["placement"]["assignment_id"]
        root_call = {"operation_id": "preclaim-spoof", "assignment_id": assignment, "run_id": "not-a-run",
                     "action": {"kind": "rename_issue", "issue_id": parent, "title": "Not claimed"}}
        f.sync(profile, status=409, records=[{"kind": "operation", "data": root_call}])
        assert f.title(parent) == sample.ROOT_ORIGINAL
        assert "owner_agent_id" not in f.request(f"/api/v1/issues/{parent}/overview")
        assert f.history(parent) == {"changes": []}

        offline = Event()
        drop = Event()
        with lose_first_response(f.address, drop_when=lambda reply: drop.is_set()
                                 and bool(reply.get("confirmed_operations")),
                                 block_requests=lambda _: offline.is_set()) as (proxy, posted):
            with f.run(proxy):
                delegated = f.read("delegated.json")
                child = delegated["issue_id"]
                task = delegated["task_id"]
                contract = f.read("worker-contract.json")
                worker = contract["placement"]
                detail = wait_for(lambda: f.cli("issue", "read", child),
                                  lambda row: row["status"] == "running")
                original_run = detail["run"]["run_id"]
                child_number = str(detail["short_number"])
                assert f.title(parent) == sample.ROOT_RENAMED
                assert f.history(parent)["changes"][0]["actor"] == {"kind": "owner", "agent_id": "coordinator"}
                f.gate("read-root-receipt")
                assert f.read("root-replayed.json")["title"] == sample.ROOT_ORIGINAL
                f.settled_router(source)

                root_receipt = f.read("root-rename.json")
                cross = {**root_receipt, "operation_id": "wrong-target",
                         "action": {**root_receipt["action"], "issue_id": child}}
                f.sync(profile, status=409, records=[{"kind": "operation", "data": cross}])
                wrong_assignment = {**cross, "operation_id": "wrong-assignment",
                                    "assignment_id": worker["assignment_id"]}
                f.sync(profile, status=409, records=[{"kind": "operation", "data": wrong_assignment}])
                foreign = {**profile, "runner_id": "different-runner"}
                f.sync(foreign, status=409, records=[{"kind": "operation", "data": root_receipt}])
                assert f.history(child) == {"changes": []}

                # Notifications can dynamically label older events with the current title.
                before = f.unchanged_by_rename()
                drop.set()
                f.gate("rename-online")
                first, no_change = f.read("online.json")
                for saved in (first, no_change):
                    identity = saved["operation_id"]
                    wait_for(lambda: f.sql("SELECT count(*) FROM run_outbox WHERE id=? AND acknowledged=1",
                                           identity, database="runner"), lambda count: count == [(1,)])
                    assert saved["assignment_id"] == worker["assignment_id"] and saved["run_id"] == original_run
                    assert saved["action"] == {"kind": "rename_issue", "issue_id": child,
                                               "title": sample.CHILD_FIRST}
                assert sum(first["operation_id"] in reply.get("confirmed_operations", [])
                           for _, _, reply in posted) >= 2
                [change] = f.history(child)["changes"]
                assert change["operation_id"] == first["operation_id"]
                assert change["old_title"] == sample.CHILD_ORIGINAL
                assert change["new_title"] == sample.CHILD_FIRST
                assert change["actor"] == {"kind": "owner", "agent_id": "worker"}
                assert change["changed_at"] > 0
                assert f.title(child) == sample.CHILD_FIRST
                conflicting = {**first, "action": {**first["action"], "title": "Wrong replay"}}
                f.sync(profile, status=409, records=[{"kind": "operation", "data": conflicting}])
                oversized = "题" * (sample.TITLE_BYTES // 3 + 1)
                f.cli("issue", "rename", child, oversized, error="HTTP 400")
                invalid = {**first, "operation_id": "oversized-wire",
                           "action": {**first["action"], "title": oversized}}
                f.sync(profile, status=409, records=[{"kind": "operation", "data": invalid}])

                offline.set()
                f.gate("rename-offline")
                second = f.read("offline.json")
                assert second["run_id"] == original_run and second["assignment_id"] == worker["assignment_id"]
                assert f.sql("SELECT acknowledged FROM run_outbox WHERE id=?", second["operation_id"],
                             database="runner") == [(0,)]
                assert f.title(child) == sample.CHILD_FIRST
                # Incremental stream remains disconnected. A second Runner
                # command replays the original durable Task records.
                f.reconcile(task)
                assert f.title(child) == sample.CHILD_SECOND
                changes = f.history(child)["changes"]
                assert [row["operation_id"] for row in changes] == [second["operation_id"], first["operation_id"]]
                assert changes[0]["old_title"] == sample.CHILD_FIRST
                assert changes[0]["actor"] == {"kind": "owner", "agent_id": "worker"}
                [(persisted,)] = f.sql("SELECT operation FROM runner_operations WHERE id=?", second["operation_id"])
                assert json.loads(persisted) == second
                f.reconcile(task)
                assert f.history(child)["changes"] == changes

                # The user path is independently authenticated and idempotent,
                # including a no-op request replayed after a subsequent title.
                no_op = f.cli("issue", "rename", child, sample.CHILD_SECOND, "user-noop")
                receipt = f.cli("issue", "rename", f"#{child_number}", f" {sample.CHILD_USER} ", "user-rename")
                assert f.cli("issue", "rename", child, sample.CHILD_USER, "user-rename") == receipt
                assert f.cli("issue", "rename", child, sample.CHILD_SECOND, "user-noop") == no_op
                f.cli("issue", "rename", child, "another title", "user-rename", error="HTTP 409")
                f.cli("issue", "rename", backlog, sample.CHILD_USER, "user-rename", error="HTTP 409")
                f.cli("issue", "rename", child, "  ", error="HTTP 400")
                f.cli("issue", "rename", "unassigned-missing", "another title", error="HTTP 404")
                f.request(f"/api/v1/issues/{child}/title", {"operation_id": "scope",
                          "title": sample.CHILD_USER, "objective": "Do not change"}, status=400)
                new_history = f.history(child)["changes"]
                assert new_history == [receipt, *changes]
                now = f.unchanged_by_rename()
                assert before[:-1] == now[:-1], "rename changed execution, linkage, or message facts"
                notices_before, notices_after = before[-1], now[-1]
                assert {**notices_before, "activities": []} == {**notices_after, "activities": []}
                clean = lambda rows: [{key: value for key, value in row.items() if key != "issue_title"}
                                      for row in rows["activities"]]
                assert clean(notices_before) == clean(notices_after), "rename added a user notification"
                assert f.title(child) == sample.CHILD_USER
                overview = f.request(f"/api/v1/issues/%23{child_number}/overview")
                assert overview["title_history"]["changes"] == new_history
                assert overview["source_message_id"] == source
                assert overview["id"] == child and overview["objective"] == sample.CHILD_OBJECTIVE
                page = f.cli("issue", "messages", child_number)
                assert page["title"] == sample.CHILD_USER
                assert page["messages"][0]["text"] == sample.CHILD_OBJECTIVE
                assert page["messages"][0]["issue_title"] == sample.CHILD_USER
                assert contract["work"]["title"] == sample.CHILD_ORIGINAL

                # A valid reconcilable history stays sorted by the committed
                # cursor, even with several events in one second.
                for i in range(14):
                    f.cli("issue", "rename", backlog, f"准备发布说明与验证记录 {i:02d}", f"backlog-{i}")
                first_page = f.history(backlog)
                assert len(first_page["changes"]) == 12
                remaining = f.history(backlog, str(first_page["next_before"]))
                assert "next_before" not in remaining and len(remaining["changes"]) == 3
                assert remaining["changes"][-1] == preclaim
                if f.args.web:
                    subprocess.run(["node", WEB / "tests/issue-title.mjs"], check=True, timeout=35,
                                   env={**f.env, "TITLE_ISSUE": child_number, "TITLE_SOURCE": source,
                                        "TITLE_BACKLOG": backlog[1:]})
                offline.clear()
                f.gate("finish-worker")
                wait_for(lambda: f.cli("issue", "read", child), lambda row: row["issue_state"] == "closed")

                f.cli("issue", "start", backlog)
                fresh = f.read("started-record.json")
                assert fresh["issue_id"] == recorded["issue_id"]
                assert fresh["work"]["title"] == "准备发布说明与验证记录 13"
                assert fresh["work"]["objective"] == sample.RECORD

        f.reconcile(task)  # Runner exited and original ledger alone supplies replay.
        assert f.history(child)["changes"] == new_history
        assert f.title(child) == sample.CHILD_USER


def exercise_budget(f, *, full):
    offline = Event()
    # Disable automatic full reconciliation for the incremental-only case.
    def blocked(path):
        return offline.is_set() or (not full and "/reconcile/" in path)

    with (f.start_server(),
          lose_first_response(f.address, drop_when=lambda _: False,
                              block_requests=blocked) as (proxy, incremental),
          f.run(proxy)):
        f.owners_ready()
        f.send(sample.BUDGET)
        contract = f.read("budget-started.json")
        issue, task = contract["issue_id"], contract["task_id"]
        running = wait_for(lambda: f.cli("issue", "read", issue),
                           lambda row: row["status"] == "running")
        run = running["run"]["run_id"]
        offline.set()
        f.gate("exercise-budget")
        operations = f.read("budget-operations.json")
        ids = [op["operation_id"] for op in operations]
        assert len(ids) == len(set(ids)) == 13
        local = f.sql("SELECT o.id, b.acknowledged FROM local_work_operations o "
                      "JOIN run_outbox b ON b.id=o.id WHERE o.run_id=? ORDER BY o.rowid",
                      run, database="runner")
        assert local == [(op, 0) for op in ids], local
        assert f.sql("SELECT call_id FROM local_work_operations WHERE run_id=? "
                     "AND call_id IN ('oversized-title','multibyte-title')", run, database="runner") == []
        assert f.history(issue) == {"changes": []}

        if full:
            with lose_first_response(f.address, drop_when=lambda _: False) as (observer, posted):
                f.reconcile(task, observer)
        else:
            posted = incremental
            offline.clear()

        closed = wait_for(lambda: f.cli("issue", "read", issue),
                          lambda row: row["issue_state"] == "closed")
        assert closed["title"] == sample.boundary_title(5)
        history = f.history(issue)["changes"]
        renames = [op["operation_id"] for op in operations if op["action"]["kind"] == "rename_issue"]
        assert [row["operation_id"] for row in history] == list(reversed(renames))
        assert all(len(row["new_title"].encode()) == sample.TITLE_BYTES for row in history)
        assert [op for (op,) in f.sql(
            "SELECT id FROM runner_operations WHERE json_extract(operation, '$.run_id')=? ORDER BY rowid",
            run,
        )] == ids
        sent_pages = [
            ([record["data"]["operation_id"] for record in request.get("records", [])
              if record["kind"] == "operation" and record["data"]["operation_id"] in ids], status)
            for request, status, _ in posted
        ]
        sent_pages = [(page, status) for page, status in sent_pages if page]
        assert len(sent_pages) >= 2 and all(status == 200 for _, status in sent_pages), sent_pages
        assert [op for page, _ in sent_pages for op in page] == ids, sent_pages
        f.reconcile(task)
        assert f.history(issue)["changes"] == history


def mixed_versions(f, legacy_server, legacy_runner):
    if legacy_server:
        with f.start_server(legacy_server), f.run():
            f.owners_ready()
            message = f.send(sample.LEGACY)
            before = f.read("new-worker-False.json")
            wait_for(lambda: f.cli("issue", "read", before["issue_id"]),
                     lambda issue: issue["issue_state"] == "closed")
            f.settled_router(message)
            f.cli("issue", "rename", before["issue_id"], "Unsupported", error="HTTP 404")
            assert f.sql("SELECT id FROM local_work_operations WHERE json_extract(operation, '$.action.kind') = 'rename_issue'",
                         database="runner") == []
        f.gate("expect-rename")
        with f.start_server(), f.run():
            # Wait for ordered negotiation on the pre-upgrade Runner database.
            wait_for(lambda: f.sql("SELECT name FROM local_server_features", database="runner"),
                     lambda rows: rows == [("issue_rename",)])
            f.send(sample.LEGACY)
            after = f.read("new-worker-True.json")
            wait_for(lambda: f.title(after["issue_id"]), lambda title: title == "Renamed after Server upgrade")
            assert f.title(before["issue_id"]) == "Compatibility fixture"
            assert f.history(before["issue_id"]) == {"changes": []}
            assert len(f.history(after["issue_id"])["changes"]) == 1
        (f.root / "expect-rename").unlink()
    if legacy_runner:
        f.gate("old-runner")
        with f.start_server(), f.run(binary=legacy_runner):
            f.owners_ready()
            f.send(sample.LEGACY)
            old = f.read("legacy-worker-False.json")
            wait_for(lambda: f.cli("issue", "read", old["issue_id"]),
                     lambda issue: issue["issue_state"] == "closed")
            assert f.title(old["issue_id"]) == "Compatibility fixture"
            assert f.history(old["issue_id"]) == {"changes": []}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "cli", "runner", "--legacy-server", "--legacy-runner"):
        parser.add_argument(name, type=lambda path: Path(path).resolve())
    parser.add_argument("--web", action="store_true")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-issue-title-") as tmp:
        exercise(Fixture(args, Path(tmp)))
    print("PASS Issue title Runner origin, incremental and full replay, user receipts, no wake or notice")
    for full in (False, True):
        with tempfile.TemporaryDirectory(prefix="tend-issue-title-budget-") as tmp:
            exercise_budget(Fixture(args, Path(tmp)), full=full)
        scope = "full reconciliation" if full else "incremental sync"
        print(f"PASS rejected oversized title and ordered split records through {scope}")
    if args.legacy_server:
        with tempfile.TemporaryDirectory(prefix="tend-issue-title-old-server-") as tmp:
            mixed_versions(Fixture(args, Path(tmp)), args.legacy_server, None)
        print("PASS new Runner with the old Server and the same ledger across Server upgrade")
    if args.legacy_runner:
        with tempfile.TemporaryDirectory(prefix="tend-issue-title-old-runner-") as tmp:
            mixed_versions(Fixture(args, Path(tmp)), None, args.legacy_runner)
        print("PASS old Runner with the new Server, without Issue rename")


if __name__ == "__main__":
    main()
