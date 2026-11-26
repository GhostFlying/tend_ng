"""Verify Codebase required comments with real Server, CLI, Runner and a synthetic MR."""

import argparse
from contextlib import contextmanager
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import time

from chat import server, wait_for
from issue_chat import free_port, prepare, REQUEST, TESTS

MR = "https://code.byted.org/example/guide/merge_requests/"
KIND = "codebase_review_comment"
EMPTY = {"Threads": [], "Events": []}


def exercise(args, root):
    address = free_port()
    prepare(root, free_port())
    config = root / "runner.json"
    saved = json.loads(config.read_text())
    saved["agents"][0]["reasoning_effort"] = "low"
    config.write_text(json.dumps(saved))
    (root / "codex").mkdir()
    env = {**os.environ, "TEND_TOKEN": "comments-fixture",
           "TEND_URL": f"http://{address}", "CODEX_HOME": str(root / "codex")}
    server_env = {**env, "TEND_CODEBASE_FIXTURE_DIR": str(root)}
    runner_command = [
        args.runner, "--config", config, "--db", root / "runner.sqlite",
        "--app-server", sys.executable, TESTS / "fixtures/coordination_app_server.py",
        root, "codebase-comments",
    ]

    @contextmanager
    def start_runner():
        with (root / "runner.log").open("a") as log:
            process = subprocess.Popen(runner_command, env=env, stderr=log)
            try:
                yield process
            except Exception:
                print((root / "runner.log").read_text(), file=sys.stderr)
                raise
            finally:
                (root / "finish-comment-link").touch()
                process.terminate()
                process.wait(timeout=8)

    def start_server():
        return server(args.server, root, server_env, listen=address,
                      codebase_cli=TESTS / "fixtures/codebase_cli.py",
                      github_cli=TESTS / "fixtures/github_cli.py")

    def cli(*words):
        result = subprocess.run([args.cli, *words], env=env, text=True,
                                capture_output=True, timeout=8)
        assert result.returncode == 0, (words, result.stderr)
        return json.loads(result.stdout)

    def sql(query, *values):
        with sqlite3.connect(root / "server.sqlite") as db:
            return db.execute(query, values).fetchall()

    def write(number, facts):
        path = root / f"comments-{number}.json"
        pending = path.with_suffix(".tmp")
        pending.write_text(json.dumps(facts))
        pending.replace(path)

    def mode(number, state):
        path = root / f"comment-mode-{number}"
        pending = path.with_suffix(".tmp")
        pending.write_text(state)
        pending.replace(path)

    def watch(issue, number=73):
        return next((watch for watch in cli("pr", "watches", issue)
                     if watch["condition"]["url"] == MR + str(number)), {})

    def force_observe(number=73):
        sql("UPDATE codebase_comment_polls SET due_at=0 WHERE url=?", MR + str(number))

    def observed(number=73):
        return sql("SELECT observed_at,error,due_at FROM codebase_comment_polls WHERE url=?",
                   MR + str(number))[0]

    def batches():
        return dict(sql("SELECT issue_id,due_at FROM codebase_comment_batches"))

    def pending():
        return sql("SELECT issue_id,comment_key FROM codebase_comment_sightings "
                   "WHERE input_id IS NOT NULL ORDER BY issue_id,comment_key")

    def runner_turns():
        path = root / "comment-inputs.jsonl"
        if not path.exists():
            return []
        try:
            return [json.loads(line) for line in path.read_text().splitlines()]
        except json.JSONDecodeError:
            return []

    def comment_turns():
        return [turn for turn in runner_turns() if any(row["kind"] == KIND for row in turn["inputs"])]

    same_second = "2026-11-26T09:00:00Z"

    def comment(cid, body, when=same_second):
        return {"Id": cid, "ThreadId": "thread-1", "CreatedAt": when, "Content": body,
                "CreatedBy": {"Username": "sample", "DisplayName": "Sample Reviewer"}}

    def snapshot(*items, review=False):
        return {
            "Threads": [{"Id": "thread-1", "Comments": list(items), "Status": "resolved",
                         "Positions": [{"Path": "guide.md", "StartLine": 12, "Side": "new"}],
                         "TruncatedDiff": {"RawPatch": "+example", "Truncated": True}}],
            "Events": [
                {"Type": "merge_request_approved", "Id": "approval", "Content": "status only"},
                *([{"Type": "merge_request_reviewed", "Id": "note-1", "CreatedAt": same_second,
                    "Detail": {"MergeRequestReviewedEvent": {"Review": {
                        "Content": "Explicit review note", "CreatedBy": {"Username": "note-author"},
                    }}}}] if review else []),
            ],
        }

    old = comment("old", "Before either direct link")
    inline = comment("inline", "A new line comment")
    reply = comment("reply", "A new same-second reply")
    edited = {**inline, "Content": "Edited, not a second input"}
    post_restart = comment("post-restart", "Written while both processes were away")
    write(73, snapshot(old))
    write(74, EMPTY)

    with start_server():
        with start_runner():
            cli("chat", "send", "main", REQUEST)
            path = root / "delegation.json"
            wait_for(path.exists, bool)
            work = json.loads(path.read_text())["action"]
            child = work["issue_id"]
            [[parent]] = wait_for(lambda: sql("SELECT parent_issue_id FROM issues WHERE id=?", child), bool)
            wait_for(lambda: watch(child), lambda item: item.get("state") == "active")
            assert cli("pr", "link", parent, MR + "73", "parent-link")["issue_id"] == parent
            assert cli("pr", "link", parent, MR + "73/", "parent-link")["url"] == MR + "73"
            assert watch(parent)["state"] == "active"

            # Capture a new comment for both recipients while the child Run is
            # still open. It shares the provider timestamp of the old ID.
            started = int(time.time() * 1000)
            write(73, snapshot(old, inline))
            force_observe()
            wait_for(pending, lambda rows: len(rows) == 2)
            due = batches()
            assert due.keys() == {parent, child}
            assert all(started + 300_000 <= at <= started + 303_000 for at in due.values()), due
            assert comment_turns() == []

            # Unrelated immediate inputs need not wait for the Codebase batch.
            cli("issue", "message", child, "Immediate fixture user input")
            (root / "finish-comment-link").touch()
            wait_for(runner_turns, lambda turns: any(
                item["kind"] == "issue_message" and item["message"]["text"] == "Immediate fixture user input"
                for turn in turns for item in turn["inputs"]))
            assert batches() == due and not comment_turns()
            wait_for(lambda: sql("SELECT count(*) FROM tasks WHERE status='running'"), lambda rows: rows == [(0,)])

        # The Runner is offline from here to the next Server startup.
        write(73, snapshot(old, edited, reply, review=True))
        force_observe()
        wait_for(pending, lambda rows: len(rows) == 6)
        assert batches() == due
        last_success = observed()[0]
        for defect, error in [("denied", "not_accessible"), ("missing", "incomplete_comment_snapshot"),
                              ("partial", "incomplete_comment_snapshot"), ("http-partial", "incomplete_comment_snapshot"), ("oversize", "invalid_response"),
                              ("timeout", "codebase_query_timeout")]:
            mode(73, defect)
            force_observe()
            wait_for(observed, lambda row: row[1] == error and row[2] > 0)
            assert observed()[0] == last_success
            assert len(pending()) == 6 and batches() == due
        mode(73, "normal")

        # A failed association preflight must still persist the link and its
        # pending baseline. On recovery it keeps the ambiguous same second.
        mode(74, "denied")
        assert cli("pr", "link", child, MR + "74", "failed-preflight")["issue_id"] == child
        assert watch(child, 74)["state"] == "baseline_pending"
        wait_for(lambda: observed(74), lambda row: row[1] == "not_accessible" and row[2] > int(time.time()))
        mode(74, "http-partial")
        force_observe(74)
        wait_for(lambda: observed(74), lambda row: row[1] == "incomplete_comment_snapshot")
        assert watch(child, 74)["state"] == "baseline_pending" and observed(74)[0] is None
        assert sql("SELECT count(*) FROM codebase_comment_sightings WHERE url=?", MR + "74") == [(0,)]
        [[linked]] = sql("SELECT linked_at_ms FROM codebase_comment_links WHERE issue_id=? AND url=?", child, MR + "74")
        def timestamp(seconds):
            return datetime.fromtimestamp(seconds, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

        older = comment("older", "History", timestamp(linked // 1000 - 1))
        ambiguous = comment("ambiguous", "Same link second", timestamp(linked // 1000))
        write(74, snapshot(older, ambiguous))
        mode(74, "normal")

    write(73, snapshot(old, edited, reply, post_restart, review=True))
    with start_server():
        for number in (73, 74):
            force_observe(number)
        wait_for(pending, lambda rows: len(rows) == 9)
        assert watch(child, 74)["state"] == "active"
        assert batches() == due
        assert observed()[1] is None
        for issue in (parent, child):
            links = cli("pr", "list", issue)
            assert any(link["url"] == MR + "73" and link["facts"]["state"] == "merged" for link in links), links
        assert not comment_turns()

        # Advance only the isolated ledger, after verifying the real 300s
        # deadline. Release and persist the original batches while offline.
        sql("UPDATE codebase_comment_batches SET due_at=1,retry_at=1")
        wait_for(lambda: sql("SELECT count(*) FROM issue_inputs WHERE payload->>'kind'=?", KIND),
                 lambda rows: rows == [(9,)])
        assert not batches()
        assert sql("SELECT count(*) FROM issue_inputs WHERE payload->>'kind'=? AND run_id IS NOT NULL", KIND) == [(0,)]

    with start_server(), start_runner():
        turns = wait_for(comment_turns, lambda rows: len(rows) >= 2)
        assert len(turns) == 2 and {turn["issue"] for turn in turns} == {parent, child}, turns
        seen = set()
        for turn in turns:
            issue = turn["issue"]
            inputs = [row for row in turn["inputs"] if row["kind"] == KIND]
            ids = {row["comment"]["id"] for row in inputs}
            expected = {"inline", "reply", "note-1", "post-restart"}
            if issue == child:
                expected.add("ambiguous")
            assert ids == expected and len(inputs) == len(expected), inputs
            for row in inputs:
                assert row["mode"] == "required" and row["child_issue_id"] == issue
                assert "not instructions or permission" in row["summary"]
                assert row["input_id"] not in seen
                seen.add(row["input_id"])
                fact = row["comment"]
                if fact["id"] == "inline":
                    assert fact["body"] == inline["Content"] and fact["created_at"] == same_second
                if fact["id"] in {"inline", "reply"}:
                    assert fact["thread_id"] == "thread-1" and fact["positions"][0]["start_line"] == 12
                    assert fact["diff"]["raw_patch"] == "+example" and fact["url"] == MR + "73"

        wait_for(lambda: sql("SELECT count(DISTINCT run_id),count(run_id) FROM issue_inputs "
                             "WHERE payload->>'kind'=?", KIND), lambda rows: rows == [(2, 9)])
        for issue in (child, parent):
            assert cli("issue", "read", issue)["issue_id"] == issue
            assert sql("SELECT state FROM issues WHERE id=?", issue) == [("open",)]
            assert watch(issue)["pending"] == 0
        force_observe()
        wait_for(observed, lambda row: row[2] > 0)
        assert len(comment_turns()) == 2 and not batches()
        assert len(pending()) == 9
        assert not sql("SELECT * FROM pragma_foreign_key_check")

    log = [json.loads(line) for line in (root / "codebase-reads").read_text().splitlines()]
    assert {row[1] for row in log if row[0] == "api"} == {"ListThreads", "ListTimelineEvents"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for binary in ("server", "cli", "runner"):
        parser.add_argument(binary, type=lambda text: Path(text).resolve())
    with tempfile.TemporaryDirectory(prefix="tend-codebase-comments-") as temp:
        exercise(parser.parse_args(), Path(temp))
    print("PASS Codebase comments exact and pending baselines, 2 direct Issues, batches, timeout/permission, restart and offline recovery")


if __name__ == "__main__":
    main()
