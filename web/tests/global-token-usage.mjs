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
const [root, missing, metered, empty, parent] = process.argv.slice(2);
assert(url && token && root && missing && metered && empty && parent);
assert.equal(new URL(url).hostname, "127.0.0.1");
if (images) await mkdir(images, { recursive: true });
const browser = await chromium.launch({ args: ["--no-proxy-server"] });
const direct = `${url}/token-usage`;
const endpoint = "/api/v1/token-usage";

async function ready(page) {
  await page.getByRole("heading", { level: 1, name: "Token 用量" }).waitFor();
  await page
    .locator(".usage-overview .token-total bdi")
    .getByText("127,200", { exact: true })
    .waitFor();
  assert.equal(page.url(), direct);
  assert.equal(await page.title(), "Tend");
  assert.equal(await page.locator("vite-error-overlay").count(), 0);
}

async function noOverflow(page, width) {
  const sizes = await page.evaluate(() =>
    [
      document.documentElement,
      document.querySelector("main"),
      document.querySelector(".usage-page-inner"),
    ].map((node) => [node.scrollWidth, node.clientWidth]),
  );
  assert.equal(sizes[0][1], width);
  for (const [scroll, client] of sizes) assert(scroll <= client + 1, sizes);
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
    let injected = 0;
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (message) => {
      if (isAnonymousSessionProbe(message)) return;
      if (
        message.type() === "error" &&
        message.location().url.includes(`${endpoint}?parent=`) &&
        /\b503\b/.test(message.text())
      ) {
        injected++;
        return;
      }
      if (message.type() === "error" || message.type() === "warning")
        errors.push(message.text());
    });
    page.on("request", (request) => {
      const requestURL = new URL(request.url());
      if (requestURL.pathname === endpoint) queries.push(requestURL.search);
    });

    // Follow the shared address in a new, signed-out context before navigating from either menu.
    assert.equal((await page.goto(direct))?.status(), 200);
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    await ready(page);
    assert.deepEqual(queries, [""], "one root page, no eager child reads");
    assert.equal((await page.reload())?.status(), 200);
    await ready(page);
    assert.deepEqual(queries, ["", ""]);

    if (width === 390) {
      const phone = page.getByRole("navigation", { name: "手机导航" });
      assert.equal(
        await phone
          .getByRole("link", { name: "更多" })
          .getAttribute("aria-current"),
        "page",
      );
      await phone.getByRole("link", { name: "更多" }).click();
      const menu = page.getByRole("navigation", { name: "更多导航" });
      for (const name of ["Agents", "主机", "Token 用量"])
        await menu.getByRole("link", { name, exact: true }).waitFor();
      if (images)
        await page.screenshot({
          path: join(images, "token-usage-mobile-more.png"),
        });
      await menu.getByRole("link", { name: "Token 用量" }).click();
    } else {
      const sidebar = page.getByRole("navigation", { name: "主导航" });
      assert.equal(
        await sidebar
          .getByRole("link", { name: "Token 用量" })
          .getAttribute("aria-current"),
        "page",
      );
      await sidebar.getByRole("link", { name: "Agents" }).click();
      await page.getByRole("heading", { level: 1, name: "Agents" }).waitFor();
      await sidebar.getByRole("link", { name: "Token 用量" }).click();
    }
    await ready(page);
    assert.deepEqual(queries, ["", "", ""]);
    const coverage = page.locator(".usage-overview .token-coverage");
    const method = page.locator(".usage-overview .usage-method > summary");
    await method.click();
    await coverage.getByText("部分覆盖", { exact: true }).waitFor();
    await coverage
      .getByText("已观测 2 / 3 个已开始 Issue Run", { exact: true })
      .waitFor();
    await method.click();
    const tree = page.getByRole("list", { name: "顶层 Issue", exact: true });
    assert.equal(await tree.locator(":scope > li").count(), 30);
    const emptyRow = tree.locator(":scope > li").filter({
      has: page.getByRole("link", { name: new RegExp(`^#${empty} `) }),
    });
    assert((await emptyRow.textContent()).includes("没有已开始的 Run"));
    const rootRow = tree.locator(":scope > li").filter({
      has: page.getByRole("link", { name: new RegExp(`^#${root} `) }),
    });
    assert.deepEqual(
      await rootRow.locator(".usage-tree-tokens").allTextContents(),
      ["41,200", "127,200"],
    );

    // Only an expanded branch reads children, and a retry does not reset siblings.
    const childrenRead = `${url}${endpoint}?parent=${parent}`;
    await page.route(
      childrenRead,
      (route) =>
        route.fulfill({ status: 503, json: { error: "Fixture unavailable" } }),
      { times: 1 },
    );
    await rootRow.getByRole("button", { name: `展开 #${root} 的子级` }).click();
    await rootRow.getByRole("alert").waitFor();
    assert.equal(
      await rootRow.getByRole("alert").textContent(),
      "暂时无法读取直接子级。",
    );
    assert.equal(await tree.locator(":scope > li").count(), 30);
    await rootRow.getByRole("button", { name: "重试" }).click();
    const children = page.getByRole("list", { name: `#${root} 的直接子级` });
    const meteredLink = children.getByRole("link", {
      name: new RegExp(`^#${metered} `),
    });
    await meteredLink.waitFor();
    const missingRow = children.locator(":scope > li").filter({
      has: page.getByRole("link", { name: new RegExp(`^#${missing} `) }),
    });
    assert((await missingRow.textContent()).includes("完全未观测"));
    assert.deepEqual(
      await missingRow.locator(".usage-tree-tokens").allTextContents(),
      ["—", "—"],
    );
    const meteredRow = children.locator(":scope > li").filter({
      has: page.getByRole("link", { name: new RegExp(`^#${metered} `) }),
    });
    assert.deepEqual(
      await meteredRow.locator(".usage-tree-tokens").allTextContents(),
      ["86,000", "86,000"],
    );
    assert((await meteredRow.textContent()).includes("已观测全部 Run"));
    const overview = page.locator(".usage-overview .model-usage");
    await page
      .locator(".usage-headline-amount")
      .getByText("$0.2", { exact: true })
      .waitFor();
    const astra = overview
      .locator(".model-row > summary")
      .filter({ hasText: "gpt-6.2-astra" });
    const astraFields = astra.locator(".model-line-fields");
    for (const [label, value] of [
      ["其中缓存读取", "4,096"],
      ["输出", "400"],
    ]) {
      const field = astraFields.locator(".model-line-field").filter({
        has: page.getByText(label, { exact: true }),
      });
      assert.equal(await field.locator("bdi").last().textContent(), value);
    }
    assert.equal(
      await astraFields.getByText("非缓存输入", { exact: true }).count(),
      0,
    );
    const sol = overview
      .locator(".model-row > summary")
      .filter({ hasText: "gpt-6.2-sol" });
    assert.equal(await sol.getByText("缓存写入").count(), 0);
    const solFields = sol.locator(".model-line-fields");
    for (const [label, value] of [
      ["其中缓存读取", "8,192"],
      ["输出", "800"],
    ]) {
      const field = solFields.locator(".model-line-field").filter({
        has: page.getByText(label, { exact: true }),
      });
      assert.equal(await field.locator("bdi").last().textContent(), value);
    }
    assert.equal(
      await solFields.getByText("非缓存输入", { exact: true }).count(),
      0,
    );
    const flash = overview
      .locator(".model-row > summary")
      .filter({ hasText: "deepseek-v4.1-flash" });
    assert.equal(await flash.getByText("缓存写入").count(), 0);
    await flash.click();
    await overview.getByText("原生回合所选 1 条", { exact: false }).waitFor();
    const flashRaw = flash.locator("..").locator(".model-raw");
    await flashRaw
      .locator("div")
      .filter({
        has: page.getByText("缓存写入", { exact: true }),
      })
      .getByText("0", { exact: true })
      .waitFor();
    await flash.click();
    await astra.click();
    const priceLink = overview.getByRole("link", {
      name: "openai/gpt-6.2-astra",
    });
    await priceLink.waitFor();
    assert.equal(
      await priceLink.getAttribute("href"),
      "https://openrouter.ai/openai/gpt-6.2-astra",
    );
    assert.equal(await astra.locator(".model-cost").textContent(), "$0.1");
    const feeItems = overview.locator(".model-fee-items");
    const writeFee = feeItems.locator("div").filter({
      has: page.getByText("缓存写入", { exact: true }),
    });
    assert.equal(await writeFee.locator(".model-cost").textContent(), "<$0.1");
    for (const label of ["缓存读取", "缓存写入", "输出"]) {
      await feeItems.getByText(label, { exact: true }).first().waitFor();
    }
    assert.equal(
      await feeItems.locator("dt").filter({ hasText: "非缓存输入" }).count(),
      0,
    );
    await noOverflow(page, width);
    if (images && width === 1440) {
      await overview.evaluate((node) => {
        node.closest("main").scrollTop = 0;
      });
      await page.screenshot({
        path: join(images, "hide-uncached-global-1440.png"),
      });
    }
    await astra.click();
    await rootRow.getByText(`#${root} 模型明细`, { exact: true }).click();
    const directModels = rootRow.getByRole("region", { name: "直属模型明细" });
    const subtreeModels = rootRow.getByRole("region", {
      name: "含后代模型明细",
    });
    await directModels.getByText("gpt-6.2-sol", { exact: true }).waitFor();
    await subtreeModels
      .getByText("deepseek-v4.1-flash", { exact: true })
      .waitFor();
    assert.equal(
      await directModels.getByText("deepseek-v4.1-flash").count(),
      0,
    );
    await rootRow.getByText(`#${root} 模型明细`, { exact: true }).click();
    assert.deepEqual(queries, [
      "",
      "",
      "",
      `?parent=${parent}`,
      `?parent=${parent}`,
    ]);
    await noOverflow(page, width);
    if (images) {
      await overview.evaluate((node) => {
        node.closest("main").scrollTop = 0;
      });
      await page.screenshot({ path: join(images, `token-usage-${width}.png`) });
      if (width === 390) {
        await overview.evaluate((node) => {
          const main = node.closest("main");
          main.scrollTop += node.getBoundingClientRect().top - 70;
        });
        await page.screenshot({
          path: join(images, "hide-uncached-global-390.png"),
        });
        await page
          .getByRole("heading", { name: "Issue 树" })
          .evaluate((node) => node.scrollIntoView({ block: "start" }));
        await page.screenshot({
          path: join(images, "token-usage-mobile-tree.png"),
        });
      }
    }

    await page.getByRole("button", { name: "加载更多" }).click();
    await page.waitForFunction(
      () =>
        document.querySelectorAll(
          '.usage-tree-list[aria-label="顶层 Issue"] > li',
        ).length === 34,
    );
    assert.match(queries.at(-1), /^\?before=v1\./);
    assert.equal(
      await page.getByRole("button", { name: "加载更多" }).count(),
      0,
    );
    await meteredLink.click();
    await page
      .getByRole("heading", { level: 1, name: new RegExp(`^#${metered} `) })
      .waitFor();
    assert.equal(new URL(page.url()).pathname, `/issues/${metered}`);
    await page.locator(".token-section > summary").click();
    await page
      .locator(".token-section .token-total bdi")
      .getByText("86,000", { exact: true })
      .waitFor();
    assert.deepEqual(errors, [], `console and script errors at ${width}px`);
    assert.equal(injected, 1);
    await page.close();
    console.log(
      `PASS ${width}×${height} global usage: direct address and reload, resource navigation, bounded tree, retry, paging and Issue detail`,
    );
  }
} finally {
  await browser.close();
}
