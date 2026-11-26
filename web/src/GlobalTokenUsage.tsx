import { useEffect, useId, useState } from "react";
import { issueLink, issueTitle, readTokenOverview } from "./api";
import type {
  TokenBreakdown,
  TokenOverviewIssue,
  TokenOverviewPage,
} from "./api";
import {
  coverageLabel,
  fieldNote,
  format,
  RunCoverage,
  tokens,
  TokenTotal,
} from "./TokenUsageSummary";
import "./global-token-usage.css";

function IssueValue({
  label,
  usage,
}: {
  label: string;
  usage: TokenBreakdown;
}) {
  const field = usage.total_tokens;
  return (
    <div className="usage-tree-value">
      <dt>{label}</dt>
      <dd>
        <bdi>{tokens(field)}</bdi>
      </dd>
      <dd className="detail-muted">
        {coverageLabel(usage)}
        {usage.started_runs > 0 &&
          ` · ${format.format(usage.observed_runs)} / ${format.format(usage.started_runs)}`}
      </dd>
      {usage.reported_requests > 0 && fieldNote(field, usage) && (
        <dd className="detail-muted">{fieldNote(field, usage)}</dd>
      )}
    </div>
  );
}

function IssueRow({
  issue,
  depth,
}: {
  issue: TokenOverviewIssue;
  depth: number;
}) {
  const [expanded, setExpanded] = useState(false);
  const childrenId = useId();
  const title = issueTitle(issue.title, issue.number);
  return (
    <li>
      <div className="usage-tree-row">
        <div className="usage-tree-issue">
          {issue.has_children ? (
            <button
              className="usage-tree-toggle"
              type="button"
              aria-label={`${expanded ? "收起" : "展开"} #${issue.number} 的子级`}
              aria-expanded={expanded}
              aria-controls={expanded ? childrenId : undefined}
              onClick={() => setExpanded((value) => !value)}
            >
              <svg
                width="16"
                height="16"
                viewBox="0 0 16 16"
                fill="none"
                aria-hidden="true"
              >
                <path
                  d="m6 4 4 4-4 4"
                  stroke="currentColor"
                  strokeWidth="1.5"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                />
              </svg>
            </button>
          ) : (
            <span className="usage-tree-toggle-space" aria-hidden="true" />
          )}
          <a href={issueLink(issue.issue_id, issue.number)}>{title}</a>
        </div>
        <dl className="usage-tree-values">
          <IssueValue label="直属" usage={issue.direct} />
          <IssueValue label="含后代" usage={issue.subtree} />
        </dl>
      </div>
      {expanded && (
        <div
          id={childrenId}
          className="usage-tree-children"
          data-indent={depth < 2}
        >
          <UsageLevel
            parent={issue.issue_id}
            label={`#${issue.number} 的直接子级`}
            depth={depth + 1}
          />
        </div>
      )}
    </li>
  );
}

function UsageLevel({
  parent,
  label,
  depth,
  onReachable,
}: {
  parent?: string;
  label: string;
  depth: number;
  onReachable?: (reachable: boolean) => void;
}) {
  const [attempt, setAttempt] = useState<{ before: string | null }>({
    before: null,
  });
  const [state, setState] = useState<{
    status: "loading" | "ready" | "error";
    page: TokenOverviewPage | null;
  }>({ status: "loading", page: null });

  useEffect(() => {
    const controller = new AbortController();
    void readTokenOverview(parent, attempt.before, controller.signal).then(
      (page) => {
        if (controller.signal.aborted) return;
        setState((previous) => ({
          status: "ready",
          page:
            attempt.before !== null && previous.page
              ? {
                  ...page,
                  issues: [...previous.page.issues, ...page.issues],
                }
              : page,
        }));
        onReachable?.(true);
      },
      () => {
        if (controller.signal.aborted) return;
        setState((previous) => ({ ...previous, status: "error" }));
        onReachable?.(false);
      },
    );
    return () => controller.abort();
  }, [parent, attempt, onReachable]);

  function load(before: string | null) {
    setState((previous) => ({ ...previous, status: "loading" }));
    setAttempt({ before });
  }

  const { status, page } = state;
  return (
    <>
      {page?.total && (
        <section
          className="token-pane usage-overview"
          aria-labelledby="usage-total-heading"
        >
          <h2 id="usage-total-heading" className="token-state-heading">
            已归属 Issue 的全局用量
          </h2>
          <TokenTotal usage={page.total} />
          <RunCoverage usage={page.total} />
          <p className="detail-muted token-metric-note">
            按各 Issue 直属用量汇总，每个 Issue 只计算一次。不含未归属 Issue
            的用量。
          </p>
        </section>
      )}
      {page && !parent && (
        <div className="usage-tree-heading">
          <h2>Issue 树</h2>
          <p className="detail-muted">
            直属只包含该 Issue，含后代包括该 Issue 本身和全部后代。
          </p>
        </div>
      )}
      {page &&
        (page.issues.length ? (
          <ul className="usage-tree-list" aria-label={label}>
            {page.issues.map((issue) => (
              <IssueRow key={issue.issue_id} issue={issue} depth={depth} />
            ))}
          </ul>
        ) : (
          <p className="detail-muted">
            {parent ? "没有直接子级。" : "还没有顶层 Issue。"}
          </p>
        ))}
      {status === "loading" && !page && (
        <p className="detail-muted" role="status">
          {parent ? "正在读取直接子级…" : "正在读取用量…"}
        </p>
      )}
      {status === "error" && (
        <p className="usage-tree-status" role="alert">
          {attempt.before
            ? "暂时无法读取下一页。"
            : parent
              ? "暂时无法读取直接子级。"
              : "暂时无法读取用量。"}
        </p>
      )}
      {status === "error" || page?.next_before ? (
        <button
          className="token-action usage-tree-more"
          type="button"
          disabled={status === "loading"}
          onClick={() =>
            load(
              status === "error" ? attempt.before : (page?.next_before ?? null),
            )
          }
        >
          {status === "error"
            ? "重试"
            : status === "loading"
              ? "正在加载…"
              : "加载更多"}
        </button>
      ) : null}
    </>
  );
}

export function GlobalTokenUsage({
  onReachable,
}: {
  onReachable: (reachable: boolean) => void;
}) {
  const [revision, setRevision] = useState(0);
  return (
    <main className="resource-page">
      <div className="usage-page-inner">
        <div className="usage-page-toolbar">
          <p className="detail-muted">基于已同步的 Issue 执行数据</p>
          <button
            className="token-action"
            type="button"
            onClick={() => setRevision((value) => value + 1)}
          >
            刷新用量
          </button>
        </div>
        <UsageLevel
          key={revision}
          label="顶层 Issue"
          depth={0}
          onReachable={onReachable}
        />
      </div>
    </main>
  );
}
