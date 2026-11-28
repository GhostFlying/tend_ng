import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";
import { collectPageErrors } from "./console-errors.mjs";

const { TEND_URL, TEND_TOKEN, TEND_SCREENSHOTS } = process.env;
const requests = Number(process.argv[2]);
assert(TEND_URL && TEND_TOKEN && TEND_SCREENSHOTS && requests === 2);
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
    collectPageErrors(page, errors);
    let reads = 0;
    page.on("request", (request) => {
      if (new URL(request.url()).pathname === "/api/v1/token-usage") reads++;
    });
    await page.goto(TEND_URL + "/token-usage");
    await page.getByLabel("访问令牌").fill(TEND_TOKEN);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    const sources = page.getByRole("region", { name: "按调用来源" });
    await sources.waitFor();
    assert.equal(new URL(page.url()).pathname, "/token-usage");
    await page.getByRole("heading", { name: "全系统 Agent 用量" }).waitFor();
    await page.getByRole("heading", { name: "Issue 树" }).waitFor();
    assert.equal(
      await page.locator(".usage-request-count").textContent(),
      "2 条已识别请求",
    );
    const summaries = sources.locator(".usage-source > summary");
    assert.equal(await summaries.count(), 6);
    for (const name of [
      "Issue Agent",
      "主 Chat Router",
      "讨论回复",
      "讨论转交总结",
      "隔离转交接收",
      "飞书研判",
    ]) {
      await summaries.getByText(name, { exact: true }).waitFor();
    }
    const router = summaries.filter({ hasText: "主 Chat Router" });
    await router.getByText("1,727 Token", { exact: true }).waitFor();
    await router.getByText("$0.008202", { exact: true }).waitFor();
    await router.click();
    const models = sources.getByRole("region", {
      name: "主 Chat Router · 模型明细",
    });
    await models.waitFor();
    assert.equal(await models.locator(".model-row").count(), 2);
    for (const [name, total, cost] of [
      ["gpt-6.2-sol", "864 Token", "$0.001642"],
      ["gpt-6.2-astra", "863 Token", "$0.00656"],
    ]) {
      const model = models
        .locator(".model-row > summary")
        .filter({ hasText: name });
      await model.getByText(total, { exact: true }).waitFor();
      await model.getByText(cost, { exact: true }).waitFor();
      assert.equal(await model.getByText(/部分估算|请求未计价/).count(), 0);
    }
    const feishu = summaries.filter({ hasText: "飞书研判" });
    await feishu.getByText("无数据", { exact: true }).waitFor();
    await feishu.click();
    await sources.getByText("该来源暂无已记录请求。").waitFor();
    assert.equal(await feishu.locator(".model-cost").count(), 0);
    assert.equal(reads, 1, "source expansion does not load additional usage");
    assert(
      await page.evaluate(
        () => document.documentElement.scrollWidth === globalThis.innerWidth,
      ),
    );
    await sources
      .getByRole("heading", { level: 2 })
      .evaluate((heading) => heading.scrollIntoView({ block: "start" }));
    await page.screenshot({
      path: join(TEND_SCREENSHOTS, `token-sources-${width}.png`),
      animations: "disabled",
    });
    assert.deepEqual(errors, []);
    await page.close();
  }
} finally {
  await browser.close();
}
