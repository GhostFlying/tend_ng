import assert from "node:assert/strict";
import { join } from "node:path";
import { chromium } from "playwright";
import { collectPageErrors } from "./console-errors.mjs";

const {
  TEND_URL: url,
  TEND_TOKEN: token,
  TEND_SCREENSHOTS: images,
} = process.env;
const [issue] = process.argv.slice(2);
assert(url && token && images && issue);
assert.equal(new URL(url).hostname, "127.0.0.1");
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
    const usageReads = [];
    page.on("request", (request) => {
      const { pathname } = new URL(request.url());
      if (pathname.startsWith("/api/") && pathname.endsWith("/token-usage"))
        usageReads.push(pathname);
    });

    async function capture(name, anchor, block = "start") {
      assert.equal(
        await page.getByText("部分估算", { exact: true }).count(),
        0,
      );
      const [client, scroll] = await page.evaluate(() => [
        document.documentElement.clientWidth,
        document.documentElement.scrollWidth,
      ]);
      assert.equal(client, width);
      assert(scroll <= client + 1);
      await anchor.evaluate(
        (node, block) => node.scrollIntoView({ block }),
        block,
      );
      await page.screenshot({
        path: join(images, `${name}-${width}.png`),
        animations: "disabled",
      });
    }

    await page.goto(`${url}/token-usage`);
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    const total = page.getByRole("region", { name: "全系统 Agent 用量" });
    const count = "4 条请求未计价";
    await total
      .locator("h4")
      .getByText("$1.0942952", { exact: true })
      .waitFor();
    await total.locator("h4").getByText(count, { exact: true }).waitFor();
    await capture("pricing-global", total);

    const feishu = page
      .locator(".usage-source")
      .filter({ hasText: "飞书研判" });
    const line = feishu.locator(":scope > summary");
    await line.getByText(count, { exact: true }).waitFor();
    assert.equal(await line.locator(".model-cost").count(), 0);
    assert.equal(await line.getByText(/请求用量未知/).count(), 0);
    await line.click();
    const unknown = feishu.locator(".model-row > summary");
    await unknown.getByText(count, { exact: true }).waitFor();
    await unknown.click();
    await feishu
      .getByText("请求缺少可归属的原生模型证据，或模型证据不一致。")
      .waitFor();
    await capture("pricing-feishu-detail", feishu, "center");
    assert.equal(
      usageReads.length,
      1,
      "source/model expansion does not read again",
    );

    const empty = page.locator(".usage-tree-row").filter({ hasText: "无请求" });
    await empty.waitFor();
    assert.equal(await empty.locator(".model-estimate-values").count(), 0);
    await page.getByRole("button", { name: `展开 #${issue} 的子级` }).click();
    await page
      .locator(".usage-tree-row")
      .filter({ hasText: "子级 · 费率档未知" })
      .getByText("$0", { exact: true })
      .first()
      .waitFor();

    await page.goto(`${url}/issues/${issue}`);
    await page
      .getByRole("heading", { level: 1, name: /52 条 · #131 口径复现/ })
      .waitFor();
    await page.locator(".token-section > summary").click();
    const pane = page.locator(".token-pane");
    const model = pane.locator(".model-row");
    const summary = model.locator("summary");
    await summary.getByText("$1.0942952", { exact: true }).waitFor();
    await summary.getByText("52 条请求", { exact: true }).waitFor();
    assert.equal(await pane.locator(".model-unpriced-count").count(), 0);
    await capture("pricing-issue", pane.locator(".model-usage"), "center");
    await summary.click();
    const detail = model.locator(".model-detail");
    await detail.getByText("openai/gpt-6.2-sol", { exact: true }).waitFor();
    await detail.getByText(/1 条的缓存读折扣未观测/).waitFor();
    const items = detail.locator(".model-fee-items");
    const write = items
      .locator("div")
      .filter({ has: page.getByText("缓存写入", { exact: true }) });
    await write.getByText("未计 52 条 · 52 条数量缺失或未分拆").waitFor();
    const baseline = items
      .locator("div")
      .filter({ has: page.getByText("未细分输入（基准）", { exact: true }) });
    await baseline.getByText("已计 52 / 52 条").waitFor();
    await capture("pricing-issue-detail", detail, "center");

    await pane.getByRole("button", { name: "含所有后代" }).click();
    await summary.getByText("54 条请求", { exact: true }).waitFor();
    await pane.getByText("子级 · 费率档未知", { exact: false }).waitFor();
    assert.equal(await pane.locator(".model-unpriced-count").count(), 0);
    assert.equal(
      usageReads.length,
      3,
      "the tree expands once and scope switch reuses the Issue response",
    );
    assert.deepEqual(errors, []);
    await page.close();
    console.log(
      `PASS ${width}×${height} real global, source and Issue pricing, details and empty/zero states`,
    );
  }
} finally {
  await browser.close();
}
