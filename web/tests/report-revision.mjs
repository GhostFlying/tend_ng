import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";
import { isAnonymousSessionProbe } from "./console-errors.mjs";

const {
  TEND_URL: url,
  TEND_TOKEN: token,
  TEND_FIXTURE_ISSUE: issue,
  TEND_SCREENSHOTS: screenshots,
} = process.env;
assert(url && token && issue && new URL(url).hostname === "127.0.0.1");
const issueUrl = `${url}/issues/${issue}`;
const browser = await chromium.launch({ args: ["--no-proxy-server"] });

async function capture(page, name) {
  if (screenshots) {
    await page.screenshot({
      path: join(screenshots, `report-revision-${name}.png`),
      animations: "disabled",
    });
  }
}

try {
  for (const [width, height, device] of [
    [1440, 1000, "desktop"],
    [390, 844, "mobile"],
  ]) {
    const page = await browser.newPage({
      viewport: { width, height },
      colorScheme: "light",
      hasTouch: device === "mobile",
    });
    const errors = [];
    const requests = [];
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (message) => {
      if (isAnonymousSessionProbe(message)) return;
      if (["warn", "warning", "error"].includes(message.type()))
        errors.push(message.text());
    });
    page.on("request", (request) => requests.push(request.url()));
    await page.goto(issueUrl);
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    const main = page.getByRole("main");
    await main
      .getByRole("heading", { name: `#${issue} 任务摘要导出交付` })
      .waitFor();
    const current = main.locator("#message-msg-new");
    const history = current.locator(":scope > .delivery-history");
    const summary = history.locator(":scope > summary");
    const old = history.locator("#message-msg-old");
    const solo = main.locator("#message-msg-solo");
    await current.getByText("修订后的导出说明", { exact: true }).waitFor();
    await solo.getByText("字段核对记录", { exact: true }).waitFor();
    assert.equal(await page.title(), "Tend");
    assert.equal(page.url(), issueUrl);
    assert.equal(await page.locator("vite-error-overlay").count(), 0);
    assert.equal(await summary.innerText(), "版本历史 (1)");
    assert.equal(await history.evaluate((node) => node.open), false);
    assert.equal(await old.isVisible(), false);
    assert.equal(
      await current
        .getByText("export-notes-v2.md", { exact: true })
        .isVisible(),
      true,
    );
    assert.equal(
      await solo.getByText("field-checklist.txt", { exact: true }).isVisible(),
      true,
    );
    assert.equal(await main.locator(".work-deliveries").count(), 0);
    assert.equal(
      requests.filter((url) => url.includes("/api/v1/artifacts/")).length,
      0,
    );
    assert.equal(await main.evaluate((node) => node.scrollTop), 0);
    await page.evaluate(() => document.fonts.ready);
    await capture(page, device);

    await summary.focus();
    assert.equal(
      await summary.evaluate((node) => node === document.activeElement),
      true,
    );
    await summary.press("Enter");
    await old.getByText("初版导出说明", { exact: true }).waitFor();
    assert.equal(await history.evaluate((node) => node.open), true);
    assert.equal(
      await old
        .getByText("来自原 Chat 的交付报告", { exact: true })
        .isVisible(),
      true,
    );
    assert.equal(
      await old.getByText("Markdown", { exact: true }).isVisible(),
      true,
    );
    assert.equal(
      await old.getByText("HTML 原件", { exact: true }).isVisible(),
      true,
    );
    assert.equal(
      await old.getByText("export-sketch-v1.html", { exact: true }).isVisible(),
      true,
    );

    const markdown = old.getByText("阅读 Markdown 原件", { exact: true });
    await markdown.click();
    const heading = old.getByRole("heading", { name: "初版｜导出格式与边界" });
    await heading.waitFor();
    await old.getByRole("table").waitFor();
    assert.equal(
      await page.evaluate(() => document.documentElement.scrollWidth),
      width,
    );
    assert.equal(
      await main.evaluate((node) => node.scrollWidth),
      width === 390 ? 390 : width - 288,
    );
    if (device === "mobile") {
      for (const box of [
        old.getByRole("region", { name: "表格" }),
        old.locator("pre"),
      ]) {
        assert.equal(
          await box.evaluate((node) => {
            node.scrollLeft = 100;
            const scrollable =
              node.scrollWidth > node.clientWidth && node.scrollLeft > 0;
            node.scrollLeft = 0;
            return scrollable;
          }),
          true,
        );
      }
      await old.evaluate((node) => {
        node.scrollIntoView({ block: "start" });
      });
    }
    if (device === "desktop") {
      await main.evaluate((node) => {
        node.scrollTop = 240;
      });
    }
    await capture(page, `${device}-expanded`);

    const download = page.waitForEvent("download");
    await old.getByRole("button", { name: "打开原件（下载 HTML）" }).click();
    const html = await download;
    assert.equal(html.suggestedFilename(), "export-sketch-v1.html");
    assert.match(
      await readFile(await html.path(), "utf8"),
      /Synthetic export sketch/,
    );
    await summary.click();
    assert.equal(await old.isVisible(), false);

    // The shared Issue anchor handling opens just the disclosure containing
    // the addressed old report. The original Chat retains its own message.
    await page.goto(`${issueUrl}#message-msg-old`);
    const original = old.getByRole("link", { name: "查看原报告消息" });
    await original.waitFor();
    assert.equal(
      await original.getAttribute("href"),
      "/?chat=report-fixture#message-msg-old",
    );
    await original.click();
    await page.waitForURL(`${url}/?chat=report-fixture#message-msg-old`);
    const chatMessage = main.locator("#message-msg-old");
    await chatMessage.waitFor();
    assert.match(await chatMessage.innerText(), /初版导出说明/);
    assert.deepEqual(errors, []);
    console.log(
      `PASS ${width}×${height} ordinary Issue version history, original Markdown, HTML download, anchor and Chat link`,
    );
    await page.close();
  }
} finally {
  await browser.close();
}
