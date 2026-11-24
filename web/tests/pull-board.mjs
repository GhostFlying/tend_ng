import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";

const {
  TEND_URL: url,
  TEND_TOKEN: token,
  TEND_BOARD_SCREENSHOTS: screenshots,
} = process.env;
assert(url && token);
const browser = await chromium.launch({ args: ["--no-proxy-server"] });

try {
  for (const [width, height] of [
    [1440, 960],
    [390, 844],
  ]) {
    const page = await browser.newPage({ viewport: { width, height } });
    const errors = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await page.goto(url);
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    await page.getByRole("link", { name: "看板", exact: true }).click();
    await page.getByRole("heading", { name: "Issue 看板" }).waitFor();
    const card = page
      .locator(".board-card")
      .filter({ has: page.getByRole("link", { name: "核对安装步骤" }) });
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
    await page.getByRole("link", { name: "整理发布记录" }).waitFor();
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
    await card.getByRole("link", { name: "核对安装步骤" }).click();
    await page.getByRole("heading", { name: "核对安装步骤" }).waitFor();
    assert.deepEqual(errors, []);
  }
  console.log(
    "PASS read-only Issue board and PR links in Chromium on desktop and mobile",
  );
} finally {
  await browser.close();
}
