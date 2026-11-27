import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";
import { collectPageErrors } from "./console-errors.mjs";

const {
  TEND_URL: url,
  TEND_TOKEN: token,
  TEND_SCREENSHOTS: screenshots,
  TEND_TIMELINE_PHASE: phase,
  TEND_TIMELINE_PARENT: parent,
  TEND_TIMELINE_SOURCE: source,
  TEND_TIMELINE_LEGACY: legacy,
  TEND_TIMELINE_LEGACY_SOURCE: legacySource,
  TEND_TIMELINE_OTHER: other,
  TEND_TIMELINE_OTHER_SOURCE: otherSource,
  TEND_TIMELINE_CHILD: child,
} = process.env;

assert(url && token && parent && source);
assert(
  phase === "issued"
    ? legacy && legacySource && other && otherSource
    : phase === "accepted" && child,
);
const browser = await chromium.launch({ args: ["--no-proxy-server"] });
const errors = [];

const objective = "整理示例仓库的快速上手指南。";
const originalText = "请整理示例仓库的快速上手指南。";
const childObjective = "整理安装和首次运行步骤。";
const legacyObjective = "先记下稍后整理发布说明。";
const otherObjective = "准备另一个仓库的发布记录。";
const otherOriginal = "顺带准备另一个仓库的发布记录。";

async function verifyAndCapture(page, label, size) {
  const heading = page.locator("#issue-discussion");
  await heading.waitFor();
  if (size === "mobile" && label === "accepted") {
    await heading.evaluate((element) =>
      element.scrollIntoView({ block: "start" }),
    );
  } else {
    await page.locator(".timeline").evaluate((element) => {
      element.scrollTop = 0;
    });
  }
  await page.evaluate(() => document.fonts.ready.then(() => undefined));
  assert.equal(await page.title(), "Tend");
  assert.equal(await page.locator("vite-error-overlay").count(), 0);
  assert.equal(
    await page.evaluate(() => document.documentElement.scrollWidth),
    page.viewportSize().width,
  );
  if (screenshots) {
    await mkdir(screenshots, { recursive: true });
    await page.screenshot({
      path: join(screenshots, `issue-timeline-${label}-${size}.png`),
    });
  }
}

async function openIssue(page, number, title) {
  const link = page.getByRole("link", {
    name: `查看 Issue #${number}`,
    exact: true,
  });
  assert.equal(await link.getAttribute("href"), `/issues/${number}`);
  await link.click();
  await page.waitForURL(`${url}/issues/${number}`);
  await page
    .getByRole("heading", { name: `#${number} ${title}`, exact: true })
    .waitFor();
}

function dispatchRow(page, text) {
  return page
    .locator(".message")
    .filter({ has: page.getByText(text, { exact: true }) });
}

async function verifyOwner(page, expected, size, capture = false) {
  const properties = page.getByRole("complementary", { name: "工作属性" });
  const labels = properties.locator("dl dt");
  const owner = properties.getByText(expected, { exact: true });
  await owner.waitFor();
  assert.deepEqual((await labels.allTextContents()).slice(0, 2), [
    "阶段",
    "负责 Agent",
  ]);
  assert.equal(
    await properties.locator("dl dd").nth(1).textContent(),
    expected,
  );
  assert(
    await labels.nth(1).evaluate((label) => {
      const range = document.createRange();
      range.selectNodeContents(label);
      return (
        range.getBoundingClientRect().height <
        1.5 * parseFloat(globalThis.getComputedStyle(label).fontSize)
      );
    }),
    "the Owner label fits on one line",
  );
  if (!capture) return;

  const panel = await properties.boundingBox();
  const summary = await page.locator(".issue-detail-summary").boundingBox();
  if (size === "mobile") {
    assert.equal(panel.x, summary.x);
    assert(panel.y > summary.y + summary.height);
    await page
      .getByRole("navigation", { name: "Issue 入口" })
      .getByRole("link", { name: "工作属性" })
      .click();
    await page.waitForURL(/#issue-properties$/);
  } else {
    assert(panel.x > summary.x + summary.width);
  }
  await owner.scrollIntoViewIfNeeded();
  assert(await owner.isVisible());
  if (screenshots) {
    await mkdir(screenshots, { recursive: true });
    await page.screenshot({
      path: join(screenshots, `issue-owner-${size}.png`),
    });
  }
}

try {
  for (const [size, width, height] of [
    ["desktop", 1440, 1000],
    ["mobile", 390, 844],
  ]) {
    const page = await browser.newPage({ viewport: { width, height } });
    collectPageErrors(page, errors);
    page.on("console", (message) => {
      if (message.type() === "warning") errors.push(message.text());
    });
    await page.goto(url);
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    await openIssue(page, parent, "示例仓库上手指南");
    const dispatch = dispatchRow(page, objective);
    await dispatch.getByText("Tend", { exact: true }).waitFor();
    await dispatch.getByText("来自主会话", { exact: true }).waitFor();
    assert.equal(
      await dispatch.getByRole("link", { name: "原对话" }).getAttribute("href"),
      `/#message-${source}`,
    );
    const claims = page.locator(".takeover-row");
    if (phase === "issued") {
      assert.equal(await claims.count(), 0);
      await verifyOwner(page, "未领取", size);
      await verifyAndCapture(page, "issued", size);
      await dispatch.getByRole("link", { name: "原对话" }).click();
      await page.waitForURL(`${url}/#message-${source}`);
      await page
        .locator(`#message-${source}`)
        .getByText(originalText, { exact: true })
        .waitFor();
      await openIssue(page, legacy, "稍后整理发布说明");
      await verifyOwner(page, "未领取", size);
      const unnamed = dispatchRow(page, legacyObjective);
      await unnamed.getByText("来自主会话", { exact: true }).waitFor();
      assert.equal(
        await unnamed.locator(".author").textContent(),
        "来自主会话",
      );
      assert.equal(
        await unnamed
          .getByRole("link", { name: "原对话" })
          .getAttribute("href"),
        `/#message-${legacySource}`,
      );
      assert.equal(await claims.count(), 0);
      await verifyAndCapture(page, "historical", size);
      await page.goto(`${url}/issues/${other}`);
      const otherDispatch = dispatchRow(page, otherObjective);
      await otherDispatch.getByText("来自其他会话", { exact: true }).waitFor();
      assert.equal(
        await otherDispatch.getByText("来自主会话", { exact: true }).count(),
        0,
      );
      const otherLink = otherDispatch.getByRole("link", { name: "原对话" });
      assert.equal(
        await otherLink.getAttribute("href"),
        `/?chat=other#message-${otherSource}`,
      );
      await verifyAndCapture(page, "other-chat", size);
      await otherLink.click();
      await page.waitForURL(`${url}/?chat=other#message-${otherSource}`);
      await page
        .locator(`#message-${otherSource}`)
        .getByText(otherOriginal, { exact: true })
        .waitFor();
    } else {
      const childDispatch = dispatchRow(page, childObjective);
      await childDispatch.getByText("coordinator", { exact: true }).waitFor();
      assert.equal(
        await childDispatch.getByText("来自主会话", { exact: true }).count(),
        0,
      );
      assert.deepEqual(await claims.locator("span").allTextContents(), [
        "coordinator 已接手",
        "worker 已接手",
      ]);
      assert.equal(await claims.getByRole("button").count(), 0);
      assert.deepEqual(
        await page
          .locator(".message, .takeover-row")
          .evaluateAll(
            (rows, childText) =>
              rows
                .map((row) =>
                  row.classList.contains("takeover-row")
                    ? row.querySelector("span").textContent
                    : row.querySelector("p")?.textContent,
                )
                .filter(
                  (text) => text?.includes("已接手") || text === childText,
                ),
            childObjective,
          ),
        ["coordinator 已接手", childObjective, "worker 已接手"],
      );
      await verifyAndCapture(page, "accepted", size);
      await verifyOwner(page, "coordinator", size, true);
      const childLink = claims.getByRole("link", {
        name: `#${child} 整理快速上手指南`,
      });
      assert.equal(await childLink.getAttribute("href"), `/issues/${child}`);
      await childLink.click();
      await page.waitForURL(`${url}/issues/${child}`);
      await page
        .getByRole("heading", {
          name: `#${child} 整理快速上手指南`,
          exact: true,
        })
        .waitFor();
      await claims.getByText("worker 已接手", { exact: true }).waitFor();
      assert.equal(await claims.count(), 1);
      await verifyOwner(page, "worker", size);
      await verifyAndCapture(page, "child", size);
    }
    await page.close();
  }
  assert.deepEqual(errors, []);
} finally {
  await browser.close();
}
console.log(
  `PASS built Issue timeline and owner (${phase}) at 1440×1000 and 390×844`,
);
