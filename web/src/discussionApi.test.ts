import { afterEach, expect, it, vi } from "vitest";
import { listDiscussions, setDiscussionArchived } from "./discussionApi";
import { beginSession } from "./webSession";

afterEach(() => vi.unstubAllGlobals());

it("lists active and archived discussions and sends the same desired state as the CLI", async () => {
  const fetch = vi.fn().mockImplementation(async () => Response.json({}));
  vi.stubGlobal("fetch", fetch);
  const end = beginSession(() => {});
  const signal = new AbortController().signal;
  try {
    await listDiscussions(false, signal);
    await listDiscussions(true, signal);
    await setDiscussionArchived("fixture", true);
    await setDiscussionArchived("fixture", false);

    expect(fetch.mock.calls.map(([url]) => url)).toEqual([
      "/api/v1/discussions",
      "/api/v1/discussions?archived=true",
      "/api/v1/discussions/fixture",
      "/api/v1/discussions/fixture",
    ]);
    const writes = fetch.mock.calls.slice(2).map(([, request]) => ({
      method: request.method,
      body: JSON.parse(request.body),
      web: request.headers.get("X-Tend-Web"),
    }));
    expect(writes).toEqual([
      { method: "PATCH", body: { archived: true }, web: "1" },
      { method: "PATCH", body: { archived: false }, web: "1" },
    ]);
  } finally {
    end();
  }
});
