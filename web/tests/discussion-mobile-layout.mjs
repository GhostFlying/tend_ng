import assert from "node:assert/strict";
import { readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { chromium } from "playwright";

const chat = "layout-fixture";
const title = "整理讨论记录，如果标题很长也能一眼找到正在读的内容和转交入口";
const path = `/discussions?chat=${chat}`;
const api = "/api/v1";
const chatPath = `${api}/discussions/${chat}`;
const viewport = { width: 390, height: 844 };
const keyboard = { top: 32, height: 468 };
const draft = "先补充两点，再继续阅读。";
const detail = {
  chat_id: chat,
  title,
  updated_at: 1782345600,
  archived: false,
  origin: { label: "主 Chat 消息", href: "/#message-origin" },
  handoffs: [],
  execution_configured: true,
  references: [
    "读者首次打开讨论时可见的范围与补充说明",
    "嵌入式浏览器下输入状态的布局及验证记录",
    "讨论摘要与转交后的原始资料清单",
  ].map((label, id) => ({
    id: `fixture-reference-${id}`,
    kind: "issue",
    issue_id: `example-${id}`,
    status: "available",
    href: `/issues/example-${id}`,
    label,
  })),
};
const subjects = [
  "进入讨论",
  "查找资料",
  "保留原文",
  "阅读进度",
  "输入与回复",
  "回到主对话",
];
const messages = subjects.flatMap((subject, index) => [
  {
    id: `question-${index}`,
    author: "user",
    text: `请整理「${subject}」这一步要保留的内容。`,
    router_state: "completed",
    reply_to: null,
  },
  {
    id: `answer-${index}`,
    author: "discussion",
    text: `**${index + 1}. ${subject}**\n\n先说明适用场景，再给一个可以复用的例子。保留原始资料的入口，方便读者回到来源核对。\n\n下一步由读者确认。继续阅读时不要丢掉当前位置，转交时则明确哪些内容已经确认。`,
    reply_to: null,
  },
]);

// The measured interval is the intersection of the timeline scrollport and
// the visible viewport. Subtract any overlaid UI intersecting that interval.
function measure({ top, height }) {
  const bounds = (selector) => {
    const element = document.querySelector(selector);
    if (!element?.getClientRects().length) return null;
    const { top, bottom, left, right } = element.getBoundingClientRect();
    return { top, bottom, left, right };
  };
  const timeline = bounds("main.timeline");
  if (!timeline) return null;
  const viewportEnd = top + height;
  const start = Math.max(top, timeline.top);
  const end = Math.min(viewportEnd, timeline.bottom);
  const overlays = [
    ".content > .header",
    ".discussion-bar",
    ".dock",
    ".phone-nav",
  ]
    .map(bounds)
    .filter(Boolean)
    .map((region) => [
      Math.max(start, region.top),
      Math.min(end, region.bottom),
    ])
    .filter(([from, to]) => from < to)
    .sort(([from], [next]) => from - next);
  let covered = 0;
  let past = start;
  for (const [from, to] of overlays) {
    covered += Math.max(0, to - Math.max(past, from));
    past = Math.max(past, to);
  }
  const latest = bounds("#message-answer-5");
  const editor = bounds(".composer textarea");
  return {
    width: document.documentElement.clientWidth,
    viewport: [top, viewportEnd],
    timeline: [timeline.top, timeline.bottom],
    intersection: [start, end],
    covered,
    visible: Math.max(0, end - start) - covered,
    latestVisible: latest
      ? Math.max(0, Math.min(latest.bottom, end) - Math.max(latest.top, start))
      : 0,
    editorVisible: editor
      ? Math.max(
          0,
          Math.min(editor.bottom, viewportEnd) - Math.max(editor.top, top),
        )
      : 0,
    editorHeight: editor ? editor.bottom - editor.top : 0,
    editorBottom: editor?.bottom,
    dockBottom: bounds(".dock")?.bottom,
  };
}

async function serve(dist) {
  const host = "127.0.0.1";
  const index = await readFile(join(dist, "index.html"), "utf8");
  const server = createServer(async (req, res) => {
    const { pathname } = new URL(req.url, "http://local");
    const json = (value) => {
      res.setHeader("Content-Type", "application/json");
      res.end(JSON.stringify(value));
    };
    if (req.method !== "GET") {
      res.writeHead(405).end();
    } else if (
      pathname === `${api}/web/session` ||
      pathname === "/favicon.ico"
    ) {
      res.writeHead(204).end();
    } else if (pathname === `${api}/discussions`) {
      json({ discussions: [detail] });
    } else if (pathname === chatPath) {
      json(detail);
    } else if (pathname === `${chatPath}/handoff`) {
      json({ operation_id: "fixture-quick", request: null });
    } else if (pathname === `${chatPath}/draft`) {
      json({ draft_request: null });
    } else if (pathname === `${api}/chats/${chat}/messages`) {
      json({ messages, next_before: null });
    } else if (pathname === `${api}/chats/main/messages`) {
      json({ messages: [], next_before: null });
    } else if (pathname === "/discussions" || pathname === "/") {
      res.setHeader("Content-Type", "text/html; charset=utf-8");
      res.end(index);
    } else if (/^\/assets\/[\w.-]+\.(css|js)$/.test(pathname)) {
      const file = await readFile(join(dist, pathname)).catch(() => null);
      if (!file) return res.writeHead(404).end();
      res.setHeader(
        "Content-Type",
        pathname.endsWith(".css") ? "text/css" : "text/javascript",
      );
      res.end(file);
    } else {
      res.writeHead(404).end();
    }
  });
  await new Promise((resolve, reject) =>
    server.listen(0, host, resolve).once("error", reject),
  );
  return { server, url: `http://${host}:${server.address().port}${path}` };
}

async function scrollByWheel(page, delta) {
  const box = await page.locator("main.timeline").boundingBox();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.wheel(0, delta);
}

async function verify(browser, url, before) {
  const context = await browser.newContext({
    viewport,
    isMobile: true,
    hasTouch: true,
    locale: "zh-CN",
  });
  const page = await context.newPage();
  // Model the browser reporting OSK occlusion without changing the layout
  // viewport. The screenshot is clipped to that reported rectangle.
  await page.addInitScript(() => {
    const visual = globalThis.visualViewport;
    globalThis.layoutFixtureViewport = (height, top = 0, scale = 1) => {
      Object.defineProperties(visual, {
        height: { configurable: true, get: () => height },
        offsetTop: { configurable: true, get: () => top },
        scale: { configurable: true, get: () => scale },
      });
      visual.dispatchEvent(new Event("resize"));
      visual.dispatchEvent(new Event("scroll"));
    };
  });
  const simulate = (height, top, scale) =>
    page.evaluate(
      ([h, t, s]) => {
        globalThis.layoutFixtureViewport(h, t, s);
        return new Promise((resolve) =>
          globalThis.requestAnimationFrame(resolve),
        );
      },
      [height, top, scale],
    );
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (["error", "warning"].includes(message.type()))
      errors.push(message.text());
  });
  const output = process.env.TEND_SCREENSHOTS;
  const shoot = async (name, clip) => {
    if (output)
      await page.screenshot({
        path: join(output, `${before ? "before" : "after"}-${name}.png`),
        scale: "css",
        clip,
      });
  };
  try {
    await page.goto(url);
    assert.equal(page.url(), url);
    assert.equal(await page.title(), "Tend");
    await page.getByRole("heading", { name: title }).waitFor();
    await page.locator("#message-answer-5").waitFor();
    const frame = page.locator(".discussion-view");
    const timeline = page.locator("main.timeline");
    const nav = page.getByRole("navigation", { name: "手机导航" });
    const editor = page.getByRole("textbox", { name: "在讨论中发消息" });
    const reading = await page.evaluate(measure, {
      top: 0,
      height: viewport.height,
    });
    assert.equal(reading.width, viewport.width);
    assert.ok(
      await page.evaluate(
        () =>
          document.documentElement.scrollWidth ===
          document.documentElement.clientWidth,
      ),
    );
    await shoot("reading");

    await editor.evaluate((element) => {
      element.focus({ preventScroll: true });
    });
    await editor.fill(draft);
    await nav.waitFor();
    await simulate(keyboard.height, keyboard.top);
    if (!before)
      await frame.and(page.locator("[data-discussion-keyboard]")).waitFor();
    const typing = await page.evaluate(measure, keyboard);
    await shoot("keyboard", {
      x: 0,
      y: keyboard.top,
      width: viewport.width,
      height: keyboard.height,
    });
    assert.deepEqual(errors, []);
    if (before) return { reading, typing };

    await nav.waitFor({ state: "hidden" });
    assert.equal(typing.dockBottom, keyboard.top + keyboard.height);
    assert.equal(typing.editorVisible, typing.editorHeight);
    assert.ok(typing.latestVisible > 0);
    assert.ok(reading.editorHeight <= 44);
    const longDraft = draft + "\n" + draft.repeat(10);
    await editor.fill(longDraft);
    await page.waitForFunction(
      () => document.querySelector("textarea").offsetHeight > 44,
    );
    assert.ok((await editor.boundingBox()).height <= 110);
    await simulate(viewport.height, 0);
    await nav.waitFor();
    const restingEditor = (await editor.boundingBox()).height;
    await simulate(270, 0);
    await nav.waitFor({ state: "hidden" });
    assert.ok((await editor.boundingBox()).height < restingEditor);
    await simulate(viewport.height, 0);
    await nav.waitFor();
    assert.equal((await editor.boundingBox()).height, restingEditor);
    assert.equal(await editor.inputValue(), longDraft);
    assert.equal(
      await frame.evaluate((node) =>
        node.style.getPropertyValue("--discussion-visible-height"),
      ),
      "",
    );

    await editor.fill(draft);
    await page.setViewportSize({ width: 375, height: 667 });
    await simulate(667, 0);
    // Let the rotation's layout viewport settle before applying the keyboard.
    await page.waitForFunction(
      () => document.documentElement.clientWidth === 375,
    );
    await simulate(340, 0);
    await nav.waitFor({ state: "hidden" });
    await page.setViewportSize({ width: 667, height: 375 });
    await simulate(177, 0);
    await page.waitForFunction(
      () =>
        document.querySelector(".dock").getBoundingClientRect().bottom === 177,
    );
    const landscape = await page.evaluate(measure, { top: 0, height: 177 });
    assert.equal(landscape.editorVisible, landscape.editorHeight);
    assert.ok(landscape.visible > 0);
    // The reverse rotation also covers a host that resizes both viewports.
    await page.setViewportSize({ width: 375, height: 340 });
    await simulate(340, 0);
    await page.waitForFunction(
      () =>
        document.querySelector(".dock").getBoundingClientRect().bottom === 340,
    );
    await nav.waitFor({ state: "hidden" });
    await page.setViewportSize({ width: 375, height: 667 });
    await simulate(667, 0);
    await nav.waitFor();
    assert.equal(await editor.inputValue(), draft);
    await page.setViewportSize(viewport);
    await simulate(viewport.height, 0);
    await simulate(450, 0, 1.5);
    await nav.waitFor();
    await simulate(viewport.height, 0);

    await page.setViewportSize({ ...viewport, height: 500 });
    await simulate(500, 0);
    await nav.waitFor({ state: "hidden" });
    assert.equal(
      (await page.evaluate(measure, { top: 0, height: 500 })).dockBottom,
      500,
    );
    await page.setViewportSize(viewport);
    await simulate(viewport.height, 0);
    await nav.waitFor();
    await editor.blur();
    await simulate(keyboard.height, keyboard.top);
    await nav.waitFor();
    await simulate(viewport.height, 0);
    const references = page.getByRole("button", { name: /^引用资料/ });
    const list = page.getByRole("list", { name: "已附加资料" });
    await list.waitFor({ state: "hidden" });
    await references.tap();
    assert.equal(await list.getByRole("link").count(), 3);
    await references.tap();
    await list.waitFor({ state: "hidden" });
    const more = page.getByRole("button", { name: "更多讨论操作" });
    await more.focus();
    await more.press("Enter");
    await page.getByRole("button", { name: "改标题" }).press("Enter");
    assert.ok(
      await page
        .getByRole("textbox", { name: "讨论标题" })
        .evaluate((element) => element === document.activeElement),
    );
    await page.getByRole("button", { name: "取消", exact: true }).click();
    await page.getByRole("button", { name: "预览 / 编辑" }).click();
    await page.getByRole("dialog").waitFor();
    await page.getByRole("button", { name: "关闭转交草稿" }).click();
    await more.press("Escape");
    assert.equal(await more.getAttribute("aria-expanded"), "false");
    assert.ok(
      await more.evaluate((element) => element === document.activeElement),
    );
    await more.blur();

    const wheel = (delta) => scrollByWheel(page, delta);
    const collapsed = frame.and(page.locator("[data-discussion-collapsed]"));
    await timeline.evaluate((element) => {
      element.scrollTop = 0;
    });
    await wheel(36);
    await page.waitForFunction(
      () => document.querySelector("main.timeline").scrollTop >= 36,
    );
    assert.equal(
      await collapsed.count(),
      0,
      "avoid an uncompensated jump near the start",
    );
    const textY = async () =>
      (await page.locator("#message-answer-2").boundingBox()).y;
    const initial = await textY();
    await wheel(260);
    await collapsed.waitFor();
    assert.ok(
      Math.abs((await textY()) - initial + 260) <= 2,
      "collapse preserves text position",
    );
    const beforeNudge = await timeline.evaluate((element) => element.scrollTop);
    await wheel(-8);
    await page.waitForFunction(
      (top) => document.querySelector("main.timeline").scrollTop < top,
      beforeNudge,
    );
    await collapsed.waitFor();
    const anchor = await timeline.evaluate((element) => element.scrollTop);
    await editor.evaluate((element) => element.focus({ preventScroll: true }));
    await simulate(keyboard.height, keyboard.top);
    await nav.waitFor({ state: "hidden" });
    assert.equal(
      await timeline.evaluate((element) => element.scrollTop),
      anchor,
    );
    await simulate(viewport.height, 0);
    await nav.waitFor();
    assert.equal(
      await timeline.evaluate((element) => element.scrollTop),
      anchor,
    );
    assert.equal(await editor.inputValue(), draft);
    await editor.blur();
    await wheel(-96);
    await collapsed.waitFor({ state: "detached" });
    await wheel(96);
    await collapsed.waitFor();
    await page.getByRole("button", { name: "显示讨论工具栏" }).focus();
    await collapsed.waitFor({ state: "detached" });
    const back = page.getByRole("link", { name: "所有讨论" });
    assert.ok(
      await back.evaluate((element) => element === document.activeElement),
    );
    await back.tap();
    await page.waitForURL("**/discussions");
    await page
      .getByRole("heading", { name: "讨论", exact: true })
      .first()
      .waitFor();
    await nav.waitFor();
    await page.goto(new URL("/", url).href);
    await page.getByRole("textbox", { name: "发消息给 Tend" }).waitFor();
    await nav.waitFor();
    assert.deepEqual(errors, []);
    return { reading, typing };
  } finally {
    await context.close();
  }
}

async function verifyToolbarEdges(browser, url) {
  const context = await browser.newContext({
    viewport,
    isMobile: true,
    hasTouch: true,
  });
  try {
    const page = await context.newPage();
    const timeline = page.locator("main.timeline");
    const back = page.getByRole("link", { name: "所有讨论" });
    const collapsed = page.locator(
      ".discussion-view[data-discussion-collapsed]",
    );
    const apiPath = `**${api}/chats/${chat}/messages`;
    await page.route(apiPath, (route) =>
      route.fulfill({
        json: {
          messages: messages.map((message, index) =>
            index === 1
              ? {
                  ...message,
                  text: `${message.text}\n\n[跳到末条](#message-answer-5)`,
                }
              : message,
          ),
          next_before: null,
        },
      }),
    );
    await page.route(`**${chatPath}`, (route) =>
      route.fulfill({
        json: {
          ...detail,
          handoffs: [0, 1].map((id) => ({
            message_id: `fixture-${id}`,
            href: `/#message-fixture-${id}`,
          })),
        },
      }),
    );
    await page.goto(url);
    const link = page.getByRole("link", { name: "跳到末条" });
    await link.waitFor();
    await timeline.evaluate((element) => {
      element.scrollTop = 0;
    });
    await scrollByWheel(page, 11);
    await page.waitForFunction(
      () => document.querySelector("main.timeline").scrollTop === 11,
    );
    await link.click();
    await page.waitForURL(`${url}#message-answer-5`);
    await page.waitForFunction(
      () => document.querySelector("main.timeline").scrollTop > 260,
    );
    await page.evaluate(
      () => new Promise((resolve) => globalThis.requestAnimationFrame(resolve)),
    );
    assert.equal(
      await collapsed.count(),
      0,
      "a link activation must not inherit wheel intent",
    );

    await page.goto(url);
    await page.locator("#message-answer-5").waitFor();
    const history = page.locator(".discussion-handoff-history");
    await history.locator("summary").click();
    await history.getByRole("link").first().waitFor();
    await timeline.focus();
    await timeline.evaluate((element) => {
      element.scrollTop = 0;
    });
    await scrollByWheel(page, 260);
    await page.waitForFunction(
      () => document.querySelector("main.timeline").scrollTop >= 260,
    );
    assert.equal(
      await collapsed.count(),
      0,
      "open handoff history pins the toolbar",
    );
    await history.getByRole("link").first().waitFor();

    await page.unroute(`**${chatPath}`);
    await page.unroute(apiPath);
    await page.route(apiPath, (route) =>
      route.fulfill({
        json: { messages: messages.slice(-4), next_before: null },
      }),
    );
    await page.setViewportSize({ ...viewport, height: 550 });
    await page.goto(url);
    await page.locator("#message-answer-5").waitFor();
    await timeline.evaluate((element) => {
      element.scrollTop = 0;
    });
    await scrollByWheel(page, 260);
    await collapsed.waitFor();
    await page.setViewportSize(viewport);
    await back.waitFor();
    assert.equal(
      await collapsed.count(),
      0,
      "restore the toolbar when a short thread fits after resizing",
    );
  } finally {
    await context.close();
  }
}

async function verifyDesktopRename(browser, url) {
  const page = await browser.newPage({
    viewport: { width: 1280, height: 840 },
  });
  try {
    await page.goto(url);
    await page.getByRole("button", { name: "改标题" }).press("Enter");
    await page.keyboard.press("Tab");
    const name = page.getByRole("textbox", { name: "讨论标题" });
    assert.ok(
      await name.evaluate((element) => element === document.activeElement),
    );
  } finally {
    await page.close();
  }
}

async function verifyKeyboardResizeEdges(browser, url) {
  const context = await browser.newContext({
    viewport,
    isMobile: true,
    hasTouch: true,
  });
  async function focusEditor(page) {
    await page.goto(url);
    const editor = page.getByRole("textbox", { name: "在讨论中发消息" });
    await editor.fill(draft);
    return editor;
  }
  async function pinned(page, height) {
    await page.waitForFunction(
      (bottom) =>
        document.querySelector(".discussion-view[data-discussion-keyboard]") &&
        document.querySelector(".dock").getBoundingClientRect().bottom ===
          bottom,
      height,
      { timeout: 5000 },
    );
    const geometry = await page.evaluate(measure, { top: 0, height });
    assert.ok(geometry.visible > 0);
    assert.equal(geometry.editorVisible, geometry.editorHeight);
  }
  async function restored(page, editor) {
    await page
      .getByRole("navigation", { name: "手机导航" })
      .waitFor({ timeout: 5000 });
    assert.equal(await page.locator("[data-discussion-keyboard]").count(), 0);
    assert.equal(await editor.inputValue(), draft);
    assert.ok(await editor.evaluate((node) => node === document.activeElement));
  }
  async function coResizingPage() {
    const page = await context.newPage();
    await page.addInitScript(() => {
      Object.defineProperties(globalThis.visualViewport, {
        height: { get: () => globalThis.innerHeight },
        offsetTop: { get: () => 0 },
      });
    });
    return page;
  }
  try {
    const coResize = await coResizingPage();
    await coResize.setViewportSize({ width: 375, height: 667 });
    const firstEditor = await focusEditor(coResize);
    await coResize.setViewportSize({ width: 375, height: 340 });
    await pinned(coResize, 340);
    // Neither of these widths exactly matches the original layout height.
    await coResize.setViewportSize({ width: 664, height: 177 });
    await pinned(coResize, 177);
    await coResize.setViewportSize({ width: 650, height: 177 });
    await pinned(coResize, 177);
    await coResize.setViewportSize({ width: 375, height: 340 });
    await pinned(coResize, 340);
    await coResize.setViewportSize({ width: 664, height: 177 });
    await pinned(coResize, 177);
    await coResize.setViewportSize({ width: 664, height: 375 });
    await restored(coResize, firstEditor);

    const shortLandscape = await coResizingPage();
    await shortLandscape.setViewportSize({ width: 664, height: 300 });
    await shortLandscape.goto(url);
    await shortLandscape.getByRole("link", { name: "所有讨论" }).waitFor();
    assert.equal(
      await shortLandscape.evaluate(() => globalThis.visualViewport.height),
      300,
    );
    await shortLandscape.setViewportSize({ width: 375, height: 667 });
    await shortLandscape.evaluate(
      () => new Promise((resolve) => globalThis.requestAnimationFrame(resolve)),
    );
    const shortEditor = shortLandscape.getByRole("textbox", {
      name: "在讨论中发消息",
    });
    await shortEditor.fill(draft);
    await shortLandscape.setViewportSize({ width: 375, height: 340 });
    await pinned(shortLandscape, 340);
    await shortLandscape.setViewportSize({ width: 664, height: 177 });
    await pinned(shortLandscape, 177);
    await shortLandscape.setViewportSize({ width: 664, height: 300 });
    await restored(shortLandscape, shortEditor);
    await shortLandscape.getByRole("link", { name: "所有讨论" }).waitFor();

    const roundTrip = await context.newPage();
    await roundTrip.addInitScript(() => {
      const visual = globalThis.visualViewport;
      let occluded = false;
      Object.defineProperties(visual, {
        height: {
          get: () =>
            occluded
              ? Math.min(468, globalThis.innerHeight - 120)
              : globalThis.innerHeight,
        },
        offsetTop: { get: () => 0 },
      });
      globalThis.layoutFixtureKeyboard = (next) => {
        occluded = next;
        visual.dispatchEvent(new Event("resize"));
      };
    });
    const secondEditor = await focusEditor(roundTrip);
    const showKeyboard = (open) =>
      roundTrip.evaluate((value) => {
        globalThis.layoutFixtureKeyboard(value);
        return new Promise((resolve) =>
          globalThis.requestAnimationFrame(resolve),
        );
      }, open);
    const enterWide = async () => {
      await roundTrip.setViewportSize({ width: 844, height: 390 });
      await roundTrip.waitForFunction(
        () => !document.querySelector("[data-discussion-keyboard]"),
      );
      assert.ok(
        await secondEditor.evaluate((node) => node === document.activeElement),
      );
    };
    await showKeyboard(true);
    await pinned(roundTrip, 468);
    await enterWide();
    await roundTrip.setViewportSize(viewport);
    await pinned(roundTrip, 468);
    await showKeyboard(false);
    await restored(roundTrip, secondEditor);
    await showKeyboard(true);
    await pinned(roundTrip, 468);
    await enterWide();
    await showKeyboard(false);
    await roundTrip.setViewportSize(viewport);
    await restored(roundTrip, secondEditor);
  } finally {
    await context.close();
  }
}

const dist = process.argv[2] ?? new URL("../dist", import.meta.url).pathname;
const baseline = process.env.TEND_LAYOUT_BASELINE;
const browser = await chromium.launch({ channel: "chromium" });
const { server, url } = await serve(dist);
const base = baseline ? await serve(baseline) : null;
try {
  const after = await verify(browser, url, false);
  await verifyToolbarEdges(browser, url);
  await verifyDesktopRename(browser, url);
  await verifyKeyboardResizeEdges(browser, url);
  const before = base ? await verify(browser, base.url, true) : null;
  if (before) {
    assert.ok(after.reading.visible > before.reading.visible);
    assert.ok(after.typing.visible > before.typing.visible);
  }
  const results = JSON.stringify({ before, after }, null, 2);
  if (process.env.TEND_SCREENSHOTS)
    await writeFile(
      join(process.env.TEND_SCREENSHOTS, "geometry.json"),
      `${results}\n`,
    );
  console.log(results);
} finally {
  await browser.close();
  server.close();
  base?.server.close();
}
