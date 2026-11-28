import assert from "node:assert/strict";
import { join } from "node:path";
import { chromium } from "playwright";
import { isAnonymousSessionProbe } from "./console-errors.mjs";

const {
  TEND_URL: base,
  TEND_TOKEN: token,
  TEND_SCREENSHOTS: output,
  TEND_FEISHU_CARD: id,
  TEND_FEISHU_ALIAS: alias,
  TEND_FEISHU_PRIOR_CHAT: priorChat,
  TEND_FEISHU_PRIOR_MESSAGE: priorMessage,
  TEND_FEISHU_CACHED_DETAIL: cachedDetail,
  TEND_FEISHU_RELATIONSHIP: relationship,
  TEND_FEISHU_ROLES: roles,
  TEND_FEISHU_TOPIC: topic,
  TEND_FEISHU_EXPANDED: expanded,
} = process.env;
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
    page.on("console", (message) => {
      if (
        message.type() === "error" &&
        !isAnonymousSessionProbe(message) &&
        !message.location().url.endsWith("/favicon.ico")
      ) {
        errors.push(message.text());
      }
    });
    await page.goto(base);
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    await page
      .getByRole("navigation", { name: mobile ? "手机导航" : "主导航" })
      .getByRole("link", { name: "飞书建议" })
      .click();
    await page.waitForURL(`${base}/feishu-suggestions`);
    assert.equal(await page.title(), "Tend");
    await page
      .getByRole("heading", { name: "飞书建议", exact: true })
      .waitFor();
    const list = page.getByRole("region", { name: "建议列表" });
    const detail = page.getByRole("region", { name: "建议详情" });
    if (expanded) {
      await list.getByRole("button", { name: "已解决" }).click();
      await list
        .locator(`a[href="/feishu-suggestions?id=${expanded}"]`)
        .click();
      await page.waitForURL(`${base}/feishu-suggestions?id=${expanded}`);
      await detail
        .getByText("展开同秒的回复后，签字项齐全，已通过验收。", {
          exact: false,
        })
        .waitFor();
      await detail.getByRole("button", { name: "进展依据 1" }).click();
      const source = detail.locator(".fs-message:focus");
      assert.match(await source.innerText(), /验收凭据：签字项齐全。/);
      const jump = source.getByRole("link", { name: /飞书原文/ });
      const href = new URL(await jump.getAttribute("href"));
      assert.equal(href.pathname, "/client/thread/open");
      assert.equal(href.searchParams.get("openthreadid"), "omt_expandpos");
      assert.equal(href.searchParams.get("thread_position"), "9");
      await jump.scrollIntoViewIfNeeded();
      await shot(page, `feishu-expanded-source-${name}`);
      await context.close();
      continue;
    }
    if (topic) {
      await list.locator(`a[href="/feishu-suggestions?id=${topic}"]`).click();
      await page.waitForURL(`${base}/feishu-suggestions?id=${topic}`);
      await detail
        .getByText("此话题还未看到验收回复。", { exact: false })
        .waitFor();
      await detail.getByText("在已查范围内未看到相关回复。").waitFor();
      assert.doesNotMatch(await detail.innerText(), /旧群聊验收/);
      await detail.getByRole("button", { name: "请求依据 2" }).click();
      await detail
        .locator(".fs-message:focus")
        .getByText("验收前先核对本次灰度版本。")
        .waitFor();
      await shot(page, `feishu-topic-scope-${name}`);
      await context.close();
      continue;
    }
    if (roles) {
      await list.getByRole("link", { name: /顾宁请你负责审批/ }).click();
      await page.waitForURL(`${base}/feishu-suggestions?id=${roles}`);
      await detail.getByText("当前轮到你审批。", { exact: false }).waitFor();
      await detail.getByRole("button", { name: "请求依据 1" }).click();
      const source = detail.locator(".fs-message:focus");
      await source
        .getByText("@林星 负责发版，@林星 负责审批。", { exact: true })
        .waitFor();
      assert.doesNotMatch(
        await source.innerText(),
        /ou_fixture_owner|ou_same_name/,
      );
      await shot(page, `feishu-role-binding-${name}`);
      await context.close();
      continue;
    }
    if (relationship) {
      await list.getByRole("link", { name: /确认周二的演示时间/ }).waitFor();
      assert.doesNotMatch(await list.innerText(), /新手册已经发出/);
      if (mobile) await shot(page, "feishu-relationship-mobile-list");
      await list.getByRole("button", { name: "仅知会" }).click();
      await list.getByRole("link", { name: /新手册已经发出/ }).waitFor();
      assert.match(await list.innerText(), /当前未见需要你操作/);
      await list.getByRole("button", { name: "已忽略" }).click();
      await list.getByRole("link", { name: /周四版对照表/ }).click();
      await page.waitForURL(`${base}/feishu-suggestions?id=${relationship}`);
      await detail
        .getByText("你已确认用毫秒，王文负责改表。", { exact: false })
        .waitFor();
      await detail
        .getByText("未能核实 2 条直接回复对象或引用的原文。")
        .waitFor();
      await detail.getByRole("button", { name: "进展依据 1" }).click();
      const selectedSource = detail.locator(".fs-message:focus");
      assert.match(await selectedSource.innerText(), /小林（你）.*单位用毫秒/s);
      assert.match(
        await selectedSource
          .getByRole("link", { name: /飞书原文/ })
          .getAttribute("href"),
        /^https:\/\/applink\.feishu\.cn\//,
      );
      await page.locator(".fs-page").evaluate((pane) => pane.scrollTo(0, 0));
      await shot(page, `feishu-relationship-${name}`);
      await context.close();
      continue;
    }
    await list.getByRole("link", { name: /确认 Android 灰度窗口/ }).waitFor();
    if (mobile) await shot(page, "feishu-mobile-list");
    await list.getByRole("button", { name: "已解决" }).click();
    assert.match(await list.innerText(), /本地启动文档已补齐/);
    assert.doesNotMatch(await list.innerText(), /确认 Android 灰度窗口/);
    await list.getByRole("button", { name: "待查看" }).click();
    await list.getByRole("link", { name: /确认 Android 灰度窗口/ }).click();
    await page.waitForURL(`${base}/feishu-suggestions?id=${id}`);
    await detail
      .getByRole("heading", { name: "确认 Android 灰度窗口与回滚阈值" })
      .waitFor();
    assert.match(await detail.innerText(), /Android 12 和 15/);
    assert.match(await detail.innerText(), /来源与后续/);
    await detail.getByRole("button", { name: "请求依据 1" }).click();
    await detail
      .locator(".fs-message:focus")
      .getByText(/今天能确认 Android 灰度窗口吗/)
      .waitFor();
    await detail.getByRole("button", { name: "请求依据 2" }).click();
    await detail
      .locator(".fs-message:focus")
      .getByText(/Android 发布排期/)
      .waitFor();
    assert.match(
      await detail.locator(".fs-message:focus").innerText(),
      /回复对象/,
    );
    assert.doesNotMatch(await detail.innerText(), /未能核实 .*直接回复对象/);
    await shot(page, `feishu-reference-${name}`);
    await page.locator(".fs-page").evaluate((pane) => pane.scrollTo(0, 0));
    assert.match(
      await detail
        .getByRole("link", { name: /飞书原文/ })
        .first()
        .getAttribute("href"),
      /^https:\/\/applink\.feishu\.cn\/client\/(chat|thread)\/open/,
    );
    assert.equal(
      await detail.getByRole("button", { name: "转交主 Chat" }).count(),
      1,
    );
    assert.equal(
      await detail.getByRole("button", { name: "带入讨论" }).count(),
      1,
    );
    await shot(page, mobile ? "feishu-mobile-detail" : "feishu-desktop");
    await checkScanAndAssessment(page, detail, mobile);
    await detail.getByRole("button", { name: "带入讨论" }).click();
    await detail
      .getByLabel("选择已有讨论")
      .selectOption({ label: "已有的验收讨论" });
    await detail.getByRole("button", { name: "附到所选讨论" }).click();
    await page.waitForURL(/\/discussions\?chat=/);
    await page.getByRole("heading", { name: "已有的验收讨论" }).waitFor();
    await page.goBack();
    await detail.getByRole("button", { name: "忽略", exact: true }).click();
    await detail.getByRole("button", { name: "撤销忽略", exact: true }).click();
    await detail.getByRole("button", { name: "忽略", exact: true }).waitFor();
    if (mobile) {
      await detail.getByRole("link", { name: /返回建议列表/ }).click();
      await list.getByRole("link", { name: /确认 Android 灰度窗口/ }).waitFor();
      assert.ok(!(await detail.isVisible()));
      await list.getByRole("button", { name: "已解决" }).click();
      await list.getByRole("link", { name: /本地启动文档已补齐/ }).click();
      await detail.getByRole("button", { name: "转交主 Chat" }).click();
      const receipt = detail.getByRole("link", { name: /查看主 Chat 转交/ });
      await receipt.waitFor();
      const href = await receipt.getAttribute("href");
      assert.match(href, /^\/#message-[a-zA-Z0-9_-]+$/);
      await receipt.click();
      await page.waitForURL(base + href);
      await page.getByText(/飞书文档项已关闭，仅供主 Chat 参考/).waitFor();
      if (alias) {
        await page.goto(`${base}/feishu-suggestions?id=${alias}`);
        await detail
          .getByRole("heading", { name: "确认周五发布负责人" })
          .waitFor();
        assert.equal(
          await detail
            .getByRole("link", { name: /合并前材料.*主 Chat/ })
            .getAttribute("href"),
          `/#message-${priorMessage}`,
        );
        assert.equal(
          await detail
            .getByRole("link", { name: /继续讨论.*合并前材料/ })
            .getAttribute("href"),
          `/discussions?chat=${priorChat}`,
        );
        if (cachedDetail) {
          const stale = JSON.parse(cachedDetail);
          stale.evidence.assessment.headline = "分页已缓存的临时建议";
          if (stale.evidence.assessment.card)
            stale.evidence.assessment.card.request = "分页已缓存的临时建议";
          // Replay only the stale page. The detail request still goes to the
          // real Server and resolves its old ID to the current suggestion.
          await page.route(
            /\/api\/v1\/feishu-suggestions(?:\?.*)?$/,
            async (route) => {
              if (new URL(route.request().url()).searchParams.has("before")) {
                await route.fulfill({
                  json: { suggestions: [stale], next_before: null },
                });
                return;
              }
              const response = await route.fetch();
              const first = await response.json();
              first.suggestions = first.suggestions.filter(
                (item) => !item.aliases.includes(alias),
              );
              first.next_before = "cached-fixture";
              await route.fulfill({ json: first });
            },
          );
          await page.goto(`${base}/feishu-suggestions`);
          await list.getByRole("button", { name: "更多建议" }).click();
          await list
            .getByRole("link", { name: /分页已缓存的临时建议/ })
            .click();
          await detail
            .getByRole("heading", { name: "确认周五发布负责人" })
            .waitFor();
          assert.equal(
            await detail
              .getByRole("heading", { name: "分页已缓存的临时建议" })
              .count(),
            0,
          );
          await detail.getByRole("link", { name: /返回建议列表/ }).click();
          await list
            .getByRole("link", { name: /确认周五发布负责人/ })
            .waitFor();
          assert.equal(
            await list
              .getByRole("link", { name: /分页已缓存的临时建议/ })
              .count(),
            0,
          );
        }
      }
    }
    await context.close();
  }
} finally {
  await browser.close();
}
assert.deepEqual(errors, []);
console.log(
  "PASS Chromium desktop and 390px scan alignment, wrapped status, assessment copy, navigation, real Web attach and ignore restoration",
);

async function checkScanAndAssessment(page, detail, mobile) {
  const scan = page.locator(".fs-scan");
  await scan.getByText(/^上次成功扫描 /).waitFor();
  const button = scan.getByRole("button", { name: "扫描新消息" });
  const buttonWidth = (await assertScanRow(scan, button)).width;
  const feedUrl = "**/api/v1/feishu-suggestions";
  let state;
  // Keep the real fixture response except for the status fields under test.
  // This makes each branch stable even if the fixture assessor runs another job.
  await page.route(feedUrl, async (route) => {
    const response = await route.fetch();
    const feed = await response.json();
    const card = feed.suggestions.find((item) => item.evidence.id === id);
    card.evidence.assessment = null;
    card.assessment_status = state === "failed" ? "failed" : "pending";
    if (state === "pending") {
      card.evidence.context_status = "pending";
      card.evidence.replies = [];
      feed.scan.phase = "failed";
      feed.scan.gap = "identity";
    }
    if (state === "unconfigured") feed.assessor = null;
    await route.fulfill({ response, json: feed });
  });
  try {
    for (const [variant, copy] of [
      ["pending", "已收录，等待 Agent 分析。现在可以查看飞书原文。"],
      ["failed", "分析未成功，仍可查看飞书原文。"],
      ["unconfigured", "未配置分析 Agent，可查看飞书原文。"],
    ]) {
      state = variant;
      await page.reload();
      await detail.getByText(copy, { exact: true }).waitFor();
      await detail
        .getByRole("link", { name: /飞书原文/ })
        .first()
        .waitFor();
      if (state !== "pending") continue;
      const gap = scan.getByText(/本机飞书身份不可用/);
      await gap.waitFor();
      if (mobile) {
        assert.ok(
          await gap.evaluate((element) => {
            const range = document.createRange();
            range.selectNodeContents(element);
            return range.getClientRects().length > 1;
          }),
          "the scan gap should wrap at 390px",
        );
      }
      assert.equal((await assertScanRow(scan, button)).width, buttonWidth);
      await button.click({ trial: true });
      await shot(page, mobile ? "feishu-scan-390px" : "feishu-scan-desktop");
    }
  } finally {
    await page.unroute(feedUrl);
  }
  await page.reload();
  await detail
    .getByRole("heading", { name: "确认 Android 灰度窗口与回滚阈值" })
    .waitFor();
}

async function assertScanRow(scan, button) {
  const status = await scan.locator(".fs-scan-copy").boundingBox();
  const action = await button.boundingBox();
  const row = await scan.boundingBox();
  assert.ok(
    status && action && row,
    "scan status and button should be visible",
  );
  const center = ({ y, height }) => y + height / 2;
  assert.ok(
    Math.abs(center(status) - center(action)) <= 1,
    "scan row should be vertically centered",
  );
  assert.ok(
    status.x + status.width < action.x,
    "scan status and button should not overlap",
  );
  assert.ok(
    action.x + action.width <= row.x + row.width + 1,
    "scan button should fit inside its row",
  );
  return action;
}

async function shot(page, name) {
  assert.ok(
    await page.evaluate(
      () =>
        document.documentElement.scrollWidth <=
        document.documentElement.clientWidth,
    ),
    `${name} horizontal overflow`,
  );
  assert.ok(
    await page
      .locator(".fs-page")
      .evaluate((pane) => pane.scrollWidth <= pane.clientWidth),
    `${name} horizontal overflow inside the Feishu page`,
  );
  await page.screenshot({ path: join(output, `${name}.png`), scale: "css" });
}
