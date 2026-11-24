"""Upgrade real v22/v23 databases and check permanent Issue numbering through Server and CLI."""
import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from http.client import HTTPConnection
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
from threading import Barrier
from uuid import UUID

from chat import server, wait_for

ISSUE_PATH = "/api/v1/issues"
WRITERS = 12


def number_map(db):
    return db.execute(
        "SELECT number, issue_id FROM issue_short_numbers ORDER BY number"
    ).fetchall()


def legacy_inventory(root):
    ids = [str(UUID(int=n)) for n in [54, 21, 8, 2]]
    inventory = [
        (ids[0], "Open work", "open", None),
        (ids[1], "Closed work", "closed", None),
        (ids[2], "Cancelled work", "cancelled", None),
        (ids[3], "Closed child", "closed", ids[0]),
    ]
    with closing(sqlite3.connect(root / "server.sqlite")) as db, db:
        db.execute("PRAGMA foreign_keys = ON")
        db.execute(
            "INSERT INTO chat_messages (id, chat_id, text) VALUES ('legacy-source', 'main', 'Legacy')"
        )
        db.executemany(
            "INSERT INTO issues (id, title, state, parent_issue_id, source_seq, objective) "
            "VALUES (?, ?, ?, ?, 1, 'Preserve the original UUID')",
            inventory,
        )
    return inventory


def readback(db, expected):
    assert db.execute("PRAGMA user_version").fetchone() == (24,)
    assert db.execute("PRAGMA foreign_key_check").fetchall() == []
    assert number_map(db) == expected


def immutable(db, number, issue):
    for statement, params in [
        ("UPDATE issue_short_numbers SET number = number + 100 WHERE number = ?", [number]),
        ("DELETE FROM issue_short_numbers WHERE number = ?", [number]),
        ("INSERT OR REPLACE INTO issue_short_numbers (number, issue_id) VALUES (?, ?)",
         [number + 100, issue]),
    ]:
        try:
            with db:
                db.execute(statement, params)
        except sqlite3.IntegrityError as error:
            assert "issue short number is immutable" in str(error), error
        else:
            raise AssertionError(f"short number changed via {statement}")


def get(address, token, path, status=200):
    with closing(HTTPConnection(address, timeout=5)) as conn:
        conn.request("GET", path, headers={"Authorization": f"Bearer {token}"})
        response = conn.getresponse()
        data = json.loads(response.read())
        assert response.status == status, (path, response.status, data)
        return data


def parallel_insert(path, expected):
    barrier = Barrier(WRITERS)

    def insert(index):
        issue = str(UUID(int=100 + index))
        with closing(sqlite3.connect(path, timeout=10)) as db, db:
            db.execute("PRAGMA foreign_keys = ON")
            barrier.wait(timeout=5)
            db.execute(
                "INSERT INTO issues (id, source_seq, title, objective) "
                "VALUES (?, 1, 'Parallel insert', 'Exercise independent SQLite writers')",
                [issue],
            )
            number, = db.execute(
                "SELECT number FROM issue_short_numbers WHERE issue_id = ?", [issue]
            ).fetchone()
            return (number, issue)

    with ThreadPoolExecutor(max_workers=WRITERS) as executor:
        added = sorted(executor.map(insert, range(WRITERS)))
    maximum = expected[-1][0]
    assert [number for number, _ in added] == list(range(maximum + 1, maximum + WRITERS + 1))
    with closing(sqlite3.connect(path)) as db:
        assert db.execute(
            "SELECT n.number, i.id FROM issues i JOIN issue_short_numbers n ON n.issue_id = i.id "
            "ORDER BY i.rowid"
        ).fetchall() == expected + added
    return expected + added


def check_upgrade(root, legacy_server, version, args, env):
    root.mkdir()
    with server(legacy_server, root, env):
        pass
    inventory = legacy_inventory(root)
    expected = [(number, row[0]) for number, row in enumerate(inventory, 1)]
    path = root / "server.sqlite"
    copies = [root / "copy-a", root / "copy-b"]
    for copy in copies:
        copy.mkdir()
        with closing(sqlite3.connect(path)) as source, closing(sqlite3.connect(copy / path.name)) as dest:
            assert source.execute("PRAGMA user_version").fetchone() == (version,)
            source.backup(dest)

    for copy in copies:
        with server(args.server, copy, env) as address:
            with closing(sqlite3.connect(copy / path.name)) as db:
                readback(db, expected)
                assert db.execute(
                    "SELECT id, title, state, parent_issue_id FROM issues ORDER BY rowid"
                ).fetchall() == inventory
                immutable(db, *expected[0])
            for number, (issue, title, _, _) in enumerate(inventory, 1):
                overview = get(address, env["TEND_TOKEN"], f"{ISSUE_PATH}/{number}/overview")
                assert overview["id"] == issue
                assert overview["title"] == title and overview["short_number"] == number
                if number == 1:
                    assert overview["children"] == [
                        {"id": inventory[-1][0], "title": "Closed child", "short_number": 4}
                    ]

    # Exercise live Server writes after upgrading one copy, then restart it.
    copy = copies[0]
    with server(args.server, copy, env) as address:
        connected = {**env, "TEND_URL": f"http://{address}"}

        def cli(*words, binary=args.cli):
            result = subprocess.run([binary, *words], env=connected, text=True,
                                    capture_output=True, timeout=8)
            assert result.returncode == 0, result.stderr
            return json.loads(result.stdout)

        receipt = cli("chat", "send", "main", "Dispatch from Chat.")
        messages = wait_for(lambda: cli("chat", "read", "main")["messages"],
                            lambda rows: len(rows) == 3)
        assert messages[1]["id"] == receipt["message_id"]
        [issue] = messages[1]["issue_ids"]
        [number] = messages[1]["issue_numbers"]
        assert number == expected[-1][0] + 1
        expected.append((number, issue))
        detail = cli("issue", "read", issue)
        page = cli("issue", "messages", issue)
        assert detail["issue_id"] == page["issue_id"] == issue
        assert detail["short_number"] == page["short_number"] == number
        assert page["messages"][0]["issue_id"] == issue
        assert page["messages"][0]["short_number"] == number
        for reference in (str(number), f"#{number}"):
            assert cli("issue", "read", reference) == detail
            assert cli("issue", "messages", reference) == page
        for suffix in ("", "/messages", "/messages?before=1", "/overview", "/reviews",
                       "/pulls", "/watches", "/pull-discoveries?operation=missing"):
            legacy = get(address, env["TEND_TOKEN"], f"{ISSUE_PATH}/{issue}{suffix}")
            for reference in (str(number), f"%23{number}"):
                assert get(address, env["TEND_TOKEN"], f"{ISSUE_PATH}/{reference}{suffix}") == legacy
        for reference in ("99999", "0", "-1", "05", "%230", "%2305", "9223372036854775808"):
            get(address, env["TEND_TOKEN"], f"{ISSUE_PATH}/{reference}", status=404)
        if args.v23_cli:
            assert cli("issue", "read", issue, binary=args.v23_cli)["issue_id"] == issue
            assert cli("issue", "messages", issue, binary=args.v23_cli)["messages"][0]["issue_id"] == issue

        cli("issue", "cancel", str(number), "Fixture cancellation", "cancel-short")
        assert cli("issue", "read", issue)["issue_state"] == "cancelled"
        cli("issue", "reopen", f"#{number}", "Fixture reopening", "reopen-short")
        restored = cli("issue", "read", issue)
        assert restored["issue_state"] == "open" and restored["short_number"] == number
        expected = parallel_insert(copy / path.name, expected)
    with server(args.server, copy, env) as address:
        with closing(sqlite3.connect(copy / path.name)) as db:
            readback(db, expected)
        assert get(address, env["TEND_TOKEN"], f"{ISSUE_PATH}/{number}") == restored
    print(f"PASS v{version} migration on two copies, restart, immutable mappings, "
          f"{WRITERS} concurrent SQLite writers, and equivalent API/CLI paths")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("server", "cli"):
        parser.add_argument(executable, type=lambda value: Path(value).resolve())
    for executable in ("v22-server", "v23-server", "v23-cli"):
        parser.add_argument(f"--{executable}", type=lambda value: Path(value).resolve(),
                            required=executable.endswith("-server"))
    args = parser.parse_args()
    env = {**os.environ, "TEND_TOKEN": "issue-short-number-fixture"}
    with tempfile.TemporaryDirectory(prefix="tend-issue-short-numbers-") as directory:
        root = Path(directory)
        for version, legacy_server in [(22, args.v22_server), (23, args.v23_server)]:
            check_upgrade(root / f"v{version}", legacy_server, version, args, env)


if __name__ == "__main__":
    main()
