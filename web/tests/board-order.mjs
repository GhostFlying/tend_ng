import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";

const {
  TEND_URL: url,
  TEND_TOKEN: token,
  TEND_BOARD_ORDER_SCREENSHOTS: screenshots,
} = process.env;
assert(url && token);
if (screenshots) await mkdir(screenshots, { recursive: true });

const browser = await chromium.launch({ args: ["--no-proxy-server"] });
try {
  for (const [width, height] of [
    [1440, 1000],
    [390, 844],
  ]) {
    const page = await browser.newPage({
      viewport: { width, height },
      colorScheme: "light",
    });
    const errors = [];
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (message) => {
      if (message.type() === "error") errors.push(message.text());
    });

    await page.goto(url);
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    await page.getByRole("link", { name: "看板", exact: true }).click();
    await page.getByRole("heading", { name: "Issue 看板" }).waitFor();
    assert.equal(new URL(page.url()).origin, url);
    assert.equal(await page.title(), "Tend");
    const column = page.locator('section[aria-labelledby="board-cancelled"]');
    const cards = column.locator(".board-card-title");
    if (width === 390) {
      await page.getByRole("button", { name: /^已取消/ }).click();
      assert.equal(await column.getAttribute("data-mobile"), "true");
    }

    async function assertPage(titles, step) {
      await cards.filter({ hasText: titles[0] }).waitFor();
      await column.scrollIntoViewIfNeeded();
      assert.deepEqual(
        (await cards.allTextContents()).map((title) =>
          title.replace(/^#\d+\s+/, ""),
        ),
        titles,
      );
      assert.equal(
        await page.evaluate(() => document.documentElement.scrollWidth),
        width,
      );
      assert.equal(await page.locator("vite-error-overlay").count(), 0);
      if (screenshots && step) {
        await page.evaluate(() => document.fonts.ready);
        await page.screenshot({
          path: join(screenshots, `board-order-${width}-${step}.png`),
        });
      }
    }

    const recent = ["离开后重新进入", "最近取消"];
    const older = ["历史卡片 Z（入列时间未知）", "历史卡片 A（入列时间未知）"];
    await assertPage(recent, "newer");
    const response = page.waitForResponse((candidate) => {
      const target = new URL(candidate.url());
      return (
        target.pathname === "/api/v1/issue-board" &&
        target.searchParams.get("before") === "v1.0.z-fixture-01"
      );
    });
    await page.getByRole("button", { name: "较早", exact: true }).click();
    assert.equal((await response).status(), 200);
    await assertPage(older, "older");
    assert.equal(
      await page
        .getByRole("button", { name: "较早", exact: true })
        .isDisabled(),
      true,
    );
    await page.getByRole("button", { name: "较新", exact: true }).click();
    await assertPage(recent);
    assert.deepEqual(errors, []);
    await page.close();
  }
} finally {
  await browser.close();
}
console.log(
  "PASS desktop and mobile board ordering and real HTTP cursor paging",
);
