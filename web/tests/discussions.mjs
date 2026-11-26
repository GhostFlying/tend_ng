import assert from "node:assert/strict";
import { join } from "node:path";
import { chromium } from "playwright";
import { collectPageErrors } from "./console-errors.mjs";

const {
  TEND_URL: base,
  TEND_TOKEN: token,
  TEND_SCREENSHOTS: output,
  TEND_REF_ISSUE: referenceIssue,
  TEND_TARGET_ISSUE: targetIssue,
} = process.env;
const open = "先聊聊演示项目的快速上手说明。";
const errors = [];
const browser = await chromium.launch();

async function visible(locator) {
  await locator.waitFor({ state: "visible" });
  return locator;
}

async function shoot(page, name) {
  const overflow = await page.evaluate(
    () =>
      document.documentElement.scrollWidth >
      document.documentElement.clientWidth,
  );
  assert.equal(overflow, false, `${name} has horizontal page overflow`);
  await page.screenshot({ path: join(output, `${name}.png`), scale: "css" });
}

try {
  for (const [name, viewport] of Object.entries({
    desktop: { width: 1440, height: 960 },
    mobile: { width: 390, height: 844 },
  })) {
    const checkedGoal =
      name === "desktop"
        ? "只整理一页上手说明，先交草案，不部署。"
        : "先交一页上手说明草案，只覆盖安装与首次运行。";
    const page = await browser.newPage({
      viewport,
      isMobile: name === "mobile",
      hasTouch: name === "mobile",
      locale: "zh-CN",
      colorScheme: "light",
    });
    collectPageErrors(page, errors);
    await page.goto(base);
    await page.getByLabel("访问令牌").fill(token);
    await page.getByRole("button", { name: "连接", exact: true }).click();
    const menu = page.getByRole("navigation", {
      name: name === "mobile" ? "手机导航" : "主导航",
    });
    await menu.getByRole("link", { name: "讨论", exact: true }).click();
    assert.equal(new URL(page.url()).pathname, "/discussions");
    assert.match(await page.title(), /Tend/i);
    await page
      .getByRole("main")
      .getByRole("button", { name: "新讨论" })
      .click();
    await page.waitForURL(/\/discussions\?chat=[a-zA-Z0-9_-]+$/);
    const discussion = page.url();
    await visible(page.getByRole("heading", { name: "可以直接开聊" }));
    const main = page.getByRole("main");
    await shoot(page, `discussion-${name}-empty`);
    await page.getByRole("button", { name: "改标题" }).click();
    const title =
      name === "desktop" ? "演示仓库 · 上手说明" : "手机验证 · 上手说明";
    await page.getByRole("textbox", { name: "讨论标题" }).fill(title);
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await visible(page.getByRole("heading", { name: title }));
    const composer = page.getByRole("textbox", { name: "在讨论中发消息" });
    await composer.fill(open);
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await visible(
      main.getByText("可以。先区分要讨论的想法与之后可能交给 Router 的工作。"),
    );
    await page.reload();
    assert.equal(page.url(), discussion);
    await visible(page.getByRole("heading", { name: title }));
    await visible(main.getByText(open, { exact: true }));

    await composer.fill("@exam");
    await page.getByRole("button", { name: "example 仓库" }).click();
    const references = page.getByRole("list", { name: "已附加资料" });
    await visible(references.getByRole("link", { name: "example" }));
    assert.equal(await composer.inputValue(), "");
    await composer.fill(`@#${referenceIssue}`);
    await page
      .getByRole("button", { name: new RegExp(`#${referenceIssue}.*Issue`) })
      .click();
    await visible(
      references.getByRole("link", { name: new RegExp(`#${referenceIssue}`) }),
    );
    await page.getByRole("button", { name: "附加资料" }).click();
    await page
      .getByRole("textbox", { name: "PR / MR 链接" })
      .fill("https://github.com/Demo/Quickstart/pull/7?from=fixture");
    await page.getByRole("button", { name: "添加链接" }).click();
    const pull = references.getByRole("link", {
      name: "PR #7 · demo/quickstart",
    });
    await visible(pull);
    assert.equal(
      await pull.getAttribute("href"),
      "https://github.com/demo/quickstart/pull/7",
    );
    await page.getByRole("button", { name: "附加资料" }).click();
    await composer.fill("看看我选中的资料，再建议第一版写什么。");
    await composer.press("Enter");
    await visible(
      main.getByText(
        "从登记仓库找到启动示例。从 Issue 了解到，第一版只需要覆盖安装和首次运行。",
      ),
    );
    await shoot(page, `discussion-${name}-references`);

    await page.getByRole("button", { name: "交给 Router" }).click();
    const dialog = page.getByRole("dialog", { name: "交给 Router" });
    await dialog.getByRole("button", { name: "生成转交草稿" }).click();
    await dialog
      .getByRole("textbox", { name: "目标", exact: true })
      .fill(checkedGoal);
    await dialog
      .getByRole("checkbox", { name: new RegExp(`#${referenceIssue}`) })
      .check();
    await dialog.getByRole("checkbox", { name: /PR #7/ }).check();
    const target = dialog.getByRole("textbox", { name: "搜索执行目标 Issue" });
    await target.fill(`#${targetIssue}`);
    await dialog
      .getByRole("button", { name: new RegExp(`#${targetIssue}`) })
      .click();
    await dialog
      .getByRole("button", { name: "发送到主 Chat", exact: true })
      .scrollIntoViewIfNeeded();
    await shoot(page, `discussion-${name}-draft`);
    await dialog
      .getByRole("button", { name: "发送到主 Chat", exact: true })
      .click();
    await visible(dialog.getByText("已发送到主 Chat", { exact: true }));
    await dialog
      .getByRole("link", { name: "查看消息和 Router 回执 ↗" })
      .click();
    await page.waitForURL(/\/#message-[a-zA-Z0-9_-]+$/);
    const id = new URL(page.url()).hash;
    const sent = page.locator(id);
    await visible(sent.getByText(checkedGoal, { exact: false }));
    await visible(
      page
        .getByRole("main")
        .getByText("Router 已收到核对后的转交，先确认下一步。")
        .last(),
    );
    if (name === "desktop") await shoot(page, "discussion-desktop-receipt");
    await sent.getByRole("link", { name: "回到原讨论 ↗" }).click();
    await page.waitForURL(discussion);
    await visible(page.getByRole("link", { name: "查看主 Chat 回执 ↗" }));
    if (name === "desktop") {
      await page.goto(`${base}/issues/${targetIssue}`);
      await page.getByRole("button", { name: "带入讨论" }).click();
      await page.waitForURL(/\/discussions\?chat=/);
      await visible(
        page
          .getByRole("region", { name: "当前讨论" })
          .getByRole("link", { name: new RegExp(`#${targetIssue}`) })
          .first(),
      );
      await page.goto(base);
      await page.getByRole("button", { name: "带入讨论" }).first().click();
      await page.waitForURL(/\/discussions\?chat=/);
      await visible(page.getByRole("link", { name: "主 Chat 消息" }));
    }
    await page.close();
  }
  assert.deepEqual(errors, []);
  console.log(
    "PASS discussion desktop and mobile Chromium, reference picker, draft, explicit send and bidirectional links",
  );
} finally {
  await browser.close();
}
