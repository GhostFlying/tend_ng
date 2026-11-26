"""Verify direct root delivery, exact-SHA CI waiting, review, and coordination via Chat."""

import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile

from chat import server, wait_for
from child_windows import expire_child_window
from issue_chat import TESTS, free_port, prepare

MERGE = "Merge the example change and stop at remote main."
CI = "Merge the example change and wait for green CI on remote main."
MULTI = "Coordinate two independent deliverables in example."
REVIEW = "Investigate example and get an independent review if necessary."
PULL = "https://github.com/example/guide/pull/82"


def exercise(server_bin, cli_bin, runner_bin, root):
    prepare(root, free_port())
    bare = root / "remote.git"
    subprocess.run(["git", "clone", "--bare", "--quiet", root / "source", bare], check=True)
    provider_url = "https://github.com/example/guide.git"
    for settings in (("remote", "set-url", "origin", provider_url),
                     ("config", f"url.{bare}.insteadOf", provider_url)):
        subprocess.run(["git", "-C", root / "example", *settings], check=True)
    (root / "branch-mode").write_text("denied")  # Exercise recording and lookup without linking another PR.
    (root / "wait-mode").write_text("matching")  # PR head is green even while the remote main SHA is pending.
    config = json.loads((root / "runner.json").read_text())
    config["workspace_root"] = str(root / "runner.runs")
    config["agents"].extend([
        {"id": "economy", "category": "implementation-economy", "revision": "two", "model": "fixture-economy"},
        {"id": "investigator", "category": "investigation", "revision": "three", "model": "fixture-investigator"},
        {"id": "reviewer", "category": "review", "revision": "four", "model": "fixture-reviewer"},
    ])
    config["agents"][1]["reasoning_effort"] = "medium"
    config.pop("inspect")
    address = free_port()
    codex = root / "codex"
    codex.mkdir()
    env = {**os.environ, "TEND_TOKEN": "direct-owner-fixture", "TEND_URL": f"http://{address}",
           "CODEX_HOME": str(codex)}
    server_env = {**env, "GH_TOKEN": "fixture-read-token", "TEND_GITHUB_FIXTURE_DIR": str(root)}
    processes = []

    def start(name, settings):
        file = root / f"{name}.json"
        file.write_text(json.dumps(settings))
        with (root / f"{name}.log").open("w") as log:
            runner = subprocess.Popen([
                runner_bin, "--config", file, "--db", root / f"{name}.sqlite",
                "--app-server", sys.executable, TESTS / "fixtures/direct_owner_app_server.py", root,
            ], env=env, stderr=log)
        processes.append((name, runner))
        return runner

    def cli(*words, raw=False):
        reply = subprocess.check_output([cli_bin, *words], env=env, timeout=5)
        return reply if raw else json.loads(reply)

    def sql(db_name, statement, *values):
        with sqlite3.connect(root / f"{db_name}.sqlite") as db:
            return db.execute(statement, values).fetchall()

    def send(text):
        original = cli("chat", "send", "main", text)["message_id"]
        msg = wait_for(lambda: next((m for m in cli("chat", "read", "main")["messages"]
                                     if m["id"] == original and m.get("issue_ids")), None), bool)
        [issue] = msg["issue_ids"]
        return original, issue

    def state(issue, expected):
        return wait_for(lambda: cli("issue", "read", issue), lambda view: view["status"] == expected)

    def report(original, snippet):
        return wait_for(lambda: next((m for m in cli("chat", "read", "main")["messages"]
                                     if m.get("reply_to") == original and snippet in m["text"]), None), bool)

    def remote_main():
        return subprocess.check_output(
            ["git", "--git-dir", bare, "rev-parse", "refs/heads/main"], text=True).strip()

    def no_children(issue, task):
        assert sql("server", "SELECT id FROM issues WHERE parent_issue_id = ?", issue) == []
        assert sql("server", "SELECT id FROM tasks WHERE issue_id = ?", issue) == [(task,)]

    with server(server_bin, root, server_env, listen=address, github_cli=TESTS / "fixtures/github_cli.py"):
        try:
            predecessor = start("runner", config)
            wait_for(lambda: cli("runner", "list"), bool)

            original, merge = send(MERGE)
            finished = state(merge, "completed")
            assert finished["category"] == "implementation" and finished["issue_state"] == "closed"
            assert finished["placement"]["agent"]["id"] == "worker"
            assert finished["placement"]["agent"]["reasoning_effort"] == "medium"
            assert finished["placement"]["host_id"] == config["host_id"]
            sha = remote_main()
            delivered = report(original, f"Remote main {sha}; merge endpoint reached.")
            [artifact] = delivered["artifacts"]
            assert cli("artifact", "download", artifact["id"], raw=True) == (sha + "\n").encode()
            no_children(merge, finished["task_id"])
            [branch] = finished["run"]["branches"]
            assert branch["branch"] == f"tend/{merge}/example" and branch["provider"] == "github"
            wait_for(lambda: (root / "branch-reads").exists(), bool)
            assert cli("pr", "watches", merge) == []
            assert not (root / "direct-ci-reads").exists()

            original, ci = send(CI)
            waiting = state(ci, "waiting")
            task = waiting["task_id"]
            sha = remote_main()
            assert waiting["placement"]["agent"]["id"] == "economy"
            assert waiting["run"]["phase"]["waiting_for_external"]
            assert waiting["issue_state"] == "open"
            no_children(ci, task)
            assert sql("runner", "SELECT execution_state, retains_workspace FROM received_assignments "
                       "WHERE task_id = ?", task) == [("waiting_external", 1)]
            wait_for(lambda: (root / "direct-ci-reads").exists(), bool)
            assert "PENDING" in (root / "direct-ci-reads").read_text()
            assert not any("CI success." in message["text"]
                           for message in cli("chat", "read", "main")["messages"])

            start("next", {**config, "runner_id": "next-runner", "standby": True})
            wait_for(lambda: cli("runner", "list"), lambda rows: len(rows) == 2)
            cli("runner", "replace", config["runner_id"], "next-runner")
            moved = wait_for(lambda: cli("issue", "read", ci), lambda view: view["status"] == "waiting"
                             and view.get("placement", {}).get("runner_id") == "next-runner")
            assert moved["task_id"] == task and moved["placement"]["host_id"] == config["host_id"]
            assert moved["placement"]["agent"] == waiting["placement"]["agent"]
            assert predecessor.wait(12) == 0
            assert cli("runner", "retire", config["runner_id"])["state"] == "retired"
            assert sql("next", "SELECT execution_state, retains_workspace FROM received_assignments "
                       "WHERE task_id = ?", task) == [("waiting_external", 1)]

            (root / "direct-ci-success").touch()
            sql("server", "UPDATE pulls SET due_at = 0 WHERE url = ?", PULL)
            wait_for(lambda: (root / "direct-ci-reads").read_text(), lambda body: "SUCCESS" in body)
            wait_for(lambda: sql("server", "SELECT due_at FROM external_batches WHERE task_id = ?", task), bool)
            sql("server", "UPDATE external_batches SET due_at = 0 WHERE task_id = ?", task)
            finished = state(ci, "completed")
            assert finished["task_id"] == task and finished["issue_state"] == "closed"
            assert finished["run"]["run_id"] != waiting["run"]["run_id"]
            [file] = report(original, f"Remote main {sha}; CI success.")["artifacts"]
            assert cli("artifact", "download", file["id"], raw=True) == (sha + "\n").encode()
            no_children(ci, task)

            start("other", {**config, "runner_id": "other-runner", "host_id": "other-host",
                            "workspace_root": str(root / "other.runs"),
                            "agents": [config["agents"][1]]})
            wait_for(lambda: cli("runner", "list"), lambda rows: len(rows) == 3)
            original, multi = send(MULTI)
            wait_for(lambda: sql("server", "SELECT state FROM issues WHERE parent_issue_id = ?", multi),
                     lambda rows: rows == [("closed",), ("closed",)])
            [multi_task] = sql("server", "SELECT id FROM tasks WHERE issue_id = ?", multi)
            expire_child_window(root, "issue_closed", task=multi_task[0])
            done = state(multi, "completed")
            assert done["category"] == "issue-router"
            assert sql("server", "SELECT DISTINCT a.runner_id FROM runner_assignments a JOIN tasks t "
                       "ON t.id=a.task_id JOIN issues i ON i.id=t.issue_id WHERE i.parent_issue_id = ?", multi) == [
                           ("next-runner",)]
            report(original, "Both independent deliveries are complete.")

            original, investigation = send(REVIEW)
            waiting = state(investigation, "waiting")
            question = report(original, "Run the independent review?")
            cli("chat", "send", "main", "Review the example candidate.", "--reply-to", question["id"])
            done = state(investigation, "completed")
            assert done["category"] == "investigation" and done["task_id"] == waiting["task_id"]
            assert sql("server", "SELECT id FROM tasks WHERE issue_id = ?", investigation) == [
                (done["task_id"],)]
            assert sql("server", "SELECT json_extract(payload,'$.kind') FROM issue_inputs "
                       "WHERE task_id=? ORDER BY rowid", done["task_id"]) == [
                           ("attention_answered",), ("review_finished",)]
            report(original, f"Independent review returned to {done['task_id']}.")
            [binding] = sql("server", "SELECT review_task_id, result_input_id FROM tend_reviews "
                            "WHERE author_task_id = ?", done["task_id"])
            [delivery] = sql("server", "SELECT json_extract(payload,'$.child_task_id'), id FROM issue_inputs "
                             "WHERE task_id = ? AND run_id = ? AND json_extract(payload,'$.kind') = 'review_finished'",
                             done["task_id"], done["run"]["run_id"])
            assert binding == delivery
        except BaseException:
            for name, _ in processes:
                print((root / f"{name}.log").read_text(), file=sys.stderr, end="")
            raise
        finally:
            for _, process in processes:
                if process.poll() is None:
                    process.terminate()
                    process.wait(7)


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="tend-direct-owner-") as directory:
        exercise(*(Path(arg).resolve() for arg in sys.argv[1:4]), Path(directory))
    print("PASS original root ownership, merge-only delivery, exact-SHA CI across handoff, coordination, and original-author review")
