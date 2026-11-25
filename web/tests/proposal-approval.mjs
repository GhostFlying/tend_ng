import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";

const {
  TEND_URL: url,
  TEND_TOKEN: token,
  TEND_ISSUE: issue,
  TEND_QUESTION_ID: question,
  TEND_SCREENSHOTS: screenshots,
} = process.env;
assert(url && token && issue && question);
if (screenshots) await mkdir(screenshots, { recursive: true });
const browser = await chromium.launch({ args: ["--no-proxy-server"] });
const errors = [];

async function open(viewport) {
  const page = await browser.newPage({ viewport, colorScheme: "light" });
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (
      ["error", "warning"].includes(message.type()) &&
      !message.text().includes("net::ERR_FAILED")
    ) {
      errors.push(message.text());
    }
  });
  await page.goto(`${url}/issues`);
  await page.getByLabel("访问令牌").fill(token);
  await page.getByRole("button", { name: "连接", exact: true }).click();
  await page.getByRole("link", { name: /设计任务运行摘要导出/ }).click();
  await page.getByRole("heading", { name: /设计任务运行摘要导出/ }).waitFor();
  await page.getByRole("heading", { name: "当前报告", exact: true }).waitFor();
  assert.equal(await page.title(), "Tend");
  assert.match(new URL(page.url()).pathname, /^\/issues\//);
  assert.equal(await page.locator("vite-error-overlay").count(), 0);
  return page;
}

async function screenshot(page, filename) {
  if (screenshots) await page.screenshot({ path: join(screenshots, filename) });
}

async function width(page, expected) {
  assert.equal(
    await page.evaluate(() => document.documentElement.scrollWidth),
    expected,
  );
  const timeline = page.locator("main.timeline");
  assert(
    await timeline.evaluate((node) => node.scrollWidth <= node.clientWidth + 1),
    "Issue document does not scroll horizontally",
  );
}

try {
  const desktop = await open({ width: 1440, height: 1000 });
  const mobile = await open({ width: 390, height: 844 });
  const approved = "Approved, waiting for continuation";
  const approveName = "批准按 v2 实现";
  const regional = desktop.locator(".proposal-section").first();
  await regional.getByText("版本历史 (1)", { exact: true }).waitFor();
  assert.equal(
    await regional.locator(".proposal-history").getAttribute("open"),
    null,
  );
  assert.equal(
    await desktop.getByRole("button", { name: approveName }).count(),
    1,
  );
  const otherReports = desktop.locator(".proposal-other-reports");
  await otherReports.locator("summary").first().click();
  await otherReports
    .getByText("独立文案记录，不属于审批版本。", { exact: true })
    .waitFor();
  await otherReports.locator("summary").first().click();
  await desktop.locator(".proposal-children summary").click();
  await desktop
    .locator(".proposal-children")
    .getByRole("link", { name: /Write the guide/ })
    .waitFor();
  await desktop.locator(".proposal-children summary").click();
  await width(desktop, 1440);
  await screenshot(desktop, "inline-approval-desktop.png");

  const properties = mobile.locator(".proposal-mobile-properties");
  assert.equal(await properties.getAttribute("open"), null);
  await properties.locator("summary").first().click();
  await properties.getByText("example", { exact: true }).waitFor();
  await properties.locator("summary").first().click();
  await width(mobile, 390);
  await screenshot(mobile, "inline-approval-mobile.png");

  const mobileReport = mobile
    .locator(".proposal-section > .proposal-report")
    .first();
  await mobileReport.getByText("展开报告原文", { exact: true }).click();
  await mobileReport.getByRole("table").waitFor();
  assert.equal(
    await mobileReport.locator(".markdown h1").textContent(),
    "任务运行摘要导出方案 v2",
  );
  const scrollable = await mobileReport
    .locator(".markdown-scroll")
    .evaluateAll((nodes) =>
      nodes.every((node) => node.scrollWidth > node.clientWidth),
    );
  assert(
    scrollable,
    "table and code each have independent horizontal scrolling",
  );
  await width(mobile, 390);
  await mobileReport.getByRole("table").scrollIntoViewIfNeeded();
  await screenshot(mobile, "inline-approval-mobile-markdown.png");
  await mobileReport.getByText("展开报告原文", { exact: true }).click();

  const history = regional.locator(".proposal-history");
  await history.locator("summary").first().click();
  await history.getByText(/任务运行摘要导出方案 · v1/).click();
  await history.getByText("summary-export-v1.md").waitFor();
  await history.locator("summary").first().click();

  const historyRequest = mobile.waitForResponse(
    (response) =>
      /\/api\/v1\/chats\/main\/attentions\/[^/]+$/.test(
        new URL(response.url()).pathname,
      ) && response.ok(),
  );
  await mobile.goto(`${url}/#message-${question}`);
  await mobile.getByRole("button", { name: approveName }).waitFor();
  await historyRequest;

  let writes = 0;
  await desktop.route("**/attentions/*/approve", async (route) => {
    writes++;
    assert.deepEqual(route.request().postDataJSON(), {
      version: 2,
      from_issue: true,
    });
    const saved = await route.fetch();
    assert.equal(saved.status(), 200);
    await route.abort();
  });
  await desktop.getByRole("button", { name: approveName }).click();
  await desktop.getByText(approved, { exact: true }).waitFor();
  await screenshot(
    desktop,
    "inline-approval-desktop-awaiting-continuation.png",
  );
  await mobile.getByText(approved, { exact: true }).waitFor();
  assert.equal(writes, 1);
  const event = desktop.locator(".approval-event");
  await event.getByText("你批准了 v2", { exact: true }).waitFor();
  assert.equal(await event.count(), 1);
  assert.equal(await event.locator("time").count(), 1);
  assert.equal(await event.locator(".message-user").count(), 0);
  await desktop.getByRole("link", { name: "查看批准记录" }).click();
  assert.equal(
    new URL(desktop.url()).hash,
    `#${await event.getAttribute("id")}`,
  );
  await screenshot(desktop, "inline-approval-desktop-record.png");

  await mobile.getByText(/你批准了 v2/).waitFor();
  assert.equal(
    await mobile.getByRole("button", { name: approveName }).count(),
    0,
  );
  await width(mobile, 390);
  assert.deepEqual(errors, []);
  console.log(
    "PASS Issue approval document and original Chat in Chromium at 1440px and 390px",
  );
} finally {
  await browser.close();
}
