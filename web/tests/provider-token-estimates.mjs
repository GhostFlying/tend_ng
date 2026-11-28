import assert from "node:assert/strict";
import { join } from "node:path";
import { chromium } from "playwright";
import { isAnonymousSessionProbe } from "./console-errors.mjs";

const {
  TEND_URL: url,
  TEND_TOKEN: token,
  TEND_SCREENSHOTS: images,
} = process.env;
const [issue] = process.argv.slice(2);
assert(url && token && images && issue);
assert.equal(new URL(url).hostname, "127.0.0.1");
const browser = await chromium.launch({ args: ["--no-proxy-server"] });

function model(container, name) {
  return container.locator(".model-row").filter({ hasText: name });
}

async function expand(container, name, amount) {
  const row = model(container, name);
  await row.getByText(amount, { exact: true }).waitFor();
  await row.locator("summary").click();
  await row.locator(".model-fee-items").first().waitFor();
  return row;
}

async function fit(page, width) {
  const [expected, actual] = await page.evaluate(() => [
    document.documentElement.clientWidth,
    document.documentElement.scrollWidth,
  ]);
  assert.equal(expected, width);
  assert(actual <= expected + 1);
  assert.equal(await page.title(), "Tend");
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
    const queries = [];
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (message) => {
      if (
        !isAnonymousSessionProbe(message) &&
        ["warning", "error"].includes(message.type())
      )
        errors.push(message.text());
    });
    page.on("request", (request) => {
      if (new URL(request.url()).pathname === "/api/v1/token-usage")
        queries.push(request.url());
    });

    assert.equal((await page.goto(`${url}/token-usage`))?.status(), 200);
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    await page.getByRole("heading", { level: 1, name: "Token 用量" }).waitFor();
    assert.equal(page.url(), `${url}/token-usage`);
    const bySource = page.locator(".usage-source-list");
    await bySource.locator("summary").first().click();
    const summary = bySource.locator(".model-usage");
    const all = await expand(summary, "deepseek-v4.1-flash", "$4.5");
    await all.getByText("已计 420 / 420 条", { exact: true }).first().waitFor();
    assert.equal(
      queries.length,
      1,
      "expanding a source or model must not refetch",
    );
    await fit(page, width);
    await all.scrollIntoViewIfNeeded();
    await page.screenshot({
      path: join(images, `runner-deepseek-${width}.png`),
    });

    assert.equal((await page.goto(`${url}/issues/${issue}`))?.status(), 200);
    const heading = page.getByRole("heading", {
      level: 1,
      name: /DeepSeek 180 条/,
    });
    await heading.waitFor();
    assert.equal(page.url(), `${url}/issues/${issue}`);
    await page.locator(".token-section > summary").click();
    const pane = page.locator(".token-pane");
    const detail = await expand(pane, "deepseek-v4.1-flash", "$0.1");
    await detail.getByText("未命中输入（miss）", { exact: true }).waitFor();
    await detail
      .getByText("已计 180 / 180 条", { exact: true })
      .first()
      .waitFor();
    await fit(page, width);
    await detail.scrollIntoViewIfNeeded();
    await page.screenshot({
      path: join(images, `issue-deepseek-${width}.png`),
    });

    await detail.locator("summary").click();
    const gpt = await expand(pane, "gpt-6.2-sol", "$0.3");
    await gpt.getByText(/缓存读折扣未观测/).waitFor();
    await gpt.getByText(/可能有未计入的写入溢价/).waitFor();
    await gpt.getByText("未细分输入（基准）").waitFor();
    await gpt.getByText("长上下文", { exact: false }).first().waitFor();
    await fit(page, width);
    if (width === 390)
      await gpt.locator(".model-rate-tier").first().scrollIntoViewIfNeeded();
    else await gpt.scrollIntoViewIfNeeded();
    await page.screenshot({
      path: join(images, `issue-gpt-baseline-${width}.png`),
    });
    assert.deepEqual(errors, []);
    console.log(
      `PASS ${width}×${height} real global and Issue pricing, detail expansion without refetch, no horizontal overflow or unexpected console errors`,
    );
    await page.close();
  }
} finally {
  await browser.close();
}
