import { useEffect, useRef, useState } from "react";
import { chatLink, readIssueBoard } from "./api";
import type { BoardIssue, BoardPage, BoardStage, PullLink } from "./api";

const columns: { id: BoardStage; title: string }[] = [
  { id: "backlog", title: "待安排" },
  { id: "todo", title: "待推进" },
  { id: "in_progress", title: "进行中" },
  { id: "waiting", title: "等待中" },
  { id: "done", title: "已完成" },
  { id: "cancelled", title: "已取消" },
];

const cancelledPreferenceKey = "tend.ng.board.cancelled-expanded";

const queuedLabels: Record<string, string> = {
  awaiting_assignment: "待分配",
  awaiting_receipt: "待领取",
  accepted: "待开始执行",
  completed: "待负责人收尾",
  failed: "待负责人处理执行失败",
};

const waitLabels = {
  external: "等待外部条件",
  reply: "等待回复",
  children: "等待子项",
  continuation: "等待接续",
};

const mobileOrder: BoardStage[] = [
  "in_progress",
  "todo",
  "waiting",
  "backlog",
  "done",
  "cancelled",
];

const pullLabels: Record<string, string> = {
  open: "开启",
  merged: "已合并",
  closed: "已关闭",
};

const checkLabels: Record<string, string> = {
  success: "CI 通过",
  pending: "CI 进行中",
  failure: "CI 失败",
};

function progress(issue: BoardIssue) {
  if (issue.running > 0) return `${issue.running} 项运行中`;
  if (issue.waiting_for) return waitLabels[issue.waiting_for];
  if (issue.stage === "todo") return queuedLabels[issue.task_state];
}

function Pull({ pull }: { pull: PullLink }) {
  const facts = pull.facts;
  const repository = new URL(pull.url).pathname
    .split("/")
    .slice(1, 3)
    .join("/");
  return (
    <li className="board-pull">
      <a href={pull.url} target="_blank" rel="noreferrer">
        {repository} #{pull.number}
      </a>
      <div className="board-pull-status">
        {facts && (
          <span>
            {facts.draft && facts.state === "open"
              ? "草稿"
              : pullLabels[facts.state]}
          </span>
        )}
        {facts?.ci && (
          <span data-check={facts.ci}>
            {pull.observation_error ? "上次：" : ""}
            {checkLabels[facts.ci]}
          </span>
        )}
        {facts && !facts.ci && <span>CI 未确认</span>}
        {pull.observation_error && <span data-check="failure">暂不可观测</span>}
        {!facts && !pull.observation_error && <span>待观测</span>}
      </div>
    </li>
  );
}

function Card({ issue }: { issue: BoardIssue }) {
  const detail = progress(issue);
  const repositories = issue.repo_ids?.join(" · ");
  return (
    <li className="board-card">
      {repositories && <div className="board-card-repos">{repositories}</div>}
      <a className="board-card-title" href={`/issues/${issue.id}`}>
        {issue.title}
      </a>
      {(detail || issue.attention) && (
        <div className="board-card-state">
          {detail && <span>{detail}</span>}
          {issue.attention && (
            <a
              className="board-attention"
              href={chatLink(
                issue.attention.chat_id,
                issue.attention.question_message_id,
              )}
            >
              待你决定
            </a>
          )}
        </div>
      )}
      {issue.pulls.length > 0 && (
        <ul className="board-pulls">
          {issue.pulls.map((pull) => (
            <Pull key={pull.url} pull={pull} />
          ))}
        </ul>
      )}
    </li>
  );
}

export function IssueBoard({
  token,
  onReachable,
}: {
  token: string;
  onReachable: (value: boolean) => void;
}) {
  const [cursors, setCursors] = useState<number[]>([]);
  const [page, setPage] = useState<BoardPage | null>(null);
  const [error, setError] = useState("");
  const [mobileStage, setMobileStage] = useState<BoardStage>();
  const [cancelledExpanded, setCancelledExpanded] = useState(() => {
    try {
      return localStorage.getItem(cancelledPreferenceKey) !== "false";
    } catch {
      return true;
    }
  });
  const mobileTabs = useRef<HTMLDivElement>(null);
  const before = cursors.at(-1);
  const loaded = page !== null;

  useEffect(() => {
    const controller = new AbortController();
    let timer: number;
    async function update() {
      try {
        const result = await readIssueBoard(token, before, controller.signal);
        if (!controller.signal.aborted) {
          setPage(result);
          setMobileStage(
            (current) =>
              current ??
              mobileOrder.find((stage) =>
                result.issues.some((issue) => issue.stage === stage),
              ) ??
              "in_progress",
          );
          setError("");
          onReachable(true);
        }
      } catch {
        if (!controller.signal.aborted) {
          setError("暂时无法读取看板");
          onReachable(false);
        }
      }
      if (!controller.signal.aborted)
        timer = window.setTimeout(() => void update(), 5000);
    }
    void update();
    return () => {
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [token, before, onReachable]);

  useEffect(() => {
    mobileTabs.current
      ?.querySelector('button[aria-pressed="true"]')
      ?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [loaded, mobileStage]);

  function turnPage(next?: number) {
    setPage(null);
    setCursors((saved) =>
      next === undefined ? saved.slice(0, -1) : [...saved, next],
    );
  }

  function toggleCancelled() {
    const expanded = !cancelledExpanded;
    setCancelledExpanded(expanded);
    try {
      localStorage.setItem(cancelledPreferenceKey, String(expanded));
    } catch {
      // Keep the current view usable when storage is unavailable.
    }
  }

  const byStage = new Map<BoardStage, BoardIssue[]>(
    columns.map(({ id }) => [id, []]),
  );
  for (const issue of page?.issues ?? []) byStage.get(issue.stage)?.push(issue);

  return (
    <main className="board">
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {!page && !error && <p role="status">正在读取看板…</p>}
      {page && (
        <>
          <div
            ref={mobileTabs}
            className="board-tabs"
            role="group"
            aria-label="选择 Issue 阶段"
          >
            {columns.map(({ id, title }) => (
              <button
                key={id}
                type="button"
                aria-pressed={mobileStage === id}
                onClick={() => setMobileStage(id)}
              >
                <span
                  className="board-dot"
                  data-column={id}
                  aria-hidden="true"
                />
                {title}
                <small>{byStage.get(id)?.length ?? 0}</small>
              </button>
            ))}
          </div>
          <div
            className="board-columns"
            data-cancelled-expanded={cancelledExpanded}
          >
            {columns.map(({ id, title }) => {
              const cards = byStage.get(id) ?? [];
              const collapsed = id === "cancelled" && !cancelledExpanded;
              return (
                <section
                  key={id}
                  aria-labelledby={`board-${id}`}
                  className="board-column"
                  data-mobile={mobileStage === id}
                  data-collapsed={collapsed}
                >
                  <h2 id={`board-${id}`}>
                    <span
                      className="board-dot"
                      aria-hidden="true"
                      data-column={id}
                    />
                    {id === "cancelled" ? (
                      <>
                        <button
                          type="button"
                          className="board-column-toggle"
                          aria-label={
                            cancelledExpanded ? "收起已取消" : "展开已取消"
                          }
                          aria-expanded={cancelledExpanded}
                          aria-controls="board-cancelled-content"
                          onClick={toggleCancelled}
                        >
                          <span className="board-column-title">{title}</span>
                          <small>{cards.length}</small>
                        </button>
                        <span className="board-mobile-heading">
                          {title}
                          <small>{cards.length}</small>
                        </span>
                      </>
                    ) : (
                      <>
                        <span className="board-column-title">{title}</span>
                        <small>{cards.length}</small>
                      </>
                    )}
                  </h2>
                  <div
                    id={
                      id === "cancelled" ? "board-cancelled-content" : undefined
                    }
                    className="board-column-content"
                  >
                    {cards.length ? (
                      <ul className="board-cards">
                        {cards.map((issue) => (
                          <Card key={issue.id} issue={issue} />
                        ))}
                      </ul>
                    ) : (
                      <p className="board-empty">暂无 Issue</p>
                    )}
                  </div>
                </section>
              );
            })}
          </div>
          {(cursors.length > 0 || page.next_before !== null) && (
            <nav className="board-pages" aria-label="看板分页">
              <span>本页 {page.issues.length} 条</span>
              <button
                className="text-button"
                type="button"
                disabled={!cursors.length}
                onClick={() => turnPage()}
              >
                较新
              </button>
              <button
                className="text-button"
                type="button"
                disabled={page.next_before === null}
                onClick={() => turnPage(page.next_before ?? undefined)}
              >
                较早
              </button>
            </nav>
          )}
        </>
      )}
    </main>
  );
}
