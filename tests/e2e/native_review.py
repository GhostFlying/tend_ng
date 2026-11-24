"""Verify native child execution facts without equating parent tool completion with a child result."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from chat import server, wait_for
from issue_chat import TESTS, free_port, prepare


def main():
    server_bin, cli_bin, runner_bin = (Path(value).resolve() for value in sys.argv[1:])
    with tempfile.TemporaryDirectory(prefix="tend-native-review-") as directory:
        root = Path(directory)
        address = free_port()
        prepare(root, free_port(), "A fixed repository.\n")
        source = root / "source"
        subprocess.run(["git", "-C", source, "branch", "pinned-demo"], check=True)
        (source / "README.md").write_text("A later main commit.\n")
        subprocess.run(["git", "-C", source, "-c", "user.name=Fixture", "-c",
                        "user.email=fixture@example.invalid", "commit", "-qam", "later main"], check=True)
        pinned = subprocess.check_output(["git", "-C", source, "rev-parse", "pinned-demo"], text=True).strip()
        options = json.loads((root / "runner.json").read_text())
        options["agents"][0]["reasoning_effort"] = "low"
        options["agents"] += [
            {**options["agents"][0], "id": "selected-coordinator"},
            {**options["agents"][1], "id": "selected-worker", "description": "Fixture alternative"}
        ]
        (root / "runner.json").write_text(json.dumps(options))
        (root / "delegate-now").touch()
        env = {**os.environ, "TEND_TOKEN": "review-fixture", "TEND_URL": f"http://{address}"}

        def cli(*words):
            return json.loads(subprocess.check_output([cli_bin, *words], env=env, timeout=5))

        with server(server_bin, root, env, listen=address):
            runner = subprocess.Popen([
                runner_bin, "--config", root / "runner.json", "--db", root / "runner.sqlite",
                "--app-server", sys.executable, TESTS / "fixtures/coordination_app_server.py",
                root, "native-review",
            ], env=env)
            try:
                candidates = wait_for(
                    lambda: cli("agent", "candidates", "issue-router", json.dumps({
                        "repo_ids": ["example"], "required_capabilities": ["image_gen"]
                    }))["candidates"],
                    lambda rows: len(rows) == 2 and all(row["available"] for row in rows),
                )
                assert [row["agent"]["id"] for row in candidates] == ["coordinator", "selected-coordinator"]
                assert cli("agent", "candidates", "review",
                           '{"required_capabilities":["missing"]}') == {"candidates": []}
                cli("chat", "send", "main", "Prepare a guide from the pinned demo branch.")
                wait_for(lambda: (root / "review.json").exists(), bool)
                child = json.loads((root / "delegation.json").read_text())["action"]["issue_id"]
                issue = cli("issue", "read", child)
                assert issue["base_refs"] == {"example": "pinned-demo"}
                assert issue["agent"] == {"id": "selected-worker"}
                assert issue["placement"]["agent"]["id"] == "selected-worker"
                selected = cli("issue", "read", issue["parent_issue_id"])
                assert selected["agent"] == {"id": "selected-coordinator", "runner_id": "demo-runner"}
                assert selected["placement"]["agent"]["id"] == "selected-coordinator"
                workspace = root / "runner.runs" / "workspaces" / issue["parent_issue_id"]
                [mount] = json.loads((workspace / "mounts.json").read_text())
                assert mount["base"] == pinned
                rows = wait_for(lambda: cli("issue", "reviews", child), lambda rows: len(rows) == 4)
                assert len({row["run_id"] for row in rows}) == 1
                assert [row["action"]["observation"]["agentsStates"]["native-review-child"]["status"]
                        for row in rows[:-1]] == ["running", "running", "completed"]
                assert rows[-1] == json.loads((root / "review.json").read_text())
                assert rows[-1]["action"]["review"]["candidate"] == "guide-v1"
                parent = issue["parent_issue_id"]
                wait_for(lambda: cli("issue", "read", parent),
                         lambda current: current["status"] == "completed" and current["issue_state"] == "closed")
                query = cli("chat", "send", "main", "Check the finished fixture Issue.")["message_id"]
                wait_for(lambda: cli("chat", "read", "main")["messages"],
                         lambda messages: any(message.get("reply_to") == query and message["text"] ==
                                              "The fixture Issue is closed." for message in messages))
                assert cli("workspace", "reclaim", parent, "dirty-reclaim")["operation_id"] == "dirty-reclaim"
                dirty = wait_for(lambda: cli("workspace", "status", "dirty-reclaim"),
                                 lambda receipt: receipt["state"] != "pending")
                assert dirty["state"] == "failed" and "uncommitted changes" in dirty["result"]["error"]
                assert workspace.is_dir()
                (workspace / "repos/example/guide.txt").unlink()
                expected = cli("workspace", "reclaim", parent, "clean-reclaim")
                wait_for(lambda: cli("workspace", "status", "clean-reclaim"),
                         lambda receipt: receipt["state"] == "reclaimed")
                assert not workspace.exists()
                assert cli("workspace", "reclaim", parent, "clean-reclaim") == expected
                subprocess.run(["git", "-C", root / "example", "show-ref", "--verify", "--quiet",
                                f"refs/heads/tend/{parent}/example"], check=True)
            finally:
                runner.terminate()
                runner.wait(timeout=7)
    print("PASS named Agent selection, native review evidence, explicit workspace base, and clean reclaim")


if __name__ == "__main__":
    main()
