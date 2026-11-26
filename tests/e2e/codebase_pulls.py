"""Verify Codebase linking, observation and branch discovery through Tend's real binaries."""

import argparse
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import urllib.request

from chat import server, wait_for
from child_windows import expire_child_window
from issue_chat import free_port, prepare, REQUEST, TESTS, WEB

MR = "https://code.byted.org/example/guide/merge_requests/"
GH = "https://github.com/example/guide/pull/73"


def exercise(args, root, automatic):
    address = free_port()
    prepare(root, free_port(), "A fixed repository.\n")
    config = root / "runner.json"
    runner_config = json.loads(config.read_text())
    runner_config["agents"][0]["reasoning_effort"] = "low"
    config.write_text(json.dumps(runner_config))
    # Keep the Codebase identity in Git config without fetching a real repository.
    repo = root / "example"
    origin = "git@code.byted.org:example/guide.git"
    push = "https://code.byted.org/example/guide.git"
    fork = "git@code.byted.org:fork/guide.git"

    def git(*words):
        return subprocess.check_output(["git", "-C", repo, *words], text=True).strip()

    git("remote", "set-url", "origin", origin)
    git("config", f"url.{root / 'source'}.insteadOf", origin)
    if automatic == "rewritten":
        git("config", f"url.{fork}.pushInsteadOf", origin)
        expected_push = [fork]
    else:
        git("config", "remote.origin.pushurl", push)
        expected_push = [push]
        if automatic == "multiple":
            git("config", "--add", "remote.origin.pushurl", fork)
            expected_push.append(fork)
    assert git("remote", "get-url", "origin") == str(root / "source")
    assert git("remote", "get-url", "--push", "--all", "origin").splitlines() == expected_push
    (root / "delegate-now").touch()
    mode = root / "codebase-discovery-mode"
    mode.write_text("missing")
    for number in (204, 206):
        (root / f"comments-{number}.json").write_text(json.dumps({
            "Threads": [{"Id": "history-thread", "Comments": [{"Id": "history-before-discovery",
                       "CreatedAt": "2026-11-26T03:00:00Z", "Content": "Predates branch discovery"}]}],
            "Events": [],
        }))
    env = {**os.environ, "TEND_TOKEN": "codebase-fixture-token", "TEND_URL": f"http://{address}"}
    server_env = {
        **env, "TEND_CODEBASE_FIXTURE_DIR": str(root), "TEND_GITHUB_FIXTURE_DIR": str(root),
        "GH_TOKEN": "fixture-read-token",
    }

    def cli(*words, error=None):
        proc = subprocess.run([args.cli, *words], env=env, text=True, capture_output=True, timeout=5)
        if error:
            assert proc.returncode and error in proc.stderr, proc.stderr
            return None
        assert proc.returncode == 0, proc.stderr
        return json.loads(proc.stdout)

    def lookup(issue, number):
        return next((link for link in cli("pr", "list", issue) if link["url"] == MR + str(number)), None)

    def force_due(number):
        with sqlite3.connect(root / "server.sqlite") as db:
            db.execute("UPDATE pulls SET due_at = 0 WHERE url = ?", [MR + str(number)])

    screenshots = args.screenshots if automatic == "unique" else None
    with server(args.server, root, server_env, listen=address,
                codebase_cli=TESTS / "fixtures/codebase_cli.py", github_cli=TESTS / "fixtures/github_cli.py",
                web_dir=WEB / "dist" if screenshots else None):
        runner = subprocess.Popen([
            args.runner, "--config", config, "--db", root / "runner.sqlite", "--app-server",
            sys.executable, TESTS / "fixtures/coordination_app_server.py", root, "codebase-pull",
        ], env=env)
        try:
            cli("chat", "send", "main", REQUEST)
            delegation = root / "delegation.json"
            wait_for(delegation.exists, bool)
            work = json.loads(delegation.read_text())["action"]
            task, child = work["task_id"], work["issue_id"]
            message = cli("chat", "read", "main")["messages"][0]
            [parent] = message["issue_ids"]
            [parent_number] = message["issue_numbers"]
            branch = f"tend/{parent}/example"
            (root / "recorded-branch").write_text(branch)
            wait_for((root / "pull-link.json").exists, bool)
            attached = json.loads((root / "pull-link.json").read_text())
            assert attached["action"]["url"] == MR + "73" and attached["operation_id"]
            ongoing = wait_for(lambda: cli("issue", "read", child),
                               lambda value: value["status"] == "running" and value.get("run"))
            assert ongoing["status"] == "running"
            source = "fork/guide" if automatic == "rewritten" else "example/guide"
            expected_branches = [] if automatic == "multiple" else [
                {"provider": "codebase", "repo_id": "example", "repository": "example/guide",
                 "head_repository": source, "branch": branch},
            ]
            assert ongoing["run"].get("branches", []) == expected_branches, ongoing["run"]
            known = wait_for(lambda: lookup(child, 73), lambda value: value and value["facts"])
            assert known["provider"] == "codebase" and known["number"] == 73
            assert known["repository"] == "example/guide" and known["observed_at"]
            assert known["observation_error"] is None
            assert known["facts"] == {
                "title": "Synthetic merge request 73", "state": "merged", "draft": False,
                "head_sha": "a" * 40, "head_branch": "fixture/guide", "head_repository": "example/guide",
                "merge_commit_sha": "b" * 40, "ci": "success",
            }
            assert not [job for job in cli("pr", "discoveries", child) if job["automatic"]]
            if automatic == "multiple":
                cli("pr", "find", child, task, "multiple-source")
                [result] = cli("pr", "discoveries", child, "multiple-source")
                assert result["state"] == "no_recorded_branch" and result["results"] == []

            if automatic == "unique":
                receipt = cli("pr", "link", f"#{parent_number}", MR + "073/?tab=diff#note", "link-mr")
                assert receipt == {"issue_id": parent, "url": MR + "73"}
                assert cli("pr", "link", parent, MR + "73", "link-mr") == receipt
                assert cli("pr", "link", parent, MR + "73/") == receipt
                assert lookup(parent, 73) == known
                cli("pr", "link", parent, MR + "74", "link-mr", error="HTTP 409")
                cli("pr", "link", parent, GH, "link-mr", error="HTTP 409")
                cli("pr", "link", child, MR + "73", "link-mr", error="HTTP 409")
                cli("pr", "link", parent, MR + "73/diffs", error="HTTP 400")
                cli("pr", "link", "missing-issue", MR + "73", error="HTTP 404")
                cli("pr", "link", child, GH)
                cli("pr", "link", child, MR + "74")
                cli("pr", "link", child, MR + "76")
                cli("pr", "link", child, MR + "99")

                for name, state in [("missing", "missing"), ("source-unknown", "invalid_response"),
                                    ("later", "linked")]:
                    mode.write_text(name)
                    op = f"manual-{name}"
                    receipt = cli("pr", "find", parent, task, op)
                    assert cli("pr", "find", parent, task, op) == receipt
                    result = wait_for(lambda: cli("pr", "discoveries", parent, op)[0],
                                      lambda value: value["state"] == "completed")
                    assert result["results"][0]["state"] == state, result
                    assert result["results"][0]["url"] == (MR + "204" if state == "linked" else None)

            lookup_mode = "unique" if automatic in ("rewritten", "multiple") else automatic
            mode.write_text(lookup_mode)
            (root / "finish-work").touch()
            stopped = wait_for(lambda: cli("issue", "read", child),
                               lambda value: value["status"] == "completed")
            assert stopped["run"]["phase"]["state"] == "stopped"
            assert stopped["run"].get("branches", []) == expected_branches, stopped
            if automatic == "multiple":
                assert not [job for job in cli("pr", "discoveries", child) if job["automatic"]]
                calls = [json.loads(line) for line in (root / "codebase-reads").read_text().splitlines()]
                assert not any(call[0] == "list" for call in calls), calls
                assert [link["url"] for link in cli("pr", "list", child)] == [MR + "73"]
                return

            [job] = wait_for(lambda: [job for job in cli("pr", "discoveries", child)
                                     if job["automatic"] and job["state"] == "completed"], bool)
            expected = {"unique": "linked", "rewritten": "linked",
                        "ambiguous": "ambiguous", "denied": "not_accessible"}[automatic]
            linked_url = {"unique": MR + "204", "rewritten": MR + "206"}.get(automatic)
            assert job["task_id"] == task and job["results"][0]["state"] == expected, job
            assert job["results"][0]["url"] == linked_url
            calls = [json.loads(line) for line in (root / "codebase-reads").read_text().splitlines()]
            assert calls.count(["list", lookup_mode, branch, 1]) == 1
            if linked_url:
                with sqlite3.connect(root / "server.sqlite") as db:
                    old_comment = db.execute(
                        "SELECT l.baseline_at, s.input_id FROM codebase_comment_links l "
                        "JOIN codebase_comment_sightings s USING(issue_id,url) "
                        "WHERE l.issue_id=? AND l.url=? AND s.comment_key='thread_comment:history-before-discovery'",
                        (child, linked_url),
                    ).fetchall()
                assert len(old_comment) == 1 and old_comment[0][0] and old_comment[0][1] is None, old_comment
            if automatic != "unique":
                expected_links = {MR + "73", linked_url} if linked_url else {MR + "73"}
                assert {link["url"] for link in cli("pr", "list", child)} == expected_links
                return

            unknown = wait_for(lambda: lookup(child, 74), lambda value: value["facts"])
            assert unknown["facts"]["ci"] is None and unknown["observation_error"] is None
            assert unknown["facts"]["state"] == "closed"
            failed = wait_for(lambda: lookup(child, 76), lambda value: value["facts"])
            assert failed["facts"]["ci"] == "failure"
            denied = wait_for(lambda: lookup(child, 99), lambda value: value["observation_error"])
            assert denied["observation_error"] == "not_accessible"
            assert denied["facts"] is None and denied["observed_at"] is None

            # Keep the provider success counters unchanged for the missing-conclusion case.
            for filename in ["partial-checks-73", "missing-conclusion-73"]:
                flag = root / filename
                flag.touch()
                force_due(73)
                uncertain = wait_for(lambda: lookup(child, 73),
                                     lambda value: value["facts"]["ci"] is None)
                assert uncertain["facts"] == {**known["facts"], "ci": None}
                assert uncertain["observation_error"] is None
                flag.unlink()
                force_due(73)
                known = wait_for(lambda: lookup(child, 73),
                                 lambda value: value["facts"]["ci"] == "success")

            (root / "deny-mr-73").touch()
            force_due(73)
            inaccessible = wait_for(lambda: lookup(child, 73), lambda value: value["observation_error"])
            assert inaccessible == {**known, "observation_error": "not_accessible"}
            (root / "deny-mr-73").unlink()
            (root / "timeout-checks-73").touch()
            force_due(73)
            timed_out = wait_for(lambda: lookup(child, 73),
                                 lambda value: value["observation_error"] == "codebase_query_timeout")
            assert timed_out == {**known, "observation_error": "codebase_query_timeout"}
            assert lookup(parent, 73) == timed_out

            found = wait_for(lambda: lookup(child, 204), lambda value: value and value["facts"])
            assert found["facts"]["draft"] and found["facts"]["ci"] == "pending"
            assert found["facts"]["head_branch"] == branch
            assert [call for call in calls if call[0] == "list"] == [
                ["list", "missing", branch, 1], ["list", "source-unknown", branch, 1],
                ["list", "later", branch, 1], ["list", "later", branch, 2],
                ["list", "unique", branch, 1],
            ]
            board = cli("issue", "list", "--scope=roots")["issues"]
            [card] = [entry for entry in board if entry["id"] == parent]
            urls = [pull["url"] for pull in card["pulls"]]
            assert len(urls) == 6 and set(urls) == {GH, *(MR + str(n) for n in (73, 74, 76, 99, 204))}
            github = next(pull for pull in card["pulls"] if pull["url"] == GH)
            assert github["provider"] == "github" and github["facts"]["ci"] == "success"
            request = urllib.request.Request(f"{env['TEND_URL']}/api/v1/issues/{child}/overview",
                                             headers={"Authorization": f"Bearer {env['TEND_TOKEN']}"})
            with urllib.request.urlopen(request, timeout=5) as response:
                detail = json.load(response)
            assert sorted(detail["pulls"], key=lambda link: link["url"]) == sorted(
                cli("pr", "list", child), key=lambda link: link["url"])

            if screenshots:
                expire_child_window(root, "issue_closed")
                wait_for(lambda: cli("issue", "list", "--scope=roots")["issues"],
                         lambda rows: any(row["id"] == parent and row["stage"] == "done" for row in rows))
                subprocess.run(
                    ["node", WEB / "tests/codebase-pulls.mjs"], check=True, timeout=45,
                    env={**env, "TEND_CODEBASE_SCREENSHOTS": str(screenshots),
                         "TEND_CODEBASE_PARENT": str(parent_number),
                         "TEND_CODEBASE_CHILD": str(detail["short_number"])},
                )
        finally:
            (root / "finish-work").touch()
            runner.terminate()
            runner.wait(timeout=7)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("server", "cli", "runner"):
        parser.add_argument(executable, type=lambda value: Path(value).resolve())
    parser.add_argument("--screenshots", type=lambda value: Path(value).resolve())
    args = parser.parse_args()
    for outcome in ["unique", "ambiguous", "denied", "rewritten", "multiple"]:
        with tempfile.TemporaryDirectory(prefix=f"tend-codebase-{outcome}-") as directory:
            exercise(args, Path(directory), outcome)
        print(f"PASS Codebase terminal discovery: {outcome}", flush=True)


if __name__ == "__main__":
    main()
