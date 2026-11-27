import { useEffect, useState } from "react";
import {
  chatLink,
  download,
  issueLink,
  issueStageLabels,
  issueTitle,
  markdownPreviewLimit,
  readArtifactMarkdown,
  readIssueOverview,
} from "./api";
import { PullAssociation, pullSectionTitle } from "./PullAssociation";
import type {
  IssueDelivery,
  IssueDeliveryReport,
  IssueOverviewData,
  IssueProposal,
  ProposalVersion,
} from "./api";
import { Chat } from "./Chat";
import { IssueAttention } from "./IssueAttention";
import { IssueTokenUsage } from "./IssueTokenUsage";
import { Markdown } from "./Markdown";
import { DecisionTime, ProposalApproval } from "./ProposalApproval";

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

function Delivery({
  file,
  proposal,
}: {
  file: IssueDelivery;
  proposal?: boolean;
}) {
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
    void readArtifactMarkdown(id, controller.signal).then(
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
  }, [id, opened, canPreview]);

  async function saveFile() {
    setDownloadError("");
    setDownloading(true);
    try {
      await download(file);
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
        {proposal && (
          <span className="detail-muted proposal-size">
            {file.bytes < 1024
              ? `${file.bytes} B`
              : `${(file.bytes / 1024).toFixed(1)} KiB`}
          </span>
        )}
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
          <summary>{proposal ? "展开报告原文" : "阅读 Markdown 原件"}</summary>
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

function CandidateReport({
  candidate,
  chat,
}: {
  candidate: ProposalVersion;
  chat: string;
}) {
  return (
    <article
      className="proposal-report"
      id={candidate.message_id ? `message-${candidate.message_id}` : undefined}
    >
      <div className="proposal-report-title">
        <h3>{candidate.title}</h3>
        <span className="proposal-version">v{candidate.version}</span>
      </div>
      <ul className="proposal-highlights">
        {candidate.highlights.map((highlight, index) => (
          <li key={index}>{highlight}</li>
        ))}
      </ul>
      <ul className="delivery-files" aria-label="报告附件">
        <Delivery
          key={candidate.artifact.id}
          file={candidate.artifact}
          proposal
        />
        {candidate.other_artifacts.map((file) => (
          <Delivery key={file.id} file={file} />
        ))}
      </ul>
      {candidate.message_id && (
        <a
          className="proposal-source"
          href={chatLink(chat, candidate.message_id)}
        >
          查看原报告消息
        </a>
      )}
    </article>
  );
}

function ProposalReport({
  proposal,
  chat,
  selected,
  onRefresh,
}: {
  proposal: IssueProposal;
  chat: string;
  selected: boolean;
  onRefresh: () => void;
}) {
  const [current, ...history] = proposal.versions;
  if (!current) return null;
  return (
    <section className="detail-section proposal-section">
      <div className="detail-section-heading">
        <h2>{selected ? "当前报告" : "独立方案报告"}</h2>
        <span>
          <DecisionTime time={current.updated_at} /> 更新
        </span>
      </div>
      <CandidateReport key={current.version} candidate={current} chat={chat} />
      {!selected && proposal.status.decision && (
        <ProposalApproval
          chat={chat}
          attention={proposal.attention_id}
          status={proposal.status}
          inIssue
          onRefresh={onRefresh}
        />
      )}
      {history.length > 0 && (
        <details className="proposal-history">
          <summary>版本历史 ({history.length})</summary>
          {history.map((candidate) => (
            <details key={candidate.version}>
              <summary>
                {candidate.title} · v{candidate.version} ·{" "}
                <DecisionTime time={candidate.updated_at} />
              </summary>
              <CandidateReport candidate={candidate} chat={chat} />
            </details>
          ))}
        </details>
      )}
    </section>
  );
}

function ReportArticle({
  report,
  chat,
}: {
  report: IssueDeliveryReport;
  chat: string;
}) {
  const history = report.history ?? [];
  return (
    <article id={`message-${report.message_id}`} className="delivery-report">
      <div className="delivery-report-heading">
        <span>来自原 Chat 的交付报告</span>
        <a href={chatLink(chat, report.message_id)}>查看原报告消息</a>
      </div>
      <Markdown text={report.text} />
      <ul className="delivery-files" aria-label="报告附件">
        {report.artifacts.map((file) => (
          <Delivery key={file.id} file={file} />
        ))}
      </ul>
      {history.length > 0 && (
        <details className="delivery-history">
          <summary>版本历史 ({history.length})</summary>
          {history.map((version) => (
            <ReportArticle
              key={version.message_id}
              report={version}
              chat={chat}
            />
          ))}
        </details>
      )}
    </article>
  );
}

function DeliveredReports({
  reports,
  chat,
}: {
  reports: IssueDeliveryReport[];
  chat: string;
}) {
  if (!reports.length) {
    return (
      <p className="detail-empty">
        没有可追溯到原报告消息的交付。过程留言和其他工作交付仍可在下方查看。
      </p>
    );
  }
  return reports.map((report) => (
    <ReportArticle key={report.message_id} report={report} chat={chat} />
  ));
}

function IssueSummary({
  data,
  onRefresh,
}: {
  data: IssueOverviewData;
  onRefresh: () => void;
}) {
  const reports = data.reports ?? [];
  const decisions = data.decisions ?? [];
  const proposals = data.proposals ?? [];
  const pending = proposals.filter(({ status }) => !status.decision);
  const current =
    pending.length === 1
      ? pending[0]
      : proposals.length === 1
        ? proposals[0]
        : undefined;
  const ordered = current
    ? [current, ...proposals.filter((proposal) => proposal !== current)]
    : proposals;
  return (
    <>
      <header className="detail-heading">
        <div className="detail-kicker">
          {!proposals.length && (
            <span>
              ISSUE {data.short_number ? `#${data.short_number}` : ""}
            </span>
          )}
          <span className="detail-stage" data-stage={data.stage}>
            {issueStageLabels[data.stage]}
          </span>
        </div>
        <h1>
          {proposals.length
            ? data.title
            : issueTitle(data.title, data.short_number)}
        </h1>
        <nav className="detail-jumps" aria-label="Issue 入口">
          <a href={chatLink(data.chat_id, data.source_message_id)}>
            {proposals.length ? "查看原 Chat" : "查看原消息"}
          </a>
          {proposals.length === 0 && (
            <>
              <a href="#issue-discussion">跳到 Issue 对话</a>
              <a href="#issue-properties">工作属性</a>
            </>
          )}
        </nav>
        {data.objective.trim() && <Objective text={data.objective} />}
      </header>
      {data.proposal_requires_chat ? (
        <div className="proposal-approval proposal-choices">
          <strong>待你决定</strong>
          <p>这里有多个尚未处理的决定。请在原 Chat 逐项选择。</p>
          <ul>
            {pending.map((proposal) => (
              <li key={proposal.attention_id}>
                <a
                  href={chatLink(
                    data.chat_id,
                    proposal.question_message_id ?? data.source_message_id,
                  )}
                >
                  {proposal.status.question}
                </a>
              </li>
            ))}
          </ul>
        </div>
      ) : (
        current && (
          <ProposalApproval
            key={`${current.attention_id}-${current.status.version}`}
            chat={data.chat_id}
            attention={current.attention_id}
            status={current.status}
            inIssue
            onRefresh={onRefresh}
          />
        )
      )}
      {ordered.map((proposal) => (
        <ProposalReport
          key={proposal.attention_id}
          proposal={proposal}
          chat={data.chat_id}
          selected={proposal === current}
          onRefresh={onRefresh}
        />
      ))}
      {proposals.length ? (
        reports.length > 0 && (
          <details className="proposal-other-reports">
            <summary>其他报告 ({reports.length})</summary>
            <DeliveredReports reports={reports} chat={data.chat_id} />
          </details>
        )
      ) : (
        <section className="detail-section" aria-labelledby="issue-reports">
          <div className="detail-section-heading">
            <h2 id="issue-reports">报告与评审材料</h2>
            {reports.length > 0 && <span>{reports.length} 份已回传报告</span>}
          </div>
          <DeliveredReports reports={reports} chat={data.chat_id} />
        </section>
      )}
      <IssueAttention
        key={data.id}
        issue={data.id}
        chat={data.chat_id}
        decisions={decisions}
        historyBefore={data.decision_history_before}
        onRefresh={onRefresh}
      />
      <IssueTokenUsage key={data.id} issue={data.id} />
    </>
  );
}

function WorkDeliveries({ data }: { data: IssueOverviewData }) {
  const reported = new Set([
    ...(data.reports ?? []).flatMap((report) => [
      ...report.artifacts.map(({ id }) => id),
      ...(report.history ?? []).flatMap((version) =>
        version.artifacts.map(({ id }) => id),
      ),
    ]),
    ...(data.proposals ?? []).flatMap((proposal) =>
      proposal.versions.flatMap((version) => [
        version.artifact.id,
        ...version.other_artifacts.map(({ id }) => id),
      ]),
    ),
  ]);
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
          <Delivery key={file.id} file={file} />
        ))}
      </ul>
    </details>
  );
}

function IssueProperties({ data }: { data: IssueOverviewData }) {
  const structured = Boolean(data.proposals?.length);
  const [wide, setWide] = useState(
    () => window.matchMedia("(min-width: 861px)").matches,
  );
  useEffect(() => {
    if (!structured) return;
    const viewport = window.matchMedia("(min-width: 861px)");
    const update = () => setWide(viewport.matches);
    viewport.addEventListener("change", update);
    return () => viewport.removeEventListener("change", update);
  }, [structured]);

  const children = (
    <ul>
      {data.children.map((child) => (
        <li key={child.id}>
          <a href={issueLink(child.id, child.short_number)}>
            {issueTitle(child.title, child.short_number)}
          </a>
        </li>
      ))}
    </ul>
  );
  const body = (
    <div className="detail-properties">
      <h2>工作属性</h2>
      <dl>
        <div>
          <dt>阶段</dt>
          <dd>{issueStageLabels[data.stage]}</dd>
        </div>
        <div>
          <dt>负责 Agent</dt>
          <dd>{data.owner_agent_id ?? "未领取"}</dd>
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
          <h3>{pullSectionTitle(data.pulls)}</h3>
          <ul>
            {data.pulls.map((pull) => (
              <li key={pull.url}>
                <PullAssociation pull={pull} />
              </li>
            ))}
          </ul>
        </section>
      )}
      {data.children.length > 0 && (
        <section>
          {structured ? (
            <details className="proposal-children">
              <summary>子 Issue · {data.children.length}</summary>
              {children}
            </details>
          ) : (
            <>
              <h3>子 Issue</h3>
              {children}
            </>
          )}
        </section>
      )}
    </div>
  );
  return !structured || wide ? (
    body
  ) : (
    <details className="proposal-mobile-properties">
      <summary>工作属性</summary>
      {body}
    </details>
  );
}

export function IssueDetail({
  issue,
  chatId,
  onReachable,
}: {
  issue: string;
  chatId: string;
  onReachable: (online: boolean) => void;
}) {
  const [data, setData] = useState<IssueOverviewData | null>(null);
  const [refresh, setRefresh] = useState(0);
  const [error, setError] = useState("");

  useEffect(() => {
    const controller = new AbortController();
    let timer: number;
    async function update() {
      try {
        const result = await readIssueOverview(issue, controller.signal);
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
  }, [issue, refresh]);

  return (
    <Chat
      issue={issue}
      chatId={chatId}
      onReachable={onReachable}
      detail={{
        overviewLoaded: data !== null,
        refresh,
        structured: Boolean(data?.proposals?.length),
        header: (
          <>
            {error && (
              <p className="error" role="alert">
                {error}
              </p>
            )}
            {data ? (
              <IssueSummary
                data={data}
                onRefresh={() => setRefresh((value) => value + 1)}
              />
            ) : (
              !error && <p role="status">正在读取 Issue 详情…</p>
            )}
          </>
        ),
        work: data && <WorkDeliveries data={data} />,
        properties: data && <IssueProperties data={data} />,
      }}
    />
  );
}
