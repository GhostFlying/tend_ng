import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { BoardIssue } from "./api";
import { Card } from "./IssueBoard";

const issue: BoardIssue = {
  id: "fixture-issue",
  short_number: 42,
  parent_issue_id: null,
  title: "示例卡片",
  issue_state: "cancelled",
  task_state: "running",
  stage: "cancelled",
  running: 2,
  repo_ids: ["demo-web", "demo-api"],
  attention: { chat_id: "main", question_message_id: "fixture-question" },
  pulls: [
    {
      number: 73,
      url: "https://github.com/example/demo-web/pull/73",
      facts: { state: "closed", draft: false, ci: "success" },
      observation_error: null,
    },
  ],
};

describe("board card status", () => {
  it("omits badges on cancelled cards but preserves their links and repositories", () => {
    const html = renderToStaticMarkup(<Card issue={issue} />);

    expect(html).not.toMatch(
      /board-card-state|board-attention|board-pull-status|待你决定|2 项运行中|已关闭|CI 通过/,
    );
    expect(html).toContain('href="/issues/42">#42 示例卡片</a>');
    expect(html).toContain("demo-web · demo-api");
    expect(html).toContain(
      'href="https://github.com/example/demo-web/pull/73"',
    );
    expect(html).toContain("example/demo-web #73");
  });

  it.each(["in_progress", "done"] as const)(
    "retains badges on %s cards with the same status facts",
    (stage) => {
      const html = renderToStaticMarkup(
        <Card
          issue={{
            ...issue,
            stage,
            issue_state: stage === "done" ? "closed" : "open",
          }}
        />,
      );

      expect(html).toContain("2 项运行中");
      expect(html).toContain('href="/#message-fixture-question">待你决定</a>');
      expect(html).toContain("已关闭");
      expect(html).toContain("CI 通过");
    },
  );
});
