import assert from "node:assert/strict";
import { chromium } from "playwright";

const { TEND_URL: url, TEND_TOKEN: token } = process.env;
assert(url && token);
const browser = await chromium.launch({ args: ["--no-proxy-server"] });
try {
  for (const width of [1440, 390]) {
    const page = await browser.newPage({ viewport: { width, height: 844 } });
    const errors = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await page.addInitScript(
      (value) => sessionStorage.setItem("tend.token.v1", value),
      token,
    );
    await page.goto(url);
    if (width === 390) {
      await page.getByRole("button", { name: "通知", exact: true }).click();
      const notifications = page.getByRole("region", { name: "工作通知" });
      await notifications.getByRole("button", { name: "关闭" }).click();
      await page.getByRole("link", { name: "更多" }).click();
    }
    await page.getByRole("link", { name: "Agents", exact: true }).click();
    await page.getByRole("heading", { name: "Agents", exact: true }).waitFor();
    const workers = page
      .locator("button.agent-row")
      .filter({ hasText: "fixture-worker" });
    await workers.first().waitFor();
    assert.equal(await workers.count(), 3);
    const search = page.getByRole("searchbox", { name: "搜索名称或 ID" });
    await search.fill("worker");
    assert.equal(await page.locator("button.agent-row").count(), 3);
    await page.getByLabel("运行位置或方式筛选").selectOption("host:demo-host");
    assert.equal(await workers.count(), 2);
    const chosen = workers.filter({ hasText: "current-runner" });
    await chosen.click();
    const inspector = page.locator(
      ".agent-inspector:visible, .agent-standalone:visible",
    );
    await inspector
      .getByRole("heading", { name: "worker", exact: true })
      .waitFor();
    await inspector.getByText("image_gen", { exact: true }).waitFor();
    if (width === 390) {
      await page.getByRole("button", { name: /返回列表/ }).click();
      assert.equal(await search.inputValue(), "worker");
      assert.ok(await chosen.evaluate((row) => row === document.activeElement));
    }
    await page.getByRole("tab", { name: "Categories", exact: true }).click();
    await page
      .getByRole("tabpanel", { name: "Categories" })
      .getByText("implementation-economy", { exact: true })
      .waitFor();
    assert.equal(
      await page.evaluate(() => document.documentElement.scrollWidth),
      width,
    );

    if (width === 390) await page.getByRole("link", { name: "更多" }).click();
    await page.getByRole("link", { name: "主机", exact: true }).click();
    await page.getByText("执行主机 · 2 台", { exact: true }).waitFor();
    const host = page.getByRole("button", { name: "主机详情：demo-host" });
    await host.getByText(/0 \/ 6 槽位/).waitFor();
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
    await dialog.getByText("已退役", { exact: true }).waitFor();
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "detached" });
    assert.ok(await host.evaluate((row) => row === document.activeElement));
    assert.deepEqual(errors, []);
    await page.close();
  }
} finally {
  await browser.close();
}
console.log(
  "PASS desktop and mobile Agent filtering, detail navigation, and same-host Runner details",
);
