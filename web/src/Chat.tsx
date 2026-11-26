import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useEffectEvent,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import type { FormEvent, ReactNode, RefObject } from "react";
import {
  chatLink,
  download,
  issueLink,
  issueTitle,
  mergeMessages,
  operationId,
  readAttentionStatus,
  readMessages,
  readRollout,
  sendMessage,
} from "./api";
import type { Artifact, AttentionStatus, Message, PendingMessage } from "./api";
import { ChatProposal, DecisionTime } from "./ProposalApproval";
import { DiscussionStart } from "./DiscussionStart";
import { discussionLink } from "./discussionApi";
import { DispatchAuthor, IssueTakeoverEvent } from "./IssueTimelineEvent";
const DiscussionMarkdown = lazy(() =>
  import("./Markdown").then(({ Markdown }) => ({ default: Markdown })),
);

function OrdinaryChatAttention({
  chat,
  initial,
  historical,
  onReply,
}: {
  chat: string;
  initial: AttentionStatus;
  historical: boolean;
  onReply: () => void;
}) {
  const [fresh, setFresh] = useState<AttentionStatus>();
  const [visible, setVisible] = useState(false);
  const row = useRef<HTMLDivElement>(null);
  const status = historical ? (fresh ?? initial) : initial;
  const { id, state, issue_reply: issueReply } = status;
  const watch =
    historical && (state === "awaiting_user" || state === "answered");
  useEffect(() => {
    if (!watch || !row.current) return;
    const observer = new IntersectionObserver(([entry]) =>
      setVisible(entry.isIntersecting),
    );
    observer.observe(row.current);
    return () => observer.disconnect();
  }, [watch]);

  useEffect(() => {
    if (!watch || !visible) return;
    const controller = new AbortController();
    const { signal } = controller;
    let timer: number;
    async function update() {
      try {
        const current = await readAttentionStatus(chat, id, signal);
        if (signal.aborted) return;
        setFresh(current);
        if (current.state === "delivered") return;
      } catch {
        // Keep the last known status while the read is unavailable.
      }
      if (!signal.aborted) timer = window.setTimeout(() => void update(), 5000);
    }
    void update();
    return () => {
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [chat, id, watch, visible]);

  return (
    <div ref={row}>
      <div className="attention-actions">
        <span role="status">
          {state === "delivered"
            ? "Agent 已接续"
            : state === "answered"
              ? issueReply === undefined
                ? "决定已转交"
                : "答复已记录"
              : "待你决定"}
        </span>
        {state === "awaiting_user" && (
          <button className="text-button" type="button" onClick={onReply}>
            回复
          </button>
        )}
      </div>
      {issueReply !== undefined && (
        <div className="attention-recorded">
          <span>在 Issue 页答复</span>
          <p>{issueReply}</p>
        </div>
      )}
    </div>
  );
}

function pendingKey(chat: string, issue?: string) {
  if (issue) return `tend.pending-issue-message.v1:${issue}`;
  return chat === "main"
    ? "tend.pending-message.v1"
    : `tend.pending-message.v1:${chat}`;
}

type SendOutcome = "received" | "retry" | "retry-unsaved";
type ActiveSend = { message: PendingMessage; receipt: Promise<SendOutcome> };
type FailedSend = {
  message: PendingMessage;
  failure: Exclude<SendOutcome, "received">;
};

// Preserve the original operation even if the composer is replaced and
// browser storage is unavailable. A new explicit send replaces this record.
const unconfirmedSends = new Map<string, ActiveSend | FailedSend>();

function retryMessage(outcome: FailedSend["failure"]) {
  return outcome === "retry"
    ? "暂时无法确认收件，再次发送会沿原消息重试。"
    : "暂时无法确认收件。浏览器未保存草稿，刷新或关闭标签前可沿原消息重试。";
}

function savedMessage(chat: string, issue?: string): PendingMessage | null {
  const key = pendingKey(chat, issue);
  const unconfirmed = unconfirmedSends.get(key);
  if (unconfirmed) return unconfirmed.message;
  try {
    const value = sessionStorage.getItem(key);
    return value ? (JSON.parse(value) as PendingMessage) : null;
  } catch {
    return null;
  }
}

function startMessageSend(
  key: string,
  message: PendingMessage,
  issue: string | undefined,
  chat: string,
): ActiveSend {
  let saved = true;
  try {
    sessionStorage.setItem(key, JSON.stringify(message));
  } catch {
    saved = false;
  }
  const receipt = sendMessage(message, issue, chat).then<
    SendOutcome,
    SendOutcome
  >(
    () => {
      unconfirmedSends.delete(key);
      try {
        sessionStorage.removeItem(key);
      } catch {
        // The Server already accepted the message.
      }
      return "received";
    },
    () => {
      const failure = saved ? "retry" : "retry-unsaved";
      unconfirmedSends.set(key, { message, failure });
      return failure;
    },
  );
  const delivery = { message, receipt };
  unconfirmedSends.set(key, delivery);
  return delivery;
}

function errorText(error: unknown) {
  return error instanceof Error ? error.message : "暂时无法连接 Tend";
}

function messageAnchor() {
  return /^#message-([a-zA-Z0-9_-]+)$/.exec(window.location.hash)?.[1] ?? "";
}

function RunRow({
  id,
  run,
  author,
  issue,
}: {
  id: string;
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
    void readRollout(run, signal).then(
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
  onReply,
  onRefresh,
  historical,
  canStartDiscussion,
}: {
  message: Message;
  source?: Message;
  showReference: boolean;
  sourceChat: string;
  descendant: boolean;
  onReply: (message: Message) => void;
  onRefresh: () => void;
  historical: boolean;
  canStartDiscussion: boolean;
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
      await download(file);
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
        {detail?.kind === "dispatch" ? (
          <DispatchAuthor provenance={detail.dispatch_provenance} />
        ) : (
          <span className="author">
            {user
              ? "你"
              : (detail?.author ??
                (message.author === "discussion" ? "讨论助手" : "Tend"))}
          </span>
        )}
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
      {!message.attention?.proposal ? (
        message.author === "discussion" ? (
          <Suspense fallback={<p>{message.text}</p>}>
            <DiscussionMarkdown text={message.text} />
          </Suspense>
        ) : (
          <p>{message.text}</p>
        )
      ) : (
        (message.text !== message.attention.proposal.question ||
          message.attention.proposal.decision) && (
          <details className="proposal-original-question">
            <summary>原提问</summary>
            <p>{message.text}</p>
          </details>
        )
      )}
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
      {message.attention?.proposal ? (
        <ChatProposal
          historical={historical}
          chat={sourceChat}
          attention={message.attention.id}
          initial={message.attention.proposal}
          onRefresh={onRefresh}
        />
      ) : (
        message.attention && (
          <OrdinaryChatAttention
            historical={historical}
            chat={sourceChat}
            initial={message.attention}
            onReply={() => onReply(message)}
          />
        )
      )}
      {message.discussion_id && (
        <a className="issue-link" href={discussionLink(message.discussion_id)}>
          回到原讨论 ↗
        </a>
      )}
      {canStartDiscussion && !message.attention && !message.discussion_id && (
        <DiscussionStart
          compact
          origin={{ kind: "chat", chat_id: "main", message_id: message.id }}
        />
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

type Standalone = {
  renderMention?: (query: string, done: () => void) => ReactNode;
};

function Composer({
  issue,
  chatId,
  replyId,
  replyText,
  textarea,
  onCancelReply,
  onSent,
  standalone,
}: {
  issue?: string;
  chatId: string;
  replyId: string | null;
  replyText?: string;
  textarea: RefObject<HTMLTextAreaElement | null>;
  onCancelReply: () => void;
  onSent: (reply: string | null) => void;
  standalone?: Standalone;
}) {
  const key = pendingKey(chatId, issue);
  const [pending] = useState(() => savedMessage(chatId, issue));
  const attempt = useRef(pending);
  const [text, setText] = useState(pending?.text ?? "");
  const [delivery, setDelivery] = useState<ActiveSend | undefined>(() => {
    const record = unconfirmedSends.get(key);
    return record && "receipt" in record ? record : undefined;
  });
  const sending = delivery !== undefined;
  const [error, setError] = useState(() => {
    const record = unconfirmedSends.get(key);
    return record && "failure" in record ? retryMessage(record.failure) : "";
  });

  const mention = standalone && /(?:^|\s)@([^\s@]*)$/.exec(text);

  const finishSend = useEffectEvent(
    (outcome: SendOutcome, sent: PendingMessage) => {
      if (outcome === "received") {
        attempt.current = null;
        setText("");
        onSent(sent.reply_to ?? null);
      } else {
        setError(retryMessage(outcome));
      }
      setDelivery(undefined);
    },
  );

  useEffect(() => {
    if (!delivery) return;
    let mounted = true;
    void delivery.receipt.then((outcome) => {
      if (mounted) finishSend(outcome, delivery.message);
    });
    return () => {
      mounted = false;
    };
  }, [delivery]);

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const value = text.trim();
    const record = unconfirmedSends.get(key);
    if (!value || sending || (record && "receipt" in record)) return;
    const quote = replyId ?? undefined;
    const message =
      attempt.current?.text === value && attempt.current.reply_to === quote
        ? attempt.current
        : { operation_id: operationId(), text: value, reply_to: quote };
    attempt.current = message;
    setError("");
    setDelivery(startMessageSend(key, message, issue, chatId));
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
            aria-label={
              issue
                ? "发消息给 Issue Owner"
                : standalone
                  ? "在讨论中发消息"
                  : "发消息给 Tend"
            }
            placeholder={
              issue
                ? "发消息给 Issue Owner…"
                : standalone
                  ? "发消息，或输入 @ 附加资料…"
                  : "发消息给 Tend…"
            }
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
          {mention &&
            standalone?.renderMention?.(mention[1], () => {
              setText((value) => value.replace(/(^|\s)@[^\s@]*$/, "$1"));
              textarea.current?.focus();
            })}
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
  issue,
  chatId,
  onReachable,
  detail,
  standalone,
}: {
  issue?: string;
  chatId: string;
  onReachable: (online: boolean) => void;
  standalone?: Standalone;
  detail?: {
    overviewLoaded: boolean;
    header: ReactNode;
    work: ReactNode;
    properties: ReactNode;
    refresh?: number;
    structured?: boolean;
  };
}) {
  const [messages, setMessages] = useState<Message[]>([]);
  const [latestIds, setLatestIds] = useState(() => new Set<string>());
  const [sourceChat, setSourceChat] = useState(chatId);
  const [canonicalIssue, setCanonicalIssue] = useState<string>();
  const issueLoaded = useRef(false);
  const numericRoute = issue !== undefined && /^#?[1-9][0-9]*$/.test(issue);
  const [awaitingStart, setAwaitingStart] = useState(false);
  const [before, setBefore] = useState<number | null | undefined>(undefined);
  const [historyMode, setHistoryMode] = useState<"manual" | "anchor" | null>(
    null,
  );
  const [refresh, setRefresh] = useState(0);
  const [error, setError] = useState("");
  const [historyError, setHistoryError] = useState("");
  const [anchorError, setAnchorError] = useState("");
  const [replyId, setReplyId] = useState(
    () => savedMessage(chatId, issue)?.reply_to ?? null,
  );
  const textarea = useRef<HTMLTextAreaElement>(null);
  const timeline = useRef<HTMLElement>(null);
  const follow = useRef(!detail);
  const [anchor, setAnchor] = useState(messageAnchor);
  const handledAnchor = useRef("");
  const seekingAnchor = historyMode === "anchor";
  const readyForAnchor = detail?.overviewLoaded ?? true;
  const detailRefresh = detail?.refresh;
  const historyRequest = useRef<AbortController | null>(null);
  const preserve = useRef<{ top: number; height: number } | null>(null);

  useEffect(() => {
    function cancelHistory() {
      historyRequest.current?.abort();
      historyRequest.current = null;
    }
    function followHash() {
      handledAnchor.current = "";
      cancelHistory();
      setHistoryMode(null);
      setHistoryError("");
      setAnchorError("");
      setAnchor(messageAnchor());
    }
    window.addEventListener("hashchange", followHash);
    return () => {
      window.removeEventListener("hashchange", followHash);
      cancelHistory();
    };
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    const { signal } = controller;
    let timer: number;
    async function update() {
      try {
        const page = await readMessages(undefined, signal, issue, chatId);
        if (signal.aborted) return;
        setMessages((current) => mergeMessages(current, page.messages));
        if (!issue) setLatestIds(new Set(page.messages.map(({ id }) => id)));
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
  }, [issue, chatId, refresh, detailRefresh, onReachable]);

  useLayoutEffect(() => {
    const element = timeline.current;
    if (!element) return;
    if (anchor && handledAnchor.current !== anchor && readyForAnchor) {
      const target = document.getElementById(`message-${anchor}`);
      if (target) {
        handledAnchor.current = anchor;
        setAnchorError("");
        follow.current = false;
        preserve.current = null;
        for (
          let parent = target.parentElement;
          parent;
          parent = parent.parentElement
        ) {
          if (parent instanceof HTMLDetailsElement) parent.open = true;
        }
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

  const loadHistory = useCallback(
    async (before: number, target?: string) => {
      // Manual loads and anchor searches exclusively own the same cursor.
      if (historyRequest.current) return;
      const controller = new AbortController();
      const { signal } = controller;
      historyRequest.current = controller;
      setHistoryMode(target ? "anchor" : "manual");
      if (!target) setHistoryError("");
      let cursor: number | null = before;
      const history: Message[][] = [];
      try {
        while (cursor !== null) {
          const page = await readMessages(cursor, signal, issue, chatId);
          if (signal.aborted) return;
          history.push(page.messages);
          cursor = page.next_before;
          if (
            !target ||
            handledAnchor.current === target ||
            page.messages.some((message) => message.id === target)
          ) {
            break;
          }
        }
      } catch (error) {
        if (!signal.aborted) {
          if (target && handledAnchor.current !== target) {
            setAnchorError(errorText(error));
          } else if (!target) {
            setHistoryError(errorText(error));
          }
        }
      } finally {
        if (historyRequest.current === controller) {
          if (history.length > 0) {
            if (!target && timeline.current) {
              preserve.current = {
                top: timeline.current.scrollTop,
                height: timeline.current.scrollHeight,
              };
            }
            const earlier = history.reverse().flat();
            setMessages((current) => mergeMessages(current, earlier, true));
            setBefore(cursor);
          }
          historyRequest.current = null;
          setHistoryMode(null);
        }
      }
    },
    [issue, chatId],
  );

  useEffect(() => {
    if (
      !anchor ||
      !readyForAnchor ||
      before === undefined ||
      handledAnchor.current === anchor ||
      historyMode ||
      anchorError
    )
      return;
    if (before === null) {
      setAnchorError("没有找到链接指向的原消息。");
    } else {
      void loadHistory(before, anchor);
    }
  }, [anchor, readyForAnchor, before, historyMode, anchorError, loadHistory]);

  const byId = new Map(messages.map((message) => [message.id, message]));
  let lastUser: string | null = null;
  const rows = messages.map((message) => {
    if (message.approval) {
      const { decision, issue_id, report_title } = message.approval;
      return (
        <li
          className="approval-event"
          id={`message-${message.id}`}
          key={message.id}
        >
          <span className="approval-icon" aria-hidden="true">
            ✓
          </span>
          <strong>你批准了 v{decision.version}</strong>
          <span className="detail-muted">{report_title}</span>
          <DecisionTime time={decision.decided_at} />
          {issue_id !== (canonicalIssue ?? issue) && (
            <a href={issueLink(issue_id)}>来源 Issue</a>
          )}
        </li>
      );
    }
    const detail = message.issue;
    const descendant =
      !!detail && detail.issue_id !== (canonicalIssue ?? issue);
    if (detail?.kind === "takeover") {
      return (
        <IssueTakeoverEvent
          key={message.id}
          message={detail}
          descendant={descendant}
        />
      );
    }
    if (detail?.kind === "run" && detail.run_id) {
      return (
        <RunRow
          key={message.id}
          id={message.id}
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
        onRefresh={() => setRefresh((value) => value + 1)}
        historical={!latestIds.has(message.id)}
        canStartDiscussion={!standalone && !issue && chatId === "main"}
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
          <h2 id="issue-discussion">
            {detail.structured ? "Issue 对话" : "Issue 对话与工作记录"}
          </h2>
        </div>
      )}
      {detail?.work}
      {(before != null || seekingAnchor) && (
        <button
          className="text-button older-messages"
          type="button"
          disabled={historyMode !== null}
          onClick={() => {
            if (before != null && !historyMode) void loadHistory(before);
          }}
        >
          {seekingAnchor
            ? "正在定位原消息…"
            : historyMode === "manual"
              ? "正在读取…"
              : "读取更早的消息"}
        </button>
      )}
      {(error || historyError || anchorError) && (
        <p className="error" role="alert">
          {error || historyError || anchorError}
        </p>
      )}
      {standalone && before !== undefined && !messages.length && (
        <div className="discussion-blank">
          <span aria-hidden="true">✳</span>
          <h3>可以直接开聊</h3>
          <p>发第一条消息；如果需要背景，可以随时添上参考资料。</p>
        </div>
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
          <div className="issue-detail" data-proposal={detail.structured}>
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
          issue={canonicalIssue ?? issue}
          chatId={chatId}
          replyId={replyId}
          replyText={replyId ? byId.get(replyId)?.text : undefined}
          textarea={textarea}
          standalone={standalone}
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
