import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";
import { collectPageErrors } from "./console-errors.mjs";

const {
  TEND_URL: base,
  TEND_TOKEN: token,
  TEND_ISSUE: issue,
  TEND_SECOND_ATTENTION: independent,
  TEND_SCREENSHOTS: screenshots,
} = process.env;
assert(base && token && issue && independent);
if (screenshots) await mkdir(screenshots, { recursive: true });

const browser = await chromium.launch({ args: ["--no-proxy-server"] });
const errors = [];
const actions = ["批准独立文案 v1", "批准按 v2 实现"];
let desktop;
try {
  for (const viewport of [
    { width: 1440, height: 1120 },
    { width: 390, height: 844 },
  ]) {
    const context = await browser.newContext({
      viewport,
      colorScheme: "light",
      isMobile: viewport.width < 761,
      hasTouch: viewport.width < 761,
    });
    const page = await context.newPage();
    collectPageErrors(page, errors);
    await page.goto(`${base}/issues/${issue}`);
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    for (const action of actions) {
      await page.getByRole("button", { name: action, exact: true }).waitFor();
    }
    const separate = page.locator(".proposal-section").filter({
      has: page.getByRole("heading", { name: "独立文案方案", exact: true }),
    });
    assert.equal(
      await separate.getByRole("button", { name: actions[0] }).count(),
      1,
    );
    assert.equal(
      await separate.getByRole("button", { name: actions[1] }).count(),
      0,
    );
    assert.equal(await page.locator(".proposal-choices").count(), 0);
    if (screenshots) {
      await separate
        .getByRole("button", { name: actions[0] })
        .scrollIntoViewIfNeeded();
      await page.screenshot({
        path: join(screenshots, `archive-approval-issue-${viewport.width}.png`),
      });
    }
    await page.getByRole("link", { name: "查看原 Chat", exact: true }).click();
    await page.getByText("此对话已归档", { exact: true }).waitFor();
    assert.equal(await page.locator(".composer").count(), 0);
    for (const action of actions) {
      assert.equal(
        await page.getByRole("button", { name: action, exact: true }).count(),
        0,
      );
    }
    await page.goBack();
    await page.getByRole("button", { name: actions[0], exact: true }).waitFor();
    if (viewport.width === 1440) desktop = page;
    else await context.close();
  }

  const posted = desktop.waitForResponse(
    (response) =>
      response.url().endsWith(`/attentions/${independent}/approve`) &&
      response.request().method() === "POST",
  );
  await desktop.getByRole("button", { name: actions[0], exact: true }).click();
  const response = await posted;
  assert.equal(response.status(), 200);
  assert.deepEqual(response.request().postDataJSON(), {
    version: 1,
    from_issue: true,
  });
  await desktop
    .getByRole("link", { name: "查看批准记录", exact: true })
    .waitFor();
  assert.equal(
    await desktop
      .getByRole("button", { name: actions[1], exact: true })
      .count(),
    1,
  );
  assert.deepEqual(errors, []);
} finally {
  await browser.close();
}
