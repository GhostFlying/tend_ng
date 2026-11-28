import { useEffect, useId, useState } from "react";
import { issueLink, issueTitle, readTokenOverview } from "./api";
import type {
  TokenBreakdown,
  TokenOverviewIssue,
  TokenOverviewPage,
  TokenUsageSource,
} from "./api";
import {
  coverageLabel,
  RawTokenFields,
  RequestCount,
  UsageHeadline,
  UsageMethod,
  UsageMetrics,
} from "./TokenUsageSummary";
import { format, tokens } from "./tokenDisplay";
import { TokenModelUsage } from "./TokenModelUsage";
import "./global-token-usage.css";

function IssueValue({
  label,
  usage,
}: {
  label: string;
  usage: TokenBreakdown;
}) {
  return (
    <div className="usage-tree-value">
      <dt>
        <strong>{label}</strong>
        <span>
          <bdi className="usage-tree-tokens">{tokens(usage.total_tokens)}</bdi>{" "}
          Token
        </span>
      </dt>
      <dd>
        <RequestCount usage={usage} />
      </dd>
      {usage.reported_requests > 0 && (
        <dd>
          <UsageMetrics usage={usage} />
        </dd>
      )}
      <dd className="detail-muted usage-tree-coverage">
        {coverageLabel(usage)}
        {usage.started_runs > 0 &&
          ` · ${format.format(usage.observed_runs)} / ${format.format(usage.started_runs)}`}
      </dd>
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
  const [modelsOpen, setModelsOpen] = useState(false);
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
      {((issue.direct.models?.length ?? 0) > 0 ||
        (issue.subtree.models?.length ?? 0) > 0) && (
        <details
          className="usage-tree-models"
          onToggle={(event) => setModelsOpen(event.currentTarget.open)}
        >
          <summary>#{issue.number} 模型明细</summary>
          {modelsOpen && (
            <>
              <TokenModelUsage
                usage={issue.direct}
                label="直属模型明细"
                compact
              />
              <TokenModelUsage
                usage={issue.subtree}
                label="含后代模型明细"
                compact
              />
            </>
          )}
        </details>
      )}
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

const sourceNames: Record<string, string> = {
  runner: "Issue Agent",
  router: "主 Chat Router",
  discussion: "讨论回复",
  discussion_summary: "讨论转交总结",
  handoff_receiver: "隔离转交接收",
  feishu: "飞书研判",
};

function SourceRow({ row }: { row: TokenUsageSource }) {
  const [expanded, setExpanded] = useState(false);
  const usage = row.usage;
  const name = sourceNames[row.source] ?? row.source;
  const unit = row.source === "runner" ? "Run" : "回合";
  const hasRequests = usage.reported_requests > 0;
  const notes = [
    row.source === "runner" && "含该 Run 的原生子 Agent",
    usage.unobserved_before_collection > 0 &&
      `采集启用前 ${format.format(usage.unobserved_before_collection)} 个`,
    row.responses_without_usage !== null &&
      row.responses_without_usage > 0 &&
      `${format.format(row.responses_without_usage)} 条请求用量未知`,
    row.attempts_without_turn > 0 &&
      `${format.format(row.attempts_without_turn)} 次尝试未核实回合`,
    row.shared_turn_attempts > 0 &&
      `${format.format(row.shared_turn_attempts)} 次共享回合已合并`,
    row.shared_issue_responses > 0 &&
      `${format.format(row.shared_issue_responses)} 条重叠请求归 Issue`,
    row.conflicts > 0 && `${format.format(row.conflicts)} 条请求用量冲突`,
  ].filter(Boolean);
  return (
    <li>
      <details
        className="usage-source"
        onToggle={(event) => setExpanded(event.currentTarget.open)}
      >
        <summary>
          <span className="usage-source-name">
            <strong>{name}</strong>
            <RequestCount usage={usage} />
          </span>
          {hasRequests && <UsageMetrics usage={usage} compact />}
        </summary>
        {expanded && (
          <div className="usage-source-detail">
            <p className="detail-muted">
              已观测 {format.format(usage.observed_runs)} /{" "}
              {format.format(usage.started_runs)} 个{unit}
              {notes.length > 0 && ` · ${notes.join(" · ")}`}
            </p>
            {hasRequests ? (
              <>
                <RawTokenFields usage={usage} />
                <TokenModelUsage
                  usage={usage}
                  label={`${name} · 模型明细`}
                  compact
                />
              </>
            ) : (
              <p className="detail-muted">该来源暂无已记录请求。</p>
            )}
          </div>
        )}
      </details>
    </li>
  );
}

function SourceRows({ rows }: { rows: TokenUsageSource[] }) {
  return (
    <section className="usage-sources" aria-labelledby="usage-sources-heading">
      <h2 id="usage-sources-heading">按调用来源</h2>
      <ul className="usage-source-list">
        {rows.map((row) => (
          <SourceRow key={row.source} row={row} />
        ))}
      </ul>
    </section>
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
            全系统 Agent 用量
          </h2>
          <UsageHeadline usage={page.total} />
          <UsageMethod usage={page.total} global />
          <TokenModelUsage usage={page.total} label="模型明细" />
        </section>
      )}
      {page?.sources && <SourceRows rows={page.sources} />}
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
