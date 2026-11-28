import ReactMarkdown from "react-markdown";
import remarkBreaks from "remark-breaks";
import remarkGfm from "remark-gfm";

export function markdownLink(value: string) {
  if (/^#[a-z\d_-]+$/i.test(value)) return value;
  if (!/^https?:\/\//i.test(value)) return undefined;
  try {
    const url = new URL(value);
    return url.username || url.password ? undefined : url.href;
  } catch {
    return undefined;
  }
}

export function Markdown({
  text,
  variant,
  maintenanceLinks = false,
}: {
  text: string;
  variant?: "chat";
  maintenanceLinks?: boolean;
}) {
  const chat = variant === "chat";
  return (
    <div className={chat ? "markdown chat-message-body" : "markdown"}>
      <ReactMarkdown
        skipHtml
        remarkPlugins={chat ? [remarkGfm, remarkBreaks] : [remarkGfm]}
        urlTransform={(url, key) => {
          if (key !== "href") return undefined;
          if (
            maintenanceLinks &&
            /^\/(?:feishu-suggestions#scan-status|maintenance-suggestions\?id=[\w-]+|(?:issues\/[1-9]\d*)?(?:#message-[\w-]+)|issues\/[1-9]\d*)$/.test(
              url,
            )
          )
            return url;
          return markdownLink(url);
        }}
        components={{
          a: ({ href, children }) =>
            href ? (
              <a href={href} rel="noreferrer">
                {children}
              </a>
            ) : (
              <span>{children}</span>
            ),
          img: ({ alt }) => (alt ? <span>{alt}</span> : null),
          table: ({ children }) => (
            <div
              className="markdown-scroll"
              role="region"
              aria-label="表格"
              tabIndex={0}
            >
              <table>{children}</table>
            </div>
          ),
          pre: ({ children }) => (
            <pre className="markdown-scroll" aria-label="代码块" tabIndex={0}>
              {children}
            </pre>
          ),
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
}
