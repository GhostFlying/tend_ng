"""Test the actual Discussion timeline with a real Server and gated App Server output."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from urllib.request import Request, urlopen

from chat import server
from discussions import sql
from issue_chat import TESTS, WEB


GATES = ("progress-second", "progress-final", "progress-recover", "progress-empty")


def read(base, token, path):
    with urlopen(Request(f"{base}/api/v1/{path}", headers={"Authorization": f"Bearer {token}"}), timeout=5) as r:
        return json.load(r)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server", type=lambda path: Path(path).resolve())
    parser.add_argument("--screenshots", type=lambda path: Path(path).resolve())
    args = parser.parse_args()
    if args.screenshots:
        args.screenshots.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(prefix="tend-progress-") as directory:
        root = Path(directory)
        home = root / "app-home"
        home.mkdir()
        (root / "native-cwd").mkdir()
        (home / "synthetic-key").write_text("configured-discussion-key\n")
        (home / "progress-scope-probe").touch()
        app = root / "app-discussion"
        app.write_text(f"#!{sys.executable}\n" + (TESTS / "fixtures/discussion_app_server.py").read_text())
        app.chmod(0o755)
        config = root / "discussion.json"
        config.write_text(json.dumps({
            "host_id": "fixture-host", "program": str(app), "codex_home": "app-home", "cwd": "native-cwd",
            "model_api_keys": {"FIXTURE_API_KEY": "app-home/synthetic-key"},
        }))
        token = "tend-discussion-progress-fixture"
        env = {**os.environ, "CODEX_HOME": str(root / "fixture-codex"), "TEND_TOKEN": token}
        result_file = root / "browser-result.json"
        options = dict(discussion_config=config, web_dir=WEB / "dist")
        with server(args.server, root, env, **options) as address:
            try:
                subprocess.run(
                    ["node", WEB / "tests/discussion-progress.mjs"], cwd=WEB, check=True, timeout=120,
                    env={**env, "TEND_URL": f"http://{address}", "TEND_TEST_APP_HOME": str(home),
                         "TEND_SCREENSHOTS": str(args.screenshots or ""), "TEND_RESULT_FILE": str(result_file)},
                )
            finally:
                for gate in GATES:
                    (home / gate).touch()
            chat = json.loads(result_file.read_text())["chat"]
            before = read(f"http://{address}", token, f"chats/{chat}/messages")
            assert len(before["messages"]) == 10, before
            # Draft generation, handoff generation and its Main Chat receiver
            # deliberately emit commentary, but none should enter this table.
            stored = sql(root / "server.sqlite", "SELECT item_id FROM discussion_progress ORDER BY ordinal")
            assert stored == [(i,) for i in (
                "compare-scope", "compare-shape", "seen", "native-proof", "same-item", "same-item", "literal",
            )], stored
        with server(args.server, root, env, **options) as address:
            after = read(f"http://{address}", token, f"chats/{chat}/messages")
            assert after == before, (before, after)
            oldest = read(f"http://{address}", token, f"chats/{chat}/messages?before=3")
            assert oldest["messages"] == before["messages"][:2], oldest
        print("PASS Server restart and historical page preserve exact progress identities and visible replies")


if __name__ == "__main__":
    main()
