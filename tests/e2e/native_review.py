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
        options = json.loads((root / "runner.json").read_text())
        options["agents"][0]["reasoning_effort"] = "low"
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
                cli("chat", "send", "main", "Dispatch from Chat.")
                wait_for(lambda: (root / "review.json").exists(), bool)
                child = json.loads((root / "delegation.json").read_text())["action"]["issue_id"]
                rows = wait_for(lambda: cli("issue", "reviews", child), lambda rows: len(rows) == 4)
                assert len({row["run_id"] for row in rows}) == 1
                assert [row["action"]["observation"]["agentsStates"]["native-review-child"]["status"]
                        for row in rows[:-1]] == ["running", "running", "completed"]
                assert rows[-1] == json.loads((root / "review.json").read_text())
                assert rows[-1]["action"]["review"]["candidate"] == "guide-v1"
            finally:
                runner.terminate()
                runner.wait(timeout=7)
    print("PASS native subagent state, original child identity, recorded review, and observational replay")


if __name__ == "__main__":
    main()
