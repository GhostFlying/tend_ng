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
    const parent = "示例仓库上手指南";
    if (width === 390) {
      await page.getByRole("button", { name: /^已完成/ }).click();
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
    await card.getByRole("link", { name: parent, exact: true }).click();
    await page.getByRole("heading", { name: parent, exact: true }).waitFor();
    await page.getByRole("link", { name: "看板", exact: true }).click();
    if (width === 390) {
      await page.getByRole("button", { name: /^待安排/ }).click();
    } else {
      await page.getByRole("button", { name: "展开已取消" }).click();
      await page.getByRole("button", { name: "收起已取消" }).waitFor();
      await page.getByRole("button", { name: "收起已取消" }).click();
    }
    await page.getByRole("link", { name: "稍后整理发布说明" }).click();
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
    "PASS read-only Issue board and PR links in Chromium on desktop and mobile",
  );
} finally {
  await browser.close();
}
