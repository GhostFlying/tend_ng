"""Verify Feishu scans, isolation, recovery and explicit transfers with local fixtures."""
import argparse
from datetime import datetime
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import time

from chat import lose_first_response, server, wait_for
from discussions import sql
from issue_chat import TESTS, WEB, free_port


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server", type=lambda s: Path(s).resolve())
    parser.add_argument("cli", type=lambda s: Path(s).resolve())
    parser.add_argument("--screenshots", type=lambda s: Path(s).resolve())
    args = parser.parse_args()
    if args.screenshots:
        args.screenshots.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="tend-feishu-fixture-") as directory:
        run(args, Path(directory))


def run(args, root):
    at = int(time.time())
    address = free_port()
    env = {**os.environ, "TEND_TOKEN": "test-only-feishu", "TEND_URL": f"http://{address}"}
    db = root / "server.sqlite"
    (root / "assessor-home").mkdir()
    (root / "assessor-cwd").mkdir()
    (root / "discussion-home").mkdir()
    (root / "discussion-cwd").mkdir()
    (root / "discussion-home" / "synthetic-key").write_text("configured-discussion-key\n")

    def executable(out, source):
        path = root / out
        path.write_text(f"#!{sys.executable}\n" + (TESTS / "fixtures" / source).read_text())
        path.chmod(0o700)
        return path

    executable("lark-fixture", "feishu_cli.py")
    # The test fixture has the real default program name so model/effort defaults are exercised.
    executable("codex-ark", "feishu_app_server.py")
    executable("discussion-fixture", "discussion_app_server.py")
    feishu = root / "feishu.json"
    feishu.write_text(json.dumps({
        "owner_open_id": "ou_fixture_owner", "profile": "fixture-user", "cli": "./lark-fixture",
        "agent": {"id": "FeishuFixture", "home": "./assessor-home", "cwd": "./assessor-cwd"},
    }))
    env["PATH"] = f"{root}:{env['PATH']}"
    discussion = root / "discussion.json"
    discussion.write_text(json.dumps({
        "host_id": "fixture-host", "program": "./discussion-fixture",
        "codex_home": "./discussion-home", "cwd": "./discussion-cwd",
        "model_api_keys": {"FIXTURE_API_KEY": "discussion-home/synthetic-key"},
    }))
    assessor_log = root / "assessor-home" / "assessed.jsonl"

    def set_state(mode, **kwargs):
        path = root / "next-scenario"
        path.write_text(json.dumps({"mode": mode, "at": at, **kwargs}))
        path.replace(root / "feishu-scenario.json")

    def cli(*words, upstream=address, error=None):
        reply = subprocess.run([args.cli, *map(str, words)], env={**env, "TEND_URL": f"http://{upstream}"},
                               capture_output=True, text=True, timeout=20)
        if error:
            assert reply.returncode and error in reply.stderr, reply.stderr
            return None
        assert reply.returncode == 0, reply.stderr
        return json.loads(reply.stdout)

    def feed(*words):
        return cli("feishu-suggestions", "list", *words)

    def use(*words, **kwargs):
        return cli("feishu-suggestions", *words, **kwargs)

    def card(anchor):
        return next((row for row in feed()["suggestions"] if row["evidence"]["anchor"]["id"] == anchor), None)

    def wait_scan(state):
        return wait_for(feed, lambda r: r["scan"]["phase"] == state)

    def set_checkpoints(statement, *parameters):
        with sqlite3.connect(db, timeout=5) as connection:
            connection.execute(statement, parameters)

    def list_calls():
        return [json.loads(line) for line in (root / "feishu-cli-calls.jsonl").read_text().splitlines()]

    def search_calls(start):
        items = []
        for c in list_calls()[start:]:
            if "/open-apis/im/v1/messages/search" in c:
                items.append((json.loads(c[c.index("--params") + 1]), json.loads(c[c.index("--data") + 1])))
        return items

    def status(id, revision):
        return use("handoff-status", id, revision)

    options = dict(listen=address, feishu_config=feishu, discussion_config=discussion,
                   web_dir=WEB / "dist" if args.screenshots else None)
    with server(args.server, root, env, listen=address):
        assert feed()["scan"]["phase"] == "disabled" and not feed()["suggestions"]
        use("sync", error="HTTP 409")

    set_state("empty")
    with server(args.server, root, env, **options):
        initial = wait_scan("idle")["scan"]
        assert initial["last_success_at"] >= at and initial["checkpoint_at"] >= at
        assert not feed()["suggestions"] and not assessor_log.exists()
        assert cli("chat", "read", "main")["messages"] == []
        assert feed()["assessor"] == {"id": "FeishuFixture", "category": "feishu-router"}
        assert not [c for c in list_calls() if "messages-list" in " ".join(c)]

    # Reproduce the LDD failure shape: process restarted, previous attempt still marked running,
    # and the last successful cursor eight days old. Page one succeeds then page two fails.
    outage = at - 8 * 86400
    set_checkpoints("UPDATE feishu_scan SET phase='running',checkpoint_at=?,last_success_at=?", outage, outage)
    set_state("failure")
    failure_start = len(list_calls())
    with server(args.server, root, env, **options):
        failed = wait_scan("failed")
        assert failed["scan"]["checkpoint_at"] == outage and failed["scan"]["last_success_at"] == outage
        assert failed["scan"]["gap"] == "provider_unavailable" and not failed["suggestions"]
        assert not assessor_log.exists()
        failed_calls = search_calls(failure_start)
        assert len(failed_calls) == 2 and "page_token" in failed_calls[1][0]

        set_state("gap")
        use("sync")
        gap = wait_for(feed, lambda r: r["scan"].get("gap") == "incomplete_search")
        assert gap["scan"]["checkpoint_at"] == outage and not gap["suggestions"]

    set_state("missing-detail")
    missing_start = len(list_calls())
    with server(args.server, root, env, **options):
        failed = wait_scan("failed")
        assert failed["scan"]["gap"] == "incomplete_search" and failed["scan"]["checkpoint_at"] == outage
        assert len(search_calls(missing_start)) == 1
        assert any("+messages-mget" in args for args in list_calls()[missing_start:])
        assert not failed["suggestions"]

    set_state("wrong-user")
    start = len(list_calls())
    with server(args.server, root, env, **options):
        assert wait_scan("failed")["scan"]["gap"] == "identity"
        assert list_calls()[start:] == [["whoami"]]

    set_state("recover")
    start = len(list_calls())
    with server(args.server, root, env, **options):
        recovered = wait_scan("idle")
        assert recovered["scan"]["last_success_at"] >= at
        assert recovered["scan"]["checkpoint_at"] >= at and recovered["scan"]["gap"] is None
        calls = search_calls(start)
        bounds = [(body["filter"]["time_range"]["start_time"], body["filter"]["time_range"]["end_time"])
                  for params, body in calls if "page_token" not in params]
        assert len(bounds) >= 8
        assert datetime.fromisoformat(bounds[0][0]).timestamp() == outage - 900
        assert bounds[0][0] == failed_calls[0][1]["filter"]["time_range"]["start_time"]
        assert all(datetime.fromisoformat(end).timestamp() - 900 == datetime.fromisoformat(start).timestamp()
                   for (_, end), (start, _) in zip(bounds, bounds[1:]))
        # Recovery produced one row per topic, not per hit. @all, self, plain text, old and failed pages are absent.
        assert {c["evidence"]["anchor"]["id"] for c in recovered["suggestions"]} == {
            "om_qa_anchor", "om_other_topic", "om_docs_anchor", "om_release_anchor",
        }
        assert sql(db, "SELECT count(*) FROM feishu_mentions") == [(5,)]
        assert not cli("chat", "read", "main")["messages"]
        assert sql(db, "SELECT count(*) FROM issues") == [(0,)]
        assert sql(db, "SELECT count(*) FROM runner_assignments") == [(0,)]
        print("PASS empty scan, eight-day running-row restart, failed page rollback, gap, pinned identity and segmented checkpoint recovery", flush=True)

        def judged(anchor):
            return wait_for(lambda: card(anchor), lambda c: c and c["evidence"]["assessment"])

        qa = judged("om_qa_anchor")
        resolved = judged("om_docs_anchor")
        release = judged("om_release_anchor")
        judged("om_other_topic")
        assert [m["id"] for m in qa["evidence"]["replies"]] == ["om_qa_reply"]
        assert resolved["evidence"]["assessment"]["disposition"] == "resolved"
        assert [m["id"] for m in release["evidence"]["mentions"]] == ["om_release_anchor", "om_release_update"]
        id, revision = qa["evidence"]["id"], qa["evidence"]["revision"]
        assert use("ignore", id, revision)["ignored"]
        assert use("read", id)["ignored"]
        one_thread = use("discuss", id, revision)["chat_id"]
        assert use("discuss", id, revision)["chat_id"] == one_thread
        existing = cli("discussion", "create", "已有的验收讨论")["chat_id"]
        assert use("discuss", id, revision, existing)["chat_id"] == existing
        assert not cli("chat", "read", existing)["messages"]
        assert not cli("chat", "read", "main")["messages"]
        assert {a["chat_id"] for a in use("read", id)["discussions"]} == {one_thread, existing}

        # Simulate the passage of the context poll interval. A new reply is
        # deliberately NOT an @mention and must still advance the same card.
        set_state("recover", progress=True)
        set_checkpoints("UPDATE feishu_suggestions SET checked_at=0 WHERE id=?", id)
        use("sync")
        advanced = wait_for(lambda: use("read", id),
                            lambda c: c["evidence"]["revision"] > revision and c["evidence"]["assessment"])
        updated = advanced["evidence"]["revision"]
        assert advanced["ignored"] is False and "Android 12" in advanced["evidence"]["assessment"]["overview"]
        assert [m["id"] for m in advanced["evidence"]["replies"]] == ["om_qa_reply", "om_qa_next"]
        assert next(d for d in advanced["discussions"] if d["chat_id"] == existing)["revision"] == revision
        use("ignore", id, revision, error="HTTP 409")
        assert use("discuss", id, updated, existing)["chat_id"] == existing
        assert len([line for line in assessor_log.read_text().splitlines() if "om_qa_anchor" in line]) == 2
        print("PASS direct @ filter, topic aggregation, later non-mention reply, resolved suggestion, ignore/restore semantics and no Runner use", flush=True)

        if args.screenshots:
            subprocess.run(["node", WEB / "tests/feishu-suggestions.mjs"], env={
                **env, "TEND_SCREENSHOTS": str(args.screenshots), "TEND_FEISHU_CARD": id,
            }, check=True, timeout=80)

        consult = cli("chat", "send", existing, "仅记录用户补充：回滚阈值保留待确认，不实施。")["message_id"]
        wait_for(lambda: cli("chat", "read", existing)["messages"],
                 lambda rows: any(r["author"] == "discussion" and r["reply_to"] == consult for r in rows))
        assert status(id, updated) is None
        visible_before = cli("discussion", "list")["discussions"]
        with lose_first_response(address) as (proxy, posts):
            queued = use("handoff", id, updated, upstream=proxy)
            assert len(posts) == 2 and posts[0][0] == posts[1][0]
            assert queued["operation_id"] == posts[0][2]["operation_id"]
        direct = wait_for(lambda: status(id, updated), lambda r: r and r["request"]["status"] == "sent")
        assert use("handoff", id, updated) == direct
        assert cli("discussion", "list")["discussions"] == visible_before
        sent = direct["request"]["message_id"]
        messages = wait_for(lambda: cli("chat", "read", "main")["messages"],
                            lambda rows: any(m["id"] == sent and m["router_state"] == "completed" for m in rows))
        [direct_message] = [m for m in messages if m["id"] == sent]
        assert "不是新的实施授权" in direct_message["text"]
        assert "applink.feishu.cn" in direct_message["text"] and "测试范围" in direct_message["text"]
        assert "用户补充" not in direct_message["text"]
        assert sql(db, "SELECT count(*) FROM router_dispatches") == [(0,)]

        via = cli("discussion", "handoff-now", existing)["operation_id"]
        received = wait_for(lambda: cli("discussion", "handoff-status", existing, via),
                            lambda r: r["request"]["status"] == "sent")
        via_message_id = received["request"]["message_id"]
        via_message = next(m for m in cli("chat", "read", "main")["messages"] if m["id"] == via_message_id)
        assert "用户补充" in via_message["text"] and "不实施" in via_message["text"]
        assert use("read", id)["handoffs"][0]["handoff"] == direct
        assert sql(db, "SELECT count(*) FROM router_dispatches") == [(0,)]
        print("PASS CLI and Web reference path, immutable revision, lost response, original Main Chat message, and #117 source authorization", flush=True)


if __name__ == "__main__":
    main()
