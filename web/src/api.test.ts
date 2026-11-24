import { describe, expect, it } from "vitest";
import { issueLink, issueOverviewLink, issueTitle, mergeMessages } from "./api";
import type { Message } from "./api";

function message(id: string): Message {
  return {
    id,
    text: id,
    author: "router",
    reply_to: null,
    router_state: null,
    router_error: null,
  };
}

describe("chat pages", () => {
  it("updates repeated messages without moving or dropping other messages", () => {
    const prior = [message("first"), message("second")];
    const update = { ...message("second"), text: "updated" };
    expect(mergeMessages(prior, [update, message("third")])).toEqual([
      message("first"),
      update,
      message("third"),
    ]);
  });

  it("inserts an older page in order and retains the later observation of duplicates", () => {
    const latest = { ...message("second"), text: "updated" };
    expect(
      mergeMessages([latest], [message("first"), message("second")], true),
    ).toEqual([message("first"), latest]);
  });
});

describe("Issue references", () => {
  it("prefers the permanent short number for links and labels", () => {
    expect(issueLink("original-id", 42)).toBe("/issues/42");
    expect(issueOverviewLink("original-id", 42)).toBe("/issues/42/overview");
    expect(issueTitle("Example", 42)).toBe("#42 Example");
  });

  it("uses the original link and title when an older response has no number", () => {
    expect(issueLink("original-id")).toBe("/issues/original-id");
    expect(issueOverviewLink("original-id")).toBe(
      "/issues/original-id/overview",
    );
    expect(issueTitle("Example")).toBe("Example");
  });
});
