"""Measure page reads and durable Chat receipts during two independent Runner replays."""

import concurrent.futures
import http.client
import json
import math
import os
from pathlib import Path
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

from chat import server, wait_for
from issue_chat import free_port, prepare
from issue_run import runner


def sample(values):
    ordered = sorted(values)
    return {"median_ms": round(statistics.median(ordered), 2),
            "p95_ms": round(ordered[math.ceil(len(ordered) * .95) - 1], 2),
            "max_ms": round(ordered[-1], 2)}


def main():
    server_bin, _, runner_bin = (Path(value).resolve() for value in sys.argv[1:])
    with tempfile.TemporaryDirectory(prefix="tend-storage-") as directory:
        root = Path(directory)
        runner_roots = [root / "runner-a", root / "runner-b"]
        for index, work in enumerate(runner_roots):
            work.mkdir()
            prepare(work, free_port())
            config = json.loads((work / "runner.json").read_text())
            config.update(runner_id=f"load-runner-{index}", host_id=f"load-host-{index}")
            config["agents"][0]["reasoning_effort"] = "low"
            (work / "runner.json").write_text(json.dumps(config))
        address = f"http://{free_port()}"
        token = "storage-load-fixture"
        env = {**os.environ, "TEND_TOKEN": token, "TEND_URL": address}

        def request(path, body=None):
            data = json.dumps(body).encode() if body is not None else None
            url = urllib.request.Request(address + path, data, {
                "Authorization": f"Bearer {token}", "Content-Type": "application/json"
            })
            begin = time.perf_counter()
            with urllib.request.urlopen(url, timeout=5) as response:
                result = json.load(response)
            return result, (time.perf_counter() - begin) * 1000

        def send(chat, op, text):
            return request(f"/api/v1/chats/{chat}/messages", {"operation_id": op, "text": text})

        def status(issue):
            return request(f"/api/v1/issues/{issue}")[0]

        def board():
            return request("/api/v1/chats/main/issues")

        def observed_issue(chat):
            records, _ = request(f"/api/v1/chats/{chat}/issues")
            return records["issues"]

        with runner(runner_bin, runner_roots[0], env):
            with runner(runner_bin, runner_roots[1], env):
                with server(server_bin, root, env, listen=address.removeprefix("http://")):
                    send("main", "storage-issue-a", "Dispatch from Chat.")
                    [first] = wait_for(lambda: observed_issue("main"), bool)
                    wait_for(lambda: status(first["id"]), lambda view: view["status"] == "running")
                    send("second", "storage-issue-b", "Expire this reservation.")
                    [second] = wait_for(lambda: observed_issue("second"), bool)
                    issues = [status(first["id"]), wait_for(
                        lambda: status(second["id"]), lambda view: view["status"] == "running")]
                    assert len({row["placement"]["runner_id"] for row in issues}) == 2
                for work in runner_roots:
                    (work / "finish-issue").touch()
                    def stopped():
                        with sqlite3.connect(work / "runner.sqlite") as db:
                            return db.execute("SELECT state FROM local_runs").fetchall()
                    wait_for(stopped, lambda rows: rows == [("stopped",)])

        task_by_runner = {issue["placement"]["runner_id"]: issue["task_id"] for issue in issues}
        with server(server_bin, root, env, listen=address.removeprefix("http://")):
            before = board()[0]
            with sqlite3.connect(root / "server.sqlite") as db:
                for index in range(2_000):
                    source = db.execute("INSERT INTO chat_messages(id, chat_id, text) VALUES(?, 'history', 'Unrelated')",
                                        (f"load-message-{index}",)).lastrowid
                    db.execute("INSERT INTO issues(id, source_seq, title, objective) VALUES(?, ?, 'History', 'Historical Issue')",
                               (f"load-issue-{index}", source))
                    db.execute("INSERT INTO tasks(id, issue_id, category, status, repo_ids, required_capabilities) "
                               "VALUES(?, ?, 'issue-router', 'completed', '[]', '[]')",
                               (f"load-task-{index}", f"load-issue-{index}"))
                plan = db.execute("EXPLAIN QUERY PLAN SELECT t.rowid FROM tasks t WHERE issue_id = ? ORDER BY rowid DESC LIMIT 1",
                                  (first["id"],)).fetchall()
                assert any("tasks_by_issue" in detail for *_, detail in plan), plan
            assert board()[0] == before

            with sqlite3.connect(root / "server.sqlite") as db:
                [profiles] = db.execute(
                    "SELECT json_group_array(json(profile)) FROM (SELECT profile FROM runners ORDER BY id)"
                ).fetchone()
            first_profile, second_profile = json.loads(profiles)
            partial = json.dumps({"protocol": 1, "profile": first_profile, "receipts": []}).ljust(2048).encode()
            slow = http.client.HTTPConnection(address.removeprefix("http://"), timeout=5)
            slow.putrequest("POST", "/api/v1/runners/load-runner-0/sync")
            slow.putheader("Authorization", f"Bearer {token}")
            slow.putheader("Content-Length", str(len(partial)))
            slow.putheader("Content-Type", "application/json")
            slow.endheaders(partial[:1])
            try:
                receipt, _ = send("slow-link", "slow-link-probe", "Storage measurement during partial body.")
                assert receipt["message_id"]
                assert board()[0] == before
                receipt, _ = request("/api/v1/runners/load-runner-1/sync",
                                     {"protocol": 1, "profile": second_profile, "receipts": []})
                assert "confirmed_receipts" in receipt
                slow.send(partial[1:])
                with slow.getresponse() as response:
                    assert response.status == 200, response.read()
            finally:
                slow.close()

            def measure(label, barrier=None):
                if barrier is not None:
                    barrier.wait(timeout=5)
                write_times, read_times = [], []
                for index in range(40):
                    _, write = send("measurement", f"{label}-{index}", f"Storage measurement {label} {index}.")
                    page, read = board()
                    assert [entry["id"] for entry in page["issues"]] == [first["id"]]
                    write_times.append(write)
                    read_times.append(read)
                return {"chat": sample(write_times), "board": sample(read_times)}

            def replay(index, barrier):
                work = runner_roots[index]
                task = task_by_runner[f"load-runner-{index}"]
                mode = ["--once"] if index == 0 else ["--reconcile", task]
                barrier.wait(timeout=5)
                for _ in range(5):
                    subprocess.run([runner_bin, "--config", work / "runner.json", "--db", work / "runner.sqlite", *mode],
                                   env=env, check=True, capture_output=True, timeout=8)

            baseline = measure("baseline")
            barrier = threading.Barrier(3)
            with concurrent.futures.ThreadPoolExecutor(max_workers=3) as workers:
                first_replay = workers.submit(replay, 0, barrier)
                second_replay = workers.submit(replay, 1, barrier)
                during = workers.submit(measure, "replay", barrier)
                first_replay.result()
                second_replay.result()
                mixed = during.result()
            for issue in issues:
                current = status(issue["issue_id"])
                assert current["status"] == "completed"
                assert current["run"]["run_id"] == issue["run"]["run_id"]
            with sqlite3.connect(root / "server.sqlite") as db:
                assert db.execute("SELECT COUNT(*) FROM runner_run_events").fetchone() == (4,)
                assert db.execute("SELECT COUNT(*) FROM chat_messages WHERE chat_id = 'measurement' AND operation_id IS NOT NULL").fetchone() == (80,)
                assert db.execute("PRAGMA journal_mode").fetchone() == ("wal",)
        print(json.dumps({"baseline": baseline, "with_runner_replay": mixed}), flush=True)
        print("PASS bounded API reads and durable receipts during concurrent incremental and full Runner replay")


if __name__ == "__main__":
    main()
