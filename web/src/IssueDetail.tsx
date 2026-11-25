import { useEffect, useState } from "react";
import {
  chatLink,
  download,
  issueLink,
  issueStageLabels,
  issueTitle,
  markdownPreviewLimit,
  pullLinkLabel,
  readArtifactMarkdown,
  readIssueOverview,
} from "./api";
import type { IssueDelivery, IssueOverviewData } from "./api";
import { Chat } from "./Chat";
import { Markdown } from "./Markdown";

function Objective({ text }: { text: string }) {
  const preview = Array.from(text.trim().replace(/\s+/g, " "));
  if (preview.length <= 140) return <p className="issue-objective">{text}</p>;
  return (
    <details className="issue-objective">
      <summary>
        <span className="issue-objective-preview">
          {preview.slice(0, 140).join("")}…
        </span>
        <span className="text-button issue-objective-expand">完整目标</span>
      </summary>
      <p>{text}</p>
    </details>
  );
}

function Delivery({ token, file }: { token: string; file: IssueDelivery }) {
  const [downloading, setDownloading] = useState(false);
  const [downloadError, setDownloadError] = useState("");
  const [opened, setOpened] = useState(false);
  const [body, setBody] = useState<{ text: string } | { error: string }>();
  const { id } = file;
  const html = /\.html?$/i.test(file.name);
  const markdown = /\.(md|markdown)$/i.test(file.name);
  const canPreview =
    markdown && file.available && file.bytes <= markdownPreviewLimit;

  useEffect(() => {
    if (!opened || !canPreview) return;
    const controller = new AbortController();
    void readArtifactMarkdown(token, id, controller.signal).then(
      (text) => {
        if (!controller.signal.aborted) setBody({ text });
      },
      (error: unknown) => {
        if (!controller.signal.aborted)
          setBody({
            error: error instanceof Error ? error.message : "暂时无法预览",
          });
      },
    );
    return () => controller.abort();
    // The report is polled. An immutable original doesn't need re-reading
    // when only the enclosing report object has changed.
  }, [token, id, opened, canPreview]);

  async function saveFile() {
    setDownloadError("");
    setDownloading(true);
    try {
      await download(token, file);
    } catch (error) {
      setDownloadError(
        error instanceof Error ? error.message : "暂时无法下载原件",
      );
    } finally {
      setDownloading(false);
    }
  }

  const source = file.source;
  return (
    <li className="delivery-file">
      <div className="delivery-file-heading">
        <strong>{file.name}</strong>
        <span className="delivery-kind">
          {html ? "HTML 原件" : markdown ? "Markdown" : "附件"}
        </span>
      </div>
      <div className="delivery-file-actions">
        <button
          className="download"
          type="button"
          disabled={!file.available || downloading}
          onClick={() => void saveFile()}
        >
          {!file.available
            ? "原件正在同步"
            : downloading
              ? "正在下载…"
              : html
                ? "打开原件（下载 HTML）"
                : "下载原件"}
        </button>
        {source ? (
          <a href={issueLink(source.issue.id, source.issue.short_number)}>
            来源 {issueTitle(source.issue.title, source.issue.short_number)}
          </a>
        ) : (
          file.issue_title
        )}
      </div>
      {source && (
        <details className="delivery-identity">
          <summary>交付来源</summary>
          <dl>
            <div>
              <dt>Task</dt>
              <dd>
                <code>{source.task_id}</code>
              </dd>
            </div>
            <div>
              <dt>交付 ID</dt>
              <dd>
                <code>{file.id}</code>
              </dd>
            </div>
          </dl>
        </details>
      )}
      {downloadError && (
        <p className="error" role="alert">
          {downloadError}
        </p>
      )}
      {canPreview && (
        <details
          className="delivery-preview"
          onToggle={(event) => {
            if (event.currentTarget.open) setOpened(true);
          }}
        >
          <summary>阅读 Markdown 原件</summary>
          {!body ? (
            <p role="status">正在读取原件…</p>
          ) : "error" in body ? (
            <p className="error" role="alert">
              {body.error}。可下载原件。
            </p>
          ) : (
            <Markdown text={body.text} />
          )}
        </details>
      )}
      {markdown && file.bytes > markdownPreviewLimit && (
        <p className="detail-muted">文件超出页内预览大小，可下载原件阅读。</p>
      )}
    </li>
  );
}

function IssueSummary({
  token,
  data,
}: {
  token: string;
  data: IssueOverviewData;
}) {
  const reports = data.reports ?? [];
  const decisions = data.decisions ?? [];
  return (
    <>
      <header className="detail-heading">
        <div className="detail-kicker">
          <span>ISSUE {data.short_number ? `#${data.short_number}` : ""}</span>
          <span className="detail-stage" data-stage={data.stage}>
            {issueStageLabels[data.stage]}
          </span>
        </div>
        <h1>{issueTitle(data.title, data.short_number)}</h1>
        <nav className="detail-jumps" aria-label="Issue 入口">
          <a href={chatLink(data.chat_id, data.source_message_id)}>
            查看原消息
          </a>
          <a href="#issue-discussion">跳到 Issue 对话</a>
          <a href="#issue-properties">工作属性</a>
        </nav>
        {data.objective.trim() && <Objective text={data.objective} />}
      </header>
      <section className="detail-section" aria-labelledby="issue-reports">
        <div className="detail-section-heading">
          <h2 id="issue-reports">报告与评审材料</h2>
          {reports.length > 0 && <span>{reports.length} 份已回传报告</span>}
        </div>
        {reports.length === 0 ? (
          <p className="detail-empty">
            没有可追溯到原报告消息的交付。过程留言和其他工作交付仍可在下方查看。
          </p>
        ) : (
          reports.map((report) => (
            <article
              key={report.message_id}
              id={`message-${report.message_id}`}
              className="delivery-report"
            >
              <div className="delivery-report-heading">
                <span>来自原 Chat 的交付报告</span>
                <a href={chatLink(data.chat_id, report.message_id)}>
                  查看原报告消息
                </a>
              </div>
              <Markdown text={report.text} />
              <ul className="delivery-files" aria-label="报告附件">
                {report.artifacts.map((file) => (
                  <Delivery key={file.id} token={token} file={file} />
                ))}
              </ul>
            </article>
          ))
        )}
      </section>
      {decisions.length > 0 && (
        <section className="detail-section" aria-labelledby="issue-decisions">
          <div className="detail-section-heading">
            <h2 id="issue-decisions">待你决定</h2>
          </div>
          <ul className="issue-decisions">
            {decisions.map((decision) => (
              <li key={decision.message_id}>
                <Markdown text={decision.text} />
                <a href={chatLink(data.chat_id, decision.message_id)}>
                  在原 Chat 处理
                </a>
              </li>
            ))}
          </ul>
        </section>
      )}
    </>
  );
}

function WorkDeliveries({
  token,
  data,
}: {
  token: string;
  data: IssueOverviewData;
}) {
  const reported = new Set(
    data.reports?.flatMap((report) => report.artifacts.map(({ id }) => id)),
  );
  const files = data.artifacts.filter((file) => !reported.has(file.id));
  if (!files.length) return null;
  return (
    <details className="work-deliveries">
      <summary>
        其他工作交付 <span>{files.length}</span>
      </summary>
      <p className="detail-muted">
        未作为上方报告附件展示，可能包含过程稿或其他交付。
      </p>
      <ul className="delivery-files" aria-label="其他工作交付">
        {files.map((file) => (
          <Delivery key={file.id} token={token} file={file} />
        ))}
      </ul>
    </details>
  );
}

function IssueProperties({ data }: { data: IssueOverviewData }) {
  return (
    <div className="detail-properties">
      <h2>工作属性</h2>
      <dl>
        <div>
          <dt>阶段</dt>
          <dd>{issueStageLabels[data.stage]}</dd>
        </div>
        {data.repo_ids.length > 0 && (
          <div>
            <dt>仓库</dt>
            <dd>{data.repo_ids.join(" · ")}</dd>
          </div>
        )}
        <div>
          <dt>来源</dt>
          <dd>
            <a href={chatLink(data.chat_id)}>原 Chat</a>
          </dd>
        </div>
      </dl>
      {data.pulls.length > 0 && (
        <section>
          <h3>关联 PR</h3>
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
      {data.children.length > 0 && (
        <section>
          <h3>子 Issue</h3>
          <ul>
            {data.children.map((child) => (
              <li key={child.id}>
                <a href={issueLink(child.id, child.short_number)}>
                  {issueTitle(child.title, child.short_number)}
                </a>
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}

export function IssueDetail({
  token,
  issue,
  chatId,
  onReachable,
}: {
  token: string;
  issue: string;
  chatId: string;
  onReachable: (online: boolean) => void;
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
        }
      } catch (error) {
        if (!controller.signal.aborted) {
          setError(
            error instanceof Error ? error.message : "暂时无法读取 Issue 详情",
          );
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
  }, [token, issue]);

  return (
    <Chat
      token={token}
      issue={issue}
      chatId={chatId}
      onReachable={onReachable}
      detail={{
        overviewLoaded: data !== null,
        header: (
          <>
            {error && (
              <p className="error" role="alert">
                {error}
              </p>
            )}
            {data ? (
              <IssueSummary token={token} data={data} />
            ) : (
              !error && <p role="status">正在读取 Issue 详情…</p>
            )}
          </>
        ),
        work: data && <WorkDeliveries token={token} data={data} />,
        properties: data && <IssueProperties data={data} />,
      }}
    />
  );
}
