import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";

const { TEND_URL: url, TEND_TOKEN: token, TEND_SCREENSHOTS } = process.env;
assert(url && token);
const browser = await chromium.launch({ args: ["--no-proxy-server"] });
const older = "读取更早的消息";
const seeking = "正在定位原消息…";
const missing = "没有找到链接指向的原消息。";
const unavailable = "概要暂不可用（测试注入）";

async function screenshot(page, name) {
  if (TEND_SCREENSHOTS) {
    await page.screenshot({ path: join(TEND_SCREENSHOTS, `${name}.png`) });
  }
}

async function centered(page, id) {
  await page.waitForFunction((message) => {
    const row = document.getElementById(message);
    if (!row) return false;
    const scroller = row.closest("main").getBoundingClientRect();
    const bounds = row.getBoundingClientRect();
    return (
      (bounds.top >= scroller.top && bounds.top < scroller.bottom) ||
      (bounds.top < scroller.top && bounds.bottom >= scroller.bottom)
    );
  }, `message-${id}`);
}

async function interleavedHistory(page, width) {
  const overview = Promise.withResolvers();
  const manual = Promise.withResolvers();
  const duplicate = Promise.withResolvers();
  const cursors = [];
  const issue = "/issues/historical-parent";
  const overviewPattern = `**/api/v1${issue}/overview`;
  const historyPattern = `**/api/v1${issue}/messages?before=*`;
  await page.route(overviewPattern, async (route) => {
    await overview.promise;
    await route.continue();
  });
  await page.route(historyPattern, async (route) => {
    const cursor = new URL(route.request().url()).searchParams.get("before");
    cursors.push(cursor);
    if (cursors.length === 1) await manual.promise;
    else if (cursor === cursors[0]) await duplicate.promise;
    await route.continue();
  });
  try {
    await page.goto(`${url}${issue}/overview#message-old-note`);
    await page.locator("#message-history-109").waitFor();
    await page.getByRole("button", { name: older, exact: true }).click();
    await page
      .getByRole("button", { name: "正在读取…", exact: true })
      .waitFor();
    overview.resolve();
    await page
      .getByRole("heading", { name: "#1 用量统计方案与设计稿" })
      .waitFor();
    await page.waitForResponse(
      (response) =>
        response.url() === `${url}/api/v1${issue}/messages` && response.ok(),
    );
    const requestsWhileManual = cursors.length;
    manual.resolve();
    await page.locator("#message-history-20").waitFor();
    duplicate.resolve();
    assert.equal(
      requestsWhileManual,
      1,
      "only the manual request owns the cursor",
    );
    await centered(page, "old-note");
    assert.equal(await page.getByRole("button", { name: seeking }).count(), 0);
    assert.equal(await page.locator("#message-old-note").count(), 1);
    assert.equal(await page.getByText(missing, { exact: true }).count(), 0);
    assert.equal(cursors.length, 2, "the anchor is on the third page");
    const next = page.getByRole("button", { name: older, exact: true });
    await next.waitFor();
    assert.equal(await next.isEnabled(), true);
    await screenshot(page, `interleaved-${width}`);
    await next.click();
    await page.locator("#message-earlier-0").waitFor();
    assert.equal(cursors.length, 3);
    assert.equal(new Set(cursors).size, 3);
    const ids = await page
      .getByRole("list", { name: "对话记录" })
      .locator("[id^='message-']")
      .evaluateAll((rows) => rows.map((row) => row.id));
    assert.equal(ids.length, new Set(ids).size);
  } finally {
    overview.resolve();
    manual.resolve();
    duplicate.resolve();
    await page.unroute(overviewPattern);
    await page.unroute(historyPattern);
  }
}

async function cancelAnchorSearch(page) {
  const release = Promise.withResolvers();
  const pattern = "**/api/v1/issues/historical-parent/messages?before=*";
  await page.route(pattern, async (route) => {
    await release.promise;
    await route.continue();
  });
  try {
    await page.goto(`${url}/issues/historical-parent#message-old-note`);
    const busy = page.getByRole("button", { name: seeking, exact: true });
    await busy.waitFor();
    assert.equal(await busy.isEnabled(), false);
    await page.evaluate(() => {
      document.location.hash = "message-history-109";
    });
    await centered(page, "history-109");
    const next = page.getByRole("button", { name: older, exact: true });
    await next.waitFor();
    assert.equal(await next.isEnabled(), true);
    await next.click();
    release.resolve();
    await page.locator("#message-history-20").waitFor();
    assert.equal(await page.locator("#message-old-note").count(), 0);
    assert.equal(await next.isEnabled(), true);
  } finally {
    release.resolve();
    await page.unroute(pattern);
  }
}

async function recoveredOverview(page, id, width) {
  const recovery = Promise.withResolvers();
  const failAgain = id === "review-report";
  const retry = Promise.withResolvers();
  const pattern = "**/api/v1/issues/1/overview";
  let attempts = 0;
  await page.route(pattern, async (route) => {
    attempts++;
    if (attempts === 1 || (failAgain && attempts === 2)) {
      if (attempts === 2) await retry.promise;
      await route.fulfill({ status: 503, json: { error: unavailable } });
    } else {
      await recovery.promise;
      await route.continue();
    }
  });
  try {
    const issue = failAgain ? "/issues/1" : "/issues/1/overview";
    await page.goto(`${url}${issue}#message-${id}`);
    const overviewError = page.getByText(unavailable, { exact: true });
    await overviewError.waitFor();
    if (failAgain) {
      // Even with every discussion page present, the report source is pending.
      for (const oldest of ["history-20", "earlier-20", "earlier-0"]) {
        await page.getByRole("button", { name: older, exact: true }).click();
        await page.locator(`#message-${oldest}`).waitFor();
      }
      assert.equal(await page.getByRole("button", { name: older }).count(), 0);
      const secondFailure = page.waitForResponse(
        (response) =>
          response.url() === `${url}/api/v1/issues/1/overview` &&
          response.status() === 503,
      );
      retry.resolve();
      await secondFailure;
    } else {
      await page.locator("#message-history-109").waitFor();
    }
    assert.equal(await overviewError.isVisible(), true);
    assert.equal(await page.getByText(missing, { exact: true }).count(), 0);
    assert.equal(await page.locator(`#message-${id}`).count(), 0);
    assert.equal(await page.getByRole("button", { name: seeking }).count(), 0);
    recovery.resolve();
    await centered(page, id);
    assert(attempts >= 2, "a subsequent overview read supplied the data");
    assert.equal(await overviewError.count(), 0);
    assert.equal(await page.getByText(missing, { exact: true }).count(), 0);
    assert.equal(await page.getByRole("button", { name: seeking }).count(), 0);
    await screenshot(page, `recovered-${id}-${width}`);
    if (failAgain) {
      await page.goto(`${url}/issues/1#message-not-in-this-issue`);
      await page.getByText(missing, { exact: true }).waitFor();
    }
  } finally {
    retry.resolve();
    recovery.resolve();
    await page.unroute(pattern);
  }
}

try {
  for (const [width, height] of [
    [1440, 1000],
    [390, 844],
  ]) {
    const page = await browser.newPage({ viewport: { width, height } });
    const errors = [];
    const requests = [];
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (message) => {
      if (message.type() === "error" || message.type() === "warning") {
        errors.push({ text: message.text(), url: message.location().url });
      }
    });
    page.on("request", (request) => requests.push(new URL(request.url())));
    await page.route("https://example.test/**", (route) => route.abort());
    await page.goto(url + "/issues");
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    if (width === 390) {
      await page.getByRole("button", { name: /^已完成/ }).click();
    }
    const initialLink = page.getByRole("link", {
      name: "#1 用量统计方案与设计稿",
    });
    assert.equal(await initialLink.getAttribute("href"), "/issues/1");
    await initialLink.click();
    const view = page.getByRole("main");
    await view
      .getByRole("heading", { name: "#1 用量统计方案与设计稿" })
      .waitFor();
    const report = view.locator("#message-review-report");
    await report.getByText("两份评审原件", { exact: true }).waitFor();
    assert.equal(await report.locator("ol > li").count(), 6);
    await view.getByText("工作过程记录 109", { exact: true }).waitFor();
    assert.equal(await page.title(), "Tend");
    assert.equal(await view.evaluate((node) => node.scrollTop), 0);
    assert.equal(await view.locator("#message-old-note").count(), 0);
    await screenshot(page, `issue-${width}`);
    assert.equal(
      await report.getByText("usage-plan.md", { exact: true }).count(),
      1,
    );
    await view.getByText("其他工作交付", { exact: false }).first().click();
    await view
      .locator(".work-deliveries")
      .getByText("usage-plan.md", { exact: true })
      .waitFor();
    assert.equal(
      await report.getByText("usage-plan.md", { exact: true }).count(),
      1,
    );
    await view.getByRole("heading", { name: "待你决定" }).waitFor();
    assert.equal(
      await view
        .getByRole("link", { name: "在原 Chat 处理" })
        .getAttribute("href"),
      "/?chat=work#message-review-decision",
    );

    assert.equal(
      requests.filter((address) =>
        address.pathname.startsWith("/api/v1/artifacts/"),
      ).length,
      0,
    );
    await report.getByText("阅读 Markdown 原件", { exact: true }).click();
    await report
      .getByRole("heading", { name: "用量方案原件", exact: true })
      .waitFor();
    await report.getByRole("table").waitFor();
    assert.equal(
      await report.getByRole("link", { name: "参考链接" }).getAttribute("href"),
      "https://example.test/guide",
    );
    assert.equal(
      await report
        .locator(
          "img, iframe, script, a[href^='javascript:'], a[href^='data:']",
        )
        .count(),
      0,
    );
    assert.equal(
      await page.evaluate(() => typeof globalThis.issueMarkdownExecuted),
      "undefined",
    );
    assert.equal(
      requests.filter((address) => address.hostname === "example.test").length,
      0,
    );
    if (width === 390) {
      for (const box of [
        report.getByRole("region", { name: "表格" }),
        report.locator("pre"),
      ]) {
        assert.equal(
          await box.evaluate((node) => {
            node.scrollLeft = 100;
            return node.scrollWidth > node.clientWidth && node.scrollLeft > 0;
          }),
          true,
        );
      }
    }
    assert.equal(
      await page.evaluate(() => document.documentElement.scrollWidth),
      width,
    );
    assert.equal(
      await view.evaluate((node) => node.scrollWidth),
      width === 390 ? 390 : width - 288,
    );

    const [download] = await Promise.all([
      page.waitForEvent("download"),
      report.getByRole("button", { name: "打开原件（下载 HTML）" }).click(),
    ]);
    assert.equal(download.suggestedFilename(), "usage-design.html");
    assert.match(
      await readFile(await download.path(), "utf8"),
      /<!doctype html>/,
    );
    assert.equal(
      await page.evaluate(() => typeof globalThis.issueHtmlExecuted),
      "undefined",
    );
    const htmlSource = report.getByRole("link", {
      name: "来源 #3 提交交互设计稿",
    });
    assert.equal(await htmlSource.getAttribute("href"), "/issues/3");
    const provenance = htmlSource.locator("../..");
    await provenance.getByText("交付来源", { exact: true }).click();
    await provenance.getByText("design-task", { exact: true }).waitFor();
    await provenance.getByText("html-original", { exact: true }).waitFor();
    await htmlSource.click();
    await view
      .getByRole("heading", { name: "#3 提交交互设计稿", exact: true })
      .waitFor();
    await view
      .getByText("没有可追溯到原报告消息的交付。", { exact: false })
      .waitFor();
    await page.goBack();
    const original = report.getByRole("link", { name: "查看原报告消息" });
    assert.equal(
      await original.getAttribute("href"),
      "/?chat=work#message-review-report",
    );
    await original.click();
    await page.waitForURL(url + "/?chat=work#message-review-report");
    await centered(page, "review-report");

    await page.goto(url + "/issues/1#message-text-only-report");
    await centered(page, "text-only-report");
    const textOnly = view.locator("#message-text-only-report");
    assert.equal(await textOnly.count(), 1);
    assert.equal(
      await textOnly.getByText("纯文本核对", { exact: true }).isVisible(),
      true,
    );
    assert.equal(await textOnly.getByRole("listitem").count(), 0);
    assert.equal(await page.locator("vite-error-overlay").count(), 0);
    await screenshot(page, `text-only-report-${width}`);
    await textOnly.getByRole("link", { name: "查看原报告消息" }).click();
    await page.waitForURL(url + "/?chat=work#message-text-only-report");
    await centered(page, "text-only-report");

    await page.goto(url + "/issues/%231/overview#message-review-report");
    await centered(page, "review-report");
    assert.equal(
      requests.filter(
        (address) =>
          address.pathname.startsWith("/api/v1/issues/") &&
          address.searchParams.has("before"),
      ).length,
      0,
    );

    await page.goto(
      url + "/issues/historical-parent/overview#message-old-note",
    );
    await centered(page, "old-note");
    assert.equal(
      requests.filter(
        (address) =>
          address.pathname === "/api/v1/issues/historical-parent/messages" &&
          address.searchParams.has("before"),
      ).length,
      2,
    );
    const row = view.locator("#message-old-note");
    assert.equal(
      await row.getByRole("link", { name: "原对话" }).getAttribute("href"),
      "/?chat=work#message-review-request",
    );
    await row.getByRole("button", { name: "回复", exact: true }).click();
    const input = page.getByLabel("发消息给 Issue Owner");
    assert.equal(
      await input.evaluate((node) => document.activeElement === node),
      true,
    );
    const answer = `请补充来源说明（${width}）`;
    await input.fill(answer);
    const [receipt] = await Promise.all([
      page.waitForResponse(
        (response) =>
          response.request().method() === "POST" &&
          response.url().endsWith("/api/v1/issues/historical-parent/messages"),
      ),
      input.press("Enter"),
    ]);
    assert.equal(receipt.status(), 200);
    await view.getByText(answer, { exact: true }).waitFor();
    assert.deepEqual(errors, []);

    await page.goto(url + "/issues/1");
    await page.locator("#message-history-109").waitFor();
    await page.getByRole("button", { name: older, exact: true }).click();
    await page.locator("#message-history-20").waitFor();
    assert.equal(await page.locator("#message-old-note").count(), 0);
    assert.equal(
      await page.getByRole("button", { name: older }).isEnabled(),
      true,
    );
    await interleavedHistory(page, width);
    await cancelAnchorSearch(page);
    await recoveredOverview(page, "review-report", width);
    await recoveredOverview(page, "old-note", width);
    assert.equal(errors.length, 3);
    for (const error of errors) {
      assert.equal(error.url, `${url}/api/v1/issues/1/overview`);
      assert.match(error.text, /503/);
    }
    console.log(
      `PASS ${width}×${height}: latest page, manual pagination, deep links, interleaved cursors, hash cancellation, 503→200 report and discussion anchors`,
    );
    await page.close();
  }
  console.log(
    "PASS unified Issue page at 1440×1000 and 390×844, text-only and legacy reports, Markdown isolation, HTML download, deep links and direct reply",
  );
} finally {
  await browser.close();
}
