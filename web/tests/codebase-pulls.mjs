import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";
import { collectPageErrors } from "./console-errors.mjs";

const {
  TEND_URL: url,
  TEND_TOKEN: token,
  TEND_CODEBASE_SCREENSHOTS: screenshots,
  TEND_CODEBASE_PARENT: parent,
  TEND_CODEBASE_CHILD: child,
} = process.env;
assert(url && token && screenshots && parent && child);
const mr = "https://code.byted.org/example/guide/merge_requests/";
const pr = "https://github.com/example/guide/pull/73";
const browser = await chromium.launch({ args: ["--no-proxy-server"] });

function link(scope, label) {
  return scope.getByRole("link", { name: label, exact: true });
}

async function assertPulls(scope) {
  for (const [label, href] of [
    ["example/guide !73", mr + "73"],
    ["example/guide !74", mr + "74"],
    ["example/guide !76", mr + "76"],
    ["example/guide !99", mr + "99"],
    ["example/guide !204", mr + "204"],
    ["example/guide #73", pr],
  ]) {
    const item = link(scope, label);
    await item.waitFor();
    assert.equal(await item.count(), 1);
    assert.equal(await item.getAttribute("href"), href);
    assert.equal(await item.getAttribute("target"), "_blank");
  }
  await scope.getByText("CI 未确认", { exact: true }).waitFor();
  await scope.getByText("上次：CI 通过", { exact: true }).waitFor();
  await scope.getByText("CI 失败", { exact: true }).waitFor();
  await scope.getByText("CI 进行中", { exact: true }).waitFor();
  assert.equal(await scope.getByText("暂不可观测", { exact: true }).count(), 2);
}

async function capture(page, width, name) {
  assert.equal(await page.title(), "Tend");
  assert.equal(
    await page.evaluate(() => document.documentElement.scrollWidth),
    width,
  );
  await page.evaluate(() => document.fonts.ready);
  await page.screenshot({
    path: join(screenshots, `codebase-${name}-${width}.png`),
  });
}

try {
  await mkdir(screenshots, { recursive: true });
  for (const [width, height] of [
    [1440, 1000],
    [390, 844],
  ]) {
    const page = await browser.newPage({
      viewport: { width, height },
      colorScheme: "light",
    });
    const errors = [];
    collectPageErrors(page, errors);
    await page.goto(url);
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    await link(page, "看板").click();
    await page.getByRole("heading", { name: "Issue 看板" }).waitFor();
    if (width === 390) {
      await page.getByRole("button", { name: /^已完成/ }).click();
    }
    const title = `#${parent} 示例仓库上手指南`;
    const card = page.locator(".board-card").filter({ has: link(page, title) });
    await assertPulls(card);
    await card.scrollIntoViewIfNeeded();
    const bounds = await card.boundingBox();
    assert(bounds && bounds.x >= 0 && bounds.x + bounds.width <= width);
    await capture(page, width, "board");
    await link(card, title).click();
    await page.waitForURL(`${url}/issues/${parent}`);
    const properties = page.getByRole("complementary", { name: "工作属性" });
    await link(properties, `#${child} 核对安装步骤`).click();
    await page.waitForURL(`${url}/issues/${child}`);
    await page
      .getByRole("heading", { name: `#${child} 核对安装步骤`, exact: true })
      .waitFor();
    const pulls = page
      .getByRole("heading", { name: "关联 PR / MR", exact: true })
      .locator("..");
    await assertPulls(pulls);
    if (width === 390) await pulls.scrollIntoViewIfNeeded();
    await capture(page, width, "issue");
    assert.deepEqual(errors, []);
    await page.close();
  }
} finally {
  await browser.close();
}
