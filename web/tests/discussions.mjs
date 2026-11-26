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
} = process.env;
const open = "先聊聊演示项目的快速上手说明。";
const errors = [];
const browser = await chromium.launch({
  channel: "chromium",
  ignoreDefaultArgs: ["--disable-back-forward-cache"],
});

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

try {
  for (const [name, viewport] of Object.entries({
    desktop: { width: 1440, height: 960 },
    mobile: { width: 390, height: 844 },
  })) {
    const checkedGoal =
      name === "desktop"
        ? "只整理一页上手说明，先交草案，不部署。"
        : "先交一页上手说明草案，只覆盖安装与首次运行。";
    const page = await browser.newPage({
      viewport,
      isMobile: name === "mobile",
      hasTouch: name === "mobile",
      locale: "zh-CN",
      colorScheme: "light",
    });
    await page.addInitScript(() => {
      globalThis.addEventListener("pageshow", (event) => {
        document.documentElement.dataset.bfcache = String(event.persisted);
      });
    });
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (message) => {
      // Full Chromium probes /favicon.ico, which this Server does not serve.
      const faviconProbe =
        message.location().url === `${base}/favicon.ico` &&
        /\b401\b/.test(message.text());
      if (
        ["error", "warning"].includes(message.type()) &&
        !isAnonymousSessionProbe(message) &&
        !faviconProbe
      ) {
        errors.push(`${message.location().url} ${message.text()}`);
      }
    });
    await page.goto(base);
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    const menu = page.getByRole("navigation", {
      name: name === "mobile" ? "手机导航" : "主导航",
    });
    const directory = menu.getByRole("link", { name: "讨论", exact: true });
    const navCreate = menu.getByRole("button", {
      name: "新建讨论",
      exact: true,
    });
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
    const main = page.getByRole("main");
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
    await composer.fill(open);
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await visible(
      main.getByText("可以。先区分要讨论的想法与之后可能交给 Router 的工作。"),
    );
    await page.reload();
    assert.equal(page.url(), discussion);
    await visible(page.getByRole("heading", { name: title }));
    await visible(main.getByText(open, { exact: true }));

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
    // Exercise a real cached document, not a cold navigation to the old URL.
    await page.goBack({ waitUntil: "commit" });
    assert.equal(page.url(), discussion);
    await page.waitForFunction(
      () => document.documentElement.dataset.bfcache === "true",
      null,
      { timeout: 5000 },
    );
    await visible(restore.and(page.locator(":enabled")));
    await page.goForward({ waitUntil: "commit" });
    assert.equal(page.url(), `${base}/discussions`);

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

    await restore.click();
    await visible(archive);
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

    if (name === "desktop") {
      await page.goto(`${base}/issues/${targetIssue}`);
      await page.getByRole("button", { name: "带入讨论" }).click();
      await page.waitForURL(/\/discussions\?chat=/);
      await visible(
        page
          .getByRole("region", { name: "当前讨论" })
          .getByRole("link", { name: new RegExp(`#${targetIssue}`) })
          .first(),
      );
      cli(
        "rename",
        new URL(page.url()).searchParams.get("chat"),
        "从 Issue 带入的讨论",
      );
      await page.goto(base);
      await page.getByRole("button", { name: "带入讨论" }).first().click();
      await page.waitForURL(/\/discussions\?chat=/);
      await visible(page.getByRole("link", { name: "主 Chat 消息" }));
      cli(
        "rename",
        new URL(page.url()).searchParams.get("chat"),
        "从主 Chat 带入的讨论",
      );
    }
    await page.close();
  }
  assert.deepEqual(errors, []);
  console.log(
    "PASS discussion desktop and mobile Chromium, separate navigation and creation with mouse, keyboard and touch, hover and focus, unique selection, BFCache Back, live Web/CLI archive and restore, references, draft, explicit send and bidirectional links",
  );
} finally {
  await browser.close();
}
