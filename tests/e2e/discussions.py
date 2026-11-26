"""Exercise Discussion API, the actual Runner inspection, and the Web end to end."""
import argparse
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import threading
from urllib.error import HTTPError
from urllib.request import Request, urlopen
from uuid import uuid4

from chat import lose_first_response, server, wait_for
from issue_chat import TESTS, WEB, free_port, prepare

DRAFT_TEXT = "从独立讨论转交。"
ROUTER_REPLY = "Router 已收到核对后的转交，先确认下一步。"
FIRST_ANSWER = "可以。先区分要讨论的想法与之后可能交给 Router 的工作。"
INTRO = "Fixture repository: quickstart command."

@contextmanager
def runner_process(command, env, root):
    with (root / "runner.stderr").open("w+") as logs:
        runner = subprocess.Popen(command, env=env, stderr=logs)
        try:
            yield runner
        finally:
            runner.terminate()
            try:
                runner.wait(timeout=5)
            except subprocess.TimeoutExpired:
                runner.kill()
                runner.wait(timeout=5)
            if sys.exc_info()[0] is not None:
                logs.seek(0)
                print(logs.read(), file=sys.stderr)


@contextmanager
def old_inspection():
    class Legacy(BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_error(404)

        def log_message(self, *_):
            pass

    with ThreadingHTTPServer(("127.0.0.1", 0), Legacy) as old:
        worker = threading.Thread(target=old.serve_forever)
        worker.start()
        try:
            yield f"http://127.0.0.1:{old.server_port}"
        finally:
            old.shutdown()
            worker.join()


def setup(root, inspect):
    prepare(root, inspect, INTRO + "\n")
    for name in ("app-home", "native-cwd", "runner-home"):
        (root / name).mkdir()
    fixture = root / "app-discussion"
    fixture.write_text(f"#!{sys.executable}\n" + (TESTS / "fixtures/discussion_app_server.py").read_text())
    fixture.chmod(0o755)
    (root / "app-home/synthetic-key").write_text("configured-discussion-key\n")
    discussion_config = root / "discussion.json"
    discussion_config.write_text(json.dumps({
        "host_id": "demo-host", "program": str(fixture), "codex_home": "app-home",
        "cwd": "native-cwd", "repos": {"example": "example", "outside-only": "does-not-exist"},
        "model_api_keys": {"FIXTURE_API_KEY": "app-home/synthetic-key"},
    }))
    settings = root / "runner.json"
    data = json.loads(settings.read_text())
    data["repos"].append({"id": "outside-only", "path": "missing-checkout"})
    settings.write_text(json.dumps(data))
    github = root / "fixture-gh"
    github.write_text(f"#!{sys.executable}\n" + """import json, sys
args = sys.argv[1:]
assert args[:5] == ['api','--method','GET','--hostname','github.com'], args
if args[5].endswith('/7'):
    print(json.dumps({'number':7,'title':'Synthetic quickstart PR','body':'Synthetic PR body',
                      'state':'open','merged':False,'draft':True,'head':{'sha':'1'*40}}))
else:
    print(json.dumps({'status':'404','message':'Not Found'}))
    sys.exit(1)
""")
    github.chmod(0o755)
    return discussion_config, github


def sql(db, statement, *params):
    with sqlite3.connect(db, timeout=5) as con:
        return con.execute(statement, params).fetchall()


def add_fixture_task(root, based_on, task, namespace, repo, with_mount, existing_key=None, bound_to=None):
    local = root / "runner.sqlite"
    central = root / "server.sqlite"
    assignment, encoded, repositories = sql(local, "SELECT id,contract,repositories FROM received_assignments WHERE task_id=?", based_on)[0]
    contract = json.loads(encoded)
    issue = contract["issue_id"]
    key = existing_key or f"fixture-{task}"
    new_assignment = f"{task}-assignment"
    contract["placement"]["assignment_id"] = new_assignment
    contract["task_id"] = task
    if namespace == "tasks":
        key = task
        contract.pop("parent_issue_id", None)
        contract.pop("workspace_key", None)
    else:
        contract["workspace_key"] = key
    if bound_to:
        contract["workspace_key"] = f"non-author-path-{task}"
        contract["review"] = {"request_id":"fixture-review-request", "author_issue_id":issue,
                              "author_task_id":bound_to, "candidate":"fixture", "scope":"read",
                              "author_workspace":f"workspaces/{key}"}
    encoded = json.dumps(contract)
    workspace = root / "runner.runs" / namespace / key
    if not existing_key:
        workspace.mkdir(parents=True)
    if with_mount and not existing_key:
        target = workspace / "repos" / repo
        target.parent.mkdir()
        subprocess.run(["git", "-C", root / repo, "worktree", "add",
                        "--quiet", "--detach", target, "HEAD"], check=True)
        (workspace / "mounts.json").write_text(json.dumps([{
            "repo_id": repo, "path": f"repos/{repo}", "base": "fixture",
        }]))
    stamp = workspace.stat()
    proof = {"path": str(workspace), "device": stamp.st_dev, "inode": stamp.st_ino}
    sql(local, """INSERT INTO received_assignments
        (id,task_id,contract,repositories,decision,acknowledged,execution_state)
        SELECT ?,?,?,?,decision,1,'finished' FROM received_assignments WHERE id=?""",
        new_assignment, task, encoded, repositories, assignment)
    sql(local, """INSERT INTO local_runs(id,assignment_id,state,turn_id,workspace_stamp)
                  VALUES(?,?,'finished','fixture',?)""", f"{task}-run", new_assignment, json.dumps(proof))
    sql(central, """INSERT INTO tasks(id,issue_id,category,status,repo_ids,required_capabilities)
        SELECT ?,issue_id,category,'completed',repo_ids,required_capabilities FROM tasks WHERE id=?""", task, based_on)
    sql(central, """INSERT INTO runner_assignments(id,task_id,runner_id,contract,state,expires_at)
        SELECT ?,?,runner_id,?,'accepted',0 FROM runner_assignments WHERE id=?""",
        new_assignment, task, encoded, assignment)
    sql(central, "UPDATE tasks SET current_assignment=? WHERE id=?", new_assignment, task)
    return issue, workspace


def persisted_discussion(detail):
    # Reference availability is projected live and can change when the fixture Runner stops.
    return {**detail, "references": [
        {key: value for key, value in row.items() if key != "status"}
        for row in detail["references"]
    ]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "cli", "runner"):
        parser.add_argument(name, type=lambda value: Path(value).resolve())
    parser.add_argument("--screenshots", type=lambda value: Path(value).resolve())
    parser.add_argument("--node", default="node")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-discussions-") as directory:
        root = Path(directory).resolve()
        address, inspect = free_port(), free_port()
        config, github = setup(root, inspect)
        token = "tend-discussion-fixture"
        env = {**os.environ, "TEND_TOKEN": token, "TEND_URL": f"http://{address}",
               "CODEX_HOME": str(root / "runner-home"), "OPENAI_API_KEY": "must-not-inherit"}
        command = [args.runner, "--config", root / "runner.json", "--db", root / "runner.sqlite",
                   "--app-server", sys.executable, TESTS / "fixtures/coordination_app_server.py", root, "issue-chat"]
        db = root / "server.sqlite"
        native = root / "app-home/fixture.sqlite"

        def cli(*words, at=address, error=None):
            response = subprocess.run([args.cli, *words], env={**env, "TEND_URL": f"http://{at}"},
                                      capture_output=True, text=True, timeout=20)
            if error:
                assert response.returncode and error in response.stderr, response.stderr
                return None
            assert not response.returncode, response.stderr
            return json.loads(response.stdout)

        def api(path, method="GET", body=None, status=200):
            req = Request(f"http://{address}/api/v1/{path}", method=method,
                          data=None if body is None else json.dumps(body).encode(),
                          headers={"Authorization":f"Bearer {token}","Content-Type":"application/json"})
            try:
                with urlopen(req, timeout=12) as r:
                    assert r.status == status, (path, r.status, status)
                    return json.load(r)
            except HTTPError as error:
                assert error.code == status, (path, error.code, status, error.read())
                return json.load(error)

        def messages(chat):
            return api(f"chats/{chat}/messages")["messages"]

        def say(chat, text, response=None, operation=None, error=None):
            words = ("chat", "send", chat, text, operation) if operation else ("chat", "send", chat, text)
            message_id = cli(*words)["message_id"]
            rows = wait_for(lambda: messages(chat), lambda rows: any(
                row["id"] == message_id and row["router_state"] in ("completed", "failed") for row in rows))
            user = next(row for row in rows if row["id"] == message_id)
            if error:
                assert user["router_state"] == "failed" and error in user["router_error"], user
                return user
            assert user["router_state"] == "completed", user
            if response:
                assert any(row["reply_to"] == message_id and row["text"] == response for row in rows), rows
            return user

        def assert_scope(text, thread, *roots):
            [(actual_thread, encoded)] = sql(native, "SELECT thread_id, roots FROM turns WHERE text=?", text)
            assert actual_thread == thread
            assert json.loads(encoded) == sorted({str(path) for path in roots}), encoded

        def probe(chat, thread, expected, *roots):
            text = "Scope probe: " + json.dumps({str(path): value for path, value in expected.items()})
            say(chat, text, "Fixture scope matched.")
            assert_scope(text, thread, *roots)

        def worktree_roots(checkout):
            aux = [Path(subprocess.check_output(["git", "-C", checkout, "rev-parse",
                        "--path-format=absolute", flag], text=True).strip()).resolve()
                   for flag in ("--git-dir", "--git-common-dir")]
            return [checkout, *aux]

        def attach(chat, **source):
            return api(f"discussions/{chat}/references", "POST", source)

        def status(chat, id):
            return next(ref["status"] for ref in api(f"discussions/{chat}")["references"] if ref["id"] == id)

        options = dict(listen=address, discussion_config=config, github_cli=github,
                       web_dir=WEB / "dist" if args.screenshots else None)
        with server(args.server, root, env, **options):
            created = cli("discussion", "create", "隔离讨论", "--operation", "create-once")
            chat = created["chat_id"]
            assert cli("discussion", "create", "隔离讨论", "--operation", "create-once") == created
            cli("discussion", "create", "changed", "--operation", "create-once", error="HTTP 409")
            assert not messages(chat)
            first_text = "先聊聊演示项目的目标。"
            with lose_first_response(address) as (proxy, posted):
                first = cli("chat", "send", chat, first_text, "initial-discussion-message", at=proxy)
                assert first == posted[0][2]
            initial = say(chat, first_text, FIRST_ANSWER, operation="initial-discussion-message")
            assert initial["id"] == first["message_id"] and len(messages(chat)) == 2
            cli("pr", "require", initial["id"], json.dumps({"event":"pull","state":"merged",
                "url":"https://github.com/demo/quickstart/pull/7"}), "discussion-cannot-watch", error="HTTP 404")
            assert sql(db,"SELECT count(*) FROM issues") == sql(db,"SELECT count(*) FROM tasks") == [(0,)]
            assert sql(db,"SELECT count(*) FROM router_dispatches") == [(0,)]
            assert messages(chat)[-1]["author"] == "discussion"
            say(chat, "继续这个话题。", FIRST_ANSWER)
            [(original, params)] = sql(native, "SELECT id, params FROM threads")
            assert json.loads(params)["runtimeWorkspaceRoots"] == []
            assert sql(native, "SELECT id, roots FROM resumes") == [(original, "[]")]
            assert_scope(first_text, original)
            assert_scope("继续这个话题。", original)
            # Simulate a binding left by the first release, before roots were removed from the key.
            sql(db, "UPDATE router_threads SET model=? WHERE chat_id=?", "f" * 64, chat)
            cli("discussion", "rename", chat, "可恢复的讨论")
            unnamed = cli("discussion", "create", "--operation", "legacy-unnamed")["chat_id"]
            explicitly_named = cli("discussion", "create", "新讨论")["chat_id"]
            renamed_to_default = cli("discussion", "create")["chat_id"]
            cli("discussion", "rename", renamed_to_default, "新讨论")
            assert cli("discussion", "read", unnamed)["title"] == "未命名讨论"

        # Model a database stored by v32, including its actual generated title.
        sql(db, "DROP INDEX discussions_by_activity")
        sql(db, "ALTER TABLE discussions DROP COLUMN archived")
        sql(db, "CREATE INDEX discussions_by_activity ON discussions(updated_at DESC, chat_id)")
        sql(db, "UPDATE discussions SET title='新讨论' WHERE chat_id=?", unnamed)
        sql(db, "UPDATE discussions SET updated_at=created_at+1 WHERE chat_id=?", renamed_to_default)
        sql(db, "PRAGMA user_version=32")

        with server(args.server, root, env, **options), runner_process(command, env, root):
            assert cli("discussion", "read", chat)["title"] == "可恢复的讨论"
            assert cli("discussion", "read", unnamed)["title"] == "未命名讨论"
            assert cli("discussion", "read", explicitly_named)["title"] == "新讨论"
            assert cli("discussion", "read", renamed_to_default)["title"] == "新讨论"
            assert cli("discussion", "create", "--operation", "legacy-unnamed")["chat_id"] == unnamed
            assert not cli("discussion", "list", "--archived")["discussions"]
            assert len(messages(chat)) == 4
            say(chat,"Recover discussion","恢复的是原讨论的同一条答复。")
            attempted = sql(db, """SELECT a.number,i.outcome FROM router_attempts a JOIN router_inputs i ON i.seq=a.seq
                                    JOIN chat_messages m ON m.seq=i.message_seq WHERE m.text='Recover discussion'""")
            assert attempted == [(1,"completed")], attempted
            assert_scope("Recover discussion", original)
            say(chat, "Retry discussion", FIRST_ANSWER)
            assert_scope("Retry discussion", original)
            say(chat, "Exhaust discussion retries", error="after two attempts")
            attempted = sql(db, """SELECT m.text,a.number,a.thread_id FROM router_attempts a
                        JOIN router_inputs i ON i.seq=a.seq JOIN chat_messages m ON m.seq=i.message_seq
                        WHERE m.text IN ('Retry discussion','Exhaust discussion retries')
                        ORDER BY m.seq,a.number""")
            assert attempted == [(case, n, original) for case in (
                "Retry discussion", "Exhaust discussion retries") for n in (1, 2)], attempted
            assert sql(db, "SELECT thread_id FROM router_threads WHERE chat_id=?", chat) == [(original,)]

            # Recreate the first release's exhausted retry: attempt 2 got a new ID
            # but no native rollout. That release removed the binding, not the attempts.
            missing_legacy_retry = str(uuid4())
            sql(db, """UPDATE router_attempts SET thread_id=? WHERE number=2 AND seq=(
                       SELECT i.seq FROM router_inputs i JOIN chat_messages m ON m.seq=i.message_seq
                       WHERE i.chat_id=? AND m.text='Exhaust discussion retries')""",
                missing_legacy_retry, chat)
            sql(db, "DELETE FROM router_threads WHERE chat_id=? AND thread_id=?", chat, original)
            assert sql(db, "SELECT thread_id FROM router_threads WHERE chat_id=?", chat) == []
            revived = "Continue after legacy binding loss"
            say(chat, revived, FIRST_ANSWER)
            assert_scope(revived, original)
            assert sql(native, "SELECT id, found FROM resumes ORDER BY rowid DESC LIMIT 2") == [
                (original, 1), (missing_legacy_retry, 0)]
            assert sql(native, "SELECT id FROM threads") == [(original,)]
            assert sql(db, "SELECT thread_id, model FROM router_threads WHERE chat_id=?", chat) == [(original, None)]
            [(context,)] = sql(native, "SELECT context FROM turns WHERE text=?", revived)
            assert "先聊聊演示项目的目标" not in context, context

            parent = say("main", "请整理示例仓库的快速上手指南。")
            parent_id, = parent["issue_ids"]
            parent_number, = parent["issue_numbers"]
            child = wait_for(lambda: cli("issue", "messages", parent_id)["messages"],
                             lambda rows: any(row["text"]=="初稿已整理，接下来核对首次运行步骤。" for row in rows))
            child_id = next(row["issue_id"] for row in child if row["text"].startswith("初稿已整理"))
            linked = say("other", "Dispatch from Chat.")
            linked_id, = linked["issue_ids"]
            linked_number, = linked["issue_numbers"]
            for issue, name in ((linked_id,"readable-delivery"),(parent_id,"unattached-delivery")):
                task, = sql(db, "SELECT id FROM tasks WHERE issue_id=?", issue)[0]
                body = "immutable fixture deliverable\n"
                sql(db, "INSERT INTO runner_operations(id,runner_id,operation) VALUES(?,'demo-runner','{}')", name)
                sql(db, """INSERT INTO artifacts(id,issue_id,task_id,runner_id,name,bytes,available,published)
                            VALUES(?,?,?,'demo-runner','fixture.txt',?,1,1)""", name, issue, task, len(body))
                (root / "server.artifacts" / name).write_text(body)

            repo = attach(chat,kind="repo",repo_id="example")
            assert repo["status"] == "available" and repo["href"]
            issue = attach(chat,kind="issue",issue_id=f"#{linked_number}")
            assert issue["issue_id"] == linked_id and issue["href"] == f"/issues/{linked_number}"
            pull = attach(chat,kind="pull",url="https://github.com/Demo/Quickstart/pull/7?plain=1#diff")
            assert pull["href"] == "https://github.com/demo/quickstart/pull/7" and pull["status"] == "available"
            assert attach(chat,kind="pull",url=pull["href"])["id"] == pull["id"]
            assert attach(chat,kind="pull",url="https://github.com/demo/quickstart/pull/8")["status"] == "not_accessible"
            assert attach(chat,kind="pull",url="https://code.byted.org/synthetic/repo/merge_requests/9")["status"] == "provider_not_configured"
            # Discoverability and attachment survive the absence of a Server checkout.
            sql(db,"UPDATE runners SET profile=json_insert(profile,'$.repos[#]','remote-only') WHERE id='demo-runner'")
            assert attach(chat,kind="repo",repo_id="remote-only")["status"] == "repo_not_readable_here"
            checkout = root / "example"
            attached = f"Read attached: {linked_id} {parent_id} readable-delivery unattached-delivery"
            say(chat, attached, INTRO)
            assert_scope(attached, original, checkout)
            latest = sql(native, "SELECT context FROM turns WHERE text=?", attached)[0][0]
            assert "先聊聊演示项目的目标" not in latest  # No fallback replay on an ordinary resume.
            assert str(checkout) in latest
            assert sql(native, "SELECT id FROM threads") == [(original,)]
            assert len(sql(native,"SELECT * FROM tool_results")) == 7

            cli("discussion", "detach", chat, repo["id"])
            probe(chat, original, {checkout / "README.md": None})
            before = subprocess.check_output(["git","-C",root/"example","worktree","list","--porcelain"])
            parent_choice = cli("discussion", "workspaces", f"#{parent_number}")["choices"]
            child_choice = next(row for row in parent_choice if row["issue_id"] == child_id)
            root_choice = next(row for row in parent_choice if row["issue_id"] == parent_id)
            assert child_choice["task_id"] != root_choice["task_id"]
            child_ref = attach(chat,kind="worktree",issue_id=child_id,task_id=child_choice["task_id"],repo_id="example")
            parent_ref = attach(chat,kind="worktree",issue_id=parent_id,task_id=root_choice["task_id"],repo_id="example")
            assert child_ref["status"] == "available" and parent_ref["status"] == "no_registered_mount", (child_ref,parent_ref)
            assert "path" not in child_ref and str(root) not in json.dumps(child_ref)
            assert subprocess.check_output(["git","-C",root/"example","worktree","list","--porcelain"]) == before
            mounted = root / "runner.runs" / "workspaces" / parent_id / "repos" / "example"
            say(chat, "Read mount", INTRO)
            assert_scope("Read mount", original, *worktree_roots(mounted))

            task = child_choice["task_id"]
            add_fixture_task(root, task, "shared-task", "workspaces", "example", True, existing_key=parent_id)
            shared_ref = attach(chat,kind="worktree",issue_id=child_id,task_id="shared-task",repo_id="example")
            assert shared_ref["status"] == "available", shared_ref
            add_fixture_task(root, task, "review-task", "workspaces", "example", True,
                             existing_key=parent_id, bound_to=task)
            review_ref = attach(chat,kind="worktree",issue_id=child_id,task_id="review-task",repo_id="example")
            assert review_ref["status"] == "available", review_ref
            _, isolated = add_fixture_task(root, task, "isolated-task", "workspaces", "example", True)
            _, fallback = add_fixture_task(root, task, "fallback-task", "tasks", "example", False)
            isolated_ref = attach(chat,kind="worktree",issue_id=child_id,task_id="isolated-task",repo_id="example")
            fallback_ref = attach(chat,kind="worktree",issue_id=child_id,task_id="fallback-task",repo_id="example")
            assert isolated_ref["status"] == "available" and fallback_ref["status"] == "no_registered_mount"
            before = subprocess.check_output(["git","-C",root/"example","worktree","list","--porcelain"])
            assert status(chat, isolated_ref["id"]) == "available" and fallback.is_dir()
            assert subprocess.check_output(["git","-C",root/"example","worktree","list","--porcelain"]) == before
            with old_inspection() as url:
                sql(db, "UPDATE runners SET host_id='other-host' WHERE id='demo-runner'")
                assert status(chat, child_ref["id"]) == "different_host"
                sql(db, "UPDATE runners SET host_id='demo-host',profile=json_set(profile,'$.inspect_url',?) WHERE id='demo-runner'", url)
                assert status(chat, child_ref["id"]) == "runner_workspace_query_unsupported"
                sql(db, "UPDATE runners SET profile=json_set(profile,'$.inspect_url',?) WHERE id='demo-runner'", f"http://{inspect}")

            replacement = isolated / "repos" / "example"
            readme, stale, new = (path / "README.md" for path in (checkout, mounted, replacement))
            probe(chat, original, {readme: None, stale: INTRO, new: INTRO},
                  *worktree_roots(mounted), *worktree_roots(replacement))
            for ref in (child_ref, shared_ref, review_ref):
                cli("discussion", "detach", chat, ref["id"])
            probe(chat, original, {readme: None, stale: None, new: INTRO}, *worktree_roots(replacement))

            shutil.rmtree(isolated)
            assert status(chat, isolated_ref["id"]) == "workspace_reclaimed_or_changed"
            probe(chat, original, {stale: None})


            assert cli("discussion", "create", "主 Chat 引用示例", "--from-main", parent["id"])["chat_id"] != chat
            origin = cli("discussion", "create", "Issue 资料笔记", "--from-issue", f"#{linked_number}")["chat_id"]
            assert api(f"discussions/{origin}")["references"][0]["id"] == issue["id"]

            # An unexpected permission confirmation must never start a turn or replace the binding.
            mismatch = root / "app-home" / "mismatch-roots"
            mismatch.touch()
            say(chat, "Reject incorrect roots", error="Could not establish the read-only Discussion thread")
            assert not sql(native, "SELECT id FROM turns WHERE text='Reject incorrect roots'")
            assert sql(native, "SELECT id FROM threads") == [(original,)]
            assert sql(db, "SELECT thread_id FROM router_threads WHERE chat_id=?", chat) == [(original,)]
            mismatch.unlink()

            before_count = sql(db,"SELECT count(*) FROM router_dispatches")
            cli("discussion", "draft", chat, "draft-once")
            draft = wait_for(lambda: cli("discussion", "draft-read",chat,"draft-once")["draft_request"],
                             lambda row: row and row["status"] != "queued")
            assert draft["status"] == "completed" and draft["draft"]["goal"], draft
            assert all(not row["text"].startswith(DRAFT_TEXT) for row in messages("main"))
            assert "Prepare the editable handoff" not in json.dumps(messages(chat))
            assert sql(db,"SELECT count(*) FROM router_dispatches") == before_count
            body = {**draft["draft"],"goal":"核对后的编辑目标","draft_id":"draft-once",
                    "reference_ids":[issue["id"],pull["id"]],"target_issue_id":parent_id}
            send = ("discussion", "handoff", chat, json.dumps(body,ensure_ascii=False), "send-once")
            with lose_first_response(address) as (proxy, posted):
                receipt = cli(*send, at=proxy)
                assert receipt == posted[0][2]
            assert receipt == cli(*send)
            cli(*send[:3],json.dumps({**body,"goal":"冲突"}),"send-once",error="HTTP 409")
            main = wait_for(lambda: messages("main"), lambda rows: any(row["text"] == ROUTER_REPLY for row in rows))
            handoffs = [row for row in main if row["text"].startswith(DRAFT_TEXT)]
            assert len(handoffs)==1 and handoffs[0]["id"]==receipt["message_id"]
            row = handoffs[0]
            assert row["discussion_id"]==chat and "核对后的编辑目标" in row["text"]
            assert row["text"].index(parent_id) < row["text"].index(linked_id)
            assert cli("discussion", "read", chat)["handoffs"][0]["href"] == f"/#message-{row['id']}"
            assert sql(db,"SELECT count(*) FROM router_dispatches") == before_count

            assert sql(native, "SELECT DISTINCT thread_id FROM turns") == [(original,)]
            assert sql(native, "SELECT DISTINCT id FROM resumes WHERE found=1") == [(original,)]
            assert sql(native, "SELECT id FROM threads") == [(original,)]
            # Only the explicit native 'no rollout found' response permits replacement and replay.
            sql(native, "DELETE FROM threads WHERE id=?", original)
            say(chat, "Continue after native history loss", FIRST_ANSWER)
            [(replacement_id, recovery_context)] = sql(
                native, "SELECT thread_id, context FROM turns WHERE text='Continue after native history loss'")
            assert replacement_id != original
            assert sql(native, "SELECT id, found FROM resumes ORDER BY rowid DESC LIMIT 1") == [(original, 0)]
            assert "Fixture scope matched." in recovery_context, recovery_context
            assert sql(db, "SELECT thread_id, model FROM router_threads WHERE chat_id=?", chat) == [(replacement_id, None)]
            assert_scope("Continue after native history loss", replacement_id)

            before_archive = cli("discussion", "read", chat)
            saved_messages = messages(chat)
            saved_count = sql(db, "SELECT count(*) FROM discussions")
            assert api(f"discussions/{chat}", "PATCH", {"archived": "true"}, status=400)
            assert api(f"discussions/{chat}", "PATCH", {"archived": True, "title": "conflict"}, status=400)
            assert api("discussions?archived=maybe", status=400)
            assert api("discussions/absent", "PATCH", {"archived": True}, status=404)
            expected = {"chat_id": chat, "archived": True}
            assert cli("discussion", "archive", chat) == expected
            assert cli("discussion", "archive", chat) == expected
            assert all(row["chat_id"] != chat for row in api("discussions")["discussions"])
            assert [row["chat_id"] for row in cli("discussion", "list", "--archived")["discussions"]] == [chat]
            assert api(f"discussions/{chat}") == {**before_archive, "archived": True}
            assert messages(chat) == saved_messages
            assert sql(db, "SELECT count(*) FROM discussions") == saved_count

            # Start a turn on the original native thread, then archive before its reply.
            cli("discussion", "restore", chat)
            held = "Hold discussion across archive"
            op = "archive-spanning-message"
            held_id = cli("chat", "send", chat, held, op)["message_id"]
            started = root / "app-home" / "archive-turn-started"
            wait_for(started.exists, bool)
            assert not any(row["reply_to"] == held_id for row in messages(chat))
            assert cli("discussion", "archive", chat) == expected
            (root / "app-home" / "archive-turn-release").touch()
            assert say(chat, held, FIRST_ANSWER, operation=op)["id"] == held_id
            say(chat, "Continue while archived", FIRST_ANSWER)
            assert_scope(held, replacement_id)
            assert_scope("Continue while archived", replacement_id)
            assert api(f"discussions/{chat}")["archived"]
            assert messages(chat)[:len(saved_messages)] == saved_messages
            before_archive = {**cli("discussion", "read", chat), "archived": False}
            saved_messages = messages(chat)

            # The inspection tests are done. Release the fixture worker before the
            # browser waits for the normal detail polling cadence.
            (root / "finish-first").touch()
            if args.screenshots:
                args.screenshots.mkdir(parents=True,exist_ok=True)
                subprocess.run([args.node,WEB/"tests/discussions.mjs"], check=True, timeout=150,
                    env={**env,"TEND_SCREENSHOTS":str(args.screenshots),"TEND_TEST_CLI":str(args.cli),
                         "TEND_LEGACY_DISCUSSION":unnamed, "TEND_EXPLICIT_DISCUSSION":explicitly_named,
                         "TEND_REF_ISSUE":str(linked_number),
                         "TEND_TARGET_ISSUE":str(parent_number)})

        with server(args.server, root, env, **options):
            saved_detail = persisted_discussion(before_archive)
            assert persisted_discussion(api(f"discussions/{chat}")) == {**saved_detail, "archived": True}
            assert cli("discussion", "restore", chat) == {"chat_id": chat, "archived": False}
            assert persisted_discussion(api(f"discussions/{chat}")) == saved_detail
            assert messages(chat) == saved_messages
            assert sql(db, "SELECT thread_id FROM router_threads WHERE chat_id=?", chat) == [(replacement_id,)]

        with server(args.server, root, env, listen=address):
            unavailable = cli("discussion", "create", "Unconfigured discussion")["chat_id"]
            assert not api(f"discussions/{unavailable}")["execution_configured"]
            msg = cli("chat", "send", unavailable, "This must not run in the main Router.")["message_id"]
            failed = wait_for(lambda: messages(unavailable), lambda rows: bool(rows and rows[0]["router_state"] == "failed"))
            assert len(failed) == 1 and failed[0]["id"] == msg and "not configured" in failed[0]["router_error"]

    print("PASS Discussion v32 upgrade, CLI/API archive and restore across restart, history and in-flight reply preserved, original thread and retry, exact turn scopes, Runner worktree switches, and Router handoff")


if __name__ == "__main__":
    main()
