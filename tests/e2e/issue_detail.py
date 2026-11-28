"""Verify the unified Issue page with historical reports in a real Server SQLite fixture."""

import argparse
from contextlib import closing
import http.client
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile

from chat import server
from issue_chat import WEB

REPORT = """已交付 **两份评审原件**。请确认是否采用推荐方案。

1. 分开呈现上报与估算。
2. 区分未观测与零值。
3. 使用任务归属。
4. 保留续跑历史。
5. 用卡片展示缺口。
6. 先做只读验证。
"""
TEXT_ONLY_REPORT = "已完成 **纯文本核对**。结论直接写在报告正文中，无需附加文件。"
MARKDOWN = """# 用量方案原件

- 原始记录按工作归属。
- 保留 [参考链接](https://example.test/guide)。

| 数据来源与覆盖范围 | 归属与统计周期 | 计算口径与异常处置 | 当前阶段与后续验证 |
| --- | --- | --- | --- |
| Runner 原始上报与预估读数 | 同一个 Task 的所有已结算 Run | 重复回传只计算一次且单列缺口 | 先在临时数据集中核对并发和续跑 |

```text
runner.record.observed_total_for_active_rolling_window = compare_and_tag_explicit_zeros_in_original_receipts
```

<script>globalThis.issueMarkdownExecuted = true;</script>

<iframe src="https://example.test/raw"></iframe>

![外部图片](https://example.test/tracker.png)

[危险链接](javascript:alert%281%29) [内嵌数据](data:text/html,hi)
"""
HTML = b"<!doctype html><title>Original</title><script>globalThis.issueHtmlExecuted = true</script>"


def seed(root):
    with sqlite3.connect(root / "server.sqlite") as db:
        db.execute("PRAGMA foreign_keys = ON")
        db.executescript("""
            INSERT INTO chat_messages (seq, id, chat_id, text, operation_id) VALUES
                (1, 'review-request', 'work', '请交付方案与设计稿以便评审。', 'submit-request');
            INSERT INTO issues (id, source_seq, title, objective, state, parent_issue_id) VALUES
                ('historical-parent', 1, '用量统计方案与设计稿', '明确证据来源，提交方案和可审阅的设计稿。', 'closed', NULL),
                ('plan-child', 1, '提交方案文档', '整理统计口径', 'closed', 'historical-parent'),
                ('design-child', 1, '提交交互设计稿', '修改交互版式', 'closed', 'historical-parent'),
                ('empty-issue', 1, '没有已回传报告的工作', '', 'open', NULL);
            INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities) VALUES
                ('parent-task', 'historical-parent', 'issue-router', 'completed', '["tend_ng"]', '[]'),
                ('plan-task', 'plan-child', 'implementation', 'waiting', '[]', '[]'),
                ('design-task', 'design-child', 'implementation', 'completed', '[]', '[]');
            INSERT INTO runners (id, host_id, profile, last_seen) VALUES ('runner', 'local', '{}', 0);
            INSERT INTO runner_operations (id, runner_id, operation) VALUES
                ('md-original', 'runner', '{}'), ('md-draft', 'runner', '{}'),
                ('html-original', 'runner', '{}'), ('real-report', 'runner', '{}'),
                ('real-attention', 'runner', '{}'), ('text-only', 'runner', '{}');
            INSERT INTO issue_reports VALUES
                ('real-report', 'historical-parent', '发布评审原件'),
                ('text-only', 'historical-parent', '回传纯文本核对结论');
            INSERT INTO router_inputs (chat_id, message_seq, report_id, outcome)
                VALUES ('work', 1, 'real-report', 'completed');
            INSERT INTO router_inputs (seq, chat_id, message_seq, outcome) VALUES
                (2, 'work', 1, 'completed');
            INSERT INTO router_attempts (seq, number, id, thread_id) VALUES
                (2, 1, 'dispatch-attempt', 'router-thread');
            INSERT INTO router_dispatches (operation_id, issue_id, task_id, attempt_id, call_id, request)
                VALUES ('dispatched', 'historical-parent', 'parent-task', 'dispatch-attempt', 'call', '{}');
            INSERT INTO chat_messages (seq, id, chat_id, reply_to, text) VALUES
                (2, 'review-report', 'work', 'review-request', ''),
                (3, 'review-decision', 'work', 'review-request', '是否按推荐方案进入实现？');
            INSERT INTO attentions (id, issue_id, task_id, source_seq, question_seq, request)
                VALUES ('real-attention', 'design-child', 'design-task', 1, 3, '{"question":"是否按推荐方案进入实现？"}');
            INSERT INTO router_inputs (chat_id, message_seq, attention_id, outcome)
                VALUES ('work', 1, 'real-attention', 'completed');
            INSERT INTO router_inputs (chat_id, message_seq, report_id, outcome)
                VALUES ('work', 1, 'text-only', 'completed');
        """)
        db.execute(
            "INSERT INTO chat_messages (id, chat_id, reply_to, text) VALUES ('text-only-report', 'work', 'review-request', ?)",
            (TEXT_ONLY_REPORT,),
        )
        db.execute("INSERT INTO issue_report_publications VALUES ('text-only-report', 'text-only')")
        files = [
            ("md-original", "plan-child", "plan-task", "usage-plan.md", MARKDOWN.encode()),
            ("html-original", "design-child", "design-task", "usage-design.html", HTML),
            ("md-draft", "plan-child", "plan-task", "usage-plan.md", b"A same-name work draft"),
        ]
        for file, issue, task, name, blob in files:
            db.execute(
                "INSERT INTO artifacts VALUES (?, ?, ?, 'runner', ?, ?, 1, 1)",
                (file, issue, task, name, len(blob)),
            )
            (root / "server.artifacts" / file).write_bytes(blob)
        for file in ("md-original", "html-original"):
            db.execute("INSERT INTO report_artifacts VALUES ('real-report', ?)", (file,))
            db.execute("INSERT INTO chat_artifacts VALUES ('review-report', ?)", (file,))
        db.execute("UPDATE chat_messages SET text = ? WHERE id = 'review-report'", (REPORT,))
        db.executemany(
            """INSERT INTO issue_messages (id, issue_id, kind, author, text)
               VALUES (?, 'historical-parent', 'agent', 'Issue Owner', ?)""",
            ((f"earlier-{i}", f"更早的记录 {i}") for i in range(50)),
        )
        db.execute("""
            INSERT INTO issue_messages (id, issue_id, kind, author, text, source_message_id) VALUES
                ('old-note', 'plan-child', 'agent', 'Plan Worker', '已核对历史运行记录，可直接回复这条工作备注。', 'review-request')
        """)
        db.executemany(
            """INSERT INTO issue_messages (id, issue_id, kind, author, text)
               VALUES (?, 'historical-parent', 'agent', 'Issue Owner', ?)""",
            ((f"history-{i}", f"工作过程记录 {i}") for i in range(110)),
        )
        db.executemany(
            "INSERT INTO chat_messages (id, chat_id, text) VALUES (?, 'work', ?)",
            ((f"chat-history-{i}", f"原 Chat 稍后的消息 {i}") for i in range(105)),
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server", type=lambda value: Path(value).resolve())
    args = parser.parse_args()
    token = "tend-issue-detail-fixture"
    with tempfile.TemporaryDirectory(prefix="tend-issue-detail-") as directory:
        root = Path(directory)
        env = {**os.environ, "CODEX_HOME": str(root / "fixture-codex"), "TEND_TOKEN": token}
        with server(args.server, root, env, web_dir=WEB / "dist") as address:
            seed(root)

            def get(path, authenticated=True):
                with closing(http.client.HTTPConnection(address, timeout=5)) as connection:
                    headers = {"Authorization": f"Bearer {token}"} if authenticated else {}
                    connection.request("GET", path, headers=headers)
                    response = connection.getresponse()
                    return response.status, response.read()

            assert get("/api/v1/issues/1/overview", False)[0] == 401
            assert get("/api/v1/artifacts/md-original", False)[0] == 401
            direct = json.loads(get("/api/v1/issues/1")[1])
            assert not direct.get("artifacts")
            status, overview = get("/api/v1/issues/1/overview")
            assert status == 200
            view = json.loads(overview)
            assert view["stage"] == "done"
            report, text_only = view["reports"]
            assert report["message_id"] == "review-report" and report.get("report_id") is None
            assert [a["id"] for a in report["artifacts"]] == ["md-original", "html-original"]
            assert report["artifacts"][1]["source"]["task_id"] == "design-task"
            assert text_only["message_id"] == "text-only-report" and text_only["report_id"] == "text-only"
            assert text_only["text"] == TEXT_ONLY_REPORT and text_only["artifacts"] == []
            assert view["decisions"][0]["message_id"] == "review-decision"
            assert json.loads(get("/api/v1/issues/4/overview")[1])["reports"] == []
            subprocess.run(
                ["node", WEB / "tests/issue-detail.mjs"], check=True, timeout=60,
                env={**env, "TEND_URL": f"http://{address}"},
            )
            with sqlite3.connect(root / "server.sqlite") as db:
                rows = db.execute(
                    "SELECT issue_id, viewed_issue_id, reply_to FROM issue_messages WHERE kind='user'"
                ).fetchall()
                assert rows == [("plan-child", "historical-parent", "old-note")] * 2, rows
                assert db.execute("SELECT state FROM issues WHERE id='historical-parent'").fetchone() == ("closed",)
    print("PASS exact text-only and legacy reports, authenticated downloads, decisions and reply destinations")


if __name__ == "__main__":
    main()
