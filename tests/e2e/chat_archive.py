"""Verify the v54/v55 -> v56 ordinary Chat migration and the public read-only boundary."""

import argparse
from contextlib import closing
from http.client import HTTPConnection
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile

from chat import server, wait_for

WEB = Path(__file__).resolve().parents[2] / "web"
ARCHIVED = ["legacy-a", "legacy-b", "legacy-c"]
OLD_REQUEST = "请设计任务摘要导出，等我批准对应版本后再实现。"
PENDING_REPORT = "已按原会话送达待投递的报告。"
ARTIFACTS = {
    "old-file": b"# Historical example\n",
    "pending-file": b"# Pending example\n",
}


def seed_pre_archive(root, version):
    # Recreate the requested pre-archive layout in this isolated database.
    with closing(sqlite3.connect(root / "server.sqlite")) as db, db:
        db.execute("PRAGMA foreign_keys = ON")
        db.execute("DROP TABLE IF EXISTS archived_chats")
        if version == 54:
            db.execute("DROP TABLE issue_title_changes")
        db.execute(f"PRAGMA user_version = {version}")
        for id, chat, text, op in (
            ("main-source", "main", "Default history.", "main-history"),
            ("original", "legacy-a", OLD_REQUEST, "legacy-operation"),
            ("queued", "legacy-c", "Hold this turn.", "pending-receipt"),
            ("discussion-source", "saved-discussion", "Registered history.", "discussion-history"),
        ):
            seq = db.execute("INSERT INTO chat_messages(id,chat_id,text,operation_id) VALUES(?,?,?,?)",
                             (id, chat, text, op)).lastrowid
            db.execute("INSERT INTO router_inputs(chat_id,message_seq,outcome) VALUES(?,?,?)",
                       (chat, seq, None if id == "queued" else "completed"))
        # Include a Chat with only a legacy reply to prove classification is
        # based on the stored Chat identity, not on a particular message kind.
        db.execute("INSERT INTO chat_messages(id,chat_id,text) VALUES ('other-original','legacy-b','旧会话中的历史消息')")
        db.execute("INSERT INTO chat_messages(id,chat_id,text,reply_to) VALUES "
                   "('old-reply','legacy-a','原有交付与附件。','original')")
        for id, name, archived in (
            ("saved-discussion", "正式讨论", 0),
            ("filed-discussion", "已归档的正式讨论", 1),
        ):
            db.execute("INSERT INTO discussions(chat_id,operation_id,creation,title,archived) VALUES(?,?,?,?,?)",
                       (id, f"create-{id}", "{}", name, archived))
        for issue, source in (("legacy-issue", "original"), ("issue-only", "main-source")):
            db.execute("INSERT INTO issues(id,source_seq,title,objective,state) "
                       "SELECT ?,seq,'示例 Issue','整理历史交付','closed' FROM chat_messages WHERE id=?",
                       (issue, source))
            db.execute("INSERT INTO issue_messages(id,issue_id,kind,author,text,source_message_id) "
                       "VALUES(?,?,'dispatch','router','原派发',?)", (f"{issue}-dispatch", issue, source))
        db.execute("INSERT INTO tasks(id,issue_id,category,status,repo_ids,required_capabilities) "
                   "VALUES('legacy-task','legacy-issue','implementation','completed','[]','[]')")
        db.execute("INSERT INTO runners(id,host_id,profile,last_seen) VALUES('fixture-runner','fixture-host','{}',0)")
        for id, content in ARTIFACTS.items():
            db.execute("INSERT INTO runner_operations(id,runner_id,operation) VALUES(?,'fixture-runner','{}')", (id,))
            db.execute("INSERT INTO artifacts(id,issue_id,task_id,runner_id,name,bytes,published,available) "
                       "VALUES(?,'legacy-issue','legacy-task','fixture-runner',?,?,1,1)",
                       (id, f"{id}.md", len(content)))
            (root / "server.artifacts" / id).write_bytes(content)
        db.execute("INSERT INTO chat_artifacts VALUES('old-reply','old-file')")
        db.execute("INSERT INTO runner_operations(id,runner_id,operation) VALUES('pending-report','fixture-runner','{}')")
        db.execute("INSERT INTO issue_reports VALUES('pending-report','legacy-issue',?)", (PENDING_REPORT,))
        db.execute("INSERT INTO report_artifacts VALUES('pending-report','pending-file')")
        db.execute("INSERT INTO router_inputs(chat_id,message_seq,report_id) "
                   "SELECT chat_id,seq,'pending-report' FROM chat_messages WHERE id='original'")
        if version >= 55:
            db.execute("INSERT INTO issue_title_changes(issue_id,operation_id,old_title,new_title) "
                       "VALUES('legacy-issue','legacy-rename','原示例 Issue','示例 Issue')")
        return db.execute("SELECT * FROM chat_messages ORDER BY seq").fetchall()


def exercise(root, args):
    env = {**os.environ, "CODEX_HOME": str(root / "fixture-codex"), "TEND_TOKEN": "chat-archive-fixture"}
    with server(args.server, root, env):
        pass
    original_rows = seed_pre_archive(root, args.from_schema)

    def call(path, body=None, expect=200):
        with closing(HTTPConnection(address, timeout=5)) as conn:
            conn.request("GET" if body is None else "POST", f"/api/v1/{path}",
                         None if body is None else json.dumps(body),
                         {"Authorization": f"Bearer {env['TEND_TOKEN']}", "Content-Type": "application/json"})
            result = conn.getresponse()
            data = result.read()
            assert (result.status, result.getheader("Location")) == (expect, None), (result.status, data)
            return data if path.startswith("artifacts/") else json.loads(data)

    def cli(*words, error=None):
        output = subprocess.run([args.cli, *words], text=True, capture_output=True, timeout=10,
                                env={**env, "TEND_URL": f"http://{address}"})
        if error:
            assert output.returncode and error in output.stderr, output
            return
        assert output.returncode == 0, output
        return json.loads(output.stdout)

    def read(chat, expect=200):
        return call(f"chats/{chat}/messages", expect=expect)

    def tables():
        with closing(sqlite3.connect(root / "server.sqlite")) as db:
            return [db.execute(f"SELECT * FROM {table} ORDER BY rowid").fetchall()
                    for table in ("archived_chats", "chat_messages", "router_inputs", "router_threads",
                                  "discussions", "issue_messages", "chat_artifacts")]

    with server(args.server, root, env, web_dir=WEB / "dist" if args.web else None) as address:
        archive_list = {"chat_ids": ARCHIVED}
        assert call("chats/archived") == cli("chat", "list", "--archived") == archive_list
        assert all(read(id)["chat_kind"] == "archived" for id in ARCHIVED)
        assert cli("chat", "read", "legacy-a")["chat_kind"] == "archived"
        assert read("main")["chat_kind"] == "main"
        assert read("saved-discussion")["chat_kind"] == read("filed-discussion")["chat_kind"] == "discussion"
        assert read("issue-only", 404)["error"].startswith("Chat not found")
        assert call("issues/issue-only/messages")["messages"][0]["source_message_id"] == "main-source"

        page = wait_for(lambda: read("legacy-a")["messages"],
                        lambda rows: any(row["text"] == PENDING_REPORT for row in rows))
        original, old_reply, delivered = page
        assert original["id"] == "original" and original["text"] == OLD_REQUEST
        assert original["issue_ids"] == ["legacy-issue"]
        assert old_reply["id"] == "old-reply" and old_reply["reply_to"] == delivered["reply_to"] == original["id"]
        assert [file["id"] for file in old_reply["artifacts"]] == ["old-file"]
        assert [file["id"] for file in delivered["artifacts"]] == ["pending-file"]
        assert all(call(f"artifacts/{id}") == content for id, content in ARTIFACTS.items())
        issue = call("issues/legacy-issue/overview")
        assert (issue["chat_id"], issue["source_message_id"]) == ("legacy-a", "original")
        wait_for(lambda: read("legacy-c")["messages"],
                 lambda rows: rows[0]["router_state"] == "awaiting_turn")
        assert cli("chat", "send", "legacy-c", "Hold this turn.", "pending-receipt") == {"message_id": "queued"}
        assert len(read("legacy-c")["messages"]) == 1
        (root / "app.release").touch()
        queued = wait_for(lambda: read("legacy-c")["messages"], lambda rows: len(rows) == 2)
        assert queued[-1]["reply_to"] == "queued" and queued[0]["router_state"] == "completed"
        snapshot = tables()

        for target, status, error in (
            ("unknown", 404, "Chat not found"), ("legacy-a", 403, "archived and read-only"),
        ):
            if status == 404:
                assert error in read(target, status)["error"]
                cli("chat", "read", target, error="HTTP 404")
            for reply in ({}, {"reply_to": "original"}):
                rejected = call(f"chats/{target}/messages", {
                    "text": "Should not appear", "operation_id": f"send-{target}-{len(reply)}", **reply,
                }, expect=status)
                assert error in rejected["error"], rejected
            cli("chat", "send", target, "CLI should not appear", f"cli-{target}", error=error)
        assert cli("chat", "send", "legacy-a", OLD_REQUEST, "legacy-operation") == {"message_id": "original"}
        assert cli("chat", "send", "legacy-c", "Hold this turn.", "pending-receipt") == {"message_id": "queued"}
        cli("chat", "send", "legacy-a", "changed request", "legacy-operation", error="archived and read-only")
        assert tables() == snapshot, "read, rejected writes or exact receipt replay mutated Chat state"

        if args.web:
            subprocess.run(["node", WEB / "tests/chat-archive.mjs"], check=True, timeout=40,
                           env={**env, "TEND_URL": f"http://{address}",
                                "TEND_ARCHIVE_ISSUE": str(original["issue_numbers"][0])})
            assert tables() == snapshot, "read-only Web visit changed the ledger"

        main = cli("chat", "send", "main", "Hello, Tend.", "default-send")
        wait_for(lambda: read("main")["messages"],
                 lambda rows: rows[-1].get("reply_to") == main["message_id"])
        # Archived Discussions retain their separate, list-only archive contract.
        for id in ("saved-discussion", "filed-discussion"):
            receipt = cli("chat", "send", id, "A registered Discussion still accepts input.")
            assert any(row["id"] == receipt["message_id"] for row in read(id)["messages"])

    with server(args.server, root, env) as address:
        assert call("chats/archived") == archive_list
        assert read("legacy-a")["messages"] == page and read("legacy-c")["messages"] == queued
        with closing(sqlite3.connect(root / "server.sqlite")) as db:
            # An unrelated later migration may advance the schema after
            # the archive migration has run.
            assert db.execute("PRAGMA user_version").fetchone()[0] >= 56
            assert db.execute("SELECT * FROM chat_messages ORDER BY seq LIMIT ?",
                              (len(original_rows),)).fetchall() == original_rows
            assert db.execute("PRAGMA foreign_key_check").fetchall() == []
            assert db.execute("SELECT operation_id FROM issue_title_changes").fetchall() == (
                [("legacy-rename",)] if args.from_schema == 55 else [])
            assert db.execute("SELECT message_id FROM issue_report_publications WHERE report_id='pending-report'").fetchall() == [
                (delivered["id"],)]
            assert db.execute("SELECT chat_id, outcome FROM router_inputs WHERE report_id='pending-report'").fetchall() == [
                ("legacy-a", "completed")]
    # Also force the migration body to re-enter with its rows already present.
    # A normal v56 restart above exercises the version gate separately.
    snapshot = tables()
    with closing(sqlite3.connect(root / "server.sqlite")) as db:
        db.execute("PRAGMA user_version = 55")
    with server(args.server, root, env) as address:
        assert call("chats/archived") == archive_list
        assert tables() == snapshot


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server", type=lambda value: Path(value).resolve())
    parser.add_argument("cli", type=lambda value: Path(value).resolve())
    parser.add_argument("--web", action="store_true")
    parser.add_argument("--from-schema", type=int, choices=(54, 55), default=55)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-chat-archive-") as temporary:
        exercise(Path(temporary), args)
    print("PASS migrated three ordinary Chats, preserved original history and pending delivery, and rejected new sends")


if __name__ == "__main__":
    main()
