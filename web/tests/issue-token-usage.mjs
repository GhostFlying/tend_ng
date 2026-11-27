import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";
import { isAnonymousSessionProbe } from "./console-errors.mjs";

const {
  TEND_URL: url,
  TEND_TOKEN: token,
  TEND_SCREENSHOTS: images,
} = process.env;
const [root, missing, empty] = process.argv.slice(2);
assert(url && token && root && missing && empty);
// The harness must only render and interact with the isolated fixture Server.
assert.equal(new URL(url).hostname, "127.0.0.1");
if (images) await mkdir(images, { recursive: true });
const browser = await chromium.launch({ args: ["--no-proxy-server"] });

async function noOverflow(page, width) {
  const sizes = await page.evaluate(() => {
    const dimensions = (node) => [node.scrollWidth, node.clientWidth];
    return [
      dimensions(document.documentElement),
      dimensions(document.querySelector("main")),
      dimensions(document.querySelector(".token-pane")),
    ];
  });
  assert.equal(sizes[0][0], width);
  for (const [scroll, client] of sizes) assert(scroll <= client + 1, sizes);
}

async function go(page, number) {
  await page.goto(`${url}/issues/${number}`);
  assert.equal(new URL(page.url()).pathname, `/issues/${number}`);
  assert.equal(await page.title(), "Tend");
  await page
    .getByRole("heading", { level: 1, name: new RegExp(`#${number} `) })
    .waitFor();
  assert.equal(await page.locator("vite-error-overlay").count(), 0);
}

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
    let injectFailure = false;
    let expectedFailures = 0;
    let interceptedUsage = 0;
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (message) => {
      if (isAnonymousSessionProbe(message)) return;
      if (
        /\/token-usage$/.test(message.location().url) &&
        /\b503\b/.test(message.text())
      ) {
        expectedFailures++;
        return;
      }
      if (message.type() === "error" || message.type() === "warning")
        errors.push(message.text());
    });
    page.on("request", (request) => {
      if (new URL(request.url()).pathname.endsWith("/token-usage"))
        interceptedUsage++;
    });
    await page.goto(`${url}/issues/${root}`);
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    await page
      .getByRole("heading", { level: 1, name: new RegExp(`#${root} `) })
      .waitFor();
    const pane = page.locator(".token-pane");
    const open = page.locator(".token-section > summary");
    const total = pane.locator(".token-total > bdi");
    assert.equal(
      interceptedUsage,
      0,
      "collapsed details must not start a read",
    );
    await open.click();
    await total.getByText("41,200", { exact: true }).waitFor();
    const reasoning = pane.locator(".token-metrics > div").filter({
      has: page.getByText("推理输出", { exact: true }),
    });
    assert.equal(await reasoning.locator("bdi").textContent(), "—");
    await reasoning.getByText("字段缺失", { exact: true }).waitFor();
    const models = pane.getByRole("region", { name: "模型明细与费用估算" });
    await models.getByText("gpt-6.2-sol", { exact: true }).waitFor();
    await models.getByText("gpt-6.2-astra", { exact: true }).waitFor();
    assert.equal(await models.getByText("deepseek-v4.1-flash").count(), 0);
    const directAstra = models
      .locator(".model-row > summary")
      .filter({ hasText: "gpt-6.2-astra" });
    const directAstraFields = directAstra.locator(".model-line-fields");
    for (const [label, value] of [
      ["缓存读取", "4,096"],
      ["缓存写入", "2,000"],
      ["输出", "400"],
    ]) {
      const field = directAstraFields.locator(".model-line-field").filter({
        has: page.getByText(label, { exact: true }),
      });
      assert.equal(await field.locator("bdi").textContent(), value);
    }
    assert.equal(
      await models.getByText("非缓存输入", { exact: true }).count(),
      0,
    );
    await directAstra.click();
    const feeItems = models.locator(".model-fee-items");
    for (const label of ["缓存读取", "缓存写入", "输出"]) {
      await feeItems.getByText(label, { exact: true }).first().waitFor();
    }
    assert.equal(
      await feeItems.locator("dt").filter({ hasText: "非缓存输入" }).count(),
      0,
    );
    await directAstra.click();
    await pane.getByRole("button", { name: "含所有后代" }).click();
    await total.getByText("127,200", { exact: true }).waitFor();
    await models.getByText("deepseek-v4.1-flash", { exact: true }).waitFor();
    const childFlash = models
      .locator(".model-row > summary")
      .filter({ hasText: "deepseek-v4.1-flash" });
    await childFlash.getByText("86,000", { exact: true }).waitFor();
    await pane
      .getByText("已观测 2 / 3 个已开始 Run", { exact: true })
      .waitFor();
    await pane
      .getByText("已开始但无可用用量事件 · 1 个", { exact: true })
      .waitFor();
    await pane
      .getByText("3 / 4 条请求有此字段", { exact: true })
      .first()
      .waitFor();
    const rows = pane.getByRole("list", { name: "各 Issue 直属用量" });
    assert.equal(await rows.getByRole("listitem").count(), 3);
    assert.equal(await rows.getByText("86,000", { exact: true }).count(), 1);
    assert.equal(await rows.getByText("41,200", { exact: true }).count(), 1);
    assert.equal(
      interceptedUsage,
      1,
      "switching scope reuses the same snapshot",
    );
    await noOverflow(page, width);
    if (images) {
      await open.evaluate((element, desktop) => {
        if (!desktop) return element.scrollIntoView({ block: "start" });
        const main = element.closest("main");
        main.scrollTop += Math.max(0, element.getBoundingClientRect().top - 76);
      }, width === 1440);
      await page.screenshot({
        path: join(images, `issue-token-usage-${width}.png`),
      });
      await models.evaluate((element) =>
        element.scrollIntoView({ block: "center" }),
      );
      await page.screenshot({
        path: join(images, `hide-uncached-issue-${width}.png`),
      });
    }
    await pane.getByRole("button", { name: "直属用量" }).click();
    await total.getByText("41,200", { exact: true }).waitFor();
    assert.equal(await rows.count(), 0);

    // A retry keeps the selected scope and moves keyboard focus to the result.
    const read = `${url}/api/v1/issues/*/token-usage`;
    await page.route(read, async (route) => {
      if (injectFailure) {
        injectFailure = false;
        return route.fulfill({
          status: 503,
          json: { error: "Fixture unavailable" },
        });
      }
      return route.continue();
    });
    await pane.getByRole("button", { name: "含所有后代" }).click();
    injectFailure = true;
    await pane.getByRole("button", { name: "更新用量" }).click();
    await pane
      .getByText("未显示本次用量，实际用量未知。", { exact: true })
      .waitFor();
    assert.equal(await total.count(), 0);
    await pane.getByRole("button", { name: "重试" }).focus();
    await page.keyboard.press("Enter");
    await total.getByText("127,200", { exact: true }).waitFor();
    await page.waitForFunction(
      () => document.activeElement?.textContent === "已上报总量",
    );
    await page.unroute(read);

    // Exercise exact rendering above JS Number's safe integer range.
    await page.route(read, async (route) => {
      const original = await route.fetch();
      const usage = await original.json();
      usage.subtree.total_tokens.tokens = "9007199254740993";
      return route.fulfill({ response: original, json: usage });
    });
    await pane.getByRole("button", { name: "更新用量" }).click();
    await total.getByText("9,007,199,254,740,993", { exact: true }).waitFor();
    await noOverflow(page, width);
    await page.unroute(read);

    await go(page, missing);
    await open.click();
    await pane.getByText("完全未观测", { exact: true }).waitFor();
    assert.equal(await total.textContent(), "—");
    await noOverflow(page, width);
    await go(page, empty);
    await open.click();
    await pane.getByText("没有已开始的 Run。", { exact: true }).waitFor();
    assert.equal(await total.textContent(), "—");
    await noOverflow(page, width);
    assert.deepEqual(errors, [], `console and script errors at ${width}px`);
    assert.equal(
      expectedFailures,
      1,
      "only the deliberate failure is tolerated",
    );
    await page.close();
    console.log(
      `PASS ${width}×${height} real Issue page: direct/subtree, unknown/zero, retry focus, no overflow or unexpected console errors`,
    );
  }
} finally {
  await browser.close();
}
