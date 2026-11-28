import { useEffect, useRef, useState } from "react";
import { approveProposal, issueLink, readProposalStatus } from "./api";
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
  chat,
  attention,
  status,
  inIssue,
  readOnly,
  onRefresh,
}: {
  chat: string;
  attention: string;
  status: ProposalStatus;
  inIssue?: boolean;
  readOnly?: boolean;
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
        await approveProposal(chat, attention, status.version, inIssue),
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
                : "已批准，等待接续"}
            </span>
          ) : (
            <>
              <strong>{readOnly ? "未记录决定" : "待你决定"}</strong>
              <span>{status.question}</span>
            </>
          )}
        </div>
        {decision ? (
          <a href={record}>查看批准记录</a>
        ) : status.can_approve && !readOnly ? (
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
      {decision?.continuation_blocked && (
        <p className="detail-muted">
          原 Task 未能开始接续。批准仍保存在原 Task，没有转交给其他 Task。
        </p>
      )}
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

export function ChatProposal({
  chat,
  attention,
  initial,
  historical,
  readOnly,
  onRefresh,
}: {
  chat: string;
  attention: string;
  initial: ProposalStatus;
  historical: boolean;
  readOnly?: boolean;
  onRefresh: () => void;
}) {
  const [fresh, setFresh] = useState<ProposalStatus>();
  const [refresh, setRefresh] = useState(0);
  useEffect(() => {
    if (!historical) return;
    const controller = new AbortController();
    const { signal } = controller;
    let timer: number;
    async function update() {
      try {
        const status = await readProposalStatus(chat, attention, signal);
        if (!signal.aborted) setFresh(status);
      } catch {
        // Keep the last saved view. The Server still validates every write.
      }
      if (!signal.aborted) timer = window.setTimeout(() => void update(), 5000);
    }
    void update();
    return () => {
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [chat, attention, historical, refresh]);
  const status = historical ? (fresh ?? initial) : initial;
  return (
    <ProposalApproval
      key={status.version}
      chat={chat}
      attention={attention}
      status={status}
      readOnly={readOnly}
      onRefresh={() => {
        if (historical) setRefresh((value) => value + 1);
        onRefresh();
      }}
    />
  );
}
