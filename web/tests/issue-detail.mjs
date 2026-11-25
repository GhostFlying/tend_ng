import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { chromium } from "playwright";

const { TEND_URL: url, TEND_TOKEN: token } = process.env;
assert(url && token);
const browser = await chromium.launch({ args: ["--no-proxy-server"] });

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
      if (message.type() === "error") errors.push(message.text());
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
    assert.equal(await view.evaluate((node) => node.scrollTop), 0);
    assert.equal(await view.locator("#message-old-note").count(), 0);
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
    await page.close();
  }
  console.log(
    "PASS unified Issue page at 1440×1000 and 390×844, Markdown isolation, HTML download, deep links and direct reply",
  );
} finally {
  await browser.close();
}
