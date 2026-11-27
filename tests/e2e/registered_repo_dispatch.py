"""Check Router repo context and direct dispatch with a fixed App Server."""

import argparse
from contextlib import closing
from http.client import HTTPConnection
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile

from chat import check_answer, server, wait_for


REVIEW = "review Lynx MR !75573"
FOLLOWUP = "Is UnregisteredRepo in the list?"
OBJECTIVE = "Review Lynx MR !75573 read-only. Report review findings and stop. Do not modify or merge the MR."
REPOS = "Tend registered repository IDs: "


def exercise(args, root, env):
    gh = root / "gh"
    gh.write_text("#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$FIXTURE_GITHUB_LOG\"\nexit 1\n")
    gh.chmod(0o700)
    github_log = root / "github.log"
    env = {**env, "FIXTURE_GITHUB_LOG": str(github_log), "PATH": f"{root}:{env['PATH']}"}

    def cli(address, *words):
        return json.loads(subprocess.check_output(
            [args.cli, *words], env={**env, "TEND_URL": f"http://{address}"}, timeout=10,
        ))

    def register(address, runner, repos):
        payload = {
            "protocol": 2, "receipts": [], "records": [],
            "profile": {
                "runner_id": runner, "host_id": "fixture-host", "repos": repos, "agents": [],
                "slot_limit": 1, "running_slots": 0, "available_for_assignment": 1,
                "workspace_root": str(root / "workspaces"),
            },
        }
        with closing(HTTPConnection(address, timeout=5)) as connection:
            connection.request("POST", f"/api/v1/runners/{runner}/sync", json.dumps(payload), {
                "Authorization": f"Bearer {env['TEND_TOKEN']}",
                "Content-Type": "application/json",
            })
            response = connection.getresponse()
            body = json.load(response)
            assert response.status == 200, (response.status, body)

    def exchange(address, chat, text, answer, *options):
        receipt = cli(address, "chat", "send", chat, text, *options)
        rows = wait_for(
            lambda: cli(address, "chat", "read", chat)["messages"],
            lambda messages: messages and messages[-1]["text"] == answer,
        )
        exchange = [next(row for row in rows if row["id"] == receipt["message_id"]), rows[-1]]
        check_answer(exchange, receipt, text, answer)
        return exchange

    with server(args.server, root, env, github_cli=gh) as address:
        exchange(address, "empty", "Another chat.", "The other chat can continue.")
        register(address, "runner-a", ["lynx"])
        register(address, "runner-b", ["lynx", "example"])
        register(address, "runner-old", ["removed"])
        cli(address, "runner", "replace", "runner-old", "runner-b")
        _, hello = exchange(address, "main", "Hello, Tend.", "Hello from the Router.")

    # A new App Server connection must resume the saved thread for the review.
    with server(args.server, root, env, github_cli=gh) as address:
        review, _ = exchange(address, "main", REVIEW, "I’ve queued the read-only review.")
        register(address, "runner-b", ["lynx", "another"])
        _, followup = exchange(address, "main", FOLLOWUP, "No, that repository is not registered.",
                               "--reply-to", hello["id"])
        replay, _ = exchange(address, "main", "Replay once.", "The second attempt answered.",
                             "--reply-to", followup["id"])

    with closing(sqlite3.connect(root / "app.sqlite")) as app:
        empty, normal, direct, follow, first, retry = app.execute(
            "SELECT thread_id, input, context FROM start_requests ORDER BY rowid"
        ).fetchall()
        assert empty[2] is None, empty
        assert [row[1] for row in (normal, direct, follow, first, retry)] == [
            "Hello, Tend.", REVIEW, FOLLOWUP, "Replay once.", "Replay once.",
        ]
        thread = normal[0]
        assert thread == direct[0] == follow[0] == first[0] != retry[0]
        assert (thread,) in app.execute("SELECT thread_id FROM resumes").fetchall()
        for row, expected in zip((normal, direct, follow, first, retry),
                                 (["example", "lynx"],) * 2 + (["another", "lynx"],) * 3):
            assert row[2] is not None, row
            assert [json.loads(line.removeprefix(REPOS)) for line in row[2].splitlines()
                    if line.startswith(REPOS)] == [expected], row
        assert hello["text"] in follow[2] and hello["id"] in follow[2], follow
        assert hello["text"] in retry[2] and followup["id"] in retry[2], retry
        replay_record = json.loads(retry[2].splitlines()[-1])
        assert replay_record["source_message_id"] == replay["id"], replay_record
        assert replay_record["kind"] == "replay" and replay_record["committed_dispatches"] == []
        instructions, = app.execute("SELECT instructions FROM threads WHERE id = ?", (thread,)).fetchone()
        for rule in (
            "explicitly names a repository in the Tend registered repository IDs list",
            "call tend_dispatch_issue exactly once",
            "canonical repo_ids",
            "do not call tend_agent_candidates",
            "look up or verify the MR or PR",
            "pin an Agent or Runner",
            "Leave base_refs unset unless the user explicitly provides them",
            "Pass through the named MR or PR and requested outcome",
            "Work Agent uses its own repository access and permissions",
            "Preserve the user's original goal, named targets, explicit limits, and stopping point",
            "Do not add operational restrictions the user did not specify.",
            "State an implicit stopping point as the requested outcome",
            "without adding push, test, or merge restrictions the user did not specify",
            "Leave repository-specific steps needed to preserve dependencies",
            "to the Work Agent's judgment and execution within scope",
            "the actual effect would materially expand the user's goal or authorization",
            "explain the evidence for that expansion",
            "make the objective read-only",
            "stop after reporting findings without modifying or merging the MR or PR",
        ):
            assert rule in instructions, rule
        [(call_thread, turn_id, tool)] = app.execute(
            "SELECT thread_id, turn_id, tool FROM tool_calls"
        ).fetchall()
        assert (call_thread, tool) == (thread, "tend_dispatch_issue"), (call_thread, tool)

    with closing(sqlite3.connect(root / "server.sqlite")) as db:
        [saved] = db.execute(
            "SELECT m.id, m.text, d.request, i.objective, t.repo_ids, t.base_refs, "
            "t.requested_agent, t.pinned_agent_id, t.pinned_runner_id, a.turn_id, t.category "
            "FROM router_dispatches d JOIN issues i ON i.id = d.issue_id "
            "JOIN chat_messages m ON m.seq = i.source_seq JOIN tasks t ON t.id = d.task_id "
            "JOIN router_attempts a ON a.id = d.attempt_id"
        ).fetchall()
        assert saved[:2] == (review["id"], REVIEW), saved
        request = json.loads(saved[2])
        assert request["objective"] == saved[3] == OBJECTIVE, saved
        assert request["repo_ids"] == json.loads(saved[4]) == ["lynx"], saved
        assert "agent" not in request and "base_refs" not in request, request
        assert json.loads(saved[5]) == {} and saved[6:9] == (None, None, None), saved
        assert saved[9:] == (turn_id, "investigation"), saved
        assert request["category"] == "investigation", request
        assert db.execute("SELECT count(*) FROM router_dispatches").fetchone()[0] == 1
    assert not github_log.exists(), github_log.read_text() if github_log.exists() else ""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("server", "cli"):
        parser.add_argument(executable, type=lambda value: Path(value).resolve())
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-registered-repo-") as temp:
        root = Path(temp)
        exercise(args, root, {**os.environ, "TEND_TOKEN": "registered-repo-fixture",
                              "CODEX_HOME": str(root / "codex")})
    print("PASS live registered repo context, resumed and replayed turns, and one unpinned review dispatch")


if __name__ == "__main__":
    main()
