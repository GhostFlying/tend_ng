"""Backfill an anonymous historical distribution using real Tend processes and isolated data."""
import argparse
from collections import Counter
from contextlib import closing
import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import tempfile
import time
import urllib.request

from chat import lose_first_response, server
from issue_chat import free_port
from issue_token_usage import runner_process


RUNS = 277
NATIVE = 7422
CONSISTENT = 7417
ORIGINAL, SUCCESSOR, OFFLINE = "fixture-old", "fixture-next", "fixture-offline"
ROOT, CHILD = "fixture-root", "fixture-child"
COUNTS = {"total_tokens": 10, "cached_input_tokens": 0}
BEARER = "backfill-isolated-fixture"


def run_ref(index):
    return (f"run-{index:03}", f"task-{index:03}", f"thread-{index:03}", f"turn-{index:03}")


def record(index, key, counts):
    _, _, thread, turn = run_ref(index)
    return {"type": "token_usage_record", "payload": {
        "thread_id": thread, "turn_id": turn, "response_id": key,
        "usage": counts, "thread_token_usage": {"total_tokens": 1200},
    }}


def native_path(home, thread, archived=False):
    sessions = home / ("archived_sessions" if archived else "sessions/2026/01/02")
    sessions.mkdir(parents=True, exist_ok=True)
    return sessions / f"rollout-2026-01-02T03-04-05-{thread}.jsonl"


def seed_fixture(root, workspace):
    """Load only synthetic identity/binding rows. Metering itself goes through HTTP sync."""
    with closing(sqlite3.connect(root / "server.sqlite", timeout=5)) as db, db:
        db.executescript(f"""
          INSERT INTO chat_messages (seq, id, chat_id, text) VALUES (1, 'fixture-source', 'fixture', 'Synthetic');
          INSERT INTO issues (id, source_seq, title, objective) VALUES ('{ROOT}', 1, 'Root', '');
          INSERT INTO issues (id, source_seq, title, objective, parent_issue_id) VALUES ('{CHILD}', 1, 'Child', '', '{ROOT}');
          UPDATE runners SET last_seen = 0 WHERE id = '{OFFLINE}';
        """)
        for index in range(RUNS):
            _, task, _, _ = run_ref(index)
            origin = SUCCESSOR if index in (0, 273) else OFFLINE if index == 275 else ORIGINAL
            db.execute("""INSERT INTO tasks (id, issue_id, status, category, repo_ids, required_capabilities)
                       VALUES (?, ?, 'accepted', 'implementation', '[]', '[]')""", (task, ROOT if index == 0 else CHILD))
            db.execute("""INSERT INTO runner_assignments (id, task_id, runner_id, contract, state, expires_at)
                       VALUES (?1, ?1, ?2, '{"placement":{"agent":{"id":"worker"}}}', 'accepted', 0)""", (task, origin))
            db.execute("UPDATE tasks SET current_assignment = id WHERE id = ?", (task,))
    old_home = root / "original-home"
    next_home = root / "successor-home"
    old_launch = json.dumps({"program": "unused-synthetic-launcher", "codex_home": str(old_home)})
    next_launch = json.dumps({"program": "unused-other-launcher", "codex_home": str(next_home)})
    history = workspace / ".tend-host/fixture-host/history"
    history.mkdir(parents=True, exist_ok=True)
    with closing(sqlite3.connect(history / f"{ORIGINAL}.sqlite")) as db, db:
        db.executescript("CREATE TABLE profiles(id INTEGER PRIMARY KEY, launch TEXT);"
                         "CREATE TABLE threads(id TEXT PRIMARY KEY, profile_id INTEGER);")
        db.execute("INSERT INTO profiles VALUES (1, ?)", (old_launch,))
        db.executemany("INSERT INTO threads VALUES (?, 1)",
                       [(run_ref(i)[2],) for i in range(1, RUNS) if i not in (273, 275)])
    with closing(sqlite3.connect(root / f"{SUCCESSOR}.sqlite")) as db, db:
        for index in (0, 273):
            run, task, thread, turn = run_ref(index)
            db.execute("""INSERT INTO received_assignments
                       (id, task_id, contract, repositories, decision, acknowledged, execution_state, app_server)
                       VALUES (?1, ?1, '{}', '[]', '{"status":"accepted"}', 1, 'finished', ?2)""", (task, next_launch))
            db.execute("INSERT INTO local_runs (id, assignment_id, state, thread_id, turn_id) VALUES (?, ?, ?, ?, ?)",
                       (run, task, "prepared" if index == 0 else "stopped", thread, turn))
    batches = {key: ([], []) for key in (ORIGINAL, SUCCESSOR, OFFLINE)}
    for index in range(RUNS):
        run, task, thread, turn = run_ref(index)
        origin = SUCCESSOR if index in (0, 273) else OFFLINE if index == 275 else ORIGINAL
        lifecycle, copy = batches[origin]
        start = {"event_id": f"start-{run}", "assignment_id": task, "task_id": task,
                 "run_id": run, "thread_id": thread, "turn_id": turn, "phase": {"state": "started"}}
        lifecycle.append(start)
        if index:
            lifecycle.append({**start, "event_id": f"stop-{run}",
                              "phase": {"state": "stopped", "outcome": "completed"}})
        def copied(key, counts):
            copy.append({**{key: start[key] for key in ("assignment_id", "task_id", "run_id", "thread_id", "turn_id")},
                         "event_id": f"{run}-{key}", "response_id": key, "last": counts,
                         "observed_at": 1})
        if index > 272:
            if index == 274:
                native_path(old_home, thread).write_text("{broken fixture\n")
            if index == 276:
                native_path(old_home, thread).write_text('{"type":"session_meta","payload":{}}\n')
            continue
        lines = [{"type": "session_meta", "payload": {"id": thread}}]
        if index == 2:
            lines.append({"type": "response_item", "payload": {"output": "X" * (1024 * 1024)}})
        for response in range(78 if index == 0 else 27):
            key = f"response-{response:03}"
            lines.append(record(index, key, COUNTS))
            if index == 0 and response == 4:
                lines.append(record(index, key, {**COUNTS, "total_tokens": 11}))
            if index == 0 and response in (0, 3):
                continue
            copied(key, {**COUNTS, "total_tokens": 9} if index == 0 and response in (1, 2) else COUNTS)
        if index == 0:
            copied("server-only", COUNTS)
        home = next_home if index == 0 else old_home
        native_path(home, thread, archived=index == 2).write_text("".join(json.dumps(line) + "\n" for line in lines))
    return batches


def page_query_plan(root, get):
    source = (Path(__file__).resolve().parents[2] / "crates/tend-server/src/store/token_usage.rs").read_text()
    query = re.search(r'const ISSUE_USAGE_QUERY: &str = "(.*?)";', source, re.DOTALL)[1]
    with closing(sqlite3.connect(root / "server.sqlite")) as db:
        plan = [row[3] for row in db.execute("EXPLAIN QUERY PLAN " + query, (ROOT,))]
    assert not any("SCAN " in step or any(table in step for table in (
        "token_requests", "token_runs", "token_usage_pending", "token_backfill",
    )) for step in plan), plan
    assert sum("PRIMARY KEY" in step for step in plan) == 3, plan
    samples = []
    for _ in range(100):
        started = time.perf_counter()
        get(f"/api/v1/issues/{ROOT}/token-usage")
        samples.append((time.perf_counter() - started) * 1000)
    samples.sort()
    print(json.dumps({"projection_plan": plan, "http_samples": len(samples),
                      "p50_ms": round(samples[50], 3), "p95_ms": round(samples[95], 3)}))


def verify(args, root, address):
    env = {**os.environ, "TEND_TOKEN": BEARER, "TEND_URL": f"http://{address}",
           "CODEX_HOME": str(root / "no-personal-home")}
    workspace = root / "shared"
    def get(path, data=None):
        request = urllib.request.Request(env["TEND_URL"] + path, data=None if data is None else json.dumps(data).encode(),
                                         headers={"Authorization": f"Bearer {BEARER}", "Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=5) as response:
            return json.load(response)

    def cli(*words, error=None):
        result = subprocess.run([args.cli, *words], env=env, text=True, capture_output=True, timeout=15)
        if error:
            assert result.returncode != 0 and error in result.stderr, result.stderr
            return
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout)

    def maint(command, *words, error=None):
        return cli("token-usage", command, *words, error=error)

    def status(job):
        return maint("status", job)

    def runs(job, first):
        result = first["runs"]
        while first["next_after"] is not None:
            first = maint("status", job, "--after", str(first["next_after"]))
            result += first["runs"]
        return {item["run_id"]: item for item in result}

    def usage():
        return cli("issue", "token-usage", ROOT)

    def command(runner, host):
        config = root / f"{runner}.json"
        config.write_text(json.dumps({"runner_id": runner, "host_id": host, "slot_limit": 0,
                                      "workspace_root": str(workspace if host == "fixture-host" else root / "offline"),
                                      "repos": [], "agents": []}))
        return [args.runner, "--db", root / f"{runner}.sqlite", "--config", config]

    def exchange(runner, profile, *, observations=(), uses=()):
        reply = get(f"/api/v1/runners/{runner}/sync", {"protocol": 2, "profile": profile, "receipts": [],
                                                       "runs": observations, "token_usage": uses})
        assert "token_backfill" not in reply
        assert len(reply.get("confirmed_runs", [])) == len(observations)
        assert len(reply.get("confirmed_token_usage", [])) == len(uses)

    with server(args.server, root, env, listen=address):
        original = command(ORIGINAL, "fixture-host")
        successor = command(SUCCESSOR, "fixture-host")
        offline = command(OFFLINE, "other-host")
        for command_line in (original, successor, offline):
            subprocess.run([*command_line, "--once"], env=env, check=True, timeout=10)
        cli("runner", "replace", ORIGINAL, SUCCESSOR)
        batches = seed_fixture(root, workspace)
        profiles = {row["profile"]["runner_id"]: row["profile"] for row in cli("runner", "list")}
        for runner, (lifecycle, copy) in batches.items():
            for i in range(0, len(lifecycle), 32):
                exchange(runner, profiles[runner], observations=lifecycle[i:i + 32])
            for i in range(0, len(copy), 32):
                exchange(runner, profiles[runner], uses=copy[i:i + 32])
        # The OFFLINE wire fixture above had to submit its historic Start. Age
        # its synthetic heartbeat so the status test doesn't require sleeps.
        with closing(sqlite3.connect(root / "server.sqlite")) as db, db:
            db.execute("UPDATE runners SET last_seen = 0 WHERE id = ?", (OFFLINE,))
        baseline = usage()
        assert baseline["subtree"]["started_runs"] == RUNS, baseline
        assert baseline["subtree"]["reported_requests"] == 7421, baseline

        queued = maint("backfill", "--all", "--job", "full")
        assert queued["counts"] == {"reader_offline": 1, "reader_unsupported": 276}, queued
        assert maint("backfill", "--all", "--job", "full")["selected_runs"] == RUNS
        maint("backfill", "--all", "--job", "full", "--policy", "prefer-native", error="409")
        # Old transport has no optional capability. Drain the old *process*
        # without ever using it as a backfill reader, then route to its successor.
        subprocess.run([*original, "--once"], env=env, check=True, timeout=10)
        subprocess.run([*original, "--once"], env=env, check=True, timeout=10)
        assert cli("runner", "retire", ORIGINAL)["state"] == "retired"

        interleaved = []
        def drop_after_first_page(reply):
            work = reply.get("token_backfill", {})
            if work.get("run_id") != "run-000" or work.get("offset") != 32:
                return False
            with closing(sqlite3.connect(root / f"{SUCCESSOR}.sqlite", timeout=5)) as db, db:
                db.execute("""INSERT INTO local_token_usage
                    (id, run_id, assignment_id, thread_id, turn_id, response_id, last)
                    VALUES ('late-live-receipt', ?, ?, ?, ?, 'response-003', ?)""",
                    (*run_ref(0), json.dumps(COUNTS)))
            interleaved.append(True)
            return True

        with lose_first_response(address, drop_after_first_page) as (gateway, posted):
            with runner_process(successor, {**env, "TEND_URL": f"http://{gateway}"}, root):
                def idle(job):
                    deadline = time.monotonic() + 30
                    while True:
                        state = status(job)
                        if not any(state["counts"].get(key, 0) for key in ("pending", "reader_unsupported")):
                            return state
                        assert time.monotonic() < deadline, state
                        time.sleep(0.05)
                finished = idle("full")
                assert finished["counts"] == {"active_prefix": 1, "gap": 3, "reader_offline": 1, "scanned": 272}, finished
                by_run = runs("full", finished)
                decisions = Counter()
                for run in by_run.values():
                    decisions.update(run["results_at_scan"])
                assert sum(r["native_keys"] for r in by_run.values()) == NATIVE
                assert decisions == {"compatible": CONSISTENT, "native_only": 2, "quarantined": 3}, decisions
                assert by_run["run-001"]["runner_id"] == ORIGINAL and by_run["run-001"]["reader_id"] == SUCCESSOR
                assert by_run["run-273"]["gap"] == "source_missing"
                assert by_run["run-274"]["gap"] == "source_corrupt"
                assert by_run["run-275"]["state"] == "reader_offline"
                assert by_run["run-276"]["gap"] == "no_native_records"
                assert by_run["run-000"]["server_only_at_scan"] == 1
                assert len(interleaved) == 1
                first_pages = [(request["token_backfill"]["page"], request.get("token_usage", []))
                               for request, _, _ in posted
                               if request.get("token_backfill", {}).get("page", {}).get("item") == by_run["run-000"]["cursor"]
                               and request["token_backfill"]["page"]["offset"] == 0]
                assert len(first_pages) >= 2 and first_pages[0][0] == first_pages[1][0]
                assert any(item["response_id"] == "response-003" for _, live in first_pages for item in live)
                assert usage()["subtree"]["reported_requests"] == 7420
                assert len(maint("audit", "full")["entries"]) == 4

                for job, policy, tokens in (("server", "prefer-server", 74228), ("native", "prefer-native", 74220)):
                    selection = ("--task", "task-000") if job == "server" else ("--run", "run-000")
                    maint("backfill", *selection, "--job", job, "--policy", policy)
                    idle(job)
                    tally = usage()["subtree"]
                    if policy == "prefer-server":
                        # All three disputes have an unambiguous Server copy,
                        # including the key whose native source had two variants.
                        assert tally["reported_requests"] == 7423, tally
                    else:
                        assert tally["reported_requests"] == 7422, tally
                    assert tally["total_tokens"]["tokens"] == str(tokens), tally
                # Model-only enrichment of keys already in a historical job.
                old_projection = usage()
                for index, model in ((1, "gpt-6.2-sol"), (2, "deepseek-v4.1-flash")):
                    _, _, thread, turn = run_ref(index)
                    file = native_path(root / "original-home", thread, archived=index == 2)
                    original_lines = file.read_text().splitlines()
                    if index == 2:
                        # This response has a confirmed model that differs
                        # from the same-turn requested model.
                        last = json.loads(original_lines[-1])
                        last["payload"]["response_model"] = "gpt-6.2-astra"
                        original_lines[-1] = json.dumps(last)
                    context = json.dumps({"type": "turn_context", "payload": {
                        "turn_id": turn, "model": model,
                    }})
                    file.write_text("\n".join([context, *original_lines]) + "\n")
                    maint("backfill", "--run", run_ref(index)[0], "--job", f"model-{index}")
                    assert idle(f"model-{index}")["counts"] == {"scanned": 1}
                expected = usage()
                assert expected["subtree"]["total_tokens"] == old_projection["subtree"]["total_tokens"]
                assert expected["subtree"]["reported_requests"] == old_projection["subtree"]["reported_requests"]
                models = {row["model"]: row for row in expected["subtree"]["models"]}
                assert set(models) == {None, "gpt-6.2-astra"}
                assert models["gpt-6.2-astra"]["response_model_requests"] == 1
                assert models[None]["requested_models"] == {
                    "gpt-6.2-sol": 27, "deepseek-v4.1-flash": 26,
                }
                assert models[None]["reported_requests"] == expected["subtree"]["reported_requests"] - 1
                assert maint("overview")["total"] == expected["subtree"]
                for job, scope in (("again", ("--all",)), ("issue", ("--issue", "#1")),
                                   ("runner", ("--runner", ORIGINAL)), ("host", ("--host", "fixture-host"))):
                    maint("backfill", *scope, "--job", job)
                    idle(job)
                    assert usage() == expected
                assert status("runner")["selected_runs"] == 274
                assert status("host")["selected_runs"] == 276
                assert status("issue")["selected_runs"] == RUNS

                # Deliberately damage only projections in this anonymous
                # fixture. The CLI rebuild must use durable request evidence.
                with closing(sqlite3.connect(root / "server.sqlite")) as db, db:
                    assert db.execute("SELECT count(*) FROM token_requests").fetchone()[0] == 7423
                    db.execute("DELETE FROM token_issue_rollups")
                    db.execute("UPDATE token_run_tallies SET tally = '{}', valid_records = 0")
                assert usage()["subtree"]["reported_requests"] == 0
                assert maint("rebuild", "--issue", "#1")["runs"] == RUNS
                assert usage() == expected
                assert maint("overview")["total"] == expected["subtree"]
                assert maint("rebuild", "--all")["runs"] == RUNS
                assert usage() == expected
                assert maint("overview")["total"] == expected["subtree"]
                page_query_plan(root, get)
                print(f"PASS {RUNS} Started Runs, {NATIVE} native keys, {CONSISTENT} dual-source matches, "
                      "coverage gaps, original owner, replay, real sync interleaving, correction and rebuild")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "cli", "runner"):
        parser.add_argument(name, type=lambda p: Path(p).resolve())
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-token-backfill-") as directory:
        root = Path(directory)
        verify(args, root, free_port())


if __name__ == "__main__":
    main()
