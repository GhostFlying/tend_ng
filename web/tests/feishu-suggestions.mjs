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
    await page
      .getByRole("heading", { name: "飞书建议", exact: true })
      .waitFor();
    const list = page.getByRole("region", { name: "建议列表" });
    const detail = page.getByRole("region", { name: "建议详情" });
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
    assert.match(await detail.innerText(), /上次成功扫描|来源与后续/);
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
      }
    }
    await context.close();
  }
} finally {
  await browser.close();
}
assert.deepEqual(errors, []);
console.log(
  "PASS Chromium desktop and mobile rendering and navigation, both user actions visible, real Web attach and ignore restoration",
);

async function shot(page, name) {
  assert.ok(
    await page.evaluate(
      () =>
        document.documentElement.scrollWidth <=
        document.documentElement.clientWidth,
    ),
    `${name} horizontal overflow`,
  );
  await page.screenshot({ path: join(output, `${name}.png`), scale: "css" });
}
