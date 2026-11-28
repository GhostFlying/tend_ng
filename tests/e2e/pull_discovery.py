"""Trace explicit and terminal PR lookup back to actual Runner worktree facts."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from chat import server, wait_for
from issue_chat import free_port, prepare, TESTS

PULL = "https://github.com/example/guide/pull/73"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("server", "cli", "runner"):
        parser.add_argument(executable, type=lambda value: Path(value).resolve())
    parser.add_argument("--already-linked", action="store_true")
    args = parser.parse_args()

    with tempfile.TemporaryDirectory(prefix="tend-github-discovery-") as directory:
        root = Path(directory)
        address = free_port()
        prepare(root, free_port(), "A fixed repository.\n")
        runner_config = json.loads((root / "runner.json").read_text())
        runner_config["agents"][0]["reasoning_effort"] = "low"
        (root / "runner.json").write_text(json.dumps(runner_config))
        # Use a local fetch transport while preserving the configured actual GitHub remote.
        repo = root / "example"
        origin = "https://github.com/example/guide.git"
        subprocess.run(["git", "-C", repo, "remote", "set-url", "origin", origin], check=True)
        subprocess.run(["git", "-C", repo, "config", f"url.{root / 'source'}.insteadOf", origin], check=True)
        (root / "delegate-now").touch()
        mode = root / "branch-mode"
        mode.write_text("unique" if args.already_linked else "ambiguous")
        env = {**os.environ, "CODEX_HOME": str(root / "fixture-codex"), "TEND_TOKEN": "discovery-fixture", "TEND_URL": f"http://{address}"}
        server_env = {**env, "GH_TOKEN": "fixture-read-token", "TEND_GITHUB_FIXTURE_DIR": str(root)}

        def cli(*words):
            out = subprocess.check_output([args.cli, *words], env=env, timeout=5)
            return json.loads(out)

        def job(issue, operation):
            found = cli("pr", "discoveries", issue, operation)
            return found[0] if found else None

        def calls():
            log = root / "branch-reads"
            return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []

        with server(args.server, root, server_env, listen=address, github_cli=TESTS / "fixtures/github_cli.py"):
            runner = subprocess.Popen([
                args.runner, "--config", root / "runner.json", "--db", root / "runner.sqlite",
                "--app-server", sys.executable, TESTS / "fixtures/coordination_app_server.py",
                root, "pull-discovery",
            ], env=env)
            try:
                cli("chat", "send", "main", "Dispatch from Chat.")
                wait_for(lambda: (root / "find-pull.json").exists(), bool)
                m = cli("chat", "read", "main")["messages"][0]
                [parent] = m["issue_ids"]
                delegated = json.loads((root / "delegation.json").read_text())["action"]
                task, child = delegated["task_id"], delegated["issue_id"]
                work = wait_for(lambda: cli("issue", "read", child),
                                lambda issue: issue["status"] == "running")
                branch = f"tend/{parent}/example"
                assert work["run"]["branches"] == [{
                    "repo_id": "example", "provider": "github", "repository": "example/guide",
                    "head_repository": "example/guide", "branch": branch,
                }], work["run"]
                accepted = json.loads((root / "find-pull.json").read_text())
                result = wait_for(lambda: job(child, accepted["operation_id"]),
                                  lambda value: value and value["state"] == "completed")
                [lookup] = result["results"]
                assert lookup["state"] == ("linked" if args.already_linked else "ambiguous")
                assert not any(item["automatic"] for item in cli("pr", "discoveries", child))

                if args.already_linked:
                    wait_for(lambda: cli("pr", "list", child),
                             lambda links: links and links[0]["facts"])
                else:
                    assert cli("pr", "list", child) == []
                    mode.write_text("denied")
                    receipt = cli("pr", "find", parent, task, "manual-find")
                    assert cli("pr", "find", parent, task, "manual-find") == receipt
                    requested = wait_for(lambda: job(parent, receipt["operation_id"]),
                                         lambda value: value and value["state"] == "completed")
                    assert requested["results"][0]["state"] == "not_accessible"
                    cli("pr", "find", child, m["id"], "unrelated-source")
                    assert job(child, "unrelated-source")["state"] == "unrelated_task"
                    mode.write_text("unique")

                (root / "finish-work").touch()
                wait_for(lambda: cli("issue", "read", child), lambda issue: issue["status"] == "completed")
                if args.already_linked:
                    assert not any(item["automatic"] for item in cli("pr", "discoveries", child))
                    assert calls() == [["unique", branch]]
                else:
                    automatic = wait_for(lambda: [item for item in cli("pr", "discoveries", child)
                                                  if item["automatic"] and item["state"] == "completed"], bool)
                    [one] = automatic
                    assert one["task_id"] == task and one["results"][0]["url"] == PULL
                    assert calls() == [["ambiguous", branch], ["denied", branch], ["unique", branch]]
                    assert len(cli("pr", "list", child)) == 1
            finally:
                (root / "finish-work").touch()
                runner.terminate()
                runner.wait(timeout=7)
    print("PASS explicit and terminal PR discovery from recorded worktree facts")


if __name__ == "__main__":
    main()
