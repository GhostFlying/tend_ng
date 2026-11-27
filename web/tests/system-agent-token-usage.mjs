import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";
import { isAnonymousSessionProbe } from "./console-errors.mjs";

const { TEND_URL, TEND_TOKEN, TEND_SCREENSHOTS } = process.env;
const requests = Number(process.argv[2]);
assert(TEND_URL && TEND_TOKEN && TEND_SCREENSHOTS && requests > 0);
assert.equal(new URL(TEND_URL).hostname, "127.0.0.1");
await mkdir(TEND_SCREENSHOTS, { recursive: true });

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
      if (message.type() === "error" && !isAnonymousSessionProbe(message))
        errors.push(message.text());
    });
    await page.goto(TEND_URL + "/token-usage");
    await page.getByLabel("访问令牌").fill(TEND_TOKEN);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    const source = page.getByRole("region", { name: "按调用来源" });
    await source.waitFor();
    await page.getByRole("heading", { name: "全系统 Agent 用量" }).waitFor();
    await page.getByRole("heading", { name: "Issue 树" }).waitFor();
    assert.equal(
      await page.locator(".usage-request-count").textContent(),
      `${requests} 条已识别请求`,
    );
    const rows = source.getByRole("listitem");
    assert.equal(await rows.count(), 6);
    for (const name of [
      "Issue Agent",
      "主 Chat Router",
      "讨论回复",
      "讨论转交总结",
      "隔离转交接收",
      "飞书研判",
    ]) {
      const row = rows.filter({
        has: page.getByRole("heading", { name, exact: true }),
      });
      assert.match(await row.innerText(), /[1-9]\d* 条已识别请求/);
    }
    assert.equal(
      (await rows.allTextContents()).reduce(
        (sum, row) => sum + Number(row.match(/(\d+) 条已识别请求/)[1]),
        0,
      ),
      requests,
    );
    assert(
      await page.evaluate(
        () => document.documentElement.scrollWidth === globalThis.innerWidth,
      ),
    );
    await page
      .locator(".usage-overview")
      .getByText("模型明细与费用估算", { exact: true })
      .click();
    await page.screenshot({
      path: join(TEND_SCREENSHOTS, `system-agent-token-usage-${width}.png`),
      fullPage: true,
      animations: "disabled",
    });
    if (width === 390) {
      await source
        .getByRole("heading", { level: 2 })
        .evaluate((heading) => heading.scrollIntoView({ block: "start" }));
      await page.screenshot({
        path: join(
          TEND_SCREENSHOTS,
          "system-agent-token-usage-mobile-sources.png",
        ),
        animations: "disabled",
      });
    }
    assert.deepEqual(errors, []);
    await page.close();
  }
} finally {
  await browser.close();
}
