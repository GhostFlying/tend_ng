"""Isolated JSON-RPC fixture for the named Feishu assessor."""
import fcntl
import json
import os
from pathlib import Path
import signal
import sys
import time
import uuid

root = Path(os.environ["HOME"])
home = Path(os.environ["CODEX_HOME"])
assert home == root / ".codex-ark"
assert not os.environ.get("TEND_TOKEN")
assert sys.argv[1:4] == ["app-server", "--listen", "stdio://"]
flags = dict(a.split("=", 1) for a in sys.argv[4:] if "=" in a)
assert flags["agents.enabled"] == "false" and flags["mcp_servers"] == "{}"
assert flags["default_permissions"] == '"tend-router-routing"'
assert flags["permissions.tend-router-routing.network.enabled"] == "false"


def result(request, body):
    print(json.dumps({"id": request["id"], "result": body}), flush=True)


for line in sys.stdin:
    request = json.loads(line)
    params = request.get("params", {})
    match request["method"]:
        case "initialize":
            result(request, {"codexHome": str(home)})
        case "initialized":
            pass
        case "config/read":
            result(request, {"config": {"approval_policy": "never", "web_search": "disabled",
                                       "agents": {"enabled": False}, "mcp_servers": {}, "features": {
                                           k.split(".", 1)[1]: False for k, v in flags.items()
                                           if k.startswith("features.") and v == "false"
                                       }}})
        case "thread/start":
            assert params["ephemeral"] is False and params["historyMode"] == "legacy"
            assert [tool["name"] for tool in params["dynamicTools"]] == [
                "feishu_read_material", "feishu_read_page"
            ]
            if (root / "system-meter-fixture").exists():
                assert params["experimentalRawEvents"]
            assert params["model"] == "deepseek-v4.1-flash"
            thread = str(uuid.uuid4())
            sessions = home / "sessions"
            sessions.mkdir(exist_ok=True)
            rollout = sessions / f"rollout-{thread}.jsonl"
            def retain(kind, body):
                with rollout.open("a") as file:
                    file.write(json.dumps({"type": kind, "payload": body}, ensure_ascii=False) + "\n")
            retain("session_meta", {"id": thread})
            result(request, {"thread": {"id": thread, "ephemeral": False, "path": str(rollout)},
                             "model": "fixture-confirmed-model", "approvalPolicy": "never", "runtimeWorkspaceRoots": [],
                             "activePermissionProfile": {"id": "tend-router-routing"},
                             "sandbox": {"type": "readOnly", "networkAccess": False}})
            stall = root / "stall-next"
            if stall.exists():
                stall.unlink()
                capacity = fcntl.fcntl(sys.stdin.fileno(), fcntl.F_SETPIPE_SZ, 4096)
                (root / "stalled").write_text(f"{os.getpid()} {capacity}")
                while True:
                    signal.pause()
        case "turn/start":
            assert params["threadId"] == thread and params["effort"] == "high"
            assert params["permissions"] == "tend-router-routing"
            context = json.loads(params["input"][0]["text"])
            anchor = context["anchor"]["id"]
            assert "不相关的群聊" not in str(context)
            assert context["owner"]["id"] == "ou_fixture_owner" and context["owner"]["name"]
            assert len(context["before"]) <= 2 and len(context["after"]) <= 4
            assert "cited" not in context and "assessment" not in context
            material = [context["anchor"], *context["direct"], *context["before"], *context["after"]]
            assert len({m["id"] for m in material}) == len(material)
            retain("fixture_input", context)
            turn = str(uuid.uuid4())
            result(request, {"turn": {"id": turn, "status": "inProgress", "items": []}})

            def tool(name, arguments):
                call = str(uuid.uuid4())
                print(json.dumps({"id": call, "method": "item/tool/call", "params": {
                    "threadId": thread, "turnId": turn, "callId": call, "tool": name, "arguments": arguments
                }}), flush=True)
                answer = json.loads(sys.stdin.readline())
                assert answer["id"] == call, answer
                body = json.loads(answer["result"]["contentItems"][0]["text"])
                retain("fixture_tool", [name, arguments, body])
                return answer["result"]["success"], body

            replies = context["after"]
            request_ids = [anchor]
            current_ids = [m["id"] for m in replies] or [anchor]
            question_basis = current_ids[-1]
            if anchor == "om_qa_anchor":
                assert context["thread"] == "omt_qa"
                assert context["anchor"]["source"]["reply_to"] == "om_qa_parent"
                assert context["anchor"]["source"]["root_id"] == "om_qa_parent"
                assert [r["id"] for r in context["references"]] == ["om_qa_parent"]
                if context["references"][0]["status"] == "unavailable":
                    assert "direct_reference_unavailable" in context["observation"]["gaps"]
                    (root.parent / "fixture-qa-read-parent").touch()
                ok, parent = tool("feishu_read_material", {"id":"om_qa_parent"})
                assert ok and parent["scope"] == "direct_reference" and "Android 发布排期" in parent["message"]["text"]
                request_ids += ["om_qa_parent"]
                hold = root / "hold-qa-assessment"
                if hold.exists():
                    (root / "qa-assessment-has-input").touch()
                    while hold.exists():
                        time.sleep(0.01)
                headline = "确认 Android 灰度窗口与回滚阈值"
                request_copy = "林青请你确认 Android 灰度窗口与回滚阈值。"
                overview = ("QA 新增 Android 12 和 15 验收范围，回滚阈值仍待确认。"
                            if len(replies) > 1 else "候选包已通过冒烟，灰度比例先维持 5%。")
                questions = ["请 QA 确认触发回滚的阈值。"]
                disposition = "requested_of_you"
            elif anchor == "om_docs_anchor":
                assert [reply["id"] for reply in replies] in (["om_docs_reply"], ["om_docs_reply", "om_docs_followup"])
                headline, overview, questions, disposition = "本地启动文档已补齐", "来源回复明确关闭此项。", [], "completed"
                request_copy = "周韵请你核对本地启动流程，随后本地启动文档已补齐。"
            elif anchor == "om_release_anchor":
                headline, overview, questions, disposition = "确认周五发布负责人", "发布窗口已确认。", ["由谁确认发布？"], "requested_of_you"
                request_copy = "顾宁请你确认周五发布负责人。"
            elif anchor in ("om_transition_anchor", "om_scantransition_anchor"):
                key = anchor.removeprefix("om_").removesuffix("_anchor")
                headline, request_copy = "跟进灰度验收", "沈言请你跟进灰度验收。"
                old_id = f"om_{key}_old_group"
                direct_id = f"om_{key}_reference"
                if context["thread"] is None:
                    assert [r["id"] for r in replies] == [old_id], context
                    overview, disposition, questions = "旧群聊已有验收回复，仍需确认对应范围。", "requested_of_you", []
                    current_ids = [old_id]
                else:
                    assert context["thread"] == f"omt_{key}"
                    assert all(row["id"] != old_id for row in material)
                    assert [r["id"] for r in context["direct"]] == [direct_id]
                    ok, direct = tool("feishu_read_material", {"id": direct_id})
                    assert ok and direct["scope"] == "direct_reference" and direct["message"]["source"]["thread_id"] is None
                    ok, outside = tool("feishu_read_material", {"id": old_id})
                    assert not ok and outside["gap"] == "outside_material_scope"
                    if replies:
                        assert [r["id"] for r in replies] == [f"om_{key}_done"]
                        overview, disposition, questions = "话题回复确认已通过灰度验收。", "completed", []
                    else:
                        assert context["observation"]["after_status"] == "checked_empty"
                        ok, after = tool("feishu_read_page", {"direction":"after"})
                        assert ok and after["status"] == "checked_no_additional_material" and not after["messages"]
                        request_ids += [direct_id]
                        overview, disposition, questions = "此话题还未看到验收回复。", "requested_of_you", []
                        if (root / "fixture-use-old-topic-citation").exists():
                            current_ids = [old_id]
                            overview, disposition = "已经验收完成。", "completed"
            elif anchor in ("om_expandpos_anchor", "om_expandmeta_anchor", "om_expandreply_anchor"):
                key = anchor.removeprefix("om_").removesuffix("_anchor")
                assert context["thread"] == f"omt_{key}"
                source, *conflicts = replies
                assert len(conflicts) == 3 and source["id"] == f"om_{key}_done"
                assert source["created_at"] == context["anchor"]["created_at"]
                assert source["source"]["position"] == 9 and source["source"]["truncated"]
                assert "签字项齐全" not in source["text"]
                for conflict in conflicts:
                    ok, rejected = tool("feishu_read_material", {"id": conflict["id"]})
                    assert not ok and rejected["gap"] == "version_not_proven_at_cutoff", (conflict["id"], rejected)
                    assert "message" not in rejected
                ok, expanded = tool("feishu_read_material", {"id": source["id"]})
                assert ok and expanded["through"] == context["observation"]["through"], expanded
                m = expanded["message"]
                assert m["created_at"] == source["created_at"] and m["updated_at"] == source["updated_at"]
                assert m["href"] == source["href"] and m["href"].startswith("https://applink.feishu.cn/client/thread/open?")
                assert m["source"] == {**source["source"], "truncated": False}
                assert m["text"].endswith("验收凭据：签字项齐全。")
                headline, request_copy = "核对扩读验收结果", "沈言请你核对扩读验收结论。"
                overview, disposition, questions = "展开同秒的回复后，签字项齐全，已通过验收。", "completed", []
                current_ids = [source["id"]]
            elif anchor == "om_roles_anchor":
                owner = context["owner"]["id"]
                text = context["anchor"]["text"]
                assert {m["id"] for m in context["anchor"]["source"]["mentions"]} == {owner, "ou_same_name"}
                assert text in ("@{ou_fixture_owner} 负责发版，@{ou_same_name} 负责审批。",
                                "@{ou_same_name} 负责发版，@{ou_fixture_owner} 负责审批。"), text
                action = "发版" if f"@{{{owner}}} 负责发版" in text else "审批"
                headline, request_copy = "本轮发版和审批分工", f"顾宁请你负责{action}。"
                overview, disposition, questions = f"当前轮到你{action}。", "requested_of_you", []
            elif anchor == "om_chain_anchor":
                assert [r["id"] for r in replies] == [f"om_chain_{i}" for i in range(2, 6)]
                assert "om_chain_5" in context["related_reply_ids"]
                ok, after = tool("feishu_read_page", {"direction":"after"})
                assert ok and [r["message"]["id"] for r in after["messages"]] == ["om_chain_1"]
                assert after["messages"][0]["scope"] == "related"
                headline, request_copy = "多层回复链验收", "顾宁请你跟进这一轮验收。"
                overview, disposition, questions = "第五条回复确认已经完成验收。", "completed", []
                current_ids = ["om_chain_5"]
            elif anchor == "om_promotion_anchor":
                assert not replies
                ok, after = tool("feishu_read_page", {"direction":"after"})
                assert ok and after["next_cursor"]
                assert [(r["message"]["id"], r["scope"]) for r in after["messages"]] == [
                    ("om_promotion_5", "nearby_unverified")]
                ok, next_page = tool("feishu_read_page", {"direction":"after", "cursor":after["next_cursor"]})
                assert ok and "om_promotion_5" in next_page["promoted_related_ids"], next_page
                assert all(row["message"]["id"] != "om_promotion_5" for row in next_page["messages"])
                ok, proof = tool("feishu_read_material", {"id":"om_promotion_5"})
                assert ok and proof["scope"] == "related"
                headline, request_copy = "跨页核实验收", "顾宁请你跟进这一轮验收。"
                overview, disposition, questions = "补页确认第五条回复属于本事项且验收已完成。", "completed", []
                current_ids = ["om_promotion_5"]
            elif anchor == "om_spec_ask":
                if context["anchor"]["source"]["withdrawn"]:
                    headline, request_copy = "对照表单位问题已撤回", "陈宁原本向你询问对照表的耗时单位。"
                    overview, disposition, questions, current_ids = "该 @ 已在飞书撤回。", "withdrawn", [], [anchor]
                else:
                    assert context["anchor"]["source"]["mentions"] == [{"id": "ou_fixture_owner"}]
                    first_ids = [m["id"] for m in material]
                    assert "om_spec_fyi" not in first_ids and "om_spec_demo" not in first_ids and "om_spec_noise" not in first_ids
                    assert not {"om_spec_sibling", "om_spec_unknown_edit", "om_spec_unknown_quote"} & set(first_ids)
                    assert not any("future" in id for id in first_ids)
                    assert "om_spec_private" not in first_ids
                    through = context["observation"]["through"]
                    ok, bad = tool("feishu_read_material", {"id":"om_unrelated_fake"})
                    assert not ok and bad["gap"] == "outside_material_scope"
                    ok, bad = tool("feishu_read_material", {"id":"om_spec_private"})
                    assert not ok and bad["gap"] == "direct_reference_unavailable"
                    ok, unproven = tool("feishu_read_material", {"id":"om_spec_unknown_quote"})
                    assert not ok and unproven["gap"] == "version_not_proven_at_cutoff"
                    assert "message" not in unproven
                    (root.parent / "fixture-spec-read-parent").touch()
                    ok, target = tool("feishu_read_material", {"id":"om_spec_table"})
                    assert target["through"] == through
                    if ok:
                        assert target["message"]["sender"] == "小林" and target["message"]["source"]["author_id"] == "ou_other_same_name"
                        assert [m["id"] for m in context["direct"]].count("om_spec_table") <= 1
                        request_ids += ["om_spec_table"]
                    else:
                        assert target["gap"] == "direct_reference_unavailable"
                    ok, before = tool("feishu_read_page", {"direction":"before"})
                    assert ok and before["through"] == through
                    assert any(row["message"]["id"] == "om_spec_version" and row["scope"] == "nearby_unverified"
                               for row in before["messages"]), before
                    ok, first = tool("feishu_read_page", {"direction":"after"})
                    assert ok and first["next_cursor"] and first["through"] == through
                    assert all("future" not in row["message"]["id"] for row in first["messages"])
                    assert any(row["message"]["id"] == "om_spec_sibling" and row["scope"] == "nearby_unverified"
                               for row in first["messages"]), first
                    ok, wrong = tool("feishu_read_page", {"direction":"before", "cursor":first["next_cursor"]})
                    assert not ok and wrong["gap"] == "invalid_cursor"
                    ok, next_page = tool("feishu_read_page", {"direction":"after", "cursor":first["next_cursor"]})
                    assert ok and next_page["through"] == through and next_page["next_cursor"] is None
                    assert all("future" not in row["message"]["id"] and row["message"]["id"] != "om_spec_unknown_edit"
                               for row in next_page["messages"])
                    assert "version_not_proven_at_cutoff" in next_page["gaps"], next_page
                    hold = root / "hold-spec-assessment"
                    if hold.exists():
                        (root / "spec-assessment-has-input").touch()
                        while hold.exists():
                            time.sleep(0.01)
                    headline = "周四版对照表的耗时单位"
                    request_copy = ("陈宁问周四版对照表的耗时列是否也改用毫秒。"
                                    if "om_spec_table" in request_ids else "陈宁问你是否也改对照表的这一列。")
                    if any(m["id"] == "om_spec_owner" for m in replies):
                        finishing = root / "hold-spec-new-result"
                        if finishing.exists():
                            (root / "spec-new-result-has-input").touch()
                            while finishing.exists():
                                time.sleep(0.01)
                        m = next(m for m in replies if m["id"] == "om_spec_owner")
                        assert m["source"]["author_id"] == context["owner"]["id"] and m["sender"] == "本人"
                        overview, disposition = "你已确认用毫秒，王文负责改表。", "answered"
                        current_ids = ["om_spec_owner", "om_spec_handover"]
                        questions = []
                    else:
                        overview, disposition = "王文负责改表，单位仍等你确认。", "requested_of_you"
                        current_ids = ["om_spec_handover"]
                        questions = [] if "om_spec_table" in request_ids else ["这是对照表的哪一版？"]
                        question_basis = "direct_reference_unavailable"
                    if (root / "fixture-unsupported-completion").exists():
                        overview, disposition = "另一项请求已验收。", "completed"
                        current_ids = ["om_spec_sibling"]
            elif anchor == "om_spec_fyi":
                headline, request_copy = "新手册已发", "宋青向你同步新手册已经发出。"
                overview, disposition, questions = "当前未见需要你操作。", "for_information", []
            elif anchor == "om_spec_demo":
                headline, request_copy = "周二演示时间", "宋青问你能否确认周二的演示时间。"
                overview, disposition, questions = "现在等你确认时间。", "requested_of_you", []
            else:
                headline, overview, questions, disposition = "补齐另一个发布话题", "原消息没有足够材料支持行动。", [], "unclear"
                request_copy = "顾宁发来一条需核实的发布消息。"
            with (root / "assessed.jsonl").open("a", encoding="utf-8") as log:
                log.write(json.dumps([anchor, context["revision"], [reply["id"] for reply in replies]]) + "\n")
            response = {"headline": headline, "card": {
                "request": request_copy, "current": overview, "relationship": disposition,
                "request_ids": request_ids, "current_ids": current_ids,
                "question": {"text": questions[0], "basis": question_basis} if questions else None,
            }}
            retain("fixture_answer", response)
            if (root / "system-meter-fixture").exists():
                assert params["threadId"] == thread
                for _ in range(2):
                    print(json.dumps({"method": "rawResponse/completed", "params": {
                        "threadId": thread, "turnId": turn, "responseId": "fixture-feishu-response",
                        "usage": {"totalTokens": 1220, "inputTokens": 1000, "outputTokens": 220},
                    }}), flush=True)
            item = {"type": "agentMessage", "id": turn + "-answer", "phase": "final_answer",
                    "text": json.dumps(response, ensure_ascii=False)}
            print(json.dumps({"method": "item/completed", "params": {"threadId": thread, "turnId": turn, "item": item}},
                             ensure_ascii=False), flush=True)
            print(json.dumps({"method": "turn/completed", "params": {
                "threadId": thread, "turn": {"id": turn, "status": "completed", "items": [item]}}},
                ensure_ascii=False), flush=True)
        case _:
            raise AssertionError(request)
