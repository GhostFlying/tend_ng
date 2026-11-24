import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";

const url = process.env.TEND_URL;
const token = process.env.TEND_TOKEN;
const screenshots = process.env.TEND_SCREENSHOTS;
const question = "使用说明你想要精简版，还是完整版？";
const answer = "用精简版。";
assert(url && token);

const browser = await chromium.launch({ args: ["--no-proxy-server"] });

async function connect(viewport) {
  const page = await browser.newPage({ viewport });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (
      message.type() === "error" &&
      !message.text().includes("net::ERR_FAILED")
    ) {
      errors.push(message.text());
    }
  });
  await page.goto(url);
  await page.getByLabel("访问令牌").fill(token);
  await page.getByRole("button", { name: "连接", exact: true }).click();
  await page
    .locator(".message:has(.attention-actions)")
    .getByText(question, { exact: true })
    .waitFor();
  assert.equal(await page.title(), "Tend");
  return { page, errors };
}

try {
  const desktop = await connect({ width: 1440, height: 1000 });
  const page = desktop.page;
  const post = page
    .locator(".message:has(.attention-actions)")
    .filter({ has: page.getByText(question, { exact: true }) });
  await post.getByText("待你决定").waitFor();
  await post.getByRole("button", { name: "回复", exact: true }).click();
  const input = page.getByLabel("发消息给 Tend");
  await input.fill(answer);
  await page.getByText("回复：" + question, { exact: true }).waitFor();
  if (screenshots) {
    await mkdir(screenshots, { recursive: true });
    await page.screenshot({
      path: join(screenshots, "attention-awaiting.png"),
    });
  }

  const requests = [];
  await page.route("**/api/v1/chats/main/messages", async (route) => {
    if (route.request().method() !== "POST") return route.continue();
    const body = route.request().postDataJSON();
    requests.push(body);
    if (requests.length === 1) {
      const stored = await route.fetch();
      assert.equal(stored.status(), 200);
      await route.abort();
      return;
    }
    await route.continue();
  });
  await input.press("Enter");
  await page.getByText(/无法确认收件/).waitFor();
  await page.reload();
  await page.getByText("回复：" + question, { exact: true }).waitFor();
  assert.equal(await input.inputValue(), answer);
  await page.getByRole("button", { name: "发送", exact: true }).click();
  await post.getByText("Agent 已接续").waitFor();
  assert.equal(
    await post.getByRole("button", { name: "回复", exact: true }).count(),
    0,
  );
  await page
    .getByRole("button", { name: "取消引用", exact: true })
    .waitFor({ state: "detached" });

  assert.equal(requests.length, 2);
  assert.deepEqual(requests[0], requests[1]);
  assert.equal(
    "message-" + requests[0].reply_to,
    await post.getAttribute("id"),
  );
  assert.equal(await page.getByText(answer, { exact: true }).count(), 1);
  assert.deepEqual(desktop.errors, []);

  const mobile = await connect({ width: 390, height: 844 });
  await mobile.page.getByText("Agent 已接续").waitFor();
  assert.equal(
    await mobile.page.evaluate(() => document.documentElement.scrollWidth),
    390,
  );
  assert.deepEqual(mobile.errors, []);
  if (screenshots) {
    await mobile.page.screenshot({
      path: join(screenshots, "attention-delivered.png"),
    });
  }
  console.log(
    "PASS Attention in Chat with a quoted reply, lost receipt, and live status",
  );
} finally {
  await browser.close();
}
