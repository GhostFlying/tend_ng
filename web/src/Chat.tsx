import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { FormEvent, RefObject } from "react";
import { download, mergeMessages, readMessages, sendMessage } from "./api";
import type { Artifact, Message, PendingMessage } from "./api";

const pendingKey = "tend.pending-message.v1";

function savedMessage(): PendingMessage | null {
  const value = sessionStorage.getItem(pendingKey);
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

function MessageRow({
  message,
  source,
  token,
  onReply,
}: {
  message: Message;
  source?: Message;
  token: string;
  onReply: (message: Message) => void;
}) {
  const [error, setError] = useState("");
  const [downloading, setDownloading] = useState<string | null>(null);
  const user = message.author === "user";
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
      <span className="author">{user ? "你" : "Tend"}</span>
      {source && (
        <a className="reference" href={`#message-${source.id}`}>
          {source.text}
        </a>
      )}
      <p>{message.text}</p>
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
  replyId,
  replyText,
  textarea,
  onCancelReply,
  onSent,
}: {
  token: string;
  replyId: string | null;
  replyText?: string;
  textarea: RefObject<HTMLTextAreaElement | null>;
  onCancelReply: () => void;
  onSent: (reply: string | null) => void;
}) {
  const [pending] = useState(savedMessage);
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
    sessionStorage.setItem(pendingKey, JSON.stringify(message));
    setSending(true);
    setError("");
    try {
      await sendMessage(token, message);
      sessionStorage.removeItem(pendingKey);
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
            aria-label="发消息给 Tend"
            placeholder="发消息给 Tend…"
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
  onReachable,
}: {
  token: string;
  onReachable: (online: boolean) => void;
}) {
  const [messages, setMessages] = useState<Message[]>([]);
  const [before, setBefore] = useState<number | null | undefined>(undefined);
  const [loadingHistory, setLoadingHistory] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const [error, setError] = useState("");
  const [historyError, setHistoryError] = useState("");
  const [replyId, setReplyId] = useState(
    () => savedMessage()?.reply_to ?? null,
  );
  const textarea = useRef<HTMLTextAreaElement>(null);
  const timeline = useRef<HTMLElement>(null);
  const follow = useRef(true);
  const preserve = useRef<{ top: number; height: number } | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    let timer: number;
    async function update() {
      try {
        const page = await readMessages(token, undefined, controller.signal);
        if (controller.signal.aborted) return;
        setMessages((current) => mergeMessages(current, page.messages));
        setBefore((cursor) =>
          cursor === undefined ? page.next_before : cursor,
        );
        setError("");
        onReachable(true);
      } catch (error) {
        if (!controller.signal.aborted) {
          setError(errorText(error));
          onReachable(false);
        }
      }
      if (!controller.signal.aborted)
        timer = window.setTimeout(() => void update(), 1500);
    }
    void update();
    return () => {
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [token, refresh, onReachable]);

  useLayoutEffect(() => {
    const element = timeline.current;
    if (!element) return;
    if (preserve.current) {
      element.scrollTop =
        preserve.current.top + element.scrollHeight - preserve.current.height;
      preserve.current = null;
    } else if (follow.current) {
      element.scrollTop = element.scrollHeight;
    }
  }, [messages]);

  async function loadEarlier() {
    if (before == null || loadingHistory) return;
    setLoadingHistory(true);
    setHistoryError("");
    try {
      const page = await readMessages(token, before);
      if (timeline.current) {
        preserve.current = {
          top: timeline.current.scrollTop,
          height: timeline.current.scrollHeight,
        };
      }
      setBefore(page.next_before);
      setMessages((current) => mergeMessages(current, page.messages, true));
    } catch (error) {
      setHistoryError(errorText(error));
    } finally {
      setLoadingHistory(false);
    }
  }

  const byId = new Map(messages.map((message) => [message.id, message]));
  let lastUser: string | null = null;
  const rows = messages.map((message) => {
    if (message.author === "user") lastUser = message.id;
    const source =
      message.reply_to && message.reply_to !== lastUser
        ? byId.get(message.reply_to)
        : undefined;
    return (
      <MessageRow
        key={message.id}
        message={message}
        source={source}
        token={token}
        onReply={(question) => {
          setReplyId(question.id);
          textarea.current?.focus({ preventScroll: true });
        }}
      />
    );
  });

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
        <div className="lane chat">
          {before != null && (
            <button
              className="text-button"
              type="button"
              disabled={loadingHistory}
              onClick={() => void loadEarlier()}
            >
              {loadingHistory ? "正在读取…" : "读取更早的消息"}
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
        </div>
      </main>
      <Composer
        token={token}
        replyId={replyId}
        replyText={replyId ? byId.get(replyId)?.text : undefined}
        textarea={textarea}
        onCancelReply={() => setReplyId(null)}
        onSent={(sentReply) => {
          setReplyId((current) => (current === sentReply ? null : current));
          setRefresh((number) => number + 1);
        }}
      />
    </>
  );
}
