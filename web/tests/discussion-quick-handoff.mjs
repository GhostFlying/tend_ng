import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { chromium } from "playwright";
import { isAnonymousSessionProbe } from "./console-errors.mjs";

const {
  TEND_URL: base,
  TEND_TOKEN: token,
  TEND_TEST_CLI: binary,
  TEND_SCREENSHOTS: output,
  TEND_TEST_APP_HOME: home,
} = process.env;

const raw = `  合成数据导出讨论：先比较方案。\n\u3000${"保留完整的首条标题以验证桌面和手机都只在界面截断。".repeat(12)}  `;
const title = raw.trim().replace(/\s+/gu, " ");
const hold = join(home, "handoff-hold");
const fault = join(home, "handoff-invalid");
const errors = [];

function cli(...args) {
  return JSON.parse(execFileSync(binary, args, { encoding: "utf8" }));
}

async function shoot(page, name) {
  assert.equal(new URL(page.url()).origin, base);
  assert.match(await page.title(), /Tend/i);
  assert.ok((await page.locator("main").count()) > 0);
  assert.equal(await page.locator("vite-error-overlay").count(), 0);
  assert.ok(
    await page.evaluate(
      () =>
        document.documentElement.scrollWidth <=
        document.documentElement.clientWidth,
    ),
    `${name} page overflow`,
  );
  await page.screenshot({ path: join(output, `${name}.png`), scale: "css" });
}

async function assertTruncated(locator) {
  await locator.waitFor({ state: "visible" });
  assert.equal(await locator.getAttribute("title"), title);
  assert.equal(await locator.textContent(), title);
  assert.ok(
    await locator.evaluate(
      (element) => element.scrollWidth > element.clientWidth,
    ),
    "full title should stay available while CSS truncates its rendering",
  );
}

function mainCount(chat) {
  return cli("chat", "read", "main").messages.filter(
    (message) => message.discussion_id === chat,
  ).length;
}

const browser = await chromium.launch({ channel: "chromium" });
try {
  for (const [name, viewport] of Object.entries({
    desktop: { width: 1440, height: 960 },
    mobile: { width: 390, height: 844 },
  })) {
    const { chat_id: chat } = cli("discussion", "create");
    const url = `${base}/discussions?chat=${chat}`;
    const context = await browser.newContext({
      viewport,
      isMobile: name === "mobile",
      hasTouch: name === "mobile",
      locale: "zh-CN",
      colorScheme: "light",
    });
    const page = await context.newPage();
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (message) => {
      // Chromium probes a favicon even though the app serves none.
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
    await page.goto(url);
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    await page.getByRole("heading", { name: "未命名讨论" }).waitFor();
    await shoot(page, `${name}-01-empty`);

    const composer = page.getByRole("textbox", { name: "在讨论中发消息" });
    await composer.fill(raw);
    await composer.press("Enter");
    const header = page.getByRole("heading", { name: title, exact: true });
    await assertTruncated(header);
    if (name === "desktop") {
      const recent = page.locator(
        `.discussion-sidebar-history a[href="/discussions?chat=${chat}"]`,
      );
      assert.equal(await recent.getAttribute("title"), title);
      assert.ok(
        await recent
          .locator("span")
          .evaluate((element) => element.scrollWidth > element.clientWidth),
      );
    }
    assert.equal(cli("discussion", "read", chat).title, title);
    assert.equal(
      cli("discussion", "list").discussions.find((row) => row.chat_id === chat)
        .title,
      title,
    );
    await shoot(page, `${name}-02-long-page-title`);
    await page.getByRole("link", { name: "所有讨论", exact: true }).click();
    await page.waitForURL(`${base}/discussions`);
    const item = page.locator(
      `.discussion-list a[href="/discussions?chat=${chat}"]`,
    );
    await assertTruncated(item.locator("strong"));
    await shoot(page, `${name}-03-long-list-title`);
    await item.click();
    await page.waitForURL(url);
    await assertTruncated(header);
    assert.equal(mainCount(chat), 0);

    writeFileSync(hold, "");
    writeFileSync(fault, "");
    const handoff = page.getByRole("button", {
      name: "转交主 Chat",
      exact: true,
    });
    await handoff.dblclick();
    await page.getByText("正在生成摘要并转交…", { exact: true }).waitFor();
    assert.equal(mainCount(chat), 0);
    await shoot(page, `${name}-04-queued`);
    // Pending state is durable, not just a spinner in the initiating page.
    await page.reload();
    await page.getByText("正在生成摘要并转交…", { exact: true }).waitFor();
    rmSync(hold);
    const error = page.getByText(/转交失败，未发送到主 Chat/);
    await error.waitFor();
    assert.equal(mainCount(chat), 0);
    await shoot(page, `${name}-05-failed-retry`);

    await page
      .getByRole("button", { name: "预览 / 编辑", exact: true })
      .click();
    const dialog = page.getByRole("dialog", { name: "预览 / 编辑转交" });
    await dialog.getByRole("button", { name: "手动填写", exact: true }).click();
    await dialog
      .getByRole("textbox", { name: "目标", exact: true })
      .fill("合成讨论摘要（手填预览）");
    await dialog
      .getByRole("textbox", { name: "待定事项", exact: true })
      .fill("保留天数待确认");
    await dialog
      .getByRole("textbox", { name: "授权终点", exact: true })
      .fill("只讨论，未授权实施");
    await shoot(page, `${name}-06-preview-edit`);
    await dialog.getByRole("button", { name: "关闭转交草稿" }).click();
    assert.equal(mainCount(chat), 0);

    rmSync(fault);
    await page.getByRole("button", { name: "重试转交", exact: true }).click();
    const sent = page.getByRole("link", {
      name: "查看主 Chat 消息 ↗",
      exact: true,
    });
    await sent.waitFor();
    assert.equal(mainCount(chat), 1);
    await shoot(page, `${name}-07-sent`);
    await sent.click();
    await page.waitForURL(/\/#message-/);
    const delivered = page.locator(new URL(page.url()).hash);
    await delivered.getByText(/不是新的实施授权或命令/).waitFor();
    await delivered.getByRole("link", { name: "回到原讨论 ↗" }).click();
    await page.waitForURL(url);
    await sent.waitFor();
    assert.equal(mainCount(chat), 1);
    if (name === "desktop") {
      const sendPrompt = async (text) => {
        const input = page.getByRole("textbox", { name: "在讨论中发消息" });
        const saved = page.waitForResponse(
          (response) =>
            new URL(response.url()).pathname ===
              `/api/v1/chats/${chat}/messages` &&
            response.request().method() === "POST" &&
            response.ok(),
        );
        await input.fill(text);
        await input.press("Enter");
        await saved;
        await page.getByText(text, { exact: true }).waitFor();
      };
      await sendPrompt("第一次失败转交的补充讨论");
      await page.waitForFunction(
        () =>
          ![...document.querySelectorAll("button")].find(
            (button) => button.textContent.trim() === "转交主 Chat",
          )?.disabled,
      );
      writeFileSync(fault, "");
      await page
        .getByRole("button", { name: "转交主 Chat", exact: true })
        .click();
      await page.getByText(/转交失败，未发送到主 Chat/).waitFor();
      await sendPrompt("新的实际用户输入应当可以独立转交");
      await page.reload();
      await page.getByText(/先前转交失败/).waitFor();
      const fresh = page.getByRole("button", {
        name: "转交主 Chat",
        exact: true,
      });
      await shoot(page, "desktop-08-failed-previous-new-operation");
      rmSync(fault);
      await fresh.click();
      await sent.waitFor();
      assert.equal(mainCount(chat), 2);
      await page.getByText(/先前转交失败/).waitFor();
      console.log(
        "PASS failed prior operation remains retryable while a fresh prompt can transfer after reload",
      );
    }
    await context.close();
    console.log(
      `PASS Chromium ${name} ${viewport.width}×${viewport.height}: empty → title → quick queued → persisted failure → manual preview → retry → one receipt → back link`,
    );
  }
  assert.deepEqual(errors, []);
  console.log(
    "PASS page identity, content, overflow, error overlay and console checks",
  );
} finally {
  rmSync(hold, { force: true });
  rmSync(fault, { force: true });
  await browser.close();
}
