"""Check the real maintenance observer and manual destinations in a disposable Tend NG."""
import argparse
from contextlib import closing
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import time

from chat import lose_first_response, server
from discussions import sql
from issue_chat import TESTS, WEB, free_port
from web_session import call, login, web_headers

TOKEN = "fixture-maintenance-only"
ROOT = "/api/v1/maintenance-suggestions"
OWNER = "ou_maintenance_fixture"


def until(read, predicate, seconds=25):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = read()
        if predicate(value):
            return value
        time.sleep(0.2)
    raise AssertionError(value)


def run(args, root, *, restart_age=None):
    address = free_port()
    base = f"http://{address}"
    env = {**os.environ, "TEND_TOKEN": TOKEN, "TEND_URL": base,
           "CODEX_HOME": str(root / "router-home")}
    db = root / "server.sqlite"
    config = root / "feishu.json"
    config.write_text(json.dumps({"owner_open_id": OWNER, "cli": "./maintenance-feishu"}))
    fixture = root / "maintenance-feishu"
    fixture.write_text(f"#!{sys.executable}\n" +
                       (TESTS / "fixtures" / "maintenance_feishu_cli.py").read_text())
    fixture.chmod(0o700)
    old = int(time.time()) - (restart_age or 8 * 86400)

    def mode(name, release_result="failure"):
        updated = root / "scenario.tmp"
        updated.write_text(json.dumps({"mode": name, "first_through": old + 86400,
                                       "release_result": release_result}))
        updated.replace(root / "maintenance-feishu.json")

    def api(method, path, body=None, *, code=200, headers=None):
        request = {"Authorization": f"Bearer {TOKEN}"} if headers is None else headers.copy()
        if body is not None:
            request["Content-Type"] = "application/json"
        status, _, raw = call(address, method, path, request,
                              json.dumps(body) if body is not None else None)
        assert status == code, (status, method, path, raw)
        return json.loads(raw) if raw else None

    def cli(*words, upstream=address):
        out = subprocess.run([args.cli, *map(str, words)], env={**env, "TEND_URL": f"http://{upstream}"},
                             capture_output=True, text=True, timeout=20)
        assert out.returncode == 0, out.stderr
        return json.loads(out.stdout)

    def feed(view="active"):
        return api("GET", f"{ROOT}?view={view}")

    def detail(id):
        return api("GET", f"{ROOT}/{id}")["suggestion"]

    profile = {"runner_id": "maintenance-runner", "host_id": "fixture-host", "repos": [],
               "agents": [], "slot_limit": 1, "running_slots": 0, "available_for_assignment": 0}

    def sync(*records):
        return api("POST", "/api/v1/runners/maintenance-runner/sync", {
            "protocol": 2, "profile": profile, "receipts": [], "records": list(records),
        })

    def observation(subject, run, phase):
        return {"kind": "run", "data": {
            "event_id": f"{run}-{phase['state']}", "assignment_id": f"{subject}-claim",
            "task_id": f"{subject}-task", "run_id": run, "thread_id": f"{subject}-thread",
            "turn_id": f"{run}-turn", "phase": phase,
        }}

    def business():
        return [sql(db, statement) for statement in (
            "SELECT id,state,title FROM issues ORDER BY id",
            "SELECT id,status,current_assignment FROM tasks ORDER BY id",
            "SELECT id,observation FROM task_runs ORDER BY id",
            "SELECT * FROM router_inputs ORDER BY seq",
            "SELECT * FROM issue_inputs ORDER BY rowid",
        )]

    options = dict(listen=address, web_dir=WEB / "dist" if args.screenshots else None)
    if restart_age is not None:
        short_backlog = restart_age == 3600
        with server(args.server, root, env, **options):
            with closing(sqlite3.connect(db)) as conn, conn:
                conn.execute("INSERT INTO feishu_scan(owner,phase,checkpoint_at,last_success_at) "
                             "VALUES(?,'idle',?,?)", (OWNER, old, old))

        def scan_state():
            return api("GET", "/api/v1/feishu-suggestions")["scan"]

        mode("held-before-restart")
        try:
            with server(args.server, root, env, feishu_config=config, **options):
                until(lambda: (root / "maintenance-before-restart-started").exists(), bool)
                scan = scan_state()
                assert (scan["phase"], scan["gap"]) == (
                    "running", None if short_backlog else "catching_up"), scan
                original, = until(lambda: feed()["suggestions"], bool)
                assert "扫描上次退出" not in original["fault"]["known"]
                cli("maintenance-suggestions", "ignore", original["id"], original["revision"])
                assert detail(original["id"])["ignored"]
        finally:
            (root / "maintenance-before-restart-release").touch()

        mode("held-after-restart", release_result="success" if short_backlog else "failure")
        with server(args.server, root, env, feishu_config=config, **options):
            try:
                until(lambda: (root / "maintenance-after-restart-started").exists(), bool)
                stalled = until(lambda: detail(original["id"]),
                                lambda card: card["last_seen"] > original["last_seen"])
                assert stalled["revision"] == original["revision"] and stalled["ignored"], stalled
                assert stalled["fault"] == original["fault"]
                scan = scan_state()
                expected_gap = "interrupted" if short_backlog else "catching_up"
                assert (scan["phase"], scan["gap"], scan["checkpoint_at"], scan["last_success_at"]) == (
                    "running", expected_gap, old, old), scan
                assert sql(db, "SELECT gap FROM feishu_scan") == [(None,)]
            finally:
                (root / "maintenance-after-restart-release").touch()
            if short_backlog:
                complete = until(scan_state, lambda scan: scan["phase"] == "idle")
                assert complete["gap"] is None
                assert complete["checkpoint_at"] > old and complete["last_success_at"] > old
                recovered = until(lambda: detail(original["id"]), lambda card: card["status"] == "recovered")
                assert recovered["id"] == original["id"] and recovered["revision"] == original["revision"] + 1
                print("PASS short-backlog restart keeps the public interruption hint until a successful commit", flush=True)
                return
            until(scan_state, lambda scan: scan["phase"] == "failed" and scan["gap"] == "provider_unavailable")
            failed = until(lambda: detail(original["id"]),
                           lambda card: card["revision"] > original["revision"])
            assert failed["revision"] == original["revision"] + 1 and not failed["ignored"]
            assert [card["id"] for card in feed()["suggestions"]] == [original["id"]]
        print("PASS real restart preserves ignored stall without a failure until a new result", flush=True)
        return

    with server(args.server, root, env, **options):
        assert not feed()["feishu_configured"] and not feed()["suggestions"]
        sync()
        with closing(sqlite3.connect(db, timeout=5)) as conn, conn:
            conn.execute("PRAGMA foreign_keys=ON")
            src = conn.execute("INSERT INTO chat_messages(id,chat_id,text) VALUES"
                               "('fixture-topic','main','合成验证 · 后台发布索引')").lastrowid
            for key, title, status in [
                ("failed", "后台发布索引核对", "accepted"),
                ("waiting", "等待已登记检查", "accepted"),
                ("queue", "正常容量排队", "awaiting_assignment"),
                ("working", "正在生成验收报告", "accepted"),
            ]:
                conn.execute("INSERT INTO issues(id,source_seq,title,objective) VALUES(?,?,?,'fixture')",
                             (key, src, title))
                conn.execute("INSERT INTO tasks(id,issue_id,category,status,repo_ids,required_capabilities)"
                             "VALUES(?,?,'implementation',?,'[]','[]')", (key + "-task", key, status))
                if status == "accepted":
                    conn.execute("INSERT INTO runner_assignments(id,task_id,runner_id,contract,state,expires_at)"
                                 "VALUES(?,?,'maintenance-runner',?,'accepted',0)", (
                                     key + "-claim", key + "-task",
                                     json.dumps({"placement": {"agent": {"id": "FixtureAgent"}}})))
                    conn.execute("UPDATE tasks SET current_assignment=? WHERE id=?",
                                 (key + "-claim", key + "-task"))
            conn.execute("INSERT INTO feishu_scan(owner,checkpoint_at,last_success_at,phase) "
                         "VALUES(?,?,?,'running')", (OWNER, old, old))
        sync(
            observation("failed", "index-run", {"state": "started"}),
            observation("failed", "index-run", {"state": "stopped", "outcome": "failed"}),
            observation("waiting", "wait-run", {"state": "started"}),
            observation("waiting", "wait-run", {"state": "stopped", "outcome": "failed",
                                                "waiting_for_external": True}),
            observation("working", "working-run", {"state": "started"}),
        )
        assert sql(db, "SELECT id,status FROM tasks ORDER BY id") == [
            ("failed-task", "failed"), ("queue-task", "awaiting_assignment"),
            ("waiting-task", "waiting"), ("working-task", "running"),
        ]
        existing = api("POST", "/api/v1/discussions", {
            "operation_id": "fixture-discussion", "title": "已有的合成维护讨论",
        })["chat_id"]

    mode("stalled")
    with server(args.server, root, env, feishu_config=config, **options):
        baseline = business()
        assert api("GET", "/health") == {"status": "ok"}
        cards = until(feed, lambda page: len(page["suggestions"]) == 2)["suggestions"]
        scan = next(card for card in cards if card["kind"] == "feishu_scan_stalled")
        task = next(card for card in cards if card["kind"] == "task_execution_failed")
        scan_id, task_id = scan["id"], task["id"]
        assert scan["status"] == "active" and task["status"] == "active"
        assert "#message-index-run" in task["fault"]["links"][0]["href"]
        assert [link["href"] for link in task["fault"]["links"][1:]] == ["/#message-fixture-topic"]
        until(lambda: detail(scan_id), lambda card: "未能从飞书" in card["fault"]["known"])

        api("GET", ROOT, code=401, headers={})
        api("POST", f"{ROOT}/{task_id}/ignore", {"revision": 1, "ignored": True}, code=401,
            headers=web_headers(base))
        status, headers, _ = login(address, web_headers(base), False, token=TOKEN)
        assert status == 204
        cookie = headers["Set-Cookie"].split(";", 1)[0]
        api("POST", f"{ROOT}/{task_id}/handoff", {"revision": 1}, code=403,
            headers={**web_headers(base), "Origin": "https://wrong.invalid", "Cookie": cookie})
        assert cli("maintenance-suggestions", "read", task_id)["suggestion"]["id"] == task_id

        ignored_scan = detail(scan_id)
        cli("maintenance-suggestions", "ignore", scan_id, ignored_scan["revision"])
        mode("held-retry")
        api("POST", "/api/v1/feishu-suggestions/sync", {}, code=202)
        try:
            until(lambda: (root / "maintenance-retry-started").exists(), bool)
            phase = api("GET", "/api/v1/feishu-suggestions")["scan"]
            assert (phase["phase"], phase["gap"]) == ("running", "catching_up"), phase
            observed_at = detail(scan_id)["last_seen"]
            during = until(lambda: detail(scan_id), lambda card: card["last_seen"] > observed_at)
            assert during["revision"] == ignored_scan["revision"] and during["ignored"], during
            assert phase["checkpoint_at"] == old
        finally:
            (root / "maintenance-retry-release").touch()
        until(lambda: api("GET", "/api/v1/feishu-suggestions")["scan"],
              lambda scan: scan["phase"] == "failed" and scan["gap"] == "provider_unavailable")
        observed_at = detail(scan_id)["last_seen"]
        after = until(lambda: detail(scan_id), lambda card: card["last_seen"] > observed_at)
        assert after["revision"] == ignored_scan["revision"] and after["ignored"], after
        assert after["fault"] == ignored_scan["fault"]
        cli("maintenance-suggestions", "restore", scan_id, ignored_scan["revision"])
        print("PASS original ignored scan through held retry and the same terminal failure", flush=True)

        if args.screenshots:
            subprocess.run(["node", WEB / "tests" / "maintenance-suggestions.mjs"], env={
                **env, "TEND_SCREENSHOTS": str(args.screenshots), "TEND_CASE_SCAN": scan_id,
                "TEND_CASE_TASK": task_id, "TEND_EXISTING_DISCUSSION": existing,
                "TEND_MAINTENANCE_FEISHU_SCENARIO": str(root / "maintenance-feishu.json"),
            }, check=True, timeout=150, cwd=WEB)

        # Replay after a response was lost, including across the case revision.
        task = detail(task_id)
        rev = task["revision"]
        with lose_first_response(address, after_headers=True) as (proxy, posted):
            result = subprocess.run([args.cli, "maintenance-suggestions", "handoff", task_id, str(rev)],
                                    env={**env, "TEND_URL": f"http://{proxy}"}, capture_output=True, timeout=10)
            assert result.returncode and len(posted) == 1 and posted[0][1] == 200, (
                result.returncode, result.stderr, posted,
            )
        main = cli("maintenance-suggestions", "handoff", task_id, rev)
        assert main == posted[0][2]
        new = cli("maintenance-suggestions", "discuss", task_id, rev)
        applied = cli("maintenance-suggestions", "discuss", task_id, rev, existing)
        assert new["chat_id"] != existing and applied["chat_id"] == existing
        for receipt in [main, new, applied]:
            messages = api("GET", f"/api/v1/chats/{receipt['chat_id']}/messages")["messages"]
            note, = [row for row in messages if row["id"] == receipt["message_id"]]
            assert note["author"] == "maintenance" and note["router_state"] is None
            assert all(part in note["text"] for part in [
                "影响", "证据", "已知", "未知", "不授权", f"/maintenance-suggestions?id={task_id}",
            ])
        cli("maintenance-suggestions", "ignore", task_id, rev)
        assert [row["id"] for row in feed("ignored")["suggestions"]] == [task_id]
        assert all(row["id"] != task_id for row in feed()["suggestions"])
        cli("maintenance-suggestions", "restore", task_id, rev)
        assert not feed("ignored")["suggestions"]

        assert business() == baseline, "manual actions cannot change issues, tasks, runs or agent queues"
        old_note = api("GET", "/api/v1/chats/main/messages")["messages"]
        initial_scan = detail(scan_id)
        mode("partial")
        api("POST", "/api/v1/feishu-suggestions/sync", {}, code=202)
        partial = until(lambda: detail(scan_id), lambda card: card["status"] == "not_observed")
        assert partial["id"] == scan_id and partial["first_seen"] == initial_scan["first_seen"]
        assert "尚未证实" in partial["followup"]["known"]
        assert all(c["id"] != scan_id for c in feed()["suggestions"])
        mode("empty")
        api("POST", "/api/v1/feishu-suggestions/sync", {}, code=202)
        recovered = until(lambda: detail(scan_id), lambda card: card["status"] == "recovered")
        assert recovered["revision"] > partial["revision"] > initial_scan["revision"]
        assert api("GET", "/api/v1/chats/main/messages")["messages"] == old_note
        assert business() == baseline
        assert sql(db, "SELECT count(*) FROM maintenance_cases") == [(2,)]

    with server(args.server, root, env, feishu_config=config, **options):
        assert detail(scan_id)["status"] == "recovered"
        assert cli("maintenance-suggestions", "handoff", task_id, rev) == main
        assert api("GET", "/api/v1/chats/main/messages")["messages"] == old_note
        assert business() == baseline
    print("PASS real observer, backlog vs stall, proof-based recovery, auth, manual receipts and unchanged business state")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server", type=lambda value: Path(value).resolve())
    parser.add_argument("cli", type=lambda value: Path(value).resolve())
    parser.add_argument("--screenshots", type=lambda value: Path(value).resolve())
    args = parser.parse_args()
    if args.screenshots:
        args.screenshots.mkdir(parents=True, exist_ok=True)
    for age in (3600, 8 * 86400):
        with tempfile.TemporaryDirectory(prefix="tend-maintenance-restart-fixture-") as path:
            run(args, Path(path), restart_age=age)
    with tempfile.TemporaryDirectory(prefix="tend-maintenance-fixture-") as path:
        run(args, Path(path))


if __name__ == "__main__":
    main()
