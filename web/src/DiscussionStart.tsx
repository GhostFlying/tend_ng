import { useRef, useState } from "react";
import { operationId } from "./api";
import { createDiscussion, discussionLink } from "./discussionApi";
import type { Origin } from "./discussionApi";

export function DiscussionStart({
  origin,
  compact = false,
}: {
  origin?: Origin;
  compact?: boolean;
}) {
  const id = useRef<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  async function open() {
    if (busy) return;
    setBusy(true);
    setError("");
    id.current ??= operationId();
    try {
      const { chat_id } = await createDiscussion(id.current, origin);
      window.location.assign(discussionLink(chat_id));
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : "暂时无法新建讨论");
      setBusy(false);
    }
  }
  return (
    <span className="discussion-start">
      <button
        type="button"
        className={compact ? "text-button" : "primary"}
        disabled={busy}
        onClick={() => void open()}
      >
        {busy ? "正在新建…" : origin ? "带入讨论" : "新讨论"}
      </button>
      {error && (
        <small className="error" role="alert">
          {error}
        </small>
      )}
    </span>
  );
}
