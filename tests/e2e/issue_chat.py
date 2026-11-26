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

from child_windows import expire_child_window
from chat import lose_first_response, server, wait_for

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


def prepare(root, inspect, readme="Fixture quickstart repository.\n"):
    source = root / "source"
    subprocess.run(["git", "init", "--quiet", "-b", "main", source], check=True)
    (source / "README.md").write_text(readme)
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
        default_home, alternate_home, replacement_home = (
            root / name for name in ("default-codex", "alternate-codex", "replacement-codex"))
        for home in (default_home, alternate_home, replacement_home):
            home.mkdir()
        config = root / "runner.json"
        settings = json.loads(config.read_text())
        settings["agents"][1]["app_server"] = "alternate"
        settings["app_servers"] = {"alternate": {
            "program": sys.executable,
            "args": [str(TESTS / "fixtures/coordination_app_server.py"), str(root), "issue-chat"],
            "codex_home": "alternate-codex",
        }}
        config.write_text(json.dumps(settings))
        env = {**os.environ, "TEND_TOKEN": "issue-chat-fixture", "TEND_URL": f"http://{address}",
               "CODEX_HOME": str(default_home)}
        runner_command = [
            args.runner, "--config", config, "--db", root / "runner.sqlite",
            "--app-server", sys.executable, TESTS / "fixtures/coordination_app_server.py",
            root, "issue-chat",
        ]

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
                runner = subprocess.Popen(runner_command, env=env, stderr=stderr)
                try:
                    original = cli("chat", "send", "main", REQUEST)["message_id"]
                    messages = wait_for(lambda: cli("chat", "read", "main")["messages"],
                                        lambda messages: len(messages) == 2)
                    [parent] = messages[0]["issue_ids"]
                    [parent_number] = messages[0]["issue_numbers"]
                    first = wait_message(parent, FIRST)
                    child = first["issue_id"]
                    child_number = first["short_number"]
                    assert 0 < parent_number < child_number
                    assert cli("issue", "messages", str(parent_number))["short_number"] == parent_number
                    assert cli("issue", "read", f"#{child_number}")["issue_id"] == child
                    assert child != parent and first["author"] == "worker"
                    assert first == wait_message(child, FIRST)
                    assert len([row for row in rows(parent) if row["id"] == first["id"]]) == 1
                    assert rows(parent)[0]["source_message_id"] == original
                    assert cli("issue", "read", child)["status"] == "running"
                    assert not (root / "inspection.jsonl").exists()

                    command = ("issue", "message", f"#{parent_number}", REPLY, "issue-chat-reply", "--reply-to", first["id"])
                    reply = cli(*command)["message_id"]
                    assert cli("issue", "message", parent, *command[3:])["message_id"] == reply
                    cli("issue", "message", parent, "other text", "issue-chat-reply", error="HTTP 409")
                    cli("issue", "message", child, "unrelated quote", "--reply-to", rows(parent)[0]["id"],
                        error="HTTP 404")
                    local = wait_for(lambda: sql("runner", "SELECT run_id FROM local_issue_inputs "
                                                 "WHERE json_extract(payload, '$.message.id') = ?", reply),
                                     bool)
                    assert local == [(None,)], local
                    assert sql(
                        "server", "SELECT json_type(payload, '$.message.short_number') FROM issue_inputs "
                        "WHERE json_extract(payload, '$.message.id') = ?", reply,
                    ) == [(None,)]
                    assert wait_message(child, REPLY)["reply_to"] == first["id"]
                    (root / "finish-first").touch()
                    answer = wait_message(parent, ANSWER)
                    assert answer["reply_to"] == reply and answer["issue_id"] == child
                    assert answer["run_id"] != first["run_id"]
                    wait_for(lambda: cli("issue", "read", child),
                             lambda issue: issue["status"] == "completed")
                    assert cli("issue", "read", child)["issue_state"] == "closed"
                    assert not (root / "inspection.jsonl").exists()
                    expire_child_window(root, "issue_closed")

                    notices = cli("notification", "list", "main")["activities"]
                    child_activity = [item for item in notices if item["issue_id"] == child]
                    assert {item["short_number"] for item in child_activity} == {child_number}
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
                                       env={**env, "TEND_CHILD_ISSUE_NUMBER": str(child_number),
                                            "TEND_PARENT_ISSUE_ID": parent,
                                            "TEND_PARENT_ISSUE_NUMBER": str(parent_number)})
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
                    assert relay["issue_id"] == parent and relay["short_number"] == parent_number
                    cli("chat", "send", "other", f"Verify cross-chat Issue access #{parent_number}")
                    wait_for(lambda: cli("chat", "read", "other")["messages"],
                             lambda rows: rows[-1]["text"] == "The Issue is outside this Chat.")
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
                    assert list(default_home.glob("fixture-parent-*-thread.json"))
                    assert list(alternate_home.glob("fixture-child-*-thread.json"))
                    runner.terminate()
                    runner.wait(timeout=8)
                    settings["app_servers"]["alternate"]["codex_home"] = str(replacement_home)
                    config.write_text(json.dumps(settings))
                    runner = subprocess.Popen(runner_command, env=env, stderr=stderr)
                    check = [args.cli, "run", "rollout", first["run_id"]]
                    restored = wait_for(
                        lambda: subprocess.run(check, env=env, capture_output=True, timeout=8),
                        lambda result: result.returncode == 0,
                    )
                    assert json.loads(restored.stdout)["turn"]["items"][1]["text"] == DETAIL
                    assert not list(replacement_home.glob("fixture-*-thread.json"))

                except BaseException:
                    print((root / "runner.stderr").read_text(), end="", file=sys.stderr)
                    raise
                finally:
                    runner.terminate()
                    runner.wait(timeout=8)
            cli("run", "rollout", first["run_id"], error="HTTP 503")
            assert wait_message(parent, FIRST) == first
            delegation = json.loads((root / "delegation.json").read_text())
            assert delegation["action"]["issue_id"] == child
            [(parent_task,)] = sql("server", "SELECT task_id FROM task_runs WHERE id = ?", delegation["run_id"])
            mapping = sql("server", "SELECT number, issue_id FROM issue_short_numbers ORDER BY number")
            with lose_first_response(address, drop_when=lambda _: False) as (proxy, posted):
                subprocess.run([
                    args.runner, "--config", root / "runner.json", "--db", root / "runner.sqlite",
                    "--reconcile", parent_task,
                ], check=True, timeout=12, env={**env, "TEND_URL": f"http://{proxy}"})
            assert any(
                status == 200 and {"kind": "operation", "data": delegation} in request.get("records", [])
                and delegation["operation_id"] in reply.get("confirmed_operations", [])
                for request, status, reply in posted
            ), posted
            assert cli("issue", "read", f"#{child_number}")["issue_id"] == child
            assert sql("server", "SELECT number, issue_id FROM issue_short_numbers ORDER BY number") == mapping

            old_id = settings["runner_id"]
            successor_id = "next-runner"
            default_run = delegation["run_id"]
            named_run = first["run_id"]
            bindings = dict(sql("runner", "SELECT id, json_extract(app_server, '$.codex_home') FROM local_runs WHERE id IN (?, ?)",
                                default_run, named_run))
            assert bindings == {default_run: str(default_home), named_run: str(alternate_home)}
            workspace_root = (root / "runner.sqlite").with_suffix(".runs")
            # Old default-launch rows lacked a binding. Migrate with the original launcher and home.
            with sqlite3.connect(root / "runner.sqlite") as db:
                db.execute("UPDATE local_runs SET app_server=NULL WHERE id=?", (default_run,))
            subprocess.run([TESTS.parent / "scripts/tend-runner-retire-files", "adopt",
                            "--db", root / "runner.sqlite", "--config", config, "--runner", old_id,
                            "--host", settings["host_id"], "--workspace-root", workspace_root,
                            "--codex-home", default_home, "--app-server", sys.executable,
                            TESTS / "fixtures/coordination_app_server.py", root, "issue-chat"], check=True, env=env)
            adopted = subprocess.Popen([args.runner, "--config", config, "--db", root / "runner.sqlite",
                                        "--app-server", "/bin/false"], env={**env, "CODEX_HOME": str(replacement_home)})
            try:
                cmd = [args.cli, "run", "rollout", default_run]
                direct = wait_for(lambda: subprocess.run(cmd, env=env, capture_output=True, timeout=8),
                                  lambda result: result.returncode == 0)
                assert json.loads(direct.stdout)["turn"]["items"][1]["text"] == "协调指南的整理工作。"
            finally:
                adopted.terminate()
                adopted.wait(timeout=8)
            (root / "runner.sqlite").rename(root / "removed-original.sqlite")
            successor_inspect = free_port()
            settings.update(runner_id=successor_id, workspace_root=str(workspace_root), standby=True,
                            inspect={"listen": successor_inspect, "url": f"http://{successor_inspect}"})
            successor_config = root / "next.json"
            successor_config.write_text(json.dumps(settings))
            successor = subprocess.Popen([args.runner, "--config", successor_config, "--db", root / "next.sqlite",
                                          "--app-server", "/bin/false"], env={**env, "CODEX_HOME": str(replacement_home)})
            try:
                wait_for(lambda: cli("runner", "list"), lambda members: any(
                    m["profile"]["runner_id"] == successor_id for m in members))
                # The successor exists but cannot read on behalf of a predecessor until retirement.
                with sqlite3.connect(root / "server.sqlite") as db:
                    db.execute("INSERT INTO runner_replacements(predecessor,successor,drained_at) VALUES(?,?,unixepoch())",
                               (old_id, successor_id))
                cli("run", "rollout", named_run, error="HTTP 503")
                cli("runner", "retire", old_id)
                for run_id, text in [(default_run, "协调指南的整理工作。"), (named_run, DETAIL)]:
                    result = cli("run", "rollout", run_id)
                    assert result["runner_id"] == old_id and result["reader_id"] == successor_id
                    assert result["run_id"] == run_id and result["turn"]["items"][1]["text"] == text
                assert not list(replacement_home.glob("fixture-*-thread.json"))
            finally:
                successor.terminate()
                successor.wait(timeout=8)
    print("PASS Issue discussion with queued and late replies, Router relay, and opt-in original Runner history")


if __name__ == "__main__":
    main()
