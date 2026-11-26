import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";

const {
  TEND_URL: url,
  TEND_TOKEN: token,
  TEND_SCREENSHOTS: output,
} = process.env;
async function capture(page, name) {
  if (output) {
    await mkdir(output, { recursive: true });
    await page.screenshot({ path: join(output, name) });
  }
}
assert(url && token);
const browser = await chromium.launch({ args: ["--no-proxy-server"] });
try {
  for (const width of [1440, 390]) {
    const page = await browser.newPage({ viewport: { width, height: 844 } });
    const errors = [];
    const documents = [];
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("request", (request) => {
      if (request.resourceType() === "document") documents.push(request.url());
    });
    await page.goto(url);
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    if (width === 390) {
      await page.getByRole("button", { name: "通知", exact: true }).click();
      const notifications = page.getByRole("region", { name: "工作通知" });
      await notifications.getByRole("button", { name: "关闭" }).click();
      await page.getByRole("link", { name: "更多" }).click();
    }
    await page.getByRole("link", { name: "Agents", exact: true }).click();
    await page.getByRole("heading", { name: "Agents", exact: true }).waitFor();
    page.on("console", (message) => {
      if (message.type() === "error") errors.push(message.text());
    });
    const workers = page
      .locator("button.agent-row")
      .filter({ hasText: "fixture-worker" });
    await workers.first().waitFor();
    assert.equal(await workers.count(), 3);
    assert.equal(
      await workers.filter({ hasText: "retired-runner" }).count(),
      0,
    );
    await workers.filter({ hasText: "previous-runner" }).waitFor();
    await capture(page, `resource-agents-${width}.png`);
    const search = page.getByRole("searchbox", { name: "搜索名称或 ID" });
    if (width === 1440) {
      await search.fill("synthetic-filter");
      await page.waitForURL(`${url}/agents?q=synthetic-filter`);
      assert.equal(await workers.count(), 0);
      await page.getByRole("link", { name: "Agents", exact: true }).click();
      await page.waitForURL(`${url}/agents`);
      assert.equal(await search.inputValue(), "");
      assert.equal(await workers.count(), 3);
      await page.goBack({ waitUntil: "commit" });
      await page.waitForURL(`${url}/agents?q=synthetic-filter`);
      assert.equal(await search.inputValue(), "synthetic-filter");
      await page.goForward({ waitUntil: "commit" });
      await page.waitForURL(`${url}/agents`);
      assert.equal(await search.inputValue(), "");
    }
    const category = page.getByLabel("类别筛选");
    assert.equal(
      await category.locator('option[value="implementation"]').textContent(),
      "implementation",
    );
    await category.selectOption("implementation");
    await search.fill("worker");
    assert.equal(await page.locator("button.agent-row").count(), 3);
    await page.getByLabel("运行位置或方式筛选").selectOption("host:demo-host");
    assert.equal(await workers.count(), 2);
    const chosen = workers.filter({ hasText: "current-runner" });
    await chosen
      .getByText(width === 390 ? /^implementation ·/ : /^implementation$/)
      .waitFor();
    await chosen.click();
    const inspector = page.locator(
      ".agent-inspector:visible, .agent-standalone:visible",
    );
    await inspector
      .getByRole("heading", { name: "worker", exact: true })
      .waitFor();
    await inspector.getByText("image_gen", { exact: true }).waitFor();
    await inspector.getByText("implementation", { exact: true }).waitFor();
    if (width === 390) {
      await page.getByRole("button", { name: /返回列表/ }).click();
      assert.equal(await search.inputValue(), "worker");
      assert.ok(await chosen.evaluate((row) => row === document.activeElement));
    }
    await page.getByRole("tab", { name: "Categories", exact: true }).click();
    await page
      .getByRole("tabpanel", { name: "Categories" })
      .locator("strong")
      .filter({ hasText: /^implementation-economy$/ })
      .waitFor();
    assert.equal(
      await page.getByRole("button", { name: /retired-runner/ }).count(),
      0,
    );
    assert.equal(
      await page.evaluate(() => document.documentElement.scrollWidth),
      width,
    );

    if (width === 390) await page.getByRole("link", { name: "更多" }).click();
    await page.getByRole("link", { name: "主机", exact: true }).click();
    await page.getByText("执行主机 · 2 台", { exact: true }).waitFor();
    const host = page.getByRole("button", { name: "主机详情：demo-host" });
    await host.getByText(/0 \/ 6 槽位 · 2 个 Runner/).waitFor();
    await page
      .getByRole("button", { name: "主机详情：offline-host" })
      .getByText("离线")
      .waitFor();
    await host.click();
    const dialog = page.getByRole("dialog", { name: "主机详情" });
    await dialog
      .getByRole("heading", { name: "current-runner", exact: true })
      .waitFor();
    await dialog
      .getByRole("heading", { name: "previous-runner", exact: true })
      .waitFor();
    assert.equal(
      await dialog.getByRole("heading", { name: "retired-runner" }).count(),
      0,
    );
    await dialog.getByText("排空中", { exact: true }).waitFor();
    assert.equal(await dialog.locator("section.host-runner").count(), 2);
    await capture(page, `resource-host-${width}.png`);
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "detached" });
    assert.ok(await host.evaluate((row) => row === document.activeElement));
    assert.deepEqual(errors, []);
    assert.deepEqual(documents, [`${url}/`]);
    await page.close();
  }
} finally {
  await browser.close();
}
console.log(
  "PASS desktop and mobile Agent filtering, detail navigation, and same-host Runner details",
);
