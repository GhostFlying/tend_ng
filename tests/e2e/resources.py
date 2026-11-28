"""Exercise the read-only resource views against real Server and Runner state."""

import argparse
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
from urllib.error import HTTPError
from urllib.request import ProxyHandler, build_opener

from chat import server
from issue_chat import free_port, prepare, WEB


def seed_activity(ledger):
    """Fixed projection fixture. Native start/stop is exercised by runner_upgrade.py."""
    ledger.execute("INSERT INTO chat_messages(id,chat_id,text) VALUES ('fixture-message','main','fixture')")
    ledger.execute("DELETE FROM sqlite_sequence WHERE name='issue_short_numbers'")
    ledger.execute("INSERT INTO sqlite_sequence(name,seq) VALUES ('issue_short_numbers',85)")
    rows = [
        (86, None, "展示派发来源与接手记录", None),
        (87, "issue-86", "实现派发与接手条目", ("current-runner", "Cedar", "implementation", "running", "started", 258)),
        (88, "issue-86", "汇总时间线展示与验收", ("current-runner", "Laurel", "issue-router", "running", "started", 423)),
        (89, None, "修订派发与 Attention 指引", ("previous-runner", "Maple", "review", "running", "started", None)),
        (90, "issue-89", "不展示已暂停 Task", ("previous-runner", "Maple", "review", "waiting", "stopped", None)),
        (91, None, "不展示已退役 Runner 的残留观测", ("retired-runner", "Maple", "review", "running", "started", 60)),
        (92, "issue-86", "不展示排队的 Task", ("current-runner", "Cedar", "implementation", "accepted", None, None)),
    ]
    observed = ledger.execute("SELECT unixepoch()").fetchone()[0]
    for number, parent, title, run in rows:
        issue, task, assignment = (f"{prefix}-{number}" for prefix in ("issue", "task", "assignment"))
        ledger.execute(
            "INSERT INTO issues(id,source_seq,title,objective,parent_issue_id) "
            "SELECT ?,seq,?,?,? FROM chat_messages WHERE id='fixture-message'",
            (issue, title, title, parent),
        )
        assert ledger.execute("SELECT number FROM issue_short_numbers WHERE issue_id=?", (issue,)).fetchone() == (number,)
        if run is None:
            continue
        runner, agent, category, status, phase, elapsed = run
        ledger.execute(
            "INSERT INTO tasks(id,issue_id,category,status,repo_ids,required_capabilities) VALUES(?,?,?,?,'[]','[]')",
            (task, issue, category, status),
        )
        ledger.execute(
            "INSERT INTO runner_assignments(id,task_id,runner_id,contract,state,expires_at) VALUES(?,?,?,?,'accepted',0)",
            (assignment, task, runner, json.dumps({"placement": {"agent": {"id": agent, "revision": "one"}}})),
        )
        ledger.execute("UPDATE tasks SET current_assignment=? WHERE id=?", (assignment, task))
        if phase:
            facts = {"phase": {"state": phase}}
            if elapsed is not None:
                facts["started_at"] = observed - elapsed
            ledger.execute("INSERT INTO task_runs(id,assignment_id,task_id,observation) VALUES(?,?,?,?)",
                           (f"run-{number}", assignment, task, json.dumps(facts)))
    ledger.executemany("UPDATE runners SET profile=json_set(profile, '$.running_slots', ?) WHERE id=?",
                       [(2, "current-runner"), (1, "previous-runner"), (1, "retired-runner")])


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
        env = {**os.environ, "CODEX_HOME": str(root / "fixture-codex"), "TEND_TOKEN": "resources-fixture", "TEND_URL": f"http://{address}"}

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
            http = build_opener(ProxyHandler({}))
            for path in ("agents", "execution-hosts"):
                try:
                    http.open(f"http://{address}/api/v1/{path}", timeout=3)
                except HTTPError as error:
                    assert error.code == 401
                else:
                    raise AssertionError("resource endpoint accepted an unauthenticated read")

            register("current-runner", "devbox", 6)
            register("previous-runner", "devbox", 6)
            register("retired-runner", "devbox", 6)
            register("idle-runner", "smallbox", 12)
            with sqlite3.connect(root / "server.sqlite") as ledger:
                ledger.execute(
                    "UPDATE runners SET last_seen = unixepoch() - 60 WHERE id = 'retired-runner'"
                )
                ledger.execute(
                    "INSERT INTO runner_replacements(predecessor, successor, drained_at, retired_at) "
                    "VALUES ('retired-runner', 'previous-runner', unixepoch(), unixepoch()),"
                    "       ('previous-runner', 'current-runner', NULL, NULL)"
                )
            roster = cli("runner", "list")
            instances = {r["profile"]["runner_id"]: r for r in roster}
            assert {rid for rid, r in instances.items() if r["online"]} == {"current-runner", "previous-runner", "idle-runner"}
            directory = cli("agent", "list")
            assert directory["router"] == {"id": "router", "model": None, "reasoning_effort": None}
            assert directory["runners"] == roster and directory["activity"] == []
            assert instances["retired-runner"]["retired_at"] is not None
            assert instances["previous-runner"]["successor"] == "current-runner"
            with sqlite3.connect(root / "server.sqlite") as ledger:
                seed_activity(ledger)
            snapshot = cli("runner", "activity")
            devbox, smallbox = snapshot["hosts"]
            assert (devbox["host_id"], devbox["running_slots"], devbox["slot_limit"]) == ("devbox", 3, 6)
            assert (smallbox["host_id"], smallbox["running_slots"], smallbox["slot_limit"]) == ("smallbox", 0, 12)
            current, previous = devbox["runners"]
            assert (current["mode"], previous["mode"]) == ("serving", "draining")
            assert [r["issue"]["short_number"] for r in current["runs"]] == [87, 88]
            assert [r["root_issue"]["short_number"] for r in current["runs"]] == [86, 86]
            assert [r["agent_id"] for r in current["runs"]] == ["Cedar", "Laurel"]
            assert previous["runs"][0]["started_at"] is None
            assert 258 <= snapshot["observed_at"] - current["runs"][0]["started_at"] < 270
            if args.web:
                subprocess.run(["node", WEB / "tests/resources.mjs"], check=True, timeout=48,
                               env={**env, "TEND_RESOURCES_DB": str(root / "server.sqlite")})
            with sqlite3.connect(root / "server.sqlite") as ledger:
                ledger.execute("UPDATE runners SET last_seen = unixepoch() - 60 WHERE id = 'current-runner'")
            offline = cli("runner", "activity")["hosts"][0]["runners"][0]
            assert not offline["online"] and offline["running_slots"] is None and not offline["runs"]

    print("PASS authenticated Agent directory and current execution-host activity projection")


if __name__ == "__main__":
    main()
