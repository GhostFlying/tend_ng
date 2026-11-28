"""Exercise hot category and Agent changes without replacing live execution identities."""

import argparse
from contextlib import closing
from http.client import HTTPConnection
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile

from chat import lose_first_response, server, wait_for

FIXTURE = Path(__file__).resolve().parents[1] / "fixtures/runtime_agent_categories_app_server.py"


def category(identifier, role="work", top=True, delegated=True, **extra):
    return {"id": identifier, "role": role, "display_name": identifier,
            "description": f"Fixture purpose for {identifier}",
            "top_level": top, "delegated": delegated, **extra}


def agent(identifier, category_id, revision="one", **extra):
    return {"id": identifier, "category": category_id, "revision": revision, **extra}


def exercise(args, root):
    env = {**os.environ, "TEND_TOKEN": "runtime-categories-fixture",
           "CODEX_HOME": str(root / "server-home"),
           "TEND_AGENT_CONFIG_RELOAD_SECONDS": "1"}
    server_pid_file = root / "server.pid"
    catalogue = root / "categories.json"
    hot_config = root / "hot.json"
    old_config = root / "old.json"
    early = category("early-work")
    hot = category("hot-work")
    review = category("hot-review", "review", top=False)
    coordinator = category("hot-coordinator", "coordinator", delegated=False)

    def write(path, value):
        temporary = path.with_suffix(".next")
        temporary.write_text(json.dumps(value))
        temporary.replace(path)

    def settings(name, *agents):
        return {"runner_id": name, "host_id": f"{name}-host", "slot_limit": 4,
                "workspace_root": str(root / f"{name}-workspace"),
                "repos": [], "agents": list(agents)}

    def read(name):
        file = root / f"{name}.json"
        return wait_for(lambda: json.loads(file.read_text()) if file.exists() else None, bool)

    def sql(name, statement, *values):
        file = root / f"{name}.sqlite"
        if not file.exists():
            return []
        with closing(sqlite3.connect(file)) as db:
            return db.execute(statement, values).fetchall()

    def drop_confirmation(reply):
        for assignment in reply.get("assignments", []):
            if assignment["work"]["title"] == "hot-top":
                cutoff["assignment"] = assignment["placement"]["assignment_id"]
        return bool(cutoff.get("assignment") in reply.get("confirmed_receipts", []))

    def hold_old_offer(request, reply):
        delayed = next((a for a in reply.get("assignments", [])
                        if a["work"]["title"] == "delayed-owner"), None)
        if delayed is None:
            return False
        current = next(a for a in request["profile"]["agents"] if a["id"] == "original")
        if current["model"] == "fixture-new":
            return False
        cutoff["delayed"] = delayed["placement"]["assignment_id"]
        return True

    def owner_agent(version):
        # Leave the operator-provided revision and launcher key unchanged.
        return agent("original", "early-work", model=f"fixture-{version}",
                     app_server="owner-launch", reasoning_effort="high" if version == "new" else "low")

    def owner_launcher(version):
        return {"owner-launch": {"program": sys.executable,
                                 "args": [str(FIXTURE), str(root), version],
                                 "codex_home": str(root / f"{version}-home")}}

    write(catalogue, [early])
    owner_settings = settings("hot", owner_agent("original"))
    owner_settings["app_servers"] = owner_launcher("original")
    write(hot_config, owner_settings)
    old_settings = settings("old", agent("old-decoy", "hot-work"),
                            agent("economy", "implementation-economy"))
    write(old_config, old_settings)
    cutoff = {}
    with (
        server(args.server, root, env, router_fixture=FIXTURE, category_config=catalogue,
               pid_file=server_pid_file) as address,
        lose_first_response(address, drop_when=drop_confirmation,
                            hold_response=hold_old_offer) as (proxy, posted),
    ):
        server_pid = int(server_pid_file.read_text())

        def cli(*words):
            return json.loads(subprocess.check_output([args.cli, *words], timeout=8,
                              env={**env, "TEND_URL": f"http://{address}"}))

        def send(name):
            cli("chat", "send", "main", f"category fixture {name}")
            return read(f"route-{name}")

        def directory():
            return {entry["id"]: entry for entry in cli("agent", "categories")["categories"]}

        def profile(runner_id):
            return next((row["profile"] for row in cli("runner", "list")
                         if row["profile"]["runner_id"] == runner_id), None)

        def runner_env(name, target):
            return {**env, "CODEX_HOME": str(root / f"{name}-home"), "TEND_URL": f"http://{target}"}

        def start(binary, name, path, target):
            with (root / f"{name}.log").open("w") as log:
                return subprocess.Popen([
                    binary, "--config", path, "--db", root / f"{name}.sqlite",
                    "--app-server", sys.executable, FIXTURE, root,
                ], env=runner_env(name, target), stderr=log)

        def emulated_old_heartbeat():
            with closing(HTTPConnection(address, timeout=5)) as connection:
                payload = {"protocol": 2, "receipts": [], "records": [],
                           "profile": {**old_settings, "repos": [], "direct_issue_owners": True,
                                       "available_for_assignment": 4, "running_slots": 0}}
                connection.request("POST", "/api/v1/runners/old/sync", json.dumps(payload), {
                    "Content-Type": "application/json",
                    "Authorization": "Bearer " + env["TEND_TOKEN"],
                })
                response = connection.getresponse()
                reply = json.load(response)
                assert response.status == 200 and "agent_categories" not in reply, reply
                assert not reply["assignments"], reply

        runner = start(args.runner, "hot", hot_config, proxy)
        old = start(args.legacy_runner, "old", old_config, address) if args.legacy_runner else None
        try:
            if old is None:
                # CI can exercise the old wire shape without building two versions.
                emulated_old_heartbeat()
            wait_for(lambda: profile("hot"), bool)
            old_profile = wait_for(lambda: profile("old"), bool)
            assert "dynamic_categories" not in old_profile
            assert any(entry["category"] == "hot-work" for entry in old_profile["agents"])
            owner_dispatch = send("owner")["dispatch"]
            owner = read("owner-before")
            warm = send("warm")
            delayed_id = wait_for(lambda: cutoff.get("delayed"), bool)
            assert sql("server", "SELECT state FROM runner_assignments WHERE id=?", delayed_id) == [("issued",)]
            assert sql("hot", "SELECT id FROM received_assignments WHERE id=?", delayed_id) == []
            assert owner_dispatch["task_id"] == owner["assignment"]["task_id"]
            assert "hot-work" not in {entry["id"] for entry in owner["before"]}

            # Invalid input cannot change an existing role or partially publish a catalogue.
            write(catalogue, [{**early, "role": "coordinator", "delegated": False}, hot])
            assert "hot-work" not in directory() and directory()["early-work"]["role"] == "work"
            write(catalogue, [{**early, "enabled": False}, hot, review, coordinator])
            wait_for(directory, lambda entries: entries.get("hot-work") and not entries["early-work"]["enabled"])
            (root / "router-hot.release").touch()
            pending = read("route-no-candidate")
            assert pending["dispatch"]["status"] == "awaiting_assignment"
            assert pending["dispatch"]["assignment_reason"]
            assert pending["candidates"]["candidates"] == []
            hot_settings = settings("hot", agent("hot-first", "hot-work", model="fixture-first"),
                                    agent("hot-reviewer", "hot-review"),
                                    agent("hot-coord", "hot-coordinator"), owner_agent("new"))
            hot_settings["app_servers"] = owner_launcher("new")
            write(hot_config, hot_settings)
            wait_for(lambda: profile("hot"), lambda row: row["agents"][0]["id"] == "hot-first")
            delayed = read("work-delayed-owner")
            assert delayed["assignment"]["placement"]["assignment_id"] == delayed_id
            assert delayed["assignment"]["placement"]["agent"] == owner["assignment"]["placement"]["agent"]
            assert delayed["launcher"] == owner["launcher"]
            (root / "delegate.release").touch()
            delegated = read("owner-delegated")
            assert (owner["pid"], owner["thread"], owner["turn"]) == (
                delegated["pid"], delegated["thread"], delegated["turn"])
            assert [item["id"] for item in delegated["candidates"]["candidates"]] == ["hot-first"]
            assert delegated["child"]["admission"]["agent"] == delegated["candidates"]["candidates"][0]
            child = read("work-hot-child")["assignment"]
            reviewer = read("work-child-review")["assignment"]
            for assignment, receipt, role in ((child, delegated["child"], "work"),
                                               (reviewer, delegated["review"], "review")):
                assert assignment["parent_issue_id"] == owner_dispatch["issue_id"]
                assert assignment["task_id"] == receipt["action"]["task_id"]
                assert assignment["placement"]["runner_id"] == "hot"
                assert assignment["placement"]["host_id"] == "hot-host"
                assert assignment["agent_role"] == role
            edited = {**hot, "display_name": "Hot edited", "description": "Updated purpose"}
            write(catalogue, [{**early, "enabled": False}, edited, review, coordinator])
            hot_settings["agents"][0] = agent("hot-second", "hot-work", "two", model="fixture-second")
            write(hot_config, hot_settings)
            wait_for(directory, lambda entries: entries["hot-work"]["display_name"] == "Hot edited")
            wait_for(lambda: profile("hot"), lambda row: row["agents"][0]["id"] == "hot-second")
            (root / "router-top.release").touch()
            assert next(item for item in read("route-edited")["directory"] if item["id"] == hot["id"]) == {
                **edited, "enabled": True}
            top = read("route-hot-top")
            assert [item["agent"]["id"] for item in top["candidates"]["candidates"]] == ["hot-second"]
            top_run = read("work-hot-top")["assignment"]
            assert top_run["placement"]["agent"]["revision"] == top["candidates"]["candidates"][0]["agent"]["revision"]
            assert top_run["placement"]["agent"]["model"] == "fixture-second"
            assignment_id = top_run["placement"]["assignment_id"]
            wait_for(lambda: sum(assignment_id in reply.get("confirmed_receipts", [])
                                 for _, _, reply in list(posted)), lambda count: count >= 2)
            assert sql("server", "SELECT id, state FROM runner_assignments WHERE task_id=?",
                       top_run["task_id"]) == [(assignment_id, "accepted")]
            wait_for(lambda: sql("hot", "SELECT id, acknowledged FROM received_assignments WHERE task_id=?",
                                 top_run["task_id"]), lambda rows: rows == [(assignment_id, 1)])

            # Removal is also a disable. Original claimed identities must keep working.
            write(catalogue, [review, coordinator, category("review", "review", top=False, enabled=False)])
            wait_for(directory, lambda entries: not entries["hot-work"]["enabled"])
            (root / "router-disabled.release").touch()
            disabled = read("route-disabled")
            assert "disabled" in disabled["error"] and "disabled" in disabled["start_error"]
            with closing(HTTPConnection(address, timeout=5)) as connection:
                connection.request("POST", f"/api/v1/issues/{disabled['backlog_id']}/start", headers={
                    "Authorization": "Bearer " + env["TEND_TOKEN"],
                })
                response = connection.getresponse()
                assert response.status == 409 and "disabled" in response.read().decode()
            assert sql("server", "SELECT count(*) FROM tasks WHERE issue_id=?", disabled["backlog_id"]) == [(0,)]
            wait_for(lambda: sql("hot", "SELECT catalogue FROM local_agent_categories"),
                     lambda rows: rows and any(not item["enabled"] and item["id"] == "hot-work"
                                                for item in json.loads(rows[0][0])))
            (root / "disable.release").touch()
            waiting = read("owner-waiting")
            assert "disabled" in waiting["disabled"]["error"]
            assert "disabled" in waiting["disabled_review"]["error"], waiting["disabled_review"]
            assert waiting["replayed"] == delegated["child"]
            assert waiting["pid"] == owner["pid"]
            issue, task = owner_dispatch["issue_id"], owner_dispatch["task_id"]
            wait_for(lambda: cli("issue", "read", issue), lambda row: row["status"] == "waiting")
            attention = waiting["attention"]["operation_id"]
            wait_for(lambda: sql("server", "SELECT question_seq FROM attentions WHERE id=?", attention),
                     lambda rows: rows and rows[0][0] is not None)
            cli("issue", "answer", issue, attention, "Continue the original Owner.")
            resumed = read("owner-resumed")
            assert resumed["thread"] == owner["thread"] and resumed["turn"] != owner["turn"]
            assert resumed["assignment"] == owner["assignment"] and resumed["launcher"] == owner["launcher"]
            assert sql("server", "SELECT id FROM tasks WHERE issue_id=?", issue) == [(task,)]
            assert sql("server", "SELECT count(*) FROM issues WHERE title='must-not-create'") == [(0,)]
            assert sql("server", "SELECT count(*) FROM issues WHERE title='hot-child'") == [(1,)]
            assert cli("issue", "read", pending["dispatch"]["issue_id"])["status"] == "awaiting_assignment"
            # Re-enable the same category. A newly created Task must launch with
            # the current revision, while the earlier offer and resume used the old one.
            write(catalogue, [early, review, coordinator])
            wait_for(directory, lambda entries: entries["early-work"]["enabled"])
            send("new-owner")
            fresh = read("work-new-owner")
            original = owner["assignment"]["placement"]["agent"]
            current = fresh["assignment"]["placement"]["agent"]
            assert current["id"] == original["id"] and current["revision"] != original["revision"]
            assert current["model"] == fresh["launcher"]["model"] == "fixture-new"
            assert current["reasoning_effort"] == fresh["launcher"]["effort"] == "high"
            scope = send("scope")
            assert "Server" in scope["local"] and "does not allow top-level" in scope["review"]
            send("coordinator")
            assert read("work-coordinator")["assignment"]["agent_role"] == "coordinator"
            if old is not None:
                economy = send("economy")
                assert economy["candidates"]["candidates"][0]["runner_id"] == "old"
                assert read("work-economy")["assignment"]["placement"]["runner_id"] == "old"
            else:
                assert cli("agent", "candidates", "implementation-economy")["candidates"][0]["runner_id"] == "old"

            for name in ("owner", "coordinator", "scope"):
                assert read(f"route-{name}")["thread"] == warm["thread"]
            for name in ("no-candidate", "edited", "hot-top", "disabled"):
                event = read(f"route-{name}")
                assert (event["pid"], event["thread"], event["turn"]) == (
                    warm["pid"], warm["thread"], warm["turn"]), event
            assert runner.poll() is None and (old is None or old.poll() is None)
            os.kill(server_pid, 0)
        except Exception:
            for log in root.glob("*.log"):
                print(log.name, log.read_text(), file=sys.stderr)
            raise
        finally:
            for process in [runner] + ([old] if old is not None else []):
                process.terminate()
                process.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "cli", "runner", "--legacy-runner"):
        parser.add_argument(name, type=lambda path: Path(path).resolve())
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-hot-categories-") as temporary:
        exercise(args, Path(temporary))
    print("PASS hot category/Agent updates, delayed claim and resumed Task keep original launcher, "
          "new Task uses updated launcher, legacy Runner compatibility and lost receipt replay")


if __name__ == "__main__":
    main()
