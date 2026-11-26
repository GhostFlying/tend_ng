import { useRef, useState } from "react";
import { operationId } from "./api";
import {
  createDiscussion,
  discussionLink,
  notifyDiscussionChanged,
} from "./discussionApi";
import type { Origin } from "./discussionApi";
import { navigate, trackPageVisit } from "./navigation";

export function DiscussionStart({
  origin,
  compact = false,
  sidebar = false,
}: {
  origin?: Origin;
  compact?: boolean;
  sidebar?: boolean;
}) {
  const id = useRef<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  async function open() {
    if (busy) return;
    const endVisit = trackPageVisit();
    let created: string | undefined;
    setBusy(true);
    setError("");
    id.current ??= operationId();
    try {
      const { chat_id } = await createDiscussion(id.current, origin);
      id.current = null;
      created = chat_id;
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : "暂时无法新建讨论");
    } finally {
      setBusy(false);
      const stayed = endVisit();
      if (created) {
        notifyDiscussionChanged(created);
        if (stayed) navigate(discussionLink(created));
      }
    }
  }
  const action = (
    <>
      <button
        type="button"
        className={
          sidebar
            ? "discussion-nav-create"
            : `primary${compact ? " discussion-start-sidebar" : ""}`
        }
        aria-label={sidebar ? "新建讨论" : undefined}
        title={sidebar ? "新建讨论" : undefined}
        disabled={busy}
        onClick={() => void open()}
      >
        {!origin && <span aria-hidden="true">+</span>}
        {!sidebar && (busy ? "正在新建…" : origin ? "带入讨论" : "新建讨论")}
      </button>
      {error && (
        <small
          className={sidebar ? "error discussion-nav-error" : "error"}
          role="alert"
        >
          {error}
        </small>
      )}
    </>
  );
  return sidebar ? action : <span className="discussion-start">{action}</span>;
}
