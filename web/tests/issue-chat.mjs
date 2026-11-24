import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";

const {
  TEND_URL: url,
  TEND_TOKEN: token,
  TEND_SCREENSHOTS: screenshots,
  TEND_CHILD_ISSUE: child,
  TEND_NOTIFICATION_SCREENSHOTS: notificationScreenshots,
} = process.env;
assert(url && token && child);

const first = "初稿已整理，接下来核对首次运行步骤。";
const answer = "验证命令已补进指南。";
const detail = "核对安装步骤和验证命令。";
const browser = await chromium.launch({ args: ["--no-proxy-server"] });
const errors = [];
const reads = [];

async function open(width, height) {
  const page = await browser.newPage({ viewport: { width, height } });
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("request", (request) => {
    if (/\/api\/v1\/runs\/[^/]+\/rollout$/.test(request.url()))
      reads.push(request.url());
  });
  await page.goto(url);
  await page.getByLabel("访问令牌").fill(token);
  await page.getByRole("button", { name: "连接", exact: true }).click();
  await page.getByRole("link", { name: "查看 Issue", exact: true }).click();
  await page
    .getByRole("heading", { name: "示例仓库上手指南", exact: true })
    .waitFor();
  await page.getByRole("paragraph").filter({ hasText: first }).waitFor();
  return page;
}

try {
  const desktop = await open(1440, 1000);
  const entry = desktop
    .locator(".message")
    .filter({ has: desktop.getByRole("paragraph").filter({ hasText: first }) });
  assert.equal(
    await entry
      .getByRole("link", { name: "整理快速上手指南" })
      .getAttribute("href"),
    `/issues/${child}`,
  );
  assert.equal(await desktop.getByText(detail, { exact: true }).count(), 0);
  assert.deepEqual(reads, []);
  await desktop
    .locator(".message")
    .filter({ has: desktop.getByText(answer, { exact: true }) })
    .getByRole("button", { name: "回复", exact: true })
    .click();
  const input = desktop.getByLabel("发消息给 Issue Owner");
  const followup = "谢谢，命令适用于 macOS 吗？";
  await input.fill(followup);
  await desktop.getByText("回复：" + answer, { exact: true }).waitFor();
  if (screenshots) {
    await mkdir(screenshots, { recursive: true });
    await desktop.screenshot({
      path: join(screenshots, "issue-chat-desktop.png"),
    });
  }
  await input.press("Enter");
  await desktop
    .getByText("适用于 macOS，使用终端执行即可。", { exact: true })
    .waitFor();
  assert.equal(
    await desktop.getByRole("paragraph").filter({ hasText: followup }).count(),
    1,
  );

  const mobile = await open(390, 844);
  const firstWorkerRun = mobile
    .locator(".run-row")
    .filter({ hasText: "worker" })
    .first();
  await firstWorkerRun.getByRole("button", { name: /查看执行过程/ }).click();
  await firstWorkerRun.getByText(detail, { exact: true }).waitFor();
  assert.equal(reads.length, 1);
  assert.equal(
    await mobile.evaluate(() => document.documentElement.scrollWidth),
    390,
  );
  if (screenshots) {
    await mobile.screenshot({
      path: join(screenshots, "issue-chat-mobile.png"),
    });
  }
  await firstWorkerRun.getByRole("button", { name: /收起执行过程/ }).click();
  assert.equal(await mobile.getByText(detail, { exact: true }).count(), 0);
  await desktop.goto(url);
  await desktop
    .getByRole("link", { name: "查看 Issue", exact: true })
    .waitFor();
  const draft = desktop.getByLabel("发消息给 Tend");
  await draft.fill("下一步，请看下示例仓库的发布说明。");
  const timeline = desktop.locator(".timeline");
  const initialHeight = await timeline.evaluate(
    (element) => element.clientHeight,
  );
  const noticeReads = [];
  desktop.on("request", (request) => {
    if (request.url().endsWith("/notifications"))
      noticeReads.push(request.method());
  });
  await desktop.getByRole("button", { name: "通知", exact: true }).click();
  const panel = desktop.getByRole("region", { name: "工作通知" });
  await panel.getByText("已接续", { exact: true }).first().waitFor();
  assert.equal(
    await panel
      .getByRole("link", { name: "整理快速上手指南" })
      .first()
      .getAttribute("href"),
    `/issues/${child}`,
  );
  assert.deepEqual(noticeReads, ["GET"]);
  assert.equal(
    await timeline.evaluate((element) => element.clientHeight),
    initialHeight,
  );
  if (notificationScreenshots) {
    await mkdir(notificationScreenshots, { recursive: true });
    await desktop.screenshot({
      path: join(notificationScreenshots, "notifications-desktop.png"),
    });
  }
  await panel.getByRole("button", { name: "关闭", exact: true }).click();
  assert.equal(await draft.inputValue(), "下一步，请看下示例仓库的发布说明。");
  assert.equal(
    await draft.evaluate((element) => element === document.activeElement),
    true,
  );

  await desktop.setViewportSize({ width: 390, height: 345 });
  const position = await timeline.evaluate((element) => {
    element.scrollTop = 16;
    return element.scrollTop;
  });
  assert(position > 0, "fixture needs a scrollable narrow Chat");
  await draft.focus();
  await desktop.getByRole("button", { name: "通知", exact: true }).click();
  await panel.getByText("已结案", { exact: true }).waitFor();
  assert.equal(
    await desktop.evaluate(() => document.documentElement.scrollWidth),
    390,
  );
  await panel.getByRole("button", { name: "关闭", exact: true }).click();
  assert.equal(
    await timeline.evaluate((element) => element.scrollTop),
    position,
  );
  assert.deepEqual(noticeReads, ["GET", "GET"]);
  assert.deepEqual(errors, []);
  console.log(
    "PASS Issue discussion on desktop and mobile with one explicit native history read",
  );
} finally {
  await browser.close();
}
