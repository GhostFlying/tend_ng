"""Exercise Issue discussion, original owner delivery, and lazy native history."""
import argparse
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import sys
import tempfile

from chat import server, wait_for

TESTS = Path(__file__).resolve().parents[1]
WEB = TESTS.parent / "web"
REQUEST = "请整理示例仓库的快速上手指南。"
RELAY = "请让负责指南的 Agent 补一段常见问题。"
FIRST = "初稿已整理，接下来核对首次运行步骤。"
REPLY = "请把验证命令也放进去。"
FOLLOWUP = "谢谢，命令适用于 macOS 吗？"
ANSWER = "验证命令已补进指南。"
FOLLOWUP_ANSWER = "适用于 macOS，使用终端执行即可。"
DETAIL = "核对安装步骤和验证命令。"


def free_port():
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        return f"127.0.0.1:{reservation.getsockname()[1]}"


def prepare(root, inspect):
    source = root / "source"
    subprocess.run(["git", "init", "--quiet", "-b", "main", source], check=True)
    (source / "README.md").write_text("Fixture quickstart repository.\n")
    subprocess.run(["git", "-C", source, "add", "README.md"], check=True)
    subprocess.run(["git", "-C", source, "-c", "user.name=Fixture",
                    "-c", "user.email=fixture@example.invalid", "commit",
                    "--quiet", "-m", "fixture"], check=True)
    subprocess.run(["git", "clone", "--quiet", source, root / "example"], check=True)
    (root / "runner.json").write_text(json.dumps({
        "host_id": "demo-host", "runner_id": "demo-runner", "slot_limit": 1,
        "inspect": {"listen": inspect, "url": f"http://{inspect}"},
        "repos": [{"id": "example", "path": "example"}],
        "agents": [
            {"id": "coordinator", "category": "issue-router", "revision": "one",
             "model": "fixture-coordinator"},
            {"id": "worker", "category": "implementation", "revision": "one",
             "model": "fixture-worker", "capabilities": ["image_gen"]},
        ],
    }))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("server", "cli", "runner"):
        parser.add_argument(executable, type=lambda value: Path(value).resolve())
    parser.add_argument("--web", action="store_true")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-issue-chat-") as directory:
        root = Path(directory)
        address, inspect = free_port(), free_port()
        prepare(root, inspect)
        env = {**os.environ, "TEND_TOKEN": "issue-chat-fixture", "TEND_URL": f"http://{address}"}

        def cli(*words, error=None):
            result = subprocess.run([args.cli, *words], env=env, text=True,
                                    capture_output=True, timeout=10)
            if error is not None:
                assert result.returncode and error in result.stderr, result.stderr
                return None
            assert result.returncode == 0, result.stderr
            return json.loads(result.stdout)

        def rows(issue):
            return cli("issue", "messages", issue)["messages"]

        def wait_message(issue, text):
            return wait_for(lambda: next((row for row in rows(issue) if row["text"] == text), None),
                            bool)

        def sql(database, statement, *params):
            with sqlite3.connect(root / f"{database}.sqlite") as conn:
                return conn.execute(statement, params).fetchall()

        with server(args.server, root, env, listen=address, web_dir=WEB / "dist" if args.web else None):
            with (root / "runner.stderr").open("w") as stderr:
                runner = subprocess.Popen([
                    args.runner, "--config", root / "runner.json", "--db", root / "runner.sqlite",
                    "--app-server", sys.executable, TESTS / "fixtures/coordination_app_server.py",
                    root, "issue-chat",
                ], env=env, stderr=stderr)
                try:
                    original = cli("chat", "send", "main", REQUEST)["message_id"]
                    messages = wait_for(lambda: cli("chat", "read", "main")["messages"],
                                        lambda messages: len(messages) == 2)
                    [parent] = messages[0]["issue_ids"]
                    first = wait_message(parent, FIRST)
                    child = first["issue_id"]
                    assert child != parent and first["author"] == "worker"
                    assert first == wait_message(child, FIRST)
                    assert len([row for row in rows(parent) if row["id"] == first["id"]]) == 1
                    assert rows(parent)[0]["source_message_id"] == original
                    assert cli("issue", "read", child)["status"] == "running"
                    assert not (root / "inspection.jsonl").exists()

                    command = ("issue", "message", parent, REPLY, "issue-chat-reply", "--reply-to", first["id"])
                    reply = cli(*command)["message_id"]
                    assert cli(*command)["message_id"] == reply
                    cli("issue", "message", parent, "other text", "issue-chat-reply", error="HTTP 409")
                    cli("issue", "message", child, "unrelated quote", "--reply-to", rows(parent)[0]["id"],
                        error="HTTP 404")
                    local = wait_for(lambda: sql("runner", "SELECT run_id FROM local_issue_inputs "
                                                 "WHERE json_extract(payload, '$.message.id') = ?", reply),
                                     bool)
                    assert local == [(None,)], local
                    assert wait_message(child, REPLY)["reply_to"] == first["id"]
                    (root / "finish-first").touch()
                    answer = wait_message(parent, ANSWER)
                    assert answer["reply_to"] == reply and answer["issue_id"] == child
                    assert answer["run_id"] != first["run_id"]
                    wait_for(lambda: cli("issue", "read", child),
                             lambda issue: issue["status"] == "completed")
                    assert cli("issue", "read", child)["issue_state"] == "closed"
                    assert not (root / "inspection.jsonl").exists()

                    notices = cli("notification", "list", "main")["activities"]
                    child_activity = [item for item in notices if item["issue_id"] == child]
                    edges = {(item["previous"], item["current"]) for item in child_activity}
                    assert {("running", "waiting"), ("waiting", "running"),
                            ("running", "completed"), ("open", "closed")} <= edges, edges
                    assert len([item for item in child_activity if item["current"] == "closed"]) == 1
                    assert cli("notification", "list", "other")["activities"] == []
                    cli("notification", "list", "main", "0", error="HTTP 400")
                    cursor = str(notices[0]["seq"])
                    assert cli("notification", "list", "main", cursor)["activities"] == notices[1:]
                    assert cli("notification", "list", "main", str(int(cursor) + 1))["activities"] == notices
                    if args.web:
                        subprocess.run(["node", WEB / "tests/issue-chat.mjs"], check=True, timeout=45,
                                       env={**env, "TEND_CHILD_ISSUE": child})
                    else:
                        rollout = cli("run", "rollout", first["run_id"])
                        assert rollout["runner_id"] == "demo-runner"
                        assert rollout["turn"]["items"][1]["text"] == DETAIL
                        cli("issue", "message", parent, FOLLOWUP, "--reply-to", answer["id"])
                    assert wait_message(parent, FOLLOWUP_ANSWER)["issue_id"] == child
                    assert cli("issue", "read", child)["issue_state"] == "closed"
                    assert len((root / "inspection.jsonl").read_text().splitlines()) == 1
                    assert not any(row["text"] == DETAIL for row in rows(parent))
                    wait_for(lambda: cli("issue", "read", child),
                             lambda issue: issue["status"] == "completed")
                    relayed = cli("chat", "send", "main", RELAY, "--reply-to", messages[1]["id"])["message_id"]
                    relay = wait_message(parent, RELAY)
                    assert relay["source_message_id"] == relayed and relay["kind"] == "relay"
                    forwarded = wait_message(child, RELAY)
                    assert forwarded["source_message_id"] == relayed
                    assert forwarded["forwarded_from"] == relay["id"]
                    assert forwarded["author"] == "coordinator"
                    assert wait_message(child, "常见问题已补进指南。")["reply_to"] == forwarded["id"]
                    assert len([row for row in rows(parent) if row["text"] == RELAY]) == 2
                    assert len([row for row in rows(child) if row["text"] == RELAY]) == 1
                    inputs = wait_for(lambda: (root / "owner-inputs.jsonl").read_text().splitlines(),
                                      lambda inputs: any(json.loads(item).get("message", {}).get("id") ==
                                                         relay["id"] for item in inputs))
                    assert [item["message"]["id"] for line in inputs
                            if (item := json.loads(line))["kind"] == "issue_message"].count(first["id"]) == 1
                except BaseException:
                    print((root / "runner.stderr").read_text(), end="", file=sys.stderr)
                    raise
                finally:
                    runner.terminate()
                    runner.wait(timeout=8)
            cli("run", "rollout", first["run_id"], error="HTTP 503")
            assert wait_message(parent, FIRST) == first
    print("PASS Issue discussion with queued and late replies, Router relay, and opt-in original Runner history")


if __name__ == "__main__":
    main()
