import { describe, expect, it } from "vitest";
import { mergeMessages } from "./api";
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
