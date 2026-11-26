import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { join } from "node:path";
import { chromium } from "playwright";
import { isAnonymousSessionProbe } from "./console-errors.mjs";

const {
  TEND_URL: base,
  TEND_TOKEN: token,
  TEND_TEST_CLI: cliPath,
  TEND_SCREENSHOTS: output,
  TEND_LEGACY_DISCUSSION: legacyDiscussion,
  TEND_EXPLICIT_DISCUSSION: explicitDiscussion,
  TEND_REF_ISSUE: referenceIssue,
  TEND_TARGET_ISSUE: targetIssue,
  TEND_MAIN_USER_MESSAGE: mainUserMessage,
  TEND_MAIN_TEND_REPLY: mainTendReply,
} = process.env;
const open = "先聊聊演示项目的快速上手说明。";
const errors = [];
const browser = await chromium.launch({ channel: "chromium" });

function cli(...args) {
  return JSON.parse(
    execFileSync(cliPath, ["discussion", ...args], { encoding: "utf8" }),
  );
}
function cliList(archived = false) {
  return cli("list", ...(archived ? ["--archived"] : [])).discussions;
}

async function visible(locator) {
  await locator.waitFor({ state: "visible" });
  return locator;
}

async function shoot(page, name) {
  const overflow = await page.evaluate(
    () =>
      document.documentElement.scrollWidth >
      document.documentElement.clientWidth,
  );
  assert.equal(overflow, false, `${name} has horizontal page overflow`);
  await page.screenshot({ path: join(output, `${name}.png`), scale: "css" });
}

async function createOne(page, activate) {
  const before = new Set(cliList().map(({ chat_id }) => chat_id));
  const previous = new URL(page.url()).searchParams.get("chat");
  await activate();
  await page.waitForURL(
    (url) =>
      url.pathname === "/discussions" &&
      url.searchParams.has("chat") &&
      url.searchParams.get("chat") !== previous,
  );
  const chat = new URL(page.url()).searchParams.get("chat");
  assert.ok(chat && !before.has(chat));
  assert.deepEqual(
    new Set(cliList().map(({ chat_id }) => chat_id)),
    new Set([...before, chat]),
  );
  return chat;
}

async function sendAcrossNavigation(
  page,
  chat,
  composer,
  text,
  leaveAndReturn,
) {
  const path = `${base}/api/v1/chats/${chat}/messages`;
  const accepted = Promise.withResolvers();
  const release = Promise.withResolvers();
  const posts = [];
  const handler = async (route) => {
    if (route.request().method() !== "POST") return route.continue();
    posts.push(route.request().postDataJSON());
    const response = await route.fetch();
    accepted.resolve(response.ok());
    await release.promise;
    await route.fulfill({ response });
  };
  await page.route(path, handler);
  try {
    await composer.fill(text);
    await composer.press("Enter");
    assert.equal(await accepted.promise, true);
    await leaveAndReturn();
    // The server has accepted the POST but the original composer did not
    // receive the receipt yet. Enter on the remounted composer must not resend.
    await visible(composer.and(page.locator(":read-only")));
    assert.equal(await composer.inputValue(), text);
    await composer.press("Enter");
    release.resolve();
    await page.waitForFunction(
      (label) =>
        document.querySelector(`textarea[aria-label="${label}"]`)?.value === "",
      await composer.getAttribute("aria-label"),
    );
    assert.equal(posts.length, 1);
    assert.equal(posts[0].text, text);
    assertOneUserMessage(chat, text);
  } finally {
    release.resolve();
    await page.unroute(path, handler);
  }
}

function assertOneUserMessage(chat, text) {
  const messages = JSON.parse(
    execFileSync(cliPath, ["chat", "read", chat], { encoding: "utf8" }),
  ).messages;
  assert.equal(
    messages.filter((row) => row.author === "user" && row.text === text).length,
    1,
  );
}

async function retryWithoutStorage(page, composer, leave, returnToChat) {
  const path = `${base}/api/v1/chats/main/messages`;
  const text = "Storage measurement · 丢失回执后沿原消息重试";
  const accepted = Promise.withResolvers();
  const release = Promise.withResolvers();
  const posts = [];
  const handler = async (route) => {
    if (route.request().method() !== "POST") return route.continue();
    posts.push(route.request().postDataJSON());
    if (posts.length > 1) return route.continue();
    const response = await route.fetch();
    accepted.resolve(response.ok());
    await release.promise;
    await route.abort("failed");
  };
  await page.evaluate(() => {
    const original = globalThis.Storage.prototype.setItem;
    globalThis.tendRestoreTestStorage = () => {
      globalThis.Storage.prototype.setItem = original;
    };
    globalThis.Storage.prototype.setItem = function (key, value) {
      if (
        this === globalThis.sessionStorage &&
        key === "tend.pending-message.v1"
      ) {
        throw new globalThis.DOMException(
          "Fixture storage unavailable",
          "QuotaExceededError",
        );
      }
      original.call(this, key, value);
    };
  });
  await page.route(path, handler);
  try {
    await composer.fill(text);
    await composer.press("Enter");
    assert.equal(await accepted.promise, true);
    await leave();
    const lost = page.waitForEvent(
      "requestfailed",
      (request) => request.method() === "POST" && request.url() === path,
    );
    release.resolve();
    await lost;
    await returnToChat();
    await visible(page.getByText(/浏览器未保存草稿/));
    assert.equal(await composer.inputValue(), text);
    const receipt = page.waitForResponse(
      (response) =>
        response.url() === path && response.request().method() === "POST",
    );
    await composer.press("Enter");
    assert.ok((await receipt).ok());
    await page.waitForFunction(
      () =>
        document.querySelector('textarea[aria-label="发消息给 Tend"]')
          ?.value === "",
    );
    assert.equal(posts.length, 2);
    assert.ok(posts[0].operation_id);
    assert.deepEqual(posts[1], posts[0]);
    assertOneUserMessage("main", text);
  } finally {
    release.resolve();
    await page.unroute(path, handler);
    await page.evaluate(() => globalThis.tendRestoreTestStorage());
  }
}

async function deferWrite(page, path, method, whileWaiting) {
  const started = Promise.withResolvers();
  const release = Promise.withResolvers();
  const done = Promise.withResolvers();
  const handler = async (route) => {
    if (route.request().method() !== method) return route.continue();
    started.resolve(route.request().postDataJSON());
    await release.promise;
    const response = await route.fetch();
    await route.fulfill({ response });
    done.resolve(response.ok());
  };
  await page.route(path, handler);
  try {
    await whileWaiting(started.promise);
    release.resolve();
    assert.equal(await done.promise, true);
  } finally {
    release.resolve();
    await page.unroute(path, handler);
  }
}

function traverseHistory(page, delta) {
  // A same-URL traversal is observable by popstate, not by waiting for a
  // different URL or a navigation response.
  return page.evaluate(
    (step) =>
      new Promise((resolve, reject) => {
        const controller = new globalThis.AbortController();
        const timer = setTimeout(() => {
          controller.abort();
          reject(new Error(`No popstate for history.go(${step})`));
        }, 5000);
        globalThis.addEventListener(
          "popstate",
          () => {
            clearTimeout(timer);
            resolve(globalThis.location.href);
          },
          { once: true, signal: controller.signal },
        );
        globalThis.history.go(step);
      }),
    delta,
  );
}

try {
  for (const [name, viewport] of Object.entries({
    desktop: { width: 1440, height: 960 },
    mobile: { width: 390, height: 844 },
  })) {
    const checkedGoal =
      name === "desktop"
        ? "只整理一页上手说明，先交草案，不部署。"
        : "先交一页上手说明草案，只覆盖安装与首次运行。";
    const context = await browser.newContext({
      viewport,
      isMobile: name === "mobile",
      hasTouch: name === "mobile",
      locale: "zh-CN",
      colorScheme: "light",
    });
    const page = await context.newPage();
    await page.addInitScript(() => {
      globalThis.tendTestDocument = globalThis.crypto.randomUUID();
    });
    const documentRequests = [];
    page.on("request", (request) => {
      if (request.resourceType() === "document")
        documentRequests.push(request.url());
    });
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (message) => {
      // Full Chromium probes /favicon.ico, which this Server does not serve.
      const faviconProbe =
        message.location().url === `${base}/favicon.ico` &&
        /\b401\b/.test(message.text());
      // The storage-failure case explicitly drops one successful POST receipt.
      const lostReceipt =
        message.location().url === `${base}/api/v1/chats/main/messages` &&
        message.text().endsWith("net::ERR_FAILED");
      if (
        ["error", "warning"].includes(message.type()) &&
        !isAnonymousSessionProbe(message) &&
        !faviconProbe &&
        !lostReceipt
      ) {
        errors.push(`${message.location().url} ${message.text()}`);
      }
    });
    await page.goto(base);
    const originalDocument = await page.evaluate(
      () => globalThis.tendTestDocument,
    );
    async function assertSameDocument() {
      assert.equal(
        await page.evaluate(() => globalThis.tendTestDocument),
        originalDocument,
      );
      assert.deepEqual(documentRequests, [`${base}/`]);
    }
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    const menu = page.getByRole("navigation", {
      name: name === "mobile" ? "手机导航" : "主导航",
    });
    const directory = menu.getByRole("link", { name: "讨论", exact: true });
    const chatNav = menu.getByRole("link", { name: "对话", exact: true });
    const activate = (link) => (name === "mobile" ? link.tap() : link.click());
    const navCreate = menu.getByRole("button", {
      name: "新建讨论",
      exact: true,
    });
    const main = page.getByRole("main");
    const userMessage = main.locator(`#message-${mainUserMessage}`);
    const tendReply = main.locator(`#message-${mainTendReply}`);
    await visible(userMessage.getByText("你", { exact: true }));
    await visible(tendReply.getByText("Tend", { exact: true }));
    assert.equal(
      await main.getByRole("button", { name: "带入讨论", exact: true }).count(),
      0,
      `${name} main Chat messages should not offer discussion creation`,
    );
    await userMessage.scrollIntoViewIfNeeded();
    await tendReply.scrollIntoViewIfNeeded();
    await shoot(page, `discussion-${name}-main-chat`);
    const beforeNavigation = cliList();
    if (name === "desktop") {
      // The shortcut works globally and doesn't wait for hover or the list.
      await visible(navCreate);
      await directory.click();
    } else {
      assert.equal(await navCreate.count(), 0);
      await directory.tap();
    }
    await page.waitForURL(`${base}/discussions`);
    assert.deepEqual(cliList(), beforeNavigation);
    assert.match(await page.title(), /Tend/i);
    await assertSameDocument();
    const currentList = main.getByRole("list", {
      name: "讨论列表",
      exact: true,
    });
    const sidebarList = menu.getByRole("list", { name: "最近讨论" });
    const create = (name === "desktop" ? menu : main).getByRole("button", {
      name: "新建讨论",
      exact: true,
    });
    const legacyHref = `/discussions?chat=${legacyDiscussion}`;
    const explicitHref = `/discussions?chat=${explicitDiscussion}`;
    const legacyLink = currentList.locator(`a[href="${legacyHref}"]`);
    assert.equal(
      await (await visible(legacyLink)).locator("strong").textContent(),
      "未命名讨论",
    );
    const explicitlyNamed = currentList.locator(`a[href="${explicitHref}"]`);
    assert.equal(
      await (await visible(explicitlyNamed)).locator("strong").textContent(),
      "新讨论",
    );
    assert.equal(
      await create.locator('[aria-hidden="true"]').textContent(),
      "+",
    );
    assert.equal(await create.getAttribute("aria-current"), null);
    if (name === "desktop") {
      await visible(sidebarList.locator(`a[href="${legacyHref}"]`));
      assert.equal(await sidebarList.getByRole("button").count(), 0);
      assert.equal(await sidebarList.locator("[aria-current]").count(), 0);
      assert.equal(await directory.getAttribute("href"), "/discussions");
      assert.equal(await directory.getAttribute("aria-current"), "page");
      assert.equal(
        await create.evaluate((button) => {
          const link = button.previousElementSibling;
          if (link?.tagName !== "A") return false;
          const a = link.getBoundingClientRect();
          const b = button.getBoundingClientRect();
          return a.right <= b.left && a.top === b.top && a.height === b.height;
        }),
        true,
        "the plus has its own target directly to the right of the link",
      );
      assert.equal(await create.textContent(), "+");
      assert.equal(await create.getAttribute("aria-label"), "新建讨论");
      await page.mouse.move(0, 0);
      const bounds = await create.boundingBox();
      assert.ok(bounds && bounds.width >= 40 && bounds.height >= 40);
      const background = () =>
        create.evaluate(
          (button) => globalThis.getComputedStyle(button).backgroundColor,
        );
      assert.equal(await background(), "rgba(0, 0, 0, 0)");
      assert.equal(
        await create.evaluate(
          (button) => globalThis.getComputedStyle(button).opacity,
        ),
        "1",
      );
      await create.hover();
      assert.notEqual(await background(), "rgba(0, 0, 0, 0)");
      await page.mouse.move(0, 0);
      await directory.focus();
      await page.keyboard.press("Tab");
      assert.equal(
        await create.evaluate((button) => {
          const style = globalThis.getComputedStyle(button);
          return (
            button.matches(":focus-visible") &&
            style.outlineStyle === "solid" &&
            parseFloat(style.outlineWidth) >= 2
          );
        }),
        true,
      );
      const keyboardChat = await createOne(page, () =>
        page.keyboard.press("Enter"),
      );
      await visible(
        sidebarList.locator(
          `a[href="/discussions?chat=${keyboardChat}"][aria-current="page"]`,
        ),
      );
      await create.focus();
      await page.keyboard.press("Shift+Tab");
      assert.equal(
        await directory.evaluate((link) => link === document.activeElement),
        true,
      );
      const beforeEnter = cliList();
      await page.keyboard.press("Enter");
      await page.waitForURL(`${base}/discussions`);
      assert.deepEqual(cliList(), beforeEnter);
      await visible(
        sidebarList.locator(`a[href="/discussions?chat=${keyboardChat}"]`),
      );
      assert.ok(
        (await sidebarList
          .getByRole("link", { name: "未命名讨论", exact: true })
          .count()) >= 2,
      );
      await shoot(page, "discussion-desktop-directory");

      // The narrow icon rail keeps the existing directory action.
      await page.setViewportSize({ width: 900, height: 844 });
      assert.equal(await navCreate.count(), 0);
      await visible(
        main.getByRole("button", { name: "新建讨论", exact: true }),
      );
      await page.setViewportSize(viewport);

      await directory.focus();
      await page.keyboard.press("Tab");
      assert.equal(
        await create.evaluate((button) => button === document.activeElement),
        true,
      );
      await createOne(page, () => page.keyboard.press("Space"));
    }
    if (name === "mobile") await shoot(page, "discussion-mobile-directory");
    const chat = await createOne(page, () =>
      name === "mobile" ? create.tap() : create.click(),
    );
    const discussion = page.url();
    assert.notEqual(
      new URL(discussion).search,
      new URL(legacyHref, base).search,
    );
    await visible(page.getByRole("heading", { name: "未命名讨论" }));
    await visible(page.getByRole("heading", { name: "可以直接开聊" }));
    if (name === "desktop") {
      const selected = await visible(
        sidebarList
          .getByRole("link", { name: "未命名讨论", exact: true })
          .and(sidebarList.locator('a[aria-current="page"]')),
      );
      assert.equal(
        await selected.getAttribute("href"),
        new URL(discussion).pathname + new URL(discussion).search,
      );
      assert.equal(await menu.locator('[aria-current="page"]').count(), 1);
      assert.ok(
        (await sidebarList
          .getByRole("link", { name: "未命名讨论", exact: true })
          .count()) >= 2,
      );
    }
    await shoot(page, `discussion-${name}-empty`);
    await page.getByRole("button", { name: "改标题" }).click();
    const title =
      name === "desktop" ? "演示仓库 · 上手说明" : "手机验证 · 上手说明";
    await page.getByRole("textbox", { name: "讨论标题" }).fill(title);
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await visible(page.getByRole("heading", { name: title }));
    const composer = page.getByRole("textbox", { name: "在讨论中发消息" });
    const mainComposer = page.getByRole("textbox", { name: "发消息给 Tend" });
    const recent = currentList.locator(`a[href="/discussions?chat=${chat}"]`);
    await sendAcrossNavigation(page, chat, composer, open, async () => {
      await activate(chatNav);
      await page.waitForURL(`${base}/`);
      await visible(mainComposer);
      assert.equal(await chatNav.getAttribute("aria-current"), "page");
      await activate(directory);
      await page.waitForURL(`${base}/discussions`);
      await activate(recent);
      await page.waitForURL(discussion);
      await visible(page.getByRole("heading", { name: title }));
    });
    await visible(
      main.getByText("可以。先区分要讨论的想法与之后可能交给 Router 的工作。"),
    );
    await page.goBack({ waitUntil: "commit" });
    await page.waitForURL(`${base}/discussions`);
    await visible(recent);
    await page.goForward({ waitUntil: "commit" });
    await page.waitForURL(discussion);
    await visible(page.getByRole("heading", { name: title }));
    await visible(main.getByText(open, { exact: true }));
    assert.equal(await main.getByText(open, { exact: true }).count(), 1);
    await assertSameDocument();
    if (name === "desktop") {
      const messageInChat =
        "Storage measurement · 切换页面后仍能看到已发送消息";
      await activate(chatNav);
      await sendAcrossNavigation(
        page,
        "main",
        mainComposer,
        messageInChat,
        async () => {
          await activate(directory);
          await visible(recent);
          await activate(recent);
          await visible(composer);
          await activate(chatNav);
          await visible(mainComposer);
        },
      );
      await visible(main.getByText(messageInChat, { exact: true }));
      await shoot(page, "discussion-desktop-navigation-chat");
      await retryWithoutStorage(
        page,
        mainComposer,
        async () => {
          await activate(directory);
          await visible(recent);
        },
        async () => {
          await activate(chatNav);
          await visible(mainComposer);
        },
      );
      await activate(directory);
      const popup = context.waitForEvent("page");
      await recent.click({
        modifiers: [process.platform === "darwin" ? "Meta" : "Control"],
      });
      const newTab = await popup;
      await newTab.waitForURL(discussion);
      await visible(newTab.getByText(open, { exact: true }));
      assert.equal(page.url(), `${base}/discussions`);
      await newTab.close();
      await activate(recent);
    } else {
      const direct = await context.newPage();
      await direct.goto(discussion);
      await visible(direct.getByText(open, { exact: true }));
      await direct.close();
      await shoot(page, "discussion-mobile-navigation-chat");
    }

    await composer.fill("@exam");
    await page.getByRole("button", { name: "example 仓库" }).click();
    const references = page.getByRole("list", { name: "已附加资料" });
    await visible(references.getByRole("link", { name: "example" }));
    assert.equal(await composer.inputValue(), "");
    await composer.fill(`@#${referenceIssue}`);
    await page
      .getByRole("button", { name: new RegExp(`#${referenceIssue}.*Issue`) })
      .click();
    await visible(
      references.getByRole("link", { name: new RegExp(`#${referenceIssue}`) }),
    );
    await page.getByRole("button", { name: "附加资料" }).click();
    await page
      .getByRole("textbox", { name: "PR / MR 链接" })
      .fill("https://github.com/Demo/Quickstart/pull/7?from=fixture");
    await page.getByRole("button", { name: "添加链接" }).click();
    const pull = references.getByRole("link", {
      name: "PR #7 · demo/quickstart",
    });
    await visible(pull);
    assert.equal(
      await pull.getAttribute("href"),
      "https://github.com/demo/quickstart/pull/7",
    );
    await page.getByRole("button", { name: "附加资料" }).click();
    await composer.fill("看看我选中的资料，再建议第一版写什么。");
    await composer.press("Enter");
    await visible(
      main.getByText(
        "从登记仓库找到启动示例。从 Issue 了解到，第一版只需要覆盖安装和首次运行。",
      ),
    );
    await shoot(page, `discussion-${name}-references`);

    await page.getByRole("button", { name: "交给 Router" }).click();
    const dialog = page.getByRole("dialog", { name: "交给 Router" });
    await dialog.getByRole("button", { name: "生成转交草稿" }).click();
    await dialog
      .getByRole("textbox", { name: "目标", exact: true })
      .fill(checkedGoal);
    await dialog
      .getByRole("checkbox", { name: new RegExp(`#${referenceIssue}`) })
      .check();
    await dialog.getByRole("checkbox", { name: /PR #7/ }).check();
    const target = dialog.getByRole("textbox", { name: "搜索执行目标 Issue" });
    await target.fill(`#${targetIssue}`);
    await dialog
      .getByRole("button", { name: new RegExp(`#${targetIssue}`) })
      .click();
    await dialog
      .getByRole("button", { name: "发送到主 Chat", exact: true })
      .scrollIntoViewIfNeeded();
    await shoot(page, `discussion-${name}-draft`);
    await dialog
      .getByRole("button", { name: "发送到主 Chat", exact: true })
      .click();
    await visible(dialog.getByText("已发送到主 Chat", { exact: true }));
    await dialog
      .getByRole("link", { name: "查看消息和 Router 回执 ↗" })
      .click();
    await page.waitForURL(/\/#message-[a-zA-Z0-9_-]+$/);
    const id = new URL(page.url()).hash;
    const sent = page.locator(id);
    await visible(sent.getByText(checkedGoal, { exact: false }));
    await visible(
      page
        .getByRole("main")
        .getByText("Router 已收到核对后的转交，先确认下一步。")
        .last(),
    );
    if (name === "desktop") await shoot(page, "discussion-desktop-receipt");
    await sent.getByRole("link", { name: "回到原讨论 ↗" }).click();
    await page.waitForURL(discussion);
    await visible(page.getByRole("link", { name: "查看主 Chat 回执 ↗" }));

    const beforeArchive = cli("read", chat);
    const count = cliList().length + cliList(true).length;
    const archive = page.getByRole("button", { name: "归档讨论", exact: true });
    const archiveRequest = page.waitForRequest(
      (req) =>
        req.method() === "PATCH" &&
        new URL(req.url()).pathname === `/api/v1/discussions/${chat}`,
    );
    await archive.click();
    assert.deepEqual((await archiveRequest).postDataJSON(), { archived: true });
    await page.waitForURL(`${base}/discussions`);
    await visible(legacyLink);
    if (name === "desktop") {
      await visible(sidebarList.locator(`a[href="${legacyHref}"]`));
      assert.equal(
        await sidebarList
          .locator(`a[href="/discussions?chat=${chat}"]`)
          .count(),
        0,
      );
      assert.equal(await sidebarList.locator("[aria-current]").count(), 0);
      assert.equal(await directory.getAttribute("aria-current"), "page");
      await visible(navCreate);
    }
    assert.equal(
      await currentList.getByRole("link", { name: new RegExp(title) }).count(),
      0,
    );
    assert.equal(
      cliList().some((row) => row.chat_id === chat),
      false,
    );
    assert.equal(
      cliList(true).find((row) => row.chat_id === chat).archived,
      true,
    );
    assert.equal(cliList().length + cliList(true).length, count);

    const restore = page.getByRole("button", { name: "恢复讨论", exact: true });
    // Same-document Back must load the archived state of the original chat.
    await page.goBack({ waitUntil: "commit" });
    await page.waitForURL(discussion);
    await assertSameDocument();
    await visible(restore.and(page.locator(":enabled")));
    await page.goForward({ waitUntil: "commit" });
    await page.waitForURL(`${base}/discussions`);

    await main.getByRole("link", { name: "已归档", exact: true }).click();
    await page.waitForURL(`${base}/discussions?archived=true`);
    const archivedLink = await visible(
      main
        .getByRole("list", { name: "已归档讨论" })
        .getByRole("link", { name: new RegExp(title) }),
    );
    assert.equal(
      new URL(await archivedLink.getAttribute("href"), base).href,
      discussion,
    );
    await shoot(page, `discussion-${name}-archive-list`);
    await archivedLink.click();
    await page.waitForURL(discussion);
    await visible(restore);
    await visible(page.getByText("已归档", { exact: true }));
    await visible(main.getByText(open, { exact: true }));
    await visible(references.getByRole("link", { name: "example" }));
    await visible(page.getByRole("link", { name: "查看主 Chat 回执 ↗" }));
    if (name === "desktop")
      assert.equal(await sidebarList.locator("[aria-current]").count(), 0);
    await shoot(page, `discussion-${name}-restore`);

    if (name === "desktop") {
      await deferWrite(
        page,
        `${base}/api/v1/discussions/${chat}`,
        "PATCH",
        async (started) => {
          await restore.click();
          assert.deepEqual(await started, { archived: false });
          await activate(directory);
          await visible(legacyLink);
          await page.goBack({ waitUntil: "commit" });
          await visible(restore.and(page.locator(":enabled")));
        },
      );
      // The new details view must react to the receipt, not the 12s poll.
      await archive.waitFor({ state: "visible", timeout: 5000 });
    } else {
      await restore.click();
      await visible(archive);
    }
    assert.equal(page.url(), discussion);
    assert.deepEqual(cli("read", chat), beforeArchive);
    assert.equal(
      cliList(true).some((row) => row.chat_id === chat),
      false,
    );
    assert.equal(cliList().find((row) => row.chat_id === chat).archived, false);
    if (name === "desktop") {
      await visible(
        sidebarList
          .getByRole("link", { name: title, exact: true })
          .and(sidebarList.locator('a[aria-current="page"]')),
      );
      assert.equal(await menu.locator('[aria-current="page"]').count(), 1);
    }

    // Keep the document mounted so only polling can observe CLI changes.
    const recentLink = sidebarList.locator(
      `a[href="/discussions${new URL(discussion).search}"]`,
    );
    cli("archive", chat);
    await visible(restore);
    if (name === "desktop") await recentLink.waitFor({ state: "detached" });
    cli("restore", chat);
    await visible(archive);
    if (name === "desktop") {
      await visible(recentLink.and(page.locator('[aria-current="page"]')));
      assert.equal(await menu.locator('[aria-current="page"]').count(), 1);
    }
    assert.equal(page.url(), discussion);
    await visible(main.getByText(open, { exact: true }));

    const issueLink = references.getByRole("link", {
      name: new RegExp(`#${referenceIssue}`),
    });
    await activate(issueLink);
    await page.waitForURL(`${base}/issues/${referenceIssue}`);
    const issueEntries = page.getByRole("navigation", { name: "Issue 入口" });
    await visible(issueEntries.getByRole("link", { name: "查看原消息" }));
    assert.equal(
      await page.getByRole("button", { name: "带入讨论", exact: true }).count(),
      0,
    );
    await shoot(page, `discussion-${name}-issue-title`);
    await page.goBack({ waitUntil: "commit" });
    await page.waitForURL(discussion);
    await visible(issueLink);
    if (name === "desktop") {
      await activate(directory);
      await main.getByRole("link", { name: "已归档", exact: true }).click();
      await page.waitForURL(`${base}/discussions?archived=true`);
      const oldIds = new Set(cliList().map(({ chat_id }) => chat_id));
      await deferWrite(
        page,
        `${base}/api/v1/discussions`,
        "POST",
        async (started) => {
          await navCreate.click();
          await started;
          await main
            .getByRole("link", { name: "当前讨论", exact: true })
            .click();
          await visible(legacyLink);
          await visible(sidebarList);
        },
      );
      const created = cliList().filter(({ chat_id }) => !oldIds.has(chat_id));
      assert.equal(created.length, 1);
      const href = `/discussions?chat=${created[0].chat_id}`;
      await currentList
        .locator(`a[href="${href}"]`)
        .waitFor({ state: "visible", timeout: 5000 });
      await sidebarList
        .locator(`a[href="${href}"]`)
        .waitFor({ state: "visible", timeout: 5000 });
      assert.equal(page.url(), `${base}/discussions`);

      const agents = menu.getByRole("link", { name: "Agents", exact: true });
      const filter = page.getByRole("searchbox", { name: "搜索名称或 ID" });
      const matchingEntry = `${base}/agents?q=same-entry-fixture`;
      await agents.click();
      await filter.fill("same-entry-fixture");
      await page.waitForURL(matchingEntry);
      await agents.click();
      await page.waitForURL(`${base}/agents`);
      await filter.fill("same-entry-fixture");
      await page.waitForURL(matchingEntry);
      const previousIds = new Set(cliList().map(({ chat_id }) => chat_id));
      await deferWrite(
        page,
        `${base}/api/v1/discussions`,
        "POST",
        async (started) => {
          await navCreate.click();
          await started;
          assert.equal(await traverseHistory(page, -1), matchingEntry);
        },
      );
      await visible(navCreate.and(page.locator(":enabled")));
      assert.equal(page.url(), matchingEntry);
      assert.equal(await traverseHistory(page, 1), matchingEntry);
      assert.equal(await filter.inputValue(), "same-entry-fixture");
      assert.equal(
        cliList().filter(({ chat_id }) => !previousIds.has(chat_id)).length,
        1,
      );
    }
    await activate(directory);
    await activate(currentList.getByRole("link", { name: /主 Chat 引用示例/ }));
    const origin = await visible(
      page
        .getByRole("region", { name: "当前讨论" })
        .getByRole("link", { name: "主 Chat 消息", exact: true }),
    );
    await activate(origin);
    await page.waitForURL(/\/#message-[a-zA-Z0-9_-]+$/);
    await visible(
      page
        .locator(new URL(page.url()).hash)
        .getByText("请整理示例仓库的快速上手指南。", { exact: true }),
    );
    await assertSameDocument();
    await context.close();
  }
  assert.deepEqual(errors, []);
  console.log(
    "PASS desktop and mobile Chromium: separate plus, same-document Back/Forward including same-URL entries during a pending create, native new tab and deep links, one in-flight POST, identity retained on lost receipt with storage failure, late create/restore refresh, no main Chat or Issue creation shortcut, manual references, existing origins, handoff and backlinks",
  );
} finally {
  await browser.close();
}
