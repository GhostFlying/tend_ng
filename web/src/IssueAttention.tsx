import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import type { FormEvent } from "react";
import {
  answerIssueAttention,
  chatLink,
  operationId,
  readIssueAttentionHistory,
} from "./api";
import type { IssueDecision, PendingAttentionReply } from "./api";
import { Markdown } from "./Markdown";

function savedReply(key: string): PendingAttentionReply | null {
  try {
    const value = JSON.parse(sessionStorage.getItem(key) ?? "null");
    return typeof value?.operation_id === "string" &&
      typeof value.text === "string"
      ? value
      : null;
  } catch {
    return null;
  }
}

function DirectReply({
  token,
  issue,
  reply,
  onRefresh,
}: {
  token: string;
  issue: string;
  reply: NonNullable<IssueDecision["reply"]>;
  onRefresh: () => void;
}) {
  const key = `tend.pending-attention-reply.v1:${issue}:${reply.attention_id}`;
  const [saved] = useState(() =>
    reply.answer === null ? savedReply(key) : null,
  );
  const attempt = useRef(saved);
  const [text, setText] = useState(saved?.text ?? "");
  const [sending, setSending] = useState(false);
  const [error, setError] = useState("");
  const [confirmed, setConfirmed] = useState<string | null>(null);
  const record = useRef<HTMLDivElement>(null);
  const input = useId();
  const answer = reply.answer ?? confirmed;

  useEffect(() => {
    if (answer !== null) sessionStorage.removeItem(key);
  }, [answer, key]);

  useEffect(() => {
    if (confirmed !== null) record.current?.focus();
  }, [confirmed]);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (sending || !text.trim()) return;
    const pending =
      attempt.current?.text === text
        ? attempt.current
        : { operation_id: operationId(), text };
    attempt.current = pending;
    setSending(true);
    setError("");
    try {
      sessionStorage.setItem(key, JSON.stringify(pending));
      const receipt = await answerIssueAttention(
        token,
        issue,
        reply.attention_id,
        pending,
      );
      setConfirmed(receipt.answer);
      onRefresh();
    } catch (error) {
      setError(
        error instanceof TypeError
          ? "暂时无法确认答复是否已记录。再次提交原文会沿原操作重试。"
          : error instanceof Error
            ? error.message
            : "暂时无法提交答复。",
      );
    } finally {
      setSending(false);
    }
  }

  if (answer !== null) {
    return (
      <div className="attention-recorded" ref={record} tabIndex={-1}>
        <span>已记录答复</span>
        <p>{answer}</p>
      </div>
    );
  }

  return (
    <details className="issue-attention-reply" open={saved ? true : undefined}>
      <summary className="text-button">回答此问题</summary>
      <form onSubmit={submit}>
        <label htmlFor={input}>答复原文</label>
        <textarea
          id={input}
          value={text}
          rows={3}
          readOnly={sending}
          onChange={(event) => setText(event.target.value)}
        />
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
        <button
          type="submit"
          className="proposal-button"
          disabled={sending || !text.trim()}
        >
          {sending ? "正在提交…" : "提交答复"}
        </button>
      </form>
    </details>
  );
}

type ReplyHistory = {
  decisions: IssueDecision[];
  head: string[];
  before: number | null;
  mayHaveGap: boolean;
};

function combineReplies(head: IssueDecision[], tail: IssueDecision[]) {
  const seen = new Set(head.map(({ message_id }) => message_id));
  return [
    ...head,
    ...tail.filter(({ message_id }) => {
      if (seen.has(message_id)) return false;
      seen.add(message_id);
      return true;
    }),
  ];
}

function replyRowId(message: string) {
  return `issue-attention-${message}`;
}

export function IssueAttention({
  token,
  issue,
  chat,
  decisions,
  historyBefore,
  onRefresh,
}: {
  token: string;
  issue: string;
  chat: string;
  decisions: IssueDecision[];
  historyBefore?: number;
  onRefresh: () => void;
}) {
  const [older, setOlder] = useState<ReplyHistory>();
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const request = useRef<AbortController | null>(null);
  const loadButton = useRef<HTMLButtonElement>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  const nextFocus = useRef<string | null>(null);
  useEffect(() => () => request.current?.abort(), []);
  useLayoutEffect(() => {
    if (nextFocus.current === null) return;
    const target = document.getElementById(replyRowId(nextFocus.current));
    (target ?? heading.current)?.focus();
    nextFocus.current = null;
  }, [older]);

  const pending = decisions.filter(({ reply }) => reply?.answer == null);
  const recent = decisions.filter(({ reply }) => reply?.answer != null);
  const head = recent.map(({ message_id }) => message_id);
  if (
    older &&
    (older.head.length !== head.length ||
      older.head.some((id, index) => id !== head[index]))
  ) {
    // A live poll can push previously displayed replies beyond the hot slice.
    // Keep them once the reader has opted into history, even at end of cursor.
    setOlder({
      ...older,
      head,
      decisions: combineReplies(recent, older.decisions),
      mayHaveGap:
        older.mayHaveGap ||
        (older.head.length > 0 &&
          head.length > 0 &&
          !older.head.some((id) => head.includes(id))),
    });
  }
  const before = older ? older.before : historyBefore;
  const settled = older?.decisions ?? recent;
  const rows = [...pending, ...settled];
  if (!rows.length) return null;

  async function loadOlder() {
    if (before == null || request.current) return;
    const controller = new AbortController();
    request.current = controller;
    setLoading(true);
    setError("");
    try {
      const page = await readIssueAttentionHistory(
        token,
        issue,
        before,
        controller.signal,
      );
      if (controller.signal.aborted) return;
      if (
        page.next_before === null &&
        loadButton.current &&
        document.activeElement === loadButton.current
      )
        nextFocus.current = page.decisions[0]?.message_id ?? "";
      setOlder((current) => ({
        decisions: combineReplies(current?.decisions ?? recent, page.decisions),
        head: current?.head ?? head,
        before: page.next_before,
        mayHaveGap: current?.mayHaveGap ?? false,
      }));
    } catch (error) {
      if (!controller.signal.aborted)
        setError(error instanceof Error ? error.message : "暂时无法读取答复");
    } finally {
      if (!controller.signal.aborted) {
        request.current = null;
        setLoading(false);
      }
    }
  }
  function restartHistory() {
    if (request.current) return;
    setOlder(undefined);
    setError("");
    heading.current?.focus();
  }
  return (
    <section className="detail-section" aria-labelledby="issue-decisions">
      <div className="detail-section-heading">
        <h2 id="issue-decisions" ref={heading} tabIndex={-1}>
          {settled.length === 0
            ? "待你决定"
            : pending.length === 0
              ? "已答复的问题"
              : "问题与答复"}
        </h2>
      </div>
      <ul className="issue-decisions">
        {rows.map(({ message_id: message, text, reply }) => (
          <li key={message} id={replyRowId(message)} tabIndex={-1}>
            <Markdown text={text} />
            {reply && (
              <DirectReply
                key={`${issue}:${reply.attention_id}`}
                token={token}
                issue={issue}
                reply={reply}
                onRefresh={onRefresh}
              />
            )}
            <a href={chatLink(chat, message)}>
              {!reply || reply.answer === null
                ? "在原 Chat 处理"
                : "查看原提问"}
            </a>
          </li>
        ))}
      </ul>
      {older?.mayHaveGap && (
        <div className="issue-attention-history">
          <p role="status">可能还有未显示的新答复。</p>
          <button
            type="button"
            className="text-button"
            aria-disabled={loading}
            onClick={restartHistory}
          >
            从最新答复重新查看
          </button>
        </div>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {before != null && (
        <button
          type="button"
          className="text-button issue-attention-history"
          ref={loadButton}
          aria-disabled={loading}
          onClick={() => void loadOlder()}
        >
          {loading ? "正在读取…" : "读取更早的答复"}
        </button>
      )}
    </section>
  );
}
