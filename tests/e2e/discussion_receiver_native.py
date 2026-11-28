"""Inspect real Codex receiver inputs and ordinary Router continuation for a handoff."""
import argparse
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import threading

from chat import server, wait_for, FIXTURE

PROMPT = "Synced synthetic receiver boundary test: compare choices, do not act."
SUMMARY = "Compare two synthetic retention options. No implementation was authorized."
REPLY = "Both synthetic options are worth comparing. No work was started."
OVERLAP_PROMPT = "Synthetic overlapping input test: compare two export formats without implementing."
OVERLAP_SUMMARY = "Compare synthetic CSV and JSON formats. No work was authorized."
OVERLAP_REPLY = "Synthetic CSV opens in a spreadsheet while JSON preserves nested records."
MODEL = "gpt-5.4"
MAIN_ONLY = "Storage measurement MAIN_ONLY_PROVIDER_BOUNDARY_CANARY"


@contextmanager
def model():
    captured = {}
    receiving_requests = []
    overlapping_receiver_started = threading.Event()
    release_overlapping_reply = threading.Event()

    class Fixture(BaseHTTPRequestHandler):
        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            prompts = [part.get("text", "") for item in request["input"]
                       if item.get("role") == "user" for part in item.get("content", [])]
            last = prompts[-1]
            if last.startswith("Write the Main Chat handoff message"):
                if OVERLAP_PROMPT in prompts:
                    mode, answer = "overlap_summary", OVERLAP_SUMMARY
                else:
                    mode, answer = "summary", SUMMARY
            elif last.startswith("讨论转交 · 上下文摘要。"):
                receiving_requests.append(request)
                if OVERLAP_SUMMARY in last:
                    mode, answer = "overlap_receiver", OVERLAP_REPLY
                    overlapping_receiver_started.set()
                    assert release_overlapping_reply.wait(30), "overlap reply not released"
                else:
                    mode, answer = "receiver", REPLY
            else:
                assert last in (PROMPT, OVERLAP_PROMPT), prompts
                mode, answer = "discussion", "This is a synthetic read-only discussion."
            captured[mode] = [tool.get("name", tool["type"]) for tool in request.get("tools", [])]
            item = {"id": f"msg_{mode}", "type": "message", "role": "assistant",
                    "status": "completed", "phase": "final_answer",
                    "content": [{"type": "output_text", "text": answer, "annotations": []}]}
            response = {"id": f"resp_{mode}", "model": MODEL, "output": [item]}
            events = [
                {"type": "response.created", "response": {"id": response["id"]}},
                {"type": "response.output_item.added", "output_index": 0, "item": item},
                {"type": "response.output_item.done", "output_index": 0, "item": item},
                {"type": "response.completed", "response": response},
            ]
            body = "".join(f"data: {json.dumps(event)}\n\n" for event in events).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    with ThreadingHTTPServer(("127.0.0.1", 0), Fixture) as listener:
        worker = threading.Thread(target=listener.serve_forever)
        worker.start()
        try:
            yield (f"http://127.0.0.1:{listener.server_port}", captured, receiving_requests,
                   overlapping_receiver_started, release_overlapping_reply)
        finally:
            release_overlapping_reply.set()
            listener.shutdown()
            worker.join()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "cli", "codex", "scratch"):
        parser.add_argument(name, type=lambda p: Path(p).resolve())
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-native-receiver-", dir=args.scratch) as temp, model() as fixture:
        url, captured, receiving_requests, overlapping_receiver_started, release_overlapping_reply = fixture
        root = Path(temp)
        home = root / "codex"
        cwd = root / "empty"
        home.mkdir()
        cwd.mkdir()
        (home / "model-key").write_text("local-fixture-key")
        (home / "config.toml").write_text(f'''
model = "{MODEL}"
model_provider = "fixture"

[model_providers.fixture]
name = "Local receiver fixture"
base_url = "{url}/v1"
wire_api = "responses"
env_key = "FIXTURE_API_KEY"

[features]
shell_tool = true
unified_exec = true
browser_use = true
apply_patch_freeform = true
view_image = true
goals = true

[tools.experimental_request_user_input]
enabled = true
''')
        config = root / "discussion.json"
        config.write_text(json.dumps({
            "host_id": "synthetic-receiver", "program": str(args.codex),
            "codex_home": str(home), "cwd": str(cwd), "model": MODEL,
            "model_api_keys": {"FIXTURE_API_KEY": "codex/model-key"},
        }))
        env = {**os.environ, "CODEX_HOME": str(root / "fixture-codex"), "TEND_TOKEN": "synthetic-native-receiver"}
        with server(args.server, root, env, discussion_config=config, router_fixture=FIXTURE) as address:
            # Emulate deliveries already persisted by v42, without migrating
            # them into the newly authorized routing path.
            with sqlite3.connect(root / "server.sqlite") as db:
                db.executescript("""CREATE TRIGGER legacy_handoffs AFTER INSERT ON discussion_handoffs
                    WHEN json_extract(NEW.intent, '$.mode')='quick'
                    BEGIN UPDATE discussion_handoffs SET intent='{"mode":"quick"}'
                        WHERE message_id=NEW.message_id; END;""")
            def cli(*words):
                result = subprocess.run([args.cli, *words], capture_output=True, text=True, timeout=10,
                                        env={**env, "TEND_URL": f"http://{address}"})
                assert result.returncode == 0, result.stderr
                return json.loads(result.stdout)

            def wait_main_reply(mid):
                return next(r for r in wait_for(
                    lambda: cli("chat", "read", "main")["messages"],
                    lambda rows: any(r["reply_to"] == mid for r in rows),
                ) if r["reply_to"] == mid)

            def main_say(text, *options):
                return wait_main_reply(cli("chat", "send", "main", text, *options)["message_id"])

            def query(file, statement, *values):
                with sqlite3.connect(root / file) as db:
                    return db.execute(statement, values).fetchall()

            def actual_turn(text):
                return wait_for(lambda: query("app.sqlite",
                    "SELECT thread_id, context FROM start_requests WHERE input=?", text), bool)[0]

            main_say(MAIN_ONLY)
            original_router, _ = actual_turn(MAIN_ONLY)
            chat = cli("discussion", "create")["chat_id"]
            original = cli("chat", "send", chat, PROMPT)["message_id"]
            wait_for(lambda: cli("chat", "read", chat)["messages"],
                     lambda rows: any(row["reply_to"] == original for row in rows))
            cli("discussion", "handoff-now", chat)
            rows = wait_for(lambda: cli("chat", "read", "main")["messages"],
                            lambda rows: any(row["text"] == REPLY for row in rows))
            [delivery] = [r for r in rows if r.get("discussion_id") == chat]
            [reply] = [r for r in rows if r["reply_to"] == delivery["id"]]
            assert SUMMARY in delivery["text"] and delivery["router_state"] == "completed"
            assert len(receiving_requests) == 1
            actual_receiver = json.dumps(receiving_requests[0], ensure_ascii=False)
            assert MAIN_ONLY not in actual_receiver and SUMMARY in actual_receiver
            assert "Tend provenance:" in actual_receiver
            assert query("app.sqlite", "SELECT count(*) FROM start_requests") == [(1,)]
            assert query("server.sqlite", "SELECT count(*) FROM router_dispatches") == [(0,)]
            assert query("server.sqlite", "SELECT thread_id FROM router_threads WHERE chat_id='main'") == [(original_router,)]
            print("PASS actual Discussion provider model request excludes unrelated Main Chat history")

            def assert_exchange(context, chat=chat, delivery=delivery, reply=reply,
                                summary=SUMMARY, reply_text=REPLY):
                assert context is not None and summary in context, "ordinary Router did not receive the handoff exchange"
                lines = context.splitlines()
                [index] = [i for i,l in enumerate(lines) if l.startswith("Tend background: recent agent-written")]
                [exchange] = json.loads(lines[index + 1])
                assert exchange["provenance"] == "agent_generated_discussion_handoff", exchange
                assert exchange["discussion_chat_id"] == chat and exchange["handoff_message_id"] == delivery["id"]
                assert summary in exchange["handoff_text"]
                expected_replies = [{"message_id": reply["id"], "text": reply_text}] if reply else []
                assert exchange["isolated_receiver_replies"] == expected_replies
                assert context.count(summary) == 1
                assert context.count(reply_text) == int(reply is not None), context

            follow = "Storage measurement warm unthreaded question about your advice"
            main_say(follow)
            warm, context = actual_turn(follow)
            assert warm == original_router
            assert_exchange(context)
            print("PASS warm ordinary Router sees the complete labeled isolated exchange on an unthreaded follow-up")

            explicit = "Storage measurement explicit reply to isolated advice"
            main_say(explicit, "--reply-to", reply["id"])
            _, context = actual_turn(explicit)
            assert '"kind":"isolated_handoff_receiver_reply"' in context and '"authorizes_work":false' in context

            # Synthetic durable external batch on the earlier ordinary topic.
            # Remove the fixture binding to force the event down cold replay.
            with sqlite3.connect(root / "server.sqlite") as db:
                db.execute("DELETE FROM router_threads WHERE chat_id='main'")
                db.execute("INSERT INTO external_route_batches VALUES ('context-fixture','main',0,0)")
                db.execute("""INSERT INTO router_inputs(chat_id,message_seq,external_batch)
                    SELECT 'main',seq,'context-fixture' FROM chat_messages WHERE text=?""", (MAIN_ONLY,))
            cold, context = actual_turn("Tend external facts collected for this Chat.\n[]")
            assert cold != original_router
            assert_exchange(context)
            lines = context.splitlines()
            [index] = [i for i,l in enumerate(lines) if l.startswith("Recent settled messages")]
            assert all(SUMMARY not in entry["text"] and REPLY not in entry["text"]
                       for entry in json.loads(lines[index + 1]))
            assert MAIN_ONLY in lines[index + 1]
            print("PASS external-event Router recovery replays generated exchanges once, outside human history")

            overlapping_chat = cli("discussion", "create")["chat_id"]
            overlapping_prompt = cli("chat", "send", overlapping_chat, OVERLAP_PROMPT)["message_id"]
            wait_for(lambda: cli("chat", "read", overlapping_chat)["messages"],
                     lambda rows: any(row["reply_to"] == overlapping_prompt for row in rows))
            cli("discussion", "handoff-now", overlapping_chat)
            assert overlapping_receiver_started.wait(30), "the overlapping receiver did not start"
            rows = cli("chat", "read", "main")["messages"]
            [overlapping_delivery] = [r for r in rows if r.get("discussion_id") == overlapping_chat]
            assert not any(r["reply_to"] == overlapping_delivery["id"] for r in rows)
            intervening = "Storage measurement unrelated overlapping ordinary input"
            intervening_id = cli("chat", "send", "main", intervening)["message_id"]
            release_overlapping_reply.set()
            overlapping_reply = wait_main_reply(overlapping_delivery["id"])
            assert overlapping_reply["text"] == OVERLAP_REPLY
            wait_main_reply(intervening_id)
            thread, context = actual_turn(intervening)
            assert thread == cold
            assert_exchange(context, overlapping_chat, overlapping_delivery, None, OVERLAP_SUMMARY, OVERLAP_REPLY)

            after = "Storage measurement what was your advice on the export formats?"
            after_reply = main_say(after)
            ids = (overlapping_delivery["id"], intervening_id, overlapping_reply["id"], after_reply["reply_to"])
            actual_order = query("server.sqlite", "SELECT id FROM chat_messages WHERE id IN (?,?,?,?) ORDER BY seq", *ids)
            assert actual_order == [(mid,) for mid in ids], actual_order
            thread, context = actual_turn(after)
            assert thread == cold
            assert_exchange(context, overlapping_chat, overlapping_delivery, overlapping_reply, OVERLAP_SUMMARY, OVERLAP_REPLY)

            settled = "Storage measurement another ordinary input after the advice was bridged"
            main_say(settled)
            thread, context = actual_turn(settled)
            assert thread == cold
            assert OVERLAP_SUMMARY not in (context or "") and OVERLAP_REPLY not in (context or "")
            print("PASS warm Router bridges a late isolated reply at the first eligible input cutoff, without repeating it")

            assert "exec_command" in captured["discussion"], captured
            assert "tend_read_discussion_history" in captured["summary"], captured
            assert captured["receiver"] == [], captured
            assert captured["overlap_receiver"] == [], captured
            native = query("server.sqlite", """SELECT source,thread,turn FROM token_agent_attempts
                WHERE source IN ('discussion','discussion_summary','handoff_receiver')""")
            assert sorted(source for source, _, _ in native) == sorted(
                ["discussion", "discussion_summary", "handoff_receiver"] * 2), native
            assert len({thread for source, thread, _ in native if source == "handoff_receiver"}) == 2
            for source, native_thread, native_turn in native:
                [rollout] = (home / "sessions").rglob(f"rollout-*-{native_thread}.jsonl")
                records = [json.loads(line) for line in rollout.read_text().splitlines()]
                assert records[0]["payload"]["id"] == native_thread
                assert any(row["type"] == "turn_context" and row["payload"]["turn_id"] == native_turn
                           for row in records), source
                assert not list((root / "fixture-codex").rglob(f"*-{native_thread}.jsonl"))
            print(json.dumps(captured, ensure_ascii=False))
            print("PASS Discussion, summary, and tool-free isolated receiver retain their original native turn IDs in the Discussion home")


if __name__ == "__main__":
    main()
