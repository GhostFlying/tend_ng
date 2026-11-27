import { useCallback, useEffect, useRef, useState } from "react";
import type { FormEvent, ReactNode } from "react";
import { chatLink, issueTitle, operationId } from "./api";
import {
  DiscussionError,
  readDraft,
  readQuickHandoff,
  referenceStatus,
  requestDraft,
  sendDraft,
  submitQuickHandoff,
} from "./discussionApi";
import type {
  Draft,
  DraftRequest,
  IssueChoice,
  QuickHandoff,
  QuickHandoffAction,
  Reference,
  SendDraft,
} from "./discussionApi";
import { discussionError, useReferenceSearch } from "./discussionSearch";

const labels: [keyof Draft, string][] = [
  ["goal", "目标"],
  ["decisions", "已明确决策"],
  ["pending_items", "待定事项"],
  ["constraints", "约束"],
  ["authorization_end", "授权终点"],
];

function cached<T>(key: string): T | undefined {
  try {
    const value = sessionStorage.getItem(key);
    return value ? (JSON.parse(value) as T) : undefined;
  } catch {
    return undefined;
  }
}

function save(key: string, value: unknown) {
  try {
    if (value === null) sessionStorage.removeItem(key);
    else sessionStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* The original operation remains in memory in this tab. */
  }
}

export function QuickHandoffControl({
  chat,
  revision,
  onSent,
  render,
}: {
  chat: string;
  revision: number;
  onSent: () => void;
  render: (controls: { action: ReactNode; status: ReactNode }) => ReactNode;
}) {
  const key = `tend.discussion.quick.v2.${chat}`;
  const [tracked, setTracked] = useState(
    () => cached<QuickHandoffAction[]>(key) ?? [],
  );
  const trackedRef = useRef(tracked);
  const [current, setCurrent] = useState<QuickHandoff>();
  const [older, setOlder] = useState<QuickHandoff[]>([]);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState<string>();
  const posting = useRef(false);
  const message = current?.request?.message_id;

  const retain = useCallback(
    (operations: QuickHandoffAction[]) => {
      trackedRef.current = operations;
      save(key, operations.length ? operations : null);
      setTracked(operations);
    },
    [key],
  );

  useEffect(() => {
    if (message) onSent();
  }, [message, onSent]);

  useEffect(() => {
    const controller = new AbortController();
    let timer: number | undefined;
    async function poll() {
      let delay = 5000;
      try {
        if (posting.current) {
          delay = 850;
          return;
        }
        const [latest, ...previous] = await Promise.allSettled([
          readQuickHandoff(chat, controller.signal),
          ...tracked.map(({ operation_id }) =>
            readQuickHandoff(chat, controller.signal, operation_id),
          ),
        ]);
        if (controller.signal.aborted || posting.current) return;
        if (latest.status === "fulfilled") setCurrent(latest.value);
        const previousRecords = previous.flatMap((result) =>
          result.status === "fulfilled" ? [result.value] : [],
        );
        setOlder(previousRecords);
        setError(
          latest.status === "rejected" ||
            previous.some((result) => result.status === "rejected")
            ? "暂时无法查证转交状态。"
            : "",
        );
        const sent = new Set(
          previousRecords
            .filter(({ request }) => request?.status === "sent")
            .map(({ operation_id }) => operation_id),
        );
        if (sent.size) {
          retain(tracked.filter(({ operation_id }) => !sent.has(operation_id)));
          onSent();
        }
        if (
          previousRecords.some(({ request }) => request?.status === "queued") ||
          (latest.status === "fulfilled" &&
            latest.value.request?.status === "queued")
        )
          delay = 850;
      } catch {
        if (!controller.signal.aborted && !posting.current)
          setError("暂时无法查证转交状态。");
      } finally {
        if (!controller.signal.aborted)
          timer = window.setTimeout(() => void poll(), delay);
      }
    }
    void poll();
    return () => {
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [chat, tracked, revision, retain, onSent]);

  async function submit(record: QuickHandoff) {
    const { operation_id, request } = record;
    if (
      posting.current ||
      request?.status === "queued" ||
      request?.status === "sent"
    )
      return;
    const prior = trackedRef.current.find(
      (action) => action.operation_id === operation_id,
    );
    const action = {
      operation_id,
      retry_generation:
        request?.status === "failed"
          ? request.generation
          : prior?.retry_generation,
    };
    const without = () =>
      trackedRef.current.filter(
        (action) => action.operation_id !== operation_id,
      );
    posting.current = true;
    retain([...without(), action]);
    setBusy(operation_id);
    setError("");
    try {
      const result = await submitQuickHandoff(chat, action);
      setCurrent((curr) =>
        curr?.operation_id === operation_id ? result : curr,
      );
      setOlder((old) => [
        ...old.filter((row) => row.operation_id !== operation_id),
        result,
      ]);
      if (result.request?.status === "sent") {
        retain(without());
        onSent();
      }
    } catch (error) {
      if (error instanceof DiscussionError && error.status === 409) {
        retain(without());
        setError("讨论内容已变化，刷新状态后可重新发起转交。");
      } else {
        setError("暂时无法确认是否已登记。可用同一操作重试。");
      }
    } finally {
      posting.current = false;
      setBusy(undefined);
    }
  }

  const statuses = [
    ...(current ? [current] : []),
    ...older.filter((row) => row.operation_id !== current?.operation_id),
  ];
  return render({
    action: (
      <button
        className="primary discussion-quick-action"
        type="button"
        disabled={!current || !!busy || Boolean(current.request)}
        onClick={() => current && void submit(current)}
      >
        转交主 Chat
      </button>
    ),
    status: (
      <>
        {statuses.map((record) => {
          const { operation_id, request } = record;
          const previous = operation_id !== current?.operation_id;
          if (
            !request &&
            busy !== operation_id &&
            !tracked.some((row) => row.operation_id === operation_id)
          )
            return null;
          return (
            <div className="discussion-quick-status" key={operation_id}>
              {busy === operation_id || request?.status === "queued" ? (
                <span role="status">正在生成摘要并转交…</span>
              ) : request?.message_id ? (
                <span role="status">
                  已转交主 Chat ·{" "}
                  <a href={chatLink("main", request.message_id)}>
                    查看主 Chat 消息 ↗
                  </a>
                </span>
              ) : (
                <>
                  <span className="error" role="alert">
                    {request?.status === "failed"
                      ? `${previous ? "先前" : ""}转交失败，未发送到主 Chat。${request.error ?? ""}`
                      : "暂时未查到转交请求。可用同一操作重试。"}
                  </span>
                  <button
                    className="discussion-secondary"
                    type="button"
                    disabled={!!busy}
                    onClick={() => void submit(record)}
                  >
                    重试转交
                  </button>
                </>
              )}
            </div>
          );
        })}
        {!!error && (
          <div className="discussion-quick-status error" role="alert">
            {error}
          </div>
        )}
      </>
    ),
  });
}

export function HandoffDialog({
  chat,
  references,
  onClose,
  onSent,
}: {
  chat: string;
  references: Reference[];
  onClose: () => void;
  onSent: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const requestKey = `tend.discussion.draft.${chat}`;
  const [id, setId] = useState<string | undefined>(() => cached(requestKey));
  const [record, setRecord] = useState<DraftRequest | null>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [sent, setSent] = useState("");
  const [reload, setReload] = useState(0);
  useEffect(() => {
    dialog.current?.showModal();
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    let timer: number | undefined;
    async function load() {
      try {
        const { draft_request } = await readDraft(chat, controller.signal, id);
        if (controller.signal.aborted) return;
        setRecord(draft_request);
        if (draft_request?.status === "queued") {
          timer = window.setTimeout(() => void load(), 900);
        } else if (draft_request && id) {
          save(requestKey, null);
        }
      } catch (error) {
        if (!controller.signal.aborted) setError(discussionError(error));
      }
    }
    void load();
    return () => {
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [chat, id, reload, requestKey]);

  async function generate(manual = false) {
    if (busy) return;
    const operation = id && !record && !manual ? id : operationId();
    setBusy(true);
    setError("");
    setSent("");
    save(requestKey, operation);
    setId(operation);
    try {
      await requestDraft(chat, operation, manual);
      setRecord(undefined);
      setReload((value) => value + 1);
    } catch (error) {
      setError(discussionError(error));
    } finally {
      setBusy(false);
    }
  }

  function close() {
    dialog.current?.close();
    onClose();
  }

  const message = sent || record?.message_id;
  return (
    <dialog
      ref={dialog}
      className="discussion-dialog"
      aria-labelledby="handoff-title"
      onCancel={(event) => {
        event.preventDefault();
        close();
      }}
    >
      <div className="discussion-dialog-inner">
        <header>
          <div>
            <p className="discussion-mode">独立讨论 → 主 Chat</p>
            <h2 id="handoff-title">预览 / 编辑转交</h2>
          </div>
          <button
            type="button"
            className="text-button"
            onClick={close}
            aria-label="关闭转交草稿"
          >
            关闭
          </button>
        </header>
        <p className="discussion-muted">
          这是一条独立的手动发送路径。核对草稿后，可选择引用和执行目标。
          只有点击「发送到主 Chat」才会投递。转交不会扩大原授权。
        </p>
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
        {message ? (
          <div className="discussion-sent">
            <p role="status">已发送到主 Chat</p>
            <a className="primary" href={chatLink("main", message)}>
              查看消息和 Router 回执 ↗
            </a>
            <button
              type="button"
              className="text-button"
              onClick={() => void generate()}
              disabled={busy}
            >
              生成另一份草稿
            </button>
          </div>
        ) : record?.draft ? (
          <HandoffFields
            key={record.id}
            chat={chat}
            draftId={record.id}
            initial={record.draft}
            references={references}
            onSent={(message) => {
              setSent(message);
              onSent();
            }}
          />
        ) : (
          <>
            {record?.status === "failed" && (
              <p className="error" role="alert">
                {record.error}
              </p>
            )}
            {(record === undefined || busy || record?.status === "queued") &&
            !error ? (
              <p role="status">
                {busy || record?.status === "queued"
                  ? "正在生成转交草稿…"
                  : "正在读取草稿…"}
              </p>
            ) : (
              <div className="discussion-actions">
                <button
                  type="button"
                  className="primary"
                  disabled={busy}
                  onClick={() => void generate()}
                >
                  {id && error ? "重试生成草稿" : "生成转交草稿"}
                </button>
                <button
                  type="button"
                  className="discussion-secondary"
                  disabled={busy}
                  onClick={() => void generate(true)}
                >
                  手动填写
                </button>
              </div>
            )}
          </>
        )}
      </div>
    </dialog>
  );
}

type Edit = { draft: Draft; ids: string[]; target: IssueChoice | null };

function HandoffFields({
  chat,
  draftId,
  initial,
  references,
  onSent,
}: {
  chat: string;
  draftId: string;
  initial: Draft;
  references: Reference[];
  onSent: (message: string) => void;
}) {
  const key = `tend.discussion.handoff.${chat}.${draftId}`;
  const [edit, setEdit] = useState<Edit>(
    () =>
      cached<Edit>(`${key}.edit`) ?? { draft: initial, ids: [], target: null },
  );
  const [pending, setPending] = useState(() => cached<SendDraft>(key));
  const [sending, setSending] = useState(false);
  const [error, setError] = useState("");
  const [query, setQuery] = useState("");
  const { result, error: searchError } = useReferenceSearch(query, true);
  const allowed = edit.ids.filter((id) =>
    references.some((ref) => ref.id === id),
  );

  function update(value: Edit) {
    setEdit(value);
    save(`${key}.edit`, value);
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (sending || (!pending && !edit.draft.goal.trim())) return;
    const body = pending ?? {
      ...edit.draft,
      operation_id: operationId(),
      draft_id: draftId,
      reference_ids: allowed,
      target_issue_id: edit.target?.issue_id,
    };
    setPending(body);
    save(key, body);
    setSending(true);
    setError("");
    try {
      const { message_id } = await sendDraft(chat, body);
      save(key, null);
      save(`${key}.edit`, null);
      onSent(message_id);
    } catch (error) {
      if (error instanceof DiscussionError && error.status === 409) {
        try {
          const { draft_request } = await readDraft(
            chat,
            new AbortController().signal,
            draftId,
          );
          setPending(undefined);
          save(key, null);
          if (draft_request?.message_id) {
            save(`${key}.edit`, null);
            onSent(draft_request.message_id);
          } else {
            setError("此操作与已记录请求冲突。请核对并重新发送。");
          }
        } catch {
          setError("暂时无法查证转交回执。再次发送会使用同一操作 ID。");
        }
      } else if (
        error instanceof DiscussionError &&
        [400, 404].includes(error.status)
      ) {
        setPending(undefined);
        save(key, null);
        setError(error.message);
      } else {
        setError("暂时无法确认是否送达。再次发送会使用同一操作 ID。");
      }
    } finally {
      setSending(false);
    }
  }
  return (
    <form
      className="discussion-handoff-form"
      onSubmit={(event) => void submit(event)}
    >
      <fieldset disabled={Boolean(pending)}>
        {labels.map(([key, label]) => (
          <label key={key}>
            {label}
            <textarea
              value={edit.draft[key] ?? ""}
              required={key === "goal"}
              rows={key === "goal" ? 2 : 1}
              onChange={(event) =>
                update({
                  ...edit,
                  draft: { ...edit.draft, [key]: event.target.value },
                })
              }
            />
          </label>
        ))}
      </fieldset>
      <fieldset
        className="discussion-handoff-references"
        disabled={Boolean(pending)}
      >
        <legend>随转交发送的参考资料（可选）</legend>
        {references.length === 0 && (
          <p className="discussion-muted">当前没有附加资料。</p>
        )}
        {references.map((ref) => (
          <label key={ref.id}>
            <input
              type="checkbox"
              checked={allowed.includes(ref.id)}
              onChange={(event) =>
                update({
                  ...edit,
                  ids: event.target.checked
                    ? [...allowed, ref.id]
                    : allowed.filter((id) => id !== ref.id),
                })
              }
            />
            <span>
              {ref.label}
              <small>{referenceStatus[ref.status] ?? ref.status}</small>
            </span>
          </label>
        ))}
      </fieldset>
      <fieldset
        className="discussion-handoff-target"
        disabled={Boolean(pending)}
      >
        <legend>指定要推进的 Issue（可选）</legend>
        <p className="discussion-muted">
          参考 Issue 不会自动成为执行目标。这里只列出主 Chat 能推进的 Issue。
        </p>
        {edit.target ? (
          <div className="discussion-target-value">
            <strong>
              {issueTitle(edit.target.title, edit.target.short_number)}
            </strong>
            <button
              type="button"
              className="text-button"
              onClick={() => update({ ...edit, target: null })}
            >
              清除执行目标
            </button>
          </div>
        ) : (
          <>
            <input
              aria-label="搜索执行目标 Issue"
              value={query}
              placeholder="输入名称或 #编号"
              onChange={(event) => setQuery(event.target.value)}
            />
            {query.trim() && (
              <div className="discussion-options">
                {result?.issues.map((issue) => (
                  <button
                    type="button"
                    key={issue.issue_id}
                    onClick={() => {
                      update({ ...edit, target: issue });
                      setQuery("");
                    }}
                  >
                    {issueTitle(issue.title, issue.short_number)}
                  </button>
                ))}
                {result && !result.issues.length && (
                  <p className="discussion-muted">没有符合的执行目标。</p>
                )}
              </div>
            )}
            {searchError && (
              <p className="error" role="alert">
                {searchError}
              </p>
            )}
          </>
        )}
      </fieldset>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      <footer>
        <button
          type="submit"
          className="primary"
          disabled={sending || (!pending && !edit.draft.goal.trim())}
        >
          {sending
            ? "正在发送…"
            : pending
              ? "重试发送到主 Chat"
              : "发送到主 Chat"}
        </button>
      </footer>
    </form>
  );
}
