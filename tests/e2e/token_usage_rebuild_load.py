"""Measure on-demand Token rebuild under Chat and RunnerSync writes.

Create the anonymous seed with the existing 500k Rust scale benchmark:
TEND_TOKEN_USAGE_BENCH_DB=/tmp/isolated.sqlite cargo test --release --locked \
    -p tend-server token_usage_scale -- --ignored --nocapture

This script copies the seed, verifies the fixture marker and never writes it.
"""
import argparse
from contextlib import closing
import json
import math
import os
from pathlib import Path
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request

from chat import FIXTURE, wait_for
from issue_chat import free_port


BASELINE = 500_000
INPUT_TOKENS = BASELINE * 50
AUTH = "isolated-token-rebuild-load"


def stats(samples):
    if not samples:
        return None
    ordered = sorted(samples)
    percentile = lambda p: ordered[max(0, math.ceil(len(ordered) * p) - 1)]
    return {"n": len(ordered), "p50": round(percentile(.5), 3),
            "p95": round(percentile(.95), 3), "max": round(ordered[-1], 3)}


def report_timing(stderr):
    pattern = r"sqlite-write operation=(\S+) wait_us=(\d+) transaction_us=(\d+) commit_us=(\d+)"
    ops = {}
    for op, wait, body, commit in re.findall(pattern, stderr):
        if "token_rebuild" in op or op == "token_usage_rebuild":
            ops.setdefault(op, []).append((int(body) + int(commit)) / 1000)
    return {op: stats(holds) for op, holds in ops.items()}


def run(args, directory):
    seed = args.seed
    with closing(sqlite3.connect(seed.as_uri() + "?mode=ro&immutable=1", uri=True)) as db:
        assert db.execute("SELECT id, text FROM chat_messages").fetchall() == [("source", "Synthetic")]
        assert db.execute("SELECT count(*) FROM issues").fetchone()[0] == 2_000
        assert db.execute("SELECT count(*) FROM token_runs").fetchone()[0] == 20_000
        assert db.execute("SELECT count(*) FROM token_requests").fetchone()[0] == BASELINE
    path = directory / "server.sqlite"
    shutil.copyfile(seed, path)
    with closing(sqlite3.connect(path)) as db, db:
        # Deliberately stale but decodable projection. Per-run evidence is
        # unchanged. Readers should see either this generation or the exact
        # replacement, never a partial 500k-row rebuild.
        db.execute("DELETE FROM token_issue_rollups WHERE issue_id='issue-0' AND scope='subtree'")
    address = free_port()
    base = f"http://{address}"

    def call(path, body=None):
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(base + path, data=data, headers={
            "Authorization": f"Bearer {AUTH}", "Content-Type": "application/json",
        })
        with urllib.request.urlopen(request, timeout=15) as response:
            return json.load(response)

    profile = {"runner_id": "runner", "host_id": "local", "slot_limit": 0,
               "running_slots": 0, "available_for_assignment": 0, "agents": [], "repos": []}
    sync_path = "/api/v1/runners/runner/sync"

    def chat(n):
        receipt = call("/api/v1/chats/load-fixture/messages", {
            "operation_id": f"load-chat-{n}", "text": "Is UnregisteredRepo in the list?",
        })
        assert receipt["message_id"]

    def sync(n):
        # Fan out a full sync batch across 32 deep branches, then replay
        # the same native keys with new receipts. Some replays disagree.
        if args.branches:
            roots = range(31, 63)
            run_numbers = [((root * 5 + 1) * 5 + 1) * 10 for root in roots]
            replies = [(run, n // 2, 4 if n % 2 and n // 2 % 7 == 0 else 3)
                       for run in run_numbers]
        else:
            replies = [(0, n, value) for value in ([3, 3] if n % 7 else [3, 3, 4])]
        records = []
        for i, (run, response, value) in enumerate(replies):
            records.append({
                "assignment_id": f"task-{run}", "task_id": f"task-{run}", "run_id": f"run-{run}",
                "thread_id": f"thread-run-{run}", "turn_id": f"turn-run-{run}",
                "response_id": f"load-sync-{run}-{response}", "observed_at": 1,
                "event_id": f"receipt-{n}-{i}", "last": {"total_tokens": value},
            })
        receipt = call(sync_path, {"protocol": 2, "profile": profile, "receipts": [], "token_usage": records})
        assert len(receipt.get("confirmed_token_usage", [])) == len(records)
        return [(r["response_id"], r["last"]["total_tokens"]) for r in records]

    def usage(_):
        result = call("/api/v1/issues/issue-0/token-usage")
        count = result["subtree"]["reported_requests"]
        assert count < 32_000 or BASELINE <= count < BASELINE + 32_000, count
        return count

    def worker(fn, out, ready, stop):
        for n in range(1_000):
            begun = time.monotonic()
            error, value = None, None
            try:
                value = fn(n)
            except Exception as exc:
                error = repr(exc)
            ended = time.monotonic()
            out.append((begun, ended, error, value))
            ready.set()
            if stop.wait(.015 if fn is not usage else .04):
                return

    env = {**os.environ, "TEND_TOKEN": AUTH, "TEND_URL": base, "TEND_SQLITE_TIMING": "1",
           "CODEX_HOME": str(directory / "codex-fixture")}
    operations = {}
    log = directory / "server.stderr"
    with log.open("w") as stderr:
        process = subprocess.Popen([
            args.server, "--db", path, "--listen", address, "--router-cwd", directory,
            "--app-server", sys.executable, FIXTURE, directory / "app.sqlite",
        ], env=env, stderr=stderr)
        stop = threading.Event()
        threads = []
        try:
            def health():
                try:
                    return call("/health")
                except OSError:
                    return None
            wait_for(health, bool)
            call(sync_path, {"protocol": 2, "profile": profile, "receipts": []})
            for fn in (chat, sync, usage):
                ready, samples = threading.Event(), []
                operations[fn.__name__] = samples
                thread = threading.Thread(target=worker, args=(fn, samples, ready, stop))
                threads.append(thread)
                thread.start()
                assert ready.wait(6), f"{fn.__name__} did not complete an initial request"
            time.sleep(.25)
            start = time.monotonic()
            rebuilt = json.loads(subprocess.check_output(
                [args.cli, "token-usage", "rebuild", "--all"], env=env, timeout=30,
            ))
            end = time.monotonic()
            time.sleep(.12)
            stop.set()
            for thread in threads:
                thread.join(timeout=16)
                assert not thread.is_alive()
            assert rebuilt == {"scope": {"kind": "all"}, "runs": 20_000, "direct_issues": 2_000,
                               "subtree_issues": 2_000}, rebuilt
            final = call("/api/v1/issues/issue-0/token-usage")["subtree"]
            with closing(sqlite3.connect(path)) as db:
                received = db.execute("SELECT count(*) FROM chat_messages WHERE operation_id LIKE 'load-chat-%'").fetchone()[0]
                written, accepted = db.execute(
                    "SELECT count(*), sum(conflict IS NULL) FROM token_requests WHERE response_id LIKE 'load-sync-%'"
                ).fetchone()
                assert received == sum(row[2] is None for row in operations["chat"]), received
                receipts = {}
                for _, _, error, response in operations["sync"]:
                    if error is None:
                        for key, count in response:
                            receipts.setdefault(key, set()).add(count)
                valid = [next(iter(values)) for values in receipts.values() if len(values) == 1]
                assert written == len(receipts), written
                assert accepted == len(valid), accepted
            assert final["reported_requests"] == BASELINE + accepted, final
            assert final["total_tokens"]["tokens"] == str(INPUT_TOKENS + sum(valid)), final
            timings = {}
            for name, outcomes in operations.items():
                before = [row for row in outcomes if row[1] < start]
                during = [row for row in outcomes if row[0] < end and start <= row[1]]
                assert before and during, (name, outcomes)
                timings[name] = {
                    "before_ms": stats([(b - a) * 1000 for a, b, _, _ in before]),
                    "during_ms": stats([(b - a) * 1000 for a, b, _, _ in during]),
                    "finished_inside": sum(b <= end and error is None for _, b, error, _ in during),
                    "errors": [error for _, _, error, _ in outcomes if error is not None],
                }
                if args.shadow:
                    assert not timings[name]["errors"], timings
                    assert timings[name]["finished_inside"] > 0, timings
        finally:
            stop.set()
            for thread in threads:
                thread.join(timeout=16)
            process.terminate()
            process.wait(timeout=6)
    holds = report_timing(log.read_text())
    if args.shadow:
        assert {"token_rebuild_stage", "token_rebuild_catchup", "token_rebuild_catchup_issues",
                "token_rebuild_catchup_ack", "token_rebuild_publish"} <= holds.keys(), holds
        if args.branches:
            assert holds["token_rebuild_catchup_issues"]["n"] > holds["token_rebuild_catchup_ack"]["n"], holds
    print(json.dumps({"seed_requests": BASELINE, "runs": 20_000, "issues": 2_000,
                      "sync_branches": 32 if args.branches else 1,
                      "wall_ms": round((end - start) * 1000, 3), "writes_and_reads": timings,
                      "writer_hold_ms": holds, "chat_committed": received,
                      "sync_distinct_keys": written, "sync_unconflicted": accepted,
                      "final_requests": final["reported_requests"]}))
    print("PASS concurrent Chat, real RunnerSync, exact post-rebuild counts and projection visibility")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "cli", "seed"):
        parser.add_argument(name, type=lambda p: Path(p).resolve())
    parser.add_argument("--shadow", action="store_true", help="Require observed shadow catch-up and availability")
    parser.add_argument("--branches", action="store_true", help="Write distinct keys on 32 disjoint leaf branches")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-rebuild-load-") as tmp:
        run(args, Path(tmp))


if __name__ == "__main__":
    main()
