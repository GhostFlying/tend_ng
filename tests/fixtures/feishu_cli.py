"""Local lark-cli contract fixture. No provider account or network."""
from datetime import datetime
import json
import os
from pathlib import Path
import sys

root = Path(__file__).parent
state = json.loads((root / "feishu-scenario.json").read_text())
args = sys.argv[1:]
assert os.environ.get("TZ") == "UTC" and not os.environ.get("TEND_TOKEN")
assert args[-2:] == ["--profile", "fixture-user"]
args = args[:-2]
owner = "ou_fixture_owner"
with (root / "feishu-cli-calls.jsonl").open("a") as log:
    log.write(json.dumps(args) + "\n")


def arg(flag):
    return args[args.index(flag) + 1]


def msg(key, chat, _name, text, at, *, mentions=None, author="林青", sender="ou_colleague", thread=None,
        reply_to=None, position=120, quote=None):
    output = {
        "message_id": f"om_{key}", "chat_id": f"oc_{chat}",
        "body": {"content": json.dumps({"text": text}, ensure_ascii=False)}, "msg_type": "text",
        "create_time": str(at * 1000), "update_time": str(at * 1000), "updated": False,
        "deleted": False, "message_position": str(position),
        "mentions": [
            {**m, "id": m["id"] if isinstance(m["id"], dict) else {"open_id": m["id"]}}
            for m in (mentions or [])
        ],
        "sender": {"id": sender, "id_type": "open_id", "sender_name": author, "sender_type": "user"},
    }
    if quote:
        output["quoted_message_id"] = f"om_{quote}"
    if reply_to:
        output["parent_id"] = f"om_{reply_to}"
    if thread:
        output["thread_id"] = f"omt_{thread}"
        output["thread_message_position"] = str(position)
        # Raw topic replies can carry both the topic and the parent/root.
        output["root_id"] = f"om_{thread}_parent" if reply_to else output["message_id"]
    return output


def page(messages, *, more=False, token=""):
    data = {"items": messages, "has_more": more, "page_token": token}
    print(json.dumps({"ok": True, "identity": "user", "data": data}, ensure_ascii=False))


epoch = state["at"]
mark = [{"id": owner, "key": "@_user_1", "name": "本人"}]
qa = msg("qa_anchor", "qa", "移动端发布协作", "@你 今天能确认 Android 灰度窗口吗？先沿用 5%，需要补上回滚阈值。",
         (epoch - 1800) // 60 * 60 + 30, mentions=mark, thread="qa", reply_to="qa_parent")
qa_parent = msg("qa_parent", "qa", "移动端发布协作", "Android 发布排期，今天要决定是否继续 5% 灰度。",
                epoch-1900, sender="ou_schedule", author="沈言", thread="qa", position=105)
qa_reply = msg("qa_reply", "qa", "移动端发布协作", "候选包已通过冒烟测试。灰度比例先维持 5%，回滚阈值还在等 QA 答复。",
               (epoch - 1800) // 60 * 60 + 45, author="沈言", thread="qa", position=124)
docs = msg("docs_anchor", "docs", "研发体验", "@你 能看一下新文档的本地启动流程吗？", (epoch - 3000) // 60 * 60 + 30, mentions=mark, author="周韵")
docs_reply = msg("docs_reply", "docs", "研发体验", "本地启动步骤已补齐，示例经两位同学核对，文档项可以关闭。",
                 (epoch - 3000) // 60 * 60 + 45, reply_to="docs_anchor", author="周韵", position=337)
old = msg("too_old", "skip", "超期", "存储期以前的材料", epoch - 8 * 86400 + 3600, mentions=mark)
mention_variant = [{"id": {"open_id": owner}, "key": "@_user_1", "name": "本人"}]
release = msg("release_anchor", "release", "发布支持", "@你 周五发布看板还缺一个负责人，请问这次谁确认？",
              epoch - 4200, mentions=mention_variant, author="顾宁")
release2 = msg("release_update", "release", "发布支持", "@你 已经确认发布窗口，负责人还需要明确。",
               epoch - 3600, mentions=mark, author="顾宁", position=122, reply_to="release_anchor")
release3 = msg("release_final", "release", "发布支持", "@你 顾宁继续跟进负责人。",
               epoch - 120, mentions=mark, author="顾宁", position=123, reply_to="release_parent")
all_only = msg("all", "qa", "移动端发布协作", "@全员 不应进入建议", epoch - 1300,
               mentions=[{"id": "all", "name": "所有人"}])
self_post = msg("self", "qa", "移动端发布协作", "@你 不应采集本人发言", epoch - 1500, mentions=mark, sender=owner)
name_only = msg("nameonly", "qa", "移动端发布协作", "仅文字 @你 无结构化提及", epoch - 800)
other_topic = msg("other_topic", "qa", "移动端发布协作", "@你 另一个话题不应归入灰度",
                  epoch - 5400, mentions=mark, thread="qa_other", position=101)
messages = [qa, docs, old, release, release2, all_only, self_post, name_only, other_topic]
if state.get("progress"):
    messages.append(release3)
reference = msg("spec_table", "spec", "新版对照表讨论", "周四版，耗时单位从秒改成毫秒。", epoch-700,
                author="小林", sender="ou_other_same_name", position=600)
spec = msg("spec_ask", "spec", "新版对照表讨论", "@_user_1 这一列也要改吗？", epoch-600,
           mentions=[{"id": owner, "key": "@_user_1", "name": "小林"}], author="陈宁", sender="ou_chen", position=604,
           reply_to="spec_table", quote="spec_table")
fyi = msg("spec_fyi", "spec", "新版对照表讨论", "@小林 新手册已发，供参考。", epoch-660, mentions=mark,
          author="宋青", sender="ou_song", position=601)
other = msg("spec_demo", "spec", "新版对照表讨论", "@小林 能否确认周二的演示时间？", epoch-590,
            mentions=mark, author="宋青", sender="ou_song", position=605)
boundary_target = msg("spec_private", "skip", "范围外的会话", "不应进入任一研判", epoch-660,
                      position=432)
spec["quoted_message_ids"] = ["om_spec_private", "om_spec_unknown_quote"]
unknown_quote = msg("spec_unknown_quote", "spec", "新版对照表讨论", "不能核实引用的版本", epoch-690)
unknown_quote["updated"] = True
unknown_quote.pop("update_time")
sibling = msg("spec_sibling", "spec", "新版对照表讨论", "这一行我验收完了（另一项请求）。", epoch-490,
              reply_to="spec_table", position=608)
unknown_edit = msg("spec_unknown_edit", "spec", "新版对照表讨论", "编辑时刻未知，不能用这句下结论。", epoch-260,
                   reply_to="spec_ask", position=619)
unknown_edit["updated"] = True
unknown_edit.pop("update_time")
handover = msg("spec_handover", "spec", "新版对照表讨论", "表我来改，单位还是请你定。", epoch-500,
               author="王文", sender="ou_wang", position=606, reply_to="spec_ask")
owner_reply = msg("spec_owner", "spec", "新版对照表讨论", "单位用毫秒。", epoch-300,
                  author="小林", sender=owner, position=610, reply_to="spec_ask")
extra_before = msg("spec_version", "spec", "新版对照表讨论", "上面那份周四版才是本周确认的对照表。",
                   epoch-650, author="林澜", sender="ou_lan", position=602)
future = msg("spec_future", "spec", "新版对照表讨论", "未到截止时的判断", epoch+3600,
             author="小林", sender=owner, position=620, reply_to="spec_ask")
future_edit = msg("spec_future_edit", "spec", "新版对照表讨论", "截止后才写入的内容", epoch-250,
                  author="小林", sender=owner, position=621, reply_to="spec_ask")
future_edit["update_time"] = str((epoch+3600) * 1000)
future_edit["updated"] = True
spec_noise = msg("spec_noise", "spec", "新版对照表讨论", "只是一条同群邻近材料。", epoch-480,
                 position=607)
if state.get("stage") == "rename":
    spec["mentions"][0]["name"] = "林星"
    spec["sender"]["sender_name"] = "陈宁（新显示名）"
if state.get("stage") == "withdrawn":
    spec["deleted"] = True
if state.get("stage") == "edited":
    spec["body"]["content"] = json.dumps({"text": "@_user_1 这一列也要改吗？验收表也用同一单位。"})
    spec["update_time"] = str((epoch-220)*1000)
    spec["updated"] = True
if state.get("relationship"):
    messages.extend([spec, fyi, other])

edges = state.get("edges")
edge_names = {
    "oc_transition": "灰度话题切换",
    "oc_scantransition": "采集命中的话题切换",
    "oc_roles": "同名分工",
    "oc_chain": "五层验收回复",
    "oc_promotion": "跨页核实验收回复",
}
transition_ids = ("transition", "scantransition")
threaded = edges in ("topic_empty", "oldedit", "new")
expansion_topics = {
    "expandpos": ("done", "other_thread", "other_position", "future"),
    "expandmeta": ("done", "bad_root", "bad_link", "edited_ms"),
    "expandreply": ("done", "bad_parent", "bad_quote", "bad_body"),
}
edge_names.update({f"oc_{key}": "同秒验收补读" for key in expansion_topics})
edge_messages = {}
edge_replies = {}
if edges:
    for key in transition_ids:
        thread = key if threaded else None
        anchor = msg(f"{key}_anchor", key, "", "@你 请跟进灰度验收。", epoch-760,
                     mentions=mark, thread=thread, position=20, author="沈言", sender="ou_shen",
                     quote=f"{key}_reference" if thread else None)
        edge_messages[anchor["message_id"]] = anchor
        reference = msg(f"{key}_reference", key, "", "验收前先核对本次灰度版本。", epoch-800, position=18)
        edge_messages[reference["message_id"]] = reference
        old = msg(f"{key}_old_group", key, "", "@你 旧群聊验收已经完成。", epoch-660,
                  mentions=mark, reply_to=f"{key}_anchor", position=21)
        if edges in ("oldedit", "new"):
            old["body"]["content"] = json.dumps({"text": "@你 旧群聊验收补充了情况。"})
            old["updated"] = True
            old["update_time"] = str((epoch-72)*1000)
        messages.append(old)
        reply = msg(f"{key}_done", key, "", "这项灰度验收已经通过。", epoch-500,
                    reply_to=f"{key}_anchor", thread=thread, position=21)
        if thread:
            reply["root_id"] = anchor["message_id"]
        edge_replies[key] = [reply]
        # transition tests exact-only routing. The other tests new search
        # hydration of a message whose source ID was already recorded.
        if not threaded or key != "transition":
            messages.append(anchor)
    same_name = "小林" if edges == "initial" else "林星"
    role_ids = [owner, "ou_same_name"]
    if edges == "new":
        role_ids.reverse()
    roles = msg("roles_anchor", "roles", "", "@_user_1 负责发版，@_user_2 负责审批。", epoch-700, position=20,
                author="顾宁", sender="ou_gu",
                mentions=[{"id": id, "key": f"@_user_{i}", "name": same_name}
                          for i, id in enumerate(role_ids, 1)])
    if edges == "new":
        roles["updated"] = True
        roles["update_time"] = str((epoch-60)*1000)
    messages.append(roles)
    for key in ("chain", "promotion"):
        anchor = msg(f"{key}_anchor", key, "", "@你 请跟进这一轮验收。", epoch-680, mentions=mark, position=20, author="顾宁", sender="ou_gu")
        messages.append(anchor)
        edge_replies[key] = [
            msg(f"{key}_{i}", key, "", "验收通过，确认关闭这项请求。" if i == 5 else f"第 {i} 步验收跟进。",
                epoch-680+i*40, reply_to=f"{key}_{i-1}" if i > 1 else f"{key}_anchor", position=20+i)
            for i in range(1, 6)
        ]

if edges == "new":
    for key, variants in expansion_topics.items():
        at = epoch-710
        anchor = msg(f"{key}_anchor", key, "", "@你 请核对扩读验收结论。", at,
                     mentions=mark, thread=key, position=1, author="沈言", sender="ou_shen")
        edge_messages[anchor["message_id"]] = anchor
        messages.append(anchor)
        edge_replies[key] = []
        for n, kind in enumerate(variants, 9):
            reply = msg(f"{key}_{kind}", key, "", "已完成验收。" + "合成验收明细" * 240 +
                        "验收凭据：签字项齐全。", at, thread=key, position=n,
                        reply_to=f"{key}_anchor", quote=f"{key}_anchor")
            reply["root_id"] = anchor["message_id"]
            if kind == "edited_ms":
                reply["updated"] = True
                reply["update_time"] = str(at*1000+100)
            edge_replies[key].append(reply)

def seconds(value):
    return int(value) // 1000

if args == ["whoami"]:
    print(json.dumps({"brand": "feishu", "identity": "user", "available": True, "onBehalfOf": {
        "openId": "ou_other" if state["mode"] == "wrong-user" else owner,
        "userName": "林星" if state.get("stage") == "rename" else "小林",
    }}))
elif args[:3] == ["api", "POST", "/open-apis/im/v1/messages/search"]:
    raw = json.loads(arg("--data"))
    query = json.loads(arg("--params"))
    assert arg("--as") == "user" and raw["filter"]["is_at_me"] and raw["query"] == ""
    assert query["page_size"] == "50"
    time_range = raw["filter"]["time_range"]
    start = datetime.fromisoformat(time_range["start_time"]).timestamp()
    end = datetime.fromisoformat(time_range["end_time"]).timestamp()

    def search_page(messages, *, more=False, token=""):
        (root / "feishu-hydration.json").write_text(json.dumps(messages, ensure_ascii=False))
        items = [{"meta_data": {"message_id": m["message_id"], "chat_id": m["chat_id"],
                               "create_time": m["create_time"],
                               "update_time": m.get("update_time")}}
                 for m in messages]
        print(json.dumps({"ok": True, "identity": "user", "data": {
            "items": items, "has_more": more, "page_token": token}}))

    assert 0 <= end - start <= 86400 + 900
    if state["mode"] in ("failure", "gap") or state.get("fail_after") == "yes":
        if "page_token" in query:
            if state["mode"] == "failure":
                sys.exit(4)
            search_page([], more=True, token="fixture-cursor")
        else:
            problem = msg("failed_page", "skip", "不能提交的页", "@你 cursor 未完成", int(end)-90, mentions=mark)
            search_page([problem], more=True, token="fixture-cursor")
        sys.exit(0)
    if state["mode"] == "missing-detail":
        search_page([qa])
        sys.exit(0)
    if state["mode"] == "empty":
        search_page([])
        sys.exit(0)
    matched = [m for m in messages if start <= seconds(m["create_time"]) <= end and not m["deleted"]]
    if "page_token" in query:
        assert query["page_token"] == "fixture-cursor" and len(matched) > 1
        search_page(matched[1:])
    elif len(matched) > 1:
        search_page(matched[:1], more=True, token="fixture-cursor")
    else:
        search_page(matched)
elif args[:3] == ["api", "GET", "/open-apis/im/v1/messages/mget"]:
    query = json.loads(arg("--params"))
    assert arg("--as") == "user" and query["with_sender_name"] is True
    ids = set(query["message_ids"])
    stub = (root / "feishu-hydration.json")
    hydrated = json.loads(stub.read_text()) if stub.exists() else []
    all_messages = {m["message_id"]: m for m in messages + hydrated + [
        qa_reply, docs_reply, reference, boundary_target, extra_before, handover, spec_noise, unknown_quote,
    ]}
    all_messages.update(edge_messages)
    for replies in edge_replies.values():
        all_messages.update((m["message_id"], m) for m in replies)
    if (root / "fixture-qa-read-parent").exists():
        all_messages[qa_parent["message_id"]] = qa_parent
    if state.get("stage") in ("answered", "rename", "edited", "withdrawn"):
        all_messages[owner_reply["message_id"]] = owner_reply
    if state.get("stage") == "hold" and not (root / "fixture-spec-read-parent").exists():
        all_messages.pop(reference["message_id"], None)
    if state.get("stage") in ("rename", "withdrawn"):
        all_messages[spec["message_id"]] = spec
    # Provisional release parent is intentionally unavailable until the
    # next scanner pass. Explicit snapshots must not later be rewritten.
    if not state.get("provisional_only"):
        parent = msg("release_parent", "release", "发布支持", "先给负责人一个确认窗口。", epoch-240,
                     author="顾宁", reply_to="release_anchor", position=122)
        all_messages[parent["message_id"]] = parent
    if state.get("progress"):
        after = msg("qa_next", "qa", "移动端发布协作", "新进展：QA 要求验收设备覆盖 Android 12 和 15，阈值继续待确认。",
                    epoch-300, author="沈言", thread="qa", position=127)
        all_messages[after["message_id"]] = after
    selected = [dict(all_messages[id]) for id in sorted(ids) if id in all_messages]
    for row in selected:
        key = row["chat_id"].removeprefix("oc_")
        if key not in expansion_topics or row["message_id"] == f"om_{key}_anchor":
            continue
        kind = row["message_id"].removeprefix(f"om_{key}_")
        if kind == "done" and state.get("conflicting_batch_locator") and len(ids) > 1:
            # The thread list still proves slot 9; only the batched exact
            # response disagrees. Keep body, identity, timestamps and edges.
            chat = f"oc_{key}" if key != "expandmeta" else "oc_unrelated"
            link_position = 9 if key == "expandmeta" else 999
            if key == "expandreply":
                row["thread_message_position"] = str(link_position)
            row["message_app_link"] = ("https://applink.feishu.cn/client/thread/open?"
                                       f"openchatid={chat}&openthreadid=omt_{key}&thread_position={link_position}")
            with (root / "fixture-batch-locator-probes.jsonl").open("a") as log:
                log.write(json.dumps({
                    "id": row["message_id"], "batch": len(ids),
                    "position": row["thread_message_position"], "href": row["message_app_link"],
                }) + "\n")
            continue
        if kind == "done":
            row.pop("thread_message_position")
            # Native mget can retain only the chat's (different) position.
            row["message_position"] = "356"
            if key != "expandpos":
                row.pop("thread_id")
                row.pop("root_id")
            if key == "expandreply":
                row.pop("parent_id")
                row.pop("quoted_message_id")
        # Conflict probes exercise only the on-demand lookup, not background reads.
        if len(ids) != 1:
            continue
        if kind == "other_thread":
            row["thread_id"] = "omt_unrelated"
        elif kind == "other_position":
            row["thread_message_position"] = "999"
        elif kind == "future":
            row["updated"] = True
            row["update_time"] = str((epoch+3600)*1000)
        elif kind == "bad_root":
            row["root_id"] = "om_unrelated_root"
        elif kind == "bad_link":
            row["message_app_link"] = ("https://applink.feishu.cn/client/thread/open?"
                                       f"openchatid=oc_{key}&openthreadid=omt_unrelated&thread_position=9")
        elif kind == "edited_ms":
            row["update_time"] = str(int(row["update_time"]) + 1)
        elif kind == "bad_parent":
            row["parent_id"] = "om_unrelated_parent"
        elif kind == "bad_quote":
            row.pop("quoted_message_id")
            row["quoted_message_ids"] = []
        elif kind == "bad_body":
            row["body"] = {"content": json.dumps({"text": "更改验收结果"})}
    page([] if state["mode"] == "missing-detail" else selected)
elif args[:3] == ["im", "chats", "get"]:
    assert arg("--as") == "user"
    title = {"oc_qa": "移动端发布协作", "oc_docs": "研发体验", "oc_release": "发布支持",
             "oc_spec": "新版对照表讨论", **edge_names}[arg("--chat-id")]
    print(json.dumps({"ok": True, "identity": "user", "data": {"name": title}}, ensure_ascii=False))
elif args[:3] == ["api", "GET", "/open-apis/im/v1/messages"] and json.loads(arg("--params"))["container_id_type"] == "thread":
    query = json.loads(arg("--params"))
    assert arg("--as") == "user" and query["sort_type"] == "ByCreateTimeAsc"
    assert not {"start_time", "end_time"} & query.keys()
    if query["container_id"] == "omt_qa":
        before = msg("qa_before", "qa", "移动端发布协作", "这是锚点前的历史消息",
                     (epoch-1800)//60*60+15, thread="qa", position=119)
        ambiguous = msg("qa_ambiguous", "qa", "移动端发布协作", "无法核实是锚点之后",
                        seconds(qa["create_time"]), thread="qa", position=121)
        ambiguous.pop("thread_message_position")
        ambiguous.pop("message_position")
        rows = [before, qa, ambiguous, qa_reply]
        if state.get("progress"):
            rows.insert(0, msg("qa_next", "qa", "移动端发布协作", "新进展：QA 要求验收设备覆盖 Android 12 和 15，阈值继续待确认。",
                               epoch - 300, author="沈言", thread="qa", position=127))
        if state.get("big_payload"):
            rows.extend(msg(f"large_{i}", "qa", "移动端发布协作", "补充材料" * 300,
                            epoch-60+i, author="沈言", thread="qa", position=200+i)
                        for i in range(12))
        page(rows)
    elif query["container_id"].removeprefix("omt_") in expansion_topics:
        key = query["container_id"].removeprefix("omt_")
        assert edges == "new" and "page_token" not in query
        page([edge_messages[f"om_{key}_anchor"], *edge_replies[key]])
    elif query["container_id"].removeprefix("omt_") in transition_ids:
        key = query["container_id"].removeprefix("omt_")
        assert threaded and "page_token" not in query
        page([edge_messages[f"om_{key}_anchor"], *(edge_replies[key] if edges == "new" else [])])
    else:
        assert query["container_id"] == "omt_qa_other"
        earlier = msg("other_early", "qa", "移动端发布协作", "旧消息不能支撑新结论", epoch-5400,
                      thread="qa_other", position=100)
        unknown = msg("other_unknown", "qa", "移动端发布协作", "位置不明不能用作后续", epoch-5400,
                      thread="qa_other", position=101)
        unknown.pop("thread_message_position")
        unknown.pop("message_position")
        later = msg("other_later", "qa", "移动端发布协作", "位置已核实的后续", epoch-5400,
                    thread="qa_other", position=103)
        page([earlier, unknown, later])
elif args[:3] == ["api", "GET", "/open-apis/im/v1/messages"]:
    query = json.loads(arg("--params"))
    assert arg("--as") == "user" and query["start_time"] and query["container_id_type"] == "chat"
    assert query["only_thread_root_messages"] is True
    if query["container_id"] in edge_names:
        assert edges
        key = query["container_id"].removeprefix("oc_")
        token = query.get("page_token")
        if key in transition_ids:
            assert not threaded, "an accepted topic must not continue the old chat listing"
            n = int(token.removeprefix("old-chat-")) if token else 0
            page([], more=True, token=f"old-chat-{n+1}")
        elif key == "chain":
            page(edge_replies[key])
        elif key == "promotion" and query["page_size"] == "12":
            # A provider gap exposes a later result first, then provides its
            # earlier proof on the next page. Only send each body once.
            if not token:
                page(edge_replies[key][-1:], more=True, token="missing-chain")
            else:
                assert token == "missing-chain"
                page(edge_replies[key][:-1])
        else:
            page([])
    elif query["container_id"] == "oc_spec":
        if query["page_size"] == "12":
            if query["sort_type"] == "ByCreateTimeDesc":
                assert "page_token" not in query
                page([extra_before, reference, fyi, future])
            elif "page_token" in query:
                assert query["page_token"] == "spec-more"
                extra = [owner_reply] if state.get("stage") in ("answered", "rename", "edited", "withdrawn") else []
                page([future_edit, unknown_edit, future, *extra])
            else:
                page([spec_noise, sibling, handover, future], more=True, token="spec-more")
        else:
            stored = [fyi, other, spec, handover, future_edit, future, spec_noise, sibling, unknown_edit]
            if state.get("stage") in ("answered", "rename", "edited", "withdrawn"):
                stored.append(owner_reply)
            page(stored)
    elif query["container_id"] == "oc_docs":
        assert query["sort_type"] == "ByCreateTimeAsc"
        token = query.get("page_token", "")
        last = 5 if state.get("deep_context") else 2
        number = int(token.removeprefix("doc-page-")) if token else 1
        # On the fifth page C only refers to B on page two.
        noise = [msg(f"noise_{number}_{i}", "docs", "研发体验", "不相关的群聊",
                     (epoch-3000)//60*60+31+i//4 if number==1 else epoch-2400+i,
                     position=150+number*100+i) for i in range(49)]
        if number < last:
            seeds = [docs] if number == 1 else [docs_reply] if number == 2 else []
            page(seeds + noise, more=True, token=f"doc-page-{number+1}")
        elif last == 2:
            page([docs_reply])
        else:
            further = msg("docs_followup", "docs", "研发体验", "补充：验收样例已同步。", epoch-200,
                          reply_to="docs_reply", author="周韵", position=999)
            page([further])
    else:
        assert query["container_id"] == "oc_release"
        if state.get("progress"):
            parent = msg("release_parent", "release", "发布支持", "先给负责人一个确认窗口。", epoch - 240,
                         author="顾宁", position=122, reply_to="release_anchor")
            child = msg("release_final", "release", "发布支持", "@你 顾宁继续跟进负责人。", epoch-120,
                        mentions=mark, author="顾宁", position=123, reply_to="release_parent")
            further = msg("release_deleted", "release", "发布支持", "这段草稿后来撤回。", epoch-60,
                          author="顾宁", position=124, reply_to="release_final")
            if state.get("provisional_only"):
                # Before the original topic can see B, the child card can
                # still see a direct reply to its own anchor.
                since = int(query["start_time"])
                page([further] if since >= epoch-240 else [])
            else:
                further["deleted"] = True
                page([parent, child, further])
        else:
            page([])
else:
    raise AssertionError(f"unexpected command {args!r}")
