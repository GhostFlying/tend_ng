import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright";
import { collectPageErrors } from "./console-errors.mjs";

const url = process.env.TEND_URL;
const token = process.env.TEND_TOKEN;
const root = process.env.TEND_FIXTURE_ROOT;
const screenshots = process.env.TEND_SCREENSHOTS;
assert(url && token && root && screenshots);
const fixture = JSON.parse(
  await readFile(
    new URL("../../tests/fixtures/router_feedback.json", import.meta.url),
    "utf8",
  ),
);
const browser = await chromium.launch({ args: ["--no-proxy-server"] });
const errors = [];
try {
  await mkdir(screenshots, { recursive: true });
  const pages = await Promise.all(
    [
      { width: 1440, height: 1000 },
      { width: 390, height: 844 },
    ].map(async (viewport) => {
      const context = await browser.newContext({
        viewport,
        deviceScaleFactor: 1,
        colorScheme: "light",
        isMobile: viewport.width < 761,
        hasTouch: viewport.width < 761,
      });
      const page = await context.newPage();
      collectPageErrors(page, errors);
      await page.goto(url);
      await page.getByLabel("访问令牌").fill(token);
      await page.getByRole("button", { name: "连接", exact: true }).click();
      await page.getByLabel("发消息给 Tend").waitFor();
      assert.equal(await page.title(), "Tend");
      assert.equal(new URL(page.url()).pathname, "/");
      return { context, page, width: viewport.width };
    }),
  );
  await writeFile(join(root, "browser-ready"), "");
  for (const [name, request, text] of [
    ["dispatch", fixture.dispatch_request, fixture.dispatch_acceptance],
    ["progress", fixture.self_request, fixture.progress],
  ]) {
    await Promise.all(
      pages.map(async ({ context, page, width }) => {
        const user = page.locator(".message-user").filter({ hasText: request });
        const feedback = page
          .locator(".message")
          .filter({ has: page.getByText(text, { exact: true }) });
        await feedback.waitFor();
        await user.getByText("已收到", { exact: true }).waitFor();
        assert.equal(await feedback.count(), 1);
        assert.equal(await page.getByText(fixture.answer).count(), 0);
        assert.equal(await page.locator("vite-error-overlay").count(), 0);
        assert.equal(
          await page.evaluate(() => document.documentElement.scrollWidth),
          width,
        );
        await page.screenshot({
          path: join(screenshots, `router-${name}-${width}.png`),
        });
        if (name === "dispatch") {
          assert.equal(
            await page.getByRole("link", { name: /^查看 Issue #\d+$/ }).count(),
            1,
          );
          // Open an independent tab so the original timeline keeps polling.
          const detail = await context.newPage();
          collectPageErrors(detail, errors);
          await detail.goto(url);
          const link = detail
            .locator(".message")
            .filter({ has: detail.getByText(text, { exact: true }) })
            .getByRole("link", { name: /^查看 Issue #\d+$/ });
          const href = await link.getAttribute("href");
          assert.match(href, /^\/issues\/\d+$/);
          const number = href.split("/").at(-1);
          await link.click();
          await detail.waitForURL((target) => target.pathname === href);
          await detail
            .getByRole("heading", {
              name: `#${number} ${fixture.issue_title}`,
              exact: true,
            })
            .waitFor();
          await detail.close();
        }
      }),
    );
    assert.deepEqual(errors, []);
    await writeFile(join(root, `browser-${name}-seen`), "");
  }
  await Promise.all(
    pages.map(async ({ page }) => {
      await page.getByText(fixture.answer, { exact: true }).waitFor();
      assert.equal(
        await page.getByText(fixture.progress, { exact: true }).count(),
        1,
      );
    }),
  );
  assert.deepEqual(errors, []);
  console.log(
    "PASS live page polling, clickable Issue, and desktop/mobile feedback",
  );
} finally {
  await browser.close();
}
