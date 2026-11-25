import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { Markdown, markdownLink } from "./Markdown";

describe("Issue Markdown", () => {
  it("renders headings, lists, tables, code and safe links", () => {
    const html = renderToStaticMarkup(
      <Markdown
        text={`# Plan

1. **Review** the [guide](https://example.test/guide).

| Source | Count |
| --- | --- |
| Runner | 1 |

\`\`\`sh
echo hello
\`\`\``}
      />,
    );
    expect(html).toContain("<h1>Plan</h1>");
    expect(html).toContain("<ol>");
    expect(html).toContain("<th>Count</th>");
    expect(html).toContain('<code class="language-sh">echo hello');
    expect(html).toContain('href="https://example.test/guide"');
    expect(html).toContain('class="markdown-scroll"');
  });

  it("never emits active HTML, images or unsafe hrefs", () => {
    const html = renderToStaticMarkup(
      <Markdown
        text={`<script>globalThis.injected = true</script>

<img src="https://example.test/raw.png" onerror="alert(1)" />

<iframe src="https://example.test/frame"></iframe>

![Preview](https://example.test/image.png)

![Inline](data:image/svg+xml,test)

[plain](javascript:alert%281%29) [escaped](jav&#97;script:alert%281%29) [data](data:text/html,hi) [relative](//example.test/unsafe) [valid](https://example.test/valid)`}
      />,
    );
    expect(html).not.toMatch(
      /<(script|img|iframe)\b|src=|onerror=|href="(javascript:|data:|\/\/)/,
    );
    expect(html).toContain("<span>Preview</span>");
    expect(html).toContain("<span>plain</span>");
    expect(html).toContain("<span>escaped</span>");
    expect(html).toContain('href="https://example.test/valid"');
  });

  it.each([
    " javascript:alert(1)",
    "data:text/html,hi",
    "java\nscript:fake",
    "//evil.test",
    "file:///example",
    "https://user:pass@example.test",
  ])("rejects URL %j", (url) => expect(markdownLink(url)).toBeUndefined());
});
