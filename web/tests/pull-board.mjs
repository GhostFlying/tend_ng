import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";

const {
  TEND_URL: url,
  TEND_TOKEN: token,
  TEND_BOARD_SCREENSHOTS: screenshots,
  TEND_BOARD_SOURCE: source,
  TEND_BOARD_PARENT: parentId,
  TEND_BOARD_PARENT_NUMBER: parentNumber,
  TEND_BOARD_CHILD_NUMBER: childNumber,
  TEND_ISSUE_NUMBER_SCREENSHOTS: numberScreenshots,
} = process.env;
assert(url && token && source && parentId && parentNumber && childNumber);
const issueMessage = "board-issue-history-54";
const browser = await chromium.launch({ args: ["--no-proxy-server"] });

async function waitForVisibleMessage(page, id) {
  await page.waitForFunction((id) => {
    const row = document.getElementById(`message-${id}`);
    if (!row) return false;
    const view = row.closest("main").getBoundingClientRect();
    const bounds = row.getBoundingClientRect();
    return bounds.top >= view.top && bounds.bottom <= view.bottom;
  }, id);
}

try {
  for (const [width, height] of [
    [1440, 1000],
    [390, 844],
  ]) {
    const page = await browser.newPage({ viewport: { width, height } });
    const errors = [];
    const reads = [];
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (message) => {
      if (message.type() === "error") errors.push(message.text());
    });
    page.on("request", (request) => {
      const target = new URL(request.url());
      if (target.pathname.endsWith("/messages")) reads.push(target);
    });
    await page.goto(url);
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    await page.getByRole("link", { name: "看板", exact: true }).click();
    await page.getByRole("heading", { name: "Issue 看板" }).waitFor();
    const parent = `#${parentNumber} 示例仓库上手指南`;
    if (width === 390) {
      await page.getByRole("button", { name: /^已完成/ }).click();
    } else {
      const close = page.getByRole("button", { name: "收起已取消" });
      await close.waitFor();
      assert.equal(await close.getAttribute("aria-expanded"), "true");
      assert.equal(
        await close.evaluate(
          (node) =>
            globalThis.getComputedStyle(node.firstElementChild).writingMode,
        ),
        "horizontal-tb",
      );
    }
    const card = page
      .locator(".board-card")
      .filter({ has: page.getByRole("link", { name: parent, exact: true }) });
    const pull = card
      .locator(".board-pull")
      .filter({ has: page.getByRole("link", { name: "example/guide #73" }) });
    await pull.getByText("CI 通过").waitFor();
    await pull.getByText("暂不可观测").waitFor();
    assert.equal(
      await pull.getByRole("link").getAttribute("href"),
      "https://github.com/example/guide/pull/73",
    );
    await card.getByRole("link", { name: "example/guide #99" }).waitFor();
    await card.getByText("CI 未确认").waitFor();
    assert.equal(
      await card
        .locator(".board-card-title")
        .evaluate((node) => globalThis.getComputedStyle(node).fontWeight),
      "500",
    );
    assert.equal(
      await page.getByRole("link", { name: "核对安装步骤" }).count(),
      0,
    );
    assert.equal(
      await page.evaluate(() => document.documentElement.scrollWidth),
      width,
    );
    if (screenshots) {
      await mkdir(screenshots, { recursive: true });
      await page.screenshot({
        path: join(screenshots, `issue-board-${width}.png`),
      });
    }
    const cardLink = card.getByRole("link", { name: parent, exact: true });
    assert.equal(
      await cardLink.getAttribute("href"),
      `/issues/${parentNumber}/overview`,
    );
    await cardLink.click();
    await page.getByRole("heading", { name: parent, exact: true }).waitFor();
    const overview = page.getByRole("main");
    await overview.getByText("已完成 · example", { exact: true }).waitFor();
    const child = overview.getByRole("link", {
      name: `#${childNumber} 核对安装步骤`,
      exact: true,
    });
    await child.waitFor();
    assert.equal(
      await child.getAttribute("href"),
      `/issues/${childNumber}/overview`,
    );
    assert.equal(await page.title(), "Tend");
    assert.equal(
      await page.evaluate(() => document.documentElement.scrollWidth),
      width,
    );
    if (numberScreenshots) {
      await mkdir(numberScreenshots, { recursive: true });
      const filename =
        width === 390
          ? "issue-short-number-mobile-390.png"
          : "issue-short-number-desktop-1440.png";
      await page.evaluate(() => document.fonts.ready);
      await page.screenshot({ path: join(numberScreenshots, filename) });
    }
    assert.equal(
      await overview.getByRole("link", { name: "example/guide #73" }).count(),
      1,
    );
    const chat = overview.getByRole("link", { name: "进入 Issue 对话" });
    const ownerPath = await chat.getAttribute("href");
    assert.equal(ownerPath, `/issues/${parentNumber}`);
    assert.equal(new URL(page.url()).pathname, ownerPath + "/overview");
    assert.equal(await page.getByLabel("发消息给 Issue Owner").count(), 0);
    await overview.getByRole("link", { name: "查看原消息" }).click();
    await page.waitForURL(`${url}/#message-${source}`);
    await waitForVisibleMessage(page, source);
    const olderReads = (path) =>
      reads.filter(
        (read) => read.pathname === path && read.searchParams.has("before"),
      );
    assert.equal(olderReads("/api/v1/chats/main/messages").length, 2);

    await page.goBack();
    await chat.click();
    const discussionHeading = page.getByRole("heading", {
      name: parent,
      exact: true,
    });
    const lastMessage = page.getByText("Issue history 159", { exact: true });
    await discussionHeading.waitFor();
    await lastMessage.waitFor();
    assert.equal(
      await page.evaluate(() => document.documentElement.scrollWidth),
      width,
    );
    assert.equal(
      await lastMessage
        .locator("..")
        .getByRole("link", { name: parent, exact: true })
        .count(),
      0,
    );
    const ownerApi = `/api/v1${ownerPath}/messages`;
    assert.equal(olderReads(ownerApi).length, 0);
    assert.equal(await page.locator(`#message-${issueMessage}`).count(), 0);
    await page.goto(`${url}${ownerPath}#message-${issueMessage}`);
    await waitForVisibleMessage(page, issueMessage);
    assert.equal(olderReads(ownerApi).length, 2);
    await page.waitForResponse((response) => {
      const target = new URL(response.url());
      return target.pathname === ownerApi && !target.search;
    });
    await waitForVisibleMessage(page, issueMessage);
    assert.equal(olderReads(ownerApi).length, 2);
    await page.getByRole("button", { name: "读取更早的消息" }).click();
    await page.getByText("Issue history 0", { exact: true }).waitFor();
    assert.equal(olderReads(ownerApi).length, 3);
    await page.goto(`${url}/issues/${parentId}#message-${issueMessage}`);
    await discussionHeading.waitFor();
    await waitForVisibleMessage(page, issueMessage);
    const overviewLink = page.getByRole("link", {
      name: "Issue 概览",
      exact: true,
    });
    assert.equal(
      await overviewLink.getAttribute("href"),
      `${ownerPath}/overview`,
    );
    await page.goto(`${url}/issues/${parentId}/overview`);
    await discussionHeading.waitFor();
    assert.equal(await chat.getAttribute("href"), ownerPath);
    await page.getByRole("link", { name: "返回看板" }).click();
    if (width === 390) {
      await page.getByRole("button", { name: /^已取消/ }).click();
      await page.locator("#board-cancelled-content .board-empty").waitFor();
      await page.setViewportSize({ width: 280, height });
      assert.equal(
        (await page.locator(".board-column:visible").boundingBox()).width,
        256,
      );
      assert.equal(
        await page.evaluate(() => document.documentElement.scrollWidth),
        280,
      );
      await page.setViewportSize({ width, height });
      await page.getByRole("button", { name: /^待安排/ }).click();
    } else {
      await page.getByRole("button", { name: "收起已取消" }).click();
      await page.reload();
      await page
        .locator("#board-cancelled-content")
        .waitFor({ state: "attached" });
      assert.equal(
        await page.locator("#board-cancelled-content").isVisible(),
        false,
      );
      await page.getByRole("button", { name: "展开已取消" }).click();
      await page.locator("#board-cancelled-content .board-empty").waitFor();
      await page.setViewportSize({ width: 1024, height });
      const scroll = page.locator(".board-columns");
      const columnWidths = () =>
        page
          .locator(".board-column")
          .evaluateAll((nodes) =>
            nodes.map((node) => node.getBoundingClientRect().width),
          );
      assert.deepEqual(await columnWidths(), Array(6).fill(256));
      await scroll.evaluate((node) => {
        node.scrollLeft = 0;
      });
      const box = await scroll.boundingBox();
      await page.mouse.move(box.x + 40, box.y + 140);
      await page.mouse.wheel(900, 0);
      await page.waitForFunction(() => {
        const node = document.querySelector(".board-columns");
        return node.scrollWidth - node.clientWidth - node.scrollLeft <= 1;
      });
      assert.equal(await page.evaluate(() => globalThis.scrollX), 0);
      assert.equal(
        await page.evaluate(() => document.documentElement.scrollWidth),
        1024,
      );
      await page.getByRole("button", { name: "收起已取消" }).click();
      assert.deepEqual(await columnWidths(), [...Array(5).fill(256), 44]);
      await page.setViewportSize({ width, height });
    }
    await page.getByRole("link", { name: "稍后整理发布说明" }).click();
    await overview
      .getByText("待安排 · release-notes", { exact: true })
      .waitFor();
    assert.equal(await overview.getByRole("list").count(), 0);
    await chat.click();
    await page.getByText("暂无 Issue Owner。", { exact: false }).waitFor();
    await page.route("**/api/v1/issue-board", (route) =>
      route.fulfill({ json: { issues: [], next_before: null } }),
    );
    await page.goto(`${url}/issues`);
    await page.locator(".board-column:visible .board-empty").first().waitFor();
    assert.deepEqual(
      await page.locator(".board-column-title").allTextContents(),
      ["待安排", "待推进", "进行中", "等待中", "已完成", "已取消"],
    );
    if (width === 390)
      assert.equal(await page.locator(".board-column:visible").count(), 1);
    assert.deepEqual(errors, []);
    await page.close();
  }
  console.log(
    "PASS Issue overview, PR links, and third-page message anchors in desktop and mobile Chromium",
  );
} finally {
  await browser.close();
}
