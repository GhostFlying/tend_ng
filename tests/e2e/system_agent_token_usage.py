"""Exercise all Tend meter producers, restart, historical overlap and the actual global screen."""
import argparse
from decimal import Decimal
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import time

from chat import server, wait_for
from issue_chat import TESTS, WEB, free_port, prepare
from issue_token_usage import runner_process


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for binary in ("server", "cli", "runner"):
        parser.add_argument(binary, type=lambda path: Path(path).resolve())
    parser.add_argument("--legacy-runner", type=lambda path: Path(path).resolve())
    parser.add_argument("--screenshots", type=lambda path: Path(path).resolve())
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-system-usage-") as temp:
        run(args, Path(temp))


def run(args, root):
    addr = free_port()
    prepare(root, free_port())
    discussion_home, discussion_cwd, assessor_home, assessor_cwd, router_home, runner_home = (
        root / name for name in ("discussion-home", "discussion-cwd", "assessor-home",
                                "assessor-cwd", "router-home", "runner-home"))
    for path in (discussion_home, discussion_cwd, assessor_home, assessor_cwd, router_home, runner_home):
        path.mkdir()
    for path in (root, discussion_home, assessor_home):
        (path / "system-meter-fixture").touch()
    (discussion_home / "synthetic-key").write_text("configured-discussion-key\n")

    def install(name, source):
        file = root / name
        file.write_text(f"#!{sys.executable}\n" + (TESTS / "fixtures" / source).read_text())
        file.chmod(0o700)
        return str(file)

    discussion = root / "discussion.json"
    discussion.write_text(json.dumps({
        "host_id": "fixture-host", "program": install("discussion-app", "discussion_app_server.py"),
        "codex_home": str(discussion_home), "cwd": str(discussion_cwd),
        "model_api_keys": {"FIXTURE_API_KEY": "discussion-home/synthetic-key"},
    }))
    feishu = root / "feishu.json"
    feishu.write_text(json.dumps({
        "owner_open_id": "ou_fixture_owner", "profile": "fixture-user",
        "cli": install("lark-fixture", "feishu_cli.py"),
        "agent": {"id": "FeishuFixture", "program": install("assessor-app", "feishu_app_server.py"),
                  "home": str(assessor_home), "cwd": str(assessor_cwd)},
    }))
    date = int(time.time())
    (root / "feishu-scenario.json").write_text(json.dumps({"mode": "empty", "at": date}))
    env = {**os.environ, "CODEX_HOME": str(router_home), "TEND_TOKEN": "isolated-system-usage",
           "TEND_URL": f"http://{addr}"}
    options = dict(listen=addr, discussion_config=discussion, feishu_config=feishu,
                   web_dir=WEB / "dist" if args.screenshots else None)
    command = [args.runner, "--config", root / "runner.json", "--db", root / "runner.sqlite",
               "--app-server", sys.executable, TESTS / "fixtures/system_meter_runner.py"]
    runner_env = {**env, "CODEX_HOME": str(runner_home)}

    def cli(*words):
        result = subprocess.run([args.cli, *words], env=env, text=True, capture_output=True, timeout=15)
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout)

    def overview():
        return cli("token-usage", "overview")

    def sources():
        return {row["source"]: row for row in overview()["sources"]}

    def speak(chat, text):
        msg = cli("chat", "send", chat, text)["message_id"]
        rows = wait_for(lambda: cli("chat", "read", chat)["messages"],
                        lambda rows: any(row["reply_to"] == msg for row in rows))
        return next(row for row in rows if row["id"] == msg)

    def sql(statement, *params):
        with sqlite3.connect(root / "server.sqlite", timeout=5) as conn:
            return conn.execute(statement, params).fetchall()

    def requests(source):
        return sources()[source]["usage"]["reported_requests"]

    def job(job_id):
        receipt = cli("token-usage", "backfill", "--all", "--job", job_id)
        assert receipt["job"]["job_id"] == job_id
        return wait_for(lambda: cli("token-usage", "status", job_id),
                        lambda result: result["state"].startswith("finished"))

    with runner_process(command, runner_env, root):
        with server(args.server, root, env, **options):
            wait_for(lambda: cli("feishu-suggestions", "list")["scan"]["phase"], lambda state: state == "idle")
            assert requests("feishu") == 0, "the mechanical scan did not run a model"
            speak("main", "Hello, Tend.")
            speak("main", "Next input.")
            assert requests("router") == 2
            router = sources()["router"]["usage"]
            models = {model["model"]: model for model in router["models"]}
            assert set(models) == {"gpt-6.2-sol", "gpt-6.2-astra"}, router
            assert models["gpt-6.2-sol"]["selected_model_requests"] == 1
            assert models["gpt-6.2-astra"]["response_model_requests"] == 1
            # The unclassified input remainder uses the ordinary input rate.
            assert models["gpt-6.2-sol"]["estimate"] == {"usd": "0.001642", "partial": True}
            assert models["gpt-6.2-astra"]["estimate"] == {"usd": "0.00656", "partial": True}
            assert router["estimate"] == {"usd": "0.008202", "partial": True}
            if args.screenshots:
                subprocess.run(
                    ["node", "tests/system-agent-token-usage.mjs", "2"],
                    cwd=WEB, env={**env, "TEND_SCREENSHOTS": str(args.screenshots)}, check=True, timeout=45)
            [(same_thread, num_turns, num_keys)] = sql(
                "SELECT count(DISTINCT thread),count(DISTINCT turn),count(DISTINCT response) FROM token_agent_requests")
            assert (same_thread, num_turns, num_keys) == (1, 2, 1)

            [issue] = speak("main", "Dispatch from Chat.")["issue_ids"]
            wait_for(lambda: requests("runner"), lambda n: n == 4)
            original = cli("issue", "token-usage", issue)
            assert original["direct"]["total_tokens"] == {"tokens": "10000", "reported_requests": 3}, original
            assert original["direct"]["reported_requests"] == 4
            assert original["direct"]["cache_write_input_tokens"]["tokens"] is None
            assert sql("SELECT count(DISTINCT thread_id), count(DISTINCT turn_id), count(DISTINCT response_id) FROM token_requests") == [(2, 3, 2)]

            discussion_id = cli("discussion", "create")["chat_id"]
            speak(discussion_id, "Compare two local export formats.")
            speak(discussion_id, "Discussion with unavailable counts.")
            assert requests("discussion") == 2
            assert sources()["discussion"]["responses_without_usage"] == 1

            # Old quick receipts intentionally use the separate restricted
            # receiving entry point. The normal summary still has its own source.
            sql("""CREATE TRIGGER use_legacy_receiver AFTER INSERT ON discussion_handoffs
                   WHEN json_extract(NEW.intent, '$.mode')='quick'
                   BEGIN UPDATE discussion_handoffs SET intent='{"mode":"quick"}'
                    WHERE message_id=NEW.message_id; END""")
            cli("discussion", "handoff-now", discussion_id)
            wait_for(lambda: (requests("discussion_summary"), requests("handoff_receiver")), lambda pair: pair == (1, 1))

            sql("UPDATE feishu_scan SET checkpoint_at=?, last_success_at=?", date - 7200, date - 7200)
            scenario = root / "next-feishu.json"
            scenario.write_text(json.dumps({"mode": "recover", "at": date}))
            scenario.replace(root / "feishu-scenario.json")
            cli("feishu-suggestions", "sync")
            wait_for(lambda: requests("feishu"), lambda n: n == 4)
            models = sources()["feishu"]["usage"]["models"]
            assert models[0]["model"] == "fixture-confirmed-model" and models[0]["selected_model_requests"] == 4
            assert sources()["feishu"]["usage"]["estimate"]["usd"] is None
            assert sql("SELECT DISTINCT selected_model,codex_home FROM token_feishu_attempt_launch") == [
                ("fixture-confirmed-model", str(assessor_home / ".codex-ark"))]

            # A completed native turn whose startup reply was dropped is
            # recoverable, but its queued meter must not be guessed live.
            before_recovery = requests("router")
            speak("main", "Recover this reply.")
            assert requests("router") == before_recovery
            initial = overview()

        replacement = {**env, "CODEX_HOME": str(root / "replacement-router-home")}
        with server(args.server, root, replacement, **options):
            assert overview() == initial
            # The existing attempts retain their old absolute paths after the
            # service is reconfigured. Even a readable bound file must obey
            # the original home's layout before any response can enter usage.
            sessions = router_home / "sessions"
            moved = root / "moved-router-sessions"
            sessions.rename(moved)
            sessions.symlink_to(moved, target_is_directory=True)
            blocked = job("system-history-linked-root")
            [router] = [row for row in blocked["server"] if row["source"] == "router"]
            assert (router["state"], router["detail"], router["native_keys"]) == (
                "gap", "source_unreadable", 0), router
            assert requests("router") == before_recovery
            sessions.unlink()
            moved.rename(sessions)

            archived = router_home / "archived_sessions"
            archived.symlink_to(sessions, target_is_directory=True)
            blocked = job("system-history-linked-sibling")
            [router] = [row for row in blocked["server"] if row["source"] == "router"]
            assert (router["state"], router["detail"], router["native_keys"]) == (
                "gap", "source_unreadable", 0), router
            assert requests("router") == before_recovery
            archived.unlink()

            receipt = job("system-history-first")
            rows = receipt["server"]
            assert any(r["source"] == "router" and r["state"] == "scanned" for r in rows), rows
            assert any(r["source"] == "feishu" and r["detail"] == "source_missing"
                       and r["attempts"] == 4 for r in rows), rows
            assert requests("router") == before_recovery + 1
            assert sources()["discussion"]["responses_without_usage"] == 0
            assert sources()["discussion"]["usage"]["cache_write_input_tokens"] == {
                "tokens": None, "reported_requests": 0,
            }, "a normalized zero must not override explicitly missing provider fields"
            assert cli("issue", "token-usage", issue) == original, "Runner native child overlap adds no extra usage"
            assert receipt["counts"].get("scanned") == 1, receipt
            assert receipt["runs"][0]["native_keys"] == 3, receipt
            merged = overview()
            assert merged["total"]["reported_requests"] == sum(row["usage"]["reported_requests"] for row in merged["sources"])
            assert int(merged["total"]["total_tokens"]["tokens"]) == sum(
                int(row["usage"]["total_tokens"]["tokens"] or 0) for row in merged["sources"])
            for source in merged["sources"]:
                usage = source["usage"]
                assert usage["reported_requests"] == sum(row["reported_requests"] for row in usage["models"])
                assert int(usage["total_tokens"]["tokens"] or 0) == sum(
                    int(row["total_tokens"]["tokens"] or 0) for row in usage["models"])
                parts = [Decimal(row["estimate"]["usd"]) for row in usage["models"]
                         if row["estimate"]["usd"] is not None]
                assert (Decimal(usage["estimate"]["usd"]) if usage["estimate"]["usd"] is not None else None
                        ) == (sum(parts) if parts else None)
            sol = next(row for row in sources()["router"]["usage"]["models"] if row["model"] == "gpt-6.2-sol")
            assert sol["selected_model_requests"] > 0 and sol["response_model_requests"] > 0
            cli("token-usage", "rebuild", "--all")
            assert overview() == merged
            archived.mkdir()
            for path in sessions.glob("rollout-*.jsonl"):
                path.rename(archived / path.name)
            for job_id in ("system-history-first", "system-history-repeat"):
                repeated = job(job_id)
                assert any(row["source"] == "router" and row["state"] == "scanned"
                           and row["native_keys"] > 0 for row in repeated["server"]), repeated
                assert overview() == merged
            print("PASS bound Router history rejects linked session roots, recovers from the original home after config drift and archive, and never double counts")
    if args.legacy_runner:
        # Reuse only the local database and retained evidence of this isolated
        # fixture, with the actual pre-feature Runner attached to the new Server.
        with server(args.server, root, env, **options), runner_process(
            [args.legacy_runner, *command[1:]], runner_env, root
        ):
            [(run,)] = sql("SELECT run_id FROM token_runs")
            legacy = "history-with-baseline-reader"
            cli("token-usage", "backfill", "--run", run, "--job", legacy)
            result = wait_for(lambda: cli("token-usage", "status", legacy),
                              lambda status: status["state"].startswith("finished"))
            [row] = result["runs"]
            assert row["state"] == "gap" and row["gap"] == "child_reader_unsupported", result
            assert row["native_keys"] == 1 and overview() == merged
        print("PASS actual pre-feature Runner contributes only the root and explicitly reports missing child coverage")
    print("PASS source attribution, upstream identity/replay, same-thread turns, native children, Feishu privacy, persisted restart and full historical overlap")


if __name__ == "__main__":
    main()
