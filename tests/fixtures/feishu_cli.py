"""Local lark-cli contract fixture. No provider account or network."""
from datetime import datetime, timezone
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


def now_label(epoch):
    return datetime.fromtimestamp(epoch, timezone.utc).strftime("%Y-%m-%d %H:%M")


def msg(key, chat, name, text, at, *, mentions=None, author="林青", sender="ou_colleague", thread=None,
        reply_to=None, position=120):
    output = {
        "message_id": f"om_{key}", "chat_id": f"oc_{chat}", "chat_name": name,
        "chat_type": "group", "content": text, "msg_type": "text", "create_time": now_label(at),
        "update_time": now_label(at), "deleted": False, "message_position": position,
        "mentions": mentions or [],
        "sender": {"id": sender, "id_type": "open_id", "name": author, "sender_type": "user"},
        "message_app_link": f"https://applink.feishu.cn/client/chat/open?openChatId=oc_{chat}&position={position}",
    }
    if thread:
        output["thread_id"] = f"omt_{thread}"
        output["message_app_link"] = ("https://applink.feishu.cn/client/thread/open?"
                                      f"openthreadid=omt_{thread}&openchatid=oc_{chat}&thread_position={position}")
    elif reply_to:
        output["reply_to"] = f"om_{reply_to}"
    return output


def page(messages, *, more=False, token="", total=None):
    data = {"messages": messages, "has_more": more, "page_token": token}
    if total is not None:
        data["total"] = total
    print(json.dumps({"ok": True, "identity": "user", "data": data}, ensure_ascii=False))


epoch = state["at"]
mark = [{"id": owner, "key": "@_user_1", "name": "本人"}]
qa = msg("qa_anchor", "qa", "移动端发布协作", "@你 今天能确认 Android 灰度窗口吗？先沿用 5%，需要补上回滚阈值。",
         epoch - 1800, mentions=mark, thread="qa")
qa_reply = msg("qa_reply", "qa", "移动端发布协作", "候选包已通过冒烟测试。灰度比例先维持 5%，回滚阈值还在等 QA 答复。",
               (epoch - 1800) // 60 * 60 + 45, author="沈言", thread="qa", position=124)
docs = msg("docs_anchor", "docs", "研发体验", "@你 能看一下新文档的本地启动流程吗？", epoch - 3000, mentions=mark, author="周韵")
docs_reply = msg("docs_reply", "docs", "研发体验", "本地启动步骤已补齐，示例经两位同学核对，文档项可以关闭。",
                 (epoch - 3000) // 60 * 60 + 45, reply_to="docs_anchor", author="周韵", position=337)
if args == ["whoami"]:
    print(json.dumps({"brand": "feishu", "identity": "user", "available": True, "onBehalfOf": {
        "openId": "ou_other" if state["mode"] == "wrong-user" else owner,
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
                               "create_time": (int(datetime.fromisoformat(m["create_time"] + "+00:00").timestamp()) + 30
                                               if m["message_id"] in ("om_docs_anchor", "om_qa_anchor")
                                               else m["create_time"]),
                               "update_time": m["update_time"]}}
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
    matched = [m for m in messages if start - 59 <= datetime.fromisoformat(m["create_time"] + "+00:00").timestamp() <= end]
    if "page_token" in query:
        assert query["page_token"] == "fixture-cursor" and len(matched) > 1
        search_page(matched[1:])
    elif len(matched) > 1:
        search_page(matched[:1], more=True, token="fixture-cursor")
    else:
        search_page(matched)
elif args[:2] == ["im", "+messages-mget"]:
    assert arg("--as") == "user" and "--no-reactions" in args
    hydrated = json.loads((root / "feishu-hydration.json").read_text())
    assert {m["message_id"] for m in hydrated} == set(arg("--message-ids").split(","))
    for m in hydrated:
        m.pop("chat_name")
    page([] if state["mode"] == "missing-detail" else hydrated, total=len(hydrated))
elif args[:3] == ["im", "chats", "get"]:
    assert arg("--as") == "user"
    title = {"oc_qa": "移动端发布协作", "oc_docs": "研发体验", "oc_release": "发布支持"}[arg("--chat-id")]
    print(json.dumps({"ok": True, "identity": "user", "data": {"name": title}}, ensure_ascii=False))
elif args[:2] == ["im", "+threads-messages-list"]:
    assert arg("--as") == "user" and arg("--order") == "asc"
    if arg("--thread") == "omt_qa":
        before = msg("qa_before", "qa", "移动端发布协作", "这是锚点前的历史消息",
                     epoch-1800, thread="qa", position=119)
        ambiguous = msg("qa_ambiguous", "qa", "移动端发布协作", "无法核实是锚点之后",
                        epoch-1800, thread="qa", position=121)
        ambiguous.pop("message_app_link")
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
    else:
        assert arg("--thread") == "omt_qa_other"
        earlier = msg("other_early", "qa", "移动端发布协作", "旧消息不能支撑新结论", epoch-5400,
                      thread="qa_other", position=100)
        unknown = msg("other_unknown", "qa", "移动端发布协作", "位置不明不能用作后续", epoch-5400,
                      thread="qa_other", position=101)
        unknown.pop("message_app_link")
        unknown.pop("message_position")
        later = msg("other_later", "qa", "移动端发布协作", "位置已核实的后续", epoch-5400,
                    thread="qa_other", position=103)
        page([earlier, unknown, later])
elif args[:2] == ["im", "+chat-messages-list"]:
    assert arg("--as") == "user" and arg("--start")
    assert arg("--order") == "asc"
    if arg("--chat-id") == "oc_docs":
        token = arg("--page-token") if "--page-token" in args else ""
        last = 5 if state.get("deep_context") else 2
        number = int(token.removeprefix("doc-page-")) if token else 1
        # Real formatting does not have root_id or parent_id for these
        # ordinary replies. On the fifth page C only refers to B on page two.
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
        assert arg("--chat-id") == "oc_release"
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
                since = datetime.fromisoformat(arg("--start")).timestamp()
                page([further] if since >= epoch-240 else [])
            else:
                further["deleted"] = True
                page([parent, child, further])
        else:
            page([])
else:
    raise AssertionError(f"unexpected command {args!r}")
