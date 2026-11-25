import { afterEach, describe, expect, it, vi } from "vitest";
import {
  allowsWebLogin,
  beginSession,
  loginSession,
  sessionFetch,
} from "./webSession";

afterEach(() => vi.unstubAllGlobals());

describe("Web requests", () => {
  it("sends cookies without Bearer, and requires the custom header on writes", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetch);
    const expired = vi.fn();
    const end = beginSession(expired);
    try {
      await sessionFetch("/api/v1/chats/main/messages");
      await sessionFetch("/api/v1/chats/main/messages", { method: "POST" });
      const [, read] = fetch.mock.calls[0];
      const [, write] = fetch.mock.calls[1];
      expect(read.headers.has("Authorization")).toBe(false);
      expect(read.headers.has("X-Tend-Web")).toBe(false);
      expect(write.headers.get("X-Tend-Web")).toBe("1");
      expect(write.headers.has("Authorization")).toBe(false);
      expect(write.credentials).toBe("same-origin");
      expect(expired).not.toHaveBeenCalled();
    } finally {
      end();
    }
  });

  it("notifies only the current session of 401, not 403, 503 or network errors", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(new Response(null, { status: 403 }))
      .mockResolvedValueOnce(new Response(null, { status: 503 }))
      .mockRejectedValueOnce(new TypeError("network error"))
      .mockResolvedValueOnce(new Response(null, { status: 401 }));
    vi.stubGlobal("fetch", fetch);
    const expired = vi.fn();
    const end = beginSession(expired);
    try {
      await expect(sessionFetch("/")).resolves.toHaveProperty("status", 403);
      await expect(sessionFetch("/")).resolves.toHaveProperty("status", 503);
      await expect(sessionFetch("/")).rejects.toThrow("network error");
      expect(expired).not.toHaveBeenCalled();
      await expect(sessionFetch("/")).rejects.toThrow("登录已失效");
      expect(expired).toHaveBeenCalledOnce();
    } finally {
      end();
    }
  });

  it("aborts outstanding requests and ignores old 401 after a new login", async () => {
    let respond: (response: Response) => void = () => {};
    const result = new Promise<Response>((resolve) => (respond = resolve));
    const fetch = vi.fn().mockReturnValue(result);
    vi.stubGlobal("fetch", fetch);
    const oldExpired = vi.fn();
    const newExpired = vi.fn();
    const endOld = beginSession(oldExpired);
    const request = sessionFetch("/");
    endOld();
    const endNew = beginSession(newExpired);
    try {
      expect(fetch.mock.calls[0][1].signal.aborted).toBe(true);
      respond(new Response(null, { status: 401 }));
      await expect(request).rejects.toHaveProperty("name", "AbortError");
      expect(oldExpired).not.toHaveBeenCalled();
      expect(newExpired).not.toHaveBeenCalled();
    } finally {
      endNew();
    }
  });

  it("admits HTTPS and only the explicit HTTP loopback hostnames", () => {
    for (const url of [
      "https://tend.example:8443",
      "http://localhost:8832",
      "http://127.0.0.1:8832",
      "http://[::1]:8832",
    ])
      expect(allowsWebLogin(new URL(url))).toBe(true);
    for (const url of [
      "http://tend.example",
      "http://localhost.example",
      "http://127.0.0.2:8832",
      "file:///tend/index.html",
    ])
      expect(allowsWebLogin(new URL(url))).toBe(false);
  });

  it("rejects remote plaintext login before sending a request", async () => {
    vi.stubGlobal("location", new URL("http://tend.example"));
    const fetch = vi.fn();
    vi.stubGlobal("fetch", fetch);
    await expect(loginSession("fixture", false)).rejects.toThrow("HTTPS 地址");
    expect(fetch).not.toHaveBeenCalled();
  });

  it("discards a successful cookie confirmation from a canceled login", async () => {
    vi.stubGlobal("location", new URL("https://tend.example"));
    vi.stubGlobal("navigator", {});
    const removeItem = vi.fn();
    vi.stubGlobal("sessionStorage", { removeItem });
    let respond: (response: Response) => void = () => {};
    const confirmation = new Promise<Response>(
      (resolve) => (respond = resolve),
    );
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(new Response(null, { status: 204 }))
      .mockReturnValueOnce(confirmation);
    vi.stubGlobal("fetch", fetch);

    const attempt = new AbortController();
    const result = loginSession("fixture", false, attempt.signal);
    await vi.waitFor(() => expect(fetch).toHaveBeenCalledTimes(2));
    const rejected = expect(result).rejects.toHaveProperty(
      "name",
      "AbortError",
    );
    attempt.abort();
    respond(new Response(null, { status: 204 }));
    await rejected;
    expect(removeItem).not.toHaveBeenCalled();
  });

  it("removes the legacy key only after a successful cookie round trip", async () => {
    vi.stubGlobal("location", new URL("https://tend.example"));
    vi.stubGlobal("navigator", {});
    const removeItem = vi.fn();
    vi.stubGlobal("sessionStorage", { removeItem });
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(new Response(null, { status: 401 }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }))
      .mockResolvedValueOnce(new Response(null, { status: 401 }))
      .mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetch);

    await expect(loginSession("invalid-fixture", false)).rejects.toThrow(
      "访问令牌无效",
    );
    await expect(loginSession("fixture", false)).rejects.toThrow(
      "请允许此站点使用 Cookie",
    );
    expect(removeItem).not.toHaveBeenCalled();
    await expect(loginSession("fixture", false)).resolves.toBe("");
    expect(removeItem).toHaveBeenCalledOnce();
    expect(removeItem).toHaveBeenCalledWith("tend.token.v1");
  });
});
