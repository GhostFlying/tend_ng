import { useEffect, useId, useRef, useState } from "react";
import type { ReactNode } from "react";
import { readNotifications } from "./api";
import type { Activity, NotificationPage } from "./api";

const labels: Record<string, string> = {
  open: "已重开",
  closed: "已结案",
  running: "开始执行",
  waiting: "等待接续",
  completed: "执行完成",
  failed: "执行失败",
  cancelled: "已取消",
};

const dateTime = new Intl.DateTimeFormat("zh-CN", {
  month: "short",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
  hour12: false,
});

function change(activity: Activity) {
  if (activity.current === "running" && activity.previous === "waiting") {
    return "已接续";
  }
  return labels[activity.current] ?? activity.current;
}

function ActivityList({ token }: { token: string }) {
  const [page, setPage] = useState<NotificationPage | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  useEffect(() => {
    const controller = new AbortController();
    void readNotifications(token, undefined, controller.signal).then(
      setPage,
      () => {
        if (!controller.signal.aborted) setError("暂时无法读取通知");
      },
    );
    return () => controller.abort();
  }, [token]);

  async function earlier() {
    if (page?.next_before == null || loading) return;
    setLoading(true);
    setError("");
    try {
      const next = await readNotifications(token, page.next_before);
      setPage({
        activities: [...page.activities, ...next.activities],
        next_before: next.next_before,
      });
    } catch {
      setError("暂时无法读取更早的通知");
    } finally {
      setLoading(false);
    }
  }

  return (
    <>
      {error && (
        <p role="alert" className="error">
          {error}
        </p>
      )}
      {!page && !error && <p role="status">正在读取…</p>}
      {page?.activities.length === 0 && <p>还没有工作动态</p>}
      {page && (
        <ol className="activity-list">
          {page.activities.map((activity) => {
            const timestamp = new Date(activity.changed_at * 1000);
            return (
              <li key={activity.seq} className="activity">
                <a href={`/issues/${activity.issue_id}`}>
                  {activity.issue_title}
                </a>
                <span>{change(activity)}</span>
                <time dateTime={timestamp.toISOString()}>
                  {dateTime.format(timestamp)}
                </time>
              </li>
            );
          })}
        </ol>
      )}
      {page?.next_before != null && (
        <button
          type="button"
          className="text-button"
          disabled={loading}
          onClick={() => void earlier()}
        >
          {loading ? "正在读取…" : "读取更早的通知"}
        </button>
      )}
    </>
  );
}

export function Notifications({
  token,
  mobileIcon,
}: {
  token: string;
  mobileIcon?: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const panelId = useId();
  const focusBefore = useRef<HTMLElement | null>(null);

  function close() {
    setOpen(false);
    focusBefore.current?.focus({ preventScroll: true });
  }

  return (
    <>
      <button
        type="button"
        className={mobileIcon ? "phone-item" : "text-button"}
        aria-expanded={open}
        aria-controls={open ? panelId : undefined}
        onPointerDown={() => {
          if (!open && document.activeElement instanceof HTMLElement) {
            focusBefore.current = document.activeElement;
          }
        }}
        onClick={(event) => {
          if (open) return close();
          if (!event.detail) focusBefore.current = event.currentTarget;
          setOpen(true);
        }}
      >
        {mobileIcon}
        通知
      </button>
      {open && (
        <section
          id={panelId}
          className="notifications"
          aria-label="工作通知"
          onKeyDown={(event) => {
            if (event.key === "Escape") close();
          }}
        >
          <div className="notifications-heading">
            <h2>工作动态</h2>
            <button type="button" className="text-button" onClick={close}>
              关闭
            </button>
          </div>
          <ActivityList token={token} />
        </section>
      )}
    </>
  );
}
