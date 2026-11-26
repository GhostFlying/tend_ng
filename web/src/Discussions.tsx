import { useCallback, useEffect, useRef, useState } from "react";
import type { FormEvent } from "react";
import { Chat } from "./Chat";
import { DiscussionStart } from "./DiscussionStart";
import { issueTitle } from "./api";
import {
  attachReference,
  detachReference,
  discussionChanged,
  discussionIndexLink,
  discussionLink,
  listDiscussions,
  notifyDiscussionChanged,
  readDiscussion,
  readWorkspaceChoices,
  referenceStatus,
  renameDiscussion,
  setDiscussionArchived,
} from "./discussionApi";
import type {
  Discussion,
  DiscussionDetail,
  IssueChoice,
  Source,
  WorkspaceChoice,
} from "./discussionApi";
import { HandoffDialog } from "./DiscussionHandoff";
import { discussionError, useReferenceSearch } from "./discussionSearch";
import { navigate, trackPageVisit } from "./navigation";
import "./discussions.css";

function useDiscussionList(
  archived: boolean,
  onReachable?: (online: boolean) => void,
) {
  const [rows, setRows] = useState<Discussion[] | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {
    let pending: AbortController | undefined;
    async function load() {
      pending?.abort();
      const controller = new AbortController();
      pending = controller;
      try {
        const { discussions } = await listDiscussions(
          archived,
          controller.signal,
        );
        if (controller.signal.aborted) return;
        setRows(discussions);
        setError("");
        onReachable?.(true);
      } catch (error) {
        if (!controller.signal.aborted) {
          setError(discussionError(error));
          onReachable?.(false);
        }
      }
    }
    function resume(event: PageTransitionEvent) {
      if (event.persisted) void load();
    }
    void load();
    window.addEventListener(discussionChanged, load);
    window.addEventListener("focus", load);
    window.addEventListener("pageshow", resume);
    return () => {
      pending?.abort();
      window.removeEventListener(discussionChanged, load);
      window.removeEventListener("focus", load);
      window.removeEventListener("pageshow", resume);
    };
  }, [archived, onReachable]);
  return { rows, error };
}

const RECENT_DISCUSSIONS = 8;

export function DiscussionSidebar({ chat }: { chat: string | null }) {
  const { rows } = useDiscussionList(false);
  const recent = rows?.slice(0, RECENT_DISCUSSIONS) ?? [];
  const current = rows?.find(({ chat_id }) => chat_id === chat);
  if (current && !recent.includes(current)) {
    recent[RECENT_DISCUSSIONS - 1] = current;
  }
  return (
    <div className="discussion-sidebar-items">
      {!!recent.length && (
        <div className="discussion-sidebar-history">
          <h2>最近讨论</h2>
          <ul aria-label="最近讨论">
            {recent.map(({ chat_id, title }) => (
              <li key={chat_id}>
                <a
                  href={discussionLink(chat_id)}
                  aria-current={chat_id === chat ? "page" : undefined}
                  title={title}
                >
                  <span>{title}</span>
                </a>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}

export function DiscussionPage({
  chat,
  archived,
  onReachable,
}: {
  chat: string | null;
  archived: boolean;
  onReachable: (online: boolean) => void;
}) {
  return chat ? (
    <DiscussionView key={chat} chat={chat} onReachable={onReachable} />
  ) : (
    <DiscussionIndex
      key={String(archived)}
      archived={archived}
      onReachable={onReachable}
    />
  );
}

function DiscussionIndex({
  archived,
  onReachable,
}: {
  archived: boolean;
  onReachable: (online: boolean) => void;
}) {
  const { rows, error } = useDiscussionList(archived, onReachable);
  return (
    <main className="timeline">
      <div className="lane discussion-index">
        <div className="discussion-title">
          <div>
            <h2>讨论</h2>
            <p>单独聊一个想法，随时回来接着说。</p>
          </div>
          <DiscussionStart />
        </div>
        <nav className="discussion-list-nav" aria-label="筛选讨论">
          <a
            href={discussionIndexLink()}
            aria-current={!archived ? "page" : undefined}
          >
            当前讨论
          </a>
          <a
            href={discussionIndexLink(true)}
            aria-current={archived ? "page" : undefined}
          >
            已归档
          </a>
        </nav>
        {error ? (
          <p className="error" role="alert">
            {error}
          </p>
        ) : !rows ? (
          <p role="status">正在读取讨论…</p>
        ) : !rows.length ? (
          <p className="discussion-empty">
            {archived ? "还没有已归档的讨论。" : "还没有讨论。"}
          </p>
        ) : (
          <ul
            className="discussion-list"
            aria-label={archived ? "已归档讨论" : "讨论列表"}
          >
            {rows.map(({ chat_id, title, updated_at }) => (
              <li key={chat_id}>
                <a href={discussionLink(chat_id)}>
                  <strong>{title}</strong>
                  <time dateTime={new Date(updated_at * 1000).toISOString()}>
                    {new Date(updated_at * 1000).toLocaleDateString("zh-CN")}
                  </time>
                </a>
              </li>
            ))}
          </ul>
        )}
      </div>
    </main>
  );
}

function DiscussionView({
  chat,
  onReachable,
}: {
  chat: string;
  onReachable: (online: boolean) => void;
}) {
  const [meta, setMeta] = useState<DiscussionDetail>();
  const [error, setError] = useState("");
  const [refresh, setRefresh] = useState(0);
  const [title, setTitle] = useState<string | null>(null);
  const [savingTitle, setSavingTitle] = useState(false);
  const [savingArchive, setSavingArchive] = useState(false);
  const [picker, setPicker] = useState(false);
  const [handoff, setHandoff] = useState(false);
  const previousArchived = useRef(false);
  const archivalState = meta?.archived;

  useEffect(() => {
    if (
      archivalState === undefined ||
      archivalState === previousArchived.current
    )
      return;
    previousArchived.current = archivalState;
    notifyDiscussionChanged();
  }, [archivalState]);

  useEffect(() => {
    const controller = new AbortController();
    let timer: number | undefined;
    async function load() {
      try {
        const detail = await readDiscussion(chat, controller.signal);
        if (controller.signal.aborted) return;
        setMeta(detail);
        onReachable(true);
      } catch (error) {
        if (!controller.signal.aborted) {
          setError(discussionError(error));
          onReachable(false);
        }
      } finally {
        if (!controller.signal.aborted)
          timer = window.setTimeout(() => void load(), 12000);
      }
    }
    function resume(event: PageTransitionEvent) {
      if (!event.persisted) return;
      controller.abort();
      // A browser Back navigation can restore the page before React had
      // committed the state update from a successful archive.
      setSavingArchive(false);
      setRefresh((value) => value + 1);
    }
    function onChange(event: Event) {
      if (!(event instanceof CustomEvent) || event.detail !== chat) return;
      controller.abort();
      setRefresh((value) => value + 1);
    }
    void load();
    window.addEventListener("pageshow", resume);
    window.addEventListener(discussionChanged, onChange);
    return () => {
      controller.abort();
      window.clearTimeout(timer);
      window.removeEventListener("pageshow", resume);
      window.removeEventListener(discussionChanged, onChange);
    };
  }, [chat, onReachable, refresh]);

  const add = useCallback(
    async (source: Source) => {
      const reference = await attachReference(chat, source);
      setMeta((prev) => {
        if (!prev) return prev;
        return {
          ...prev,
          references: [
            ...prev.references.filter((row) => row.id !== reference.id),
            reference,
          ],
        };
      });
      setRefresh((value) => value + 1);
    },
    [chat],
  );

  async function remove(id: string) {
    try {
      setError("");
      await detachReference(chat, id);
      setMeta(
        (prev) =>
          prev && {
            ...prev,
            references: prev.references.filter((ref) => ref.id !== id),
          },
      );
      setRefresh((value) => value + 1);
    } catch (error) {
      setError(discussionError(error));
    }
  }

  async function saveTitle(event: FormEvent) {
    event.preventDefault();
    if (!title?.trim()) return;
    setSavingTitle(true);
    setError("");
    try {
      await renameDiscussion(chat, title);
      setMeta((prev) => prev && { ...prev, title: title.trim() });
      setTitle(null);
      notifyDiscussionChanged(chat);
    } catch (error) {
      setError(discussionError(error));
    } finally {
      setSavingTitle(false);
    }
  }

  async function changeArchive(archived: boolean) {
    const endVisit = trackPageVisit();
    let changed = false;
    setSavingArchive(true);
    setError("");
    try {
      await setDiscussionArchived(chat, archived);
      previousArchived.current = archived;
      setMeta((prev) => prev && { ...prev, archived });
      changed = true;
      notifyDiscussionChanged(chat);
    } catch (error) {
      setError(discussionError(error));
    } finally {
      setSavingArchive(false);
      if (endVisit() && changed && archived) navigate(discussionIndexLink());
    }
  }

  if (!meta)
    return (
      <main className="lane discussion-index">
        <a href={discussionIndexLink()}>返回讨论</a>
        <p role={error ? "alert" : "status"}>{error || "正在打开讨论…"}</p>
      </main>
    );
  return (
    <>
      <section className="discussion-bar" aria-label="当前讨论">
        <div className="lane discussion-bar-inner">
          <div className="discussion-meta">
            <a
              className="text-button"
              href={discussionIndexLink(meta.archived)}
            >
              {meta.archived ? "已归档列表" : "所有讨论"}
            </a>
            {meta.origin && (
              <a className="text-button" href={meta.origin.href}>
                {meta.origin.label}
              </a>
            )}
            <span className="discussion-mode">只读讨论</span>
            {meta.archived && (
              <span className="discussion-archive-label">已归档</span>
            )}
          </div>
          {title !== null ? (
            <form
              className="discussion-title"
              onSubmit={(event) => void saveTitle(event)}
            >
              <input
                aria-label="讨论标题"
                maxLength={120}
                value={title}
                onChange={(event) => setTitle(event.target.value)}
                required
              />
              <button
                className="text-button"
                type="button"
                onClick={() => setTitle(null)}
              >
                取消
              </button>
              <button
                className="primary"
                disabled={savingTitle || !title.trim()}
              >
                保存
              </button>
            </form>
          ) : (
            <div className="discussion-title">
              <h2>{meta.title}</h2>
              <button
                className="text-button"
                type="button"
                onClick={() => setTitle(meta.title)}
              >
                改标题
              </button>
            </div>
          )}
          <div className="discussion-actions">
            <button
              className="discussion-secondary"
              type="button"
              aria-expanded={picker}
              onClick={() => setPicker((value) => !value)}
            >
              附加资料
            </button>
            <button
              className="discussion-secondary"
              type="button"
              onClick={() => setHandoff(true)}
            >
              交给 Router
            </button>
            <button
              className="discussion-secondary discussion-archive-action"
              type="button"
              disabled={savingArchive}
              onClick={() => void changeArchive(!meta.archived)}
            >
              {meta.archived ? "恢复讨论" : "归档讨论"}
            </button>
            {meta.handoffs[0] && (
              <a className="text-button" href={meta.handoffs[0].href}>
                查看主 Chat 回执 ↗
              </a>
            )}
          </div>
          {!meta.execution_configured && (
            <p role="status" className="discussion-muted">
              讨论 App Server 尚未配置。可以整理资料，但目前不能生成答复。
            </p>
          )}
          {error && (
            <p className="error" role="alert">
              {error}
            </p>
          )}
          {meta.references.length > 0 && (
            <ul className="discussion-references" aria-label="已附加资料">
              {meta.references.map((ref) => (
                <li key={ref.id}>
                  {ref.href ? (
                    <a href={ref.href}>{ref.label}</a>
                  ) : (
                    <span>{ref.label}</span>
                  )}
                  <small data-readable={ref.status === "available"}>
                    {referenceStatus[ref.status] ?? ref.status}
                  </small>
                  <button
                    type="button"
                    className="text-button"
                    aria-label={`移除 ${ref.label}`}
                    onClick={() => void remove(ref.id)}
                  >
                    ×
                  </button>
                </li>
              ))}
            </ul>
          )}
          {meta.handoffs.length > 1 && (
            <details className="discussion-handoff-history">
              <summary>全部转交记录（{meta.handoffs.length}）</summary>
              <ul>
                {meta.handoffs.map((item) => (
                  <li key={item.message_id}>
                    <a href={item.href}>
                      主 Chat 消息 {item.message_id.slice(0, 8)}
                    </a>
                  </li>
                ))}
              </ul>
            </details>
          )}
          {picker && <ReferencePanel add={add} />}
        </div>
      </section>
      <Chat
        chatId={chat}
        onReachable={onReachable}
        standalone={{
          renderMention: (query, done) => (
            <ReferencePicker
              query={query}
              onChoose={async (source) => {
                await add(source);
                done();
              }}
            />
          ),
        }}
      />
      {handoff && (
        <HandoffDialog
          chat={chat}
          references={meta.references}
          onClose={() => setHandoff(false)}
          onSent={() => setRefresh((value) => value + 1)}
        />
      )}
    </>
  );
}

function ReferencePicker({
  query,
  onChoose,
}: {
  query: string;
  onChoose: (source: Source) => Promise<void>;
}) {
  const { result, error } = useReferenceSearch(query, false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState("");
  async function choose(source: Source) {
    setBusy(true);
    setFailure("");
    try {
      await onChoose(source);
    } catch (error) {
      setFailure(discussionError(error));
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="discussion-picker" aria-label="选择登记的资料">
      <p className="discussion-muted">选择登记仓库或 Issue</p>
      <div className="discussion-options">
        {result?.repos.map((repo_id) => (
          <button
            type="button"
            key={repo_id}
            disabled={busy}
            onClick={() => void choose({ kind: "repo", repo_id })}
          >
            {repo_id}
            <small>仓库</small>
          </button>
        ))}
        {result?.issues.map((issue) => (
          <button
            type="button"
            key={issue.issue_id}
            disabled={busy}
            onClick={() =>
              void choose({ kind: "issue", issue_id: issue.issue_id })
            }
          >
            {issueTitle(issue.title, issue.short_number)}
            <small>Issue</small>
          </button>
        ))}
        {result && !result.repos.length && !result.issues.length && (
          <p className="discussion-muted">没有符合的资料。</p>
        )}
      </div>
      {(error || failure) && (
        <p className="error" role="alert">
          {error || failure}
        </p>
      )}
    </div>
  );
}

function ReferencePanel({ add }: { add: (source: Source) => Promise<void> }) {
  const [query, setQuery] = useState("");
  const [choices, setChoices] = useState<WorkspaceChoice[] | null>(null);
  const [opened, setOpened] = useState<IssueChoice | null>(null);
  const [link, setLink] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const current = useRef<AbortController | null>(null);
  useEffect(() => () => current.current?.abort(), []);
  async function attach(source: Source) {
    setBusy(true);
    setError("");
    try {
      await add(source);
      return true;
    } catch (error) {
      setError(discussionError(error));
      return false;
    } finally {
      setBusy(false);
    }
  }
  async function workspaces(issue: IssueChoice) {
    current.current?.abort();
    const controller = new AbortController();
    current.current = controller;
    setOpened(issue);
    setChoices(null);
    setError("");
    try {
      setChoices(
        (await readWorkspaceChoices(issue.issue_id, controller.signal)).choices,
      );
    } catch (error) {
      if (!controller.signal.aborted) setError(discussionError(error));
    }
  }
  return (
    <div className="discussion-reference-panel">
      <label>
        查找仓库或 Issue{" "}
        <input
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="输入名称或 #编号"
        />
      </label>
      <ReferencePicker query={query} onChoose={add} />
      <form
        className="discussion-inline-form"
        onSubmit={async (event) => {
          event.preventDefault();
          if (await attach({ kind: "pull", url: link.trim() })) setLink("");
        }}
      >
        <label>
          PR / MR 链接
          <input
            type="url"
            placeholder="粘贴 https:// 链接"
            value={link}
            onChange={(event) => setLink(event.target.value)}
            required
          />
        </label>
        <button
          type="submit"
          className="discussion-secondary"
          disabled={!link.trim() || busy}
        >
          添加链接
        </button>
      </form>
      <details>
        <summary>读 Issue worktree</summary>
        <WorkspaceIssueSelect onSelect={(issue) => void workspaces(issue)} />
        {opened && (
          <div className="discussion-picker">
            <strong>{issueTitle(opened.title, opened.short_number)}</strong>
            {choices === null ? (
              !error && <p role="status">正在读取 Task…</p>
            ) : choices.length === 0 ? (
              <p className="discussion-muted">没有关联仓库的 Task。</p>
            ) : (
              <div className="discussion-options">
                {choices.map((choice) => (
                  <button
                    key={`${choice.task_id}:${choice.repo_id}`}
                    type="button"
                    disabled={busy}
                    onClick={() =>
                      void attach({
                        kind: "worktree",
                        issue_id: choice.issue_id,
                        task_id: choice.task_id,
                        repo_id: choice.repo_id,
                      })
                    }
                  >
                    <span>
                      {issueTitle(choice.title, choice.short_number)} ·{" "}
                      {choice.repo_id}
                      <small>Task {choice.task_id.slice(0, 8)}</small>
                    </span>
                    <small>
                      {referenceStatus[choice.status] ?? choice.status}
                    </small>
                  </button>
                ))}
              </div>
            )}
          </div>
        )}
      </details>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

function WorkspaceIssueSelect({
  onSelect,
}: {
  onSelect: (issue: IssueChoice) => void;
}) {
  const [query, setQuery] = useState("");
  const { result, error } = useReferenceSearch(query, false);
  return (
    <div className="discussion-picker">
      <label>
        选择 Issue 以查看具体 Task
        <input
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="搜索 Issue 名称或 #编号"
        />
      </label>
      <div className="discussion-options">
        {result?.issues.map((issue) => (
          <button
            type="button"
            key={issue.issue_id}
            onClick={() => onSelect(issue)}
          >
            {issueTitle(issue.title, issue.short_number)}
          </button>
        ))}
      </div>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
