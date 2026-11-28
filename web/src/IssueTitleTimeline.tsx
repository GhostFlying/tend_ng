import { useEffect, useRef, useState } from "react";
import { readIssueTitleHistory, type IssueTitleHistory } from "./api";
import { DecisionTime } from "./ProposalApproval";

export function IssueTitleTimeline({
  issue,
  recent,
}: {
  issue: string;
  recent: IssueTitleHistory;
}) {
  const [history, setHistory] = useState<IssueTitleHistory>();
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const pending = useRef<AbortController>(null);
  useEffect(() => () => pending.current?.abort(), []);

  // Page from a stable view once browsing older events. Closing and reopening
  // returns to the current first page, which is refreshed with the overview.
  const page = history ?? recent;
  function reset() {
    pending.current?.abort();
    pending.current = null;
    setHistory(undefined);
    setLoading(false);
    setError("");
  }

  async function loadEarlier() {
    if (page.next_before === undefined || pending.current) return;
    const controller = new AbortController();
    pending.current = controller;
    setHistory(page);
    setLoading(true);
    setError("");
    try {
      const earlier = await readIssueTitleHistory(
        issue,
        page.next_before,
        controller.signal,
      );
      if (controller.signal.aborted) return;
      setHistory({
        changes: [...page.changes, ...earlier.changes],
        next_before: earlier.next_before,
      });
    } catch {
      if (!controller.signal.aborted) setError("暂时无法读取更早的改名");
    } finally {
      if (pending.current === controller) {
        pending.current = null;
        setLoading(false);
      }
    }
  }

  return (
    <details
      className="issue-title-history"
      onToggle={(event) => {
        if (!event.currentTarget.open) reset();
      }}
    >
      <summary>改名历史</summary>
      <ol>
        {page.changes.map(
          ({ operation_id, old_title, new_title, actor, changed_at }) => (
            <li key={`${actor.kind}:${operation_id}`}>
              <p className="issue-title-history-meta">
                <strong>
                  {actor.kind === "owner" ? actor.agent_id : "用户"}
                </strong>
                <DecisionTime time={changed_at} />
              </p>
              <dl>
                <dt>原题</dt>
                <dd>{old_title}</dd>
                <dt>新题</dt>
                <dd>{new_title}</dd>
              </dl>
            </li>
          ),
        )}
      </ol>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {page.next_before !== undefined && (
        <button
          type="button"
          className="text-button"
          onClick={() => void loadEarlier()}
          disabled={loading}
        >
          {loading ? "正在读取…" : "更早改名"}
        </button>
      )}
    </details>
  );
}
