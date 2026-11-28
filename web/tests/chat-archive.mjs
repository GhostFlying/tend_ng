import assert from "node:assert/strict";
import { mkdir, readFile } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";
import { isAnonymousSessionProbe } from "./console-errors.mjs";

const {
  TEND_URL: base,
  TEND_TOKEN: token,
  TEND_ARCHIVE_ISSUE: issue,
  TEND_SCREENSHOTS: screenshots,
} = process.env;
assert(base && token && issue);
const browser = await chromium.launch({ args: ["--no-proxy-server"] });
try {
  if (screenshots) await mkdir(screenshots, { recursive: true });
  for (const viewport of [
    { width: 1440, height: 1000 },
    { width: 390, height: 844 },
  ]) {
    const context = await browser.newContext({
      viewport,
      colorScheme: "light",
      isMobile: viewport.width < 761,
      hasTouch: viewport.width < 761,
    });
    const page = await context.newPage();
    const errors = [];
    const messagePosts = [];
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (message) => {
      const expectedMissingChat =
        message.location().url === `${base}/api/v1/chats/unknown/messages` &&
        /\b404\b/.test(message.text());
      if (
        message.type() === "error" &&
        !expectedMissingChat &&
        !isAnonymousSessionProbe(message)
      ) {
        errors.push(message.text());
      }
    });
    context.on("request", (request) => {
      if (request.method() === "POST" && request.url().includes("/messages")) {
        messagePosts.push(request.url());
      }
    });
    const unknown = `${base}/?chat=unknown`;
    await page.goto(unknown);
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    await page.getByText("找不到此对话", { exact: true }).waitFor();
    assert.equal(page.url(), unknown);
    assert.equal(await page.locator(".composer").count(), 0);
    assert.equal(await page.locator("textarea").count(), 0);
    const back = page.getByRole("link", { name: "返回默认主对话" });
    assert.equal(await back.getAttribute("href"), "/");
    if (screenshots) {
      await page.screenshot({
        path: join(screenshots, `chat-unknown-${viewport.width}.png`),
      });
    }
    await back.click();
    await page.waitForURL(`${base}/`);
    await page.getByLabel("发消息给 Tend").waitFor();

    await page.goto(`${base}/issues/${issue}`);
    const source = page.getByRole("link", { name: "查看原消息" });
    await source.waitFor();
    assert.equal(
      await source.getAttribute("href"),
      "/?chat=legacy-a#message-original",
    );
    await source.click();
    await page.waitForURL(`${base}/?chat=legacy-a#message-original`);
    await page.locator("#message-original").waitFor();
    await page.getByText("此对话已归档", { exact: true }).waitFor();
    await page.getByText("已按原会话送达待投递的报告。").waitFor();
    assert.equal(await page.locator(".composer").count(), 0);
    assert.equal(
      await page.getByRole("button", { name: "回复", exact: true }).count(),
      0,
    );
    assert.equal(
      await page
        .getByRole("link", { name: `查看 Issue #${issue}` })
        .getAttribute("href"),
      `/issues/${issue}`,
    );
    if (screenshots) {
      await page.screenshot({
        path: join(screenshots, `chat-archived-${viewport.width}.png`),
      });
    }
    const downloadEvent = page.waitForEvent("download");
    await page.getByRole("button", { name: "old-file.md" }).click();
    const download = await downloadEvent;
    assert.equal(
      await readFile(await download.path(), "utf8"),
      "# Historical example\n",
    );
    await back.click();
    await page.waitForURL(`${base}/`);
    await page.getByLabel("发消息给 Tend").waitFor();
    assert.deepEqual(messagePosts, []);
    assert.deepEqual(errors, []);
    await context.close();
  }
} finally {
  await browser.close();
}
