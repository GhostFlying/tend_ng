"""Exercise the read-only resource views against real Server and Runner state."""

import argparse
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
from urllib.error import HTTPError
from urllib.request import urlopen

from chat import server
from issue_chat import free_port, prepare, WEB


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("server", "cli", "runner"):
        parser.add_argument(executable, type=lambda value: Path(value).resolve())
    parser.add_argument("--web", action="store_true")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-resources-") as temp:
        root = Path(temp)
        prepare(root, free_port())
        address = free_port()
        env = {**os.environ, "TEND_TOKEN": "resources-fixture", "TEND_URL": f"http://{address}"}

        def cli(*words):
            return json.loads(subprocess.check_output([args.cli, *words], env=env, timeout=6))

        def register(runner, host, capacity):
            profile = json.loads((root / "runner.json").read_text())
            profile.pop("inspect")
            profile.update(runner_id=runner, host_id=host, slot_limit=capacity)
            path = root / f"{runner}.json"
            path.write_text(json.dumps(profile))
            subprocess.run(
                [args.runner, "--config", path, "--db", root / f"{runner}.sqlite", "--once"],
                env=env, check=True, timeout=6,
            )

        with server(args.server, root, env, listen=address, web_dir=WEB / "dist" if args.web else None):
            try:
                urlopen(f"http://{address}/api/v1/agents", timeout=3)
            except HTTPError as error:
                assert error.code == 401
            else:
                raise AssertionError("resource endpoint accepted an unauthenticated read")

            register("current-runner", "demo-host", 6)
            register("previous-runner", "demo-host", 2)
            register("retired-runner", "demo-host", 4)
            register("offline-runner", "offline-host", 3)
            with sqlite3.connect(root / "server.sqlite") as ledger:
                ledger.execute(
                    "UPDATE runners SET last_seen = unixepoch() - 60 WHERE id != 'current-runner'"
                )
                ledger.execute(
                    "INSERT INTO runner_replacements(predecessor, successor, drained_at, retired_at) "
                    "VALUES ('retired-runner', 'previous-runner', unixepoch(), unixepoch()),"
                    "       ('previous-runner', 'current-runner', NULL, NULL)"
                )
            roster = cli("runner", "list")
            instances = {r["profile"]["runner_id"]: r for r in roster}
            assert {rid for rid, r in instances.items() if r["online"]} == {"current-runner"}
            directory = cli("agent", "list")
            assert directory["router"] == {"id": "router", "model": None, "reasoning_effort": None}
            assert directory["runners"] == roster and directory["activity"] == []
            assert instances["retired-runner"]["retired_at"] is not None
            assert instances["previous-runner"]["successor"] == "current-runner"
            if args.web:
                subprocess.run(["node", WEB / "tests/resources.mjs"], env=env, check=True, timeout=28)

    print("PASS authenticated Agent directory and Runner instance observations")


if __name__ == "__main__":
    main()
