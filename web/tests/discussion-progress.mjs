import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { existsSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { chromium } from "playwright";
import { isAnonymousSessionProbe } from "./console-errors.mjs";

const {
  TEND_URL: base,
  TEND_TOKEN: token,
  TEND_TEST_APP_HOME: home,
  TEND_SCREENSHOTS: output,
  TEND_RESULT_FILE: result,
} = process.env;
const title = "首版数据导出方案";
const prompt = "请比较 CSV 和 JSON，给出第一版导出建议。";
const first = "我会先比较可读性、字段结构和后续接入成本。";
const final =
  "首版用 CSV。它方便直接查看和导入表格。对需要嵌套字段的场景，再提供 JSON。";
const errors = [];

async function api(path, body) {
  const response = await fetch(`${base}/api/v1/${path}`, {
    method: body ? "POST" : "GET",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: body && JSON.stringify(body),
  });
  const value = await response.json();
  assert.ok(response.ok, JSON.stringify(value));
  return value;
}

async function until(read, ready, explanation) {
  const deadline = Date.now() + 12_000;
  let value;
  do {
    value = await read();
    if (ready(value)) return value;
    await delay(60);
  } while (Date.now() < deadline);
  assert.fail(`${explanation}: ${JSON.stringify(value)}`);
}

const { chat_id: chat } = await api("discussions", {
  operation_id: randomUUID(),
  title,
});
const url = `${base}/discussions?chat=${chat}`;
const path = `chats/${chat}/messages`;
const readRows = async () => (await api(path)).messages;
const rowFor = (rows, text) => rows.find((row) => row.text === text);
const repliesTo = (rows, message) =>
  rows.filter((row) => row.reply_to === message.id);
const progressOf = (row) =>
  row.discussion_progress?.map((item) => [item.item_id, item.text]);
const release = (gate) => writeFileSync(join(home, gate), "");

async function send(page, text) {
  const composer = page.getByRole("textbox", { name: "在讨论中发消息" });
  await composer.fill(text);
  await composer.press("Enter");
}

async function settled(text, answer) {
  const rows = await until(
    readRows,
    (rows) => rowFor(rows, text)?.router_state === "completed",
    `turn did not settle for ${text}`,
  );
  const user = rowFor(rows, text);
  const replies = repliesTo(rows, user);
  assert.equal(replies.length, 1);
  assert.equal(replies[0].text, answer);
  assert.equal(replies[0].author, "discussion");
  assert.ok(!("discussion_progress" in replies[0]));
  return user;
}

async function pageFor(browser, viewport, mobile = false) {
  const context = await browser.newContext({
    viewport,
    isMobile: mobile,
    hasTouch: mobile,
    locale: "zh-CN",
    colorScheme: "light",
  });
  const page = await context.newPage();
  page.setDefaultTimeout(9000);
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    const faviconProbe =
      message.location().url === `${base}/favicon.ico` &&
      /\b401\b/.test(message.text());
    if (
      ["error", "warning"].includes(message.type()) &&
      !isAnonymousSessionProbe(message) &&
      !faviconProbe
    ) {
      errors.push(`${message.text()} @ ${message.location().url}`);
    }
  });
  page.on("dialog", async (dialog) => {
    errors.push(`unexpected dialog: ${dialog.message()}`);
    await dialog.dismiss();
  });
  await page.goto(url);
  await page.getByLabel("访问令牌").fill(token);
  await page.getByRole("button", { name: "连接", exact: true }).click();
  await page.getByRole("heading", { name: title, exact: true }).waitFor();
  return page;
}

async function render(page, name, progress) {
  assert.equal(page.url(), url);
  assert.match(await page.title(), /Tend/i);
  await page.getByRole("heading", { name: title, exact: true }).waitFor();
  assert.equal(await page.locator("vite-error-overlay").count(), 0);
  assert.equal(await page.getByText(/HIDDEN_/).count(), 0);
  assert.ok(
    await page.evaluate(
      () =>
        document.documentElement.scrollWidth <=
        document.documentElement.clientWidth,
    ),
  );
  const box = await progress.boundingBox();
  const timeline = await page.locator("main.timeline").boundingBox();
  assert.ok(
    box &&
      timeline &&
      box.x >= 0 &&
      box.x + box.width <= page.viewportSize().width + 0.5,
  );
  assert.ok(
    box.y >= timeline.y && box.y + box.height <= timeline.y + timeline.height,
  );
  if (output)
    await page.screenshot({ path: join(output, `${name}.png`), scale: "css" });
}

function row(page, user) {
  return page.locator(`#message-${user.id}`);
}

function live(page, user) {
  return row(page, user).locator(".discussion-progress-live");
}

const browser = await chromium.launch({ channel: "chromium" });
try {
  const [desktop, mobile] = await Promise.all([
    pageFor(browser, { width: 1440, height: 960 }),
    pageFor(browser, { width: 390, height: 844 }, true),
  ]);
  await send(desktop, prompt);
  const starting = await until(
    readRows,
    (rows) => rowFor(rows, prompt)?.discussion_progress?.length,
    "first item never arrived",
  );
  const user = rowFor(starting, prompt);
  assert.equal(user.router_state, "awaiting_turn");
  assert.deepEqual(progressOf(user), [["compare-scope", first]]);
  assert.equal(starting.length, 1, "a progress event must not create a reply");
  for (const page of [desktop, mobile]) {
    await live(page, user).getByText(first, { exact: true }).waitFor();
    assert.equal(await page.getByText(/首版用 CSV/).count(), 0);
  }
  release("progress-second");
  const secondPrefix = "两种格式都能保存同样的字段。";
  for (const [page, name] of [
    [desktop, "desktop-live"],
    [mobile, "mobile-390-live"],
  ]) {
    const line = live(page, user).locator(".discussion-progress-latest");
    await live(page, user).getByText(new RegExp(secondPrefix)).waitFor();
    assert.equal(await live(page, user).count(), 1);
    assert.equal(
      await live(page, user).getByText(first, { exact: true }).count(),
      0,
    );
    assert.equal(await page.locator(".message").count(), 1);
    await line.evaluate((element) => {
      const style = globalThis.getComputedStyle(element);
      if (
        style.textOverflow !== "ellipsis" ||
        style.whiteSpace !== "nowrap" ||
        element.scrollWidth <= element.clientWidth ||
        element.clientHeight > parseFloat(style.lineHeight) + 0.5
      ) {
        throw new Error("progress must clip to one line with an ellipsis");
      }
    });
    await render(page, name, live(page, user));
  }
  assert.ok(!existsSync(join(home, "progress-final")));
  const held = await readRows();
  assert.equal(held.length, 1);
  assert.equal(held[0].router_state, "awaiting_turn");
  assert.deepEqual(
    held[0].discussion_progress.map((item) => item.item_id),
    ["compare-scope", "compare-shape"],
  );
  await mobile.reload();
  await live(mobile, user).getByText(new RegExp(secondPrefix)).waitFor();
  assert.equal(await mobile.locator(".message").count(), 1);
  release("progress-final");

  const completed = await settled(
    prompt,
    final.replace("首版用 CSV。", "**首版用 CSV。** "),
  );
  assert.deepEqual(completed.discussion_progress, held[0].discussion_progress);
  for (const [page, name] of [
    [desktop, "desktop-completed"],
    [mobile, "mobile-390-expanded"],
  ]) {
    const history = row(page, user).locator(
      "details.discussion-progress-history",
    );
    await history.locator("summary").waitFor();
    await page.getByText("首版用 CSV。", { exact: true }).waitFor();
    assert.equal(await live(page, user).count(), 0);
    assert.equal(await history.getAttribute("open"), null);
    assert.equal(
      await history.getByText(first, { exact: true }).isVisible(),
      false,
    );
    if (page === mobile)
      await history.getByText("查看回合进展 · 2 条", { exact: true }).click();
    await render(page, name, history);
    if (page === desktop) await history.locator("summary").press("Enter");
    assert.deepEqual(
      await history.locator("li").allTextContents(),
      completed.discussion_progress.map((item) => item.text),
    );
    await history.getByText(first, { exact: true }).waitFor();
    await page.waitForResponse(
      (response) =>
        response.url() === `${base}/api/v1/${path}` && response.ok(),
    );
    assert.ok(
      await history.getByText(first, { exact: true }).isVisible(),
      "poll silently closed the history",
    );
    await history.locator("summary").click();
    assert.equal(await history.getAttribute("open"), null);
  }
  console.log(
    "PASS desktop and 390px mobile show one clipped live row before final; collapse, expand and refresh work",
  );

  const recovery = "Recover discussion progress";
  await send(desktop, recovery);
  const recoveryRows = await until(
    readRows,
    (rows) => rowFor(rows, recovery)?.discussion_progress?.length,
    "recovery precondition missing",
  );
  const recovering = rowFor(recoveryRows, recovery);
  await live(desktop, recovering)
    .getByText("恢复之前已显示的进展。", { exact: true })
    .waitFor();
  assert.equal(repliesTo(recoveryRows, recovering).length, 0);
  release("progress-recover");
  const recovered = await settled(recovery, "恢复原回合的正式回复。");
  assert.deepEqual(progressOf(recovered), [
    ["seen", "恢复之前已显示的进展。"],
    ["native-proof", "从原生完成记录恢复的进展。"],
  ]);
  assert.equal(
    new Set(recovered.discussion_progress.map((item) => item.attempt_id)).size,
    1,
  );
  assert.equal(
    recovered.discussion_progress[0].attempt_id,
    recovering.discussion_progress[0].attempt_id,
  );

  const retry = "Retry discussion progress";
  await send(desktop, retry);
  const retried = await settled(retry, "重试正常完成。");
  assert.deepEqual(progressOf(retried), [
    ["same-item", "第一次尝试的进展。"],
    ["same-item", "重试的进展。"],
  ]);
  assert.equal(
    new Set(retried.discussion_progress.map((item) => item.attempt_id)).size,
    2,
  );

  const empty = "No discussion progress";
  await send(desktop, empty);
  const emptyRows = await until(
    readRows,
    (rows) => rowFor(rows, empty)?.router_state === "awaiting_turn",
    "empty case never started",
  );
  const emptyUser = rowFor(emptyRows, empty);
  await row(desktop, emptyUser).getByText("已收到").waitFor();
  assert.equal(
    await row(desktop, emptyUser).locator(".discussion-progress").count(),
    0,
  );
  release("progress-empty");
  assert.ok(
    !(
      "discussion_progress" in
      (await settled(empty, "没有进展过程的正式回复。"))
    ),
  );
  await desktop
    .getByText("没有进展过程的正式回复。", { exact: true })
    .waitFor();
  assert.equal(
    await row(desktop, emptyUser).locator(".discussion-progress").count(),
    0,
  );

  const literal = "Literal discussion progress";
  await send(desktop, literal);
  const literalUser = await settled(literal, "纯文本测试结束。");
  const textHistory = row(desktop, literalUser).locator(
    "details.discussion-progress-history",
  );
  await textHistory.locator("summary").click();
  await textHistory.getByText(/<img src=x onerror=/).waitFor();
  assert.equal(await textHistory.locator("img, a, script").count(), 0);
  assert.equal(
    await desktop.evaluate(() => globalThis.progressWasHtml),
    undefined,
  );
  const rows = await readRows();
  assert.ok(!JSON.stringify(rows).includes("HIDDEN_"));

  const draft = await api(`discussions/${chat}/draft`, {
    operation_id: randomUUID(),
  });
  await until(
    () => api(`discussions/${chat}/draft?id=${draft.id}`),
    (value) => value.draft_request?.status === "completed",
    "draft did not settle",
  );
  const handoff = `discussions/${chat}/handoff`;
  const { operation_id: operation } = await api(handoff);
  await api(handoff, { operation_id: operation });
  await until(
    () => api(`${handoff}?operation_id=${operation}`),
    (value) => value.request?.status === "sent",
    "handoff did not settle",
  );
  const main = await until(
    () => api("chats/main/messages"),
    (value) => value.messages.length === 2,
    "handoff receiver did not reply",
  );
  assert.ok(
    main.messages.every((message) => !("discussion_progress" in message)),
  );
  assert.deepEqual(await readRows(), rows);
  assert.deepEqual(errors, []);
  writeFileSync(result, JSON.stringify({ chat }));
  console.log(
    "PASS recovery proof, attempt/item dedup, empty turn, inert text, hidden events, draft, handoff and Main Chat isolation",
  );
} finally {
  await browser.close();
}
