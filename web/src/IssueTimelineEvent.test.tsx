import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";
import { DispatchAuthor, IssueTakeoverEvent } from "./IssueTimelineEvent";

it("labels an unnamed historical dispatch as coming from the main session", () => {
  const html = renderToStaticMarkup(
    <DispatchAuthor provenance={{ origin: "main_chat" }} />,
  );
  expect(html).toBe('<span class="author">来自主会话</span>');
});

it("displays only the stored name, with main-session provenance when applicable", () => {
  const main = renderToStaticMarkup(
    <DispatchAuthor provenance={{ origin: "main_chat", agent_name: "Tend" }} />,
  );
  const child = renderToStaticMarkup(
    <DispatchAuthor
      provenance={{ origin: "issue_delegation", agent_name: "coordinator" }}
    />,
  );
  expect(main).toContain("Tend");
  expect(main).toContain("来自主会话");
  expect(child).toBe('<span class="author">coordinator</span>');
});

it("does not label another Router Chat as the main session", () => {
  const html = renderToStaticMarkup(
    <DispatchAuthor provenance={{ origin: "other_chat" }} />,
  );
  expect(html).toBe('<span class="author">来自其他会话</span>');
});

it("renders a distinct, non-replyable takeover with the descendant Issue link", () => {
  const html = renderToStaticMarkup(
    <IssueTakeoverEvent
      message={{
        id: "event",
        kind: "takeover",
        issue_id: "child",
        short_number: 12,
        issue_title: "Child task",
        author: "worker",
        text: "",
        reply_to: null,
        run_id: null,
        source_message_id: null,
      }}
      descendant
    />,
  );
  expect(html).toContain("<strong>worker</strong> 已接手");
  expect(html).toContain('href="/issues/12"');
  expect(html).not.toContain("<button");
});
