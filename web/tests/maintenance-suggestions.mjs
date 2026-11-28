import assert from "node:assert/strict";
import { readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";
import { isAnonymousSessionProbe } from "./console-errors.mjs";

const {
  TEND_URL: base,
  TEND_TOKEN: token,
  TEND_SCREENSHOTS: output,
  TEND_CASE_SCAN: scan,
  TEND_CASE_TASK: task,
  TEND_EXISTING_DISCUSSION: existing,
  TEND_MAINTENANCE_FEISHU_SCENARIO: scenario,
} = process.env;
const root = `${base}/maintenance-suggestions`;
const errors = [];
const browser = await chromium.launch({ channel: "chromium" });
try {
  for (const [name, viewport] of Object.entries({
    desktop: { width: 1440, height: 960 },
    mobile: { width: 390, height: 844 },
  })) {
    const mobile = name === "mobile";
    const context = await browser.newContext({
      viewport,
      isMobile: mobile,
      hasTouch: mobile,
      locale: "zh-CN",
      colorScheme: "light",
      timezoneId: "Asia/Shanghai",
    });
    const page = await context.newPage();
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (log) => {
      if (
        log.type() === "error" &&
        !isAnonymousSessionProbe(log) &&
        !log.location().url.endsWith("/favicon.ico")
      )
        errors.push(log.text());
    });
    await page.goto(base);
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    await page
      .getByRole("navigation", { name: mobile ? "手机导航" : "主导航" })
      .getByRole("link", { name: "维护建议", exact: true })
      .click();
    await page.waitForURL(root);
    const list = page.getByRole("region", { name: "建议列表" });
    const detail = page.getByRole("region", { name: "建议详情" });
    const scanRow = () => list.getByRole("link", { name: /飞书扫描停止推进/ });
    await scanRow().waitFor();
    assert.equal(await list.locator(".maint-item").count(), 2);
    if (mobile) {
      await shot(page, "maintenance-mobile-list");
      assert.equal(await detail.isVisible(), false);
    }
    await scanRow().click();
    await page.waitForURL(`${root}?id=${scan}`);
    await detail.getByRole("heading", { name: "飞书扫描停止推进" }).waitFor();
    assert.match(await detail.innerText(), /首次观察/);
    assert.match(await detail.innerText(), /最近异常观察/);
    assert.match(await detail.innerText(), /尚未确定/);
    assert.equal(
      await detail
        .getByRole("link", { name: /飞书扫描状态与检查点/ })
        .getAttribute("href"),
      "/feishu-suggestions#scan-status",
    );
    await shot(
      page,
      mobile ? "maintenance-mobile-detail" : "maintenance-desktop",
    );

    if (mobile) {
      await detail.getByRole("button", { name: "转给原主 Chat" }).click();
      await detail.getByRole("heading", { name: "发送前查看说明" }).waitFor();
      assert.match(await detail.innerText(), /不授权调查/);
      const back = detail.getByRole("link", { name: "返回维护建议" });
      assert.equal(
        await back.getAttribute("href"),
        `/maintenance-suggestions?id=${scan}`,
      );
      await detail.getByRole("button", { name: "确认转给原主 Chat" }).click();
      await page.getByRole("link", { name: /打开原主 Chat/ }).click();
      await page.waitForURL(/\/#message-/);
      const note = page
        .locator(".message")
        .filter({ hasText: "维护建议 · 飞书扫描停止推进" });
      await note.getByRole("button", { name: "回复", exact: true }).waitFor();
      await note.getByRole("link", { name: "返回维护建议" }).click();
      await page.waitForURL(`${root}?id=${scan}`);
      await detail.getByRole("link", { name: /返回建议列表/ }).click();
      await page.waitForURL(root);
      await scanRow().waitFor();
    } else {
      await detail.getByRole("button", { name: "忽略此版本" }).click();
      await detail.getByRole("button", { name: "撤销忽略" }).waitFor();
      await list.getByRole("link", { name: "已忽略", exact: true }).click();
      await page.waitForURL(`${root}?view=ignored`);
      await scanRow().click();
      await detail.getByRole("button", { name: "撤销忽略" }).click();
      await detail.getByRole("button", { name: "忽略此版本" }).waitFor();
      await list.getByRole("link", { name: "待查看", exact: true }).click();
      await scanRow().waitFor();
    }

    await list.getByRole("link", { name: /后台发布索引核对/ }).click();
    await page.waitForURL(`${root}?id=${task}`);
    await detail.getByRole("heading", { name: /后台发布索引核对/ }).waitFor();
    assert.match(
      await detail
        .getByRole("link", { name: /Issue #\d+ · 执行证据/ })
        .getAttribute("href"),
      /^\/issues\/\d+#message-index-run$/,
    );
    await detail.getByRole("button", { name: "带入讨论" }).click();
    if (mobile) {
      await page.getByLabel("选择讨论").selectOption(existing);
      await detail.getByRole("button", { name: "带入所选讨论" }).click();
    } else {
      await detail.getByRole("button", { name: "新建讨论并带入" }).click();
    }
    await page.getByRole("link", { name: /打开讨论/ }).click();
    await page.waitForURL(/\/discussions\?chat=.+#message-/);
    const note = page
      .locator(".message")
      .filter({ hasText: "维护建议 · Task 执行失败" });
    await note.getByRole("link", { name: "返回维护建议" }).waitFor();
    await note.getByRole("link", { name: "返回维护建议" }).click();
    await page.waitForURL(`${root}?id=${task}`);
    await detail.getByRole("heading", { name: /后台发布索引核对/ }).waitFor();
    await context.close();
  }
  assert.deepEqual(errors, []);
  await checkUncertainShares();
  console.log(
    "PASS desktop and 390px Chromium navigation, evidence, ignore, preview, and manual destinations",
  );
} finally {
  await browser.close();
}

async function checkUncertainShares() {
  const context = await browser.newContext();
  const api = context.request;
  const headers = { Authorization: `Bearer ${token}` };
  const source = "/api/v1/feishu-suggestions";
  const target = `/api/v1/maintenance-suggestions/${scan}`;
  const read = async (path) => {
    const response = await api.get(base + path, { headers });
    assert.ok(response.ok(), await response.text());
    return response.json();
  };
  const evidence = async () => (await read(target)).suggestion;
  const mode = async (name, previous) => {
    const fixture = JSON.parse(await readFile(scenario, "utf8"));
    const temporary = `${scenario}.browser`;
    await writeFile(temporary, JSON.stringify({ ...fixture, mode: name }));
    await rename(temporary, scenario);
    const sync = await api.post(`${base}${source}/sync`, { headers, data: {} });
    assert.equal(sync.status(), 202);
    const deadline = Date.now() + 26_000;
    while (Date.now() < deadline) {
      const current = await evidence();
      if (current.revision > previous.revision) return current;
      await new Promise((resolve) => setTimeout(resolve, 200));
    }
    assert.fail("the real Feishu observer did not publish the new evidence");
  };

  try {
    // A real new provider result, not a synthetic response from the list API.
    const original = await mode("incomplete", await evidence());
    assert.match(original.fault.known, /不完整的搜索覆盖/);
    const login = await context.newPage();
    await login.goto(base);
    await login.getByLabel("访问令牌").fill(token);
    await login.getByRole("button", { name: "连接", exact: true }).click();
    await login.getByRole("navigation", { name: "主导航" }).waitFor();

    const actions = [
      {
        path: "handoff",
        click: "转给原主 Chat",
        confirm: "确认转给原主 Chat",
        destination: "转给原主 Chat",
      },
      {
        path: "discussions",
        click: "带入讨论",
        confirm: "新建讨论并带入",
        destination: "新建讨论并带入",
      },
      {
        path: "discussions",
        click: "带入讨论",
        confirm: "带入所选讨论",
        chat: existing,
        destination: "带入讨论「已有的合成维护讨论」",
      },
    ];
    const pending = await Promise.all(
      actions.map(async ({ path, click, confirm, chat, destination }) => {
        const page = await context.newPage();
        const attempts = [];
        await page.route(`${base}${target}/${path}`, async (route) => {
          // Apply the authenticated request to the real Server. Only damage
          // delivery of the first body after the receipt has been committed.
          const upstream = await route.fetch();
          attempts.push({
            body: route.request().postDataJSON(),
            status: upstream.status(),
            receipt: await upstream.json(),
          });
          if (attempts.length === 1)
            await route.fulfill({ response: upstream, body: "{" });
          else await route.fulfill({ response: upstream });
        });
        await page.goto(`${root}?id=${scan}`);
        const detail = page.getByRole("region", { name: "建议详情" });
        await detail.getByRole("button", { name: click, exact: true }).click();
        if (chat) await detail.getByLabel("选择讨论").selectOption(chat);
        await detail
          .getByRole("button", { name: confirm, exact: true })
          .click();
        await page
          .getByRole("alert")
          .getByText(/没有收到分享回执/)
          .waitFor();
        assert.equal(attempts[0].status, 200);
        assert.deepEqual(attempts[0].body, {
          ...(chat ? { chat_id: chat } : {}),
          revision: original.revision,
        });
        return { page, detail, attempts, click, confirm, destination };
      }),
    );

    const originalChats = await read("/api/v1/discussions");
    const chatNotes = async (chat) =>
      (await read(`/api/v1/chats/${chat}/messages`)).messages.filter(
        (note) => note.author === "maintenance",
      );
    const updated = await mode("stalled", original);
    await Promise.all(
      pending.map(
        async (
          { page, detail, attempts, click, confirm, destination },
          index,
        ) => {
          const recovery = page.getByRole("region", { name: "分享查证" });
          await detail
            .getByText(`证据更新 · 版本 ${updated.revision}`, { exact: true })
            .waitFor({ timeout: 36_000 });
          await recovery
            .getByText(`证据版本 ${original.revision} · ${destination}`, {
              exact: false,
            })
            .waitFor();
          assert.equal(
            await page.getByRole("button", { name: "放弃查证" }).count(),
            0,
          );
          if (index === 0) {
            await detail.getByRole("link", { name: /返回建议列表/ }).click();
            await page.waitForURL(root);
            await recovery
              .getByRole("link", { name: /飞书扫描停止推进/ })
              .click();
          } else {
            const lists = page.getByRole("region", { name: "建议列表" });
            await lists.getByRole("link", { name: "观察记录" }).click();
            await page.waitForURL(`${root}?view=history`);
            await recovery
              .getByRole("button", { name: "重试原提交" })
              .waitFor();
            if (index === 2) await shot(page, "maintenance-pending-share");
            await lists.getByRole("link", { name: "待查看" }).click();
            await lists.getByRole("link", { name: /飞书扫描停止推进/ }).click();
          }
          await page.waitForURL(`${root}?id=${scan}`);
          const shareButton = detail.getByRole("button", {
            name: click,
            exact: true,
          });
          await shareButton.waitFor();
          assert.ok(await shareButton.isDisabled());
          assert.equal(
            await detail
              .getByRole("button", { name: confirm, exact: true })
              .count(),
            0,
          );
          assert.equal(attempts.length, 1);
          const first = attempts[0];
          const before = await chatNotes(first.receipt.chat_id);
          assert.equal(
            before.filter((note) => note.id === first.receipt.message_id)
              .length,
            1,
          );
          await recovery.getByRole("button", { name: "重试原提交" }).click();
          const link = page.getByRole("link", {
            name: /打开(?:讨论|原主 Chat)/,
          });
          await link.waitFor();
          assert.equal(await link.getAttribute("href"), first.receipt.href);
          assert.deepEqual(attempts, [first, first]);
          assert.deepEqual(await chatNotes(first.receipt.chat_id), before);
        },
      ),
    );
    assert.deepEqual(await read("/api/v1/discussions"), originalChats);

    // A new version is a separate explicit confirmation, not a recovery retry.
    const main = pending[0];
    const before = await chatNotes("main");
    await main.detail
      .getByRole("button", { name: "转给原主 Chat", exact: true })
      .click();
    assert.equal(main.attempts.length, 2);
    await main.detail
      .getByRole("button", { name: "确认转给原主 Chat" })
      .click();
    await main.page.getByRole("link", { name: /打开原主 Chat/ }).waitFor();
    assert.equal(main.attempts[2].body.revision, updated.revision);
    assert.notEqual(
      main.attempts[2].receipt.message_id,
      main.attempts[0].receipt.message_id,
    );
    assert.equal((await chatNotes("main")).length, before.length + 1);
    console.log(
      "PASS browser recovered all three exact receipts across a real evidence revision and detail/category navigation",
    );

    // The first request never reached the Server. Only its real rejection on
    // replay permits a new preview and a separate confirmation.
    const rejected = await context.newPage();
    const attempts = [];
    await rejected.route(`${base}${target}/discussions`, async (route) => {
      const body = route.request().postDataJSON();
      if (!attempts.length) {
        attempts.push({ body, status: "not sent" });
        await route.abort();
        return;
      }
      const upstream = await route.fetch();
      attempts.push({ body, status: upstream.status() });
      await route.fulfill({ response: upstream });
    });
    await rejected.goto(`${root}?id=${scan}`);
    const open = rejected.getByRole("button", {
      name: "带入讨论",
      exact: true,
    });
    const confirm = rejected.getByRole("button", { name: "带入所选讨论" });
    const existingBefore = await chatNotes(existing);
    await open.click();
    await rejected.getByLabel("选择讨论").selectOption(existing);
    await confirm.click();
    await rejected
      .getByRole("alert")
      .getByText(/没有收到分享回执/)
      .waitFor();
    const latest = await mode("incomplete", updated);
    await rejected
      .getByText(`证据更新 · 版本 ${latest.revision}`, { exact: true })
      .waitFor({ timeout: 36_000 });
    assert.ok(await open.isDisabled());
    await rejected.getByRole("button", { name: "重试原提交" }).click();
    await rejected
      .getByRole("alert")
      .getByText(/原提交未保存/)
      .waitFor();
    assert.deepEqual(attempts, [
      {
        body: { revision: updated.revision, chat_id: existing },
        status: "not sent",
      },
      { body: { revision: updated.revision, chat_id: existing }, status: 409 },
    ]);
    assert.deepEqual(await chatNotes(existing), existingBefore);
    await open.click();
    await rejected.getByLabel("选择讨论").selectOption(existing);
    assert.equal(attempts.length, 2);
    await confirm.click();
    await rejected.getByRole("link", { name: /打开讨论/ }).waitFor();
    assert.deepEqual(attempts[2], {
      body: { revision: latest.revision, chat_id: existing },
      status: 200,
    });
    assert.equal((await chatNotes(existing)).length, existingBefore.length + 1);
    console.log(
      "PASS browser requires a real 409 before confirming a newer version for a request that never reached Server",
    );
  } finally {
    await context.close();
  }
}

async function shot(page, name) {
  await page.evaluate(() => document.fonts.ready);
  const dimensions = await page.evaluate(() => {
    const page = document.querySelector(".maint-page");
    return {
      viewport: document.documentElement.clientWidth,
      page: page.scrollWidth,
      document: document.documentElement.scrollWidth,
    };
  });
  assert.ok(
    dimensions.document <= dimensions.viewport,
    JSON.stringify(dimensions),
  );
  assert.ok(dimensions.page <= dimensions.viewport, JSON.stringify(dimensions));
  await page.screenshot({
    path: join(output, `${name}.png`),
    animations: "disabled",
  });
}
