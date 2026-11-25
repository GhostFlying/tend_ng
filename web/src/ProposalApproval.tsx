import { useRef, useState } from "react";
import { approveProposal, issueLink } from "./api";
import type { ApprovalDecision, ProposalStatus } from "./api";

export function DecisionTime({ time }: { time: number }) {
  const date = new Date(time * 1000);
  return (
    <time dateTime={date.toISOString()}>
      {date.toLocaleString(undefined, {
        dateStyle: "short",
        timeStyle: "short",
      })}
    </time>
  );
}

export function ProposalApproval({
  token,
  chat,
  attention,
  status,
  inIssue,
  onRefresh,
}: {
  token: string;
  chat: string;
  attention: string;
  status: ProposalStatus;
  inIssue?: boolean;
  onRefresh: () => void;
}) {
  const inFlight = useRef(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [receipt, setReceipt] = useState<ApprovalDecision>();
  const decision = status.decision ?? receipt;

  async function approve() {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setError("");
    try {
      setReceipt(
        await approveProposal(token, chat, attention, status.version, inIssue),
      );
    } catch (reason) {
      setError(
        reason instanceof Error ? reason.message : "暂时无法确认审批结果",
      );
    } finally {
      inFlight.current = false;
      setBusy(false);
      onRefresh();
    }
  }

  const record =
    (inIssue ? "" : issueLink(status.issue_id)) +
    `#message-approval-${decision?.id}`;
  return (
    <div className="proposal-approval">
      <div className="proposal-action-line">
        <div>
          {decision ? (
            <span role="status">
              {decision.continuation_run_id
                ? "Agent 已接续"
                : "Approved, waiting for continuation"}
            </span>
          ) : (
            <>
              <strong>待你决定</strong>
              <span>{status.question}</span>
            </>
          )}
        </div>
        {decision ? (
          <a href={record}>查看批准记录</a>
        ) : status.can_approve ? (
          <button
            type="button"
            className="proposal-button"
            onClick={() => void approve()}
            disabled={busy}
          >
            {busy ? "正在提交…" : status.action}
          </button>
        ) : (
          <span className="detail-muted">当前不可批准</span>
        )}
      </div>
      {!inIssue && !decision && (
        <a href={issueLink(status.issue_id)}>查看 v{status.version} 报告</a>
      )}
      {!inIssue && decision && (
        <p className="detail-muted">
          你批准了 v{decision.version} ·{" "}
          <DecisionTime time={decision.decided_at} />
        </p>
      )}
      {error && !decision && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
