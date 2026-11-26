"""Check GitHub registration, observation, and the read-only Issue board."""

import argparse
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile

from child_windows import expire_child_window
from chat import server, wait_for
from issue_chat import free_port, prepare, REQUEST, TESTS, WEB

PULL = "https://github.com/example/guide/pull/73"
OLD_HEAD = "https://github.com/example/guide/pull/74"
DENIED = "https://github.com/example/guide/pull/99"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("server", "cli", "runner"):
        parser.add_argument(executable, type=lambda value: Path(value).resolve())
    parser.add_argument("--web", action="store_true")
    args = parser.parse_args()

    with tempfile.TemporaryDirectory(prefix="tend-github-board-") as directory:
        root = Path(directory)
        address = free_port()
        prepare(root, free_port(), "A fixed repository.\n")
        runner_config = json.loads((root / "runner.json").read_text())
        runner_config["agents"][0]["reasoning_effort"] = "low"
        (root / "runner.json").write_text(json.dumps(runner_config))
        (root / "delegate-now").touch()
        env = {**os.environ, "TEND_TOKEN": "pull-board-fixture", "TEND_URL": f"http://{address}"}

        def cli(*words, error=None):
            result = subprocess.run([args.cli, *words], env=env, text=True,
                                    capture_output=True, timeout=5)
            if error:
                assert result.returncode and error in result.stderr, result.stderr
                return None
            assert result.returncode == 0, result.stderr
            return json.loads(result.stdout)

        def links(issue):
            return {entry["url"]: entry for entry in cli("pr", "list", issue)}

        def calls():
            log = root / "github-reads"
            return log.read_text().splitlines() if log.exists() else []

        server_env = {**env, "GH_TOKEN": "fixture-read-token", "TEND_GITHUB_FIXTURE_DIR": str(root)}
        with server(args.server, root, server_env, listen=address,
                    web_dir=WEB / "dist" if args.web else None, github_cli=TESTS / "fixtures/github_cli.py"):
            assert calls() == []
            assert cli("issue", "list") == {"issues": [], "next_before": None}
            runner = subprocess.Popen([
                args.runner, "--config", root / "runner.json", "--db", root / "runner.sqlite",
                "--app-server", sys.executable, TESTS / "fixtures/coordination_app_server.py",
                root, "pull-board",
            ], env=env)
            try:
                original = cli("chat", "send", "main", REQUEST)["message_id"]
                messages = wait_for(lambda: cli("chat", "read", "main")["messages"],
                                    lambda rows: len(rows) == 2)
                [parent] = messages[0]["issue_ids"]
                wait_for(lambda: (root / "delegation.json").exists(), bool)
                child = json.loads((root / "delegation.json").read_text())["action"]["issue_id"]
                wait_for(lambda: cli("issue", "list", "main")["issues"],
                         lambda rows: any(row["id"] == child for row in rows))
                latest = wait_for(lambda: links(child).get(PULL), lambda item: item and item["facts"])
                assert latest["facts"]["head_sha"] == "a" * 40
                assert latest["facts"]["ci"] == "success"
                assert json.loads((root / "pull-link.json").read_text())["action"]["url"] == PULL

                [parent_number] = messages[0]["issue_numbers"]
                attached = cli("pr", "link", f"#{parent_number}", PULL, "pr-link")
                assert attached == {"issue_id": parent, "url": PULL}
                assert cli("pr", "link", parent, PULL + "/", "pr-link") == attached
                cli("pr", "link", parent, DENIED, "pr-link", error="HTTP 409")
                cli("pr", "link", parent, "https://github.com/example/guide/issues/99", error="HTTP 400")
                assert calls().count("73") == 1

                cli("pr", "link", child, OLD_HEAD)
                stale = wait_for(lambda: links(child).get(OLD_HEAD), lambda link: link and link["facts"])
                assert stale["facts"]["ci"] is None

                cli("pr", "link", child, DENIED)
                wait_for(calls, lambda rows: "99" in rows)
                assert cli("chat", "read", "main")["messages"] == messages
                cards = cli("issue", "list", "main")["issues"]
                assert {item["id"] for item in cards} == {parent, child}
                assert cli("issue", "list", "unrelated")["issues"] == []
                (root / "release-denied-pr").touch()
                denied = wait_for(lambda: links(child)[DENIED], lambda link: link["observation_error"])
                assert denied["facts"] is None and denied["observation_error"] == "not_accessible"

                # A new provider failure keeps the last verified state for both associations.
                (root / "deny-73").touch()
                with sqlite3.connect(root / "server.sqlite") as database:
                    database.execute("UPDATE pulls SET due_at = 0 WHERE url = ?", [PULL])
                observed = wait_for(lambda: links(child)[PULL], lambda link: link["observation_error"])
                assert observed["facts"] == latest["facts"]
                assert observed["observed_at"] == latest["observed_at"]
                assert links(parent)[PULL] == observed
                assert calls().count("73") == 2
                cli("chat", "send", "another-chat", "顺带准备另一个仓库的发布记录。")
                [other] = wait_for(lambda: cli("issue", "list", "another-chat")["issues"], bool)
                assert other["title"] == "整理发布记录"
                assert other["task_state"] == "awaiting_assignment"
                all_issues = cli("issue", "list")
                assert {item["id"] for item in all_issues["issues"]} == {parent, child, other["id"]}
                assert all_issues["next_before"] is None
                assert {item["id"] for item in cli("issue", "list", "main")["issues"]} == {parent, child}
                assert cli("issue", "list", "--all")["issues"] == []
                cli("chat", "send", "record-chat", "先记下稍后整理发布说明。")
                [record] = wait_for(lambda: cli("issue", "list", "record-chat")["issues"], bool)
                assert record["stage"] == "backlog"
                recorded = cli("issue", "read", f"#{record['short_number']}")
                assert "task_id" not in recorded and recorded["issue_id"] == record["id"]
                assert cli("issue", "messages", record["id"])["awaiting_start"]
                assert {item["id"] for item in cli("issue", "list", "--scope=roots")["issues"]} == {
                    parent, other["id"], record["id"],
                }
                expire_child_window(root, "issue_closed")
                wait_for(lambda: cli("issue", "list", "--scope=roots")["issues"],
                         lambda rows: any(row["id"] == parent and row["stage"] == "done" for row in rows))
                if args.web:
                    # Put both anchors on page three, with another page of Issue history behind it.
                    with sqlite3.connect(root / "server.sqlite") as database:
                        database.executemany(
                            "INSERT INTO chat_messages (id, chat_id, text) VALUES (?, 'main', ?)",
                            ((f"board-chat-history-{i}", f"Main Chat history {i}") for i in range(105)),
                        )
                        database.executemany(
                            "INSERT INTO issue_messages (id, issue_id, kind, author, text) "
                            "VALUES (?, ?, 'agent', 'fixture', ?)",
                            ((f"board-issue-history-{i}", parent, f"Issue history {i}") for i in range(160)),
                        )
                    browser_env = {**env, "TEND_BOARD_SOURCE": original,
                                   "TEND_BOARD_PARENT": parent,
                                   "TEND_BOARD_PARENT_NUMBER": str(next(card["short_number"] for card in cards
                                                                        if card["id"] == parent)),
                                   "TEND_BOARD_CHILD_NUMBER": str(next(card["short_number"] for card in cards
                                                                       if card["id"] == child))}
                    subprocess.run(["node", WEB / "tests/pull-board.mjs"], check=True, timeout=45, env=browser_env)
                cli("chat", "send", "record-chat", "开始刚才记下的发布说明。")
                [started] = wait_for(lambda: cli("issue", "list", "record-chat")["issues"],
                                     lambda rows: len(rows) == 1 and rows[0]["stage"] == "todo")
                assert started["id"] == record["id"]
                receipt = cli("issue", "read", record["id"])
                replay = cli("issue", "start", f"#{record['short_number']}")
                for key in ("issue_id", "short_number", "operation_id", "task_id"):
                    assert replay[key] == receipt[key], key
                assert receipt["short_number"] == record["short_number"]
                assert not cli("issue", "messages", record["id"])["awaiting_start"]
                with sqlite3.connect(root / "server.sqlite") as database:
                    assert database.execute("SELECT count(*) FROM tasks WHERE issue_id = ?", [record["id"]]).fetchone() == (1,)
            finally:
                runner.terminate()
                runner.wait(timeout=7)
    print("PASS shared Server GitHub observation, durable links, current-head CI, and paged Issue reads")


if __name__ == "__main__":
    main()
