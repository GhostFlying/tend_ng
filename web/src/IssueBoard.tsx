import { useEffect, useState } from "react";
import { readIssueBoard } from "./api";
import type { BoardIssue, BoardPage, PullLink } from "./api";

const columns = [
  { id: "queued", title: "待推进" },
  { id: "active", title: "进行中" },
  { id: "closed", title: "已结束" },
] as const;

const taskLabels: Record<string, string> = {
  awaiting_assignment: "待分配",
  awaiting_receipt: "待领取",
  accepted: "已领取",
  running: "运行中",
  waiting: "等待接续",
  completed: "执行完成",
  failed: "执行失败",
  cancelled: "已取消",
};

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

function column(issue: BoardIssue) {
  if (issue.issue_state !== "open") return "closed";
  if (["running", "waiting", "accepted", "failed"].includes(issue.task_state))
    return "active";
  return "queued";
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
  return (
    <li className="board-card">
      <a className="board-card-title" href={`/issues/${issue.id}`}>
        {issue.title}
      </a>
      <p className="board-card-state">
        {issue.parent_issue_id && <span>子 Issue</span>}
        <span>
          {issue.issue_state === "closed"
            ? "已结案"
            : (taskLabels[issue.task_state] ?? issue.task_state)}
        </span>
      </p>
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
  const before = cursors.at(-1);

  useEffect(() => {
    const controller = new AbortController();
    let timer: number;
    async function update() {
      try {
        const result = await readIssueBoard(token, before, controller.signal);
        if (!controller.signal.aborted) {
          setPage(result);
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

  function turnPage(next?: number) {
    setPage(null);
    setCursors((saved) =>
      next === undefined ? saved.slice(0, -1) : [...saved, next],
    );
  }

  return (
    <main className="board">
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {!page && !error && <p role="status">正在读取看板…</p>}
      {page?.issues.length === 0 && <p>暂无 Issue</p>}
      {page && page.issues.length > 0 && (
        <>
          <div className="board-columns">
            {columns.map(({ id, title }) => (
              <section
                key={id}
                aria-labelledby={`board-${id}`}
                className="board-column"
              >
                <h2 id={`board-${id}`}>
                  <span aria-hidden="true" data-column={id} />
                  {title}
                </h2>
                <ul className="board-cards">
                  {page.issues
                    .filter((issue) => column(issue) === id)
                    .map((issue) => (
                      <Card key={issue.id} issue={issue} />
                    ))}
                </ul>
              </section>
            ))}
          </div>
          {(cursors.length > 0 || page.next_before !== null) && (
            <nav className="board-pages" aria-label="看板分页">
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
