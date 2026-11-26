import { useEffect, useRef, useState } from "react";
import { issueLink, issueTitle, readIssueTokenUsage } from "./api";
import type { IssueTokenUsageData, TokenBreakdown, TokenField } from "./api";

const format = new Intl.NumberFormat("zh-CN");
const metrics = [
  ["input_tokens", "输入"],
  ["cached_input_tokens", "缓存读取"],
  ["cache_write_input_tokens", "缓存写入"],
  ["output_tokens", "输出"],
  ["reasoning_output_tokens", "推理输出"],
] as const;
const pageSize = 30;

type State =
  | { status: "loading" }
  | { status: "error" }
  | { status: "ready"; usage: IssueTokenUsageData };

function tokens(field: TokenField) {
  return field.tokens === null ? "—" : format.format(BigInt(field.tokens));
}

function fieldNote(field: TokenField, usage: TokenBreakdown) {
  if (field.tokens === null) {
    return usage.reported_requests > 0 ? "字段缺失" : "未知";
  }
  if (field.reported_requests < usage.reported_requests) {
    return `${format.format(field.reported_requests)} / ${format.format(usage.reported_requests)} 条请求有此字段`;
  }
  return undefined;
}

function RunCoverage({ usage }: { usage: TokenBreakdown }) {
  if (usage.started_runs === 0) {
    return <p className="detail-muted token-coverage">没有已开始的 Run。</p>;
  }
  return (
    <div className="token-coverage">
      <p>
        <strong>{usage.observed_runs ? "部分覆盖" : "完全未观测"}</strong>
        <span>
          已观测 {format.format(usage.observed_runs)} /{" "}
          {format.format(usage.started_runs)} 个已开始 Run
        </span>
      </p>
      {usage.unobserved_before_collection > 0 && (
        <p className="detail-muted">
          采集启用前 · {format.format(usage.unobserved_before_collection)} 个
        </p>
      )}
      {usage.unobserved_no_usage > 0 && (
        <p className="detail-muted">
          已开始但无可用用量事件 · {format.format(usage.unobserved_no_usage)} 个
        </p>
      )}
      {usage.observed_runs > 0 && (
        <p className="detail-muted">
          至少收到一条有效请求即为已观测，无法确认尾部完整。
        </p>
      )}
    </div>
  );
}

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
              <small className="detail-muted">
                {direct.started_runs === 0
                  ? "没有已开始的 Run"
                  : direct.observed_runs === 0
                    ? "完全未观测"
                    : `已观测 ${format.format(direct.observed_runs)} / ${format.format(direct.started_runs)} 个 Run`}
              </small>
            </div>
            <div className="token-issue-value">
              <bdi>{tokens(direct.total_tokens)}</bdi>
              <small className="detail-muted">
                {fieldNote(direct.total_tokens, direct) ?? "已上报 Token"}
              </small>
            </div>
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
          <div className="token-total">
            <bdi>{tokens(active.total_tokens)}</bdi>
            <span>Token</span>
          </div>
          <p className="detail-muted token-source">
            App Server 原生上报
            {fieldNote(active.total_tokens, active)
              ? ` · ${fieldNote(active.total_tokens, active)}`
              : ""}
          </p>
          <RunCoverage usage={active} />
          <dl className="token-metrics">
            {metrics.map(([key, label]) => (
              <div key={key}>
                <dt>{label}</dt>
                <dd>
                  <bdi>{tokens(active[key])}</bdi>
                </dd>
                {fieldNote(active[key], active) && (
                  <dd className="detail-muted">
                    {fieldNote(active[key], active)}
                  </dd>
                )}
              </div>
            ))}
          </dl>
          <p className="detail-muted token-metric-note">
            各分项按原生字段汇总，不相加补齐总量。
          </p>
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
