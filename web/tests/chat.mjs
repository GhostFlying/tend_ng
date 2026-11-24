import assert from "node:assert/strict";
import { mkdir, readFile } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";

const mode = process.argv[2];
const base = process.env.TEND_URL;
const token = process.env.TEND_TOKEN;
const screenshots = process.env.TEND_SCREENSHOTS;
const request = "在示例仓库整理一份使用说明，完成后把文件和简短结论发回来。";
const answer = "使用说明已整理好，包含环境准备和第一条运行命令。";
const contents = "A fixture guide.\n".repeat(32768);
assert(base && token);

const browser = await chromium.launch({ args: ["--no-proxy-server"] });
try {
  if (mode === "send") {
    const context = await browser.newContext();
    const page = await context.newPage();
    const errors = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await page.goto(base);
    await page.getByLabel("访问令牌").fill("fixture-invalid-token");
    await page.getByRole("button", { name: "连接", exact: true }).click();
    await page.getByText("访问令牌无效", { exact: true }).waitFor();
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();

    const operations = [];
    await page.route("**/api/v1/chats/main/messages", async (route) => {
      if (route.request().method() !== "POST") return route.continue();
      operations.push(route.request().postDataJSON());
      if (operations.length === 1) {
        const response = await route.fetch();
        assert.equal(response.status(), 200);
        await route.abort();
        return;
      }
      await route.continue();
    });
    await page.getByLabel("发消息给 Tend").fill(request);
    await page.getByLabel("发消息给 Tend").press("Enter");
    await page.getByText(/无法确认收件/).waitFor();
    await page.reload();
    const draft = page.getByLabel("发消息给 Tend");
    await draft.waitFor();
    assert.equal(await draft.inputValue(), request);
    const receipt = page.waitForResponse(
      (response) =>
        response.url().endsWith("/api/v1/chats/main/messages") &&
        response.request().method() === "POST" &&
        response.status() === 200,
    );
    await page.getByRole("button", { name: "发送", exact: true }).click();
    assert.deepEqual(operations[0], operations[1]);
    assert.equal(operations.length, 2);
    assert.deepEqual(errors, []);
    console.log(JSON.stringify(await (await receipt).json()));
  } else {
    for (const viewport of [
      { width: 1440, height: 1000 },
      { width: 390, height: 844 },
    ]) {
      const context = await browser.newContext({
        viewport,
        colorScheme: "light",
        isMobile: viewport.width < 761,
        hasTouch: viewport.width < 761,
        acceptDownloads: true,
      });
      const page = await context.newPage();
      const errors = [];
      page.on("pageerror", (error) => errors.push(error.message));
      page.on("console", (message) => {
        if (message.type() === "error") errors.push(message.text());
      });
      await page.goto(base);
      await page.getByLabel("访问令牌").fill(token);
      await page.getByRole("button", { name: "连接", exact: true }).click();
      await page.getByText(answer, { exact: true }).waitFor();
      assert.equal(await page.title(), "Tend");
      assert.equal(
        await page
          .locator(".message-user")
          .getByText(request, { exact: true })
          .count(),
        1,
      );
      await page.reload();
      const attachment = page.getByRole("button", { name: "下载 guide.txt" });
      await attachment.waitFor();
      assert.equal(await attachment.count(), 1);
      const reference = page.getByRole("link", { name: request });
      assert.equal(await reference.count(), mode === "read-repeat" ? 2 : 1);
      for (const link of await reference.all()) {
        assert.match((await link.getAttribute("href")) ?? "", /^#message-/);
      }
      if (mode === "read-repeat") {
        assert.equal(
          await page
            .getByText("I also verified the repository command.", {
              exact: true,
            })
            .count(),
          1,
        );
      }
      assert.equal(await page.getByText(answer, { exact: true }).count(), 1);
      assert.equal(
        await page.evaluate(() => document.documentElement.scrollWidth),
        viewport.width,
      );
      const composer = await page.locator(".composer").boundingBox();
      const send = await page
        .getByRole("button", { name: "发送", exact: true })
        .boundingBox();
      assert.equal(composer.width, viewport.width === 1440 ? 760 : 366);
      assert.equal(composer.x, viewport.width === 1440 ? 484 : 12);
      assert.equal(composer.height, 128);
      assert.equal(send.width, viewport.width === 1440 ? 32 : 44);
      assert.equal(send.height, send.width);
      assert.equal(
        await page
          .locator(".header")
          .evaluate((header) => header.getBoundingClientRect().height),
        52,
      );
      if (screenshots) {
        await mkdir(screenshots, { recursive: true });
        await page.screenshot({
          path: join(screenshots, `chat-${viewport.width}.png`),
        });
      }
      const completed = page.waitForEvent("download");
      await attachment.click();
      const downloaded = await completed;
      assert.equal(downloaded.suggestedFilename(), "guide.txt");
      assert.equal(await readFile(await downloaded.path(), "utf8"), contents);
      assert.deepEqual(errors, []);
      await context.close();
    }
    console.log(
      "PASS browser send receipt, original reply, download, and desktop/mobile layout",
    );
  }
} finally {
  await browser.close();
}
