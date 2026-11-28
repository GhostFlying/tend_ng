import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";
import { collectPageErrors } from "./console-errors.mjs";

const {
  TEND_URL: url,
  TEND_TOKEN: token,
  TEND_SCREENSHOTS: screenshots,
  TITLE_ISSUE: issue,
  TITLE_SOURCE: source,
  TITLE_BACKLOG: backlog,
} = process.env;
assert(url && token && issue && source && backlog);
const browser = await chromium.launch({ args: ["--no-proxy-server"] });
const errors = [];

try {
  for (const [label, width, height] of [
    ["desktop", 1440, 1000],
    ["narrow", 390, 844],
    ["minimum", 320, 800],
  ]) {
    const page = await browser.newPage({ viewport: { width, height } });
    collectPageErrors(page, errors);
    const writes = [];
    page.on("request", (request) => {
      if (!["GET", "HEAD"].includes(request.method()))
        writes.push(new URL(request.url()).pathname);
    });
    await page.goto(`${url}/issues/${issue}`);
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    const title = `#${issue} 核对示例仓库的安装步骤与首次运行命令`;
    await page.getByRole("heading", { name: title, exact: true }).waitFor();
    const origin = page.getByRole("link", { name: "查看原消息", exact: true });
    assert.equal(await origin.getAttribute("href"), `/#message-${source}`);
    const disclosure = page.locator(".issue-title-history");
    await disclosure.getByText("改名历史", { exact: true }).click();
    const rows = disclosure.locator("ol li");
    await rows.nth(2).waitFor();
    assert.equal(await rows.count(), 3);
    assert.deepEqual(await rows.locator("strong").allTextContents(), [
      "用户",
      "worker",
      "worker",
    ]);
    assert.deepEqual(await rows.locator("dd").allTextContents(), [
      "核对安装步骤与首次运行示例",
      "核对示例仓库的安装步骤与首次运行命令",
      "补齐安装命令并验证首次运行",
      "核对安装步骤与首次运行示例",
      "准备示例仓库使用说明",
      "补齐安装命令并验证首次运行",
    ]);
    for (const time of await rows.locator("time").all())
      assert(Number.isFinite(Date.parse(await time.getAttribute("datetime"))));
    assert.equal(await disclosure.locator("button,input,textarea").count(), 0);
    assert.equal(
      await page.getByRole("button", { name: /^(改名|编辑标题)$/ }).count(),
      0,
    );
    await page.evaluate(() => document.fonts.ready);
    assert.equal(
      await page.evaluate(() => document.documentElement.scrollWidth),
      width,
    );
    const last = await rows.last().boundingBox();
    assert(last.x >= 0 && last.x + last.width <= width);
    assert.deepEqual(writes, ["/api/v1/web/session"]);
    if (screenshots && label !== "minimum") {
      await mkdir(screenshots, { recursive: true });
      await page.screenshot({
        path: join(screenshots, `issue-title-${label}.png`),
      });
    }

    // A valid Web session and same-origin browser request cannot rename.
    const denied = await page.request.post(
      `${url}/api/v1/issues/${issue}/title`,
      {
        headers: { Origin: url },
        data: { operation_id: "web-denied", title: "Unsupported" },
      },
    );
    assert.equal(denied.status(), 401);

    await page.goto(`${url}/issues/${backlog}`);
    await page.getByText("改名历史", { exact: true }).click();
    const earlier = page.getByRole("button", { name: "更早改名", exact: true });
    assert.equal(await rows.count(), 12);
    await earlier.click();
    await rows.nth(14).waitFor();
    assert.equal(await rows.count(), 15);
    assert.equal(
      await rows.last().locator("dd").first().textContent(),
      "稍后撰写发布说明",
    );
    assert.equal(await earlier.count(), 0);
    await page.close();
  }
  assert.deepEqual(errors, []);
  console.log(
    "PASS read-only rename history on real desktop and narrow Issue pages",
  );
} finally {
  await browser.close();
}
