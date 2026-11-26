import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { chromium } from "playwright";

const {
  TEND_WEB_TEST_ORIGIN: origin,
  TEND_WEB_TEST_OTHER_ORIGIN: otherOrigin,
  TEND_WEB_TEST_LOOPBACK: loopback,
  TEND_TOKEN: token,
  TEND_SCREENSHOTS: screenshots,
} = process.env;
assert(origin && otherOrigin && loopback && token);
const allowed = [origin, otherOrigin, loopback];
assert(
  allowed.every((value) => new URL(value).hostname === "127.0.0.1"),
  "Run only against the isolated loopback fixture",
);

const root = await mkdtemp(join(tmpdir(), "tend-session-browser-"));
const key = "tend.token.v1";
const api = "/api/v1/web/session";
const protectedRoute = "/issues?session-fixture=1#board";
const pageErrors = [];
const options = {
  args: ["--no-proxy-server"],
  ignoreHTTPSErrors: true,
  serviceWorkers: "block",
  viewport: { width: 1440, height: 1000 },
};
let context;

const connect = (page) => page.getByRole("heading", { name: "连接 Tend" });
const disconnect = (page) =>
  page.getByRole("button", { name: "断开连接", exact: true });
const remember = (page) =>
  page.getByRole("checkbox", { name: "记住此浏览器（30 天）", exact: true });

async function launch(profile, viewport = options.viewport) {
  const ctx = await chromium.launchPersistentContext(join(root, profile), {
    ...options,
    viewport,
  });
  const monitor = (page) =>
    page.on("pageerror", (error) => pageErrors.push(error.name));
  ctx.on("page", monitor);
  ctx.pages().forEach(monitor);
  await ctx.route("**/*", (route) => {
    const request = route.request();
    const url = new URL(request.url());
    assert(allowed.includes(url.origin), "No non-fixture API or assets");
    const auth = request.headers().authorization;
    if (url.pathname === api && request.method() === "POST") {
      assert(auth?.startsWith("Bearer "));
      assert.equal(request.headers()["x-tend-web"], "1");
    } else {
      assert.equal(Boolean(auth), false);
    }
    return route.continue();
  });
  return ctx;
}

async function screenshot(page, name) {
  assert.equal(await page.title(), "Tend");
  assert.equal(await page.locator("vite-error-overlay").count(), 0);
  assert.equal(
    await page.evaluate(() => document.documentElement.scrollWidth),
    page.viewportSize().width,
  );
  if (screenshots) await page.screenshot({ path: join(screenshots, name) });
}

async function submit(page, value) {
  await page.getByLabel("访问令牌").fill(value);
  await page.getByRole("button", { name: "连接", exact: true }).click();
}

async function holdSessionProbe(page, ordinal) {
  // Hold a real response after receipt, and deliver it even if the caller aborts.
  await page.addInitScript(
    ({ endpoint, ordinal }) => {
      const original = globalThis.fetch.bind(globalThis);
      let probes = 0;
      globalThis.fetch = async (input, init) => {
        const read =
          input === endpoint && (!init?.method || init.method === "GET");
        if (read && ++probes === ordinal) {
          init?.signal?.addEventListener(
            "abort",
            () => (globalThis.heldProbeAborted = true),
            { once: true },
          );
          const response = await original(input, init);
          globalThis.heldProbeStatus = response.status;
          await new Promise(
            (release) => (globalThis.releaseHeldProbe = release),
          );
          return response;
        }
        return original(input, init);
      };
    },
    { endpoint: api, ordinal },
  );
}

async function waitForHeldProbe(page) {
  await page.waitForFunction(() => Boolean(globalThis.releaseHeldProbe));
  assert.equal(await page.evaluate(() => globalThis.heldProbeStatus), 204);
}

async function releaseHeldProbe(page) {
  await page.evaluate(async () => {
    globalThis.releaseHeldProbe();
    await new Promise((resolve) =>
      globalThis.requestAnimationFrame(() =>
        globalThis.requestAnimationFrame(resolve),
      ),
    );
  });
}

async function close() {
  if (context) await context.close();
  context = undefined;
}

try {
  context = await launch("remembered");
  let page = context.pages()[0];
  await page.goto(origin);
  await connect(page).waitFor();
  assert.equal(await remember(page).isChecked(), false);
  assert.equal(await remember(page).isEnabled(), true);
  await screenshot(page, "web-session-connect.png");

  await page.evaluate(
    (key) => sessionStorage.setItem(key, "legacy-fixture"),
    key,
  );
  await page.reload();
  await connect(page).waitFor();
  assert.equal(
    await page.evaluate((key) => sessionStorage.getItem(key), key),
    "legacy-fixture",
  );
  await submit(page, "invalid-fixture");
  await page.getByText("访问令牌无效", { exact: true }).waitFor();
  assert.equal(
    await page.evaluate((key) => sessionStorage.getItem(key), key),
    "legacy-fixture",
  );
  await remember(page).check();
  const earliestExpiry = Math.floor(Date.now() / 1000) + 2_592_000;
  await submit(page, token);
  await disconnect(page).waitFor();
  assert.deepEqual(
    await page.evaluate(
      (key) => [
        sessionStorage.getItem(key),
        localStorage.getItem(key),
        document.cookie.includes("tend-session"),
      ],
      key,
    ),
    [null, null, false],
  );
  const [remembered] = await context.cookies(origin);
  assert.equal(remembered.name, "__Host-tend-session");
  assert.equal(remembered.httpOnly && remembered.secure, true);
  assert.equal(remembered.path, "/");
  assert.equal(remembered.sameSite, "Lax");
  const latestExpiry = Math.ceil(Date.now() / 1000) + 2_592_000;
  assert(remembered.expires >= earliestExpiry);
  assert(remembered.expires <= latestExpiry);
  await page.getByLabel("发消息给 Tend").waitFor();
  await screenshot(page, "web-session-connected.png");

  await page
    .getByRole("navigation", { name: "主导航" })
    .getByRole("link", { name: "看板" })
    .click();
  await page.waitForURL(origin + "/issues");
  await disconnect(page).waitFor();
  await page.reload();
  await disconnect(page).waitFor();
  await page.goBack();
  await page.waitForURL(origin + "/");
  await disconnect(page).waitFor();
  await page.goForward();
  await page.waitForURL(origin + "/issues");
  await disconnect(page).waitFor();
  const independent = await context.newPage();
  await independent.goto(origin);
  await disconnect(independent).waitFor();
  await independent.close();
  const reopened = await context.newPage();
  await reopened.goto(origin);
  await disconnect(reopened).waitFor();

  await page.goto(otherOrigin);
  await connect(page).waitFor();
  assert.equal(
    await page.evaluate((key) => sessionStorage.getItem(key), key),
    null,
  );
  await page.goto(origin);
  await disconnect(page).waitFor();

  await close();
  context = await launch("remembered");
  page = await context.newPage();
  await page.goto(origin + protectedRoute);
  await disconnect(page).waitFor();
  const peer = await context.newPage();
  await peer.goto(origin);
  await disconnect(peer).waitFor();

  const opening = await context.newPage();
  const openingUrl = origin + "/more?session-fixture=1#startup";
  await holdSessionProbe(opening, 1);
  await opening.goto(openingUrl);
  await waitForHeldProbe(opening);
  const rechecked = opening.waitForResponse(
    (response) =>
      response.url() === origin + api &&
      response.request().method() === "GET" &&
      response.status() === 401,
    { timeout: 5000 },
  );
  await disconnect(page).click();
  await rechecked;
  await connect(opening).waitFor();
  assert.equal(await opening.evaluate(() => globalThis.heldProbeAborted), true);
  await releaseHeldProbe(opening);
  assert.equal(await connect(opening).isVisible(), true);
  assert.equal(await disconnect(opening).count(), 0);
  assert.equal(opening.url(), openingUrl);
  await opening.close();
  await Promise.all([connect(page).waitFor(), connect(peer).waitFor()]);
  assert.equal(page.url(), origin + protectedRoute);
  await peer.reload();
  await connect(peer).waitFor();
  await peer.close();
  assert.equal((await context.cookies(origin)).length, 0);
  console.log(
    "PASS same-tab history, refresh, new/reopened tabs, origin change, browser restart, and logout during both active and pending startup sessions",
  );

  // Start another session and verify non-authentication failures do not return
  // to the connection form. Revoke the same real cookie, bypassing the Web logout handler.
  await page.goto(origin);
  await submit(page, token);
  await disconnect(page).waitFor();
  for (const status of [403, 503, "offline"]) {
    const fail = (route) =>
      status === "offline"
        ? route.abort("internetdisconnected")
        : route.fulfill({ status, json: { error: "fixture request failure" } });
    await page.route("**/api/v1/chats/main/messages", fail);
    await page.getByText("连接中断", { exact: true }).waitFor();
    assert.equal(await disconnect(page).isVisible(), true);
    await page.unroute("**/api/v1/chats/main/messages", fail);
    await page.getByText("已连接", { exact: true }).waitFor();
  }
  await page.goto(origin + protectedRoute);
  await disconnect(page).waitFor();
  let polls = 0;
  page.on("request", (request) => {
    if (request.url().startsWith(origin + "/api/v1/issue-board")) polls++;
  });
  const revoke = await page.request.delete(origin + api, {
    headers: { Origin: origin, "X-Tend-Web": "1" },
  });
  assert.equal(revoke.status(), 204);
  await page
    .getByText("登录已失效，请重新连接 Tend。", { exact: true })
    .waitFor();
  assert.equal(page.url(), origin + protectedRoute);
  const atExpiry = polls;
  assert(atExpiry > 0, "At least one authenticated API poll saw expiry");
  await screenshot(page, "web-session-after-401.png");
  await new Promise((resolve) => setTimeout(resolve, 5300));
  assert.equal(polls, atExpiry);
  await submit(page, token);
  await disconnect(page).waitFor();
  assert.equal(page.url(), origin + protectedRoute);
  const [transient] = await context.cookies(origin);
  assert.equal(transient.expires, -1);
  await close();

  context = await launch("remembered");
  page = await context.newPage();
  await page.goto(origin);
  await connect(page).waitFor();
  console.log(
    "PASS invalid token, legacy storage, non-auth failures, real API 401, stopped polling, re-login and non-remembered restart",
  );

  // Serve only actual fixture assets at a synthetic remote HTTP origin. No
  // Chromium request goes to that hostname or any real remote endpoint.
  const remote = await context.newPage();
  const remoteUrl = "http://tend-web-fixture.invalid";
  const apiRequests = [];
  const bearerRequests = [];
  await remote.route(remoteUrl + "/**", async (route) => {
    const request = route.request();
    const { pathname } = new URL(request.url());
    if (request.headers().authorization) bearerRequests.push(pathname);
    if (pathname.startsWith("/api/")) {
      apiRequests.push(request.method());
      return route.fulfill({ status: 403 });
    }
    assert(request.method() === "GET");
    assert(pathname === "/issues" || pathname.startsWith("/assets/"));
    const response = await route.fetch({
      url: loopback + pathname,
      maxRedirects: 0,
    });
    await route.fulfill({ response });
  });
  await remote.goto(remoteUrl + "/issues");
  await remote
    .getByRole("heading", { name: "请使用 HTTPS 登录 Tend" })
    .waitFor();
  assert.equal(await remote.getByLabel("访问令牌").count(), 0);
  assert.equal(await remote.locator("form").count(), 0);
  assert.deepEqual(apiRequests, []);
  assert.deepEqual(bearerRequests, []);
  await screenshot(remote, "web-session-remote-http.png");
  await remote.close();
  console.log(
    "PASS remote HTTP shows HTTPS guidance without a form, login POST, or Authorization",
  );

  // Session storage and cookie storage are independent. Simulate each being unavailable.
  await page.route("**/api/v1/web/session", async (route) => {
    if (route.request().method() === "POST")
      return route.fulfill({ status: 204 });
    return route.continue();
  });
  await submit(page, token);
  await page.getByText(/请允许此站点使用 Cookie/).waitFor();
  await close();

  for (const channel of ["broadcast", "storage"]) {
    context = await launch("manual-" + channel);
    await context.addInitScript((channel) => {
      Object.defineProperty(globalThis.navigator, "locks", {
        value: undefined,
      });
      if (channel === "storage")
        Object.defineProperty(globalThis, "BroadcastChannel", {
          value: undefined,
        });
    }, channel);
    const owner = context.pages()[0];
    const manual = await context.newPage();
    const manualUrl = origin + "/more?session-fixture=1#manual-login";
    await holdSessionProbe(manual, 2);
    await owner.goto(origin);
    await manual.goto(manualUrl);
    await Promise.all([connect(owner).waitFor(), connect(manual).waitFor()]);
    assert.equal(
      await manual.evaluate(() => Boolean(globalThis.navigator.locks)),
      false,
    );
    assert.equal(
      await manual.evaluate(() => Boolean(globalThis.BroadcastChannel)),
      channel === "broadcast",
    );
    await manual.evaluate(
      (key) => sessionStorage.setItem(key, "legacy-fixture"),
      key,
    );
    await submit(owner, token);
    await disconnect(owner).waitFor();
    await submit(manual, token);
    await waitForHeldProbe(manual); // Server accepted B's Cookie before logout.

    await disconnect(owner).click();
    await connect(owner).waitFor();
    assert.equal((await manual.request.get(origin + api)).status(), 401);
    await manual.waitForFunction(
      () => globalThis.heldProbeAborted === true,
      null,
      { timeout: 5000 },
    );
    await releaseHeldProbe(manual);
    await manual.getByText("另一标签已退出，请重新连接 Tend。").waitFor();
    assert.equal(await connect(manual).isVisible(), true);
    assert.equal(await disconnect(manual).count(), 0);
    assert.equal(await manual.getByLabel("访问令牌").inputValue(), "");
    if (channel === "broadcast")
      await screenshot(manual, "web-session-manual-cancelled.png");
    assert.equal(manual.url(), manualUrl);
    assert.equal(
      await manual.evaluate((key) => sessionStorage.getItem(key), key),
      "legacy-fixture",
    );

    await submit(manual, token);
    await disconnect(manual).waitFor();
    assert.equal(
      await manual.evaluate((key) => sessionStorage.getItem(key), key),
      null,
    );
    await close();
    console.log(
      "PASS manual login without Web Locks ignores stale confirmation after cross-tab " +
        channel +
        " logout",
    );
  }

  context = await launch("mobile", { width: 390, height: 844 });
  page = context.pages()[0];
  await page.addInitScript(() => {
    Object.defineProperty(globalThis, "sessionStorage", {
      get() {
        throw new DOMException("fixture denied storage", "SecurityError");
      },
    });
  });
  await page.goto(loopback);
  await connect(page).waitFor();
  assert.equal(await remember(page).isDisabled(), true);
  await screenshot(page, "web-session-loopback-mobile.png");
  await submit(page, token);
  await page.getByText(/当前浏览器限制了站点存储/).waitFor();
  await page.getByLabel("发消息给 Tend").waitFor();
  const [local] = await context.cookies(loopback);
  assert.equal(local.name, "tend-loopback-session");
  assert.equal(local.secure, false);
  assert.equal(local.expires, -1);
  assert.deepEqual(pageErrors, []);
  console.log(
    "PASS mobile loopback, blocked Cookie and Web Storage recovery, no uncaught page errors",
  );
} finally {
  await close();
  await rm(root, { force: true, recursive: true });
}
