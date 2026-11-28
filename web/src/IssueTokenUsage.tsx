import { useEffect, useRef, useState } from "react";
import { issueLink, issueTitle, readIssueTokenUsage } from "./api";
import type { IssueTokenUsageData } from "./api";
import {
  coverageLabel,
  RequestCount,
  UsageHeadline,
  UsageMethod,
  UsageMetrics,
} from "./TokenUsageSummary";
import { format } from "./tokenDisplay";
import { TokenModelUsage } from "./TokenModelUsage";

const pageSize = 30;

type State =
  | { status: "loading" }
  | { status: "error" }
  | { status: "ready"; usage: IssueTokenUsageData };

function IssueRows({
  issue,
  nodes,
}: {
  issue: string;
  nodes: IssueTokenUsageData["nodes"];
}) {
  const [limit, setLimit] = useState(pageSize);
  return (
    <div className="token-issue-rows">
      <h4>子树明细</h4>
      <p className="detail-muted">
        各 Issue 直属合计等于子树汇总。每行不含该 Issue 的后代。
      </p>
      <ul aria-label="各 Issue 直属用量">
        {nodes.slice(0, limit).map(({ issue_id, number, title, direct }) => (
          <li key={issue_id}>
            <div>
              {issue_id === issue ? (
                <span>{issueTitle(title, number)} · 本 Issue</span>
              ) : (
                <a href={issueLink(issue_id, number)}>
                  {issueTitle(title, number)}
                </a>
              )}
              <RequestCount usage={direct} />
              <small className="detail-muted">
                {coverageLabel(direct)}
                {direct.started_runs > 0 &&
                  ` · ${format.format(direct.observed_runs)} / ${format.format(direct.started_runs)}`}
              </small>
            </div>
            <UsageMetrics usage={direct} />
          </li>
        ))}
      </ul>
      {nodes.length > limit && (
        <button
          className="token-action"
          type="button"
          onClick={() => setLimit((n) => n + pageSize)}
        >
          显示更多（{limit} / {nodes.length}）
        </button>
      )}
    </div>
  );
}

function UsagePane({ issue }: { issue: string }) {
  const [scope, setScope] = useState<"direct" | "subtree">("direct");
  const [state, setState] = useState<State>({ status: "loading" });
  const [revision, setRevision] = useState(0);
  const focusAfterRefresh = useRef(false);
  const status = useRef<HTMLHeadingElement>(null);

  useEffect(() => {
    const controller = new AbortController();
    void readIssueTokenUsage(issue, controller.signal).then(
      (usage) => {
        if (!controller.signal.aborted) setState({ status: "ready", usage });
      },
      () => {
        if (!controller.signal.aborted) setState({ status: "error" });
      },
    );
    return () => controller.abort();
  }, [issue, revision]);

  useEffect(() => {
    if (!focusAfterRefresh.current) return;
    // Don't steal focus if the user moved on while the request was pending.
    if (
      document.activeElement === document.body ||
      document.activeElement === status.current
    ) {
      status.current?.focus();
    }
    if (state.status !== "loading") focusAfterRefresh.current = false;
  }, [state.status]);

  function refresh() {
    focusAfterRefresh.current = true;
    setState({ status: "loading" });
    setRevision((value) => value + 1);
  }

  const active = state.status === "ready" ? state.usage[scope] : undefined;
  return (
    <div className="token-pane" aria-busy={state.status === "loading"}>
      <div className="token-controls">
        <div className="token-scope" role="group" aria-label="Token 用量范围">
          <button
            type="button"
            aria-pressed={scope === "direct"}
            onClick={() => setScope("direct")}
          >
            直属用量
          </button>
          <button
            type="button"
            aria-pressed={scope === "subtree"}
            onClick={() => setScope("subtree")}
          >
            含所有后代
          </button>
        </div>
        {state.status === "ready" && (
          <button className="token-action" type="button" onClick={refresh}>
            更新用量
          </button>
        )}
      </div>
      <h3 className="token-state-heading" ref={status} tabIndex={-1}>
        {state.status === "loading"
          ? "正在读取用量…"
          : state.status === "error"
            ? "暂时无法读取用量"
            : "已上报总量"}
      </h3>
      {state.status === "error" && (
        <div role="alert">
          <p className="detail-muted">未显示本次用量，实际用量未知。</p>
          <button className="token-action" type="button" onClick={refresh}>
            重试
          </button>
        </div>
      )}
      {active && (
        <>
          <UsageHeadline usage={active} />
          <div className="usage-issue-fields">
            <UsageMetrics usage={active} compact showEstimate={false} />
          </div>
          <UsageMethod usage={active} />
          <TokenModelUsage key={scope} usage={active} label="模型明细" />
        </>
      )}
      {state.status === "ready" && scope === "subtree" && (
        <IssueRows issue={issue} nodes={state.usage.nodes} />
      )}
    </div>
  );
}

export function IssueTokenUsage({ issue }: { issue: string }) {
  const [opened, setOpened] = useState(false);
  return (
    <details
      className="detail-section token-section"
      onToggle={(event) => setOpened(event.currentTarget.open)}
    >
      <summary>
        <span>Token 用量</span>
        <small className="detail-muted">直属与子树</small>
      </summary>
      {opened && <UsagePane issue={issue} />}
    </details>
  );
}
