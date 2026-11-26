import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { chromium } from "playwright";

const {
  TEND_URL: url,
  TEND_TOKEN: token,
  TEND_SCREENSHOTS: output,
  TEND_RESOURCES_DB: fixtureDb,
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
    const page = await browser.newPage({
      viewport: { width, height: 844 },
      hasTouch: width === 390,
      isMobile: width === 390,
    });
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
    await page.getByLabel("运行位置或方式筛选").selectOption("host:devbox");
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
    await page.waitForURL(`${url}/hosts`);
    await page.getByText(/执行主机 · 2 台 · 2 台在线/).waitFor();
    const host = page.getByRole("region", { name: "主机 devbox" });
    const idle = page.getByRole("region", { name: "主机 smallbox" });
    assert.match(
      await host.locator(".execution-host-count").innerText(),
      /3.*6.*槽位/,
    );
    assert.match(
      await idle.locator(".execution-host-count").innerText(),
      /0.*12.*槽位/,
    );
    const current = host
      .locator("details")
      .filter({ hasText: "current-runner" });
    const draining = host
      .locator("details")
      .filter({ hasText: "previous-runner" });
    const idleRunner = idle.locator("details");
    assert.equal(await host.locator("details").count(), 2);
    assert.equal(
      await page.getByText("retired-runner", { exact: true }).count(),
      0,
    );
    await current.getByText("Cedar", { exact: true }).waitFor();
    await current.getByText("Laurel", { exact: true }).waitFor();
    assert.equal(
      await current
        .getByRole("link", { name: /#86 展示派发来源/ })
        .getAttribute("href"),
      "/issues/86",
    );
    assert.equal(
      await current
        .getByRole("link", { name: "实现派发与接手条目" })
        .getAttribute("href"),
      "/issues/87",
    );
    assert.match(
      await current.locator(".host-run-time").first().innerText(),
      /^4 分/,
    );
    await draining.getByText("收尾中", { exact: true }).waitFor();
    await draining.getByText("Maple", { exact: true }).waitFor();
    assert.equal(await draining.getByLabel("启动时间未知").innerText(), "—");
    assert.equal(await host.getByText(/不展示/).count(), 0);
    assert.equal(await idleRunner.evaluate((details) => details.open), false);
    await capture(page, `execution-hosts-render-${width}.png`);
    const toggle = current.locator("summary");
    await toggle.focus();
    await page.keyboard.press("Enter");
    assert.equal(await current.evaluate((details) => details.open), false);
    assert.equal(await draining.evaluate((details) => details.open), true);
    assert.ok(
      await toggle.evaluate((summary) => summary === document.activeElement),
    );
    await page.keyboard.press("Space");
    assert.equal(await current.evaluate((details) => details.open), true);
    if (width === 390) await idleRunner.locator("summary").tap();
    else await idleRunner.locator("summary").click();
    await idleRunner.getByText("当前没有运行中的 Task。").waitFor();
    assert.equal(
      await page.evaluate(() => document.documentElement.scrollWidth),
      width,
    );
    assert.equal(await page.locator("vite-error-overlay").count(), 0);
    if (width === 390 && fixtureDb) {
      const db = new DatabaseSync(fixtureDb);
      try {
        db.prepare(
          "UPDATE runner_replacements SET drained_at=unixepoch(), retired_at=unixepoch() WHERE predecessor='previous-runner'",
        ).run();
      } finally {
        db.close();
      }
      await draining.waitFor({ state: "detached" });
      assert.equal(await host.locator("details").count(), 1);
      assert.match(
        await host.locator(".execution-host-count").innerText(),
        /2.*6.*槽位/,
      );
      assert.equal(await current.evaluate((details) => details.open), true);
      assert.equal(await idleRunner.evaluate((details) => details.open), true);
    }
    assert.deepEqual(errors, []);
    assert.deepEqual(documents, [`${url}/`]);
    await page.close();
  }
} finally {
  await browser.close();
}
console.log(
  "PASS desktop and mobile Agent filtering and keyboard/touch Runner activity, including retirement and unknown starts",
);
