"""Exercise board keyset paging through the real Server, CLI, and optionally Web."""

import argparse
from contextlib import closing
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import time

from chat import server
from issue_chat import WEB


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("server", "cli"):
        parser.add_argument(executable, type=lambda value: Path(value).resolve())
    parser.add_argument("--web", action="store_true")
    args = parser.parse_args()
    env = {**os.environ, "TEND_TOKEN": "board-order-fixture"}

    with tempfile.TemporaryDirectory(prefix="tend-board-order-") as directory:
        root = Path(directory)
        with server(args.server, root, env, web_dir=WEB / "dist" if args.web else None) as address:
            env["TEND_URL"] = f"http://{address}"

            def cli(*words, error=None):
                result = subprocess.run([args.cli, *words], env=env, text=True,
                                        capture_output=True, timeout=5)
                if error:
                    assert result.returncode and error in result.stderr, result.stderr
                    return None
                assert result.returncode == 0, result.stderr
                return json.loads(result.stdout)

            with closing(sqlite3.connect(root / "server.sqlite")) as db, db:
                db.execute(
                    "INSERT INTO chat_messages (id, chat_id, text) VALUES "
                    "('board-fixture', 'main', 'Synthetic board data')"
                )
                # Seed comparison baselines like a migrated database. The known
                # entry times below will come exclusively from real CLI writes.
                db.executemany(
                    "INSERT INTO issues (id, source_seq, title, objective, state, board_stage) "
                    "VALUES (?, 1, ?, '', ?, ?)",
                    [
                        ("order-z", "离开后重新进入", "open", "backlog"),
                        ("order-a", "最近取消", "open", "backlog"),
                        ("legacy-a", "历史卡片 A（入列时间未知）", "cancelled", "cancelled"),
                        ("legacy-z", "历史卡片 Z（入列时间未知）", "cancelled", "cancelled"),
                    ] + [
                        (f"z-fixture-{n:02}", f"待安排示例 {n:02}", "open", "backlog")
                        for n in range(1, 49)
                    ],
                )

            cli("issue", "cancel", "order-z", "Fixture", "first-z")
            time.sleep(0.01)
            cli("issue", "cancel", "order-a", "Fixture", "first-a")
            assert [row["id"] for row in cli("issue", "list")["issues"][:2]] == ["order-a", "order-z"]
            cli("issue", "reopen", "order-z", "Fixture", "reopen-z")
            time.sleep(0.01)
            cli("issue", "cancel", "order-z", "Fixture", "second-z")

            for scope in ("--scope=roots", "--scope=all", "main"):
                first = cli("issue", "list", scope)
                assert len(first["issues"]) == 50
                assert [row["id"] for row in first["issues"][:2]] == ["order-z", "order-a"]
                cursor = first["next_before"]
                assert cursor == "v1.0.z-fixture-01", cursor
                later = cli("issue", "list", scope, cursor)
                assert [row["id"] for row in later["issues"]] == ["legacy-z", "legacy-a"]
                assert later["next_before"] is None
            cli("issue", "list", "--scope=roots", "50", error="restart at the first page")

            if args.web:
                subprocess.run(["node", WEB / "tests/board-order.mjs"], check=True, timeout=45, env=env)
    print("PASS real stage reentry, all board scopes, keyset paging, and legacy cursor rejection")


if __name__ == "__main__":
    main()
