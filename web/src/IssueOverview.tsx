import { useEffect, useState } from "react";
import {
  chatLink,
  download,
  issueLink,
  issueOverviewLink,
  issueStageLabels,
  issueTitle,
  pullLinkLabel,
  readIssueOverview,
} from "./api";
import type { Artifact, IssueOverviewData } from "./api";

function Objective({ text }: { text: string }) {
  const preview = Array.from(text.trim().replace(/\s+/g, " "));
  if (preview.length <= 120) return <p className="issue-objective">{text}</p>;
  return (
    <details className="issue-objective">
      <summary>
        <span className="issue-objective-preview">
          {preview.slice(0, 120).join("")}…
        </span>
        <span className="text-button issue-objective-expand">完整目标</span>
      </summary>
      <p>{text}</p>
    </details>
  );
}

function Deliveries({
  token,
  files,
}: {
  token: string;
  files: IssueOverviewData["artifacts"];
}) {
  const [downloading, setDownloading] = useState<string | null>(null);
  const [error, setError] = useState("");

  async function saveFile(file: Artifact) {
    setError("");
    setDownloading(file.id);
    try {
      await download(token, file);
    } catch (error) {
      setError(error instanceof Error ? error.message : "暂时无法下载交付");
    } finally {
      setDownloading(null);
    }
  }

  return (
    <section>
      <h2>交付文件</h2>
      <ul>
        {files.map((file) => (
          <li key={file.id}>
            <button
              className="download"
              type="button"
              disabled={!file.available || downloading !== null}
              onClick={() => void saveFile(file)}
            >
              {!file.available
                ? `${file.name} · 正在同步`
                : downloading === file.id
                  ? `正在下载 ${file.name}…`
                  : `下载 ${file.name}`}
            </button>
            {file.issue_title && (
              <span className="issue-file-source"> · {file.issue_title}</span>
            )}
          </li>
        ))}
      </ul>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}

export function IssueOverview({
  token,
  issue,
  onReachable,
}: {
  token: string;
  issue: string;
  onReachable: (value: boolean) => void;
}) {
  const [data, setData] = useState<IssueOverviewData | null>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    const controller = new AbortController();
    let timer: number;
    async function update() {
      try {
        const result = await readIssueOverview(token, issue, controller.signal);
        if (!controller.signal.aborted) {
          setData(result);
          setError("");
          onReachable(true);
        }
      } catch (error) {
        if (!controller.signal.aborted) {
          setError(
            error instanceof Error ? error.message : "暂时无法读取 Issue 概览",
          );
          onReachable(false);
        }
      }
      if (!controller.signal.aborted)
        timer = window.setTimeout(() => void update(), 5000);
    }
    void update();
    return () => {
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [token, issue, onReachable]);

  return (
    <main className="timeline">
      <div className="lane issue-overview">
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
        {!data && !error && <p role="status">正在读取 Issue 概览…</p>}
        {data && (
          <>
            <h1>{issueTitle(data.title, data.short_number)}</h1>
            <p className="issue-overview-meta">
              {[issueStageLabels[data.stage], ...data.repo_ids].join(" · ")}
            </p>
            <nav className="issue-overview-links" aria-label="Issue 入口">
              <a href={issueLink(data.id, data.short_number)}>
                进入 Issue 对话
              </a>
              <a href={chatLink(data.chat_id, data.source_message_id)}>
                查看原消息
              </a>
            </nav>
            {data.objective.trim() && (
              <section>
                <h2>工作目标</h2>
                <Objective text={data.objective} />
              </section>
            )}
            {data.children.length > 0 && (
              <section>
                <h2>子 Issue</h2>
                <ul>
                  {data.children.map((child) => (
                    <li key={child.id}>
                      <a href={issueOverviewLink(child.id, child.short_number)}>
                        {issueTitle(child.title, child.short_number)}
                      </a>
                    </li>
                  ))}
                </ul>
              </section>
            )}
            {data.pulls.length > 0 && (
              <section>
                <h2>关联 PR</h2>
                <ul>
                  {data.pulls.map((pull) => (
                    <li key={pull.url}>
                      <a href={pull.url} target="_blank" rel="noreferrer">
                        {pullLinkLabel(pull)}
                      </a>
                    </li>
                  ))}
                </ul>
              </section>
            )}
            {data.artifacts.length > 0 && (
              <Deliveries token={token} files={data.artifacts} />
            )}
          </>
        )}
      </div>
    </main>
  );
}
