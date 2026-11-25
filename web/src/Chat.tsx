import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { FormEvent, ReactNode, RefObject } from "react";
import {
  chatLink,
  download,
  issueLink,
  issueTitle,
  mergeMessages,
  readMessages,
  readRollout,
  sendMessage,
} from "./api";
import type { Artifact, Message, PendingMessage } from "./api";

function pendingKey(chat: string, issue?: string) {
  if (issue) return `tend.pending-issue-message.v1:${issue}`;
  return chat === "main"
    ? "tend.pending-message.v1"
    : `tend.pending-message.v1:${chat}`;
}

function savedMessage(chat: string, issue?: string): PendingMessage | null {
  const value = sessionStorage.getItem(pendingKey(chat, issue));
  if (!value) return null;
  try {
    return JSON.parse(value) as PendingMessage;
  } catch {
    return null;
  }
}

function operationId() {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join(
    "",
  );
}

function errorText(error: unknown) {
  return error instanceof Error ? error.message : "暂时无法连接 Tend";
}

function messageAnchor() {
  return /^#message-([a-zA-Z0-9_-]+)$/.exec(window.location.hash)?.[1] ?? "";
}

function RunRow({
  id,
  token,
  run,
  author,
  issue,
}: {
  id: string;
  token: string;
  run: string;
  author: string;
  issue?: string;
}) {
  const [open, setOpen] = useState(false);
  const [text, setText] = useState<string | null>(null);
  const [error, setError] = useState("");
  const request = useRef<AbortController | null>(null);
  useEffect(() => () => request.current?.abort(), []);
  function toggle() {
    setOpen(!open);
    if (open) {
      request.current?.abort();
      return;
    }
    request.current = new AbortController();
    const { signal } = request.current;
    setText(null);
    setError("");
    void readRollout(token, run, signal).then(
      (value) => {
        if (!signal.aborted) setText(value);
      },
      (reason) => {
        if (!signal.aborted) setError(errorText(reason));
      },
    );
  }
  return (
    <li className="run-row" id={`message-${id}`}>
      <button
        type="button"
        className="text-button run-toggle"
        aria-expanded={open}
        aria-controls={`run-${run}`}
        onClick={toggle}
      >
        <span aria-hidden="true">{open ? "▾" : "▸"}</span>
        <span>{open ? "收起执行过程" : "查看执行过程"}</span>
        <span className="run-author">
          {author}
          {issue ? ` · ${issue}` : ""}
        </span>
      </button>
      {open && (
        <div id={`run-${run}`} className="run-detail">
          {error ? (
            <span className="error" role="alert">
              {error}
            </span>
          ) : text === null ? (
            <span role="status">正在读取…</span>
          ) : text ? (
            <pre>{text}</pre>
          ) : (
            <span>这次执行暂无过程记录</span>
          )}
        </div>
      )}
    </li>
  );
}

function MessageRow({
  message,
  source,
  showReference,
  sourceChat,
  descendant,
  token,
  onReply,
}: {
  message: Message;
  source?: Message;
  showReference: boolean;
  sourceChat: string;
  descendant: boolean;
  token: string;
  onReply: (message: Message) => void;
}) {
  const [error, setError] = useState("");
  const [downloading, setDownloading] = useState<string | null>(null);
  const user = message.author === "user";
  const detail = message.issue;
  const pending =
    message.router_state === "queued" ||
    message.router_state === "awaiting_turn";

  async function saveFile(file: Artifact) {
    setError("");
    setDownloading(file.id);
    try {
      await download(token, file);
    } catch (error) {
      setError(errorText(error));
    } finally {
      setDownloading(null);
    }
  }

  return (
    <li
      className={user ? "message message-user" : "message"}
      id={`message-${message.id}`}
    >
      <div className="message-heading">
        <span className="author">
          {user ? "你" : (detail?.author ?? "Tend")}
        </span>
        {descendant && detail && (
          <a
            className="issue-link"
            href={issueLink(detail.issue_id, detail.short_number)}
          >
            {issueTitle(detail.issue_title, detail.short_number)}
          </a>
        )}
        {detail?.kind === "dispatch" && (
          <span className="message-state">派发</span>
        )}
        {detail?.kind === "relay" && (
          <span className="message-state">
            {detail.forwarded_from ? "转达" : "来自主对话"}
          </span>
        )}
      </div>
      {source && showReference && (
        <a className="reference" href={`#message-${source.id}`}>
          {source.text}
        </a>
      )}
      <p>{message.text}</p>
      {message.issue_ids?.map((id, index, ids) => {
        const number = message.issue_numbers?.[index];
        return (
          <a className="issue-link" key={id} href={issueLink(id, number)}>
            查看 Issue
            {number !== undefined
              ? ` #${number}`
              : ids.length > 1
                ? ` ${index + 1}`
                : ""}
          </a>
        );
      })}
      {detail?.source_message_id && (
        <a
          className="issue-link"
          href={chatLink(sourceChat, detail.source_message_id)}
        >
          原对话
        </a>
      )}
      {!message.attention &&
      (detail || message.issue_ids?.length || source?.issue_ids?.length) ? (
        <button
          className="text-button"
          type="button"
          onClick={() => onReply(message)}
        >
          回复
        </button>
      ) : null}
      {message.attention && (
        <div className="attention-actions">
          <span role="status">
            {message.attention.state === "delivered"
              ? "Agent 已接续"
              : message.attention.state === "answered"
                ? "决定已转交"
                : "待你决定"}
          </span>
          {message.attention.state === "awaiting_user" && (
            <button
              className="text-button"
              type="button"
              onClick={() => onReply(message)}
            >
              回复
            </button>
          )}
        </div>
      )}
      {pending && <span className="message-state">已收到</span>}
      {message.router_error && (
        <span className="message-state error">{message.router_error}</span>
      )}
      {message.artifacts?.map((file) => (
        <button
          className="download"
          key={file.id}
          type="button"
          disabled={!file.available || downloading !== null}
          onClick={() => void saveFile(file)}
        >
          {!file.available
            ? `${file.name} · 正在同步`
            : downloading === file.id
              ? `正在下载 ${file.name}…`
              : `下载 ${file.name}`}
        </button>
      ))}
      {error && (
        <span role="alert" className="error">
          {error}
        </span>
      )}
    </li>
  );
}

function Composer({
  token,
  issue,
  chatId,
  replyId,
  replyText,
  textarea,
  onCancelReply,
  onSent,
}: {
  token: string;
  issue?: string;
  chatId: string;
  replyId: string | null;
  replyText?: string;
  textarea: RefObject<HTMLTextAreaElement | null>;
  onCancelReply: () => void;
  onSent: (reply: string | null) => void;
}) {
  const [pending] = useState(() => savedMessage(chatId, issue));
  const attempt = useRef(pending);
  const [text, setText] = useState(pending?.text ?? "");
  const [sending, setSending] = useState(false);
  const [error, setError] = useState("");

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const value = text.trim();
    if (!value || sending) return;
    const quote = replyId ?? undefined;
    const message =
      attempt.current?.text === value && attempt.current.reply_to === quote
        ? attempt.current
        : { operation_id: operationId(), text: value, reply_to: quote };
    attempt.current = message;
    sessionStorage.setItem(pendingKey(chatId, issue), JSON.stringify(message));
    setSending(true);
    setError("");
    try {
      await sendMessage(token, message, issue, chatId);
      sessionStorage.removeItem(pendingKey(chatId, issue));
      attempt.current = null;
      setText("");
      onSent(replyId);
    } catch {
      setError("暂时无法确认收件，再次发送会沿原消息重试。");
    } finally {
      setSending(false);
    }
  }

  return (
    <footer className="dock">
      <div className="lane">
        {error && (
          <p className="error composer-error" role="alert">
            {error}
          </p>
        )}
        <form className="composer" onSubmit={submit}>
          {replyId && (
            <div className="composer-reply">
              <span>回复：{replyText ?? "原问题"}</span>
              <button
                className="text-button"
                type="button"
                onClick={onCancelReply}
                disabled={sending}
              >
                取消引用
              </button>
            </div>
          )}
          <textarea
            ref={textarea}
            aria-label={issue ? "发消息给 Issue Owner" : "发消息给 Tend"}
            placeholder={issue ? "发消息给 Issue Owner…" : "发消息给 Tend…"}
            value={text}
            rows={2}
            readOnly={sending}
            onChange={(event) => setText(event.target.value)}
            onKeyDown={(event) => {
              if (
                event.key === "Enter" &&
                !event.shiftKey &&
                !event.nativeEvent.isComposing
              ) {
                event.preventDefault();
                event.currentTarget.form?.requestSubmit();
              }
            }}
          />
          <div className="compose-actions">
            <span>Enter 发送 · Shift+Enter 换行</span>
            <button
              type="submit"
              className="send"
              aria-label="发送"
              disabled={sending || !text.trim()}
            >
              <svg
                aria-hidden="true"
                viewBox="0 0 20 20"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.65"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                <path d="M10 15.5v-11m-4.5 4.5L10 4.5 14.5 9" />
              </svg>
            </button>
          </div>
        </form>
      </div>
    </footer>
  );
}

export function Chat({
  token,
  issue,
  chatId,
  onReachable,
  detail,
}: {
  token: string;
  issue?: string;
  chatId: string;
  onReachable: (online: boolean) => void;
  detail?: {
    ready: boolean;
    header: ReactNode;
    work: ReactNode;
    properties: ReactNode;
  };
}) {
  const [messages, setMessages] = useState<Message[]>([]);
  const [sourceChat, setSourceChat] = useState(chatId);
  const [canonicalIssue, setCanonicalIssue] = useState<string>();
  const issueLoaded = useRef(false);
  const numericRoute = issue !== undefined && /^#?[1-9][0-9]*$/.test(issue);
  const [awaitingStart, setAwaitingStart] = useState(false);
  const [before, setBefore] = useState<number | null | undefined>(undefined);
  const [loadingHistory, setLoadingHistory] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const [error, setError] = useState("");
  const [historyError, setHistoryError] = useState("");
  const [replyId, setReplyId] = useState(
    () => savedMessage(chatId, issue)?.reply_to ?? null,
  );
  const textarea = useRef<HTMLTextAreaElement>(null);
  const timeline = useRef<HTMLElement>(null);
  const follow = useRef(!detail);
  const [anchor, setAnchor] = useState(messageAnchor);
  const handledAnchor = useRef("");
  const attemptedAnchor = useRef("");
  const [seekingAnchor, setSeekingAnchor] = useState(false);
  const readyForAnchor = detail?.ready ?? true;
  const historyRequest = useRef<AbortController | null>(null);
  const preserve = useRef<{ top: number; height: number } | null>(null);

  useEffect(() => {
    function followHash() {
      handledAnchor.current = "";
      attemptedAnchor.current = "";
      historyRequest.current?.abort();
      setLoadingHistory(false);
      setSeekingAnchor(false);
      setHistoryError("");
      setAnchor(messageAnchor());
    }
    window.addEventListener("hashchange", followHash);
    return () => {
      window.removeEventListener("hashchange", followHash);
      historyRequest.current?.abort();
    };
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    const { signal } = controller;
    let timer: number;
    async function update() {
      try {
        const page = await readMessages(
          token,
          undefined,
          signal,
          issue,
          chatId,
        );
        if (signal.aborted) return;
        setMessages((current) => mergeMessages(current, page.messages));
        setBefore((cursor) =>
          cursor === undefined ? page.next_before : cursor,
        );
        setError("");
        if (issue && !issueLoaded.current) {
          issueLoaded.current = true;
          const id = page.issue_id ?? issue;
          setCanonicalIssue(id);
          if (id !== issue) {
            const savedReply = savedMessage(chatId, id)?.reply_to ?? null;
            setReplyId((current) => current ?? savedReply);
          }
        }
        setSourceChat(page.chat_id || chatId);
        setAwaitingStart(page.awaiting_start === true);
        onReachable(true);
      } catch (error) {
        if (!signal.aborted) {
          setError(errorText(error));
          onReachable(false);
        }
      }
      if (!signal.aborted) timer = window.setTimeout(() => void update(), 1500);
    }
    void update();
    return () => {
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [token, issue, chatId, refresh, onReachable]);

  useLayoutEffect(() => {
    const element = timeline.current;
    if (!element) return;
    if (anchor && handledAnchor.current !== anchor && readyForAnchor) {
      const target = document.getElementById(`message-${anchor}`);
      if (target) {
        handledAnchor.current = anchor;
        follow.current = false;
        preserve.current = null;
        target.scrollIntoView({ block: "center" });
        return;
      }
    }
    if (preserve.current) {
      element.scrollTop =
        preserve.current.top + element.scrollHeight - preserve.current.height;
      preserve.current = null;
    } else if (
      follow.current &&
      (!anchor || handledAnchor.current === anchor)
    ) {
      element.scrollTop = element.scrollHeight;
    }
  }, [messages, readyForAnchor, anchor]);

  useEffect(() => {
    if (
      !anchor ||
      !readyForAnchor ||
      before === undefined ||
      handledAnchor.current === anchor ||
      attemptedAnchor.current === anchor
    )
      return;
    const controller = new AbortController();
    const { signal } = controller;

    async function seek() {
      // Let a hash change or an unmount cancel the search before it begins.
      await Promise.resolve();
      if (signal.aborted) return;
      attemptedAnchor.current = anchor;
      setSeekingAnchor(true);
      let cursor = before ?? null;
      const history: Message[][] = [];
      let found = false;
      try {
        while (cursor !== null) {
          const page = await readMessages(token, cursor, signal, issue, chatId);
          if (signal.aborted) return;
          history.push(page.messages);
          cursor = page.next_before;
          if (page.messages.some((message) => message.id === anchor)) {
            found = true;
            break;
          }
        }
        if (!found) setHistoryError("没有找到链接指向的原消息。");
      } catch (error) {
        if (!signal.aborted) setHistoryError(errorText(error));
      }
      if (signal.aborted) return;
      if (history.length > 0) {
        const linkedHistory = history.reverse().flat();
        setMessages((current) => mergeMessages(current, linkedHistory, true));
        setBefore(cursor);
      }
      setSeekingAnchor(false);
    }

    void seek();
    return () => controller.abort();
  }, [anchor, readyForAnchor, before, token, issue, chatId]);

  async function loadEarlier() {
    if (before == null || loadingHistory || seekingAnchor) return;
    const controller = new AbortController();
    const { signal } = controller;
    historyRequest.current = controller;
    setLoadingHistory(true);
    setHistoryError("");
    try {
      const page = await readMessages(token, before, signal, issue, chatId);
      if (signal.aborted) return;
      if (timeline.current) {
        preserve.current = {
          top: timeline.current.scrollTop,
          height: timeline.current.scrollHeight,
        };
      }
      setBefore(page.next_before);
      setMessages((current) => mergeMessages(current, page.messages, true));
    } catch (error) {
      if (!signal.aborted) setHistoryError(errorText(error));
    } finally {
      if (!signal.aborted) setLoadingHistory(false);
    }
  }

  const byId = new Map(messages.map((message) => [message.id, message]));
  let lastUser: string | null = null;
  const rows = messages.map((message) => {
    const detail = message.issue;
    const descendant =
      !!detail && detail.issue_id !== (canonicalIssue ?? issue);
    if (detail?.kind === "run" && detail.run_id) {
      return (
        <RunRow
          key={message.id}
          id={message.id}
          token={token}
          run={detail.run_id}
          author={detail.author}
          issue={
            descendant
              ? issueTitle(detail.issue_title, detail.short_number)
              : undefined
          }
        />
      );
    }
    const source = message.reply_to ? byId.get(message.reply_to) : undefined;
    const showReference = Boolean(issue) || message.reply_to !== lastUser;
    if (message.author === "user") lastUser = message.id;
    return (
      <MessageRow
        key={message.id}
        message={message}
        source={source}
        showReference={showReference}
        sourceChat={sourceChat}
        descendant={descendant}
        token={token}
        onReply={(question) => {
          setReplyId(question.id);
          textarea.current?.focus({ preventScroll: true });
        }}
      />
    );
  });

  const discussion = (
    <>
      {detail && (
        <div className="detail-section-heading">
          <h2 id="issue-discussion">Issue 对话与工作记录</h2>
        </div>
      )}
      {detail?.work}
      {(before != null || seekingAnchor) && (
        <button
          className="text-button older-messages"
          type="button"
          disabled={loadingHistory || seekingAnchor}
          onClick={() => void loadEarlier()}
        >
          {seekingAnchor
            ? "正在定位原消息…"
            : loadingHistory
              ? "正在读取…"
              : "读取更早的消息"}
        </button>
      )}
      {(error || historyError) && (
        <p className="error" role="alert">
          {error || historyError}
        </p>
      )}
      <ol aria-label="对话记录" className="messages">
        {rows}
      </ol>
    </>
  );

  return (
    <>
      <main
        ref={timeline}
        className="timeline"
        onScroll={(event) => {
          const view = event.currentTarget;
          follow.current =
            view.scrollHeight - view.scrollTop - view.clientHeight < 36;
        }}
      >
        {detail ? (
          <div className="issue-detail">
            <div className="issue-detail-summary">{detail.header}</div>
            <section
              className="issue-discussion"
              aria-labelledby="issue-discussion"
            >
              {discussion}
            </section>
            {detail.properties && (
              <aside
                id="issue-properties"
                aria-label="工作属性"
                className="issue-properties"
              >
                {detail.properties}
              </aside>
            )}
          </div>
        ) : (
          <div className="lane chat">{discussion}</div>
        )}
      </main>
      {awaitingStart ? (
        <footer className="dock">
          <p className="lane board-empty">
            暂无 Issue Owner。可在<a href={chatLink(sourceChat)}>原对话</a>
            中安排工作。
          </p>
        </footer>
      ) : !numericRoute || canonicalIssue ? (
        <Composer
          token={token}
          issue={canonicalIssue ?? issue}
          chatId={chatId}
          replyId={replyId}
          replyText={replyId ? byId.get(replyId)?.text : undefined}
          textarea={textarea}
          onCancelReply={() => setReplyId(null)}
          onSent={(sentReply) => {
            follow.current = true;
            setReplyId((current) => (current === sentReply ? null : current));
            setRefresh((number) => number + 1);
          }}
        />
      ) : null}
    </>
  );
}
