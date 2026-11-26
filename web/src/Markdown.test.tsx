import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { Markdown, markdownLink } from "./Markdown";

describe("shared Markdown", () => {
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

  it("renders shared chat bodies with single newlines and block formatting", () => {
    const text = [
      "See [#81](https://example.test/issues/81) and [the reply](#message-81).",
      "Run `pnpm test`.",
      "",
      "- One",
      "- Two",
      "",
      "1. First",
      "2. Second",
      "",
      "```sh",
      "echo first",
      "echo second",
      "```",
      "",
      "Indented example:",
      "",
      "    echo indented",
    ].join("\n");
    const html = renderToStaticMarkup(<Markdown text={text} variant="chat" />);

    expect(html).toContain(
      '<a href="https://example.test/issues/81" rel="noreferrer">#81</a>',
    );
    expect(html).toContain('<a href="#message-81"');
    expect(html).toMatch(/<br\s*\/>\s*Run <code>pnpm test<\/code>/);
    expect(html).toMatch(/<ul>\s*<li>One<\/li>\s*<li>Two<\/li>\s*<\/ul>/);
    expect(html).toMatch(/<ol>\s*<li>First<\/li>\s*<li>Second<\/li>\s*<\/ol>/);
    expect(html).toContain('<code class="language-sh">echo first\necho second');
    expect(html).toContain("<code>echo indented\n</code>");
    expect(html.match(/<pre [^>]*class="markdown-scroll"/g)).toHaveLength(2);
    expect(renderToStaticMarkup(<Markdown text={text} />)).not.toContain("<br");
  });

  it.each([undefined, "chat"] as const)(
    "never emits active HTML, images or unsafe hrefs (variant: %s)",
    (variant) => {
      const html = renderToStaticMarkup(
        <Markdown
          variant={variant}
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
    },
  );

  it.each([
    " javascript:alert(1)",
    "data:text/html,hi",
    "java\nscript:fake",
    "//evil.test",
    "file:///example",
    "https://user:pass@example.test",
  ])("rejects URL %j", (url) => expect(markdownLink(url)).toBeUndefined());
});
