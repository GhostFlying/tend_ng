"""Verify Feishu scans, isolation, recovery and explicit transfers with local fixtures."""
import argparse
from datetime import datetime
import errno
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
    env = {**os.environ, "CODEX_HOME": str(root / "fixture-codex"), "TEND_TOKEN": "test-only-feishu", "TEND_URL": f"http://{address}"}
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
        "agent": {"id": "FeishuFixture", "home": "./assessor-home", "cwd": "./assessor-cwd", "timeout_seconds": 8},
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
    hold = root / "assessor-home" / "hold-qa-assessment"
    hold.touch()
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

        wait_for((root / "assessor-home" / "qa-assessment-has-input").exists, bool)
        before = card("om_qa_anchor")["evidence"]
        assert before["assessment"] is None
        assert use("ignore", before["id"], before["revision"])["ignored"]
        existing = cli("discussion", "create", "已有的验收讨论")["chat_id"]
        with lose_first_response(address) as (proxy, posts):
            assert use("discuss", before["id"], before["revision"], existing, upstream=proxy)["chat_id"] == existing
            assert len(posts) == 2 and posts[0][0] == posts[1][0]
        hold.unlink()
        qa = judged("om_qa_anchor")
        assert qa["evidence"]["revision"] > before["revision"] and qa["ignored"]
        use("handoff", before["id"], before["revision"], error="HTTP 409")
        documents = card("om_docs_anchor")["evidence"]["id"]
        resolved = judged("om_docs_anchor")
        release = judged("om_release_anchor")
        rounded_anchor = judged("om_other_topic")
        assert rounded_anchor["evidence"]["anchor"]["created_at"] % 60 == 0
        assert [m["id"] for m in rounded_anchor["evidence"]["replies"]] == ["om_other_later"]
        assert rounded_anchor["evidence"]["context_status"] == "partial"
        assert [m["id"] for m in qa["evidence"]["replies"]] == ["om_qa_reply"]
        assert qa["evidence"]["anchor"]["created_at"] % 60 == 30
        assert qa["evidence"]["replies"][0]["created_at"] == qa["evidence"]["anchor"]["created_at"] // 60 * 60
        assert qa["evidence"]["context_status"] == "partial"
        assert resolved["evidence"]["anchor"]["created_at"] % 60 == 30
        assert resolved["evidence"]["replies"][0]["created_at"] == resolved["evidence"]["anchor"]["created_at"] // 60 * 60
        assert resolved["evidence"]["context_status"] == "ready"
        assert resolved["evidence"]["assessment"]["disposition"] == "resolved"
        assert [m["id"] for m in release["evidence"]["mentions"]] == ["om_release_anchor", "om_release_update"]
        id, revision = qa["evidence"]["id"], qa["evidence"]["revision"]
        assert use("ignore", id, revision)["ignored"]
        assert use("read", id)["ignored"]
        one_thread = use("discuss", id, revision)["chat_id"]
        assert use("discuss", id, revision)["chat_id"] == one_thread
        assert use("discuss", id, revision, existing)["chat_id"] == existing
        assert use("discuss", id, before["revision"], existing)["chat_id"] == existing
        assert next(a for a in use("read", id)["discussions"] if a["chat_id"] == existing)["revision"] == revision
        saved = sql(db, "SELECT revision,evidence FROM feishu_discussion_snapshots WHERE chat_id=? ORDER BY revision", existing)
        assert [n for n,_ in saved] == [before["revision"], revision]
        assert json.loads(saved[0][1])["assessment"] is None and json.loads(saved[1][1])["assessment"]
        assert not cli("chat", "read", existing)["messages"]
        assert not cli("chat", "read", "main")["messages"]
        assert {a["chat_id"] for a in use("read", id)["discussions"]} == {one_thread, existing}

        # Simulate the passage of the context poll interval. A new reply is
        # deliberately NOT an @mention and must still advance the same card.
        set_state("recover", progress=True, provisional_only=True)
        set_checkpoints("UPDATE feishu_suggestions SET checked_at=0 WHERE id=?", id)
        use("sync")
        provisional = judged("om_release_final")
        assert [m["id"] for m in provisional["evidence"]["replies"]] == ["om_release_deleted"]
        provisional_id, provisional_rev = provisional["evidence"]["id"], provisional["evidence"]["revision"]
        prior_discussion = use("discuss", provisional_id, provisional_rev)["chat_id"]
        use("handoff", provisional_id, provisional_rev)
        prior_handoff = wait_for(lambda: status(provisional_id, provisional_rev),
                                 lambda r: r and r["request"]["status"] == "sent")
        release_id = release["evidence"]["id"]
        set_state("recover", progress=True, deep_context=True)
        set_checkpoints("UPDATE feishu_suggestions SET checked_at=0 WHERE id IN (?,?)", release_id, documents)
        use("sync")
        advanced = wait_for(lambda: use("read", id),
                            lambda c: c["evidence"]["revision"] > revision and c["evidence"]["assessment"])
        updated = advanced["evidence"]["revision"]
        assert advanced["ignored"] is False and "Android 12" in advanced["evidence"]["assessment"]["overview"]
        assert [m["id"] for m in advanced["evidence"]["replies"]] == ["om_qa_reply", "om_qa_next"]
        assert next(d for d in advanced["discussions"] if d["chat_id"] == existing)["revision"] == revision
        use("ignore", id, revision, error="HTTP 409")
        assert use("discuss", id, updated, existing)["chat_id"] == existing
        assert use("discuss", id, revision, existing)["chat_id"] == existing
        assert next(a for a in use("read", id)["discussions"] if a["chat_id"] == existing)["revision"] == updated
        nested = wait_for(lambda: use("read", release_id), lambda c: len(c["evidence"]["mentions"]) == 3)
        assert [m["id"] for m in nested["evidence"]["mentions"]] == ["om_release_anchor", "om_release_update", "om_release_final"]
        assert [m["id"] for m in nested["evidence"]["replies"]] == ["om_release_parent"]
        assert len(feed()["suggestions"]) == 4
        assert use("read", provisional_id)["evidence"]["id"] == release_id
        assert nested["aliases"] == [provisional_id]
        assert next(d for d in nested["discussions"] if d["suggestion_id"] == provisional_id)["chat_id"] == prior_discussion
        assert next(h for h in nested["handoffs"] if h["suggestion_id"] == provisional_id)["handoff"] == prior_handoff
        assert use("discuss", provisional_id, provisional_rev)["chat_id"] == prior_discussion
        assert status(provisional_id, provisional_rev) == prior_handoff
        assert use("handoff", provisional_id, provisional_rev) == prior_handoff
        assert cli("discussion", "read", prior_discussion)["references"][0]["suggestion_id"] == provisional_id
        originals = sql(db, "SELECT evidence FROM feishu_discussion_snapshots WHERE suggestion_id=? ORDER BY chat_id", provisional_id)
        assert len(originals) == 2 and all("om_release_deleted" in evidence for evidence, in originals)
        print("PASS deletion read during fold is absent from current advice but both selected old snapshots remain unchanged", flush=True)
        assert sql(db, "SELECT suggestion_id FROM feishu_mentions WHERE message_id='om_release_final'") == [(release_id,)]
        print("PASS minute-precision chat and thread evidence; a late-discovered non-mention parent folds its @ child into the original card without rewriting receipts", flush=True)

        # The first poll reached its budget. The next scan must reuse the
        # original window and actually request its fifth provider page.
        wait_for(lambda: use("read", documents), lambda c: c["evidence"]["context_status"] == "partial")
        first_window = sql(db, "SELECT context_cursor FROM feishu_suggestions WHERE id=?", documents)[0][0]
        cursor = json.loads(first_window)
        assert cursor["token"] == "doc-page-5"
        use("sync")
        continued = wait_for(lambda: use("read", documents), lambda c: c["evidence"]["context_status"] == "ready"
                             and c["evidence"]["assessment"] and len(c["evidence"]["replies"]) == 2)
        assert [m["id"] for m in continued["evidence"]["replies"]] == ["om_docs_reply", "om_docs_followup"]
        assert continued["evidence"]["assessment"]["disposition"] == "resolved"
        latest_page = next(c for c in list_calls() if "doc-page-5" in c)
        assert datetime.fromisoformat(latest_page[latest_page.index("--end") + 1]).timestamp() == cursor["through"]
        assert len([line for line in assessor_log.read_text().splitlines() if "om_qa_anchor" in line]) == 2
        print("PASS direct @ filter, topic aggregation, later non-mention reply, resolved suggestion, ignore/restore semantics and no Runner use", flush=True)

        if args.screenshots:
            subprocess.run(["node", WEB / "tests/feishu-suggestions.mjs"], env={
                **env, "TEND_SCREENSHOTS": str(args.screenshots), "TEND_FEISHU_CARD": id,
                "TEND_FEISHU_ALIAS": provisional_id, "TEND_FEISHU_PRIOR_CHAT": prior_discussion,
                "TEND_FEISHU_PRIOR_MESSAGE": prior_handoff["request"]["message_id"],
                "TEND_FEISHU_CACHED_DETAIL": json.dumps(provisional, ensure_ascii=False),
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

        # Consume a rich QA prompt with a child that acknowledges thread/start
        # then stops reading its 4 KiB stdin pipe. Queue another existing
        # material while it is stuck and verify that job actually finishes.
        path = root / "assessor-home"
        (path / "stall-next").touch()
        set_state("recover", progress=True, deep_context=True, big_payload=True)
        set_checkpoints("UPDATE feishu_suggestions SET checked_at=0 WHERE id=?", id)
        use("sync")
        wait_for((path / "stalled").exists, bool)
        pid, capacity = map(int, (path / "stalled").read_text().split())
        oversized = use("read", id)["evidence"]
        assert len(json.dumps(oversized, ensure_ascii=False).encode()) > capacity * 4
        completions = assessor_log.read_text().count("om_release_anchor")
        set_checkpoints("UPDATE feishu_suggestions SET assessment_state='pending',assessed_at=0 WHERE id=?", release_id)
        wait_for(lambda: use("read", id), lambda c: c["assessment_status"] == "failed")
        wait_for(lambda: assessor_log.read_text().count("om_release_anchor"), lambda n: n > completions)
        try:
            os.kill(pid, 0)
        except OSError as exc:
            assert exc.errno == errno.ESRCH, exc
        else:
            raise AssertionError(f"expired assessor {pid} was not stopped and reaped")
        print("PASS a subprocess that stops reading a full pipe expires and the next assessment completes", flush=True)

        release_result = use("read", release_id)
        use("ignore", release_id, release_result["evidence"]["revision"])

    # Simulate elapsed time while Tend was stopped. Keep an active card with
    # one expired origin as well as a fully expired suggestion whose explicit
    # snapshots survive. The provider is now persistently failing identity.
    expired = at - 8 * 86400
    set_checkpoints("UPDATE feishu_suggestions SET first_at=?,last_at=? WHERE id=?", expired, expired, id)
    with sqlite3.connect(db) as conn:
        raw = conn.execute("SELECT evidence FROM feishu_suggestions WHERE id=?", (release_id,)).fetchone()[0]
        evidence = json.loads(raw)
        evidence["anchor"]["created_at"] = expired
        evidence["mentions"][0]["created_at"] = expired
        conn.execute("UPDATE feishu_suggestions SET evidence=?,first_at=? WHERE id=?",
                     (json.dumps(evidence), expired, release_id))
        conn.execute("UPDATE feishu_mentions SET created_at=? WHERE message_id='om_release_anchor'", (expired,))
    set_state("wrong-user")
    with server(args.server, root, env, **options):
        assert wait_scan("failed")["scan"]["gap"] == "identity"
        use("read", id, error="HTTP 404")
        assert sql(db, "SELECT count(*) FROM feishu_suggestions WHERE id=?", id) == [(0,)]
        assert sql(db, "SELECT count(*) FROM feishu_mentions WHERE message_id='om_release_anchor'") == [(0,)]
        active = use("read", release_id)
        assert active["evidence"]["anchor"]["id"] == "om_release_update" and active["ignored"]
        assert status(id, updated) == direct
        assert use("discuss", id, before["revision"], existing)["chat_id"] == existing
        print("PASS provider-independent retention trims active/expired material but preserves explicit old receipts and snapshots", flush=True)


if __name__ == "__main__":
    main()
