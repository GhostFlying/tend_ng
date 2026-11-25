import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { chromium } from "playwright";

const {
  TEND_URL: url,
  TEND_TOKEN: token,
  TEND_ISSUE: issue,
  TEND_FIRST_QUESTION_ID: firstId,
  TEND_SECOND_QUESTION_ID: secondId,
  TEND_FIRST_ATTENTION_ID: firstAttention,
  TEND_ATTENTION_HISTORY: history,
  TEND_HISTORY_FIXTURE_DIR: historyFixture,
  TEND_FIXTURE_PYTHON: python,
  TEND_SCREENSHOTS: screenshots,
} = process.env;
assert(url && token && issue && firstId && secondId);
const browser = await chromium.launch({ args: ["--no-proxy-server"] });
const first = "示例指南要采用精简版还是完整版？";
const second = "示例指南要用简体中文还是英文？";
const firstAnswer = "  用精简版。\n保留 **原文**。  ";
const secondAnswer = "使用简体中文，并保留命令原名。";

async function screenshot(page, name) {
  if (!screenshots) return;
  await mkdir(screenshots, { recursive: true });
  await page.screenshot({ path: join(screenshots, name) });
}

async function connect(viewport) {
  const page = await browser.newPage({ viewport });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (
      ["error", "warning"].includes(message.type()) &&
      !message.text().includes("net::ERR_FAILED")
    ) {
      errors.push(message.text());
    }
  });
  await page.goto(url);
  await page.getByLabel("访问令牌").fill(token);
  await page.getByRole("button", { name: "连接", exact: true }).click();
  await page.getByRole("link", { name: "看板", exact: true }).click();
  return { page, errors };
}

async function openIssue(page) {
  await page.locator(`.board-card-title[href="/issues/${issue}"]`).click();
  await page
    .getByRole("heading", { name: new RegExp(`^#${issue} `) })
    .waitFor();
  assert.equal(new URL(page.url()).pathname, `/issues/${issue}`);
  assert.equal(await page.title(), "Tend");
  assert.equal(await page.locator("vite-error-overlay").count(), 0);
}

function questionRow(page, text) {
  return page.locator(".issue-decisions > li").filter({
    has: page.getByText(text, { exact: true }),
  });
}

async function fillReply(question, value) {
  await question.getByText("回答此问题", { exact: true }).click();
  const field = question.getByLabel("答复原文");
  await field.fill(value);
  assert.equal(await field.inputValue(), value);
  return question.getByRole("button", { name: "提交答复", exact: true });
}

async function answerShown(row, answer) {
  await row.getByText("已记录答复", { exact: true }).waitFor();
  assert.equal(
    await row.locator(".attention-recorded p").textContent(),
    answer,
  );
  assert.equal(await row.getByText("回答此问题", { exact: true }).count(), 0);
}

async function historicalFlow() {
  assert(firstAttention && historyFixture && python);
  const original = await connect({ width: 1280, height: 920 });
  const issueTab = await connect({ width: 1280, height: 920 });
  const chat = original.page;
  const detail = issueTab.page;
  let historyReads = 0;
  let statusReads = 0;
  const statusPath = `/api/v1/chats/main/attentions/${firstAttention}`;
  chat.on("request", (request) => {
    const target = new URL(request.url());
    if (
      target.pathname === "/api/v1/chats/main/messages" &&
      target.searchParams.has("before")
    )
      historyReads++;
    if (target.pathname === statusPath) statusReads++;
  });
  const observed = chat.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === statusPath && response.ok(),
  );
  await chat.goto(`${url}/#message-${firstId}`);
  const oldQuestion = chat.locator(`#message-${firstId}`);
  await oldQuestion.getByText(first, { exact: true }).waitFor();
  await oldQuestion.scrollIntoViewIfNeeded();
  await observed;
  await oldQuestion
    .getByRole("button", { name: "回复", exact: true })
    .waitFor();
  assert(historyReads > 0);
  const initialHistoryReads = historyReads;

  await openIssue(detail);
  await detail.getByText("历史问题 60", { exact: true }).waitFor();
  assert.equal(
    await detail.getByText("历史问题 59", { exact: true }).count(),
    0,
  );
  const load = detail.getByRole("button", {
    name: "读取更早的答复",
    exact: true,
  });
  await load.click();
  await detail.getByText("历史问题 59", { exact: true }).waitFor();
  assert.equal(
    await detail.getByText("历史问题 60", { exact: true }).count(),
    1,
  );
  const submit = await fillReply(questionRow(detail, first), firstAnswer);
  await submit.click();
  await answerShown(questionRow(detail, first), firstAnswer);
  await detail.getByText("历史问题 60", { exact: true }).waitFor();
  await questionRow(detail, second)
    .getByText("回答此问题", { exact: true })
    .waitFor();

  // The Chat stays mounted on the old page. It may first see the saved
  // receipt or go straight to the native takeover observed by the Server.
  await oldQuestion
    .getByText("Agent 已接续", { exact: true })
    .waitFor({ timeout: 20_000 });
  assert.equal(
    await oldQuestion.locator(".attention-recorded p").textContent(),
    firstAnswer,
  );
  assert.equal(
    await oldQuestion
      .getByRole("button", { name: "回复", exact: true })
      .count(),
    0,
  );
  assert.equal(historyReads, initialHistoryReads);
  assert(statusReads >= 2);
  for (let firstInPage = 54; firstInPage >= 1; firstInPage -= 5) {
    await load.focus();
    await detail.keyboard.press("Enter");
    await detail
      .getByText(`历史问题 ${firstInPage}`, { exact: true })
      .waitFor();
  }
  await detail.waitForFunction(
    () => document.activeElement?.id === "issue-attention-history-question-4",
  );
  assert.equal(await load.count(), 0);
  const recorded = detail.locator(".issue-decisions .attention-recorded");
  assert.equal(await recorded.count(), 65);

  async function append(first, last) {
    await promisify(execFile)(
      python,
      [
        "-B",
        "-c",
        "from pathlib import Path; import sys; from issue_attention import append_history_fixture; append_history_fixture(Path(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3]))",
        historyFixture,
        String(first),
        String(last),
      ],
      { cwd: fileURLToPath(new URL("../../tests/e2e", import.meta.url)) },
    );
  }
  await append(65, 69);
  await detail.getByText("历史问题 69", { exact: true }).waitFor();
  assert.equal(await recorded.count(), 70);
  await append(70, 70);
  await detail.getByText("历史问题 70", { exact: true }).waitFor();
  assert.equal(
    await detail.getByText("历史问题 65", { exact: true }).count(),
    1,
  );
  assert.equal(await recorded.count(), 71);
  assert.equal(await load.count(), 0);

  // Six receipts arrive between polls so one isn't in either sampled head.
  await append(71, 76);
  await detail.getByText("历史问题 76", { exact: true }).waitFor();
  assert.equal(
    await detail.getByText("历史问题 71", { exact: true }).count(),
    0,
  );
  assert.equal(await recorded.count(), 76);
  const restart = detail.getByRole("button", { name: "从最新答复重新查看" });
  await restart.focus();
  await detail.keyboard.press("Enter");
  await detail.waitForFunction(
    () => document.activeElement?.id === "issue-decisions",
  );
  assert.equal(await recorded.count(), 5);
  await load.click();
  await detail.getByText("历史问题 71", { exact: true }).waitFor();
  assert.equal(
    await detail.getByText("历史问题 70", { exact: true }).count(),
    1,
  );
  assert.deepEqual(original.errors, []);
  assert.deepEqual(issueTab.errors, []);
  console.log(
    "PASS historical Chat cross-tab refresh, exhausted reply history under live updates, and final-page keyboard focus",
  );
}

async function directFlow() {
  const desktop = await connect({ width: 1440, height: 1000 });
  const page = desktop.page;
  await page.locator(".board-attention").waitFor();
  assert.equal(
    await page.locator(".board-attention").getAttribute("href"),
    `/#message-${firstId}`,
  );
  await openIssue(page);
  const one = questionRow(page, first);
  const two = questionRow(page, second);
  await two.getByText("回答此问题", { exact: true }).waitFor();
  const submit = await fillReply(one, firstAnswer);
  assert.equal(await two.getByLabel("答复原文").isVisible(), false);
  await submit.scrollIntoViewIfNeeded();
  await screenshot(page, "issue-attention-desktop-form.png");

  const posts = [];
  await page.route("**/api/v1/issues/*/attentions/*/answer", async (route) => {
    const body = route.request().postDataJSON();
    posts.push({ path: new URL(route.request().url()).pathname, body });
    if (posts.length === 1) {
      const committed = await route.fetch();
      assert.equal(committed.status(), 200);
      await route.abort();
    } else {
      await route.continue();
    }
  });
  await submit.click();
  await one
    .getByRole("alert")
    .getByText(/暂时无法确认/)
    .waitFor();
  await submit.click();
  await answerShown(one, firstAnswer);
  assert.equal(posts.length, 2);
  assert.deepEqual(posts[0], posts[1]);
  await two.getByText("回答此问题", { exact: true }).waitFor();
  await one.getByRole("link", { name: "查看原提问" }).click();
  assert.equal(new URL(page.url()).hash, `#message-${firstId}`);
  const firstMessage = page.locator(`#message-${firstId}`);
  await firstMessage.getByText("Agent 已接续", { exact: true }).waitFor();
  assert.equal(
    await firstMessage.locator(".attention-recorded p").textContent(),
    firstAnswer,
  );
  assert.equal(
    await firstMessage
      .getByRole("button", { name: "回复", exact: true })
      .count(),
    0,
  );
  await firstMessage.scrollIntoViewIfNeeded();
  await screenshot(page, "issue-attention-desktop-chat.png");

  const mobile = await connect({ width: 390, height: 844 });
  const narrow = mobile.page;
  await narrow.locator(".board-attention").waitFor();
  assert.equal(
    await narrow.locator(".board-attention").getAttribute("href"),
    `/#message-${secondId}`,
  );
  await openIssue(narrow);
  await answerShown(questionRow(narrow, first), firstAnswer);
  const secondRow = questionRow(narrow, second);
  const mobileSubmit = await fillReply(secondRow, secondAnswer);
  await mobileSubmit.scrollIntoViewIfNeeded();
  await screenshot(narrow, "issue-attention-mobile-form.png");
  assert.equal(
    await narrow.evaluate(() => document.documentElement.scrollWidth),
    390,
  );
  const visible = await mobileSubmit.boundingBox();
  assert(visible && visible.x >= 0 && visible.x + visible.width <= 390);
  await mobileSubmit.click();
  await answerShown(secondRow, secondAnswer);
  await narrow
    .getByRole("heading", { name: "已答复的问题", exact: true })
    .waitFor();
  await secondRow.scrollIntoViewIfNeeded();
  await screenshot(narrow, "issue-attention-mobile-record.png");
  await narrow.getByRole("link", { name: "看板", exact: true }).click();
  await narrow.locator(".board-card-title").waitFor();
  assert.equal(await narrow.locator(".board-attention").count(), 0);
  assert.deepEqual(desktop.errors, []);
  assert.deepEqual(mobile.errors, []);
  console.log(
    "PASS two specific Issue replies, lost response retry, Chat and board updates at 1440 and 390 px",
  );
}

try {
  if (history) await historicalFlow();
  else await directFlow();
} finally {
  await browser.close();
}
